//! The paper: a variable-contrast printing paper exposed in two passes, the
//! split-grade print (Zonensystem paper, section 9 of the SPEC).
//!
//! There is no curve to draw. The print tone of a scene value is what a
//! sheet of multigrade paper gives after a *soft* exposure (filter 00) and
//! a *hard* exposure (filter 5) through a negative of that scene, and the
//! only two numbers a print has are those two exposures, in stops of time
//! (f-stop timing). Everything the darkroom can do to a tone scale — place
//! it, stretch it, compress it, give it a toe and a shoulder — falls out of
//! that; everything it cannot do (a kink, a local steepening, a zone
//! squeezed to nothing) is not expressible here either.
//!
//! The model, in the paper's own units (log₁₀ exposure on the paper):
//!
//! * The negative is a straight line of gamma [`NEGATIVE_GAMMA`] (normal
//!   development, N). A scene value `L` (grey card 0.18, clip 1) gives the
//!   paper the exposure `log₁₀ E = log₁₀2 · (t − γ_n · log₂ L)`, `t` the
//!   exposure time in stops. A stop of time is therefore worth `1/γ_n` scene
//!   stops: the negative has compressed the scene.
//! * Each pass exposes one emulsion component, whose fraction of the
//!   developable silver is a normal CDF in log exposure (a log-normal spread
//!   of grain sensitivity), `a = Φ((log₁₀E − μ)/σ)`. The width `σ` comes
//!   from the ISO range of that grade ([`ISO_R_SOFT`], [`ISO_R_HARD`]: the
//!   log exposure range between 0.04 above base and 90 % of Dmax), the
//!   position `μ` from speed matching: each pass alone at `t = 0` puts the
//!   grey card at the ISO speed point, [`SPEED_DENSITY`] above base.
//! * The two passes share the silver: `D/Dmax = 1 − (1 − a)(1 − b)`. The
//!   soft pass, with its long toe, is what the highlights see first; the
//!   hard pass, steep, is what fills the shadows. That is the darkroom's
//!   rule of thumb, and here it is the arithmetic.
//! * Reflectance `10^−D` relative to the paper base, with the paper's Dmax
//!   taken to the output's black (black point compensation), encoded with
//!   the 1/2.2 of the rest of the pipeline. Unexposed paper is 1.
//!
//! `t = 0` for both passes is the paper's *base exposure*: the pair that
//! puts the grey card at [`GREY_DENSITY`] and the clip at [`CLIP_DENSITY`]
//! (a textured white, Zone VIII), solved once from the constants. The user
//! only ever moves the two times from there, globally with the sliders, or
//! locally with dodge & burn through one filter or both.

use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// Maximum reflection density of the paper above its base (glossy RC).
pub const DMAX: f64 = 2.0;
/// ISO range of the soft component alone (grade 00) and of the hard one
/// (grade 5), as `100 · Δlog₁₀E`.
pub const ISO_R_SOFT: f64 = 180.0;
pub const ISO_R_HARD: f64 = 60.0;
/// Straight-line gamma of the negative at normal development.
pub const NEGATIVE_GAMMA: f64 = 0.6;
/// ISO speed point of the paper, density above base: the grey card lands
/// here when one pass alone is exposed for its matched time.
pub const SPEED_DENSITY: f64 = 0.6;
/// Density of the grey card at the base exposure: 18 % reflectance.
pub const GREY_DENSITY: f64 = 0.67;
/// Density of the clip (linear 1) at the base exposure: Zone VIII, the
/// lightest textured white.
pub const CLIP_DENSITY: f64 = 0.10;
/// The ISO range is measured between these fractions of Dmax.
const R_LOW: f64 = 0.04 / DMAX;
const R_HIGH: f64 = 0.9;
/// Exposure times stay within this many stops of the base exposure.
pub const STOP_LIMIT: f64 = 3.0;
/// Name of the one paper this version knows.
pub const PAPER_NAME: &str = "multigrade";

const LOG10_2: f64 = std::f64::consts::LOG10_2;
const GREY: f64 = crate::zone::GRAY_CARD;

