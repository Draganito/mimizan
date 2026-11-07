//! The paper strip: the one control the paper has, as on the phone.
//!
//! The axis is the scene, ten stops of linear grey with the grey card in
//! the middle, one zone per stop; each slice is the paper's value for that
//! grey under the current pair of exposures. So the strip is the print of
//! a grey wedge, and the two exposures only move the greys along it. A
//! drag moves both exposures together, the whole print lighter or darker,
//! as one more or less second under the enlarger. The wheel over the strip
//! is the grade: hard light against soft, with the mean exposure fixed,
//! one twelfth of a stop per notch. A double click is the base print, and
//! a second one brings the pair back.
//!
//! Nothing here touches the paper's arithmetic: the pair the strip hands
//! back is the same two numbers the two sliders used to set, on the same
//! grid of twelfths, inside the same `STOP_LIMIT`.

use egui::{pos2, vec2, Color32, CursorIcon, Event, MouseWheelUnit, Rect, Sense, Shape, Stroke, Ui};
use mimizan_core::paper::{SplitGrade, STOP_LIMIT};
use mimizan_core::zone;

/// Height of the strip, points.
const STRIP_H: f32 = 36.0;
/// Stops of linear grey the strip spans; the grey card in the middle.
const ZONES: f64 = 10.0;
/// Slices the ramp is drawn in.
const SAMPLES: usize = 192;
/// Stops of exposure a drag across the whole strip moves, as on the phone.
const DRAG_STOPS: f64 = STOP_LIMIT;
/// Points of trackpad scroll that count as one notch of a wheel.
const WHEEL_POINTS: f32 = 50.0;
/// The darkroom's amber: the strip's frame off the base print.
pub const AMBER: Color32 = Color32::from_rgb(255, 170, 60);

