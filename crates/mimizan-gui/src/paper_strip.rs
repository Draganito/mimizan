//! The paper strip: the one control the paper has, as on the phone.
//!
//! The axis is the scene, one zone per stop, from five stops under the
//! grey card up to the clip, two and a half stops above it, where the
//! negative ends; the card stands at the mark, two thirds along. Each
//! slice is the paper's value for that grey under the current pair of
//! exposures. So the strip is the print of a grey wedge, every slice of
//! it a grey of its own, and the two exposures only move the greys along
//! it. The phone's strip has the same axis. A
//! drag moves both exposures together, the whole print lighter or darker,
//! as one more or less second under the enlarger. The wheel over the strip
//! is the grade: hard light against soft, one twelfth of a stop per
//! notch, from grade 00 to grade 5 and no further, with the print's
//! mid-grey left where it is (`SplitGrade::graded`): the ramp turns
//! round that tone, steeper or flatter, and does not slide. A double click is the base print, and
//! a second one brings the pair back: the pair of this picture, another
//! picture has its own. The strip says nothing in words: the pair it
//! stands for is in the recipe file.
//!
//! Nothing here touches the paper's arithmetic: the pair the strip hands
//! back is the same two numbers the two sliders used to set, on the same
//! grid of twelfths, inside the same `STOP_LIMIT`.

use egui::{pos2, vec2, Color32, CursorIcon, Event, Id, MouseWheelUnit, Rect, Sense, Shape, Stroke, Ui};
use mimizan_core::paper::{SplitGrade, STOP_LIMIT};
use mimizan_core::zone;

/// Height of the strip, points.
const STRIP_H: f32 = 36.0;
/// Stops of the scene the strip shows under the grey card.
const BELOW: f64 = 5.0;
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

/// Stops of the scene over the grey card: up to the clip, linear 1.
fn above() -> f64 {
    -zone::GRAY_CARD.log2()
}

/// Where a grey `stops` from the grey card stands, as a part of the
/// strip's width. The card itself is at `frac_of(0.0)`.
fn frac_of(stops: f64) -> f64 {
    (stops + BELOW) / (BELOW + above())
}

/// Linear grey at `frac` of the strip's width: [`BELOW`] stops under the
/// grey card at the left edge, the clip at the right.
fn linear_at(frac: f64) -> f64 {
    (zone::GRAY_CARD * (frac * (BELOW + above()) - BELOW).exp2()).clamp(0.0, 1.0)
}

/// The whole stops from the grey card that lie inside the strip, the
/// edges left out: the ticks.
fn zone_ticks() -> impl Iterator<Item = f64> {
    let first = 1 - BELOW.ceil() as i32;
    (first..).map(f64::from).take_while(|&stops| stops < above())
}

