//! The zone bar: where the zones of the negative land on the paper.
//!
//! The axis is the print value (gamma 2.2, close to perceptual lightness),
//! so the bar is the paper itself from its black to its white, and the
//! width of a patch is the share of the paper that zone gets under the
//! current pair of exposures. Nothing on the bar is a handle for one zone:
//! the shape of the scale is the paper's, and only the two exposures move
//! it. Dragging anywhere on the bar changes both exposures together so that
//! the tone under the pointer follows the pointer — the whole print lighter
//! or darker, as one more or less second under the enlarger. The soft and
//! hard exposures themselves are the sliders next to the bar.

use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Mesh, Rect, Sense, Shape, Stroke, Ui};
use mimizan_core::paper::{SplitGrade, STOP_LIMIT};
use mimizan_core::zone;

/// Height of the bar, points.
pub const BAR_H: f32 = 44.0;
/// The label row under the bar.
const BELOW_H: f32 = 12.0;

/// Print value shown as the print grey.
fn grey(v: f64) -> Color32 {
    Color32::from_gray((v.clamp(0.0, 1.0) * 255.0).round() as u8)
}

/// Draws the bar for `split` and lets a drag move both exposures.
pub fn zone_bar(ui: &mut Ui, split: &mut SplitGrade, width: f32) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, BAR_H + BELOW_H), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals().clone();
    let bar = Rect::from_min_size(rect.min, vec2(rect.width(), BAR_H));
    let zones = zone::bar_zones();

    // Print value <-> x.
    let x_of = |v: f64| -> f32 { bar.left() + v.clamp(0.0, 1.0) as f32 * bar.width() };
    let v_of = |x: f32| -> f64 { (((x - bar.left()) / bar.width()) as f64).clamp(0.0, 1.0) };

    // The drag: the tone that was under the pointer when the drag began
    // follows the pointer, through the pair of exposures as of that moment.
    let id = response.id;
    type Start = (f64, f64, f64);
    if response.drag_started() {
        let origin = ui.input(|i| i.pointer.press_origin()).or_else(|| response.interact_pointer_pos());
        if let Some(p) = origin.filter(|p| bar.expand2(vec2(0.0, 4.0)).contains(*p)) {
            let l0 = split.curve().linear_for(v_of(p.x));
            ui.data_mut(|d| d.insert_temp::<Start>(id, (split.soft, split.hard, l0)));
        }
    }
    let start: Option<Start> = ui.data(|d| d.get_temp::<Start>(id));
    if response.dragged() {
        if let (Some((s0, h0, l0)), Some(pos)) = (start, response.interact_pointer_pos()) {
            let at_start = SplitGrade::new(s0, h0).curve();
            let mut s = at_start.shift_for(l0, v_of(pos.x));
            // Both move by the same stops, so the clamp is one for the pair.
            s = s.clamp(-STOP_LIMIT - s0.min(h0), STOP_LIMIT - s0.max(h0));
            split.soft = s0 + s;
            split.hard = h0 + s;
        }
    }
    if response.drag_stopped() {
        ui.data_mut(|d| d.remove_temp::<Start>(id));
    }
    if start.is_some() {
        ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
    } else if response.hovered() && response.hover_pos().is_some_and(|p| bar.contains(p)) {
        ui.ctx().set_cursor_icon(CursorIcon::Grab);
    }

    // Where the zones land now.
    let curve = split.curve();
    let mut edges: Vec<f64> = Vec::with_capacity(zones + 1);
    edges.push(curve.eval(0.0));
    for k in zone::boundaries() {
        edges.push(curve.eval(zone::boundary_linear(k)));
    }
    let clip = curve.eval(1.0);
    edges.push(clip);
    let edge_x = |i: usize| x_of(edges[i]);

    // The zone under the pointer.
    let hover_zone: Option<u8> = response.hover_pos().filter(|p| bar.contains(*p)).map(|p| {
        let v = v_of(p.x);
        edges[1..zones].iter().position(|&e| v < e).unwrap_or(zones - 1) as u8
    });

    // The paper: a ramp from its black to its white (the axis is the tone,
    // so the ramp is linear whatever the exposures); the part above the
    // clip's tone is paper the negative never reaches, hatched.
    let mut mesh = Mesh::default();
    let (c0, c1) = (grey(0.0), grey(1.0));
    mesh.colored_vertex(bar.left_top(), c0);
    mesh.colored_vertex(bar.right_top(), c1);
    mesh.colored_vertex(bar.right_bottom(), c1);
    mesh.colored_vertex(bar.left_bottom(), c0);
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(Shape::mesh(mesh));
    let beyond = Rect::from_min_max(pos2(x_of(clip), bar.top()), bar.right_bottom());
    if beyond.width() > 0.5 {
        let hatch = Stroke::new(1.0, Color32::from_gray(170));
        let mut x = beyond.left() - BAR_H;
        while x < beyond.right() {
            let xa = x.max(beyond.left());
            let xb = (x + BAR_H).min(beyond.right());
            let a = pos2(xa, bar.bottom() - (xa - x));
            let b = pos2(xb, bar.top() + (x + BAR_H - xb));
            painter.line_segment([a, b], hatch);
            x += 8.0;
        }
    }

    // Numerals in the patches that have room.
    let text_on = |c: Color32| if c.r() > 128 { Color32::from_gray(20) } else { Color32::from_gray(235) };
    for k in 0..zones {
        let r = Rect::from_min_max(pos2(edge_x(k), bar.top()), pos2(edge_x(k + 1), bar.bottom()));
        if r.width() > 14.0 {
            let mid = grey(0.5 * (edges[k] + edges[k + 1]));
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

    // Boundaries: thin lines, dark on the light end, light on the dark end.
    for (i, &e) in edges.iter().enumerate().take(zones).skip(1) {
        let x = edge_x(i);
        let on = grey(e);
        let colour =
            if on.r() > 128 { Color32::from_black_alpha(140) } else { Color32::from_white_alpha(140) };
        painter.line_segment([pos2(x, bar.top()), pos2(x, bar.bottom())], Stroke::new(1.0, colour));
    }

    // Hovered zone: an outline.
    if let Some(k) = hover_zone {
        let k = k as usize;
        let r = Rect::from_min_max(pos2(edge_x(k), bar.top()), pos2(edge_x(k + 1), bar.bottom()));
        painter.rect_stroke(
            r,
            0.0,
            Stroke::new(2.0, Color32::from_rgb(255, 170, 60)),
            egui::StrokeKind::Inside,
        );
    }

    let label_y = bar.bottom() + 2.0;
    painter.text(
        pos2(bar.left(), label_y),
        Align2::LEFT_TOP,
        "black",
        FontId::proportional(9.0),
        visuals.weak_text_color(),
    );
    if x_of(clip) < bar.right() - 56.0 {
        painter.text(
            pos2(x_of(clip), label_y),
            Align2::CENTER_TOP,
            "clip",
            FontId::proportional(9.0),
            visuals.weak_text_color(),
        );
    }
    painter.text(
        pos2(bar.right(), label_y),
        Align2::RIGHT_TOP,
        "paper white",
        FontId::proportional(9.0),
        visuals.weak_text_color(),
    );

    if let Some(k) = hover_zone {
        let k = k as usize;
        let l = if k == 0 { zone::linear(0) } else { zone::linear(k) };
        let v = curve.eval(l);
        response.clone().on_hover_text(format!(
            "Zone {}: prints at {v:.3} (density {:.2}), {:.0} % of the paper\n\
             drag: both exposures, the tone under the pointer follows",
            zone::numeral(k),
            curve.density(l),
            100.0 * (edges[k + 1] - edges[k]),
        ));
    }
}
