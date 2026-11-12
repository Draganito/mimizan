//! The paper: a variable-contrast printing paper exposed in two passes, the
//! split-grade print.
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
//! * Each pass exposes one emulsion component. The hard pass (filter 5) is a
//!   normal CDF in log exposure, `b = Φ((log₁₀E − μ)/σ)`, its width `σ` from
//!   the ISO range [`ISO_R_HARD`]. The soft pass (filter 00) is the measured
//!   grade-00 curve of Ilford Multigrade RC Deluxe: the logit fitted to the
//!   separated stroke of the datasheet, and below that stroke a toe laid so
//!   the ISO range is [`ISO_R_SOFT`] (0.04 above base to 90 % of Dmax). Both
//!   are speed-matched: each pass alone at `t = 0` puts the grey card at the
//!   ISO speed point, [`SPEED_DENSITY`] above base.
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
//! only ever moves the two times from there.

use serde::{Deserialize, Serialize};
use std::sync::LazyLock;

/// Maximum reflection density of the paper above its base (glossy RC).
pub const DMAX: f64 = 2.0;
/// ISO range of the soft component (grade 00 of Ilford Multigrade RC Deluxe)
/// and of the hard one (grade 5), as `100 · Δlog₁₀E`.
pub const ISO_R_SOFT: f64 = 160.0;
pub const ISO_R_HARD: f64 = 50.0;
/// Straight-line gamma of the negative at normal development.
pub const NEGATIVE_GAMMA: f64 = 0.6;
/// ISO speed point of the paper, density above base: the grey card lands
/// here when one pass alone is exposed for its matched time.
pub const SPEED_DENSITY: f64 = 0.6;
/// Density above base at which the 18 % grey card of the scene prints at
/// the base exposure. On the paper that is a reflectance of 21 % of the
/// base (`10^−0.67`); 18 % would be a density of 0.745.
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

/// Logit of the measured grade-00 stroke, `z(h) = a + bh + ch² + dh³`.
/// `h` counts half-stops of relative log exposure from 2.0 on that chart.
const SOFT_A: f64 = -0.05416490427546971;
const SOFT_B: f64 = 1.1447781589403825;
const SOFT_C: f64 = 0.04294224462390111;
const SOFT_D: f64 = 0.1770836763782222;
/// The separated stroke. Left of this the grades share one line on the chart,
/// so the toe is not a measurement.
const SOFT_H1: f64 = -0.8533834586466166;
const SOFT_H2: f64 = 2.048872180451122;

fn soft_cubic_z(h: f64) -> f64 {
    SOFT_A + SOFT_B * h + SOFT_C * h * h + SOFT_D * h * h * h
}

fn soft_cubic_dz(h: f64) -> f64 {
    SOFT_B + 2.0 * SOFT_C * h + 3.0 * SOFT_D * h * h
}

fn soft_cubic_f(h: f64) -> f64 {
    let z = soft_cubic_z(h).clamp(-40.0, 40.0);
    1.0 / (1.0 + (-z).exp())
}

