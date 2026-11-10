//! The lamp bars: the projector's red and blue lamp, each a bar the width
//! of the paper and about half the strip's height. The bar is dark and
//! says nothing: the mark at the middle is zero, the handle stands on
//! the stops the lamp is at, the letter at the left edge, in the lamp's
//! colour, says which lamp. What the lamp does to the colour is in the
//! picture. A click or a drag puts the handle there; the wheel is a
//! twelfth a notch; a double click is zero, and a second one brings the
//! lamp back. The frame is amber off zero, as the paper strip's is.
//!
//! The value is the same `lamps[k]` the two sliders set, on the twelfth
//! grid inside ±`LAMP_LIMIT`; nothing behind the bar changes.

use crate::paper_strip::AMBER;
use egui::{
    pos2, vec2, Align2, Color32, CursorIcon, Event, FontId, MouseWheelUnit, Sense, Shape, Stroke, Ui,
};
use mimizan_core::print::LAMP_LIMIT;

/// Height of a bar, points.
const BAR_H: f32 = 20.0;
/// Twelfth of a stop, the step of the wheel and the arrow keys.
const STEP: f64 = 1.0 / 12.0;
/// Points of trackpad scroll that count as one notch of a wheel.
const WHEEL_POINTS: f32 = 50.0;

/// Which lamp the bar is.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Lamp {
    Red,
    Blue,
}

impl Lamp {
    fn mark(self) -> &'static str {
        match self {
            Lamp::Red => "R",
            Lamp::Blue => "B",
        }
    }

    /// The lamp's colour, for its letter: the glasses' red and blue.
    fn ink(self) -> Color32 {
        match self {
            Lamp::Red => Color32::from_rgb(255, 82, 66),
            Lamp::Blue => Color32::from_rgb(102, 148, 255),
        }
    }
}

/// Stops at `frac` of the bar's width: −limit at the left edge, +limit at the right.
fn stops_at(frac: f64) -> f64 {
    (frac * 2.0 - 1.0) * LAMP_LIMIT
}

/// `x` of `stops` on a bar from `left` over `width`.
fn x_of(left: f32, width: f32, stops: f64) -> f32 {
    left + ((stops / LAMP_LIMIT + 1.0) / 2.0).clamp(0.0, 1.0) as f32 * width
}

/// Stops on the twelfth grid inside the lamp's reach; a non-finite value is zero.
fn snap(v: f64) -> f64 {
    let v = if v.is_finite() { v } else { 0.0 };
    ((v * 12.0).round() / 12.0).clamp(-LAMP_LIMIT, LAMP_LIMIT)
}

/// The lamp is off zero by at least half a step.
fn is_on(v: f64) -> bool {
    v.abs() >= STEP / 2.0
}

/// What a double click does to a lamp that stood at `rest` before its
/// first press: `(the lamp now, what is remembered)`. A lamp that was on
/// goes to zero and is remembered; one at zero comes back to what was
/// remembered, or stays where it is when nothing was.
fn double_click(rest: f64, remembered: Option<f64>) -> (f64, Option<f64>) {
    if is_on(rest) {
        (0.0, Some(rest))
    } else {
        (remembered.unwrap_or(0.0), remembered)
    }
}

