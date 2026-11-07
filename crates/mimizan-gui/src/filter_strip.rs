//! The filter strip: the taking filters as one row over the paper strip,
//! as on the phone. The width is shared in equal fields, one type size
//! whatever stands in them; the chosen field is written in the glass's
//! own colour with a line under it that slides when the choice moves, the
//! others are grey. A click chooses, a finger drawn along the row scrubs.
//! A field the file cannot answer (a calibrated mix with no camera file,
//! the camera's matrix with none in the file) is dimmed and says why.

use egui::{pos2, vec2, Align2, Color32, CursorIcon, FontId, Rect, Sense, Stroke, Ui};
use mimizan_core::mix::ColorFilter;

/// Height of the strip, points.
const STRIP_H: f32 = 24.0;
/// Room each label keeps to its neighbours.
const FIELD_PAD: f32 = 10.0;
/// The underline's share of its field.
const LINE_SHARE: f32 = 0.6;

/// One field of the strip.
pub struct Field {
    pub label: String,
    /// The glass held up to the light; white for no glass.
    pub ink: Color32,
    /// Shown, not offered.
    pub dimmed: bool,
    pub hover: String,
}

impl Field {
    pub fn new(label: impl Into<String>, ink: Color32, hover: impl Into<String>) -> Self {
        Self { label: label.into(), ink, dimmed: false, hover: hover.into() }
    }

    pub fn dimmed(mut self, dimmed: bool) -> Self {
        self.dimmed = dimmed;
        self
    }
}

fn font() -> FontId {
    FontId::proportional(13.0)
}

/// The width below which `labels` in equal fields would touch.
pub fn min_width(ui: &Ui, labels: &[&str]) -> f32 {
    let widest = labels
        .iter()
        .map(|l| ui.painter().layout_no_wrap((*l).to_owned(), font(), Color32::WHITE).size().x)
        .fold(0.0f32, f32::max);
    (widest + FIELD_PAD) * labels.len().max(1) as f32
}

/// The glass by its short name, as the phone writes it.
pub fn filter_short(f: ColorFilter) -> &'static str {
    match f {
        ColorFilter::None => "Pan",
        ColorFilter::Yellow8 => "Y8",
        ColorFilter::YellowGreen11 => "YG11",
        ColorFilter::Orange16 => "O16",
        ColorFilter::Red25 => "R25",
        ColorFilter::Green58 => "G58",
        ColorFilter::Blue47 => "B47",
    }
}

/// The glass's own colour, as it looks held up to the light; the same on
/// the phone.
pub fn filter_ink(f: ColorFilter) -> Color32 {
    match f {
        ColorFilter::None => Color32::WHITE,
        ColorFilter::Yellow8 => Color32::from_rgb(255, 217, 51),
        ColorFilter::YellowGreen11 => Color32::from_rgb(191, 230, 64),
        ColorFilter::Orange16 => Color32::from_rgb(255, 153, 51),
        ColorFilter::Red25 => Color32::from_rgb(255, 82, 66),
        ColorFilter::Green58 => Color32::from_rgb(77, 217, 102),
        ColorFilter::Blue47 => Color32::from_rgb(102, 148, 255),
    }
}

/// Draws the strip; `selected` is the field written in colour, none for a
/// mix that is no preset. Returns the field a click or a scrub chose.
pub fn filter_strip(
    ui: &mut Ui,
    id_salt: &str,
    fields: &[Field],
    selected: Option<usize>,
    width: f32,
) -> Option<usize> {
    let n = fields.len().max(1);
    let (rect, response) = ui.allocate_exact_size(vec2(width, STRIP_H), Sense::click_and_drag());
    let id = ui.make_persistent_id(id_salt);
    let field_w = rect.width() / n as f32;
    let field_at = |x: f32| -> usize { (((x - rect.left()) / field_w).floor().max(0.0) as usize).min(n - 1) };

    // A click, or a scrub: the field under the pointer, if it answers.
    let mut chosen: Option<usize> = None;
    if response.clicked() || response.dragged() {
        if let Some(p) = response.interact_pointer_pos() {
            let k = field_at(p.x);
            if Some(k) != selected && fields.get(k).is_some_and(|f| !f.dimmed) {
                chosen = Some(k);
            }
        }
    }
    let shown = chosen.or(selected);

    let hovered = response.hover_pos().filter(|p| rect.contains(*p)).map(|p| field_at(p.x));
    if let Some(k) = hovered {
        if fields.get(k).is_some_and(|f| !f.dimmed) {
            ui.ctx().set_cursor_icon(CursorIcon::PointingHand);
        }
    }

    if ui.is_rect_visible(rect) {
        let painter = ui.painter_at(rect);
        let weak = ui.visuals().weak_text_color();
        for (k, f) in fields.iter().enumerate() {
            let field = Rect::from_min_size(
                pos2(rect.left() + k as f32 * field_w, rect.top()),
                vec2(field_w, STRIP_H),
            );
            let colour = if Some(k) == shown {
                f.ink
            } else if f.dimmed {
                weak.linear_multiply(0.45)
            } else if hovered == Some(k) {
                ui.visuals().text_color()
            } else {
                weak
            };
            painter.text(
                pos2(field.center().x, field.center().y - 2.0),
                Align2::CENTER_CENTER,
                &f.label,
                font(),
                colour,
            );
        }
        // The line under the chosen field, sliding to it.
        if let Some(k) = shown {
            let target = rect.left() + (k as f32 + 0.5) * field_w;
            let x = ui.ctx().animate_value_with_time(id.with("line"), target, 0.15);
            let half = field_w * LINE_SHARE / 2.0;
            let y = rect.bottom() - 1.5;
            let ink = fields.get(k).map(|f| f.ink).unwrap_or(Color32::WHITE);
            painter.line_segment([pos2(x - half, y), pos2(x + half, y)], Stroke::new(2.0, ink));
        }
    }

    if let Some(k) = hovered {
        if let Some(f) = fields.get(k) {
            response.on_hover_text(&f.hover);
        }
    }
    chosen
}
