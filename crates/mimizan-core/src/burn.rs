//! Dodging and burning: the exposure time, made local.
//!
//! The test strip gives the base exposure; then the hand, or a card,
//! holds the light back from a part of the paper (dodging), or lets more
//! of it fall on a part (burning). Neither touches the lamp or the paper.
//! They change the *time* a place is exposed, and that is all this module
//! adds to the paper model: a time factor per pixel, one for each of the
//! two lights.
//!
//! * A [`Stroke`] is one movement of the hand over the paper, with its
//!   time in stops: `+1` doubles the base time where the stroke dwelt,
//!   `−1` halves it. Times add, not stops: two strokes of a stop each make
//!   three times the base, not four, and a dodge takes time away, down to
//!   none, where the paper stays white. `T = 1 + Σ wᵢ (2^{sᵢ} − 1)`, never
//!   below 0 ([`times`]).
//! * The hand ([`Stroke::Hand`]) is a [`Tip`], an ellipse with a soft
//!   edge, moved along a path at a steady pace. A point's share of the
//!   stroke is the time the tip covered it, and the stops are read at the
//!   middle of a straight pass: a pass straight through a point gives it
//!   exactly the stroke's stops, a pass that only brushes it gives less
//!   (the chord), a second pass adds. The hand is set down at the start
//!   and lifted at the end, each for half a pass, so a stroke begins and
//!   ends with its stops; set down and not moved, it is the tip resting
//!   there.
//! * The card ([`Stroke::Card`]) is a straight edge pushed from one place
//!   to another: the paper beyond where it ends up had the light the whole
//!   time, the paper behind where it started none of it, and the band in
//!   between a ramp. A card held still is a step.
//! * Each stroke is done under one light or both ([`Light`]): the soft
//!   pass alone prints detail into a sky without closing the shadows, the
//!   hard alone deepens a shadow, both is the working grade.
//! * No edge is a line. The lens is a disc of light above the hand, so its
//!   shadow has a penumbra: the fraction of that disc a point still sees.
//!   [`penumbra`] is that fraction against the distance to the edge in
//!   units of the half-width `feather`, the hand's height above the paper
//!   as one number. Smooth, monotone, finite.
//! * Strokes live on the upright negative, normalised: the origin at its
//!   centre, the unit its long side, x right, y down, angles clockwise in
//!   degrees ([`normalize`]). A crop, a straighten or a sheet moves no
//!   stroke; it sits on the negative, and the print reads it through the
//!   same map that reads the pixels ([`Field`]).

use crate::error::{Error, Result};
use crate::straighten::Affine;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::ops::Range;
use std::sync::{Arc, Mutex, OnceLock};

/// Widest burn or dodge a stroke may carry, stops of time either way.
pub const STOP_LIMIT: f64 = 4.0;
/// Most strokes a recipe may carry.
pub const MAX_STROKES: usize = 256;
/// Most points on one stroke's path.
pub const MAX_POINTS: usize = 2048;
/// Farthest a point of a path may lie from the negative's centre, in long
/// sides: well outside the picture, so a corner can be worked from the dark.
pub const REACH: f64 = 2.0;
/// Longest half-axis, widest feather and longest push of a card, long sides.
pub const SIZE_LIMIT: f64 = 4.0;
/// Along a path the tip is set down every this fraction of its smaller
/// half-axis (finer for a soft edge, [`path_step`]), and the dwell raster
/// has this many points per smaller half-axis.
const STEPS_PER_UNIT: f64 = 16.0;
/// A soft edge is sampled along the path no finer than this fraction of
/// the smaller half-axis, however narrow its feather.
const FINEST_STEPS_PER_UNIT: f64 = 64.0;
/// Most places one stroke is set down at; a longer path is sampled coarser.
const MAX_SAMPLES: usize = 1 << 18;
/// Prepared strokes kept between fields, by their rasters' bytes.
const CACHE_BYTES: usize = 256 << 20;
/// Most cells along a side of one stroke's sample grid, and of the field's
/// index; most points along a side of a stroke's dwell raster.
const MAX_CELLS: usize = 512;
const MAX_RASTER: usize = 2048;
const INDEX_CELLS: usize = 48;

/// Which pass of the split-grade print the time is added to.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Light {
    /// Both passes: the working grade.
    #[default]
    Both,
    /// The soft pass (filter 00) alone.
    Soft,
    /// The hard pass (filter 5) alone.
    Hard,
}

impl Light {
    fn is_both(&self) -> bool {
        *self == Light::Both
    }

    pub fn name(self) -> &'static str {
        match self {
            Light::Both => "both",
            Light::Soft => "soft",
            Light::Hard => "hard",
        }
    }
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// The hand: an ellipse with a soft edge. Sizes in the normalised unit,
/// the angle degrees clockwise the x half-axis is turned from horizontal.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Tip {
    pub rx: f64,
    pub ry: f64,
    #[serde(default, skip_serializing_if = "is_zero")]
    pub angle: f64,
    /// Half-width of the penumbra at the edge. Zero is a hard edge.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub feather: f64,
}

impl Tip {
    fn axes(&self) -> (f64, f64) {
        (self.rx.max(1e-9), self.ry.max(1e-9))
    }

    /// Signed distance of the point `(dx, dy)` from the centre to the
    /// outline, negative inside: the first-order distance to the level
    /// set, exact for a circle and along the axes.
    pub fn distance(&self, dx: f64, dy: f64) -> f64 {
        let (s, c) = self.angle.to_radians().sin_cos();
        self.distance_turned(dx, dy, s, c)
    }

    #[inline]
    fn distance_turned(&self, dx: f64, dy: f64, sin: f64, cos: f64) -> f64 {
        let px = dx * cos + dy * sin;
        let py = -dx * sin + dy * cos;
        let (rx, ry) = self.axes();
        let (ex, ey) = (px / rx, py / ry);
        let r2 = ex * ex + ey * ey;
        if r2 < 1e-24 {
            return -rx.min(ry);
        }
        let r = r2.sqrt();
        let gx = px / (rx * rx);
        let gy = py / (ry * ry);
        (r - 1.0) * r / (gx * gx + gy * gy).sqrt()
    }

    /// How much of the light the tip, resting with its centre at the
    /// origin, takes from a point at `(dx, dy)`: 1 well inside, the
    /// penumbra at the edge, 0 beyond.
    pub fn weight(&self, dx: f64, dy: f64) -> f64 {
        edge_weight(self.distance(dx, dy), self.feather)
    }

    /// Beyond this distance from the centre the weight is exactly 0. With
    /// `a` and `b` the larger and the smaller half-axis and `f` the
    /// feather: the level function `r` is `1/b`-Lipschitz and at least
    /// `R/a` at radius `R`, so the first-order distance is at least
    /// `(R/a − 1) b`, which is `≥ f` from `R ≥ a (1 + f/b)`.
    pub fn reach(&self) -> f64 {
        let (rx, ry) = self.axes();
        let (a, b) = (rx.max(ry), rx.min(ry));
        a * (1.0 + self.feather / b)
    }

    /// Within this distance from the centre the weight is exactly 1: the
    /// distance is at most `R − b`, which is `< −f` from `R < b − f`.
    fn inside(&self) -> f64 {
        let (rx, ry) = self.axes();
        (rx.min(ry) - self.feather).max(0.0)
    }

    /// Half the chord through the centre along the unit direction
    /// `(ux, uy)` of the picture.
    pub fn half_chord(&self, ux: f64, uy: f64) -> f64 {
        let (s, c) = self.angle.to_radians().sin_cos();
        let ex = ux * c + uy * s;
        let ey = -ux * s + uy * c;
        let (rx, ry) = self.axes();
        1.0 / ((ex / rx).powi(2) + (ey / ry).powi(2)).sqrt().max(1e-12)
    }

    /// Half the light a point takes over a straight pass of the tip
    /// through it along `(ux, uy)`: the integral of the weight along the
    /// line, halved. The half-chord when the feather fits inside the tip
    /// (the penumbra is symmetric about the edge); more when the tip is
    /// softer than it is wide, its tail reaching past the chord.
    pub fn half_dwell(&self, ux: f64, uy: f64) -> f64 {
        let h = self.half_chord(ux, uy);
        if self.feather > 0.0 {
            -self.feather * penumbra_integral(-h / self.feather)
        } else {
            h
        }
    }

    /// The tip after `quarters` clockwise quarter turns of the picture.
    pub fn turned(self, quarters: u8) -> Self {
        Self { angle: wrap_degrees(self.angle + 90.0 * f64::from(quarters % 4)), ..self }
    }

    pub fn validate(&self) -> Result<()> {
        for (r, what) in [(self.rx, "rx"), (self.ry, "ry")] {
            if !r.is_finite() || r <= 0.0 || r > SIZE_LIMIT {
                return Err(Error::Invalid(format!("tip {what} {r} not within 0..{SIZE_LIMIT}")));
            }
        }
        if !self.angle.is_finite() || self.angle.abs() > 360.0 {
            return Err(Error::Invalid(format!("tip angle {} not within ±360", self.angle)));
        }
        if !self.feather.is_finite() || !(0.0..=SIZE_LIMIT).contains(&self.feather) {
            return Err(Error::Invalid(format!("tip feather {} not within 0..{SIZE_LIMIT}", self.feather)));
        }
        Ok(())
    }
}

/// The weight at signed distance `d` from an edge with a penumbra of
/// half-width `feather`: inside (`d < 0`) towards 1, outside towards 0.
#[inline]
fn edge_weight(d: f64, feather: f64) -> f64 {
    if feather > 0.0 {
        penumbra(d / feather)
    } else if d < 0.0 {
        1.0
    } else if d > 0.0 {
        0.0
    } else {
        0.5
    }
}

/// Degrees into (−180, 180].
pub fn wrap_degrees(a: f64) -> f64 {
    let mut a = a % 360.0;
    if a <= -180.0 {
        a += 360.0;
    } else if a > 180.0 {
        a -= 360.0;
    }
    a
}

/// Fraction of a disc of light a point sees past a straight edge at signed
/// distance `t` in units of the disc's radius: 1 well inside the lit part
/// (`t ≤ −1`), ½ on the edge, 0 at `t ≥ 1`. The area of the circular
/// segment, `(acos t − t √(1 − t²)) / π`.
#[inline]
pub fn penumbra(t: f64) -> f64 {
    if t <= -1.0 {
        1.0
    } else if t >= 1.0 {
        0.0
    } else {
        (t.acos() - t * (1.0 - t * t).sqrt()) / std::f64::consts::PI
    }
}

/// The antiderivative of [`penumbra`] that is 0 at `t ≥ 1`: `t` at
/// `t ≤ −1`, between them `[t acos t − √(1 − t²) + (1 − t²)^{3/2} / 3] / π`.
/// `∫ₐᵇ penumbra = P(b) − P(a)`.
#[inline]
pub fn penumbra_integral(t: f64) -> f64 {
    if t <= -1.0 {
        t
    } else if t >= 1.0 {
        0.0
    } else {
        let s = 1.0 - t * t;
        (t * t.acos() - s.sqrt() + s * s.sqrt() / 3.0) / std::f64::consts::PI
    }
}