/// Draws the bar for `lamp` at `v` stops and lets the pointer move it.
pub fn lamp_bar(ui: &mut Ui, lamp: Lamp, v: &mut f64, width: f32) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, BAR_H), Sense::click_and_drag());
    let id = response.id;
    let (rest_id, released_id, remembered_id) = (id.with("rest"), id.with("released"), id.with("remembered"));

    // Where the lamp stood before the pointer came down. A press puts the
    // handle under the pointer, so by the time a double click is known the
    // lamp is already there twice over: the value to remember is the one
    // from before the first press. A press that follows a release within
    // the double click's time is its second one and leaves that alone.
    let now = ui.input(|i| i.time);
    if response.is_pointer_button_down_on() && ui.input(|i| i.pointer.primary_pressed()) {
        let within = ui.ctx().options(|o| o.input_options.max_double_click_delay);
        let second = ui.data(|d| d.get_temp::<f64>(released_id)).is_some_and(|t| now - t <= within);
        if !second {
            ui.data_mut(|d| d.insert_temp::<f64>(rest_id, *v));
        }
    }
    if response.clicked() || response.drag_stopped() {
        ui.data_mut(|d| d.insert_temp::<f64>(released_id, now));
    }

    // A double click: zero, or the value the last one left.
    if response.double_clicked() {
        let rest = ui.data(|d| d.get_temp::<f64>(rest_id)).unwrap_or(*v);
        let (to, remembered) = double_click(rest, ui.data(|d| d.get_temp::<f64>(remembered_id)));
        *v = snap(to);
        if let Some(r) = remembered {
            ui.data_mut(|d| d.insert_temp::<f64>(remembered_id, r));
        }
    } else if let Some(p) = response.interact_pointer_pos() {
        // A click or a drag: the handle where the pointer is.
        *v = snap(stops_at(f64::from((p.x - rect.left()) / rect.width().max(1.0))));
    }

    // The wheel: a twelfth a notch, up is more.
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
                *v = snap(*v + f64::from(notches) * STEP);
            }
        }
    }

    if response.dragged() {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    } else if response.hovered() {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let off = is_on(*v);
        painter.rect_filled(rect, 3.0, ui.visuals().widgets.inactive.bg_fill);
        // The lamp's letter, at the left edge, in the lamp's colour.
        painter.text(
            pos2(rect.left() + 8.0, rect.center().y),
            Align2::CENTER_CENTER,
            lamp.mark(),
            FontId::proportional(11.0),
            lamp.ink(),
        );
        // Zero: the mark at the middle, never moving.
        let mx = rect.center().x;
        painter.add(Shape::convex_polygon(
            vec![pos2(mx - 4.0, rect.bottom()), pos2(mx + 4.0, rect.bottom()), pos2(mx, rect.bottom() - 5.0)],
            Color32::WHITE,
            Stroke::NONE,
        ));
        // The handle: where the lamp stands; the tone under it is the print's.
        let hx = x_of(rect.left(), rect.width(), *v);
        let ink = if off { AMBER } else { Color32::WHITE };
        painter.line_segment([pos2(hx, rect.top()), pos2(hx, rect.bottom())], Stroke::new(2.0, ink));
        let frame =
            if off { Stroke::new(2.0, AMBER) } else { Stroke::new(1.0, Color32::from_white_alpha(90)) };
        painter.rect_stroke(rect, 3.0, frame, egui::StrokeKind::Inside);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_bar_runs_from_minus_to_plus_the_lamp_limit() {
        assert!((stops_at(0.0) + LAMP_LIMIT).abs() < 1e-12);
        assert!(stops_at(0.5).abs() < 1e-12);
        assert!((stops_at(1.0) - LAMP_LIMIT).abs() < 1e-12);
        assert!((x_of(10.0, 200.0, 0.0) - 110.0).abs() < 1e-4);
        assert!((x_of(10.0, 200.0, LAMP_LIMIT) - 210.0).abs() < 1e-4);
        assert!((x_of(10.0, 200.0, -9.0) - 10.0).abs() < 1e-4);
    }

    /// The value a double click works on is the lamp before the first
    /// press, not the place the press put the handle.
    #[test]
    fn a_double_click_is_zero_and_the_next_brings_the_lamp_back() {
        // On: zero, and the lamp is remembered.
        assert_eq!(double_click(0.75, None), (0.0, Some(0.75)));
        assert_eq!(double_click(-0.25, Some(0.75)), (0.0, Some(-0.25)));
        // At zero: back to what was remembered, which stays remembered.
        assert_eq!(double_click(0.0, Some(0.75)), (0.75, Some(0.75)));
        // Less than half a step is zero.
        assert_eq!(double_click(STEP / 4.0, Some(-0.5)), (-0.5, Some(-0.5)));
        // Nothing remembered: the lamp stays at zero.
        assert_eq!(double_click(0.0, None), (0.0, None));
    }

    #[test]
    fn the_handle_snaps_to_twelfths_inside_the_limit() {
        assert!((snap(0.09) - 1.0 / 12.0).abs() < 1e-12);
        assert!((snap(7.0) - LAMP_LIMIT).abs() < 1e-12);
        assert!((snap(-7.0) + LAMP_LIMIT).abs() < 1e-12);
        assert_eq!(snap(f64::NAN), 0.0);
        // Every twelfth the bar can show comes back as itself.
        let n = (LAMP_LIMIT * 12.0).round() as i32;
        for k in -n..=n {
            let s = f64::from(k) / 12.0;
            assert!((snap(stops_at((s / LAMP_LIMIT + 1.0) / 2.0)) - s).abs() < 1e-9, "{s}");
        }
    }
}