/// Print value shown as the print grey.
fn grey(v: f64) -> Color32 {
    Color32::from_gray((v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Linear grey at `x` of the strip's width, the grey card at the middle.
fn linear_at(frac: f64) -> f64 {
    (zone::GRAY_CARD * (frac * ZONES - ZONES / 2.0).exp2()).clamp(0.0, 1.0)
}

/// Draws the strip for `split` and lets the pointer move it.
pub fn paper_strip(ui: &mut Ui, split: &mut SplitGrade, width: f32) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, STRIP_H), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let id = response.id;
    let off = split.soft.abs() >= 1.0 / 24.0 || split.hard.abs() >= 1.0 / 24.0;

    // A double click: the base print, or the pair the last one left.
    type Pair = (f64, f64);
    if response.double_clicked() {
        if off {
            ui.data_mut(|d| d.insert_temp::<Pair>(id.with("remembered"), (split.soft, split.hard)));
            split.soft = 0.0;
            split.hard = 0.0;
        } else if let Some((s, h)) = ui.data(|d| d.get_temp::<Pair>(id.with("remembered"))) {
            split.soft = s;
            split.hard = h;
        }
    }

    // The drag: both exposures by the pointer's way, from the pair as the
    // drag began, so the snap to twelfths never walks.
    if response.drag_started() {
        ui.data_mut(|d| d.insert_temp::<Pair>(id.with("origin"), (split.soft, split.hard)));
    }
    let origin: Option<Pair> = ui.data(|d| d.get_temp::<Pair>(id.with("origin")));
    if response.dragged() {
        if let (Some((s0, h0)), Some(press), Some(pos)) =
            (origin, ui.input(|i| i.pointer.press_origin()), response.interact_pointer_pos())
        {
            let delta = -f64::from(pos.x - press.x) / f64::from(rect.width().max(1.0)) * DRAG_STOPS;
            let (s, h) = shift_grade(s0, h0, delta, STOP_LIMIT);
            split.soft = s;
            split.hard = h;
        }
    }
    if response.drag_stopped() {
        ui.data_mut(|d| d.remove_temp::<Pair>(id.with("origin")));
    }

    // The wheel: the grade, a twelfth per notch, up is harder; a trackpad
    // adds up to the notch. The raw events, so no scroll area has a say.
    if response.hovered() {
        let lines: f32 = ui.input(|i| {
            i.raw
                .events
                .iter()
                .map(|e| match e {
                    Event::MouseWheel { unit, delta, .. } => {
                        let d = if delta.y != 0.0 { delta.y } else { delta.x };
                        match unit {
                            MouseWheelUnit::Line | MouseWheelUnit::Page => d,
                            MouseWheelUnit::Point => d / WHEEL_POINTS,
                        }
                    }
                    _ => 0.0,
                })
                .sum()
        });
        if lines != 0.0 {
            let acc = ui.data(|d| d.get_temp::<f32>(id.with("wheel"))).unwrap_or(0.0) + lines;
            let notches = acc.trunc();
            ui.data_mut(|d| d.insert_temp::<f32>(id.with("wheel"), acc - notches));
            if notches != 0.0 {
                let c = contrast(split.soft, split.hard) + f64::from(notches) / 12.0;
                let (s, h) = with_contrast(split.soft, split.hard, c, STOP_LIMIT);
                split.soft = s;
                split.hard = h;
            }
        }
    }

    if origin.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    } else if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
    }

    if ui.is_rect_visible(rect) {
        let curve = split.curve();
        // The ramp: each slice the paper's value for the linear grey of
        // its stop.
        let w = rect.width() / SAMPLES as f32;
        for i in 0..SAMPLES {
            let l = linear_at((i as f64 + 0.5) / SAMPLES as f64);
            let slice =
                Rect::from_min_size(pos2(rect.left() + i as f32 * w, rect.top()), vec2(w + 0.5, STRIP_H));
            painter.rect_filled(slice, 0.0, grey(curve.eval(l)));
        }
        // The zones, one stop apart, as ticks from the top edge.
        let tick = Color32::from_rgba_unmultiplied(128, 128, 128, 150);
        for z in 1..ZONES as usize {
            let x = rect.left() + rect.width() * z as f32 / ZONES as f32;
            painter.line_segment(
                [pos2(x, rect.top()), pos2(x, rect.top() + STRIP_H * 0.3)],
                Stroke::new(1.0, tick),
            );
        }
        // The mark: the grey card's place, never moving.
        let mx = rect.center().x;
        let ink = if off { AMBER } else { Color32::WHITE };
        painter.add(Shape::convex_polygon(
            vec![pos2(mx - 5.0, rect.bottom()), pos2(mx + 5.0, rect.bottom()), pos2(mx, rect.bottom() - 7.0)],
            ink,
            Stroke::NONE,
        ));
        let frame =
            if off { Stroke::new(2.0, AMBER) } else { Stroke::new(1.0, Color32::from_white_alpha(90)) };
        painter.rect_stroke(rect, 4.0, frame, egui::StrokeKind::Inside);
    }

    // The zone under the pointer, as the hover text: a zone is the stop
    // around its centre, so the card's stop is Zone V from −½ to +½.
    if let Some(p) = response.hover_pos().filter(|p| rect.contains(*p)) {
        let frac = f64::from((p.x - rect.left()) / rect.width().max(1.0));
        let stops = frac * ZONES - ZONES / 2.0;
        let k = ((stops.round() + 5.0).max(0.0) as usize).min(zone::ZONES - 1);
        let l = linear_at(frac);
        let curve = split.curve();
        let over = if zone::GRAY_CARD * stops.exp2() > 1.0 { ", over the clip" } else { "" };
        response.on_hover_text(format!(
            "Zone {}: prints at {:.3} (density {:.2}){over}\n\
             drag: both exposures · wheel: the grade · double click: the base print",
            zone::numeral(k),
            curve.eval(l),
            curve.density(l),
        ));
    }
}