/// One movement of the hand under the enlarger, with its time in stops
/// and the light it is done under. Coordinates are normalised to the
/// upright negative (see the module doc).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "tool", rename_all = "kebab-case")]
pub enum Stroke {
    /// The tip moved along `points` at a steady pace; one point is the tip
    /// set down and held.
    Hand {
        tip: Tip,
        /// Stops of time at the middle of a straight pass: `+1` doubles
        /// the base time there, `−1` halves it. Within ±[`STOP_LIMIT`].
        stops: f64,
        #[serde(default, skip_serializing_if = "Light::is_both")]
        light: Light,
        points: Vec<[f64; 2]>,
    },
    /// A straight edge through `(x, y)`, lit on the side its normal
    /// points to (`angle` degrees clockwise from +x; `0` lights everything
    /// right of a vertical line through the point, `90` everything below a
    /// horizontal one), pushed `travel` along that normal. Beyond where it
    /// ends up the full time, behind where it started none, between a ramp.
    Card {
        x: f64,
        y: f64,
        angle: f64,
        #[serde(default, skip_serializing_if = "is_zero")]
        travel: f64,
        #[serde(default, skip_serializing_if = "is_zero")]
        feather: f64,
        stops: f64,
        #[serde(default, skip_serializing_if = "Light::is_both")]
        light: Light,
    },
}

impl Stroke {
    pub fn stops(&self) -> f64 {
        match self {
            Stroke::Hand { stops, .. } | Stroke::Card { stops, .. } => *stops,
        }
    }

    pub fn light(&self) -> Light {
        match self {
            Stroke::Hand { light, .. } | Stroke::Card { light, .. } => *light,
        }
    }

    pub fn name(&self) -> &'static str {
        match self {
            Stroke::Hand { .. } => "hand",
            Stroke::Card { .. } => "card",
        }
    }

    /// The time factor at full weight, `2^stops`.
    pub fn factor(&self) -> f64 {
        2f64.powf(self.stops())
    }

    /// The stroke after `quarters` clockwise quarter turns of the picture
    /// about its centre. The unit (the long side) does not change.
    pub fn turned(&self, quarters: u8) -> Self {
        let q = quarters % 4;
        let turn = |x: f64, y: f64| -> (f64, f64) {
            match q {
                0 => (x, y),
                1 => (-y, x),
                2 => (-x, -y),
                _ => (y, -x),
            }
        };
        match self {
            Stroke::Hand { tip, stops, light, points } => Stroke::Hand {
                tip: tip.turned(q),
                stops: *stops,
                light: *light,
                points: points
                    .iter()
                    .map(|&[x, y]| {
                        let (x, y) = turn(x, y);
                        [x, y]
                    })
                    .collect(),
            },
            Stroke::Card { x, y, angle, travel, feather, stops, light } => {
                let (x, y) = turn(*x, *y);
                Stroke::Card {
                    x,
                    y,
                    angle: wrap_degrees(angle + 90.0 * f64::from(q)),
                    travel: *travel,
                    feather: *feather,
                    stops: *stops,
                    light: *light,
                }
            }
        }
    }

    pub fn validate(&self) -> Result<()> {
        let within = |v: f64, lim: f64, what: &str| {
            if !v.is_finite() || v.abs() > lim {
                return Err(Error::Invalid(format!("stroke {what} {v} not within ±{lim}")));
            }
            Ok(())
        };
        within(self.stops(), STOP_LIMIT, "stops")?;
        match self {
            Stroke::Hand { tip, points, .. } => {
                tip.validate()?;
                if points.is_empty() || points.len() > MAX_POINTS {
                    return Err(Error::Invalid(format!(
                        "stroke of {} points; 1 to {MAX_POINTS}",
                        points.len()
                    )));
                }
                for &[x, y] in points {
                    within(x, REACH, "x")?;
                    within(y, REACH, "y")?;
                }
                Ok(())
            }
            Stroke::Card { x, y, angle, travel, feather, .. } => {
                within(*x, REACH, "x")?;
                within(*y, REACH, "y")?;
                within(*angle, 360.0, "angle")?;
                if !travel.is_finite() || !(0.0..=SIZE_LIMIT).contains(travel) {
                    return Err(Error::Invalid(format!("card travel {travel} not within 0..{SIZE_LIMIT}")));
                }
                if !feather.is_finite() || !(0.0..=SIZE_LIMIT).contains(feather) {
                    return Err(Error::Invalid(format!("card feather {feather} not within 0..{SIZE_LIMIT}")));
                }
                Ok(())
            }
        }
    }

    /// How much of the stroke's time a normalised point gets, 0 to 1 for
    /// one pass, more where the hand went back and forth. What the tests
    /// measure a stroke by; a picture reads it through a [`Field`].
    #[cfg(test)]
    pub fn weight(&self, x: f64, y: f64) -> f64 {
        Ready::new(self).weight(x, y)
    }
}

/// Every stroke of a list within its range, and not too many of them.
pub fn validate(strokes: &[Stroke]) -> Result<()> {
    if strokes.len() > MAX_STROKES {
        return Err(Error::Invalid(format!("{} strokes; at most {MAX_STROKES}", strokes.len())));
    }
    strokes.iter().try_for_each(Stroke::validate)
}

/// The strokes after `quarters` clockwise quarter turns of the picture.
pub fn turned(strokes: &[Stroke], quarters: u8) -> Vec<Stroke> {
    strokes.iter().map(|s| s.turned(quarters)).collect()
}

/// The time factors at one point: of the soft pass, of the hard pass, and
/// of a print with one light (a look), each 1 where nothing is burned.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Times {
    pub soft: f64,
    pub hard: f64,
    pub all: f64,
}

impl Times {
    /// No burn here.
    pub const UNIT: Times = Times { soft: 1.0, hard: 1.0, all: 1.0 };

    #[inline]
    pub fn is_unit(&self) -> bool {
        *self == Self::UNIT
    }

    /// Both passes see the same time.
    #[inline]
    pub fn is_even(&self) -> bool {
        self.soft == self.hard
    }
}

/// One place the tip was set down, and the share of the stroke's time it
/// stands for: the pace along the path over the dwell of a pass.
#[derive(Clone, Copy, Debug)]
struct Sample {
    x: f64,
    y: f64,
    w: f64,
}

/// The step along a path: a sixteenth of the smaller half-axis, or of
/// twice the feather when that is finer, down to a 64th of the half-axis.
/// A hard edge is resolved by the sixteenth; a soft one is sampled finer
/// so its ramp is summed smoothly, but a feather narrower than an eighth
/// of the half-axis gains nothing from more samples that a hard edge
/// does not already forgo.
fn path_step(tip: &Tip) -> f64 {
    let (rx, ry) = tip.axes();
    let axis = rx.min(ry);
    let soft = if tip.feather > 0.0 { 2.0 * tip.feather } else { f64::INFINITY };
    axis.min(soft).max(axis * STEPS_PER_UNIT / FINEST_STEPS_PER_UNIT) / STEPS_PER_UNIT
}

/// The spacing of the dwell raster: a sixteenth of the smaller half-axis,
/// whatever the feather. The read between its points softens an edge by
/// less than that; a raster fine enough for a narrow feather would cost
/// the square of the ratio, for every sample set down on it.
fn raster_step(tip: &Tip) -> f64 {
    let (rx, ry) = tip.axes();
    rx.min(ry) / STEPS_PER_UNIT
}

/// Where along the path the tip is set down: the hand set down at the
/// start for half a pass, then a sample every `step` of the way (`full`),
/// and the tail: the last part of the path shorter than a step, as one
/// sample with that part's share, and the hand lifted at the end after
/// another half pass. The rests make a stroke begin and end with its
/// stops, where a tip only moving would give its start and its end half
/// a chord; and a path of no length is the two rests in one place, the
/// tip resting with the whole of a pass, which is the click. The steps
/// are fixed from the start, so a path that grows keeps the samples it
/// had and adds to them; only the tail moves.
#[derive(Clone, Debug)]
struct Sampled {
    step: f64,
    full: Vec<Sample>,
    tail: Vec<Sample>,
}

/// The share of a pass the hand rests at either end of a path.
const REST: f64 = 0.5;

fn sample_path(tip: &Tip, points: &[[f64; 2]]) -> Sampled {
    let step = path_step(tip);
    let Some(&[x0, y0]) = points.first() else { return Sampled { step, full: Vec::new(), tail: Vec::new() } };
    let mut segments: Vec<([f64; 2], [f64; 2], f64)> = Vec::new();
    for pair in points.windows(2) {
        let (p, q) = (pair[0], pair[1]);
        let len = ((q[0] - p[0]).powi(2) + (q[1] - p[1]).powi(2)).sqrt();
        if len > 0.0 {
            segments.push((p, q, len));
        }
    }
    let total: f64 = segments.iter().map(|s| s.2).sum();
    if segments.is_empty() || total <= 0.0 {
        return Sampled { step, full: Vec::new(), tail: vec![Sample { x: x0, y: y0, w: 1.0 }] };
    }
    // A path too long for its step is sampled coarser, in equal steps.
    let (step, n) = if total / step > MAX_SAMPLES as f64 {
        (total / MAX_SAMPLES as f64, MAX_SAMPLES)
    } else {
        (step, (total / step).floor() as usize)
    };
    let rest = (total - n as f64 * step).max(0.0);
    let (mut i, mut start) = (0usize, 0.0f64);
    let mut dwell = None;
    let mut at = |s: f64, share: f64| -> Sample {
        while i + 1 < segments.len() && s > start + segments[i].2 {
            start += segments[i].2;
            i += 1;
            dwell = None;
        }
        let (p, q, len) = segments[i];
        let (ux, uy) = ((q[0] - p[0]) / len, (q[1] - p[1]) / len);
        let half = *dwell.get_or_insert_with(|| tip.half_dwell(ux, uy));
        let t = ((s - start) / len).clamp(0.0, 1.0);
        Sample { x: p[0] + (q[0] - p[0]) * t, y: p[1] + (q[1] - p[1]) * t, w: share / (2.0 * half) }
    };
    let full: Vec<Sample> = std::iter::once(Sample { x: x0, y: y0, w: REST })
        .chain((0..n).map(|k| at((k as f64 + 0.5) * step, step)))
        .collect();
    let mut tail = Vec::with_capacity(2);
    if rest > 1e-12 * total.max(1.0) {
        tail.push(at(n as f64 * step + rest * 0.5, rest));
    }
    let [x1, y1] = segments[segments.len() - 1].1;
    tail.push(Sample { x: x1, y: y1, w: REST });
    Sampled { step, full, tail }
}

/// The samples of one stroke binned on a grid whose cell is at least the
/// tip's reach, so the samples that can touch a point are in its cell and
/// the eight around it. Used while the dwell is rastered, then dropped.
struct Grid {
    x0: f64,
    y0: f64,
    cell: f64,
    nx: usize,
    ny: usize,
    /// `starts[c]..starts[c + 1]` are cell `c`'s samples.
    starts: Vec<u32>,
    samples: Vec<Sample>,
}

impl Grid {
    fn new(samples: &[Sample], (x0, y0, x1, y1): (f64, f64, f64, f64), reach: f64) -> Self {
        let (w, h) = (x1 - x0, y1 - y0);
        let cell = reach.max(w / MAX_CELLS as f64).max(h / MAX_CELLS as f64).max(1e-9);
        let nx = (w / cell).floor() as usize + 1;
        let ny = (h / cell).floor() as usize + 1;
        let at = |s: &Sample| {
            let cx = (((s.x - x0) / cell).floor().max(0.0) as usize).min(nx - 1);
            let cy = (((s.y - y0) / cell).floor().max(0.0) as usize).min(ny - 1);
            cy * nx + cx
        };
        let mut starts = vec![0u32; nx * ny + 1];
        for s in samples {
            starts[at(s) + 1] += 1;
        }
        for c in 0..nx * ny {
            starts[c + 1] += starts[c];
        }
        let mut fill = starts.clone();
        let mut sorted = vec![Sample { x: 0.0, y: 0.0, w: 0.0 }; samples.len()];
        for s in samples {
            let c = at(s);
            sorted[fill[c] as usize] = *s;
            fill[c] += 1;
        }
        Self { x0, y0, cell, nx, ny, starts, samples: sorted }
    }

