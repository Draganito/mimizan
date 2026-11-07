//! The lamp bars: the projector's red and blue lamp, each a bar the width
//! of the paper and about half the strip's height. The bar is the grey
//! card under that lamp, from the lamp turned down on the left (the card
//! towards cyan, or towards yellow) through neutral in the middle to the
//! lamp turned up on the right; the mark at the middle is zero, the
//! handle stands on the stops the lamp is at, and the tone under the
//! handle is the tone the print gets. A click or a drag puts the handle
//! there; the wheel is a twelfth a notch; a double click is zero, and a
//! second one brings the lamp back. The frame is amber off zero, as the
//! paper strip's is. No words.
//!
//! The value is the same `lamps[k]` the two sliders set, on the twelfth
//! grid inside ±`LAMP_LIMIT`; nothing behind the bar changes.

use crate::paper_strip::AMBER;
use egui::{
    pos2, vec2, Align2, Color32, CursorIcon, Event, FontId, MouseWheelUnit, Rect, Sense, Shape, Stroke, Ui,
};
use mimizan_core::print::LAMP_LIMIT;
use mimizan_core::zone;

/// Height of a bar, points.
const BAR_H: f32 = 20.0;
/// Twelfth of a stop, the step of the wheel and the arrow keys.
const STEP: f64 = 1.0 / 12.0;
/// Slices the wedge is drawn in.
const SAMPLES: usize = 96;
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

    /// The grey card under this lamp at `stops`, encoded with the paper's
    /// gamma: the one channel moved, the other two the card.
    fn card(self, stops: f64) -> Color32 {
        let g = zone::GRAY_CARD;
        let lit = (g * stops.exp2()).min(1.0);
        let enc = |l: f64| (l.clamp(0.0, 1.0).powf(1.0 / zone::GAMMA) * 255.0).round() as u8;
        match self {
            Lamp::Red => Color32::from_rgb(enc(lit), enc(g), enc(g)),
            Lamp::Blue => Color32::from_rgb(enc(g), enc(g), enc(lit)),
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

/// Draws the bar for `lamp` at `v` stops and lets the pointer move it.
pub fn lamp_bar(ui: &mut Ui, lamp: Lamp, v: &mut f64, width: f32) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, BAR_H), Sense::click_and_drag());
    let id = response.id;
    let off = v.abs() >= STEP / 2.0;

    // A double click: zero, or the value the last one left.
    if response.double_clicked() {
        if off {
            ui.data_mut(|d| d.insert_temp::<f64>(id.with("remembered"), *v));
            *v = 0.0;
        } else if let Some(r) = ui.data(|d| d.get_temp::<f64>(id.with("remembered"))) {
            *v = r;
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
        let off = v.abs() >= STEP / 2.0;
        // The wedge: the card under the lamp, from turned down to turned up.
        let w = rect.width() / SAMPLES as f32;
        for i in 0..SAMPLES {
            let stops = stops_at((i as f64 + 0.5) / SAMPLES as f64);
            let slice =
                Rect::from_min_size(pos2(rect.left() + i as f32 * w, rect.top()), vec2(w + 0.5, BAR_H));
            painter.rect_filled(slice, 0.0, lamp.card(stops));
        }
        // The lamp's letter, at the left edge, on the dark end of the wedge.
        painter.text(
            pos2(rect.left() + 7.0, rect.center().y),
            Align2::CENTER_CENTER,
            lamp.mark(),
            FontId::proportional(11.0),
            Color32::from_white_alpha(200),
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

    #[test]
    fn the_card_is_neutral_at_zero_and_moves_one_channel() {
        let r0 = Lamp::Red.card(0.0);
        assert_eq!((r0.r(), r0.g(), r0.b()), (r0.g(), r0.g(), r0.g()));
        let up = Lamp::Red.card(1.0);
        assert!(up.r() > up.g() && up.g() == r0.g() && up.b() == r0.b());
        let down = Lamp::Blue.card(-1.0);
        assert!(down.b() < down.g() && down.r() == r0.r());
    }
}