/// Standard normal CDF.
#[inline]
fn phi(x: f64) -> f64 {
    0.5 * libm::erfc(-x / std::f64::consts::SQRT_2)
}

/// Its inverse, by bisection (only used for the constants).
fn probit(p: f64) -> f64 {
    let (mut lo, mut hi) = (-12.0, 12.0);
    for _ in 0..200 {
        let mid = 0.5 * (lo + hi);
        if phi(mid) < p {
            lo = mid;
        } else {
            hi = mid;
        }
    }
    0.5 * (lo + hi)
}

/// One emulsion component: a normal CDF in log₁₀ exposure.
#[derive(Clone, Copy, Debug)]
struct Component {
    mu: f64,
    sigma: f64,
}

impl Component {
    #[inline]
    fn fraction(&self, log_e: f64) -> f64 {
        phi((log_e - self.mu) / self.sigma)
    }
}

/// The paper's constants, solved once.
#[derive(Clone, Copy, Debug)]
pub struct Paper {
    soft: Component,
    hard: Component,
    /// Base exposure of each pass, in stops, on top of the speed-matched
    /// time: `t = 0` in a [`SplitGrade`] means this.
    base_soft: f64,
    base_hard: f64,
    /// `10^−Dmax`, the reflectance of the paper's black.
    black: f64,
}

/// log₁₀ paper exposure of scene value `l` at `t` stops of time.
#[inline]
fn log_exposure(l: f64, t: f64) -> f64 {
    LOG10_2 * (t - NEGATIVE_GAMMA * l.log2())
}