    /// The samples in the cell of `(x, y)` and the eight around it, in a
    /// fixed order, summed by `f`; `None` when there is none.
    fn sum(&self, x: f64, y: f64, mut f: impl FnMut(&Sample) -> f64) -> Option<f64> {
        let cx = (((x - self.x0) / self.cell).floor().max(0.0) as usize).min(self.nx - 1);
        let cy = (((y - self.y0) / self.cell).floor().max(0.0) as usize).min(self.ny - 1);
        let mut sum = 0.0;
        let mut any = false;
        for yy in cy.saturating_sub(1)..=(cy + 1).min(self.ny - 1) {
            for xx in cx.saturating_sub(1)..=(cx + 1).min(self.nx - 1) {
                let c = yy * self.nx + xx;
                for s in &self.samples[self.starts[c] as usize..self.starts[c + 1] as usize] {
                    any = true;
                    sum += f(s);
                }
            }
        }
        any.then_some(sum)
    }
}

/// The tip with its trigonometry and its two circles worked out once.
#[derive(Clone, Copy, Debug)]
struct ReadyTip {
    tip: Tip,
    sin: f64,
    cos: f64,
    /// Outside this squared radius about the centre the weight is 0,
    /// inside the other exactly 1 ([`Tip::reach`], [`Tip::inside`]).
    outside2: f64,
    inside2: f64,
}

impl ReadyTip {
    fn new(tip: Tip) -> Self {
        let (sin, cos) = tip.angle.to_radians().sin_cos();
        let (reach, inside) = (tip.reach(), tip.inside());
        Self { tip, sin, cos, outside2: reach * reach, inside2: inside * inside }
    }

    fn reach(&self) -> f64 {
        self.outside2.sqrt()
    }

    /// Half-widths of an axis-aligned box that holds every point the tip
    /// gives weight to. The distance is at least `(r − 1) b` at level `r`
    /// ([`Tip::reach`]), so the weight ends inside the level
    /// `1 + f/b`: the ellipse grown by that factor, turned, and its
    /// extents. Tighter than the square of the reach for an eccentric
    /// tip, so a stamp of it visits fewer points. (The ellipse's own
    /// extents plus the feather are not enough: the distance is of the
    /// first order, and off the ends of a long soft tip the weight reaches
    /// further than the feather.)
    fn extent(&self) -> (f64, f64) {
        let (rx, ry) = self.tip.axes();
        let grown = 1.0 + self.tip.feather.max(0.0) / rx.min(ry);
        let (s, c) = (self.sin, self.cos);
        let ex = grown * ((rx * c).powi(2) + (ry * s).powi(2)).sqrt();
        let ey = grown * ((rx * s).powi(2) + (ry * c).powi(2)).sqrt();
        (ex.min(self.reach()), ey.min(self.reach()))
    }

    /// [`Tip::weight`] of a point `(dx, dy)` from the centre.
    #[inline]
    fn weight(&self, dx: f64, dy: f64) -> f64 {
        let r2 = dx * dx + dy * dy;
        if r2 > self.outside2 {
            0.0
        } else if r2 < self.inside2 {
            1.0
        } else {
            edge_weight(self.tip.distance_turned(dx, dy, self.sin, self.cos), self.tip.feather)
        }
    }
}

