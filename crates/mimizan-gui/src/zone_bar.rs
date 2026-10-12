//! The zone bar: the paper from fog to paper white, split into the zones
//! 0 … VII. The axis is the encoded print value (gamma 2.2, close to
//! perceptual lightness), so the width of a patch is the share of the
//! paper's tonal range that zone gets, and the patch is painted with the
//! print tones it covers. The boundary between two zones is the handle:
//! drag it and the zone on one side widens while the other narrows; no
//! other zone changes. Every move goes through `zone::set_output`, so the
//! curve stays monotone and the boundaries cannot overtake each other. Fog
//! and paper white are the ends; they are set with the fields next to the
//! bar, and the paper outside them is shown as unreachable. Double-click a
//! boundary to put it back where it came from.

use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Mesh, Pos2, Rect, Sense, Shape, Stroke, Ui};
use mimizan_core::calib::LookFile;
use mimizan_core::zone;

/// Height of the bar, points.
pub const BAR_H: f32 = 44.0;
/// Identity ticks and the numeral row under the bar.
const BELOW_H: f32 = 12.0;
/// Encoded value shown as the print grey.
fn grey(v: f64) -> Color32 {
    Color32::from_gray((v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Draws the bar and edits `look` in place. Returns the zone (0 … VII)
/// under the pointer, for tinting the preview.
pub fn zone_bar(ui: &mut Ui, look: &mut LookFile, width: f32) -> Option<u8> {
    let (rect, response) = ui.allocate_exact_size(vec2(width, BAR_H + BELOW_H), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals().clone();
    let n = look.points.len();
    let zones = zone::bar_zones();
    debug_assert_eq!(n, zones + 1);
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), BAR_H));

    // Encoded print value <-> x.
    let x_of = |v: f64| -> f32 { bar.left() + v.clamp(0.0, 1.0) as f32 * bar.width() };
    let v_of = |x: f32| -> f64 { (((x - bar.left()) / bar.width()) as f64).clamp(0.0, 1.0) };
    let node_x = |look: &LookFile, i: usize| -> f32 { x_of(look.points[i][1]) };

    // Hit test: the nearest inner boundary within reach.
    let nearest = |pos: Pos2, look: &LookFile| -> Option<usize> {
        if !bar.expand2(vec2(0.0, 6.0)).contains(pos) {
            return None;
        }
        let mut best: Option<(usize, f32)> = None;
        for i in 1..n - 1 {
            let d = (node_x(look, i) - pos.x).abs();
            if d < 8.0 && best.is_none_or(|(_, bd)| d < bd) {
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
            // Past a neighbour the clamp keeps the gap.
            zone::set_output(look, i, v_of(pos.x));
        }
    }
    if response.drag_stopped() {
        ui.data_mut(|d| d.remove_temp::<usize>(id));
        dragging = None;
    }
    if response.double_clicked() {
        if let Some(i) = response.interact_pointer_pos().and_then(|p| nearest(p, look)) {
            zone::set_output(look, i, look.points[i][0]);
        }
    }

    let hover_handle = response.hover_pos().and_then(|p| nearest(p, look));
    if hover_handle.is_some() || dragging.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::ResizeHorizontal);
    }
    // The zone under the pointer: the patch, or the zone left of a handle.
    let hover_zone: Option<u8> = match (dragging.or(hover_handle), response.hover_pos()) {
        (Some(i), _) => Some((i - 1) as u8),
        (None, Some(p)) if bar.contains(p) => {
            let mut k = zones - 1;
            for i in 1..n - 1 {
                if p.x < node_x(look, i) {
                    k = i - 1;
                    break;
                }
            }
            Some(k as u8)
        }
        _ => None,
    };

    // Paper outside fog and paper white: unreachable, hatched.
    let hatch = Stroke::new(1.0, Color32::from_gray(70));
    for r in [
        Rect::from_min_max(bar.left_top(), pos2(node_x(look, 0), bar.bottom())),
        Rect::from_min_max(pos2(node_x(look, n - 1), bar.top()), bar.right_bottom()),
    ] {
        if r.width() > 0.5 {
            painter.rect_filled(r, 0.0, Color32::from_gray(28));
            let mut x = r.left() - BAR_H;
            while x < r.right() {
                let a = pos2(x.max(r.left()), bar.bottom() - (x.max(r.left()) - x));
                let b =
                    pos2((x + BAR_H).min(r.right()), bar.top() + (x + BAR_H - (x + BAR_H).min(r.right())));
                painter.line_segment([a, b], hatch);
                x += 8.0;
            }
        }
    }
    // Patches: zone k runs from node k to node k+1, painted as the print
    // tones between those two outputs.
    let text_on = |c: Color32| if c.r() > 128 { Color32::from_gray(20) } else { Color32::from_gray(235) };
    for k in 0..zones {
        let (x0, x1) = (node_x(look, k), node_x(look, k + 1));
        let (v0, v1) = (look.points[k][1], look.points[k + 1][1]);
        let r = Rect::from_min_max(pos2(x0, bar.top()), pos2(x1, bar.bottom()));
        let mut mesh = Mesh::default();
        let (c0, c1) = (grey(v0), grey(v1));
        mesh.colored_vertex(r.left_top(), c0);
        mesh.colored_vertex(r.right_top(), c1);
        mesh.colored_vertex(r.right_bottom(), c1);
        mesh.colored_vertex(r.left_bottom(), c0);
        mesh.add_triangle(0, 1, 2);
        mesh.add_triangle(0, 2, 3);
        painter.add(Shape::mesh(mesh));
        if r.width() > 14.0 {
            let mid = grey(0.5 * (v0 + v1));
            painter.text(
                r.center(),
                Align2::CENTER_CENTER,
                zone::numeral(k),
                FontId::proportional(12.0),
                text_on(mid),
            );
        }
    }
    let frame = Stroke::new(1.0, visuals.weak_text_color().linear_multiply(0.6));
    painter.rect_stroke(bar, 0.0, frame, egui::StrokeKind::Inside);

    // Hovered zone: an outline.
    if let Some(k) = hover_zone {
        let k = k as usize;
        let r = Rect::from_min_max(pos2(node_x(look, k), bar.top()), pos2(node_x(look, k + 1), bar.bottom()));
        painter.rect_stroke(
            r,
            0.0,
            Stroke::new(2.0, Color32::from_rgb(255, 170, 60)),
            egui::StrokeKind::Inside,
        );
    }

    // Boundaries: a line through the bar with a grip; moved ones in the
    // film's orange. Under the bar a tick marks where each one started.
    let tick_y = bar.bottom() + 1.0;
    for i in 1..n - 1 {
        let x = node_x(look, i);
        let identity = look.points[i][1] == look.points[i][0];
        let active = dragging == Some(i) || hover_handle == Some(i);
        let colour = if active {
            Color32::from_rgb(255, 200, 80)
        } else if identity {
            visuals.text_color()
        } else {
            Color32::from_rgb(230, 170, 60)
        };
        painter.line_segment(
            [pos2(x, bar.top()), pos2(x, bar.bottom())],
            Stroke::new(if active { 2.5 } else { 1.5 }, colour),
        );
        // Grip: a short bar in the middle, dark edge so it reads on white.
        let grip =
            Rect::from_center_size(pos2(x, bar.center().y), vec2(if active { 7.0 } else { 5.0 }, 14.0));
        painter.rect_filled(grip, 2.0, colour);
        painter.rect_stroke(
            grip,
            2.0,
            Stroke::new(1.0, Color32::from_black_alpha(160)),
            egui::StrokeKind::Outside,
        );
        // Identity tick.
        let x0 = x_of(look.points[i][0]);
        painter.line_segment(
            [pos2(x0, tick_y), pos2(x0, tick_y + 5.0)],
            Stroke::new(
                1.0,
                if identity { visuals.weak_text_color() } else { Color32::from_rgb(230, 170, 60) },
            ),
        );
        if !identity {
            painter.line_segment(
                [pos2(x0, tick_y + 3.0), pos2(x, tick_y + 3.0)],
                Stroke::new(1.0, Color32::from_rgb(230, 170, 60).linear_multiply(0.7)),
            );
        }
    }
    painter.text(
        pos2(node_x(look, 0), tick_y + 1.0),
        Align2::LEFT_TOP,
        "fog",
        FontId::proportional(9.0),
        visuals.weak_text_color(),
    );
    painter.text(
        pos2(node_x(look, n - 1), tick_y + 1.0),
        Align2::RIGHT_TOP,
        "paper white",
        FontId::proportional(9.0),
        visuals.weak_text_color(),
    );

    if let Some(i) = hover_handle.or(dragging) {
        let v = look.points[i][1];
        let e = zone::stops(zone::decode(v));
        response.clone().on_hover_text(format!(
            "Boundary {} | {}: prints at {v:.3} ({e:+.2} stops from the grey card, {:+.3} from untouched)\n\
             drag: widen one zone, narrow the other · double-click: back",
            zone::numeral(i - 1),
            zone::numeral(i),
            v - look.points[i][0],
        ));
    }

    hover_zone
}