// The grade arithmetic, the same on the phone (`GradeMath` on Android,
// `Prefs` on iOS): every pair lands on the twelfth grid inside ±`limit`.

fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
}

/// Twelfths of a stop, inside ±`limit`; a non-finite value snaps to zero.
pub fn snap_stops(v: f64, limit: f64) -> f64 {
    ((finite_or_zero(v) * 12.0).round() / 12.0).clamp(-limit, limit)
}

/// Add `delta` stops to both exposures, snapped to a twelfth, and stop at
/// `limit` so the gap stays.
pub fn shift_grade(soft: f64, hard: f64, delta: f64, limit: f64) -> (f64, f64) {
    let lo = -limit - soft.min(hard);
    let hi = limit - soft.max(hard);
    // Not `clamp`: a pair from a file may stand outside the limit, lo above hi.
    let d = ((finite_or_zero(delta) * 12.0).round() / 12.0).max(lo).min(hi);
    (soft + d, hard + d)
}

/// The grade as one number: hard light against soft, in stops. Zero is the
/// paper's grade 2.
pub fn contrast(soft: f64, hard: f64) -> f64 {
    hard - soft
}

/// The pair with `contrast` stops between hard and soft and the same
/// exposure (the mean of the two, what a drag on the strip moves). Done in
/// twenty-fourths so both land on the twelfth grid: when the mean sits
/// between two twelfths the pair goes a twenty-fourth lighter, once.
/// `contrast` is clamped so neither exposure leaves ±`limit`.
pub fn with_contrast(soft: f64, hard: f64, contrast: f64, limit: f64) -> (f64, f64) {
    let m24 = ((snap_stops(soft, limit) + snap_stops(hard, limit)) * 12.0).round() as i64;
    let lim24 = (limit * 24.0).round() as i64;
    let room = lim24 - m24.abs();
    let wanted = finite_or_zero(contrast).clamp(-2.0 * limit, 2.0 * limit);
    let c12 = ((wanted * 12.0).round() as i64).clamp(-room, room);
    let mut s24 = m24 - c12;
    if s24.rem_euclid(2) != 0 {
        s24 -= 1;
    }
    if s24 < -lim24 {
        s24 += 2;
    }
    (s24 as f64 / 24.0, (s24 + 2 * c12) as f64 / 24.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: f64 = 3.0;
    const EPS: f64 = 1e-9;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
    }

    #[test]
    fn snap_stops_rounds_to_twelfths_and_clamps() {
        assert!(close(snap_stops(0.09, LIMIT), 1.0 / 12.0));
        assert!(close(snap_stops(0.04, LIMIT), 0.0));
        assert!(close(snap_stops(7.0, LIMIT), 3.0));
        assert!(close(snap_stops(-3.4, LIMIT), -3.0));
        assert!(close(snap_stops(f64::NAN, LIMIT), 0.0));
        assert!(close(snap_stops(f64::INFINITY, LIMIT), 0.0));
    }

    #[test]
    fn shift_grade_moves_both_and_keeps_the_gap() {
        let (s, h) = shift_grade(0.5, -0.25, 1.0, LIMIT);
        assert!(close(s, 1.5) && close(h, 0.75));
    }

    #[test]
    fn shift_grade_stops_at_the_limit_so_the_gap_stays() {
        let (s, h) = shift_grade(1.0, 2.0, 2.5, LIMIT);
        assert!(close(s, 2.0) && close(h, 3.0));
        let (s, h) = shift_grade(-1.0, 1.0, -5.0, LIMIT);
        assert!(close(s, -3.0) && close(h, -1.0));
        let (s, h) = shift_grade(-3.0, 3.0, 1.0, LIMIT);
        assert!(close(s, -3.0) && close(h, 3.0));
    }

    #[test]
    fn shift_grade_snaps_the_delta_not_the_pair() {
        let (s, h) = shift_grade(0.0, 0.0, 0.3, LIMIT);
        assert!(close(s, 4.0 / 12.0) && close(h, 4.0 / 12.0));
        assert!(close(shift_grade(0.0, 0.0, f64::NAN, LIMIT).0, 0.0));
        // A pair off the grid keeps its offset: the delta is what snaps.
        let (s, h) = shift_grade(0.1, 0.1, 1.0, LIMIT);
        assert!(close(s, 1.1) && close(h, 1.1));
    }

    #[test]
    fn with_contrast_keeps_the_mean_and_sets_the_gap() {
        let (s, h) = with_contrast(-1.0, 0.5, 2.5, LIMIT);
        assert!(close(h - s, 2.5));
        assert!(close((s + h) / 2.0, -0.25));
        assert!(close(s, -1.5) && close(h, 1.0));
    }

    #[test]
    fn with_contrast_is_the_identity_on_every_pair_on_the_grid() {
        for s in -36..=36 {
            for h in -36..=36 {
                let (soft, hard) = (s as f64 / 12.0, h as f64 / 12.0);
                let (a, b) = with_contrast(soft, hard, hard - soft, LIMIT);
                assert!(close(a, soft) && close(b, hard), "soft {soft} hard {hard}");
            }
        }
    }

    #[test]
    fn with_contrast_lands_on_the_twelfth_grid_inside_the_limit() {
        for s in -36..=36 {
            for h in -36..=36 {
                for c in -72..=72 {
                    let (a, b) = with_contrast(s as f64 / 12.0, h as f64 / 12.0, c as f64 / 12.0, LIMIT);
                    assert!(close(a, (a * 12.0).round() / 12.0));
                    assert!(close(b, (b * 12.0).round() / 12.0));
                    assert!((-LIMIT - EPS..=LIMIT + EPS).contains(&a), "soft {a}");
                    assert!((-LIMIT - EPS..=LIMIT + EPS).contains(&b), "hard {b}");
                    let mean = (s + h) as f64 / 24.0;
                    assert!(((a + b) / 2.0 - mean).abs() <= 1.0 / 24.0 + EPS, "mean");
                }
            }
        }
    }

    #[test]
    fn with_contrast_clamps_so_neither_exposure_leaves_the_limit() {
        let (s, h) = with_contrast(2.0, 2.0, 5.0, LIMIT);
        assert!(close(s, 1.0) && close(h, 3.0));
        let (s, h) = with_contrast(2.0, 2.0, -5.0, LIMIT);
        assert!(close(s, 3.0) && close(h, 1.0));
        let (s, h) = with_contrast(0.0, 1.0 / 12.0, 2.0 / 12.0, LIMIT);
        assert!(close(h - s, 2.0 / 12.0));
        assert!(close(s, -1.0 / 12.0) && close(h, 1.0 / 12.0));
        assert!(close(with_contrast(0.0, 0.0, f64::NAN, LIMIT).1, 0.0));
    }

    #[test]
    fn the_zone_under_the_pointer_is_the_stop_round_its_centre() {
        let zone_at = |frac: f64| {
            let stops = frac * ZONES - ZONES / 2.0;
            ((stops.round() + 5.0).max(0.0) as usize).min(zone::ZONES - 1)
        };
        assert_eq!(zone_at(0.5), 5);
        assert_eq!(zone_at(0.46), 5);
        assert_eq!(zone_at(0.54), 5);
        assert_eq!(zone_at(0.44), 4);
        assert_eq!(zone_at(0.0), 0);
        assert_eq!(zone_at(1.0), 10);
    }

    #[test]
    fn the_strip_spans_ten_stops_round_the_grey_card() {
        assert!(close(linear_at(0.5), zone::GRAY_CARD));
        assert!(close(linear_at(0.6), zone::GRAY_CARD * 2.0));
        assert!(close(linear_at(0.0), zone::GRAY_CARD / 32.0));
        assert!(close(linear_at(1.0), 1.0));
    }
}
