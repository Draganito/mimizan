//! The tone of a print: paper or look, and the one pass that turns the
//! placed linear picture into print values.
//!
//! A recipe prints either on paper (`paper.rs`: two exposures, nothing
//! else) or through a look (`curve.rs`: a point list, for the measurement
//! pipeline and older recipes). Both are a [`Tone`], compiled once into a
//! [`ToneMap`] and applied by [`render`], which also does the placing:
//! `L ← L · 2^place`, then the tone. With a dodge or a burn in the recipe
//! ([`render_with`]) the time of each pass is local (`burn.rs`): the tone
//! is evaluated with that pixel's times, [`ToneMap::eval_timed`].

use crate::burn::{Field, Times};
use crate::calib::{LookEncoding, LookFile};
use crate::curve::Curve;
use crate::paper::{PaperCurve, PaperTable, SplitGrade};
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

    /// Print value of a placed linear value where the exposure lasted
    /// `times` its time: the dodge or the burn at this pixel. Unit times
    /// are [`Self::eval`] to the bit. The paper gives each pass its own
    /// time; a look has one light, so it takes `times.all`, a stop of
    /// time as a stop of the scene the other way (there is no negative
    /// in a look to compress it), white when no time is left.
    #[inline]
    pub fn eval_timed(&self, l: f64, times: Times) -> f64 {
        match self {
            Self::Paper(p) => p.eval_timed(l, times.soft, times.hard),
            Self::Curve(c) => {
                if times.all == 1.0 {
                    c.eval(l)
                } else if times.all <= 0.0 {
                    c.eval(1.0)
                } else {
                    c.eval(l / times.all)
                }
            }
        }
    }

    /// [`Self::eval_timed`] made quick for the screen: the paper tabulated
    /// ([`PaperTable`]). A look is quick already and gives `None`.
    pub fn table(&self) -> Option<PaperTable> {
        match self {
            Self::Paper(p) => Some(PaperTable::new(p)),
            Self::Curve(_) => None,
        }
    }

    /// The scene factor a time factor `t` equals for this tone, when one
    /// exists: `eval_timed(l, t) == eval(l · k)` for every `l` with
    /// `l ≤ 1` and `l · k ≤ 1`. Both passes must see the same time. A
    /// stop of time is `1/γ_n` scene stops on paper and one on a look. The
    /// GUI uses it to keep its table; the print evaluates the time.
    pub fn scene_factor(&self, times: Times) -> Option<f64> {
        match self {
            Self::Paper(_) if times.is_even() && times.soft > 0.0 => {
                Some(times.soft.powf(-1.0 / crate::paper::NEGATIVE_GAMMA))
            }
            Self::Curve(_) if times.all > 0.0 => Some(1.0 / times.all),
            _ => None,
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
    #[cfg(test)]
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
/// entry and print values on return. Zero placing evaluates the tone on
/// the untouched value, so the result is bit-identical to [`place`]
/// followed by the tone.
pub fn render(plane: &mut Plane, place_stops: f64, tone: &ToneMap) {
    if place_stops == 0.0 {
        plane.data.par_iter_mut().for_each(|v| *v = tone.eval(*v));
    } else {
        let global = 2f64.powf(place_stops);
        plane.data.par_iter_mut().for_each(|v| *v = tone.eval(*v * global));
    }
}

/// [`render`] with the recipe's dodges and burns: every pixel is placed,
/// then evaluated with the times `field` gives at that pixel. `origin` is
/// where the plane's pixel (0, 0) lies in the field's coordinates (a tile
/// of a larger picture). No field is [`render`], bit for bit; so is every
/// pixel no burn reaches.
pub fn render_with(
    plane: &mut Plane,
    place_stops: f64,
    tone: &ToneMap,
    field: Option<&Field>,
    origin: (usize, usize),
) {
    let Some(field) = field else {
        return render(plane, place_stops, tone);
    };
    let global = if place_stops == 0.0 { 1.0 } else { 2f64.powf(place_stops) };
    let width = plane.width.max(1);
    let place = |v: f64| if place_stops == 0.0 { v } else { v * global };
    plane.data.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let line = field.row(origin.1 + y);
        let span = line.span(origin.0, origin.0 + row.len());
        for (x, v) in row.iter_mut().enumerate() {
            let l = place(*v);
            *v = if span.contains(&(origin.0 + x)) {
                let t = line.at(origin.0 + x);
                if t.is_unit() {
                    tone.eval(l)
                } else {
                    tone.eval_timed(l, t)
                }
            } else {
                tone.eval(l)
            };
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::burn::{Light, Stroke};
    use crate::straighten::Affine;

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

    /// A burn on the left half of a 64×40 plane: those pixels are the
    /// timed evaluation, the rest is `render` to the bit, and a tile of
    /// the plane rendered with its origin agrees with the whole.
    #[test]
    fn a_burn_renders_its_pixels_and_leaves_the_rest() {
        let src = ramp(64, 40);
        // Normalised by the plane itself: the unit is 64, the centre (32, 20).
        let norm = crate::burn::normalize(64, 40);
        let burns = [Stroke::Card {
            x: 0.0,
            y: 0.0,
            angle: 180.0,
            travel: 0.0,
            feather: 0.0,
            stops: 1.0,
            light: Light::Both,
        }];
        let field = Field::new(&burns, norm).unwrap();
        for tone in [Tone::paper(), Tone::Paper(SplitGrade::new(-1.0, 0.5)), Tone::Look(LookFile::neutral())]
        {
            let map = tone.compile();
            let mut burned = src.clone();
            render_with(&mut burned, 0.3, &map, Some(&field), (0, 0));
            let mut plain = src.clone();
            render(&mut plain, 0.3, &map);
            let g = 2f64.powf(0.3);
            for y in 0..40 {
                for x in 0..64 {
                    let (b, p, s) = (burned.at(x, y), plain.at(x, y), src.at(x, y));
                    if x < 32 {
                        let t = Times { soft: 2.0, hard: 2.0, all: 2.0 };
                        assert_eq!(
                            b.to_bits(),
                            map.eval_timed(s * g, t).to_bits(),
                            "{} {x},{y}",
                            tone.name()
                        );
                        assert!(b < p, "more time is a darker print at {x},{y}");
                    } else {
                        assert_eq!(b.to_bits(), p.to_bits(), "{} {x},{y}", tone.name());
                    }
                }
            }
            // A tile from (20, 10) of the same plane, rendered with its origin.
            let mut tile = Plane::zeros(24, 12);
            for y in 0..12 {
                tile.row_mut(y).copy_from_slice(&src.row(10 + y)[20..44]);
            }
            render_with(&mut tile, 0.3, &map, Some(&field), (20, 10));
            for y in 0..12 {
                for x in 0..24 {
                    assert_eq!(tile.at(x, y).to_bits(), burned.at(20 + x, 10 + y).to_bits());
                }
            }
            // No field is `render`.
            let mut none = src.clone();
            render_with(&mut none, 0.3, &map, None, (0, 0));
            assert_eq!(none.data, plain.data);
            // The scene factor stands in for an even time where the table may be used.
            let t = Times { soft: 2.0, hard: 2.0, all: 2.0 };
            let k = map.scene_factor(t).unwrap();
            for &l in &[0.01, 0.18, 0.4] {
                assert!((map.eval_timed(l, t) - map.eval(l * k)).abs() < 1e-9, "{} {l}", tone.name());
            }
            assert!(
                map.scene_factor(Times { soft: 2.0, hard: 1.0, all: 2.0 }).is_none()
                    || tone.as_paper().is_none()
            );
            assert_eq!(map.eval_timed(0.18, Times::UNIT).to_bits(), map.eval(0.18).to_bits());
            assert_eq!(map.eval_timed(0.18, Times { soft: 0.0, hard: 0.0, all: 0.0 }), map.white());
        }
        // A field read through a window: pixel (0, 0) of a tile that starts
        // at (20, 10) is the same point.
        let shifted = field.through(&Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 20.0, ty: 10.0 });
        assert_eq!(shifted.times_at(0, 0), field.times_at(20, 10));
    }
}