/// The box the samples can reach, a step wider so the outer grid points
/// hold nothing.
fn reach_box(samples: &[Sample], reach: f64, step: f64) -> (f64, f64, f64, f64) {
    let (mut x0, mut y0, mut x1, mut y1) =
        (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
    for s in samples {
        x0 = x0.min(s.x);
        y0 = y0.min(s.y);
        x1 = x1.max(s.x);
        y1 = y1.max(s.y);
    }
    let m = reach + step;
    (x0 - m, y0 - m, x1 + m, y1 + m)
}

/// The dwell of a moving tip, worked out once on a grid of points a
/// [`raster_step`] apart and read between them. The tip is resolved by
/// the step ([`STEPS_PER_UNIT`]); what a pixel reads is the bilinear
/// blend of the four grid points around it, which is within a small part
/// of a percent of the sum where the edge is softer than the step and
/// softens a harder edge by less than a sixteenth of the tip. A raster
/// grows: new samples are added to the points they reach, the grid
/// widened on its own lattice when they lie outside it.
#[derive(Clone, Debug)]
struct Raster {
    x0: f64,
    y0: f64,
    /// The spacing of the grid points, and one over it.
    h: f64,
    inv_h: f64,
    nx: usize,
    ny: usize,
    w: Vec<f32>,
}

impl Raster {
    /// Every grid point summed over the samples near it, in parallel.
    /// `h` is the spacing asked for ([`raster_step`]); a box too wide for
    /// [`MAX_RASTER`] points of it is rastered coarser.
    fn build(tip: &ReadyTip, samples: &[Sample], h: f64) -> Self {
        let reach = tip.reach();
        let bounds = reach_box(samples, reach, h);
        let grid = Grid::new(samples, bounds, reach);
        let (x0, y0, x1, y1) = bounds;
        let h = h.max((x1 - x0) / MAX_RASTER as f64).max((y1 - y0) / MAX_RASTER as f64);
        let nx = ((x1 - x0) / h).ceil() as usize + 1;
        let ny = ((y1 - y0) / h).ceil() as usize + 1;
        let mut w = vec![0f32; nx * ny];
        w.par_chunks_mut(nx).enumerate().for_each(|(j, row)| {
            let y = y0 + j as f64 * h;
            for (i, o) in row.iter_mut().enumerate() {
                let x = x0 + i as f64 * h;
                if let Some(sum) = grid.sum(x, y, |s| tip.weight(x - s.x, y - s.y) * s.w) {
                    *o = sum as f32;
                }
            }
        });
        Self { x0, y0, h, inv_h: 1.0 / h, nx, ny, w }
    }

    fn x1(&self) -> f64 {
        self.x0 + (self.nx - 1) as f64 * self.h
    }

    fn y1(&self) -> f64 {
        self.y0 + (self.ny - 1) as f64 * self.h
    }

    fn bounds(&self) -> (f64, f64, f64, f64) {
        (self.x0, self.y0, self.x1(), self.y1())
    }

    /// `samples` added to the grid points they reach. `false` when the
    /// widened grid would pass [`MAX_RASTER`]: then it is built afresh.
    fn extend(&mut self, tip: &ReadyTip, samples: &[Sample]) -> bool {
        if samples.is_empty() {
            return true;
        }
        let reach = tip.reach();
        let (bx0, by0, bx1, by1) = reach_box(samples, reach, self.h);
        // Widen on the lattice: whole steps before and after.
        let before_x = ((self.x0 - bx0) / self.h).ceil().max(0.0) as usize;
        let before_y = ((self.y0 - by0) / self.h).ceil().max(0.0) as usize;
        let after_x = ((bx1 - self.x1()) / self.h).ceil().max(0.0) as usize;
        let after_y = ((by1 - self.y1()) / self.h).ceil().max(0.0) as usize;
        if before_x + after_x + before_y + after_y > 0 {
            let (nx, ny) = (self.nx + before_x + after_x, self.ny + before_y + after_y);
            if nx > MAX_RASTER + 1 || ny > MAX_RASTER + 1 {
                return false;
            }
            let mut w = vec![0f32; nx * ny];
            for j in 0..self.ny {
                let src = &self.w[j * self.nx..(j + 1) * self.nx];
                let at = (j + before_y) * nx + before_x;
                w[at..at + self.nx].copy_from_slice(src);
            }
            self.x0 -= before_x as f64 * self.h;
            self.y0 -= before_y as f64 * self.h;
            self.nx = nx;
            self.ny = ny;
            self.w = w;
        }
        let (ex, ey) = tip.extent();
        for s in samples {
            let i0 = (((s.x - ex - self.x0) / self.h).floor().max(0.0) as usize).min(self.nx - 1);
            let i1 = (((s.x + ex - self.x0) / self.h).ceil().max(0.0) as usize).min(self.nx - 1);
            let j0 = (((s.y - ey - self.y0) / self.h).floor().max(0.0) as usize).min(self.ny - 1);
            let j1 = (((s.y + ey - self.y0) / self.h).ceil().max(0.0) as usize).min(self.ny - 1);
            for j in j0..=j1 {
                let y = self.y0 + j as f64 * self.h;
                let row = &mut self.w[j * self.nx..(j + 1) * self.nx];
                for (i, o) in row.iter_mut().enumerate().take(i1 + 1).skip(i0) {
                    let x = self.x0 + i as f64 * self.h;
                    let w = tip.weight(x - s.x, y - s.y);
                    if w > 0.0 {
                        *o += (w * s.w) as f32;
                    }
                }
            }
        }
        true
    }

    #[inline]
    fn weight(&self, x: f64, y: f64) -> f64 {
        let fx = (x - self.x0) * self.inv_h;
        let fy = (y - self.y0) * self.inv_h;
        if !(fx >= 0.0 && fy >= 0.0 && fx < (self.nx - 1) as f64 && fy < (self.ny - 1) as f64) {
            return 0.0;
        }
        let (i, j) = (fx as usize, fy as usize);
        let (tx, ty) = (fx - i as f64, fy - j as f64);
        let at = |i: usize, j: usize| f64::from(self.w[j * self.nx + i]);
        let top = at(i, j) * (1.0 - tx) + at(i + 1, j) * tx;
        let bottom = at(i, j + 1) * (1.0 - tx) + at(i + 1, j + 1) * tx;
        top * (1.0 - ty) + bottom * ty
    }
}

/// A stroke with its gain and its dwell worked out once, for the millions
/// of pixels that ask.
#[derive(Debug)]
enum Ready {
    /// The hand set down and held: the tip itself.
    Still { tip: ReadyTip, x: f64, y: f64, gain: f64, light: Light },
    /// The hand moved: its full steps on a raster, its tail (the last
    /// part of the path and the rest at its end) read as the tip; the
    /// path it was made from, so a longer path can grow it.
    Moving {
        tip: ReadyTip,
        raster: Raster,
        tail: Vec<Sample>,
        full: usize,
        step: f64,
        points: Vec<[f64; 2]>,
        gain: f64,
        light: Light,
    },
    Card {
        x: f64,
        y: f64,
        sin: f64,
        cos: f64,
        travel: f64,
        feather: f64,
        /// The weight along the edge's normal, tabulated where it varies.
        profile: Option<Profile>,
        gain: f64,
        light: Light,
    },
}

/// A card's weight as a function of how far past the edge's starting
/// place a point lies, sampled where it is neither 0 nor 1 and read
/// between the samples: the penumbra integrals once per sample, not per
/// pixel. The samples are a 256th of the feather apart, at most
/// [`PROFILE_MAX`] of them.
#[derive(Clone, Debug)]
struct Profile {
    s0: f64,
    inv_step: f64,
    w: Vec<f32>,
}

const PROFILE_MAX: usize = 1 << 16;

impl Profile {
    /// The card's weight over `s` in `[−feather, travel + feather]`;
    /// `None` without a feather (the hard card is two branches).
    fn new(travel: f64, feather: f64) -> Option<Self> {
        if feather <= 0.0 || feather.is_nan() {
            return None;
        }
        let (s0, s1) = (-feather, travel + feather);
        let step = (feather / 256.0).max((s1 - s0) / (PROFILE_MAX - 1) as f64);
        // A feather so narrow that a 256th of it is nothing has no
        // samples to take: the card is read as it is, like the hard one.
        if step.is_nan() || step <= 0.0 {
            return None;
        }
        let n = ((s1 - s0) / step).ceil() as usize + 1;
        let w = (0..n).map(|i| card_weight(s0 + i as f64 * step, travel, feather) as f32).collect();
        Some(Self { s0, inv_step: 1.0 / step, w })
    }

    #[inline]
    fn at(&self, s: f64) -> f64 {
        let f = (s - self.s0) * self.inv_step;
        let last = self.w.len() - 1;
        if f <= 0.0 {
            return f64::from(self.w[0]);
        }
        if f >= last as f64 {
            return f64::from(self.w[last]);
        }
        let i = f as usize;
        let t = f - i as f64;
        f64::from(self.w[i]) * (1.0 - t) + f64::from(self.w[i + 1]) * t
    }
}

/// A card's weight at `s` past the edge's starting place: the penumbra
/// of a resting card, the mean of it over the travel of a moving one.
#[inline]
fn card_weight(s: f64, travel: f64, feather: f64) -> f64 {
    if travel <= 0.0 {
        edge_weight(-s, feather)
    } else if feather > 0.0 {
        let f = feather;
        // Past the penumbra on either side the weight is whole. Said
        // outright: far from a very narrow edge the two integrals below
        // are two equal large numbers, or two infinities, and their
        // difference is nothing.
        if s <= -f {
            return 0.0;
        }
        if s >= travel + f {
            return 1.0;
        }
        f / travel * (penumbra_integral((travel - s) / f) - penumbra_integral(-s / f))
    } else {
        (s / travel).clamp(0.0, 1.0)
    }
}

impl Ready {
    fn new(stroke: &Stroke) -> Self {
        let gain = stroke.factor() - 1.0;
        match stroke {
            Stroke::Hand { tip, light, points, .. } => {
                let ready = ReadyTip::new(*tip);
                let sampled = sample_path(tip, points);
                match (sampled.full.as_slice(), sampled.tail.as_slice()) {
                    ([], []) => Ready::Still { tip: ready, x: 0.0, y: 0.0, gain: 0.0, light: *light },
                    ([], [one]) => Ready::Still { tip: ready, x: one.x, y: one.y, gain, light: *light },
                    (full, _) => Ready::Moving {
                        tip: ready,
                        raster: Raster::build(&ready, full, raster_step(tip)),
                        tail: sampled.tail,
                        full: full.len(),
                        step: sampled.step,
                        points: points.clone(),
                        gain,
                        light: *light,
                    },
                }
            }
            Stroke::Card { x, y, angle, travel, feather, light, .. } => {
                let (sin, cos) = angle.to_radians().sin_cos();
                Ready::Card {
                    x: *x,
                    y: *y,
                    sin,
                    cos,
                    travel: *travel,
                    feather: *feather,
                    profile: Profile::new(*travel, *feather),
                    gain,
                    light: *light,
                }
            }
        }
    }

    /// `stroke` prepared from `from`, a shorter path of the same hand that
    /// this one continues: the raster is copied and the new steps added.
    /// `None` when it does not continue it, or the steps changed.
    fn grown(from: &Ready, stroke: &Stroke) -> Option<Self> {
        let Stroke::Hand { tip, light, points, .. } = stroke else { return None };
        let Ready::Moving { tip: ready, raster, full, step, points: old, light: old_light, gain, .. } = from
        else {
            return None;
        };
        let same_tip = ready.tip == *tip && *old_light == *light && *gain == stroke.factor() - 1.0;
        if !same_tip || old.len() > points.len() || points[..old.len()] != old[..] {
            return None;
        }
        let sampled = sample_path(tip, points);
        if sampled.step != *step || sampled.full.len() < *full {
            return None;
        }
        let mut raster = raster.clone();
        if !raster.extend(ready, &sampled.full[*full..]) {
            return None;
        }
        Some(Ready::Moving {
            tip: *ready,
            raster,
            tail: sampled.tail,
            full: sampled.full.len(),
            step: *step,
            points: points.clone(),
            gain: *gain,
            light: *light,
        })
    }

    /// What this takes to keep.
    fn bytes(&self) -> usize {
        match self {
            Ready::Moving { raster, points, tail, .. } => {
                raster.w.len() * 4 + points.len() * 16 + tail.len() * 24 + 128
            }
            Ready::Card { profile: Some(p), .. } => p.w.len() * 4 + 128,
            Ready::Card { profile: None, .. } | Ready::Still { .. } => 128,
        }
    }

    fn gain(&self) -> f64 {
        match self {
            Ready::Still { gain, .. } | Ready::Moving { gain, .. } | Ready::Card { gain, .. } => *gain,
        }
    }

    fn light(&self) -> Light {
        match self {
            Ready::Still { light, .. } | Ready::Moving { light, .. } | Ready::Card { light, .. } => *light,
        }
    }

    /// Where the stroke can reach; `None` for a card, which has no bounds.
    fn bounds(&self) -> Option<(f64, f64, f64, f64)> {
        match self {
            Ready::Still { tip, x, y, .. } => {
                let r = tip.reach();
                Some((x - r, y - r, x + r, y + r))
            }
            Ready::Moving { tip, raster, tail, .. } => {
                let (mut x0, mut y0, mut x1, mut y1) = raster.bounds();
                let r = tip.reach();
                for t in tail {
                    x0 = x0.min(t.x - r);
                    y0 = y0.min(t.y - r);
                    x1 = x1.max(t.x + r);
                    y1 = y1.max(t.y + r);
                }
                Some((x0, y0, x1, y1))
            }
            Ready::Card { .. } => None,
        }
    }

    #[inline]
    fn weight(&self, x: f64, y: f64) -> f64 {
        match self {
            Ready::Still { tip, x: cx, y: cy, .. } => tip.weight(x - cx, y - cy),
            Ready::Moving { tip, raster, tail, .. } => {
                let mut w = raster.weight(x, y);
                for t in tail {
                    w += tip.weight(x - t.x, y - t.y) * t.w;
                }
                w
            }
            Ready::Card { x: ex, y: ey, sin, cos, travel, feather, profile, .. } => {
                // How far past the edge's starting place the point lies.
                let s = (x - ex) * cos + (y - ey) * sin;
                match profile {
                    Some(p) => p.at(s),
                    None => card_weight(s, *travel, *feather),
                }
            }
        }
    }
}

/// Prepared strokes kept between fields: a preview, a tile and a
/// histogram of the same picture, and the next frame of a stroke being
/// painted, share them. Keyed by the stroke's bits; a hand's latest
/// entry is also found by its tip and first point, so a growing path is
/// continued rather than built again. The oldest go when the rasters
/// pass [`CACHE_BYTES`].
#[derive(Default)]
struct Cache {
    map: HashMap<u64, Arc<Ready>>,
    order: VecDeque<u64>,
    bytes: usize,
    /// Prefix key of a hand → the key of its latest entry.
    latest: HashMap<u64, u64>,
}

fn cache() -> &'static Mutex<Cache> {
    static CACHE: OnceLock<Mutex<Cache>> = OnceLock::new();
    CACHE.get_or_init(|| Mutex::new(Cache::default()))
}

fn hash_f64(h: &mut impl Hasher, v: f64) {
    v.to_bits().hash(h);
}

fn hash_light(h: &mut impl Hasher, l: Light) {
    (l as u8).hash(h);
}

/// Everything about the stroke, bit for bit.
fn stroke_key(s: &Stroke) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    match s {
        Stroke::Hand { tip, stops, light, points } => {
            0u8.hash(&mut h);
            for v in [tip.rx, tip.ry, tip.angle, tip.feather, *stops] {
                hash_f64(&mut h, v);
            }
            hash_light(&mut h, *light);
            points.len().hash(&mut h);
            for &[x, y] in points {
                hash_f64(&mut h, x);
                hash_f64(&mut h, y);
            }
        }
        Stroke::Card { x, y, angle, travel, feather, stops, light } => {
            1u8.hash(&mut h);
            for v in [*x, *y, *angle, *travel, *feather, *stops] {
                hash_f64(&mut h, v);
            }
            hash_light(&mut h, *light);
        }
    }
    h.finish()
}

/// A hand's tip, time, light and first point: what every length of one
/// stroke being painted shares.
fn prefix_key(s: &Stroke) -> Option<u64> {
    let Stroke::Hand { tip, stops, light, points } = s else { return None };
    let &[x, y] = points.first()?;
    let mut h = std::collections::hash_map::DefaultHasher::new();
    for v in [tip.rx, tip.ry, tip.angle, tip.feather, *stops, x, y] {
        hash_f64(&mut h, v);
    }
    hash_light(&mut h, *light);
    Some(h.finish())
}

/// The stroke prepared, from the cache when it is there, grown from its
/// shorter self when that is, built otherwise.
fn prepared(stroke: &Stroke) -> Arc<Ready> {
    let key = stroke_key(stroke);
    let prefix = prefix_key(stroke);
    let shorter = {
        let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
        if let Some(r) = c.map.get(&key).cloned() {
            if let Some(pos) = c.order.iter().position(|k| *k == key) {
                c.order.remove(pos);
            }
            c.order.push_back(key);
            return r;
        }
        prefix.and_then(|p| c.latest.get(&p).copied()).and_then(|k| c.map.get(&k).cloned())
    };
    let ready =
        Arc::new(shorter.and_then(|s| Ready::grown(&s, stroke)).unwrap_or_else(|| Ready::new(stroke)));
    let mut c = cache().lock().unwrap_or_else(|e| e.into_inner());
    if let Some(old) = c.map.insert(key, Arc::clone(&ready)) {
        c.bytes -= old.bytes();
    } else {
        c.order.push_back(key);
    }
    c.bytes += ready.bytes();
    if let Some(p) = prefix {
        c.latest.insert(p, key);
    }
    while c.bytes > CACHE_BYTES && c.order.len() > 1 {
        let Some(k) = c.order.pop_front() else { break };
        if let Some(r) = c.map.remove(&k) {
            c.bytes -= r.bytes();
        }
    }
    ready
}

/// Which strokes can reach a point: the hands' bounds binned on a coarse
/// grid, the cards everywhere.
#[derive(Debug)]
struct Index {
    x0: f64,
    y0: f64,
    x1: f64,
    y1: f64,
    inv_cell: f64,
    nx: usize,
    ny: usize,
    cells: Vec<Vec<u16>>,
    everywhere: Vec<u16>,
}

