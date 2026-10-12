//! The tone of a print: paper or look, and the one pass that turns the
//! placed linear picture into print values.
//!
//! A recipe prints either on paper (`paper.rs`: two exposures, nothing
//! else) or through a look (`curve.rs`: a point list, for the measurement
//! pipeline and older recipes). Both are a [`Tone`], compiled once into a
//! [`ToneMap`] and applied by [`render`], which also does the placing:
//! `L ← L · 2^place`, then the tone.

use crate::calib::{LookEncoding, LookFile};
use crate::curve::Curve;
use crate::paper::{PaperCurve, SplitGrade};
use crate::plane::Plane;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Placing within ±5 stops: more than that is not a print decision.
pub const PLACE_LIMIT: f64 = 5.0;

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
    /// Print value of a placed linear value.
    #[inline]
    pub fn eval(&self, l: f64) -> f64 {
        match self {
            Self::Paper(p) => p.eval(l),
            Self::Curve(c) => c.eval(l),
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

    /// Apply to a plane that is already placed.
    pub fn apply(&self, p: &Plane) -> Plane {
        let mut out = Plane::zeros(p.width, p.height);
        out.data.par_iter_mut().zip(&p.data).for_each(|(o, &v)| *o = self.eval(v));
        out
    }
}

/// `L ← L · 2^place` in place. `place == 0` returns without touching a
/// value, so the untouched recipe is bit-identical to the run that never
/// had this step.
pub fn place(plane: &mut Plane, place: f64) {
    if place == 0.0 {
        return;
    }
    let global = 2f64.powf(place);
    plane.data.par_iter_mut().for_each(|v| *v *= global);
}

/// Placing and tone in one pass, in place: `plane` holds linear values on
/// entry and print values on return.
pub fn render(plane: &mut Plane, place_stops: f64, tone: &ToneMap) {
    place(plane, place_stops);
    plane.data.par_iter_mut().for_each(|v| *v = tone.eval(*v));
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert!(Tone::paper().as_paper().is_some() && look.as_paper().is_none());
        assert!(!Tone::paper().compile().is_identity() && look.compile().is_identity());
        assert_eq!(Tone::paper().compile().white(), 1.0);
    }

    /// The one pass is placing followed by the tone; zero placing leaves
    /// the linear values untouched.
    #[test]
    fn render_equals_place_then_tone() {
        let src = ramp(64, 40);
        for tone in [Tone::paper(), Tone::Paper(SplitGrade::new(-1.0, 0.5)), Tone::Look(LookFile::neutral())]
        {
            let map = tone.compile();
            let mut a = src.clone();
            render(&mut a, 0.3, &map);
            let mut b = src.clone();
            place(&mut b, 0.3);
            let b = map.apply(&b);
            assert_eq!(a.data, b.data, "{}", tone.name());
        }
        let mut untouched = src.clone();
        place(&mut untouched, 0.0);
        assert_eq!(untouched.data, src.data);
        let mut up = src.clone();
        place(&mut up, 1.0);
        assert!(up.data.iter().zip(&src.data).all(|(a, b)| *a == 2.0 * *b));
    }
}
