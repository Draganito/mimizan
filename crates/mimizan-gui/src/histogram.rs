//! Histogram display: 256 bins, linear or log height, quarter grid, clipping
//! readout. Heights are normalised to the tallest bin *excluding* the two end
//! bins, so clipped pixels show as full-height edge bars without flattening
//! the rest.

use crate::session::{Histogram, RawHistogram};
use egui::{pos2, Color32, Sense, Stroke, Ui};
use mimizan_core::cfa::Color;

/// Colour a CFA channel is drawn in (the two greens differ in shade).
fn channel_colour(c: Color, second_green: bool) -> Color32 {
    match c {
        Color::R => Color32::from_rgb(235, 80, 70),
        Color::G if second_green => Color32::from_rgb(90, 200, 110),
        Color::G => Color32::from_rgb(150, 230, 120),
        Color::B => Color32::from_rgb(90, 140, 255),
    }
}

/// Four CFA channels as outlines on one gamma-2.2 axis, each normalised to
/// its own peak (excluding the end bins) so the shapes compare; the
/// clipping fraction per channel is printed beside the legend.
pub fn raw_histogram_widget(ui: &mut Ui, h: &RawHistogram, size: egui::Vec2, log_scale: bool) {
    let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 3.0, visuals.extreme_bg_color);
    let grid = Stroke::new(1.0, visuals.weak_text_color().linear_multiply(0.4));
    for i in 1..4 {
        let x = rect.left() + rect.width() * i as f32 / 4.0;
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], grid);
    }
    if h.total.iter().all(|&t| t == 0) {
        return;
    }
    if h.mono {
        draw_channel(&painter, rect, &h.bins[0], log_scale, Color32::from_rgb(210, 210, 210));
        painter.text(
            pos2(rect.left() + 4.0, rect.top() + 3.0),
            egui::Align2::LEFT_TOP,
            format!("sensor {:.2} %", 100.0 * h.clipped_frac(0)),
            egui::FontId::proportional(10.5),
            Color32::from_rgb(210, 210, 210),
        );
        return;
    }
    let mut seen_green = false;
    for t in 0..4 {
        let colour = channel_colour(h.colors[t], h.colors[t] == Color::G && seen_green);
        seen_green |= h.colors[t] == Color::G;
        draw_channel(&painter, rect, &h.bins[t], log_scale, colour);
    }
    // Legend with clipping per channel, top-left.
    let font = egui::FontId::proportional(10.5);
    let mut y = rect.top() + 3.0;
    seen_green = false;
    for t in 0..4 {
        let colour = channel_colour(h.colors[t], h.colors[t] == Color::G && seen_green);
        let name = match h.colors[t] {
            Color::R => "R",
            Color::G if seen_green => "G2",
            Color::G => "G1",
            Color::B => "B",
        };
        seen_green |= h.colors[t] == Color::G;
        let txt = format!("{name} {:.2} %", 100.0 * h.clipped_frac(t));
        painter.text(pos2(rect.left() + 4.0, y), egui::Align2::LEFT_TOP, txt, font.clone(), colour);
        y += 12.0;
    }
}

fn draw_channel(painter: &egui::Painter, rect: egui::Rect, bins: &[u32], log_scale: bool, colour: Color32) {
    let peak = bins[1..255].iter().copied().max().unwrap_or(0).max(1) as f64;
    let pts: Vec<egui::Pos2> = bins
        .iter()
        .enumerate()
        .map(|(i, &n)| {
            let f = if n == 0 {
                0.0
            } else if log_scale {
                ((n as f64).ln_1p() / peak.ln_1p()).min(1.0)
            } else {
                (n as f64 / peak).min(1.0)
            };
            pos2(
                rect.left() + (i as f32 + 0.5) / 256.0 * rect.width(),
                rect.bottom() - 1.0 - f as f32 * (rect.height() - 3.0),
            )
        })
        .collect();
    painter.add(egui::Shape::line(pts, Stroke::new(1.2, colour)));
}

