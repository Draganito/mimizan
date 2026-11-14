//! The toolbar's own controls. Each kind has one look, so what a control
//! does can be read before it is pressed: a panel that is shown stands on
//! a grey field, a tool in the hand is amber, an action is plain, and the
//! one button that keeps something is filled. One of several is the
//! filter strip's underline. The icons are drawn here: the fonts that
//! come with egui have no crop, no brush and no film strip. The Print
//! window uses the same looks for what is the same kind of thing there.

use crate::filter_strip::{self, filter_strip, Field};
use crate::paper_strip::AMBER;
use egui::{
    pos2, vec2, Color32, FontId, Galley, Painter, Pos2, Response, RichText, Sense, Shape, Stroke, StrokeKind,
    Ui, Widget, WidgetInfo, WidgetType,
};
use std::sync::Arc;

/// Height of a row and of every control in it, points.
pub const ROW_H: f32 = 26.0;
/// Height of the number field and the box that stand among them.
pub const FIELD_H: f32 = 24.0;

const TEXT: f32 = 13.0;
const SMALL: f32 = 11.0;
/// Side of an icon's square.
const ICON: f32 = 14.0;
/// Between the parts of one control.
const GAP: f32 = 6.0;
/// Left and right of a control with a word in it, and of one that is an
/// icon alone.
const PAD: f32 = 9.0;
const PAD_ICON: f32 = 6.0;
const RADIUS: f32 = 6.0;
/// The mark before a note that something is still to be kept.
const DOT: f32 = 6.0;

const INK: Color32 = Color32::from_gray(210);
const INK_LIT: Color32 = Color32::from_gray(242);
const INK_QUIET: Color32 = Color32::from_gray(166);
const INK_SMALL: Color32 = Color32::from_gray(140);
const SHOWN: Color32 = Color32::from_gray(48);
const SHOWN_HOVER: Color32 = Color32::from_gray(58);
const HOVER: Color32 = Color32::from_gray(40);
const IN_HAND: Color32 = Color32::from_rgb(51, 39, 15);
const IN_HAND_EDGE: Color32 = Color32::from_rgb(138, 95, 36);
const KEEP_HOVER: Color32 = Color32::from_rgb(255, 188, 98);
const KEEP_INK: Color32 = Color32::from_gray(27);
const KEEP_KEY: Color32 = Color32::from_rgb(90, 58, 12);
const BAR: Color32 = Color32::from_gray(60);

#[derive(Clone, Copy)]
pub enum Icon {
    FilmStrip,
    /// A quarter turn, clockwise.
    Turn,
    Crop,
    /// Lighter and darker: a disc, half of it filled.
    Dodge,
    Undo,
    Redo,
    Cross,
}

#[derive(Clone, Copy)]
enum Look {
    /// Shows or hides something, on its own; lit while it is shown.
    Toggle(bool),
    /// A tool; lit while it is in the hand.
    Tool(bool),
    /// Done once, when pressed.
    Action,
    /// The step that keeps what the tool has made.
    Keep,
}

/// One control of the toolbar.
pub struct Chip<'a> {
    look: Look,
    icon: Option<Icon>,
    step: Option<&'a str>,
    label: &'a str,
    key: Option<&'a str>,
    name: Option<&'a str>,
    height: f32,
    least: f32,
}

impl<'a> Chip<'a> {
    fn new(look: Look, label: &'a str) -> Self {
        Self { look, icon: None, step: None, label, key: None, name: None, height: ROW_H, least: 0.0 }
    }

    pub fn toggle(label: &'a str, shown: bool) -> Self {
        Self::new(Look::Toggle(shown), label)
    }

    pub fn tool(label: &'a str, icon: Icon, in_hand: bool) -> Self {
        Self::new(Look::Tool(in_hand), label).icon(icon)
    }

    pub fn action(label: &'a str) -> Self {
        Self::new(Look::Action, label)
    }

    pub fn keep(label: &'a str) -> Self {
        Self::new(Look::Keep, label)
    }

    /// An action that is an icon alone; `name` is what a screen reader says.
    pub fn icon_only(icon: Icon, name: &'a str) -> Self {
        let mut chip = Self::new(Look::Action, "").icon(icon);
        chip.name = Some(name);
        chip
    }