impl Index {
    fn new(ready: &[Arc<Ready>]) -> Self {
        let mut everywhere = Vec::new();
        let mut bounded = Vec::new();
        let (mut x0, mut y0, mut x1, mut y1) =
            (f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY);
        for (i, r) in ready.iter().enumerate() {
            match r.bounds() {
                Some(b) => {
                    x0 = x0.min(b.0);
                    y0 = y0.min(b.1);
                    x1 = x1.max(b.2);
                    y1 = y1.max(b.3);
                    bounded.push((i as u16, b));
                }
                None => everywhere.push(i as u16),
            }
        }
        if bounded.is_empty() {
            return Self {
                x0: 0.0,
                y0: 0.0,
                x1: 0.0,
                y1: 0.0,
                inv_cell: 1.0,
                nx: 0,
                ny: 0,
                cells: Vec::new(),
                everywhere,
            };
        }
        let cell = ((x1 - x0).max(y1 - y0) / INDEX_CELLS as f64).max(1e-9);
        let nx = (((x1 - x0) / cell).floor() as usize + 1).min(MAX_CELLS);
        let ny = (((y1 - y0) / cell).floor() as usize + 1).min(MAX_CELLS);
        let mut cells = vec![Vec::new(); nx * ny];
        for (i, (bx0, by0, bx1, by1)) in bounded {
            let cx0 = (((bx0 - x0) / cell).floor() as usize).min(nx - 1);
            let cy0 = (((by0 - y0) / cell).floor() as usize).min(ny - 1);
            let cx1 = (((bx1 - x0) / cell).floor() as usize).min(nx - 1);
            let cy1 = (((by1 - y0) / cell).floor() as usize).min(ny - 1);
            for cy in cy0..=cy1 {
                for cx in cx0..=cx1 {
                    cells[cy * nx + cx].push(i);
                }
            }
        }
        Self { x0, y0, x1, y1, inv_cell: 1.0 / cell, nx, ny, cells, everywhere }
    }

    #[inline]
    fn near(&self, x: f64, y: f64) -> &[u16] {
        if self.nx == 0 || x < self.x0 || y < self.y0 || x > self.x1 || y > self.y1 {
            return &[];
        }
        // Not negative here, so the cast is the floor.
        let cx = (((x - self.x0) * self.inv_cell) as usize).min(self.nx - 1);
        let cy = (((y - self.y0) * self.inv_cell) as usize).min(self.ny - 1);
        &self.cells[cy * self.nx + cx]
    }
}

/// The long side of a `w×h` negative: the unit of the normalised
/// coordinates. At least one.
pub fn unit(w: usize, h: usize) -> f64 {
    w.max(h).max(1) as f64
}

/// From continuous pixel coordinates of the upright `w×h` negative to the
/// normalised ones: the centre to the origin, the long side to one.
pub fn normalize(w: usize, h: usize) -> Affine {
    let s = 1.0 / unit(w, h);
    Affine { a: s, b: 0.0, c: 0.0, d: s, tx: -(w as f64) * 0.5 * s, ty: -(h as f64) * 0.5 * s }
}

/// The strokes as a function of one plane's pixels. `to_norm` takes a
/// continuous coordinate of that plane (pixel `i` centred at `i + 0.5`)
/// to the normalised negative: for the print, [`normalize`] after the
/// print's map; for a tile, the same with the tile's offset. The prepared
/// strokes are shared, so a field read through another map is cheap, and
/// so is the next field of the same strokes.
#[derive(Clone, Debug)]
pub struct Field {
    ready: Arc<[Arc<Ready>]>,
    index: Arc<Index>,
    to_norm: Affine,
}

impl Field {
    /// `None` without a stroke, so the caller keeps its untouched path.
    pub fn new(strokes: &[Stroke], to_norm: Affine) -> Option<Self> {
        if strokes.is_empty() {
            return None;
        }
        let ready: Arc<[Arc<Ready>]> = strokes.iter().map(prepared).collect();
        let index = Arc::new(Index::new(&ready));
        Some(Self { ready, index, to_norm })
    }

    /// The times at the centre of pixel `(px, py)` of the plane.
    #[inline]
    pub fn times_at(&self, px: usize, py: usize) -> Times {
        self.times_xy(px as f64 + 0.5, py as f64 + 0.5)
    }

    /// The times at a continuous plane coordinate.
    #[inline]
    pub fn times_xy(&self, x: f64, y: f64) -> Times {
        let (nx, ny) = self.to_norm.apply(x, y);
        self.times_norm(nx, ny)
    }

    /// The times at a normalised point: `1 + Σ wᵢ (2^{sᵢ} − 1)` by light,
    /// never below zero.
    #[inline]
    pub fn times_norm(&self, x: f64, y: f64) -> Times {
        let near = self.index.near(x, y);
        if near.is_empty() && self.index.everywhere.is_empty() {
            return Times::UNIT;
        }
        let (mut soft, mut hard, mut all) = (0.0, 0.0, 0.0);
        for &i in near.iter().chain(&self.index.everywhere) {
            let r = &self.ready[i as usize];
            let w = r.weight(x, y);
            if w <= 0.0 {
                continue;
            }
            let dt = w * r.gain();
            all += dt;
            match r.light() {
                Light::Both => {
                    soft += dt;
                    hard += dt;
                }
                Light::Soft => soft += dt,
                Light::Hard => hard += dt,
            }
        }
        if soft == 0.0 && hard == 0.0 && all == 0.0 {
            return Times::UNIT;
        }
        Times { soft: (1.0 + soft).max(0.0), hard: (1.0 + hard).max(0.0), all: (1.0 + all).max(0.0) }
    }

    /// Row `py` of the plane: the pixels along it read with one
    /// normalisation each, and the span of them a stroke can reach.
    pub fn row(&self, py: usize) -> Row<'_> {
        let y = py as f64 + 0.5;
        let m = &self.to_norm;
        Row { field: self, ax: m.a, bx: m.b * y + m.tx, ay: m.c, by: m.d * y + m.ty }
    }

    /// The same strokes read through `inner` first: a plane whose pixels
    /// map into this one's coordinates by `inner`.
    pub fn through(&self, inner: &Affine) -> Self {
        Self { to_norm: self.to_norm.after(inner), ..self.clone() }
    }

    /// The same strokes read from a plane whose pixel `(0, 0)` is this
    /// one's `(dx, dy)`: a tile of the picture.
    pub fn shifted(&self, dx: usize, dy: usize) -> Self {
        self.through(&Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: dx as f64, ty: dy as f64 })
    }
}

/// One row of a plane under a field: pixel `px` is at the normalised
/// point `(ax·x + bx, ay·x + by)`, `x = px + ½`.
pub struct Row<'a> {
    field: &'a Field,
    ax: f64,
    bx: f64,
    ay: f64,
    by: f64,
}

impl Row<'_> {
    /// The times at pixel `px` of the row.
    #[inline]
    pub fn at(&self, px: usize) -> Times {
        let x = px as f64 + 0.5;
        self.field.times_norm(self.ax * x + self.bx, self.ay * x + self.by)
    }

    /// The pixels of `from..to` a stroke can reach; every pixel outside
    /// the span has unit times. The whole of it with a card in the field,
    /// else the row's line clipped against the hands' box.
    pub fn span(&self, from: usize, to: usize) -> Range<usize> {
        if !self.field.index.everywhere.is_empty() {
            return from..to;
        }
        self.span_of_hands(from, to)
    }

    /// The pixels of `from..to` where the times can change from one pixel
    /// to the next, and the constant times on either side: a hand is 0
    /// outside its box, a card 0 or 1 outside its penumbra and travel, so
    /// outside the range every pixel of the row before it has `left` and
    /// every one after it `right`. A reader does the field's work on the
    /// range alone.
    pub fn varying(&self, from: usize, to: usize) -> Varying {
        if to <= from {
            return Varying { range: from..from, left: Times::UNIT, right: Times::UNIT };
        }
        let mut range = self.span_of_hands(from, to);
        for &i in &self.field.index.everywhere {
            let Ready::Card { x: ex, y: ey, sin, cos, travel, feather, .. } = &*self.field.ready[i as usize]
            else {
                continue;
            };
            // Along the row `s = ds·x + sb`, `x = px + ½`; the weight
            // changes over `s` in `[−feather, travel + feather]`.
            let ds = self.ax * cos + self.ay * sin;
            let sb = (self.bx - ex) * cos + (self.by - ey) * sin;
            let (lo, hi) = (-feather, travel + feather);
            let band = if ds.abs() < 1e-12 {
                if sb < lo || sb > hi {
                    continue;
                }
                from..to
            } else {
                let (a, b) = ((lo - sb) / ds - 0.5, (hi - sb) / ds - 0.5);
                let start = (a.min(b).floor().max(0.0) as usize).clamp(from, to);
                let end = ((a.max(b).ceil() + 1.0).max(0.0) as usize).clamp(from, to);
                start..end
            };
            if band.is_empty() {
                continue;
            }
            range =
                if range.is_empty() { band } else { range.start.min(band.start)..range.end.max(band.end) };
        }
        let left = self.at_x(range.start as f64 - 0.5);
        let right = self.at_x(range.end as f64 + 0.5);
        Varying { range, left, right }
    }

    /// `span` without the cards: the row's line clipped against the
    /// hands' box.
    fn span_of_hands(&self, from: usize, to: usize) -> Range<usize> {
        let index = &self.field.index;
        if index.nx == 0 || to <= from {
            return from..from;
        }
        let (mut t0, mut t1) = (from as f64, to as f64);
        for (k, b, lo, hi) in [(self.ax, self.bx, index.x0, index.x1), (self.ay, self.by, index.y0, index.y1)]
        {
            if k == 0.0 {
                if b < lo || b > hi {
                    return from..from;
                }
                continue;
            }
            let (a, c) = ((lo - b) / k, (hi - b) / k);
            t0 = t0.max(a.min(c));
            t1 = t1.min(a.max(c));
        }
        if t1 <= t0 {
            return from..from;
        }
        let start = (t0.floor().max(0.0) as usize).clamp(from, to);
        let end = ((t1.ceil() + 1.0).max(0.0) as usize).clamp(from, to);
        start..end
    }

    /// The times at the continuous position `x` along the row.
    #[inline]
    fn at_x(&self, x: f64) -> Times {
        self.field.times_norm(self.ax * x + self.bx, self.ay * x + self.by)
    }
}

/// What [`Row::varying`] finds: the pixels whose times can differ from
/// their neighbours', and the times of every pixel before and after them.
#[derive(Clone, Debug, PartialEq)]
pub struct Varying {
    pub range: Range<usize>,
    pub left: Times,
    pub right: Times,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tip(rx: f64, ry: f64, angle: f64, feather: f64) -> Tip {
        Tip { rx, ry, angle, feather }
    }

    fn hand(tip: Tip, stops: f64, light: Light, points: &[[f64; 2]]) -> Stroke {
        Stroke::Hand { tip, stops, light, points: points.to_vec() }
    }

    fn card(x: f64, y: f64, angle: f64, travel: f64, feather: f64, stops: f64) -> Stroke {
        Stroke::Card { x, y, angle, travel, feather, stops, light: Light::Both }
    }

    /// The time factors of `strokes` at the normalised point `(x, y)`.
    fn times(strokes: &[Stroke], x: f64, y: f64) -> Times {
        Field::new(strokes, Affine::IDENTITY).map_or(Times::UNIT, |f| f.times_norm(x, y))
    }

    /// A straight pass of length 1 through `(cx, cy)` along `deg`.
    fn pass(cx: f64, cy: f64, deg: f64) -> [[f64; 2]; 2] {
        let (s, c) = deg.to_radians().sin_cos();
        [[cx - 0.5 * c, cy - 0.5 * s], [cx + 0.5 * c, cy + 0.5 * s]]
    }