impl Paper {
    fn solve() -> Self {
        let span = probit(R_HIGH) - probit(R_LOW);
        let sigma = |r: f64| r / 100.0 / span;
        // Speed matching: at t = 0 the grey card alone reaches the speed point.
        let speed = SPEED_DENSITY / DMAX;
        let mu = |s: f64| log_exposure(GREY, 0.0) - probit(speed) * s;
        let soft = Component { mu: mu(sigma(ISO_R_SOFT)), sigma: sigma(ISO_R_SOFT) };
        let hard = Component { mu: mu(sigma(ISO_R_HARD)), sigma: sigma(ISO_R_HARD) };
        let mut p = Self { soft, hard, base_soft: 0.0, base_hard: 0.0, black: 10f64.powf(-DMAX) };
        // Base exposure: the hard time that puts the grey card at its density
        // for a given soft time, and the soft time that then puts the clip at
        // its density. Both are monotone in the time, so two nested bisections.
        let hard_for = |p: &Self, soft: f64| -> f64 {
            let (mut lo, mut hi) = (-20.0, 20.0);
            for _ in 0..200 {
                let mid = 0.5 * (lo + hi);
                if p.fraction_at(GREY, soft, mid) < GREY_DENSITY / DMAX {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            0.5 * (lo + hi)
        };
        let (mut lo, mut hi) = (-20.0, 20.0);
        for _ in 0..200 {
            let mid = 0.5 * (lo + hi);
            let h = hard_for(&p, mid);
            if p.fraction_at(1.0, mid, h) < CLIP_DENSITY / DMAX {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        p.base_soft = 0.5 * (lo + hi);
        p.base_hard = hard_for(&p, p.base_soft);
        p
    }

    /// `D/Dmax` for scene value `l` under absolute times (stops over the
    /// speed-matched time). `l ≤ 0` is a clear negative: all the light the
    /// lamp has, the paper's black.
    #[inline]
    fn fraction_at(&self, l: f64, soft: f64, hard: f64) -> f64 {
        if l <= 0.0 {
            return 1.0;
        }
        let l = l.min(1.0);
        let a = self.soft.fraction(log_exposure(l, soft));
        let b = self.hard.fraction(log_exposure(l, hard));
        1.0 - (1.0 - a) * (1.0 - b)
    }

    /// Encoded print value of a density fraction: reflectance relative to the
    /// base, the paper's black taken to 0, gamma 1/2.2.
    #[inline]
    fn encode(&self, fraction: f64) -> f64 {
        let r = 10f64.powf(-DMAX * fraction);
        ((r - self.black) / (1.0 - self.black)).clamp(0.0, 1.0).powf(1.0 / crate::zone::GAMMA)
    }
}

/// The one paper.
pub fn paper() -> &'static Paper {
    static PAPER: LazyLock<Paper> = LazyLock::new(Paper::solve);
    &PAPER
}

/// The two exposures of a split-grade print, in stops of time from the
/// paper's base exposure. The recipe's tone when it prints on paper.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SplitGrade {
    /// Which paper; only [`PAPER_NAME`] exists.
    pub paper: String,
    /// Soft pass (filter 00), stops of time.
    #[serde(default)]
    pub soft: f64,
    /// Hard pass (filter 5), stops of time.
    #[serde(default)]
    pub hard: f64,
}

impl Default for SplitGrade {
    fn default() -> Self {
        Self { paper: PAPER_NAME.into(), soft: 0.0, hard: 0.0 }
    }
}

impl SplitGrade {
    pub fn new(soft: f64, hard: f64) -> Self {
        Self { soft, hard, ..Self::default() }
    }

    pub fn validate(&self) -> crate::Result<()> {
        if self.paper != PAPER_NAME {
            return Err(crate::Error::Invalid(format!("paper '{}' unknown (only {PAPER_NAME})", self.paper)));
        }
        for (v, what) in [(self.soft, "soft"), (self.hard, "hard")] {
            if !v.is_finite() || v.abs() > STOP_LIMIT {
                return Err(crate::Error::Invalid(format!(
                    "{what} exposure {v} not within ±{STOP_LIMIT} stops"
                )));
            }
        }
        Ok(())
    }

    /// The tone function of these two exposures.
    pub fn curve(&self) -> PaperCurve {
        let p = paper();
        PaperCurve { paper: *p, soft: p.base_soft + self.soft, hard: p.base_hard + self.hard }
    }
}

/// A split-grade print ready to evaluate: both times absolute.
#[derive(Clone, Copy, Debug)]
pub struct PaperCurve {
    paper: Paper,
    soft: f64,
    hard: f64,
}

impl PaperCurve {
    /// Print value of scene value `l` when both passes see the same `l`.
    #[inline]
    pub fn eval(&self, l: f64) -> f64 {
        self.paper.encode(self.paper.fraction_at(l, self.soft, self.hard))
    }

    /// Print value when dodge & burn gave the two passes different scene
    /// values (a shape through one filter): `l_soft` for the soft pass,
    /// `l_hard` for the hard one.
    #[inline]
    pub fn eval_split(&self, l_soft: f64, l_hard: f64) -> f64 {
        let p = &self.paper;
        if l_soft <= 0.0 || l_hard <= 0.0 {
            return p.encode(1.0);
        }
        let a = p.soft.fraction(log_exposure(l_soft.min(1.0), self.soft));
        let b = p.hard.fraction(log_exposure(l_hard.min(1.0), self.hard));
        p.encode(1.0 - (1.0 - a) * (1.0 - b))
    }

    /// Unexposed paper: what the border is made of. (A scene value of 0 is
    /// the other end: a clear negative, the paper's black.)
    pub fn white(&self) -> f64 {
        self.paper.encode(0.0)
    }

    /// Density above base of scene value `l`.
    pub fn density(&self, l: f64) -> f64 {
        DMAX * self.paper.fraction_at(l, self.soft, self.hard)
    }

    /// The scene value that prints as `v` (bisection; `v` outside the
    /// reachable range gives the nearer end).
    pub fn linear_for(&self, v: f64) -> f64 {
        let (mut lo, mut hi) = (0.0f64, 1.0f64);
        for _ in 0..80 {
            let mid = 0.5 * (lo + hi);
            if self.eval(mid) < v {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    }

    /// Stops of time, added to both passes, that bring the scene value `l`
    /// to the print value `v`: the whole bar dragged. Clamped to ±8.
    pub fn shift_for(&self, l: f64, v: f64) -> f64 {
        let (mut lo, mut hi) = (-8.0f64, 8.0f64);
        for _ in 0..80 {
            let mid = 0.5 * (lo + hi);
            let c = Self { soft: self.soft + mid, hard: self.hard + mid, ..*self };
            if c.eval(l) > v {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    }

    /// Log exposure range of the combined curve between the ISO points,
    /// `100 · Δlog₁₀E`, i.e. the ISO R this split equals.
    pub fn iso_range(&self) -> f64 {
        // Both passes move together with the exposure, so the range is a
        // function of the time shift alone: find the shifts that put the
        // grey card at the two ISO fractions.
        let shift_to = |f: f64| -> f64 {
            let (mut lo, mut hi) = (-30.0f64, 30.0f64);
            for _ in 0..100 {
                let mid = 0.5 * (lo + hi);
                if self.paper.fraction_at(GREY, self.soft + mid, self.hard + mid) < f {
                    lo = mid;
                } else {
                    hi = mid;
                }
            }
            0.5 * (lo + hi)
        };
        (shift_to(R_HIGH) - shift_to(R_LOW)) * LOG10_2 * 100.0
    }

    /// Scene stops between the first tone and the near-black of the paper.
    pub fn scene_range_stops(&self) -> f64 {
        self.iso_range() / 100.0 / LOG10_2 / NEGATIVE_GAMMA
    }

    /// The single filter grade this split equals, 00 (−1) … 5, from the ISO
    /// range by the usual 20 R per grade (grade 2 = 120).
    pub fn grade(&self) -> f64 {
        ((160.0 - self.iso_range()) / 20.0).clamp(-1.0, 5.0)
    }
}

/// A grade as the darkroom writes it: 00, 0, ½, 1 … 5.
pub fn grade_label(g: f64) -> String {
    if g < -0.75 {
        return "00".into();
    }
    let halves = (g.max(0.0) * 2.0).round() as i32;
    match (halves / 2, halves % 2) {
        (n, 0) => n.to_string(),
        (0, _) => "½".into(),
        (n, _) => format!("{n}½"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(soft: f64, hard: f64, l: f64) -> f64 {
        SplitGrade::new(soft, hard).curve().eval(l)
    }

    /// The constants come out as designed: each component alone has its ISO
    /// range, the base exposure puts the grey card and the clip where the
    /// anchors say, unexposed paper is white.
    #[test]
    fn anchors_and_ranges() {
        let p = paper();
        let c = SplitGrade::default().curve();
        assert!((c.density(GREY) - GREY_DENSITY).abs() < 1e-9);
        assert!((c.density(1.0) - CLIP_DENSITY).abs() < 1e-9);
        assert_eq!(c.white(), 1.0);
        assert_eq!(c.eval(0.0), 0.0);
        assert!(c.eval(1.0) < 1.0 && c.eval(1.0) > 0.85);
        assert_eq!(c.eval_split(0.0, 0.5), 0.0);
        assert_eq!(c.eval_split(0.3, 0.3), c.eval(0.3));
        // One pass alone at its speed-matched time: speed point and ISO R.
        let soft_only = PaperCurve { paper: *p, soft: 0.0, hard: -60.0 };
        let hard_only = PaperCurve { paper: *p, soft: -60.0, hard: 0.0 };
        assert!((soft_only.density(GREY) - SPEED_DENSITY).abs() < 1e-9);
        assert!((hard_only.density(GREY) - SPEED_DENSITY).abs() < 1e-9);
        assert!((soft_only.iso_range() - ISO_R_SOFT).abs() < 0.01);
        assert!((hard_only.iso_range() - ISO_R_HARD).abs() < 0.01);
        assert_eq!(grade_label(soft_only.grade()), "00");
        assert_eq!(grade_label(hard_only.grade()), "5");
        // The base split is a middle grade over five to six scene stops.
        let g = c.grade();
        assert!((2.0..=3.5).contains(&g), "grade {g}");
        assert!((5.0..=6.5).contains(&c.scene_range_stops()), "{}", c.scene_range_stops());
    }

    /// Monotone and bounded for any pair of times in range; a stop of time
    /// on both passes is a stop of scene the other way, scaled by the
    /// negative's gamma.
    #[test]
    fn monotone_and_time_is_scene() {
        for &(s, h) in &[(0.0, 0.0), (-3.0, 3.0), (3.0, -3.0), (1.5, -0.5), (-3.0, -3.0), (3.0, 3.0)] {
            let c = SplitGrade::new(s, h).curve();
            let mut last = -1.0;
            for i in 0..=4000 {
                let v = c.eval(i as f64 / 4000.0);
                assert!((0.0..=1.0).contains(&v));
                assert!(v >= last - 1e-12, "not monotone at {i} for {s},{h}");
                last = v;
            }
        }
        let c = SplitGrade::new(0.3, -0.2).curve();
        let shifted = SplitGrade::new(0.3 + 0.6, -0.2 + 0.6).curve();
        for &l in &[0.01, 0.05, 0.18, 0.4, 0.9] {
            // +0.6 stops of time on both = the scene darker by 0.6/γ_n stops.
            let equiv = c.eval(l * 2f64.powf(-0.6 / NEGATIVE_GAMMA));
            assert!((shifted.eval(l) - equiv).abs() < 1e-9, "{l}");
        }
    }

    /// A stop more of soft light greys the highlights and hardly touches
    /// the shadows; a stop less of hard light opens the shadows and leaves
    /// the highlights where they were. (More hard light darkens shadows
    /// that are already near the paper's black, so the shadow side is
    /// tested in the direction that has room.)
    #[test]
    fn soft_is_highlights_hard_is_shadows() {
        let (zone_iii, zone_vii) = (GREY / 4.0, GREY * 4.0);
        let base = SplitGrade::default().curve();
        let soft = SplitGrade::new(1.0, 0.0).curve();
        let hard = SplitGrade::new(0.0, -1.0).curve();
        let d_soft = (base.eval(zone_vii) - soft.eval(zone_vii), base.eval(zone_iii) - soft.eval(zone_iii));
        let d_hard = (hard.eval(zone_vii) - base.eval(zone_vii), hard.eval(zone_iii) - base.eval(zone_iii));
        assert!(d_soft.0 > 4.0 * d_soft.1, "soft: highlights {} shadows {}", d_soft.0, d_soft.1);
        assert!(d_hard.1 > 10.0 * d_hard.0, "hard: highlights {} shadows {}", d_hard.0, d_hard.1);
        // More hard light is a harder grade, more soft light a softer one.
        assert!(SplitGrade::new(0.0, 1.0).curve().grade() > base.grade() && soft.grade() < base.grade());
        assert!(hard.grade() < base.grade());
    }

    #[test]
    fn inverse_and_shift() {
        let c = SplitGrade::new(-0.4, 0.7).curve();
        for &l in &[0.02, 0.18, 0.6] {
            assert!((c.linear_for(c.eval(l)) - l).abs() < 1e-9);
        }
        let s = c.shift_for(0.18, 0.3);
        let moved = SplitGrade::new(-0.4 + s, 0.7 + s).curve();
        assert!((moved.eval(0.18) - 0.3).abs() < 1e-9);
        assert_eq!(grade_label(2.0), "2");
        assert_eq!(grade_label(2.4), "2½");
        assert_eq!(grade_label(0.3), "½");
        assert_eq!(grade_label(-0.2), "0");
        assert_eq!(grade_label(4.8), "5");
    }

    #[test]
    fn split_grade_json_and_validation() {
        let s = SplitGrade::new(0.5, -0.25);
        s.validate().unwrap();
        let json = serde_json::to_string(&s).unwrap();
        assert_eq!(json, r#"{"paper":"multigrade","soft":0.5,"hard":-0.25}"#);
        let back: SplitGrade = serde_json::from_str(&json).unwrap();
        assert_eq!(back, s);
        let minimal: SplitGrade = serde_json::from_str(r#"{"paper":"multigrade"}"#).unwrap();
        assert_eq!(minimal, SplitGrade::default());
        assert!(SplitGrade::new(3.5, 0.0).validate().is_err());
        assert!(SplitGrade { paper: "fb".into(), ..SplitGrade::default() }.validate().is_err());
        assert!(tone(0.0, 0.0, 0.18) > tone(0.0, 0.0, 0.17));
    }
}
