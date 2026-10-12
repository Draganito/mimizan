//! The zone bar: the paper's split bar for a zone curve (`zone.rs`). Two
//! rows on one logarithmic axis, one stop per zone, Zone V in the middle:
//! the print on top (fixed output zones), the scene below (the negative's
//! zones). A line connects each scene zone to the print tone it gets; drag
//! the handle at the top of a line to move that zone's output. Fog and
//! paper white are the handles at the two ends. The curve stays monotone
//! because every move goes through `zone::set_output`.

use egui::{pos2, vec2, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Ui};
use mimizan_core::calib::LookFile;
use mimizan_core::zone;

const ROW_H: f32 = 26.0;
const GAP_H: f32 = 44.0;
/// Stops shown each side of Zone V: the bar covers zones 0 … X exactly.
const HALF_SPAN: f64 = 5.5;

/// Encoded tone of a linear value, as the preview grey.
fn tone(l: f64) -> Color32 {
    Color32::from_gray((zone::encode(l.clamp(0.0, 1.0)) * 255.0).round() as u8)
}

/// Where a linear value sits on the axis, in stops from Zone V, clamped to
/// the bar.
fn axis_stops(l: f64) -> f64 {
    zone::stops(l).clamp(-HALF_SPAN, HALF_SPAN)
}

