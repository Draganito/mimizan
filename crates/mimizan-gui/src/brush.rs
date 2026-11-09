//! The brush: the one hand and the one card the printer owns.
//!
//! In the darkroom the dodging tool is made once and kept: a wire with a
//! disc of card on it, a card with a straight edge. The hand learns its
//! shadow. So the brush belongs to the program, not to the picture: its
//! shape, its penumbra, its stops and its light are set on the stage with
//! handles, then locked, and every negative is worked with the same
//! brush until it is set again. What a picture keeps are the strokes
//! (`mimizan_core::burn::Stroke`), in its recipe.
//!
//! Two tips: the hand ([`Tip`], an ellipse with a soft edge, painted along
//! a path) and the card (a straight edge with a soft shadow, pushed from
//! one place to another). The brush is set in the picture as shown, after
//! the user's turns; the strokes are stored turned back onto the file's
//! upright negative.

use egui::Pos2;
use mimizan_core::burn::{self, Light, Stroke, Tip, SIZE_LIMIT, STOP_LIMIT};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Tool {
    #[default]
    Hand,
    Card,
}

impl Tool {
    pub fn name(self) -> &'static str {
        match self {
            Tool::Hand => "hand",
            Tool::Card => "card",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct HandBrush {
    pub tip: Tip,
    pub stops: f64,
    pub light: Light,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct CardBrush {
    /// Degrees clockwise from +x of the normal into the lit side: `-90`
    /// lights everything above the edge.
    pub angle: f64,
    pub feather: f64,
    pub stops: f64,
    pub light: Light,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Brush {
    pub tool: Tool,
    pub hand: HandBrush,
    pub card: CardBrush,
}

impl Default for Brush {
    /// A hand the size of a fist on an 8×10, a soft edge, a quarter stop
    /// more; a card lit above its edge, a wider shadow, half a stop more.
    fn default() -> Self {
        Self {
            tool: Tool::Hand,
            hand: HandBrush {
                tip: Tip { rx: 0.08, ry: 0.08, angle: 0.0, feather: 0.03 },
                stops: 0.25,
                light: Light::Both,
            },
            card: CardBrush { angle: -90.0, feather: 0.1, stops: 0.5, light: Light::Both },
        }
    }
}

/// Stops are set to this step.
pub const STOPS_STEP: f64 = 0.01;

fn round_stops(v: f64) -> f64 {
    ((v / STOPS_STEP).round() * STOPS_STEP).clamp(-STOP_LIMIT, STOP_LIMIT)
}

impl Brush {
    pub fn stops(&self) -> f64 {
        match self.tool {
            Tool::Hand => self.hand.stops,
            Tool::Card => self.card.stops,
        }
    }

    pub fn set_stops(&mut self, stops: f64) {
        let stops = round_stops(stops);
        match self.tool {
            Tool::Hand => self.hand.stops = stops,
            Tool::Card => self.card.stops = stops,
        }
    }

    pub fn light(&self) -> Light {
        match self.tool {
            Tool::Hand => self.hand.light,
            Tool::Card => self.card.light,
        }
    }

    pub fn set_light(&mut self, light: Light) {
        match self.tool {
            Tool::Hand => self.hand.light = light,
            Tool::Card => self.card.light = light,
        }
    }

    /// A brush read from a file, every number within its limits.
    pub fn sanitized(self) -> Self {
        let fin = |v: f64, d: f64| if v.is_finite() { v } else { d };
        let d = Brush::default();
        let axis = |v: f64, d: f64| fin(v, d).clamp(MIN_AXIS, SIZE_LIMIT);
        let feather = |v: f64, d: f64| fin(v, d).clamp(0.0, SIZE_LIMIT);
        Self {
            tool: self.tool,
            hand: HandBrush {
                tip: Tip {
                    rx: axis(self.hand.tip.rx, d.hand.tip.rx),
                    ry: axis(self.hand.tip.ry, d.hand.tip.ry),
                    angle: burn::wrap_degrees(fin(self.hand.tip.angle, 0.0)),
                    feather: feather(self.hand.tip.feather, d.hand.tip.feather),
                },
                stops: round_stops(fin(self.hand.stops, d.hand.stops)),
                light: self.hand.light,
            },
            card: CardBrush {
                angle: burn::wrap_degrees(fin(self.card.angle, d.card.angle)),
                feather: feather(self.card.feather, d.card.feather),
                stops: round_stops(fin(self.card.stops, d.card.stops)),
                light: self.card.light,
            },
        }
    }

    /// The hand's stroke along `points` (shown-normalised).
    pub fn hand_stroke(&self, points: Vec<[f64; 2]>) -> Stroke {
        Stroke::Hand { tip: self.hand.tip, stops: self.hand.stops, light: self.hand.light, points }
    }

    /// The card pushed from `from` to `to`: its normal along the push, its
    /// travel the distance. A push too short to have a direction is the
    /// card held still at `from`, lit as the brush is set.
    pub fn card_stroke(&self, from: (f64, f64), to: (f64, f64)) -> Stroke {
        let (dx, dy) = (to.0 - from.0, to.1 - from.1);
        let travel = dx.hypot(dy);
        let (angle, travel) = if travel < 1e-6 {
            (self.card.angle, 0.0)
        } else {
            (burn::wrap_degrees(dy.atan2(dx).to_degrees()), travel.min(SIZE_LIMIT))
        };
        Stroke::Card {
            x: clamp_reach(from.0),
            y: clamp_reach(from.1),
            angle,
            travel,
            feather: self.card.feather,
            stops: self.card.stops,
            light: self.card.light,
        }
    }

    /// The space between two kept points of a painted path: a quarter of
    /// the hand's smaller half-axis, so the path is smooth under the tip
    /// and stays short.
    pub fn spacing(&self) -> f64 {
        0.25 * self.hand.tip.rx.min(self.hand.tip.ry)
    }
}

/// A handle on the brush shown on the stage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Hit {
    /// The hand's half-axis ends: 0 and 2 the x axis, 1 and 3 the y.
    Axis(u8),
    /// The turn: the hand's past its x axis; the card's at either end of
    /// its edge (0 along it, 1 against it).
    Rotate(u8),
    /// The penumbra's width: on the hand's outer ring along its y axis, on
    /// the card's shadow line on the dark side.
    Feather,
    /// The stops, written at the centre: a vertical drag sets them.
    Stops,
}

/// A handle takes a press within this many screen points.
pub const HANDLE_PT: f32 = 7.0;
/// The hand's turn handle sits this far past its x axis.
pub const ROTATE_PT: f32 = 22.0;
/// The card's turn handles sit this far from its point along the edge.
pub const EDGE_ROTATE_PT: f32 = 60.0;
/// The card's stops are written this far into the lit side.
pub const LABEL_PT: f32 = 18.0;
/// The penumbra handle sits this far past the penumbra's edge, so it is
/// held apart from the tip's own handles when the edge is hard.
pub const FEATHER_PT: f32 = 14.0;
/// Stops per screen point of a vertical drag on the label.
pub const STOPS_PER_PT: f64 = 0.0025;
/// The smallest half-axis the hand can be given.
pub const MIN_AXIS: f64 = 0.002;

pub fn clamp_reach(v: f64) -> f64 {
    v.clamp(-burn::REACH, burn::REACH)
}

/// The handles of the brush shown with its centre at normalised
/// `(cx, cy)`, `k` screen points per normalised unit: `(hit, x, y)`.
pub fn handles(b: &Brush, cx: f64, cy: f64, k: f64) -> Vec<(Hit, f64, f64)> {
    match b.tool {
        Tool::Hand => {
            let Tip { rx, ry, angle, feather } = b.hand.tip;
            let (s, c) = angle.to_radians().sin_cos();
            let turn = rx + f64::from(ROTATE_PT) / k;
            let fy = ry + feather + f64::from(FEATHER_PT) / k;
            vec![
                (Hit::Axis(0), cx + rx * c, cy + rx * s),
                (Hit::Axis(1), cx - ry * s, cy + ry * c),
                (Hit::Axis(2), cx - rx * c, cy - rx * s),
                (Hit::Axis(3), cx + ry * s, cy - ry * c),
                (Hit::Rotate(0), cx + turn * c, cy + turn * s),
                (Hit::Feather, cx - fy * s, cy + fy * c),
                (Hit::Stops, cx, cy),
            ]
        }
        Tool::Card => {
            let (s, c) = b.card.angle.to_radians().sin_cos();
            let (dx, dy) = (-s, c);
            let turn = f64::from(EDGE_ROTATE_PT) / k;
            let label = f64::from(LABEL_PT) / k;
            let f = b.card.feather + f64::from(FEATHER_PT) / k;
            vec![
                (Hit::Rotate(0), cx + turn * dx, cy + turn * dy),
                (Hit::Rotate(1), cx - turn * dx, cy - turn * dy),
                (Hit::Feather, cx - f * c, cy - f * s),
                (Hit::Stops, cx + label * c, cy + label * s),
            ]
        }
    }
}

/// The handle under a press at normalised `(x, y)`, within `slop`.
pub fn hit(b: &Brush, cx: f64, cy: f64, k: f64, x: f64, y: f64, slop: f64) -> Option<Hit> {
    handles(b, cx, cy, k).into_iter().find(|&(_, hx, hy)| (x - hx).hypot(y - hy) <= slop).map(|h| h.0)
}

/// The brush after a drag of `hit` from `start` to `to`, the brush shown
/// at `centre`. `fine` scales the stops' drag (a tenth with
/// Shift). A handle dragged to where it sits changes nothing.
pub fn dragged(
    origin: &Brush,
    hit: Hit,
    centre: (f64, f64),
    start: (f64, f64),
    to: (f64, f64),
    k: f64,
    fine: f64,
) -> Brush {
    let ((cx, cy), (x, y)) = (centre, to);
    let mut b = *origin;
    let (dx, dy) = (x - cx, y - cy);
    let axis = |v: f64| v.clamp(MIN_AXIS, SIZE_LIMIT);
    match (b.tool, hit) {
        (_, Hit::Stops) => {
            let moved = (y - start.1) * k * STOPS_PER_PT * fine;
            b.set_stops(origin.stops() - moved);
        }
        (Tool::Hand, Hit::Axis(i)) => {
            let (s, c) = b.hand.tip.angle.to_radians().sin_cos();
            if i % 2 == 0 {
                b.hand.tip.rx = axis((dx * c + dy * s).abs());
            } else {
                b.hand.tip.ry = axis((-dx * s + dy * c).abs());
            }
        }
        (Tool::Hand, Hit::Rotate(_)) => {
            if dx.hypot(dy) >= 1e-9 {
                b.hand.tip.angle = burn::wrap_degrees(dy.atan2(dx).to_degrees());
            }
        }
        (Tool::Hand, Hit::Feather) => {
            let (s, c) = b.hand.tip.angle.to_radians().sin_cos();
            let along = (-dx * s + dy * c).abs();
            b.hand.tip.feather = (along - b.hand.tip.ry - f64::from(FEATHER_PT) / k).clamp(0.0, SIZE_LIMIT);
        }
        (Tool::Card, Hit::Rotate(end)) => {
            if dx.hypot(dy) >= 1e-9 {
                // The handle lies along the edge, which is the normal turned
                // a quarter: at the far end a quarter the other way.
                let along = dy.atan2(dx).to_degrees();
                b.card.angle = burn::wrap_degrees(if end == 0 { along - 90.0 } else { along + 90.0 });
            }
        }
        (Tool::Card, Hit::Feather) => {
            let (s, c) = b.card.angle.to_radians().sin_cos();
            b.card.feather = (-(dx * c + dy * s) - f64::from(FEATHER_PT) / k).clamp(0.0, SIZE_LIMIT);
        }
        (Tool::Card, Hit::Axis(_)) => {}
    }
    b
}

/// A stage point to the shown picture's normalised coordinates. `to_norm`
/// takes the picture's unit square (0..1 across and down `rect`).
pub fn stage_to_norm(p: Pos2, rect: egui::Rect, to_norm: &mimizan_core::straighten::Affine) -> (f64, f64) {
    let u = f64::from((p.x - rect.left()) / rect.width());
    let v = f64::from((p.y - rect.top()) / rect.height());
    to_norm.apply(u, v)
}

pub fn norm_to_stage(x: f64, y: f64, rect: egui::Rect, back: &mimizan_core::straighten::Affine) -> Pos2 {
    let (u, v) = back.apply(x, y);
    Pos2::new(rect.left() + u as f32 * rect.width(), rect.top() + v as f32 * rect.height())
}

/// Screen points per normalised unit: the map is a turn and one scale, so
/// one direction measures it.
pub fn points_per_unit(rect: egui::Rect, back: &mimizan_core::straighten::Affine) -> f64 {
    let o = norm_to_stage(0.0, 0.0, rect, back);
    let e = norm_to_stage(1.0, 0.0, rect, back);
    f64::from(o.distance(e))
}

/// The outline of the tip at `(cx, cy)` grown by `off` on both half-axes,
/// through `at` onto the stage.
pub fn tip_outline(tip: &Tip, cx: f64, cy: f64, off: f64, at: &dyn Fn(f64, f64) -> Pos2) -> Vec<Pos2> {
    let (s, c) = tip.angle.to_radians().sin_cos();
    let (rx, ry) = ((tip.rx + off).max(0.0005), (tip.ry + off).max(0.0005));
    (0..72)
        .map(|i| {
            let t = i as f64 / 72.0 * std::f64::consts::TAU;
            let (px, py) = (rx * t.cos(), ry * t.sin());
            at(cx + px * c - py * s, cy + px * s + py * c)
        })
        .collect()
}

/// Burns in orange, dodges in blue.
pub fn ink(stops: f64) -> egui::Color32 {
    if stops > 0.0 {
        egui::Color32::from_rgb(232, 120, 24)
    } else if stops < 0.0 {
        egui::Color32::from_rgb(90, 170, 255)
    } else {
        egui::Color32::from_gray(200)
    }
}

/// The stops, and the light when it is one pass: `+0.25 soft`.
pub fn label(stops: f64, light: Light) -> String {
    let mut txt = format!("{stops:+.2}");
    if light != Light::Both {
        txt.push(' ');
        txt.push_str(light.name());
    }
    txt
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Rect, Vec2};
    use mimizan_core::straighten;

    fn hand(rx: f64, ry: f64, angle: f64, feather: f64) -> Brush {
        Brush {
            tool: Tool::Hand,
            hand: HandBrush { tip: Tip { rx, ry, angle, feather }, ..Brush::default().hand },
            ..Brush::default()
        }
    }

    fn card(angle: f64, feather: f64) -> Brush {
        Brush {
            tool: Tool::Card,
            card: CardBrush { angle, feather, ..Brush::default().card },
            ..Brush::default()
        }
    }

    /// Every handle dragged to where it sits leaves the brush as it was.
    #[test]
    fn handles_dragged_to_themselves_change_nothing() {
        let k = 400.0;
        let (cx, cy) = (0.1, -0.05);
        for b in [hand(0.3, 0.12, 37.0, 0.04), hand(0.1, 0.1, 0.0, 0.0), card(-70.0, 0.08), card(90.0, 0.0)] {
            for (h, hx, hy) in handles(&b, cx, cy, k) {
                let moved = dragged(&b, h, (cx, cy), (hx, hy), (hx, hy), k, 1.0);
                let near = |a: f64, b: f64| (a - b).abs() < 1e-6;
                assert!(
                    near(moved.hand.tip.rx, b.hand.tip.rx)
                        && near(moved.hand.tip.ry, b.hand.tip.ry)
                        && near(moved.hand.tip.angle, b.hand.tip.angle)
                        && near(moved.hand.tip.feather, b.hand.tip.feather)
                        && near(moved.card.angle, b.card.angle)
                        && near(moved.card.feather, b.card.feather)
                        && moved.stops() == b.stops(),
                    "{h:?} moved {b:?} to {moved:?}"
                );
                assert_eq!(hit(&b, cx, cy, k, hx, hy, 7.0 / k), Some(h));
            }
        }
        assert_eq!(hit(&hand(0.1, 0.1, 0.0, 0.0), cx, cy, k, cx + 1.0, cy, 7.0 / k), None);
    }

    /// The x handle dragged out along a turned axis sets rx alone; the
    /// feather handle sets the penumbra past the y half-axis; the turn
    /// handle the angle; the label the stops in hundredths, Shift finer.
    #[test]
    fn the_hand_is_shaped_by_its_handles() {
        let k = 400.0;
        let b = hand(0.3, 0.12, 30.0, 0.0);
        let (s, c) = 30f64.to_radians().sin_cos();
        let out = dragged(&b, Hit::Axis(0), (0.0, 0.0), (0.3 * c, 0.3 * s), (0.5 * c, 0.5 * s), k, 1.0);
        assert!((out.hand.tip.rx - 0.5).abs() < 1e-9 && out.hand.tip.ry == 0.12);
        let turned = dragged(&b, Hit::Rotate(0), (0.0, 0.0), (0.0, 0.0), (0.2, 0.2), k, 1.0);
        assert!((turned.hand.tip.angle - 45.0).abs() < 1e-9);
        // The y axis points along (−s, c); 0.2 out along it, less the
        // handle's own stand-off, is a feather of 0.08 − 14/400.
        let soft = dragged(&b, Hit::Feather, (0.0, 0.0), (0.0, 0.0), (-0.2 * s, 0.2 * c), k, 1.0);
        assert!((soft.hand.tip.feather - 0.045).abs() < 1e-9, "{}", soft.hand.tip.feather);
        // Inside the tip the feather is nothing, not negative.
        assert_eq!(
            dragged(&b, Hit::Feather, (0.0, 0.0), (0.0, 0.0), (0.0, 0.05), k, 1.0).hand.tip.feather,
            0.0
        );
        // 40 points up: a tenth of a stop more; with Shift a hundredth.
        let up = dragged(&b, Hit::Stops, (0.0, 0.0), (0.0, 0.0), (0.0, -40.0 / k), k, 1.0);
        assert!((up.stops() - 0.35).abs() < 1e-9, "{}", up.stops());
        let fine = dragged(&b, Hit::Stops, (0.0, 0.0), (0.0, 0.0), (0.0, -40.0 / k), k, 0.1);
        assert!((fine.stops() - 0.26).abs() < 1e-9, "{}", fine.stops());
        let far = dragged(&b, Hit::Stops, (0.0, 0.0), (0.0, 0.0), (0.0, 9000.0 / k), k, 1.0);
        assert_eq!(far.stops(), -STOP_LIMIT);
    }

    /// The card's turn handles set its normal a quarter from the edge; its
    /// feather handle lies on the dark side.
    #[test]
    fn the_card_is_turned_and_softened_by_its_handles() {
        let k = 400.0;
        let b = card(-90.0, 0.05);
        // Lit above: the edge runs along x. The handle along it dragged
        // to the upper right turns the edge to 45°, lit to the upper right.
        let t = dragged(&b, Hit::Rotate(0), (0.0, 0.0), (0.0, 0.0), (0.2, -0.2), k, 1.0);
        assert!(
            (t.card.angle + 135.0).abs() < 1e-9 || (t.card.angle - 225.0).abs() < 1e-9,
            "{}",
            t.card.angle
        );
        let f = dragged(&b, Hit::Feather, (0.0, 0.0), (0.0, 0.0), (0.0, 0.12), k, 1.0);
        assert!((f.card.feather - 0.085).abs() < 1e-9, "{}", f.card.feather);
        assert_eq!(dragged(&b, Hit::Feather, (0.0, 0.0), (0.0, 0.0), (0.0, -0.12), k, 1.0).card.feather, 0.0);
    }

    /// A push gives the card its normal and its travel; a click holds it
    /// still as the brush is set; the hand's stroke carries the tip.
    #[test]
    fn strokes_come_from_the_brush() {
        let b = card(-90.0, 0.1);
        let pushed = b.card_stroke((0.0, 0.2), (0.0, -0.1));
        assert!(
            matches!(
                pushed,
                Stroke::Card { x: 0.0, y: 0.2, angle: -90.0, travel, feather: 0.1, stops: 0.5, light: Light::Both }
                    if (travel - 0.3).abs() < 1e-12
            ),
            "{pushed:?}"
        );
        let right = b.card_stroke((0.0, 0.0), (0.3, 0.0));
        assert!(
            matches!(right, Stroke::Card { angle, travel, .. } if angle == 0.0 && (travel - 0.3).abs() < 1e-12)
        );
        let held = b.card_stroke((0.1, 0.1), (0.1, 0.1));
        assert!(matches!(held, Stroke::Card { angle: -90.0, travel: 0.0, .. }));
        let h = hand(0.1, 0.05, 10.0, 0.02);
        let s = h.hand_stroke(vec![[0.0, 0.0], [0.1, 0.0]]);
        assert_eq!(
            s,
            Stroke::Hand {
                tip: Tip { rx: 0.1, ry: 0.05, angle: 10.0, feather: 0.02 },
                stops: 0.25,
                light: Light::Both,
                points: vec![[0.0, 0.0], [0.1, 0.0]]
            }
        );
        assert!((h.spacing() - 0.0125).abs() < 1e-12);
        burn::validate(&[pushed, s]).unwrap();
    }

    #[test]
    fn a_brush_from_a_file_is_within_its_limits_and_round_trips() {
        let wild = Brush {
            tool: Tool::Card,
            hand: HandBrush {
                tip: Tip { rx: 0.0, ry: f64::NAN, angle: 400.0, feather: -1.0 },
                stops: 7.0,
                light: Light::Soft,
            },
            card: CardBrush { angle: f64::INFINITY, feather: 9.0, stops: 0.123, light: Light::Hard },
        };
        let s = wild.sanitized();
        assert_eq!(s.hand.tip.rx, MIN_AXIS);
        assert_eq!(s.hand.tip.ry, 0.08);
        assert_eq!(s.hand.tip.angle, 40.0);
        assert_eq!(s.hand.tip.feather, 0.0);
        assert_eq!(s.hand.stops, STOP_LIMIT);
        assert_eq!(s.card.angle, -90.0);
        assert_eq!(s.card.feather, SIZE_LIMIT);
        assert!((s.card.stops - 0.12).abs() < 1e-12);
        assert_eq!((s.tool, s.hand.light, s.card.light), (Tool::Card, Light::Soft, Light::Hard));
        let json = serde_json::to_string(&Brush::default()).unwrap();
        assert_eq!(serde_json::from_str::<Brush>(&json).unwrap(), Brush::default());
        assert_eq!(Brush::default().sanitized(), Brush::default());
        let mut b = Brush::default();
        b.set_stops(0.126);
        assert!((b.stops() - 0.13).abs() < 1e-12);
        b.set_light(Light::Hard);
        assert_eq!((b.light(), b.hand.light, b.card.light), (Light::Hard, Light::Hard, Light::Both));
        assert_eq!(label(-1.0, Light::Soft), "-1.00 soft");
        assert_eq!(label(0.25, Light::Both), "+0.25");
    }

    /// Stage ↔ normalised through the map and back lands where it started,
    /// and the scale is what the stage shows.
    #[test]
    fn the_stage_and_the_normalised_picture_agree() {
        let rect = Rect::from_min_size(Pos2::new(100.0, 50.0), Vec2::new(600.0, 400.0));
        // A 3000×2000 picture, unit 3000 px: the picture's unit square is
        // the whole frame.
        let unit_to_pic = straighten::Affine { a: 3000.0, b: 0.0, c: 0.0, d: 2000.0, tx: 0.0, ty: 0.0 };
        let to_norm = burn::normalize(3000, 2000).after(&unit_to_pic);
        let back = to_norm.inverse().unwrap();
        let centre = stage_to_norm(rect.center(), rect, &to_norm);
        assert!(centre.0.abs() < 1e-6 && centre.1.abs() < 1e-6);
        let p = Pos2::new(250.0, 120.0);
        let (x, y) = stage_to_norm(p, rect, &to_norm);
        let q = norm_to_stage(x, y, rect, &back);
        assert!(p.distance(q) < 1e-3);
        // One normalised unit is the picture's long side: 600 points.
        assert!((points_per_unit(rect, &back) - 600.0).abs() < 1e-3);
        let ring =
            tip_outline(&Tip { rx: 0.1, ry: 0.05, angle: 0.0, feather: 0.0 }, 0.0, 0.0, 0.0, &|x, y| {
                norm_to_stage(x, y, rect, &back)
            });
        assert_eq!(ring.len(), 72);
        assert!((ring[0].x - (rect.center().x + 60.0)).abs() < 1e-3);
    }
}