    /// The narrowest feather a recipe can carry is a number so small that
    /// a 256th of it is nothing. Such a card passes the check as any
    /// positive feather does, and it is a hard card for every pixel.
    #[test]
    fn a_feather_too_narrow_to_sample_is_a_hard_card() {
        for (feather, travel) in [(5e-324, 0.0), (1e-322, 0.0), (5e-324, 1e-320), (1e-310, 0.0)] {
            let soft = card(0.0, 0.0, 0.0, travel, feather, 1.0);
            soft.validate().unwrap();
            let hard = card(0.0, 0.0, 0.0, 0.0, 0.0, 1.0);
            let to_norm = normalize(100, 80);
            let field = Field::new(std::slice::from_ref(&soft), to_norm).unwrap();
            let edge = Field::new(std::slice::from_ref(&hard), to_norm).unwrap();
            for (px, py) in [(10, 40), (49, 40), (51, 40), (90, 10)] {
                assert_eq!(field.times_at(px, py), edge.times_at(px, py), "feather {feather} at {px},{py}");
            }
        }
    }

    #[test]
    fn the_penumbra_is_the_disc_past_the_edge() {
        assert_eq!(penumbra(-1.0), 1.0);
        assert_eq!(penumbra(1.0), 0.0);
        assert!((penumbra(0.0) - 0.5).abs() < 1e-12);
        assert_eq!(penumbra(-5.0), 1.0);
        assert_eq!(penumbra(5.0), 0.0);
        let mut last = 1.0;
        for i in 0..=200 {
            let t = -1.0 + i as f64 / 100.0;
            let p = penumbra(t);
            assert!(p <= last + 1e-12 && (0.0..=1.0).contains(&p), "{t}: {p}");
            last = p;
        }
        // Symmetric about the edge.
        assert!((penumbra(0.3) + penumbra(-0.3) - 1.0).abs() < 1e-12);
        // Smooth at the ends: the slope goes to zero.
        assert!(penumbra(-0.999) > 0.999 && penumbra(0.999) < 0.001);
    }

    /// The closed antiderivative against the trapezoid rule, and its
    /// joins: −1 at −1, 0 at 1, continuous.
    #[test]
    fn the_penumbra_integral_is_its_antiderivative() {
        assert!((penumbra_integral(-1.0) + 1.0).abs() < 1e-12);
        assert!(penumbra_integral(1.0).abs() < 1e-12);
        assert!((penumbra_integral(-0.999999) + 0.999999).abs() < 1e-6);
        assert!(penumbra_integral(0.999999).abs() < 1e-6);
        for (a, b) in [(-1.5, 0.3), (-0.7, 0.7), (0.2, 2.0), (-3.0, 3.0), (-0.4, -0.1)] {
            let n = 20000;
            let h = (b - a) / n as f64;
            let mut sum = 0.5 * (penumbra(a) + penumbra(b));
            for i in 1..n {
                sum += penumbra(a + i as f64 * h);
            }
            let numeric = sum * h;
            let closed = penumbra_integral(b) - penumbra_integral(a);
            assert!((numeric - closed).abs() < 1e-7, "∫[{a},{b}]: {numeric} vs {closed}");
        }
        // Over the whole band the symmetric penumbra integrates to one.
        assert!((penumbra_integral(1.0) - penumbra_integral(-1.0) - 1.0).abs() < 1e-12);
    }

    #[test]
    fn the_circle_distance_is_exact_and_the_tip_turns() {
        let c = tip(0.25, 0.25, 0.0, 0.0);
        assert!((c.distance(0.0, 0.0) + 0.25).abs() < 1e-12);
        assert!(c.distance(0.25, 0.0).abs() < 1e-12);
        assert!((c.distance(0.5, 0.0) - 0.25).abs() < 1e-12);
        assert!((c.distance(0.0, 0.5) - 0.25).abs() < 1e-12);
        // Twice as wide as high: the axis ends are on its edge.
        let e = tip(0.4, 0.2, 0.0, 0.0);
        assert!(e.distance(0.4, 0.0).abs() < 1e-12 && e.distance(0.0, 0.2).abs() < 1e-12);
        assert!(e.distance(0.0, 0.0) < 0.0 && e.distance(0.5, 0.0) > 0.0 && e.distance(0.0, 0.3) > 0.0);
        // Turned 90° clockwise it stands.
        let t = tip(0.4, 0.2, 90.0, 0.0);
        assert!(t.distance(0.0, 0.4).abs() < 1e-9 && t.distance(0.2, 0.0).abs() < 1e-9);
        assert!(t.distance(0.4, 0.0) > 0.0);
        // The chord through the centre: the axes, and a circle every way.
        assert!((e.half_chord(1.0, 0.0) - 0.4).abs() < 1e-12 && (e.half_chord(0.0, 1.0) - 0.2).abs() < 1e-12);
        assert!((t.half_chord(0.0, 1.0) - 0.4).abs() < 1e-12);
        let (s, co) = 33f64.to_radians().sin_cos();
        assert!((c.half_chord(co, s) - 0.25).abs() < 1e-12);
        // Resting, the tip takes the whole light inside, half at the edge.
        let soft = tip(0.25, 0.25, 0.0, 0.05);
        assert_eq!(soft.weight(0.0, 0.0), 1.0);
        assert!((soft.weight(0.25, 0.0) - 0.5).abs() < 1e-12);
        assert_eq!(soft.weight(0.31, 0.0), 0.0);
        assert!(soft.reach() >= 0.3 && soft.inside() <= 0.2);
        // The dwell is the chord while the feather fits inside the tip; a
        // tip softer than it is wide is never fully dark at its middle, but
        // the penumbra's tail adds more dwell than the middle loses.
        assert!((soft.half_dwell(1.0, 0.0) - 0.25).abs() < 1e-12);
        let fuzzy = tip(0.05, 0.05, 0.0, 0.2);
        assert!(fuzzy.weight(0.0, 0.0) < 1.0 && fuzzy.half_dwell(1.0, 0.0) > 0.05);
        let n = 100_000;
        let numeric: f64 =
            (0..n).map(|i| fuzzy.weight((i as f64 + 0.5) / n as f64 - 0.5, 0.0)).sum::<f64>() / n as f64;
        assert!((numeric - 2.0 * fuzzy.half_dwell(1.0, 0.0)).abs() < 1e-5, "{numeric}");
    }

    /// A straight pass through a point gives it the stroke's stops, for a
    /// circle in every direction and an ellipse along its axes, with and
    /// without a feather; sharp tips to the sampling, soft ones closer.
    #[test]
    fn a_pass_through_the_middle_gives_the_stops() {
        let tips = [
            (tip(0.1, 0.1, 0.0, 0.0), vec![0.0, 37.0, 90.0, 123.0, -60.0]),
            (tip(0.1, 0.1, 0.0, 0.03), vec![0.0, 37.0, 90.0, 123.0]),
            (tip(0.1, 0.1, 0.0, 0.2), vec![0.0, 71.0]),
            (tip(0.15, 0.05, 0.0, 0.0), vec![0.0, 90.0]),
            (tip(0.15, 0.05, 30.0, 0.02), vec![30.0, 120.0]),
            (tip(0.15, 0.05, 0.0, 0.02), vec![45.0]),
        ];
        for (t, directions) in &tips {
            for &deg in directions {
                let s = hand(*t, 1.0, Light::Both, &pass(0.1, -0.2, deg));
                let w = s.weight(0.1, -0.2);
                let tol = if t.feather == 0.0 { 0.07 } else { 0.02 };
                assert!((w - 1.0).abs() < tol, "{t:?} along {deg}°: weight {w}");
                assert!((times(std::slice::from_ref(&s), 0.1, -0.2).soft - 2.0).abs() < 2.0 * tol);
                // Beside the pass the chord: at 60 % of the half-axis across, 80 %.
                if t.rx == t.ry && t.feather == 0.0 {
                    let (si, co) = deg.to_radians().sin_cos();
                    let (px, py) = (0.1 - si * 0.06, -0.2 + co * 0.06);
                    assert!((s.weight(px, py) - 0.8).abs() < 0.07, "{deg}° beside: {}", s.weight(px, py));
                }
                // Far from the path nothing.
                assert_eq!(s.weight(0.1, 0.8), 0.0);
            }
        }
    }

    #[test]
    fn a_click_rests_the_tip_and_passes_add() {
        let t = tip(0.1, 0.05, 20.0, 0.02);
        let click = hand(t, 1.0, Light::Both, &[[0.3, 0.1]]);
        let still = hand(t, 1.0, Light::Both, &[[0.3, 0.1], [0.3, 0.1], [0.3, 0.1]]);
        for i in 0..40 {
            for j in 0..40 {
                let (x, y) = (0.3 - 0.2 + i as f64 / 100.0, 0.1 - 0.2 + j as f64 / 100.0);
                let plain = t.weight(x - 0.3, y - 0.1);
                assert!((click.weight(x, y) - plain).abs() < 1e-12);
                assert!((still.weight(x, y) - plain).abs() < 1e-12);
            }
        }
        // Back and forth: twice the time at the middle.
        let there = pass(0.0, 0.0, 0.0);
        let back = hand(t, 1.0, Light::Both, &[there[0], there[1], there[0]]);
        assert!((back.weight(0.0, 0.0) - 2.0).abs() < 0.05, "{}", back.weight(0.0, 0.0));
        // Two strokes of a stop each: three times the base, not four.
        let one = hand(tip(0.1, 0.1, 0.0, 0.02), 1.0, Light::Both, &there);
        let two = [one.clone(), one.clone()];
        assert!((times(&two, 0.0, 0.0).soft - 3.0).abs() < 0.05);
        // A dodge halves; of everything, none is left.
        let dodge = hand(tip(0.1, 0.1, 0.0, 0.0), -1.0, Light::Both, &[[0.0, 0.0]]);
        assert_eq!(times(std::slice::from_ref(&dodge), 0.0, 0.0).soft, 0.5);
        let deep = hand(tip(0.1, 0.1, 0.0, 0.0), -4.0, Light::Both, &[[0.0, 0.0]]);
        assert_eq!(times(&[deep.clone(), deep], 0.0, 0.0), Times { soft: 0.0, hard: 0.0, all: 0.0 });
        // One light at a time: the other pass keeps its time; a look takes it all.
        let soft = hand(tip(0.1, 0.1, 0.0, 0.0), 1.0, Light::Soft, &[[0.0, 0.0]]);
        assert_eq!(times(std::slice::from_ref(&soft), 0.0, 0.0), Times { soft: 2.0, hard: 1.0, all: 2.0 });
        let hard = hand(tip(0.1, 0.1, 0.0, 0.0), 1.0, Light::Hard, &[[0.0, 0.0]]);
        assert_eq!(times(&[hard], 0.0, 0.0), Times { soft: 1.0, hard: 2.0, all: 2.0 });
        assert!(!times(&[soft], 0.0, 0.0).is_even() && times(&[dodge], 0.0, 0.0).is_even());
        assert!(times(&[one], 0.0, 0.8).is_unit());
    }