    pub fn icon(mut self, icon: Icon) -> Self {
        self.icon = Some(icon);
        self
    }

    /// The number of the step, small, before the word.
    pub fn step(mut self, step: &'a str) -> Self {
        self.step = Some(step);
        self
    }

    /// The key that does the same, small, after the word.
    pub fn key(mut self, key: Option<&'a str>) -> Self {
        self.key = key;
        self
    }

    /// As high as the row it stands in, where that is not the toolbar's.
    pub fn height(mut self, height: f32) -> Self {
        self.height = height;
        self
    }

    /// At least this wide, the words at its left: a button that fills
    /// the row it stands in.
    pub fn wide(mut self, width: f32) -> Self {
        self.least = width;
        self
    }

    pub fn width(&self, ui: &Ui) -> f32 {
        self.lay(ui).width.max(self.least)
    }

    fn lay(&self, ui: &Ui) -> Laid {
        let step = self.step.map(|s| galley(ui, s, SMALL));
        let label = (!self.label.is_empty()).then(|| galley(ui, self.label, TEXT));
        let key = self.key.map(|s| galley(ui, s, SMALL));
        let pad = if label.is_some() { PAD } else { PAD_ICON };
        let parts = [
            self.icon.map(|_| ICON),
            step.as_ref().map(|g| g.size().x),
            label.as_ref().map(|g| g.size().x),
            key.as_ref().map(|g| g.size().x),
        ];
        let n = parts.iter().flatten().count();
        let width = 2.0 * pad + parts.iter().flatten().sum::<f32>() + GAP * n.saturating_sub(1) as f32;
        Laid { step, label, key, pad, width }
    }
}

struct Laid {
    step: Option<Arc<Galley>>,
    label: Option<Arc<Galley>>,
    key: Option<Arc<Galley>>,
    pad: f32,
    width: f32,
}

fn galley(ui: &Ui, text: &str, size: f32) -> Arc<Galley> {
    ui.painter().layout_no_wrap(text.to_owned(), FontId::proportional(size), Color32::PLACEHOLDER)
}

impl Widget for Chip<'_> {
    fn ui(self, ui: &mut Ui) -> Response {
        let laid = self.lay(ui);
        let (rect, response) =
            ui.allocate_exact_size(vec2(laid.width.max(self.least), self.height), Sense::click());
        let enabled = ui.is_enabled();
        let name = self.name.unwrap_or(self.label);
        response.widget_info(|| match self.look {
            Look::Toggle(lit) | Look::Tool(lit) => {
                WidgetInfo::selected(WidgetType::Button, enabled, lit, name)
            }
            Look::Action | Look::Keep => WidgetInfo::labeled(WidgetType::Button, enabled, name),
        });
        if !ui.is_rect_visible(rect) {
            return response;
        }
        let hot = enabled && response.hovered();
        let (fill, edge, ink, small) = match self.look {
            Look::Toggle(true) => (Some(if hot { SHOWN_HOVER } else { SHOWN }), None, INK_LIT, INK_SMALL),
            Look::Toggle(false) => (hot.then_some(HOVER), None, if hot { INK } else { INK_QUIET }, INK_SMALL),
            Look::Tool(true) => (Some(IN_HAND), Some(IN_HAND_EDGE), AMBER, AMBER),
            Look::Tool(false) | Look::Action => (hot.then_some(HOVER), None, INK, INK_SMALL),
            Look::Keep => (Some(if hot { KEEP_HOVER } else { AMBER }), None, KEEP_INK, KEEP_KEY),
        };
        let painter = ui.painter();
        if let Some(fill) = fill {
            painter.rect_filled(rect, RADIUS, fill);
        }
        if let Some(edge) = edge {
            painter.rect_stroke(rect, RADIUS, Stroke::new(1.0, edge), StrokeKind::Inside);
        }
        let mid = rect.center().y;
        let mut x = rect.left() + laid.pad;
        if let Some(icon) = self.icon {
            paint_icon(painter, pos2(x, mid - ICON / 2.0), icon, ink);
            x += ICON + GAP;
        }
        for (text, colour) in [(laid.step, small), (laid.label, ink), (laid.key, small)] {
            if let Some(text) = text {
                let w = text.size().x;
                painter.galley(pos2(x, mid - text.size().y / 2.0), text, colour);
                x += w + GAP;
            }
        }
        response
    }
}