/// The print or negative histogram as bars; with `channels`, the plate's
/// three channels as outlines over it, each normalised to its own peak.
pub fn histogram_widget(
    ui: &mut Ui,
    h: &Histogram,
    channels: Option<&[[u32; 256]; 3]>,
    size: egui::Vec2,
    log_scale: bool,
) {
    let (rect, response) = ui.allocate_exact_size(size, Sense::hover());
    let painter = ui.painter_at(rect);
    let visuals = ui.visuals();
    painter.rect_filled(rect, 3.0, visuals.extreme_bg_color);
    let grid = Stroke::new(1.0, visuals.weak_text_color().linear_multiply(0.4));
    for i in 1..4 {
        let x = rect.left() + rect.width() * i as f32 / 4.0;
        painter.line_segment([pos2(x, rect.top()), pos2(x, rect.bottom())], grid);
    }
    if h.total == 0 {
        return;
    }

    let peak = h.bins[1..255].iter().copied().max().unwrap_or(0).max(1) as f64;
    let height = |n: u32| -> f32 {
        if n == 0 {
            return 0.0;
        }
        let f =
            if log_scale { ((n as f64).ln_1p() / peak.ln_1p()).min(1.0) } else { (n as f64 / peak).min(1.0) };
        (f as f32 * (rect.height() - 2.0)).max(1.0)
    };

    let bar_w = rect.width() / 256.0;
    let fill = visuals.text_color().linear_multiply(0.85);
    let clip = Color32::from_rgb(255, 120, 80);
    for (i, &n) in h.bins.iter().enumerate() {
        if n == 0 {
            continue;
        }
        let x0 = rect.left() + i as f32 * bar_w;
        let hh = height(n);
        let colour = if i == 0 || i == 255 { clip } else { fill };
        painter.rect_filled(
            egui::Rect::from_min_max(pos2(x0, rect.bottom() - hh), pos2(x0 + bar_w.max(1.0), rect.bottom())),
            0.0,
            colour,
        );
    }

    if let Some(ch) = channels {
        for (bins, c) in ch.iter().zip([Color::R, Color::G, Color::B]) {
            draw_channel(&painter, rect, bins, log_scale, channel_colour(c, false));
        }
    }

    // Mean marker.
    let mx = rect.left() + h.mean as f32 * rect.width();
    painter.line_segment(
        [pos2(mx, rect.top()), pos2(mx, rect.bottom())],
        Stroke::new(1.0, Color32::from_rgb(255, 200, 80).linear_multiply(0.8)),
    );

    // Readout painted into the widget itself: a tooltip whose text changes
    // every frame keeps re-opening and shows only intermittently.
    if let Some(p) = response.hover_pos().filter(|p| rect.contains(*p)) {
        let bin = (((p.x - rect.left()) / rect.width()) * 256.0).floor().clamp(0.0, 255.0) as usize;
        let n = h.bins[bin];
        let x = rect.left() + (bin as f32 + 0.5) * bar_w;
        painter.line_segment(
            [pos2(x, rect.top()), pos2(x, rect.bottom())],
            Stroke::new(1.0, visuals.strong_text_color().linear_multiply(0.6)),
        );
        let text = format!(
            "level {bin} ({:.1} %)  {n} px = {:.2} %",
            100.0 * bin as f64 / 255.0,
            100.0 * n as f64 / h.total as f64
        );
        let font = egui::FontId::proportional(11.0);
        let galley = painter.layout_no_wrap(text, font, visuals.strong_text_color());
        // Keep the label inside the widget, on the side away from the cursor.
        let pad = 3.0;
        let w = galley.size().x + 2.0 * pad;
        let left = if x + 6.0 + w <= rect.right() { x + 6.0 } else { (x - 6.0 - w).max(rect.left()) };
        let label = egui::Rect::from_min_size(
            pos2(left, rect.top() + 2.0),
            galley.size() + egui::vec2(2.0 * pad, 2.0 * pad),
        );
        painter.rect_filled(label, 2.0, visuals.extreme_bg_color.linear_multiply(0.9));
        painter.galley(label.min + egui::vec2(pad, pad), galley, visuals.strong_text_color());
    }
}