/// Draws the bar and edits `look` in place. Returns the scene zone (0 … X)
/// under the pointer, for tinting the preview.
pub fn zone_bar(ui: &mut Ui, look: &mut LookFile, width: f32) -> Option<u8> {
    let height = 2.0 * ROW_H + GAP_H;
    let (rect, response) = ui.allocate_exact_size(vec2(width, height), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals().clone();
    let n = look.points.len();
    let inner = zone::inner_zones();
    debug_assert_eq!(n, inner.len() + 2);

    let x_of = |e: f64| -> f32 { rect.left() + ((e + HALF_SPAN) / (2.0 * HALF_SPAN)) as f32 * rect.width() };
    let e_of = |x: f32| -> f64 { ((x - rect.left()) / rect.width()) as f64 * 2.0 * HALF_SPAN - HALF_SPAN };
    let print_row = Rect::from_min_size(rect.min, vec2(rect.width(), ROW_H));
    let scene_row = Rect::from_min_size(pos2(rect.left(), rect.bottom() - ROW_H), vec2(rect.width(), ROW_H));
    // Handle y: just under the print row.
    let handle_y = print_row.bottom() + 7.0;

    // Where node i (0 = fog, n-1 = paper white, else a zone) lands on the
    // print axis, in stops.
    let node_stops = |look: &LookFile, i: usize| -> f64 { axis_stops(zone::decode(look.points[i][1])) };

    // Hit test: nearest handle within reach.
    let nearest = |pos: Pos2, look: &LookFile| -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for i in 0..n {
            let d = pos2(x_of(node_stops(look, i)), handle_y).distance(pos);
            if d < 11.0 && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    };

    let id = response.id;
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp::<usize>(id));
    if response.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).or_else(|| response.interact_pointer_pos());
        dragging = origin.and_then(|p| nearest(p, look));
        ui.data_mut(|d| match dragging {
            Some(i) => {
                d.insert_temp(id, i);
            }
            None => {
                d.remove_temp::<usize>(id);
            }
        });
    }
    if response.dragged() {
        if let (Some(i), Some(pos)) = (dragging, response.interact_pointer_pos()) {
            if i < n {
                let e = e_of(pos.x).clamp(-HALF_SPAN, HALF_SPAN);
                // The ends of the axis are 0 and 1 exactly, not 2^±5.5.
                let v = if e <= -HALF_SPAN {
                    0.0
                } else if e >= HALF_SPAN {
                    1.0
                } else {
                    zone::encode(zone::GRAY_CARD * 2f64.powf(e))
                };
                zone::set_output(look, i, v);
            }
        }
    }
    if response.drag_stopped() {
        ui.data_mut(|d| d.remove_temp::<usize>(id));
        dragging = None;
    }
    if response.double_clicked() {
        // Back to the identity for that node.
        if let Some(i) = response.interact_pointer_pos().and_then(|p| nearest(p, look)) {
            zone::set_output(look, i, look.points[i][0]);
        }
    }

    // Rows: eleven patches each, the print row's tones fixed, the scene
    // row's tones the zone centres (what the negative holds there).
    let text_on = |c: Color32| if c.r() > 128 { Color32::from_gray(20) } else { Color32::from_gray(235) };
    for k in 0..zone::ZONES {
        let e0 = k as f64 - 5.5;
        let (x0, x1) = (x_of(e0), x_of(e0 + 1.0));
        let c = tone(zone::linear(k));
        for row in [print_row, scene_row] {
            let r = Rect::from_min_max(pos2(x0, row.top()), pos2(x1, row.bottom()));
            painter.rect_filled(r, 0.0, c);
            painter.text(
                r.center(),
                Align2::CENTER_CENTER,
                zone::numeral(k),
                FontId::proportional(11.0),
                text_on(c),
            );
        }
    }
    let frame = Stroke::new(1.0, visuals.weak_text_color().linear_multiply(0.6));
    painter.rect_stroke(print_row, 0.0, frame, egui::StrokeKind::Inside);
    painter.rect_stroke(scene_row, 0.0, frame, egui::StrokeKind::Inside);

    // Hover: a scene patch, or the zone a handle belongs to.
    let hover_handle = response.hover_pos().and_then(|p| nearest(p, look));
    let hover_zone: Option<u8> = match hover_handle {
        Some(i) if i > 0 && i < n - 1 => Some(inner[i - 1] as u8),
        Some(0) => Some(0),
        Some(_) => Some((zone::ZONES - 1) as u8),
        None => response.hover_pos().and_then(|p| {
            if scene_row.contains(p) || print_row.contains(p) {
                let k = ((e_of(p.x) + HALF_SPAN).floor() as i64).clamp(0, zone::ZONES as i64 - 1);
                Some(k as u8)
            } else {
                None
            }
        }),
    };
    if let Some(k) = hover_zone {
        let e0 = k as f64 - 5.5;
        let r = Rect::from_min_max(pos2(x_of(e0), scene_row.top()), pos2(x_of(e0 + 1.0), scene_row.bottom()));
        painter.rect_stroke(
            r,
            0.0,
            Stroke::new(2.0, Color32::from_rgb(255, 170, 60)),
            egui::StrokeKind::Inside,
        );
    }

    // Connections and handles.
    let line = Stroke::new(1.5, visuals.text_color().linear_multiply(0.8));
    let moved = Stroke::new(1.5, Color32::from_rgb(255, 200, 80));
    for i in 0..n {
        let (scene_e, label) = if i == 0 {
            (-HALF_SPAN, "fog")
        } else if i == n - 1 {
            (HALF_SPAN, "paper")
        } else {
            (inner[i - 1] as f64 - 5.0, "")
        };
        let out_e = node_stops(look, i);
        let identity = look.points[i][1] == look.points[i][0];
        let from = pos2(x_of(scene_e), scene_row.top());
        let to = pos2(x_of(out_e), handle_y);
        painter.line_segment([from, to], if identity { line } else { moved });
        let active = dragging == Some(i) || hover_handle == Some(i);
        let r = if active { 6.5 } else { 5.0 };
        painter.circle(
            to,
            r,
            if active {
                Color32::from_rgb(255, 200, 80)
            } else if identity {
                visuals.text_color()
            } else {
                Color32::from_rgb(230, 170, 60)
            },
            Stroke::new(1.0, visuals.extreme_bg_color),
        );
        if !label.is_empty() {
            let align = if i == 0 { Align2::LEFT_TOP } else { Align2::RIGHT_TOP };
            let p = pos2(to.x, handle_y + 9.0);
            painter.text(p, align, label, FontId::proportional(10.0), visuals.weak_text_color());
        }
    }
    // Axis legend between the rows.
    painter.text(
        pos2(rect.left() + 2.0, print_row.bottom() + 1.0),
        Align2::LEFT_TOP,
        "print",
        FontId::proportional(9.5),
        visuals.weak_text_color(),
    );
    painter.text(
        pos2(rect.left() + 2.0, scene_row.top() - 1.0),
        Align2::LEFT_BOTTOM,
        "scene",
        FontId::proportional(9.5),
        visuals.weak_text_color(),
    );

    if let Some(i) = hover_handle {
        let v = look.points[i][1];
        let what = if i == 0 {
            "fog".to_string()
        } else if i == n - 1 {
            "paper white".to_string()
        } else {
            format!("Zone {}", zone::numeral(inner[i - 1]))
        };
        response.clone().on_hover_text(format!(
            "{what}: output {v:.3} (print zone {:.2})\ndrag: move · double-click: back to identity",
            zone::zone_of(zone::decode(v))
        ));
    }

    hover_zone
}