/// The icon in its square of [`ICON`] points, top left at `at`.
fn paint_icon(painter: &Painter, at: Pos2, icon: Icon, ink: Color32) {
    let stroke = Stroke::new(1.4, ink);
    // Redo is undo seen in a mirror.
    let flip = matches!(icon, Icon::Redo);
    let p = |(x, y): (f32, f32)| pos2(at.x + if flip { ICON - x } else { x }, at.y + y);
    let line = |points: &[(f32, f32)]| {
        painter.add(Shape::line(points.iter().copied().map(p).collect(), stroke));
    };
    // Part of a circle, degrees clockwise from the right.
    let arc = |(cx, cy): (f32, f32), r: f32, from: f32, to: f32| -> Vec<(f32, f32)> {
        let steps = ((to - from).abs() / 15.0).ceil().max(1.0) as usize;
        (0..=steps)
            .map(|k| {
                let a = (from + (to - from) * k as f32 / steps as f32).to_radians();
                (cx + r * a.cos(), cy + r * a.sin())
            })
            .collect()
    };
    match icon {
        Icon::FilmStrip => {
            line(&[(1.5, 2.5), (12.5, 2.5), (12.5, 11.5), (1.5, 11.5), (1.5, 2.5)]);
            line(&[(4.5, 2.5), (4.5, 11.5)]);
            line(&[(9.5, 2.5), (9.5, 11.5)]);
        }
        Icon::Turn => {
            line(&arc((7.0, 7.0), 4.5, 0.0, 312.0));
            line(&[(10.3, 1.4), (10.3, 3.9), (7.8, 3.9)]);
        }
        Icon::Crop => {
            line(&[(4.0, 1.0), (4.0, 10.0), (13.0, 10.0)]);
            line(&[(1.0, 4.0), (10.0, 4.0), (10.0, 13.0)]);
        }
        Icon::Dodge => {
            painter.circle_stroke(p((7.0, 7.0)), 5.0, stroke);
            let half = arc((7.0, 7.0), 5.0, -90.0, 90.0).into_iter().map(p).collect();
            painter.add(Shape::convex_polygon(half, ink, Stroke::NONE));
        }
        Icon::Undo | Icon::Redo => {
            line(&[(4.5, 2.5), (2.0, 5.0), (4.5, 7.5)]);
            let mut back = vec![(2.0, 5.0)];
            back.extend(arc((8.0, 8.25), 3.25, -90.0, 90.0));
            back.push((6.0, 11.5));
            line(&back);
        }
        Icon::Cross => {
            line(&[(4.0, 4.0), (10.0, 10.0)]);
            line(&[(10.0, 4.0), (4.0, 10.0)]);
        }
    }
}

/// One of several: the filter strip's fields, as wide as their words
/// need. An option is its label, its hover, and whether it is shown but
/// not offered.
pub fn choice(ui: &mut Ui, id_salt: &str, options: &[(&str, &str, bool)], selected: usize) -> Option<usize> {
    let fields: Vec<Field> = options
        .iter()
        .map(|&(label, hover, dimmed)| Field::new(label, Color32::WHITE, hover).dimmed(dimmed))
        .collect();
    let labels: Vec<&str> = options.iter().map(|o| o.0).collect();
    let width = filter_strip::min_width(ui, &labels) + GAP * labels.len() as f32;
    filter_strip(ui, id_salt, &fields, Some(selected), width)
}

/// The bar between two groups.
pub fn sep(ui: &mut Ui) {
    let (rect, _) = ui.allocate_exact_size(vec2(13.0, ROW_H), Sense::hover());
    let c = rect.center();
    ui.painter().line_segment([pos2(c.x, c.y - 8.0), pos2(c.x, c.y + 8.0)], Stroke::new(1.0, BAR));
}

/// The name of the option that follows.
pub fn caption(ui: &mut Ui, text: &str) {
    ui.label(RichText::new(text).color(INK_SMALL));
}

