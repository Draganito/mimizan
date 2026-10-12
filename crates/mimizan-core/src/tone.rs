//! The tone of a print: paper or look, and the one pass that turns the
//! placed linear picture into print values.
//!
//! A recipe prints either on paper (`paper.rs`: two exposures, nothing
//! else) or through a look (`curve.rs`: a point list, for the measurement
//! pipeline and older recipes). Both are a [`Tone`], compiled once into a
//! [`ToneMap`] and applied by [`render_in_place`], which also does the
//! placing and the dodge & burn: on paper a shape may act through one
//! filter, so the two passes can see different exposures at a pixel, and
//! that cannot be done as a separate step on one plane.

use crate::burn::{self, Burn};
use crate::calib::{LookEncoding, LookFile};
use crate::curve::Curve;
use crate::paper::{PaperCurve, SplitGrade};
use crate::plane::Plane;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// What the recipe prints with. Untagged in JSON: a split-grade object has
/// `paper`, a look has `points`; older recipes carry the look as before.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Tone {
    Paper(SplitGrade),
    Look(LookFile),
}

impl Default for Tone {
    fn default() -> Self {
        Self::Look(LookFile::neutral())
    }
}

impl Tone {
    /// The paper at its base exposure.
    pub fn paper() -> Self {
        Self::Paper(SplitGrade::default())
    }

    pub fn validate(&self) -> crate::Result<()> {
        match self {
            Self::Paper(s) => s.validate(),
            Self::Look(l) => l.validate(),
        }
    }

    /// Name for logs and the print's description.
    pub fn name(&self) -> String {
        match self {
            Self::Paper(s) => format!("paper {} soft {:+.2} hard {:+.2}", s.paper, s.soft, s.hard),
            Self::Look(l) => l.name.clone(),
        }
    }

    /// Encoding of the output (the TIFF's transfer curve).
    pub fn encoding(&self) -> LookEncoding {
        match self {
            Self::Paper(_) => LookEncoding::Gamma22,
            Self::Look(l) => l.encoding,
        }
    }

    pub fn is_paper(&self) -> bool {
        matches!(self, Self::Paper(_))
    }

    pub fn as_paper(&self) -> Option<&SplitGrade> {
        match self {
            Self::Paper(s) => Some(s),
            Self::Look(_) => None,
        }
    }

    pub fn as_paper_mut(&mut self) -> Option<&mut SplitGrade> {
        match self {
            Self::Paper(s) => Some(s),
            Self::Look(_) => None,
        }
    }

    pub fn compile(&self) -> ToneMap {
        match self {
            Self::Paper(s) => ToneMap::Paper(s.curve()),
            Self::Look(l) => ToneMap::Curve(Curve::from_look(l)),
        }
    }
}

/// A tone ready to evaluate.
#[derive(Clone, Debug)]
pub enum ToneMap {
    Paper(PaperCurve),
    Curve(Curve),
}

impl ToneMap {
    /// Print value of a placed linear value (both passes the same).
    #[inline]
    pub fn eval(&self, l: f64) -> f64 {
        match self {
            Self::Paper(p) => p.eval(l),
            Self::Curve(c) => c.eval(l),
        }
    }

    /// Print value when the two passes saw different values; a look has
    /// one exposure and takes the soft one (they are equal whenever a look
    /// is rendered, see [`render_in_place`]).
    #[inline]
    pub fn eval_split(&self, l_soft: f64, l_hard: f64) -> f64 {
        match self {
            Self::Paper(p) => p.eval_split(l_soft, l_hard),
            Self::Curve(c) => c.eval(l_soft),
        }
    }

    /// What the border is filled with: unexposed paper, or the look's value
    /// for linear white.
    pub fn white(&self) -> f64 {
        match self {
            Self::Paper(p) => p.white(),
            Self::Curve(c) => c.white(),
        }
    }

    /// Is this the plain encoding (a look on the diagonal)? Paper never is.
    pub fn is_identity(&self) -> bool {
        match self {
            Self::Paper(_) => false,
            Self::Curve(c) => c.is_identity(),
        }
    }

    /// Will shapes through one filter make a difference here?
    pub fn splits(&self, burns: &[Burn]) -> bool {
        matches!(self, Self::Paper(_)) && burn::needs_split(burns)
    }

    /// Apply to a plane that is already placed.
    pub fn apply(&self, p: &Plane) -> Plane {
        let mut out = Plane::zeros(p.width, p.height);
        out.data.par_iter_mut().zip(&p.data).for_each(|(o, &v)| *o = self.eval(v));
        out
    }
}

/// Place, dodge & burn and tone in one pass, in place: `plane` holds linear
/// values on entry and print values on return. The shapes are evaluated in
/// the coordinates of the `full` (w, h) picture of which `plane` is the
/// window at `origin`, as `burn::apply_window` does. Without a filtered
/// shape this is exactly `burn::apply_window` followed by the tone.
pub fn render_in_place(
    plane: &mut Plane,
    place: f64,
    burns: &[Burn],
    tone: &ToneMap,
    full: (usize, usize),
    origin: (usize, usize),
) {
    if !tone.splits(burns) {
        burn::apply_window(plane, place, burns, full, origin);
        plane.data.par_iter_mut().for_each(|v| *v = tone.eval(*v));
        return;
    }
    let active: Vec<Burn> = burns.iter().copied().filter(|b| b.ev() != 0.0).collect();
    let w = plane.width;
    let (wf, hf) = (full.0 as f64, full.1 as f64);
    let long = wf.max(hf);
    plane.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let ny = ((origin.1 + y) as f64 + 0.5) / hf;
        for (x, v) in row.iter_mut().enumerate() {
            let nx = ((origin.0 + x) as f64 + 0.5) / wf;
            let (es, eh) = burn::exponents_at(place, &active, nx, ny, wf, hf, long);
            *v = tone.eval_split(*v * 2f64.powf(es), *v * 2f64.powf(eh));
        }
    });
}

