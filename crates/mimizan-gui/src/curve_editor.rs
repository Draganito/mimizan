//! Control-point editor for a custom look (PCHIP through the points, same
//! interpolation the print path uses). Drag a point to move it, double-click
//! on the curve to add one, right-click a point to remove it. End points stay
//! at x = 0 and x = 1 (black and white point are still movable in y).

use egui::{pos2, vec2, Color32, Pos2, Sense, Stroke, Ui};
use mimizan_core::curve::Pchip;

const MIN_DX: f64 = 0.02;

/// Returns true when the points changed.
pub fn curve_editor(ui: &mut Ui, points: &mut Vec<[f64; 2]>, size: f32) -> bool {
    let (rect, response) = ui.allocate_exact_size(vec2(size, size), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let to_screen = |p: [f64; 2]| -> Pos2 {
        pos2(rect.left() + p[0] as f32 * rect.width(), rect.bottom() - p[1] as f32 * rect.height())
    };
    let from_screen = |s: Pos2| -> [f64; 2] {
        [
            (((s.x - rect.left()) / rect.width()) as f64).clamp(0.0, 1.0),
            (((rect.bottom() - s.y) / rect.height()) as f64).clamp(0.0, 1.0),
        ]
    };

    let visuals = ui.visuals();
    painter.rect_filled(rect, 3.0, visuals.extreme_bg_color);
    let grid = Stroke::new(1.0, visuals.weak_text_color().linear_multiply(0.4));
    for i in 1..4 {
        let f = i as f32 / 4.0;
        painter.line_segment(
            [
                pos2(rect.left() + f * rect.width(), rect.top()),
                pos2(rect.left() + f * rect.width(), rect.bottom()),
            ],
            grid,
        );
        painter.line_segment(
            [
                pos2(rect.left(), rect.top() + f * rect.height()),
                pos2(rect.right(), rect.top() + f * rect.height()),
            ],
            grid,
        );
    }
    painter.line_segment([rect.left_bottom(), rect.right_top()], grid);

    let mut changed = false;
    let id = response.id;
    // The index of the point being dragged lives in egui's temp storage as a
    // plain `usize` (temp data is keyed by type, so insert and read must agree).
    let mut dragging: Option<usize> = ui.data(|d| d.get_temp::<usize>(id));

    // Hit test for the nearest point.
    let nearest = |pos: Pos2, points: &[[f64; 2]]| -> Option<usize> {
        let mut best: Option<(usize, f32)> = None;
        for (i, p) in points.iter().enumerate() {
            let d = to_screen(*p).distance(pos);
            if d < 10.0 && best.is_none_or(|(_, bd)| d < bd) {
                best = Some((i, d));
            }
        }
        best.map(|(i, _)| i)
    };

    if response.drag_started() {
        // Hit-test where the button went down, not where the pointer is after
        // crossing egui's drag threshold.
        let origin = ui.input(|i| i.pointer.press_origin()).or_else(|| response.interact_pointer_pos());
        dragging = origin.and_then(|pos| nearest(pos, points));
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
            if i < points.len() {
                let mut p = from_screen(pos);
                let n = points.len();
                if i == 0 {
                    p[0] = 0.0;
                } else if i == n - 1 {
                    p[0] = 1.0;
                } else {
                    p[0] = p[0].clamp(points[i - 1][0] + MIN_DX, points[i + 1][0] - MIN_DX);
                }
                // Keep y monotone (the look file demands it).
                let lo = if i > 0 { points[i - 1][1] } else { 0.0 };
                let hi = if i + 1 < n { points[i + 1][1] } else { 1.0 };
                p[1] = p[1].clamp(lo, hi);
                if points[i] != p {
                    points[i] = p;
                    changed = true;
                }
            }
        }
    }
    if response.drag_stopped() {
        ui.data_mut(|d| d.remove_temp::<usize>(id));
        dragging = None;
    }
    if response.double_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            let p = from_screen(pos);
            if let Some(k) = points.iter().position(|q| q[0] > p[0]) {
                if k > 0 && p[0] - points[k - 1][0] >= MIN_DX && points[k][0] - p[0] >= MIN_DX {
                    let y = p[1].clamp(points[k - 1][1], points[k][1]);
                    points.insert(k, [p[0], y]);
                    changed = true;
                }
            }
        }
    }
    if response.secondary_clicked() {
        if let Some(pos) = response.interact_pointer_pos() {
            if let Some(i) = nearest(pos, points) {
                if i > 0 && i + 1 < points.len() {
                    points.remove(i);
                    changed = true;
                }
            }
        }
    }

    // Curve.
    if points.len() >= 2 {
        let pchip = Pchip::new(points);
        let n = 128;
        let pts: Vec<Pos2> = (0..=n)
            .map(|k| {
                let x = k as f64 / n as f64;
                to_screen([x, pchip.eval(x).clamp(0.0, 1.0)])
            })
            .collect();
        painter.add(egui::Shape::line(pts, Stroke::new(2.0, visuals.text_color())));
    }
    let hover = response.hover_pos().and_then(|p| nearest(p, points));
    for (i, p) in points.iter().enumerate() {
        let c = to_screen(*p);
        let active = dragging == Some(i) || hover == Some(i);
        let r = if active { 6.0 } else { 4.5 };
        painter.circle(
            c,
            r,
            if active { Color32::from_rgb(255, 200, 80) } else { visuals.text_color() },
            Stroke::new(1.0, visuals.extreme_bg_color),
        );
    }
    response.on_hover_text("drag: move point · double-click: add · right-click: remove");
    changed
}