/// What the picture carries, or how many strokes: read, not pressed.
pub fn note(ui: &mut Ui, text: &str) -> Response {
    ui.add(egui::Label::new(RichText::new(text).color(INK_QUIET)).truncate())
}

pub fn pending_width(ui: &Ui, text: &str) -> f32 {
    DOT + GAP + galley(ui, text, TEXT).size().x
}

/// An amber dot and a word: something on the stage is not kept yet.
pub fn pending(ui: &mut Ui, text: &str) -> Response {
    let text = galley(ui, text, TEXT);
    let (rect, response) = ui.allocate_exact_size(vec2(DOT + GAP + text.size().x, ROW_H), Sense::hover());
    let mid = rect.center().y;
    let painter = ui.painter();
    painter.circle_filled(pos2(rect.left() + DOT / 2.0, mid), DOT / 2.0, AMBER);
    painter.galley(pos2(rect.left() + DOT + GAP, mid - text.size().y / 2.0), text, AMBER);
    response
}

#[cfg(test)]
mod tests {
    use super::{Chip, Icon, ROW_H};
    use egui::{pos2, Event, Modifiers, PointerButton, Rect};

    /// A press and a release on a chip that is the first thing in the
    /// window: whether the chip says it was clicked.
    fn clicked(enabled: bool) -> bool {
        let ctx = egui::Context::default();
        let at = pos2(24.0, 16.0);
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        let mut time = 0.0;
        let mut clicked = false;
        for events in [vec![Event::PointerMoved(at)], vec![button(true)], vec![button(false)]] {
            time += 0.02;
            let screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), egui::vec2(400.0, 300.0)));
            let input = egui::RawInput { events, time: Some(time), screen_rect, ..Default::default() };
            let _ = ctx.run_ui(input, |ui| {
                let chip = ui.add_enabled(enabled, Chip::tool("Crop", Icon::Crop, false));
                assert!(chip.rect.contains(at), "the press is not on the chip: {:?}", chip.rect);
                clicked |= chip.clicked();
            });
        }
        clicked
    }

    #[test]
    fn a_chip_is_pressed_like_a_button_and_not_while_it_is_disabled() {
        assert!(clicked(true));
        assert!(!clicked(false));
    }

    /// The crop row drops the keys in a narrow window by adding up the
    /// widths it is told, so a chip must be drawn as wide as it says.
    #[test]
    fn a_chip_is_drawn_as_wide_as_it_says() {
        let ctx = egui::Context::default();
        let _ = ctx.run_ui(egui::RawInput::default(), |ui| {
            let bare = Chip::action("Cancel").width(ui);
            let keyed = Chip::action("Cancel").key(Some("Esc")).width(ui);
            assert!(keyed > bare);
            let drawn = ui.add(Chip::action("Cancel").key(Some("Esc"))).rect;
            assert!((drawn.width() - keyed).abs() < 1.0, "{} drawn, {keyed} said", drawn.width());
            assert!((drawn.height() - ROW_H).abs() < 1.0);
            // In a panel: as high as its row, and as wide as it is told.
            let wide = Chip::keep("Export JPEG…").wide(280.0).height(30.0);
            assert!((wide.width(ui) - 280.0).abs() < 1.0);
            let drawn = ui.add(wide).rect;
            assert!((drawn.width() - 280.0).abs() < 1.0 && (drawn.height() - 30.0).abs() < 1.0);
            let narrow = Chip::toggle("1:1", true).wide(4.0).height(18.0);
            let said = narrow.width(ui);
            let drawn = ui.add(narrow).rect;
            assert!(said > 4.0 && (drawn.width() - said).abs() < 1.0 && (drawn.height() - 18.0).abs() < 1.0);
            // Every icon, in every look that carries one.
            for icon in
                [Icon::FilmStrip, Icon::Turn, Icon::Crop, Icon::Dodge, Icon::Undo, Icon::Redo, Icon::Cross]
            {
                ui.add(Chip::icon_only(icon, "icon"));
                ui.add(Chip::tool("tool", icon, true));
                ui.add(Chip::toggle("toggle", true).icon(icon).step("1"));
                ui.add(Chip::keep("keep").icon(icon));
            }
        });
    }
}