/// The whole picture.
pub fn render(plane: &mut Plane, place: f64, burns: &[Burn], tone: &ToneMap) {
    let (w, h) = (plane.width, plane.height);
    render_in_place(plane, place, burns, tone, (w, h), (0, 0));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::burn::Filter;

    fn ramp(w: usize, h: usize) -> Plane {
        Plane::from_vec(w, h, (0..w * h).map(|i| 0.01 + 0.95 * (i % w) as f64 / w as f64).collect())
    }

    #[test]
    fn tone_json_is_untagged_and_backwards_compatible() {
        let paper = Tone::Paper(SplitGrade::new(0.5, -0.25));
        let json = serde_json::to_string(&paper).unwrap();
        assert_eq!(json, r#"{"paper":"multigrade","soft":0.5,"hard":-0.25}"#);
        assert_eq!(serde_json::from_str::<Tone>(&json).unwrap(), paper);
        let look = Tone::Look(LookFile::neutral());
        let json = serde_json::to_string(&look).unwrap();
        assert!(json.contains(r#""points""#));
        assert_eq!(serde_json::from_str::<Tone>(&json).unwrap(), look);
        // An older recipe's curve object.
        let old = r#"{"schema":1,"name":"neutral","points":[[0,0],[1,1]],"encoding":"gamma22","fitted_against":null}"#;
        assert_eq!(serde_json::from_str::<Tone>(old).unwrap(), look);
        assert!(Tone::paper().is_paper() && !look.is_paper());
        assert!(!Tone::paper().compile().is_identity() && look.compile().is_identity());
        assert_eq!(Tone::paper().compile().white(), 1.0);
    }

    /// Without a filtered shape the one pass is the two old steps.
    #[test]
    fn render_equals_place_then_tone_without_filters() {
        let src = ramp(64, 40);
        let burns = [Burn::radial(0.5, 0.5, 0.3, 0.5, 0.7), Burn::gradient(0.0, 0.0, 1.0, 0.0, -0.4)];
        for tone in [Tone::paper(), Tone::Paper(SplitGrade::new(-1.0, 0.5)), Tone::Look(LookFile::neutral())]
        {
            let map = tone.compile();
            let mut a = src.clone();
            render(&mut a, 0.3, &burns, &map);
            let mut b = src.clone();
            burn::apply(&mut b, 0.3, &burns);
            let b = map.apply(&b);
            assert_eq!(a.data, b.data, "{}", tone.name());
        }
    }

    /// A shape through one filter moves only that pass: a hard burn
    /// darkens, and darkens the shadows more than the soft burn of the same
    /// stops does; a look ignores the filter.
    #[test]
    fn filtered_shapes_act_on_one_pass() {
        let src = ramp(64, 40);
        let map = Tone::paper().compile();
        let mut plain = src.clone();
        render(&mut plain, 0.0, &[], &map);
        let mut hard = Burn::radial(0.5, 0.5, 2.0, 0.0, -1.0);
        hard.set_filter(Filter::Hard);
        let mut soft = hard;
        soft.set_filter(Filter::Soft);
        let mut both = hard;
        both.set_filter(Filter::Both);
        let mut with_hard = src.clone();
        render(&mut with_hard, 0.0, &[hard], &map);
        let mut with_soft = src.clone();
        render(&mut with_soft, 0.0, &[soft], &map);
        let mut with_both = src.clone();
        render(&mut with_both, 0.0, &[both], &map);
        for i in 0..src.data.len() {
            assert!(with_hard.data[i] <= plain.data[i] + 1e-12);
            assert!(with_soft.data[i] <= plain.data[i] + 1e-12);
            assert!(with_both.data[i] <= with_hard.data[i].min(with_soft.data[i]) + 1e-12);
        }
        // Darkest column (shadows): the hard burn bites, the soft one hardly.
        let x_dark = 2;
        let d_hard = plain.at(x_dark, 0) - with_hard.at(x_dark, 0);
        let d_soft = plain.at(x_dark, 0) - with_soft.at(x_dark, 0);
        assert!(d_hard > 2.0 * d_soft, "hard {d_hard} soft {d_soft}");
        // The whole-picture `both` equals the split render with both passes.
        let mut split = src.clone();
        render(&mut split, 0.0, &[both, hard, soft], &map);
        let mut double = src.clone();
        let mut twice = both;
        twice.set_ev(-2.0);
        render(&mut double, 0.0, &[twice], &map);
        for i in 0..src.data.len() {
            assert!((split.data[i] - double.data[i]).abs() < 1e-9);
        }
        // A look: the filter makes no difference.
        let look = Tone::Look(LookFile::neutral()).compile();
        let mut a = src.clone();
        render(&mut a, 0.0, &[hard], &look);
        let mut b = src.clone();
        render(&mut b, 0.0, &[both], &look);
        assert_eq!(a.data, b.data);
    }
}