/// `h` of the speed point and the quadratic coefficient of the toe, solved
/// so the silver fraction is [`SPEED_DENSITY`]/[`DMAX`] at `h_speed` and the
/// log exposure from fraction [`R_LOW`] to [`R_HIGH`] is [`ISO_R_SOFT`]/100.
fn soft_anchors() -> (f64, f64) {
    let h_at = |target: f64| {
        let (mut lo, mut hi) = (SOFT_H1, SOFT_H2);
        for _ in 0..80 {
            let mid = 0.5 * (lo + hi);
            if soft_cubic_f(mid) < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        0.5 * (lo + hi)
    };
    let h_speed = h_at(SPEED_DENSITY / DMAX);
    let h_low = h_at(R_HIGH) - 2.0 * (ISO_R_SOFT / 100.0);
    let u = h_low - SOFT_H1;
    let z_low = (R_LOW / (1.0 - R_LOW)).ln();
    let toe = (z_low - soft_cubic_z(SOFT_H1) - soft_cubic_dz(SOFT_H1) * u) / (u * u);
    (h_speed, toe)
}

/// One emulsion component.
#[derive(Clone, Copy, Debug)]
enum Component {
    /// Grade 5: a normal CDF in log₁₀ exposure. `mu` is its median.
    Normal { mu: f64, sigma: f64 },
    /// Grade 00: the measured curve. `mu` is the log₁₀ exposure of the speed
    /// point, `h_speed` that point's chart coordinate, `toe` the quadratic
    /// coefficient below the separated stroke.
    Soft { mu: f64, h_speed: f64, toe: f64 },
}

/// Where the toe's quadratic turns: below this `u` it would rise again.
/// A toe that keeps falling (`toe ≤ 0`) has no such point. Exposures
/// that far under the stroke (some forty stops) only come from a dodge
/// that takes nearly all the time away; the toe is held at its lowest
/// there, so the curve stays monotone down to no light at all.
fn toe_floor(toe: f64) -> f64 {
    if toe > 0.0 {
        -soft_cubic_dz(SOFT_H1) / (2.0 * toe)
    } else {
        f64::NEG_INFINITY
    }
}

impl Component {
    #[inline]
    fn fraction(&self, log_e: f64) -> f64 {
        match *self {
            Component::Normal { mu, sigma } => phi((log_e - mu) / sigma),
            Component::Soft { mu, h_speed, toe } => {
                let h = h_speed + 2.0 * (log_e - mu);
                let z = if h < SOFT_H1 {
                    let u = (h - SOFT_H1).max(toe_floor(toe));
                    soft_cubic_z(SOFT_H1) + soft_cubic_dz(SOFT_H1) * u + toe * u * u
                } else if h > SOFT_H2 {
                    soft_cubic_z(SOFT_H2) + soft_cubic_dz(SOFT_H2) * (h - SOFT_H2)
                } else {
                    soft_cubic_z(h)
                };
                let z = z.clamp(-40.0, 40.0);
                1.0 / (1.0 + (-z).exp())
            }
        }
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
        let (h_speed, toe) = soft_anchors();
        let soft = Component::Soft { mu: log_exposure(GREY, 0.0), h_speed, toe };
        let hard = Component::Normal { mu: mu(sigma(ISO_R_HARD)), sigma: sigma(ISO_R_HARD) };
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

    /// [`Self::fraction_at`] with each pass's time multiplied by a factor:
    /// the dodge or burn at this point. A factor of 1 is `fraction_at` to
    /// the bit. A factor of 0 is a pass that never happened: no silver from
    /// it, even under a clear negative.
    #[inline]
    fn fraction_timed(&self, l: f64, soft: f64, hard: f64, soft_time: f64, hard_time: f64) -> f64 {
        if soft_time == 1.0 && hard_time == 1.0 {
            return self.fraction_at(l, soft, hard);
        }
        let pass = |c: &Component, t: f64, time: f64| -> f64 {
            if time <= 0.0 {
                0.0
            } else if l <= 0.0 {
                1.0
            } else {
                c.fraction(log_exposure(l.min(1.0), t + time.log2()))
            }
        };
        let a = pass(&self.soft, soft, soft_time);
        let b = pass(&self.hard, hard, hard_time);
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

    /// The pair `notches` twelfths of a stop harder, or softer below zero,
    /// with the print's mid-grey left where it is: a change of grade and
    /// nothing else, as changing one speed-matched filter for another.
    ///
    /// A pair is read as the two things a printer sets. The grade is the
    /// hard exposure over the soft. The exposure is what both have beyond
    /// the pair of that same grade which prints the grey card at
    /// [`GREY_DENSITY`] ([`matched_mean`]). Only the grade changes here,
    /// so the tone that prints as that mid-grey keeps its place, to the
    /// twelfth of a stop the pair is written in; at the base exposure it
    /// is the grey card. Adding the same stops to both exposures is the
    /// other half and changes only the exposure.
    ///
    /// The grade stops at [`grade_ends`], and before an exposure would
    /// leave ±[`STOP_LIMIT`]. A pair past an end stays there on a step
    /// outwards and comes back on one inwards. Both exposures land on
    /// twelfths, and the same steps back give the same pair again.
    pub fn graded(&self, notches: i32) -> Self {
        let limit = (STOP_LIMIT * 12.0).round() as i32;
        let twelfths =
            |v: f64| if v.is_finite() { ((v * 12.0).round() as i32).clamp(-limit, limit) } else { 0 };
        let (soft, hard) = (twelfths(self.soft), twelfths(self.hard));
        // Twenty-fourths from here on: the grade's half and the mean are
        // whole in them, and of one parity when the pair is on twelfths.
        let grade = hard - soft;
        let exposure = soft + hard - matched_mean(grade);
        let (soft_end, hard_end) = grade_ends();
        let to = grade.saturating_add(notches).clamp(soft_end.min(grade), hard_end.max(grade));
        let pair = |g: i32| {
            let mean = exposure + matched_mean(g);
            (mean - g, mean + g)
        };
        let fits = |g: i32| {
            let (s, h) = pair(g);
            s.abs() <= 2 * limit && h.abs() <= 2 * limit
        };
        // The furthest grade on the way that both exposures can reach; the
        // pair as it stands always does.
        let step = if to >= grade { 1 } else { -1 };
        let mut g = to;
        while g != grade && !fits(g) {
            g -= step;
        }
        let (s, h) = pair(g);
        Self { soft: f64::from(s) / 24.0, hard: f64::from(h) / 24.0, paper: self.paper.clone() }
    }
}

/// The mean of the two exposures, in twenty-fourths of a stop from the
/// base exposure, of the pair with `grade` twelfths of hard over soft that
/// prints the grey card at [`GREY_DENSITY`]: the speed matching of the
/// grades. Zero at the base print. It is rounded to the twenty-fourth of
/// the grade's own parity, so that the pair it makes lies on twelfths.
fn matched_mean(grade: i32) -> i32 {
    static MEANS: LazyLock<Vec<i32>> = LazyLock::new(|| {
        let p = paper();
        let reach = (2.0 * STOP_LIMIT * 12.0).round() as i32;
        (-reach..=reach)
            .map(|grade| {
                let half = f64::from(grade) / 24.0;
                // The density of the grey card rises with the time of both.
                let (mut lo, mut hi) = (-16.0f64, 16.0f64);
                for _ in 0..80 {
                    let mid = 0.5 * (lo + hi);
                    if p.fraction_at(GREY, p.base_soft - half + mid, p.base_hard + half + mid)
                        < GREY_DENSITY / DMAX
                    {
                        lo = mid;
                    } else {
                        hi = mid;
                    }
                }
                let mean = 0.5 * (lo + hi) * 24.0;
                grade + 2 * ((mean - f64::from(grade)) / 2.0).round() as i32
            })
            .collect()
    });
    let reach = (MEANS.len() / 2) as i32;
    MEANS[(grade.clamp(-reach, reach) + reach) as usize]
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

    /// Print value of scene value `l` where the soft pass lasted
    /// `soft_time` times its time and the hard pass `hard_time` times its
    /// own: the dodge or the burn at one point (`burn.rs`). Both 1 is
    /// [`Self::eval`] to the bit. Time, not scene: a clipped highlight
    /// dodged goes towards paper white, where a placing would hold it at
    /// the clip.
    #[inline]
    pub fn eval_timed(&self, l: f64, soft_time: f64, hard_time: f64) -> f64 {
        self.paper.encode(self.paper.fraction_timed(l, self.soft, self.hard, soft_time, hard_time))
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

    /// The single filter grade this split equals, 00 (−1) … 5, read from the
    /// ISO range against the published ranges of Ilford Multigrade RC Deluxe:
    /// 160, 130, 110, 90, 70, 60, 50.
    pub fn grade(&self) -> f64 {
        const R: [f64; 7] = [160.0, 130.0, 110.0, 90.0, 70.0, 60.0, 50.0];
        let r = self.iso_range();
        if r >= R[0] {
            return -1.0;
        }
        if r <= R[6] {
            return 5.0;
        }
        for i in 0..6 {
            let (hi, lo) = (R[i], R[i + 1]);
            if r <= hi && r >= lo {
                let t = (hi - r) / (hi - lo);
                return (i as f64 - 1.0) + t;
            }
        }
        5.0
    }
}

/// Entries of each pass's table in [`PaperTable`].
const PASS_TABLE: usize = 8192;
/// Stops either side of the pass's time the table covers.
const PASS_SPAN: f64 = 40.0;
/// Entries of the density-to-print table.
const ENCODE_TABLE: usize = 16384;

/// A [`PaperCurve`] tabulated for the screen: each pass's fraction is a
/// function of one number, the stops of exposure it saw
/// (`time + log₂ T − γₙ log₂ l`), so two tables of that and one of the
/// encoding stand for the whole of [`PaperCurve::eval_timed`], read with
/// linear interpolation, within a part in a thousand. The print keeps the
/// exact evaluation.
#[derive(Clone, Debug)]
pub struct PaperTable {
    soft: Vec<f32>,
    hard: Vec<f32>,
    encode: Vec<f32>,
    /// `γₙ log₂ l` over the square root of `l` in 65536 steps: the
    /// shadows a 16-bit code would lump together are told apart.
    scene: Vec<f32>,
    /// Entries per stop of exposure.
    per_stop: f64,
    white: f64,
}

/// Entries of the scene table: 16-bit steps of `√l`.
const SCENE_TABLE: usize = 65536;

impl PaperTable {
    pub fn new(curve: &PaperCurve) -> Self {
        let per_stop = (PASS_TABLE - 1) as f64 / (2.0 * PASS_SPAN);
        // Entry `i` is the pass at `time + i / per_stop − PASS_SPAN` stops.
        let pass = |c: &Component, t: f64| -> Vec<f32> {
            (0..PASS_TABLE)
                .map(|i| c.fraction(LOG10_2 * (t - PASS_SPAN + i as f64 / per_stop)) as f32)
                .collect()
        };
        let soft = pass(&curve.paper.soft, curve.soft);
        let hard = pass(&curve.paper.hard, curve.hard);
        let encode = (0..ENCODE_TABLE)
            .map(|i| curve.paper.encode(i as f64 / (ENCODE_TABLE - 1) as f64) as f32)
            .collect();
        let scene = (0..SCENE_TABLE)
            .map(|i| (2.0 * NEGATIVE_GAMMA * (i.max(1) as f64 / (SCENE_TABLE - 1) as f64).log2()) as f32)
            .collect();
        Self { soft, hard, encode, scene, per_stop, white: curve.white() }
    }

    #[inline]
    fn read(table: &[f32], f: f64) -> f64 {
        if f <= 0.0 {
            return f64::from(table[0]);
        }
        let last = table.len() - 1;
        if f >= last as f64 {
            return f64::from(table[last]);
        }
        let i = f as usize;
        let t = f - i as f64;
        f64::from(table[i]) * (1.0 - t) + f64::from(table[i + 1]) * t
    }

    /// [`PaperCurve::eval_timed`] from the tables.
    #[inline]
    pub fn eval_timed(&self, l: f64, soft_time: f64, hard_time: f64) -> f64 {
        self.eval_logged(l, log2_time(soft_time), log2_time(hard_time))
    }

    /// `eval_timed` with the times given as their logarithms to base 2,
    /// `−∞` for a pass that did not run: a field's times repeat from
    /// pixel to pixel, their logarithms are taken once.
    #[inline]
    pub fn eval_logged(&self, l: f64, soft_log2: f64, hard_log2: f64) -> f64 {
        if l <= 0.0 {
            // A clear negative: the paper's black from any pass that ran.
            return if soft_log2 == f64::NEG_INFINITY && hard_log2 == f64::NEG_INFINITY {
                self.white
            } else {
                self.encode_fraction(1.0)
            };
        }
        // The scene table is over √l; below its first step is below any
        // paper's toe.
        let code = (l.min(1.0).sqrt() * (SCENE_TABLE - 1) as f64 + 0.5) as usize;
        let scene = f64::from(self.scene[code.clamp(1, SCENE_TABLE - 1)]);
        let pass = |table: &[f32], log_time: f64| -> f64 {
            if log_time == f64::NEG_INFINITY {
                0.0
            } else {
                Self::read(table, (log_time - scene + PASS_SPAN) * self.per_stop)
            }
        };
        let a = pass(&self.soft, soft_log2);
        let b = pass(&self.hard, hard_log2);
        self.encode_fraction(1.0 - (1.0 - a) * (1.0 - b))
    }

    #[inline]
    fn encode_fraction(&self, f: f64) -> f64 {
        Self::read(&self.encode, f.clamp(0.0, 1.0) * (ENCODE_TABLE - 1) as f64)
    }
}

/// A function of a time factor tabulated for the screen: fine from 0 to 2
/// (a 32768th apart), coarser from 2 to 64 (a 1024th), the function
/// itself below a 128th and from 64. A field's times, 1 plus the weights
/// times the gains, fall in the fine range unless a burn passes six stops.
struct TimeTable {
    fine: Vec<f32>,
    coarse: Vec<f32>,
    f: fn(f64) -> f64,
}

const TIME_FINE_PER: f64 = 32768.0;
const TIME_FINE_TO: f64 = 2.0;
const TIME_COARSE_PER: f64 = 1024.0;
const TIME_COARSE_TO: f64 = 64.0;
const TIME_FROM: f64 = 1.0 / 128.0;

impl TimeTable {
    fn new(f: fn(f64) -> f64) -> Self {
        let fine = (0..=(TIME_FINE_TO * TIME_FINE_PER) as usize)
            .map(|i| {
                let t = i as f64 / TIME_FINE_PER;
                if t < TIME_FROM {
                    0.0
                } else {
                    f(t) as f32
                }
            })
            .collect();
        let coarse = (0..=((TIME_COARSE_TO - TIME_FINE_TO) * TIME_COARSE_PER) as usize)
            .map(|i| f(TIME_FINE_TO + i as f64 / TIME_COARSE_PER) as f32)
            .collect();
        Self { fine, coarse, f }
    }

    #[inline]
    fn at(&self, t: f64) -> f64 {
        if t < TIME_FROM || t >= TIME_COARSE_TO || t.is_nan() {
            return (self.f)(t);
        }
        if t < TIME_FINE_TO {
            PaperTable::read(&self.fine, t * TIME_FINE_PER)
        } else {
            PaperTable::read(&self.coarse, (t - TIME_FINE_TO) * TIME_COARSE_PER)
        }
    }
}

static SCENE_FACTOR: LazyLock<TimeTable> =
    LazyLock::new(|| TimeTable::new(|t| t.powf(-1.0 / NEGATIVE_GAMMA)));
static LOG2_TIME: LazyLock<TimeTable> = LazyLock::new(|| TimeTable::new(f64::log2));

/// [`ToneMap::scene_factor`](crate::tone::ToneMap::scene_factor) of paper
/// under `time > 0` on both passes, `time^(−1/γₙ)`, from a table: within
/// a part in 10⁵ of the power, in a tenth of its time.
#[inline]
pub fn scene_factor(time: f64) -> f64 {
    SCENE_FACTOR.at(time)
}

/// `log₂ time` from a table, `−∞` for a pass that did not run: what
/// [`PaperTable::eval_logged`] takes. Within 10⁻⁵ of the logarithm.
#[inline]
pub fn log2_time(time: f64) -> f64 {
    if time <= 0.0 {
        f64::NEG_INFINITY
    } else {
        LOG2_TIME.at(time)
    }
}

/// Where the grades end, as the contrast of the pair: hard light over
/// soft, in twelfths of a stop, at the softest grade and at the hardest.
/// The grade is a matter of that difference alone, whatever the exposure.
/// Past the soft end the hard light no longer shows and the print is
/// grade 00, the soft emulsion on its own; past the hard end it is grade
/// 5. More of the same light there is not a grade but exposure, so a
/// control that sets the grade stops here. The ends are the first
/// twelfths, counted from the base print, at which the pair's ISO range
/// reads as the paper's own, [`ISO_R_SOFT`] and [`ISO_R_HARD`], to the
/// whole number the datasheet gives them in.
pub fn grade_ends() -> (i32, i32) {
    static ENDS: LazyLock<(i32, i32)> = LazyLock::new(|| {
        let reach = (2.0 * STOP_LIMIT * 12.0).round() as i32;
        let range = |c12: i32| {
            let half = f64::from(c12) / 24.0;
            SplitGrade::new(-half, half).curve().iso_range().round()
        };
        let soft = (-reach..=0).rev().find(|&c| range(c) >= ISO_R_SOFT).unwrap_or(-reach);
        let hard = (0..=reach).find(|&c| range(c) <= ISO_R_HARD).unwrap_or(reach);
        (soft, hard)
    });
    *ENDS
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

    /// The tables read the exact paper to under a thousandth of the print
    /// range at every time a stroke can give, and the ends exactly.
    #[test]
    fn the_table_reads_the_paper() {
        for (soft, hard) in [(0.0, 0.0), (1.5, -2.0), (-3.0, 3.5), (4.0, 4.0)] {
            let curve = SplitGrade::new(soft, hard).curve();
            let table = PaperTable::new(&curve);
            let mut worst = 0.0f64;
            for i in 0..=400 {
                let l = if i == 0 { 0.0 } else { 2f64.powf(-16.0 + 16.0 * i as f64 / 400.0) };
                for (ts, th) in [
                    (1.0, 1.0),
                    (0.0, 1.0),
                    (1.0, 0.0),
                    (0.0, 0.0),
                    (0.37, 2.9),
                    (16.0, 1.0),
                    (1.0, 0.01),
                    (3841.0, 3841.0),
                ] {
                    let exact = curve.eval_timed(l, ts, th);
                    let fast = table.eval_timed(l, ts, th);
                    worst = worst.max((exact - fast).abs());
                    assert!((exact - fast).abs() < 2e-3, "l {l} times {ts} {th}: {exact} vs {fast}");
                }
            }
            assert!(worst < 1.5e-3, "{worst}");
            // No pass at all: unexposed paper. A clear negative under a pass: black.
            assert_eq!(table.eval_timed(0.5, 0.0, 0.0), curve.white());
            assert_eq!(table.eval_timed(0.0, 1.0, 1.0), curve.eval(0.0));
        }
    }

    /// The grades end where the pair's ISO range reads as the paper's own:
    /// a step short of each end it does not yet, and from each end on the
    /// grade's name no longer changes, however far the pair is pushed.
    #[test]
    fn the_grades_end_where_the_range_is_the_papers() {
        let (soft, hard) = grade_ends();
        let range = |c12: i32| {
            let half = f64::from(c12) / 24.0;
            SplitGrade::new(-half, half).curve().iso_range()
        };
        let name = |c12: i32| {
            let half = f64::from(c12) / 24.0;
            grade_label(SplitGrade::new(-half, half).curve().grade())
        };
        assert!(soft < 0 && hard > 0, "{soft} {hard}");
        assert_eq!(range(soft).round(), ISO_R_SOFT);
        assert_eq!(range(hard).round(), ISO_R_HARD);
        assert!(range(soft + 1).round() < ISO_R_SOFT && range(hard - 1).round() > ISO_R_HARD);
        let reach = (2.0 * STOP_LIMIT * 12.0).round() as i32;
        assert!((-reach..=soft).all(|c| name(c) == "00"));
        assert!((hard..=reach).all(|c| name(c) == "5"));
        // Between the ends the range falls with every twelfth: each notch is a grade.
        assert!((soft..hard).all(|c| range(c + 1) < range(c)));
        // The whole way is inside what the two exposures may be.
        assert!(soft > -reach && hard < reach);
    }

    fn pair(g: &SplitGrade) -> (i32, i32) {
        ((g.soft * 24.0).round() as i32, (g.hard * 24.0).round() as i32)
    }

    /// The grade changes and the print's mid-grey stays: at the base
    /// exposure the grey card, a stop lighter or darker the tone that
    /// prints where the card did.
    #[test]
    fn a_change_of_grade_leaves_the_mid_grey_where_it_is() {
        let (soft_end, hard_end) = grade_ends();
        for exposure in [-1.0, 0.0, 1.0] {
            // The scene value that prints as mid-grey at this exposure.
            let l = GREY * 2f64.powf(exposure / NEGATIVE_GAMMA);
            let start = SplitGrade::new(exposure, exposure);
            assert!((start.curve().density(l) - GREY_DENSITY).abs() < 1e-9);
            let mut worst = 0.0f64;
            for notches in soft_end..=hard_end {
                let density = start.graded(notches).curve().density(l);
                worst = worst.max((density - GREY_DENSITY).abs());
            }
            // The pair is written in twelfths, so the mean of a grade is
            // off by a twenty-fourth of a stop at most; at grade 5 the
            // paper gives 1.4 of density to the stop.
            assert!(worst < 0.06, "exposure {exposure}: {worst}");
        }
    }

    /// From the base print the wheel has the whole paper: grade 00 at one
    /// end and grade 5 at the other. A lighter print has less of the soft
    /// end, since the hard light cannot go under its limit.
    #[test]
    fn the_grades_within_reach() {
        let name = |g: &SplitGrade| grade_label(g.curve().grade());
        let base = SplitGrade::default();
        assert_eq!((name(&base.graded(-100)).as_str(), name(&base.graded(100)).as_str()), ("00", "5"));
        for exposure in [-2.0, -1.5, -1.0, -0.5, 0.0, 0.5, 1.0, 1.5, 2.0] {
            let start = SplitGrade::new(exposure, exposure);
            let (soft, hard) = (start.graded(-100), start.graded(100));
            assert!(soft.validate().is_ok() && hard.validate().is_ok());
            // From the base print and darker, the whole paper.
            if exposure >= 0.0 {
                assert_eq!((name(&soft).as_str(), name(&hard).as_str()), ("00", "5"), "{exposure}");
            }
        }
        // A stop lighter: grade 0 at the soft end, the hard light at its limit.
        let lighter = SplitGrade::new(-1.0, -1.0);
        assert_eq!(lighter.graded(-100).hard, -STOP_LIMIT);
        assert_eq!((name(&lighter.graded(-100)).as_str(), name(&lighter.graded(100)).as_str()), ("0", "5"));
        // Two stops lighter: grade ½ to 4½.
        let lightest = SplitGrade::new(-2.0, -2.0);
        assert_eq!(
            (name(&lightest.graded(-100)).as_str(), name(&lightest.graded(100)).as_str()),
            ("½", "4½")
        );
        // Past an end the steps change nothing more.
        assert_eq!(pair(&base.graded(100)), pair(&base.graded(24)));
        assert_eq!(pair(&base.graded(-100)), pair(&base.graded(-60)));
        // A pair from a file that stands past an end stays there on a step
        // outwards and comes back on one inwards.
        let past = SplitGrade::new(-1.5, 1.5);
        assert_eq!(pair(&past.graded(1)), (-36, 36));
        let back = past.graded(-1);
        assert!((back.hard - back.soft - (3.0 - 1.0 / 12.0)).abs() < 1e-12);
    }

    /// Every pair on the twelfth grid: the result is on twelfths and inside
    /// the limit, no step is the pair itself, and inside the grades as
    /// many steps back as the pair was moved give the pair again.
    #[test]
    fn a_grade_step_is_undone_by_the_step_back() {
        let limit = (STOP_LIMIT * 12.0).round() as i32;
        let (soft_end, hard_end) = grade_ends();
        for s in -limit..=limit {
            for h in -limit..=limit {
                let start = SplitGrade::new(f64::from(s) / 12.0, f64::from(h) / 12.0);
                assert_eq!(pair(&start.graded(0)), (2 * s, 2 * h));
                for notches in [-30, -7, -1, 1, 5, 30] {
                    let there = start.graded(notches);
                    let (ts, th) = pair(&there);
                    assert!(ts % 2 == 0 && th % 2 == 0, "{s} {h} {notches}: off the twelfths");
                    assert!(ts.abs() <= 2 * limit && th.abs() <= 2 * limit, "{s} {h} {notches}");
                    let moved = (th - ts) / 2 - (h - s);
                    // Never past the way asked for, never the other way.
                    assert!(moved * notches.signum() >= 0 && moved.abs() <= notches.abs());
                    // Never past an end the pair was inside of.
                    if (soft_end..=hard_end).contains(&(h - s)) {
                        assert!((soft_end..=hard_end).contains(&((th - ts) / 2)), "{s} {h} {notches}");
                    }
                    // Inside the grades the way back ends on the same pair.
                    if (soft_end..=hard_end).contains(&(h - s)) {
                        assert_eq!(pair(&there.graded(-moved)), (2 * s, 2 * h), "{s} {h} {notches}");
                    }
                }
            }
        }
    }

    /// The other half: the same stops on both exposures change the
    /// exposure and not the grade, so the two commute where neither is
    /// stopped.
    #[test]
    fn exposure_and_grade_are_set_apart() {
        let start = SplitGrade::new(-0.25, 0.5);
        let lighter = |g: &SplitGrade| SplitGrade::new(g.soft - 0.75, g.hard - 0.75);
        assert_eq!(pair(&lighter(&start.graded(9))), pair(&lighter(&start).graded(9)));
        assert_eq!(pair(&lighter(&start.graded(-20))), pair(&lighter(&start).graded(-20)));
        // The grade's name is the same at every exposure.
        let name = |g: &SplitGrade| grade_label(g.curve().grade());
        assert_eq!(name(&start.graded(9)), name(&lighter(&start).graded(9)));
        // A pair off the grid, or not a number, is taken to the grid first.
        assert_eq!(pair(&SplitGrade::new(0.03, f64::NAN).graded(2)), pair(&SplitGrade::default().graded(2)));
        // The base print is matched to itself.
        assert_eq!(matched_mean(0), 0);
    }

    /// The time tables are as close as they claim, over the times a field
    /// produces and past the tables' ends.
    #[test]
    fn the_time_tables_keep_their_word() {
        for i in 0..40_000 {
            let t = 2f64.powf(-9.0 + 18.0 * i as f64 / 40_000.0);
            let exact = t.powf(-1.0 / NEGATIVE_GAMMA);
            assert!((scene_factor(t) / exact - 1.0).abs() < 1e-5, "factor at {t}");
            assert!((log2_time(t) - t.log2()).abs() < 1e-5, "log at {t}");
        }
        assert_eq!(log2_time(0.0), f64::NEG_INFINITY);
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
        // One pass alone at its speed-matched time: speed point and ISO R.
        let soft_only = PaperCurve { paper: *p, soft: 0.0, hard: -60.0 };
        let hard_only = PaperCurve { paper: *p, soft: -60.0, hard: 0.0 };
        assert!((soft_only.density(GREY) - SPEED_DENSITY).abs() < 1e-9);
        assert!((hard_only.density(GREY) - SPEED_DENSITY).abs() < 1e-9);
        assert!((soft_only.iso_range() - ISO_R_SOFT).abs() < 0.01);
        assert!((hard_only.iso_range() - ISO_R_HARD).abs() < 0.01);
        assert_eq!(grade_label(soft_only.grade()), "00");
        assert_eq!(grade_label(hard_only.grade()), "5");
        // The separated stroke of grade 00, silver fraction against log
        // exposure relative to the speed point. These are the fitted curve.
        let Component::Soft { mu, h_speed, toe } = p.soft else { panic!("soft") };
        let f = |x: f64| Component::Soft { mu, h_speed, toe }.fraction(mu + x);
        for (x, want) in [(0.0, 0.300000), (0.2, 0.411751), (0.4, 0.525649), (0.8, 0.768536), (1.2, 0.952068)]
        {
            assert!((f(x) - want).abs() < 1e-5, "x {x}: {} != {want}", f(x));
        }
        // The base split lands near grade 2, a little over five scene stops.
        let g = c.grade();
        assert!((1.7..=2.0).contains(&g), "grade {g}");
        assert!((5.0..=5.4).contains(&c.scene_range_stops()), "{}", c.scene_range_stops());
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

    /// A dodge or a burn is time. Unit times are the plain curve to the
    /// bit; the same factor on both passes is a shift of both times; one
    /// pass alone moves its own end of the scale; no time at all is white,
    /// even under a clear negative; and a clipped highlight dodged goes
    /// whiter than the clip, which a placing could not do.
    #[test]
    fn time_is_local_exposure() {
        let c = SplitGrade::new(0.3, -0.2).curve();
        for &l in &[0.0, 0.004, 0.05, 0.18, 0.6, 1.0, 1.4] {
            assert_eq!(c.eval_timed(l, 1.0, 1.0).to_bits(), c.eval(l).to_bits(), "{l}");
        }
        // Twice the time on both passes is +1 stop on both.
        let up = SplitGrade::new(1.3, 0.8).curve();
        for &l in &[0.01, 0.18, 0.6, 0.95] {
            assert!((c.eval_timed(l, 2.0, 2.0) - up.eval(l)).abs() < 1e-12, "{l}");
        }
        // More soft light darkens the highlights and hardly the shadows;
        // more hard light does the opposite.
        let (zone_iii, zone_vii) = (GREY / 4.0, GREY * 4.0);
        let d_soft = (
            c.eval(zone_vii) - c.eval_timed(zone_vii, 2.0, 1.0),
            c.eval(zone_iii) - c.eval_timed(zone_iii, 2.0, 1.0),
        );
        assert!(d_soft.0 > 4.0 * d_soft.1, "soft: {d_soft:?}");
        let d_hard = (
            c.eval(zone_vii) - c.eval_timed(zone_vii, 1.0, 2.0),
            c.eval(zone_iii) - c.eval_timed(zone_iii, 1.0, 2.0),
        );
        assert!(d_hard.1 > d_hard.0, "hard: {d_hard:?}");
        // No time: unexposed paper, whatever the negative.
        assert_eq!(c.eval_timed(0.18, 0.0, 0.0), 1.0);
        assert_eq!(c.eval_timed(0.0, 0.0, 0.0), 1.0);
        // Half the time on both: lighter everywhere, still a print.
        assert!(c.eval_timed(0.18, 0.5, 0.5) > c.eval(0.18));
        assert!(
            c.eval_timed(0.0, 0.5, 0.5) < 0.1,
            "a clear negative under half the time is still near black"
        );
        // A clipped highlight (scene 1, Zone VIII at the base) dodged by a
        // stop prints whiter than the clip; placed by a stop it stays there.
        let base = SplitGrade::default().curve();
        assert!(base.eval_timed(1.0, 0.5, 0.5) > base.eval(1.0) + 0.01);
        assert_eq!(base.eval(2.0), base.eval(1.0));
        // Monotone in the time factor, down to the faintest time.
        let mut last = 0.0;
        for i in 0..=400 {
            let t = 2f64.powf(-40.0 + i as f64 * 0.1);
            let v = c.eval_timed(0.3, t, t);
            assert!(v <= last + 1e-12 || i == 0, "time {t}: {v} after {last}");
            last = v;
        }
        assert!((last - c.eval(0.3)).abs() < 1e-9);
        // The fitted toe opens downward, so it keeps falling to no light at
        // all and needs no floor; a toe that turned would be held far below
        // the stroke. Either way nothing within forty stops is touched.
        let Component::Soft { toe, .. } = paper().soft else { panic!("soft") };
        assert!(toe_floor(toe) < -20.0, "toe {toe}, floor {}", toe_floor(toe));
        assert_eq!(toe_floor(0.01), -soft_cubic_dz(SOFT_H1) / 0.02);
        assert!(toe_floor(0.01) < -20.0);
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