    /// The card's closed ramp against the edge integrated step by step
    /// over its push, still and pushed, hard and soft.
    #[test]
    fn the_card_ramp_is_the_edge_summed_over_its_push() {
        // Still, hard: a step lit on the right; soft: half on the edge.
        let step = card(0.2, 0.0, 0.0, 0.0, 0.0, 2.0);
        assert_eq!(times(std::slice::from_ref(&step), 0.5, 0.7).soft, 4.0);
        assert!(times(&[step], -0.1, -0.7).is_unit());
        let still = card(0.2, 0.0, 0.0, 0.0, 0.1, 1.0);
        assert!((still.weight(0.2, 0.4) - 0.5).abs() < 1e-12);
        assert_eq!(still.weight(0.35, 0.0), 1.0);
        assert_eq!(still.weight(0.05, 0.0), 0.0);
        // Pointing down (y down, clockwise): below the line is lit.
        let down = card(0.0, 0.1, 90.0, 0.0, 0.0, 1.0);
        assert!(down.weight(0.5, 0.4) == 1.0 && down.weight(0.5, -0.4) == 0.0);
        // Pushed: the ramp, 0 behind the start, 1 beyond the end.
        for (feather, travel, angle) in
            [(0.0, 0.3, 0.0), (0.05, 0.3, 0.0), (0.2, 0.1, 0.0), (0.08, 0.25, 40.0)]
        {
            let pushed = card(0.1, -0.1, angle, travel, feather, 1.0);
            let (s, c) = angle.to_radians().sin_cos();
            for k in -20..=40 {
                let along = k as f64 * 0.025;
                let (x, y) = (0.1 + along * c - 0.3 * s, -0.1 + along * s + 0.3 * c);
                let closed = pushed.weight(x, y);
                let n = 4000;
                let numeric: f64 = (0..n)
                    .map(|i| {
                        let e = (i as f64 + 0.5) / n as f64 * travel;
                        edge_weight(e - along, feather)
                    })
                    .sum::<f64>()
                    / n as f64;
                assert!(
                    (closed - numeric).abs() < 2e-3,
                    "feather {feather} travel {travel} at {along}: {closed} vs {numeric}"
                );
            }
            assert_eq!(pushed.weight(0.1 - 2.0 * c, -0.1 - 2.0 * s), 0.0);
            assert!((pushed.weight(0.1 + 2.0 * c, -0.1 + 2.0 * s) - 1.0).abs() < 1e-12);
        }
        // The tabulated profile reads as the integrals it samples.
        for (feather, travel) in [(0.05, 0.0), (0.05, 0.3), (0.2, 0.1), (0.01, 2.5)] {
            let profile = Profile::new(travel, feather).unwrap();
            for k in 0..=2000 {
                let s = -0.1 + (travel + 0.2) * k as f64 / 2000.0;
                let off = (profile.at(s) - card_weight(s, travel, feather)).abs();
                assert!(off < 1e-4, "feather {feather} travel {travel} at {s}: off by {off}");
            }
        }
        assert!(Profile::new(0.3, 0.0).is_none());
    }

    #[test]
    fn a_turn_of_the_picture_turns_every_stroke_with_it() {
        let strokes = [
            hand(tip(0.2, 0.1, 20.0, 0.03), 1.0, Light::Soft, &[[0.3, 0.1], [0.1, -0.2], [0.4, 0.4]]),
            card(0.2, 0.0, 10.0, 0.1, 0.05, -0.5),
        ];
        let turned_once = turned(&strokes, 1);
        assert_eq!(
            turned_once[1],
            Stroke::Card {
                x: 0.0,
                y: 0.2,
                angle: 100.0,
                travel: 0.1,
                feather: 0.05,
                stops: -0.5,
                light: Light::Both
            }
        );
        for (px, py) in [(0.42, 0.14), (0.0, 0.0), (0.25, -0.1), (-0.3, 0.2)] {
            // A point of the picture and the strokes move together.
            // The moving hand is read from a raster laid along the turned
            // picture, so its blend differs in the fourth place; the card
            // is exact.
            let a = times(&strokes, px, py);
            let b = times(&turned_once, -py, px);
            assert!((a.soft - b.soft).abs() < 1e-3 && (a.hard - b.hard).abs() < 1e-12, "{a:?} vs {b:?}");
        }
        // Four quarters are the picture again.
        let round = turned(&turned(&turned(&turned_once, 1), 1), 1);
        for (s, r) in strokes.iter().zip(&round) {
            let (a, b) = (times(std::slice::from_ref(s), 0.3, 0.1), times(std::slice::from_ref(r), 0.3, 0.1));
            assert!((a.soft - b.soft).abs() < 1e-9, "{a:?} vs {b:?}");
        }
        if let (Stroke::Hand { points: a, .. }, Stroke::Hand { points: b, .. }) = (&strokes[0], &round[0]) {
            assert_eq!(a, b);
        }
        assert_eq!(wrap_degrees(270.0), -90.0);
        assert_eq!(wrap_degrees(-190.0), 170.0);
        assert_eq!(wrap_degrees(180.0), 180.0);
    }