/// Draws the strip for `split` and lets the pointer move it. `picture`
/// names the picture the pair belongs to: what a double click remembers
/// is kept under it, so it is not handed to the next picture.
pub fn paper_strip(ui: &mut Ui, picture: Id, split: &mut SplitGrade, width: f32) {
    let (rect, response) = ui.allocate_exact_size(vec2(width, STRIP_H), Sense::click_and_drag());
    let painter = ui.painter_at(rect);
    let id = response.id;
    let remembered = id.with("remembered").with(picture);
    let off = split.soft.abs() >= 1.0 / 24.0 || split.hard.abs() >= 1.0 / 24.0;

    // A double click: the base print, or the pair the last one left.
    type Pair = (f64, f64);
    if response.double_clicked() {
        if off {
            ui.data_mut(|d| d.insert_temp::<Pair>(remembered, (split.soft, split.hard)));
            split.soft = 0.0;
            split.hard = 0.0;
        } else if let Some((s, h)) = ui.data(|d| d.get_temp::<Pair>(remembered)) {
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
                *split = split.graded(notches as i32);
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
        // The zones, whole stops from the grey card, as ticks from the top edge.
        let tick = Color32::from_rgba_unmultiplied(128, 128, 128, 150);
        for stops in zone_ticks() {
            let x = rect.left() + rect.width() * frac_of(stops) as f32;
            painter.line_segment(
                [pos2(x, rect.top()), pos2(x, rect.top() + STRIP_H * 0.3)],
                Stroke::new(1.0, tick),
            );
        }
        // The mark: the grey card's place, never moving.
        let mx = rect.left() + rect.width() * frac_of(0.0) as f32;
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
}

// The drag's arithmetic, the same on the phone (`GradeMath` on Android,
// `Prefs` on iOS). The wheel's is the core's, `SplitGrade::graded`; the
// phone's spread of two fingers takes the same step.

fn finite_or_zero(v: f64) -> f64 {
    if v.is_finite() {
        v
    } else {
        0.0
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    const LIMIT: f64 = 3.0;
    const EPS: f64 = 1e-9;

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < EPS
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

    /// The strip in a window without a screen, with the wheel turned over it.
    fn wheel_on_strip(ctx: &egui::Context, time: &mut f64, lines: f32, split: &mut SplitGrade) {
        let at = pos2(160.0, 26.0);
        let wheel = Event::MouseWheel {
            unit: MouseWheelUnit::Line,
            delta: vec2(0.0, lines),
            phase: egui::TouchPhase::Move,
            modifiers: egui::Modifiers::NONE,
        };
        // The pointer comes to rest on the strip, then the wheel turns.
        for events in [vec![Event::PointerMoved(at)], vec![], vec![wheel], vec![]] {
            *time += 0.02;
            let screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0)));
            let input = egui::RawInput { events, time: Some(*time), screen_rect, ..Default::default() };
            let _ = ctx.run_ui(input, |ui| {
                let strip = Rect::from_min_size(pos2(10.0, 8.0), vec2(300.0, STRIP_H));
                ui.scope_builder(egui::UiBuilder::new().max_rect(strip), |ui| {
                    paper_strip(ui, Id::new("a.nef"), split, 300.0)
                });
            });
        }
    }

    /// The wheel is the grade and nothing else. Before, it kept the mean
    /// of the two times and ran to their limit: by grade 5 the grey card
    /// had gone from density 0.67 to 1.33, so every change of grade had to
    /// be followed by a drag, and past grade 5 forty-eight more notches
    /// only made the print darker.
    #[test]
    fn the_wheel_turns_the_grade_and_leaves_the_grey_card_at_its_tone() {
        use mimizan_core::paper::{grade_label, GREY_DENSITY};
        use mimizan_core::zone::GRAY_CARD;
        let ctx = egui::Context::default();
        let mut time = 0.0;
        let mut split = SplitGrade::default();
        let mut worst = 0.0f64;
        let mut names = vec![grade_label(split.curve().grade())];
        // Up, a notch at a time, well past the hard end.
        for _ in 0..40 {
            wheel_on_strip(&ctx, &mut time, 1.0, &mut split);
            worst = worst.max((split.curve().density(GRAY_CARD) - GREY_DENSITY).abs());
            names.push(grade_label(split.curve().grade()));
        }
        assert!(worst < 0.06, "the grey card moved by {worst:.3} of density");
        assert_eq!(names.last().map(String::as_str), Some("5"));
        // It is the core's step, and it stopped at the end of the grades.
        assert_eq!(split, SplitGrade::default().graded(40));
        assert_eq!(split, SplitGrade::default().graded(24));
        // Down again by as many notches as it went up: the base print.
        for _ in 0..24 {
            wheel_on_strip(&ctx, &mut time, -1.0, &mut split);
        }
        assert_eq!((split.soft, split.hard), (0.0, 0.0));
        // Three lines in one turn are three notches.
        wheel_on_strip(&ctx, &mut time, -3.0, &mut split);
        assert_eq!(split, SplitGrade::default().graded(-3));
    }

    /// The strip in a window without a screen, with a double click on it.
    fn double_click_on_strip(ctx: &egui::Context, time: &mut f64, picture: &str, split: &mut SplitGrade) {
        use egui::{Modifiers, PointerButton};
        let at = pos2(160.0, 26.0);
        let button = |pressed| Event::PointerButton {
            pos: at,
            button: PointerButton::Primary,
            pressed,
            modifiers: Modifiers::NONE,
        };
        for events in [
            vec![Event::PointerMoved(at)],
            vec![button(true)],
            vec![button(false)],
            vec![button(true)],
            vec![button(false)],
        ] {
            *time += 0.02;
            let screen_rect = Some(Rect::from_min_size(pos2(0.0, 0.0), vec2(400.0, 300.0)));
            let input = egui::RawInput { events, time: Some(*time), screen_rect, ..Default::default() };
            let _ = ctx.run_ui(input, |ui| {
                let strip = Rect::from_min_size(pos2(10.0, 8.0), vec2(300.0, STRIP_H));
                ui.scope_builder(egui::UiBuilder::new().max_rect(strip), |ui| {
                    paper_strip(ui, Id::new(picture), split, 300.0)
                });
            });
        }
    }

    /// The pair a double click brings back is this picture's. Before, the
    /// strip kept one pair for the window, and the first double click on
    /// the next picture gave it the last picture's exposures.
    #[test]
    fn a_double_click_does_not_bring_back_another_pictures_pair() {
        let ctx = egui::Context::default();
        let mut time = 0.0;
        let mut first = SplitGrade::new(-0.75, 0.5);
        double_click_on_strip(&ctx, &mut time, "a.nef", &mut first);
        assert_eq!((first.soft, first.hard), (0.0, 0.0), "the base print");
        // The next picture, at its base print: nothing of its own to bring back.
        time += 5.0;
        let mut second = SplitGrade::default();
        double_click_on_strip(&ctx, &mut time, "b.nef", &mut second);
        assert_eq!((second.soft, second.hard), (0.0, 0.0), "the next picture");
        // Back on the first picture its pair is still there.
        time += 5.0;
        double_click_on_strip(&ctx, &mut time, "a.nef", &mut first);
        assert_eq!((first.soft, first.hard), (-0.75, 0.5), "the first picture's pair");
    }

    /// Every slice of the strip is a grey of its own. Before, the strip
    /// ran five stops above the grey card, but the scene ends at the clip,
    /// two and a half above it: the right quarter was one flat tone under
    /// every pair.
    #[test]
    fn every_slice_of_the_strip_is_its_own_grey() {
        let greys: Vec<f64> = (0..SAMPLES).map(|i| linear_at((i as f64 + 0.5) / SAMPLES as f64)).collect();
        let flat = greys.windows(2).filter(|w| w[1] <= w[0]).count();
        assert_eq!(flat, 0, "{flat} of {SAMPLES} slices repeat the one before");
    }

    #[test]
    fn the_strip_runs_from_five_stops_under_the_card_to_the_clip() {
        assert!(close(linear_at(0.0), zone::GRAY_CARD / 32.0));
        assert!(close(linear_at(frac_of(0.0)), zone::GRAY_CARD));
        assert!(close(linear_at(frac_of(1.0)), zone::GRAY_CARD * 2.0));
        assert!(close(linear_at(frac_of(-3.0)), zone::GRAY_CARD / 8.0));
        assert!(close(linear_at(1.0), 1.0));
        assert!(close(frac_of(above()), 1.0) && close(frac_of(-BELOW), 0.0));
        // The card stands two thirds along, and the ticks are the whole
        // stops between the edges: four under the card, the card, two over.
        assert!((frac_of(0.0) - 0.669).abs() < 1e-3, "{}", frac_of(0.0));
        let ticks: Vec<f64> = zone_ticks().collect();
        assert_eq!(ticks, [-4.0, -3.0, -2.0, -1.0, 0.0, 1.0, 2.0]);
    }
}