    #[test]
    fn the_field_reads_the_negative_through_a_map() {
        // A 400×300 negative: the unit is 400, the centre (200, 150).
        let n = normalize(400, 300);
        assert_eq!(n.apply(200.0, 150.0), (0.0, 0.0));
        assert_eq!(n.apply(400.0, 150.0), (0.5, 0.0));
        assert_eq!(n.apply(200.0, 300.0), (0.0, 0.375));
        let strokes = [hand(tip(0.1, 0.1, 0.0, 0.0), 1.0, Light::Both, &[[0.25, 0.0]])];
        let f = Field::new(&strokes, n).unwrap();
        // Pixel (300, 150) is the tip's centre.
        assert_eq!(f.times_at(299, 149).soft, 2.0);
        assert!(f.times_at(100, 150).is_unit());
        // Through a window that starts at (250, 100): its pixel (50, 50) is the same point.
        let window = Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 250.0, ty: 100.0 };
        let g = f.through(&window);
        assert_eq!(g.times_at(49, 49), f.times_at(299, 149));
        assert_eq!(f.shifted(250, 100).times_at(49, 49), f.times_at(299, 149));
        assert!(Field::new(&[], n).is_none());
        // A turned picture (300×400 upright) keeps the unit and the stroke follows.
        let m = normalize(300, 400);
        let tf = Field::new(&turned(&strokes, 1), m).unwrap();
        // (300, 150) in the 400×300 picture, turned clockwise, is (300 − 150, 300) = (150, 300).
        assert_eq!(tf.times_at(149, 299).soft, 2.0);
    }

    /// The raster reads what the plain sum over every sample gives: within
    /// a percent of the stroke where the edge is soft; a hard edge, read
    /// between grid points, is softened by at most a sixteenth of the tip,
    /// so there the two agree once the edge's own band is left out.
    /// A stroke begins where the hand is set down: the first point of a
    /// drag has the stops like a click, and keeps them as the path grows;
    /// the end has them too; a path a hair long is the click.
    #[test]
    fn a_stroke_begins_and_ends_with_its_stops() {
        for t in [tip(0.1, 0.1, 0.0, 0.02), tip(0.1, 0.1, 0.0, 0.0), tip(0.15, 0.05, 30.0, 0.01)] {
            let click = hand(t, 1.0, Light::Both, &[[0.0, 0.0]]);
            let hair = hand(t, 1.0, Light::Both, &[[0.0, 0.0], [1e-4, 0.0]]);
            for (x, y) in [(0.0, 0.0), (0.05, 0.0), (0.0, 0.04), (0.09, 0.0), (-0.03, 0.03)] {
                let (a, b) = (click.weight(x, y), hair.weight(x, y));
                assert!((a - b).abs() < 0.02, "{t:?} at ({x}, {y}): click {a}, hair {b}");
            }
            // A quarter of the half-axis on, the first frame of a drag: the
            // stops, and the little the hand moved while still over it.
            let first = hand(t, 1.0, Light::Both, &[[0.0, 0.0], [0.025, 0.0]]);
            assert!(
                (first.weight(0.0, 0.0) - 1.125).abs() < 0.05,
                "{t:?}: first frame {}",
                first.weight(0.0, 0.0)
            );
            // A long pass: its start, its middle and its end have the stops.
            let long = hand(t, 1.0, Light::Both, &[[0.0, 0.0], [0.6, 0.0]]);
            for x in [0.0, 0.3, 0.6] {
                let tol = if t.feather == 0.0 { 0.07 } else { 0.03 };
                assert!((long.weight(x, 0.0) - 1.0).abs() < tol, "{t:?} at {x}: {}", long.weight(x, 0.0));
            }
            // Before the start, nothing more than the resting tip gives.
            assert!(long.weight(-0.2, 0.0) < 1e-9);
        }
    }

    #[test]
    fn the_raster_reads_the_plain_sum() {
        let scribble: Vec<[f64; 2]> =
            (0..60).map(|i| [0.2 * (i as f64 * 0.7).sin(), 0.15 * (i as f64 * 0.45).cos() + 0.1]).collect();
        let soft = hand(tip(0.03, 0.02, 15.0, 0.01), 0.7, Light::Both, &scribble);
        let thin = hand(tip(0.01, 0.01, 0.0, 0.0), -1.0, Light::Hard, &[[-0.5, -0.4], [0.5, 0.45]]);
        let edge = card(0.0, 0.0, -30.0, 0.2, 0.05, 0.5);
        let strokes = [soft.clone(), thin.clone(), edge.clone()];
        let field = Field::new(&strokes, Affine::IDENTITY).unwrap();
        let plain_hand = |s: &Stroke, x: f64, y: f64| -> f64 {
            let Stroke::Hand { tip, points, .. } = s else { unreachable!() };
            let s = sample_path(tip, points);
            s.full.iter().chain(s.tail.iter()).map(|p| tip.weight(x - p.x, y - p.y) * p.w).sum()
        };
        let (mut touched, mut worst_soft) = (0, 0.0f64);
        for i in 0..50 {
            for j in 0..50 {
                let (x, y) = (-0.6 + i as f64 * 0.025, -0.5 + j as f64 * 0.025);
                let t = field.times_norm(x, y);
                let ws = plain_hand(&soft, x, y);
                let wt = plain_hand(&thin, x, y);
                let we = edge.weight(x, y);
                // The soft hand and the card are read by the soft time.
                let soft_expect = (1.0 + ws * (2f64.powf(0.7) - 1.0) + we * (2f64.powf(0.5) - 1.0)).max(0.0);
                worst_soft = worst_soft.max((t.soft - soft_expect).abs());
                assert!((t.soft - soft_expect).abs() < 1e-2, "({x}, {y}): {} vs {soft_expect}", t.soft);
                // The thin hard hand by the hard time alone, which the other
                // two share, away from its edge band.
                let hard_expect =
                    (1.0 + ws * (2f64.powf(0.7) - 1.0) + wt * (0.5 - 1.0) + we * (2f64.powf(0.5) - 1.0))
                        .max(0.0);
                let band = wt > 0.0 && wt < 0.5;
                if !band {
                    assert!((t.hard - hard_expect).abs() < 0.05, "({x}, {y}): {} vs {hard_expect}", t.hard);
                }
                touched += usize::from(!t.is_unit());
            }
        }
        assert!(touched > 500 && worst_soft > 0.0, "{touched} {worst_soft}");
        // A still hand is read exactly, through the raster's bypass.
        let still = hand(tip(0.03, 0.02, 15.0, 0.01), 0.7, Light::Both, &[[0.1, 0.1]]);
        let f = Field::new(std::slice::from_ref(&still), Affine::IDENTITY).unwrap();
        for (x, y) in [(0.1, 0.1), (0.125, 0.1), (0.1, 0.13), (0.14, 0.12)] {
            let w = Tip { rx: 0.03, ry: 0.02, angle: 15.0, feather: 0.01 }.weight(x - 0.1, y - 0.1);
            assert_eq!(f.times_norm(x, y).soft, 1.0 + w * (2f64.powf(0.7) - 1.0));
        }
    }

    #[test]
    fn a_path_that_grows_is_continued_and_reads_as_if_built_whole() {
        let path: Vec<[f64; 2]> =
            (0..80).map(|i| [-0.4 + i as f64 * 0.009, 0.1 * (i as f64 * 0.3).sin() - 0.2]).collect();
        let t = tip(0.04, 0.025, 20.0, 0.015);
        // Every length of the path in turn, as it is painted.
        let mut grown = None;
        for n in 1..=path.len() {
            let stroke = hand(t, 1.2, Light::Both, &path[..n]);
            let from = grown.take().map(Arc::new);
            let fresh = Ready::new(&stroke);
            let next = match &from {
                Some(f) => Ready::grown(f, &stroke).unwrap_or_else(|| Ready::new(&stroke)),
                None => Ready::new(&stroke),
            };
            if n > 2 {
                assert!(matches!(next, Ready::Moving { .. }), "{n}");
            }
            // The two rasters sit on lattices a part of a step apart, so
            // each reads the plain sum within the raster's own blur.
            let s = sample_path(&t, &path[..n]);
            for i in 0..40 {
                for j in 0..30 {
                    let (x, y) = (-0.5 + i as f64 * 0.025, -0.45 + j as f64 * 0.02);
                    let plain: f64 =
                        s.full.iter().chain(s.tail.iter()).map(|p| t.weight(x - p.x, y - p.y) * p.w).sum();
                    let (a, b) = (next.weight(x, y), fresh.weight(x, y));
                    assert!((a - plain).abs() < 1e-2, "{n} ({x}, {y}): grown {a} vs plain {plain}");
                    assert!((a - b).abs() < 1e-2, "{n} ({x}, {y}): grown {a} vs fresh {b}");
                }
            }
            grown = Some(next);
        }
        // Through the cache: the same stroke twice is the same preparation.
        let stroke = hand(t, 1.2, Light::Both, &path);
        let a = prepared(&stroke);
        let b = prepared(&stroke);
        assert!(Arc::ptr_eq(&a, &b));
        // A different time or light is not continued.
        let other = hand(t, 0.8, Light::Soft, &path);
        assert!(Ready::grown(&a, &other).is_none());
        assert!(!Arc::ptr_eq(&prepared(&other), &a));
    }

    /// A stamp of the tip on a growing raster visits the points of the
    /// tip's box and no others, so the box must hold every point the tip
    /// gives weight to; and for a long narrow tip it is smaller than the
    /// square of the reach, or the stamp gains nothing.
    #[test]
    fn the_tips_box_holds_all_of_its_weight() {
        for t in [
            tip(1.0, 0.1, 0.0, 0.1),
            tip(0.3, 0.05, 0.0, 0.05),
            tip(0.3, 0.05, 30.0, 0.02),
            tip(0.05, 0.3, -70.0, 0.05),
            tip(0.3, 0.03, 45.0, 0.03),
            tip(0.2, 0.1, 0.0, 0.05),
            tip(0.1, 0.1, 0.0, 0.05),
            tip(0.3, 0.03, 10.0, 0.0),
        ] {
            let ready = ReadyTip::new(t);
            let (ex, ey) = ready.extent();
            let reach = ready.reach();
            assert!(ex <= reach && ey <= reach);
            let n = 800;
            let (mut touched, mut outside, mut worst) = (0usize, 0usize, 0.0f64);
            for i in 0..=n {
                for j in 0..=n {
                    let x = reach * (2.0 * i as f64 / n as f64 - 1.0);
                    let y = reach * (2.0 * j as f64 / n as f64 - 1.0);
                    let w = ready.weight(x, y);
                    if w > 0.0 {
                        touched += 1;
                        if x.abs() > ex || y.abs() > ey {
                            outside += 1;
                            worst = worst.max(w);
                        }
                    }
                }
            }
            assert!(touched > 1000, "{t:?}: {touched}");
            assert_eq!(outside, 0, "{t:?}: box ({ex}, {ey}), weight up to {worst} outside it");
        }
        // Lying along an axis, the long narrow tip's box is narrow too.
        let (ex, ey) = ReadyTip::new(tip(0.3, 0.03, 0.0, 0.03)).extent();
        assert!(ey < 0.25 * ex, "({ex}, {ey})");
    }

    /// The cache lets its oldest strokes go by what they take to keep. A
    /// soft card keeps its profile, and every place a card is dragged
    /// through is a stroke of its own.
    #[test]
    fn a_soft_card_is_counted_with_its_profile() {
        for (travel, feather) in [(0.25, 0.01), (2.0, 0.001)] {
            let ready = Ready::new(&card(0.0, 0.0, 0.0, travel, feather, 1.0));
            let Ready::Card { profile: Some(p), .. } = &ready else { panic!("no profile") };
            assert!(p.w.len() > 1000);
            assert!(
                ready.bytes() >= p.w.len() * 4,
                "{} samples counted as {} bytes",
                p.w.len(),
                ready.bytes()
            );
        }
        // A hard card keeps nothing but itself.
        assert_eq!(Ready::new(&card(0.0, 0.0, 0.0, 0.25, 0.0, 1.0)).bytes(), 128);
    }

    #[test]
    fn a_row_span_holds_every_pixel_a_hand_reaches() {
        let strokes = [
            hand(tip(0.05, 0.03, 0.0, 0.02), 1.0, Light::Both, &[[-0.3, -0.1], [-0.1, 0.05]]),
            hand(tip(0.02, 0.02, 0.0, 0.0), -1.0, Light::Hard, &[[0.25, 0.2]]),
        ];
        let (w, h) = (300usize, 200usize);
        let field = Field::new(&strokes, normalize(w, h)).unwrap();
        let mut inside = 0;
        for py in 0..h {
            let row = field.row(py);
            let span = row.span(0, w);
            for px in 0..w {
                let t = field.times_at(px, py);
                assert_eq!(t, row.at(px));
                if !t.is_unit() {
                    assert!(span.contains(&px), "row {py}: pixel {px} outside {span:?}");
                    inside += 1;
                }
            }
            assert!(span.end <= w && span.start <= span.end);
        }
        assert!(inside > 1000, "{inside}");
        // A card reaches everywhere.
        let with_card = [strokes[0].clone(), card(0.0, 0.0, 0.0, 0.0, 0.1, 0.5)];
        let f = Field::new(&with_card, normalize(w, h)).unwrap();
        assert_eq!(f.row(10).span(0, w), 0..w);
        // But its times only vary in a band: outside the range every
        // pixel reads as the side's constant, with hands, with a pushed
        // and turned card, with a hard one, and on a turned plane.
        let cards = [
            with_card.to_vec(),
            vec![card(0.1, -0.05, 30.0, 0.2, 0.04, 1.0), strokes[1].clone()],
            vec![card(0.0, 0.0, 0.0, 0.0, 0.0, 1.0)],
            vec![card(0.0, 0.0, 90.0, 0.0, 0.05, 1.0)],
        ];
        for strokes in &cards {
            for (f, (fw, fh)) in [
                (Field::new(strokes, normalize(w, h)).unwrap(), (w, h)),
                (
                    Field::new(strokes, normalize(w, h)).unwrap().through(&Affine {
                        a: 0.0,
                        b: 1.0,
                        c: -1.0,
                        d: 0.0,
                        tx: 0.0,
                        ty: w as f64,
                    }),
                    (h, w),
                ),
            ] {
                let mut varied = 0usize;
                for py in 0..fh {
                    let row = f.row(py);
                    let v = row.varying(0, fw);
                    assert!(v.range.end <= fw && v.range.start <= v.range.end);
                    varied += v.range.len();
                    for px in 0..fw {
                        let t = row.at(px);
                        if px < v.range.start {
                            assert_eq!(t, v.left, "row {py} pixel {px} left of {:?}", v.range);
                        } else if px >= v.range.end {
                            assert_eq!(t, v.right, "row {py} pixel {px} right of {:?}", v.range);
                        }
                    }
                }
                assert!(varied < fw * fh / 2, "{varied} of {} vary", fw * fh);
            }
        }
        // A turned plane: the row runs down the negative.
        let turned = field.through(&Affine { a: 0.0, b: 1.0, c: -1.0, d: 0.0, tx: 0.0, ty: w as f64 });
        for py in 0..w {
            let row = turned.row(py);
            let span = row.span(0, h);
            for px in 0..h {
                if !turned.times_at(px, py).is_unit() {
                    assert!(span.contains(&px), "turned row {py}: pixel {px} outside {span:?}");
                }
            }
        }
    }

    #[test]
    fn json_is_tagged_and_within_its_limits() {
        let h = hand(tip(0.3, 0.15, 12.5, 0.05), 1.5, Light::Soft, &[[0.1, -0.2], [0.2, -0.1]]);
        let json = serde_json::to_string(&h).unwrap();
        assert_eq!(
            json,
            r#"{"tool":"hand","tip":{"rx":0.3,"ry":0.15,"angle":12.5,"feather":0.05},"stops":1.5,"light":"soft","points":[[0.1,-0.2],[0.2,-0.1]]}"#
        );
        assert_eq!(serde_json::from_str::<Stroke>(&json).unwrap(), h);
        let c = card(0.0, 0.3, 90.0, 0.0, 0.0, -1.0);
        let json = serde_json::to_string(&c).unwrap();
        assert_eq!(json, r#"{"tool":"card","x":0.0,"y":0.3,"angle":90.0,"stops":-1.0}"#);
        assert_eq!(serde_json::from_str::<Stroke>(&json).unwrap(), c);
        validate(&[h.clone(), c.clone()]).unwrap();
        let bad = |s: Stroke| assert!(s.validate().is_err(), "{s:?}");
        bad(hand(tip(0.0, 0.1, 0.0, 0.0), 1.0, Light::Both, &[[0.0, 0.0]]));
        bad(hand(tip(0.1, 0.1, 0.0, 0.0), 4.5, Light::Both, &[[0.0, 0.0]]));
        bad(hand(tip(0.1, 0.1, 0.0, -0.1), 1.0, Light::Both, &[[0.0, 0.0]]));
        bad(hand(tip(0.1, 0.1, 0.0, 0.0), 1.0, Light::Both, &[[3.0, 0.0]]));
        bad(hand(tip(0.1, f64::NAN, 0.0, 0.0), 1.0, Light::Both, &[[0.0, 0.0]]));
        bad(hand(tip(0.1, 0.1, 0.0, 0.0), 1.0, Light::Both, &[]));
        bad(hand(tip(0.1, 0.1, 0.0, 0.0), 1.0, Light::Both, &vec![[0.0, 0.0]; MAX_POINTS + 1]));
        bad(card(0.0, 0.0, 0.0, -0.1, 0.0, 1.0));
        bad(card(0.0, 0.0, 0.0, 0.0, 9.0, 1.0));
        bad(card(2.5, 0.0, 0.0, 0.0, 0.0, 1.0));
        assert!(validate(&vec![c.clone(); MAX_STROKES + 1]).is_err());
        assert!(validate(&vec![c; MAX_STROKES]).is_ok());
        assert_eq!([Light::Both, Light::Soft, Light::Hard].map(Light::name), ["both", "soft", "hard"]);
    }
}
