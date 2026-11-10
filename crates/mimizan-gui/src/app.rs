//! Window layout: the toolbar, the film strip along the bottom, and the
//! stage with three docked windows on it (Negative left, Print right,
//! Enlarger below). Nothing here changes pixels; it only chooses
//! parameters and hands them to the workers.

use crate::browser::{
    self, home_dir, ExportFormat, Folder, Persisted, Ruler, Sort, ThumbJob, ThumbState, Thumbs,
};
use crate::brush::{self, Brush, Hit, Tool};
use crate::cpu::{CpuMeter, CpuView};
use crate::edit::{Edit, History};
use crate::filter_strip::{self, filter_ink, filter_short, filter_strip, Field};
use crate::histogram::{histogram_widget, raw_histogram_widget};
use crate::lamp_bar::{lamp_bar, Lamp};
use crate::paper_strip::paper_strip;
use crate::session::{DetailAsk, DevelopParams, Histogram, PlateView, Session, ViewParams, DETAIL_MARGIN};
use crate::worker::{
    available_memory, develop_memory, develop_memory_of_size, memory_fits, Event, Heavy, Job, Loupe,
    LoupeRequest, LoupeResult, PeakMeasure, PeakPoll, Preview, PreviewRequest, PreviewResult, Redevelop,
};
use egui::{
    Color32, ColorImage, CursorIcon, Pos2, Rect, RichText, Sense, TextureHandle, TextureOptions, Vec2,
};
use egui_file_dialog::{DialogState, FileDialog, OpeningMode};
use mimizan_core::burn::{self, Light, Stroke};
use mimizan_core::ingest::WhiteBalance;
use mimizan_core::mix::{ColorFilter, Weights};
use mimizan_core::paper::{self, SplitGrade};
use mimizan_core::plate::{PlateFilters, PlateParams, PlateSpace, PlateWeights};
use mimizan_core::print::{memory_estimate, memory_text, peak_print_radius, PrintSize, LAMP_LIMIT};
use mimizan_core::recipe::{CropCorner, CropRect, Recipe, Sharpen};
use mimizan_core::resize::ResampleKernel;
use mimizan_core::separate::MaskMode;
use mimizan_core::straighten;
use mimizan_core::tone::Tone;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Which side of the sheet the user typed. The other follows the picture.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SheetSide {
    Width,
    Height,
}

/// US darkroom paper, as a ratio. The long side follows the picture until
/// the ratio is turned.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PaperFormat {
    R4x5,
    R5x7,
    R8x10,
    R11x14,
    R16x20,
    R20x24,
    Square,
    /// The open picture's own ratio, read from its pixels.
    Image,
}

impl PaperFormat {
    const ALL: [Self; 7] =
        [Self::R4x5, Self::R5x7, Self::R8x10, Self::R11x14, Self::R16x20, Self::R20x24, Self::Square];

    fn label(self) -> &'static str {
        match self {
            Self::R4x5 => "4×5",
            Self::R5x7 => "5×7",
            Self::R8x10 => "8×10",
            Self::R11x14 => "11×14",
            Self::R16x20 => "16×20",
            Self::R20x24 => "20×24",
            Self::Square => "1:1",
            // The picture's writing is computed from its pixels.
            Self::Image => "1:1",
        }
    }

    /// The same ratio written the other way. A square stays `1:1`.
    fn swapped(self) -> &'static str {
        match self {
            Self::R4x5 => "5×4",
            Self::R5x7 => "7×5",
            Self::R8x10 => "10×8",
            Self::R11x14 => "14×11",
            Self::R16x20 => "20×16",
            Self::R20x24 => "24×20",
            Self::Square => "1:1",
            Self::Image => "1:1",
        }
    }

    /// The side the frame is on. A square is always `1:1`.
    fn framed(self, turned: bool) -> &'static str {
        if turned {
            self.swapped()
        } else {
            self.label()
        }
    }

    fn parts(self) -> (u32, u32) {
        match self {
            Self::R4x5 => (4, 5),
            Self::R5x7 => (5, 7),
            Self::R8x10 => (8, 10),
            Self::R11x14 => (11, 14),
            Self::R16x20 => (16, 20),
            Self::R20x24 => (20, 24),
            Self::Square => (1, 1),
            Self::Image => (1, 1),
        }
    }

    /// Long side of the format along the long side of the image.
    fn oriented(self, img_w: u32, img_h: u32) -> (u32, u32) {
        let (a, b) = self.parts();
        if img_w >= img_h {
            if a >= b {
                (a, b)
            } else {
                (b, a)
            }
        } else if a >= b {
            (b, a)
        } else {
            (a, b)
        }
    }

    /// `turned` swaps the two sides, so a landscape frame can stand portrait.
    /// The picture's own format uses the pixels it was given.
    fn frame(self, img_w: u32, img_h: u32, turned: bool) -> (u32, u32) {
        let (a, b) =
            if self == Self::Image { (img_w.max(1), img_h.max(1)) } else { self.oriented(img_w, img_h) };
        if turned && a != b {
            (b, a)
        } else {
            (a, b)
        }
    }

    /// The format closest to a window, and whether that window is turned
    /// against the picture's long side. Formats of one shape (4×5, 8×10
    /// and 16×20) are equally close: the whole picture is then its own
    /// format, `current` stays what it is, and otherwise the first named
    /// format is it.
    fn match_window(img_w: u32, img_h: u32, cw: u32, ch: u32, current: Self) -> (Self, bool) {
        let fit = |f: Self| {
            let (a, b) = f.frame(img_w, img_h, false);
            let (turned, gap) = ratio_turn(a, b, cw, ch);
            (f, turned, gap)
        };
        let fits = Self::ALL.map(fit);
        let image = fit(Self::Image);
        let least = fits.iter().fold(image.2, |least, f| least.min(f.2));
        let whole = cw == img_w && ch == img_h;
        let closest = || fits.iter().chain([&image]).filter(|f| f.2 == least);
        let (format, turned, _) = if whole && image.2 == least {
            image
        } else if let Some(chosen) = closest().find(|f| f.0 == current) {
            *chosen
        } else {
            closest().next().copied().unwrap_or(image)
        };
        (format, turned)
    }
}

/// How far a window is from a shape: `|cw·b − ch·a| / (cw·b + ch·a)` for
/// the window `cw`×`ch` and the shape `a`:`b`. Zero is the same shape, and
/// the number does not change with the size of either or with which side
/// is called the width, so the gaps to two shapes can be compared.
#[derive(Clone, Copy, Debug)]
struct Gap {
    num: u128,
    den: u128,
}

impl Gap {
    fn new(a: u32, b: u32, cw: u32, ch: u32) -> Self {
        let (x, y) = (u128::from(cw) * u128::from(b), u128::from(ch) * u128::from(a));
        Self { num: x.abs_diff(y), den: (x + y).max(1) }
    }
}

// Fractions are compared cross-multiplied, in whole numbers. The sides are
// pixel counts of a picture in memory, far below the 2³¹ where the product
// would no longer fit.
impl PartialEq for Gap {
    fn eq(&self, other: &Self) -> bool {
        self.cmp(other).is_eq()
    }
}
impl Eq for Gap {}
impl PartialOrd for Gap {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}
impl Ord for Gap {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.num * other.den).cmp(&(other.num * self.den))
    }
}

/// Whether `cw`:`ch` sits across `a`:`b`, and how far it is from the
/// closer of the two.
fn ratio_turn(a: u32, b: u32, cw: u32, ch: u32) -> (bool, Gap) {
    let along = Gap::new(a, b, cw, ch);
    let across = Gap::new(b, a, cw, ch);
    if a != b && across < along {
        (true, across)
    } else {
        (false, along)
    }
}

fn gcd(mut a: u32, mut b: u32) -> u32 {
    while b != 0 {
        let t = a % b;
        a = b;
        b = t;
    }
    a.max(1)
}

/// A short writing of a picture's ratio, width then height.
/// Exact when the reduced numbers are small, otherwise the nearest pair up to 24.
fn picture_terms(w: u32, h: u32) -> (u32, u32) {
    let w = w.max(1);
    let h = h.max(1);
    let g = gcd(w, h);
    let (rw, rh) = (w / g, h / g);
    if rw <= 24 && rh <= 24 {
        return (rw, rh);
    }
    let mut best_n = 1u32;
    let mut best_d = 1u32;
    let mut best_err = u64::MAX;
    for d in 1..=24 {
        let n = ((u64::from(w) * u64::from(d) + u64::from(h) / 2) / u64::from(h)).clamp(1, 24) as u32;
        let err = (u64::from(w) * u64::from(d)).abs_diff(u64::from(h) * u64::from(n));
        let closer = u128::from(err) * u128::from(best_d) < u128::from(best_err) * u128::from(d);
        if closer {
            best_n = n;
            best_d = d;
            best_err = err;
        }
    }
    let g = gcd(best_n, best_d);
    (best_n / g, best_d / g)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CropHit {
    Move,
    Corner(CropCorner),
}

/// Which file the save dialog is about to write.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ExportKind {
    /// The print: paper, size, border.
    Print,
    /// The linear negative or DNG for another program.
    Linear,
}

/// Why the recipe question is on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Leave {
    Open(PathBuf),
    Quit,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SaveChoice {
    Save,
    Discard,
    Cancel,
}

/// A negative is open and its recipe is no longer the one it was opened with.
/// Reset leaves this baseline alone, so the defaults still ask to be saved.
fn recipe_needs_saving(has_negative: bool, current: &Recipe, saved: &Recipe) -> bool {
    has_negative && current != saved
}

#[derive(Clone, Copy)]
struct CropDrag {
    hit: CropHit,
    origin: CropRect,
    /// Press position, in the upright picture's pixels.
    start_x: f32,
    start_y: f32,
}

/// What the pointer holds while the burn tool is up.
#[derive(Clone, Debug)]
enum BrushDrag {
    /// The brush is being set: a handle of the tip shown on the stage.
    Handle {
        hit: Hit,
        /// The brush at the press.
        origin: Brush,
        /// Where the tip is shown, normalised.
        centre: (f64, f64),
        /// The press, normalised.
        start: (f64, f64),
    },
    /// The hand painting: the stroke's index in the recipe and the last
    /// point kept of its path, shown-normalised.
    Paint { index: usize, last: (f64, f64) },
    /// The card pushed from the press: the stroke's index and the press.
    Push { index: usize, from: (f64, f64) },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WbChoice {
    AsShot,
    GrayWorld,
    None,
    Manual,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum HistSource {
    /// The 8-bit view as shown (after placing and the paper, or the mask).
    View,
    /// The linear mix before the curve, on a gamma-2.2 axis.
    Negative,
    /// The four sensor channels as shot (per 2x2 tile position).
    Raw,
}

/// What the main picture and the print box both render: the exposure,
/// the weights, the turn and the crop. The box's place in the picture
/// is not part of it, so a drag of the crosshair updates the box alone.
#[derive(Clone, PartialEq)]
struct SharedPicture {
    /// `Session::id`.
    session: u64,
    weights: Weights,
    tone: Tone,
    place: f64,
    rotation: u8,
    angle: f64,
    crop: Option<CropRect>,
    plate: Option<PlateView>,
}

/// The sheet painted around the picture on the stage, in texture pixels,
/// with its flat values: the print's black film edge against the picture,
/// and the paper out to the sheet's edge, which is as wide on the left as
/// on the right and as wide above as below, but not the same both ways
/// when the sheet's ratio is not the picture's. Measured against the
/// picture, so the proportion holds at any size and dpi. Zero widths are
/// no sheet.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct Rings {
    black_tex: f32,
    /// Picture edge to sheet edge, left and right.
    frame_x_tex: f32,
    /// Picture edge to sheet edge, above and below.
    frame_y_tex: f32,
    white: u8,
    black: u8,
}

/// How far the black edge is painted, in sheet pixels, before the stage
/// cuts it to the sheet. With no white border that is the whole margin:
/// a uniform inset leaves a strip beyond `black_px` on a sheet that is
/// not square, and that strip is black. With a white border it is the
/// asked `black_px`; the strip beyond it stays paper.
fn stage_black_px(border_px: usize, black_px: usize, margin_x: usize, margin_y: usize) -> usize {
    if border_px == 0 && black_px > 0 {
        margin_x.max(margin_y)
    } else {
        black_px
    }
}

impl Rings {
    /// The black edge as painted: cut where it would leave the sheet.
    fn black_shown(&self) -> Vec2 {
        Vec2::new(self.black_tex.min(self.frame_x_tex), self.black_tex.min(self.frame_y_tex))
    }

    fn frame(&self) -> Vec2 {
        Vec2::new(self.frame_x_tex, self.frame_y_tex)
    }
}

/// What is drawn over and around the picture on the stage from the edit:
/// the sheet's rings and the crosshair (the 1:1 box's spot, in picture
/// fractions). Held as one while the picture belongs to the previous file.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct StageDress {
    rings: Rings,
    cross: Option<[f64; 2]>,
}

/// What is kept of a session let go for room: enough for the recipe
/// question and the sidecar's name while the picture is still on the
/// stage and the edit is still its edit.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Released {
    path: PathBuf,
    /// A monochrome sensor: the recipe file carries no mix and no plate.
    mono: bool,
}

/// The taking filters resolved for one session, one plate choice and one
/// base mix, so the resolution runs when those change, not every frame.
struct PlateResolved {
    session: u64,
    params: PlateParams,
    base: Weights,
    weights: Result<PlateWeights, String>,
}

/// What one print box was asked with: session id, weights, plate, recipe,
/// orientation, point, box side in print pixels.
type LoupeKey = (u64, Weights, Option<PlateView>, Recipe, u16, [f64; 2], usize);

/// The primaries as the window names them.
fn space_name(space: PlateSpace) -> &'static str {
    match space {
        PlateSpace::AdobeRgb => "Adobe RGB",
        PlateSpace::Srgb => "sRGB",
    }
}

/// Whether a development of `need` bytes must have the current session's
/// memory first: it does not fit in `available` with the worker's headroom
/// to spare. `available` is what is free now, with the current session's
/// planes already taken out of it, so the session's own size is not added
/// to `need`. Unknown free memory (0) never releases anything.
fn must_make_room(need: u64, available: u64) -> bool {
    !memory_fits(need, available)
}

/// A sheet of whole pixels at `dpi`, in inches as the recipe holds it.
fn sheet_px(bw: f64, bh: f64, dpi: f64) -> PrintSize {
    PrintSize::Physical { width_in: bw / dpi, height_in: bh / dpi }
}

/// The sheet that is a `uw`×`uh` picture plus `frame` pixels of rings on
/// every side, at `dpi`. `layout` gives these pixels back exactly. Absent
/// without a picture or a dpi.
fn sheet_of_pixels_at(uw: usize, uh: usize, frame: usize, dpi: f64) -> Option<PrintSize> {
    if uw == 0 || uh == 0 || dpi <= 0.0 {
        return None;
    }
    Some(sheet_px((uw + 2 * frame) as f64, (uh + 2 * frame) as f64, dpi))
}

/// The sheet whose `side` is `typed` centimetres, to the nearest pixel at
/// `dpi`; the other side follows the picture's proportion, so the sheet
/// is always the crop's shape. Absent without a picture or a dpi.
fn sheet_for_side(uw: usize, uh: usize, dpi: f64, typed_cm: f64, side: SheetSide) -> Option<PrintSize> {
    if uw == 0 || uh == 0 || dpi <= 0.0 {
        return None;
    }
    let typed_px = (typed_cm / 2.54 * dpi).round().max(1.0);
    let (bw, bh) = match side {
        SheetSide::Width => (typed_px, (typed_px * uh as f64 / uw as f64).round().max(1.0)),
        SheetSide::Height => ((typed_px * uw as f64 / uh as f64).round().max(1.0), typed_px),
    };
    Some(sheet_px(bw, bh, dpi))
}

/// `sheet` in the proportion of a `uw`×`uh` picture, its long side held:
/// the long side goes where the picture's long side is, the short side is
/// that times the picture's ratio. Absent without a picture or a dpi.
fn sheet_following(sheet: PrintSize, uw: usize, uh: usize, dpi: f64) -> Option<PrintSize> {
    let PrintSize::Physical { width_in, height_in } = sheet;
    if uw == 0 || uh == 0 || dpi <= 0.0 {
        return None;
    }
    let long_px = (width_in.max(height_in) * dpi).round().max(1.0);
    let short_px = (long_px * uw.min(uh) as f64 / uw.max(uh) as f64).round().max(1.0);
    let (bw, bh) = if uw >= uh { (long_px, short_px) } else { (short_px, long_px) };
    Some(sheet_px(bw, bh, dpi))
}

/// `sheet` with the pixels it had at `dpi_before` kept at `dpi`: the px
/// ruler's rule when the dpi moves.
fn sheet_keeping_pixels(sheet: PrintSize, dpi_before: f64, dpi: f64) -> PrintSize {
    let (bw, bh) = sheet.pixels(dpi_before);
    sheet_px(bw as f64, bh as f64, dpi)
}

/// Millimetres that are `px` whole pixels at `dpi`; `Recipe::mm_px`
/// rounds them back to `px`.
fn mm_of_pixels(px: usize, dpi: f64) -> f64 {
    px as f64 / dpi * 25.4
}

/// Whether a development that landed is another file than the edit's:
/// the session held, else the one let go for room (`make_room`), else
/// nothing was open. The same file developed again keeps its edit and
/// its history.
fn opened_is_fresh(held: Option<&Path>, released: Option<&Path>, opened: &Path) -> bool {
    held.or(released).is_none_or(|before| before != opened)
}

/// Bytes the development of the file about to open will hold: from its
/// size when the strip has read it, else the current session's own, the
/// next file being most often from the same camera. Nothing to go by
/// without either.
fn opening_memory(target: Option<(usize, usize)>, current: Option<u64>) -> Option<u64> {
    target.map(|(w, h)| develop_memory_of_size(w, h)).or(current)
}

/// The print box has a newer exposure than the picture on screen.
fn tile_must_wait(tile_has_picture: bool, tile_matches_shown: bool) -> bool {
    tile_has_picture && !tile_matches_shown
}

/// The dress to hold while another file takes the edit: the one on the
/// stage now, if a picture is there. A dress already held stays held: the
/// stage still shows the file it was taken from.
fn dress_to_hold(
    held: Option<StageDress>,
    stage_has_picture: bool,
    now: impl FnOnce() -> StageDress,
) -> Option<StageDress> {
    held.or_else(|| stage_has_picture.then(now))
}

/// The whole desktop state: the open session, the edit and its history,
/// the folder strip, the worker handles and the per-frame UI bookkeeping.
/// `App::new` builds it; `eframe::App::ui` draws one frame.
pub struct App {
    heavy: Heavy,
    preview: Preview,
    thumbs: Thumbs,
    session: Option<Arc<Session>>,
    /// A session let go to make room for the development under way
    /// (`make_room`). The picture stays on screen as its texture, and the
    /// recipe is still its recipe until the result lands; the result
    /// decides whether it was the same file.
    released: Option<Released>,
    /// "Apply" was pressed: the re-development starts with the next frame.
    redevelop: Option<DevelopParams>,

    // Folder strip
    folder: Option<Folder>,
    folder_generation: u64,
    sort: Sort,
    show_strip: bool,
    /// Strip tiles inverted (the film), not the print: one choice per
    /// process, monochrome and plate.
    strip_negative: bool,
    strip_negative_plate: bool,
    /// The process: the three-colour plate or the monochrome negative.
    /// With a file open it is the edit's (`edit.plate`); this is what the
    /// next file without a recipe opens in, and what the switch shows
    /// with nothing open.
    plate_default: bool,
    /// The plate's filters and primaries last chosen, so switching the
    /// process back on returns to them.
    plate_last: PlateParams,
    /// Strip panel height in points; `strip_height_saved` is what the
    /// config file holds, so a drag is written once, when it ends.
    strip_height: f32,
    strip_height_saved: f32,
    /// Docked windows open: 0 Negative (left), 1 Print (right), 2 Enlarger (bottom).
    panels: [bool; 3],
    /// Open panels that had no rectangle last frame. The toolbar button
    /// stays lit; the panel returns when the window has room again.
    waiting: [bool; 3],
    /// Measured height of the enlarger window (docked at the bottom); the
    /// stage above it is laid out with it.
    enlarger_h: Option<f32>,
    /// RAW counts of the strip's subfolders, arriving in the background.
    dir_counts: Option<std::sync::mpsc::Receiver<(u64, PathBuf, usize)>>,
    /// Priority list last sent to the thumbnail worker.
    last_wanted: Vec<PathBuf>,
    /// Keyboard moved the selection: bring the tile into view once.
    scroll_to_selected: bool,
    /// Picture the strip should treat as current (the one opening or open).
    current_path: Option<PathBuf>,

    busy: Option<String>,
    log: Vec<String>,
    /// Core loads for the negative panel. Sampled only while that section is open.
    cpu: CpuMeter,

    /// The development being edited (weights, recipe, turns) and its
    /// history. The export renders exactly `edit`.
    edit: Edit,
    history: History,
    // Export section
    /// The sheet in the printer as the fields show it, centimetres. The
    /// recipe always has a sheet once a file is open; 1:1 is the sheet
    /// that is the picture's pixels, not a mode of its own.
    sheet_cm: [f64; 2],
    /// What the Print window's lengths are shown and typed in.
    ruler: Ruler,
    /// The file the Export button writes, TIFF or JPEG.
    format: ExportFormat,
    /// The export estimate the user was warned about. A second click with
    /// the same estimate goes ahead; a different one warns again.
    export_warned: Option<u64>,
    /// Memory in the machine, bytes, read once. Zero when unknown.
    ram_total: u64,
    /// Where the loupe looks, as a fraction of the upright picture.
    sharpen_at: Option<[f64; 2]>,
    /// The next click on the picture reads a grey card for the white balance.
    pick_card: bool,
    /// What the last grey card read, shown under the white-balance row.
    card_status: Option<String>,
    /// The user edited radius or amount on this file. A measurement will not
    /// replace those numbers.
    peak_held: bool,
    /// Start one measurement on the next frame. Cleared when it starts.
    peak_arm: bool,
    /// Identifies the measurement in flight. A new file advances it.
    peak_gen: u64,
    peak_measure: Option<PeakMeasure>,
    /// Radius and amount Peak measured on this negative, in negative pixels.
    /// The fields may have been moved; reset puts these back.
    peak_measured: Option<(f64, f64)>,
    /// The pointer went down on the crosshair, so this gesture moves the spot.
    cross_drag: bool,
    loupe: Loupe,
    loupe_sent: u64,
    loupe_shown: u64,
    /// Last request (session id, weights, plate, recipe, orientation, point,
    /// box side in print pixels), so an unchanged frame does not send another.
    loupe_key: Option<LoupeKey>,
    /// Box side asked for this frame, in print pixels (one screen pixel each).
    loupe_px: usize,
    /// The print window was drawn this frame, so the box is requested after the sliders.
    loupe_open: bool,
    loupe_tex: Option<TextureHandle>,
    loupe_size: [usize; 2],
    /// Exposure last sent to the preview and to the print box, and the one
    /// on screen. The box keeps its pixels until the main picture shows
    /// that exposure. A drag of the crosshair does not change it.
    preview_picture: Option<SharedPicture>,
    loupe_picture: Option<SharedPicture>,
    shown_picture: Option<SharedPicture>,
    /// Preview generation that carries the new main picture.
    picture_gen: u64,
    /// A finished print box kept back until the main picture shows its exposure.
    held_loupe: Option<LoupeResult>,
    /// The dress of the picture still on the stage while the next file's
    /// first preview is on its way. The edit is already that file's, so its
    /// rings would dress the old pixels in the new recipe's border (and
    /// refit them), and its crosshair would jump to the old picture's
    /// centre, for that moment. Taken before the edit changes, let go when
    /// the new picture lands.
    held_dress: Option<StageDress>,

    /// "1:1" button pressed; applied by the preview panel, which knows the fit scale.
    zoom_to_1to1: bool,

    // Development
    wb_choice: WbChoice,
    wb_manual: [f64; 3],
    mask_mode: MaskMode,
    /// The plate's taking filters for the current edit, or why there are none.
    plate_resolved: Option<PlateResolved>,

    // Preview
    show_mask: bool,
    texture: Option<TextureHandle>,
    tex_size: [usize; 2],
    /// Rotation the shown texture was rendered with.
    tex_rotation: u8,
    /// Fit scale (points per texel) of the last frame, the opening it was
    /// measured in, and an absolute scale the next frame must restore after
    /// the texture changed shape.
    last_fit: Option<f32>,
    last_stage: Option<Rect>,
    keep_scale: Option<f32>,
    /// Sharp window of the zoomed view and its texture. At 1:1, `step` is 1
    /// and every file pixel is here. Below that, `step` is the screen grid.
    detail_texture: Option<TextureHandle>,
    detail_rect: Option<DetailAsk>,
    /// Window the view currently wants (None when the fit preview suffices).
    wanted_detail: Option<DetailAsk>,
    last_detail_request: Option<DetailAsk>,
    /// Fitted-print recipe last sent for the 100% window. `None` is the negative.
    last_inspect: Option<Recipe>,
    generation: u64,
    shown_generation: u64,
    last_view: Option<ViewParams>,
    preview_ms: u128,
    /// How long the last detail window took, ms.
    detail_ms: u128,
    hist_view: Histogram,
    hist_negative: Histogram,
    /// The plate's three channels, outlines over the print histogram.
    hist_rgb: Option<[[u32; 256]; 3]>,
    hist_source: HistSource,
    hist_log: bool,
    zoom: f32,
    pan: Vec2,
    /// The orange easel is up. The draft is not the picture until Apply.
    crop_open: bool,
    crop_format: PaperFormat,
    /// The paper ratio stands across the picture's long side.
    crop_turned: bool,
    /// Draft window, in file-upright frame pixels (before the user's
    /// turns, after the draft straighten).
    crop_draft: CropRect,
    crop_drag: Option<CropDrag>,
    /// Draft straighten while the easel is up, degrees clockwise. The
    /// view shows its frame; Apply writes it into the recipe with the crop.
    angle_draft: f64,
    /// The straighten mode of the easel: a drag on the picture draws the
    /// line that is to be level or upright, instead of moving the window.
    straighten_on: bool,
    /// The line being drawn, in screen points.
    line_drag: Option<[Pos2; 2]>,
    /// The burn tool is up: the brush is set on the stage, or, locked,
    /// paints strokes into the recipe. The strokes apply at once.
    burn_on: bool,
    /// The brush is locked: a primary drag paints. Open, the tip stands
    /// in the middle of the stage with its handles and nothing in the
    /// picture changes.
    burn_locked: bool,
    /// The one brush, kept with the program's settings.
    brush: Brush,
    /// The brush was set and locked once, on this or an earlier run; a
    /// first use opens it instead.
    brush_set: bool,
    burn_drag: Option<BrushDrag>,
    /// The Print window's content ran past its edge, and the log has it.
    print_overflow_logged: bool,

    dialog: FileDialog,
    /// The save dialog was up when this frame began: the keys of the
    /// frame are the dialog's, the Enter that names the file included.
    dialog_up: bool,
    /// A text field had the keyboard when this frame began, which is
    /// when the last one ended.
    typing: bool,
    /// The folder the Mac sandbox is allowed to read. Held for the whole
    /// time that folder (and its subfolders) stay open.
    #[cfg(target_os = "macos")]
    folder_grant: Option<mimizan_mac::FolderGrant>,
    /// What the open save dialog will write when a name is picked.
    export_kind: ExportKind,
    cameras_dir: PathBuf,
    /// Recipe as opened, as reset, or as last saved next to the RAW.
    recipe_baseline: Recipe,
    /// A file change or a close is waiting on the save question.
    pending_leave: Option<Leave>,
    /// An export is waiting on the question whether this file, which is
    /// already there, is to be replaced.
    pending_replace: Option<PathBuf>,
    /// The close question was answered; the window may go.
    close_allowed: bool,

    /// `MIMIZAN_SCREENSHOT=<png>`: capture the window once the first preview
    /// is on screen, then quit. Used for unattended checks and the README.
    screenshot_to: Option<PathBuf>,
    screenshot_state: u8,
}

/// Zoom relative to "fit"; enough for 1:1 on a 100 MP file in a small window.
const MAX_ZOOM: f32 = 40.0;

/// "yellow-green" -> "Yellow-Green".
fn capitalise(s: &str) -> String {
    s.split('-')
        .map(|w| {
            let mut c = w.chars();
            match c.next() {
                Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join("-")
}

/// The texture of a rendered picture: the plate in colour when its bytes
/// are there and fit, else the grey.
fn colour_or_gray(size: [usize; 2], gray: &[u8], rgb: Option<&[u8]>) -> ColorImage {
    match rgb {
        Some(c) if c.len() == size[0].saturating_mul(size[1]).saturating_mul(3) => {
            ColorImage::from_rgb(size, c)
        }
        _ => ColorImage::from_gray(size, gray),
    }
}

fn resource_dir(name: &str) -> PathBuf {
    mimizan_core::calib::resolve_dir(Path::new(name))
}

impl App {
    /// Start the workers, restore the saved settings and open `initial`
    /// (a file or a folder); without one, show the last folder.
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        // The panels are always dark. Pin the preference as well, so the native
        // title bar follows. A light Windows caption would otherwise stay light.
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let cameras_dir = resource_dir("cameras");
        let persisted = Persisted::load();
        let strip_height = persisted.strip_height.unwrap_or(STRIP_HEIGHT).clamp(STRIP_MIN_H, STRIP_MAX_H);
        let mut app = Self {
            heavy: Heavy::spawn(cc.egui_ctx.clone()),
            preview: Preview::spawn(cc.egui_ctx.clone()),
            thumbs: Thumbs::spawn(cc.egui_ctx.clone(), cameras_dir.clone()),
            session: None,
            released: None,
            redevelop: None,
            folder: None,
            folder_generation: 0,
            sort: persisted.sort,
            show_strip: persisted.strip.unwrap_or(true),
            strip_negative: persisted.strip_negative.unwrap_or(true),
            strip_negative_plate: persisted.strip_negative_plate.unwrap_or(false),
            plate_default: persisted.plate.unwrap_or(false),
            plate_last: PlateParams {
                filters: persisted.plate_filters.unwrap_or_default(),
                space: persisted.plate_space.unwrap_or_default(),
            },
            strip_height,
            strip_height_saved: strip_height,
            panels: persisted.panels.unwrap_or([true; 3]),
            waiting: [false; 3],
            enlarger_h: None,
            dir_counts: None,
            last_wanted: Vec::new(),
            scroll_to_selected: false,
            current_path: None,
            busy: None,
            log: Vec::new(),
            cpu: CpuMeter::new(),
            edit: Edit::new(Weights::default()),
            history: History::new(Edit::new(Weights::default())),
            export_warned: None,
            ram_total: {
                let mut sys = sysinfo::System::new();
                sys.refresh_memory();
                sys.total_memory()
            },
            sheet_cm: [30.0, 40.0],
            ruler: persisted.ruler.unwrap_or_default(),
            format: persisted.format.unwrap_or_default(),
            sharpen_at: None,
            pick_card: false,
            card_status: None,
            peak_held: false,
            peak_arm: false,
            peak_gen: 0,
            peak_measure: None,
            peak_measured: None,
            cross_drag: false,
            loupe: Loupe::spawn(cc.egui_ctx.clone()),
            loupe_sent: 0,
            loupe_shown: 0,
            loupe_key: None,
            loupe_px: 0,
            loupe_open: false,
            loupe_tex: None,
            loupe_size: [0, 0],
            preview_picture: None,
            loupe_picture: None,
            shown_picture: None,
            picture_gen: 0,
            held_loupe: None,
            held_dress: None,
            zoom_to_1to1: false,
            wb_choice: WbChoice::AsShot,
            wb_manual: [2.0, 1.0, 1.5],
            mask_mode: DevelopParams::default().mask,
            plate_resolved: None,
            show_mask: false,
            texture: None,
            tex_size: [0, 0],
            tex_rotation: 0,
            last_fit: None,
            last_stage: None,
            keep_scale: None,
            detail_texture: None,
            detail_rect: None,
            wanted_detail: None,
            last_detail_request: None,
            last_inspect: None,
            generation: 0,
            shown_generation: 0,
            last_view: None,
            preview_ms: 0,
            detail_ms: 0,
            hist_view: Histogram::empty(),
            hist_negative: Histogram::empty(),
            hist_rgb: None,
            hist_source: HistSource::View,
            hist_log: false,
            zoom: 1.0,
            pan: Vec2::ZERO,
            crop_open: false,
            crop_format: PaperFormat::R8x10,
            crop_turned: false,
            crop_draft: CropRect { x: 0, y: 0, w: 1, h: 1 },
            crop_drag: None,
            angle_draft: 0.0,
            straighten_on: false,
            line_drag: None,
            burn_on: false,
            burn_locked: false,
            brush: persisted.brush.map(Brush::sanitized).unwrap_or_default(),
            brush_set: persisted.brush.is_some(),
            burn_drag: None,
            print_overflow_logged: false,
            // A launch from the Store has its working directory in System32.
            // The save dialog always opens on the directory set for this export.
            export_kind: ExportKind::Print,
            dialog: {
                let mut dialog = FileDialog::new();
                dialog.config_mut().opening_mode = OpeningMode::AlwaysInitialDir;
                dialog
            },
            dialog_up: false,
            typing: false,
            #[cfg(target_os = "macos")]
            folder_grant: None,
            cameras_dir,
            recipe_baseline: Recipe::on_paper(),
            pending_leave: None,
            pending_replace: None,
            close_allowed: false,
            screenshot_to: std::env::var_os("MIMIZAN_SCREENSHOT").map(PathBuf::from),
            screenshot_state: 0,
        };
        match initial {
            Some(p) if p.is_dir() => {
                app.hold_grant(&p);
                app.set_folder(&p);
            }
            Some(p) => app.open(p),
            None => app.open_remembered(&persisted),
        }
        app
    }

    fn persist(&self) {
        Persisted {
            folder: self.folder.as_ref().map(|f| f.path.clone()),
            sort: self.sort,
            strip: Some(self.show_strip),
            strip_negative: Some(self.strip_negative),
            strip_negative_plate: Some(self.strip_negative_plate),
            panels: Some(self.panels),
            strip_height: Some(self.strip_height),
            plate: Some(self.plate_default),
            plate_filters: Some(self.plate_last.filters),
            plate_space: Some(self.plate_last.space),
            ruler: Some(self.ruler),
            format: Some(self.format),
            brush: self.brush_set.then_some(self.brush),
            folder_bookmark: self.saved_bookmark(),
        }
        .save();
    }

    /// The sandbox grant, when this process is inside one. A Mac build that
    /// is not sandboxed keeps the last path and the pictures folder.
    fn open_remembered(&mut self, persisted: &Persisted) {
        #[cfg(target_os = "macos")]
        if sandboxed() {
            if let Some(bytes) = persisted.folder_bookmark.as_deref() {
                if let Some(grant) = mimizan_mac::restore(bytes) {
                    let root = grant.path().to_path_buf();
                    let open = persisted.folder.clone().filter(|f| f.starts_with(&root)).unwrap_or(root);
                    self.folder_grant = Some(grant);
                    self.set_folder(&open);
                }
            }
            return;
        }
        let last = persisted.folder.clone().filter(|f| f.is_dir());
        if let Some(f) = last.or_else(start_folder) {
            self.set_folder(&f);
        }
    }

    fn saved_bookmark(&self) -> Option<Vec<u8>> {
        #[cfg(target_os = "macos")]
        {
            self.folder_grant.as_ref().and_then(|g| g.bookmark().map(|b| b.to_vec()))
        }
        #[cfg(not(target_os = "macos"))]
        None
    }

    /// The folder the sandbox grant covers. Absent outside a sandbox, so
    /// every breadcrumb stays a button.
    fn grant_root(&self) -> Option<PathBuf> {
        #[cfg(target_os = "macos")]
        if sandboxed() {
            return self.folder_grant.as_ref().map(|g| g.path().to_path_buf());
        }
        None
    }

    /// Keep a dropped or launched folder when the system will bookmark it.
    fn hold_grant(&mut self, path: &Path) {
        #[cfg(target_os = "macos")]
        if let Some(grant) = mimizan_mac::adopt(path) {
            self.folder_grant = Some(grant);
        }
        #[cfg(not(target_os = "macos"))]
        let _ = path;
    }

    #[cfg(target_os = "macos")]
    fn choose_folder(&mut self) {
        let Some(grant) = mimizan_mac::pick_folder() else { return };
        let path = grant.path().to_path_buf();
        self.folder_grant = Some(grant);
        self.set_folder(&path);
        self.show_strip = true;
    }

    /// The process shown and switched: the edit's with a file open, the
    /// default otherwise.
    fn plate_process(&self) -> bool {
        match &self.session {
            Some(_) => self.edit.plate.is_some(),
            None => self.plate_default,
        }
    }

    /// Switch the process. With a file open the development follows (and
    /// the history records it); the default for the next file follows
    /// always. A monochrome sensor has no plate and stays as it is.
    fn set_process(&mut self, plate: bool) {
        if self.session.as_ref().is_some_and(|s| s.is_mono()) {
            return;
        }
        if let Some(p) = self.edit.plate {
            self.plate_last = p;
        }
        if self.session.is_some() {
            self.edit.plate = plate.then_some(self.plate_last);
        }
        if self.plate_default != plate {
            self.plate_default = plate;
            self.strip_textures_drop();
        }
        self.persist();
    }

    /// Whether the strip's frames are inverted in the current process.
    fn strip_inverted(&self) -> bool {
        if self.plate_process() {
            self.strip_negative_plate
        } else {
            self.strip_negative
        }
    }

    /// The strip's pictures are made again next frame (the process or the
    /// inversion changed).
    fn strip_textures_drop(&mut self) {
        if let Some(f) = &mut self.folder {
            for e in &mut f.entries {
                e.texture = None;
            }
        }
    }

    /// Keep the default and the remembered filters in step with the edit,
    /// which undo, redo and a loaded recipe change behind the switch.
    fn follow_process(&mut self) {
        if self.session.as_ref().is_some_and(|s| !s.is_mono()) {
            let plate = self.edit.plate.is_some();
            let mut changed = false;
            if let Some(p) = self.edit.plate.filter(|p| *p != self.plate_last) {
                self.plate_last = p;
                changed = true;
            }
            if plate != self.plate_default {
                self.plate_default = plate;
                self.strip_textures_drop();
                changed = true;
            }
            if changed {
                self.persist();
            }
        }
    }

    /// List a folder's RAW files in the strip. The thumbnails follow as the
    /// tiles become visible.
    fn set_folder(&mut self, path: &Path) {
        self.folder_generation += 1;
        // Absolute, so the breadcrumb shows the whole way and a relative
        // CLI argument survives a restart from another directory. The
        // Windows `\\?\` prefix is a kernel form, not a path to show.
        let path = &readable_path(path);
        match Folder::scan(path, self.sort, self.folder_generation) {
            Ok(f) => {
                self.log(format!(
                    "{}: {} RAW files, {} folders",
                    f.path.display(),
                    f.entries.len(),
                    f.subdirs.len()
                ));
                self.dir_counts = (!f.subdirs.is_empty()).then(|| {
                    browser::count_raws(f.generation, f.subdirs.iter().map(|d| d.path.clone()).collect())
                });
                self.folder = Some(f);
            }
            Err(e) => {
                self.log(format!("folder {}: {e}", path.display()));
                self.folder = None;
            }
        }
        self.last_wanted.clear();
        self.persist();
    }

    fn toggle_strip(&mut self) {
        self.show_strip = !self.show_strip;
        self.persist();
    }

    /// Open the strip's neighbour of the current picture (`step` = ±1).
    fn open_neighbour(&mut self, step: isize) {
        if self.busy.is_some() {
            return;
        }
        let Some(f) = &self.folder else { return };
        if f.entries.is_empty() {
            return;
        }
        let i = self.current_path.as_ref().and_then(|p| f.index_of(p));
        let next = match i {
            Some(i) => i as isize + step,
            None if step > 0 => 0,
            None => f.entries.len() as isize - 1,
        };
        if next < 0 || next >= f.entries.len() as isize {
            return;
        }
        let path = f.entries[next as usize].path.clone();
        self.scroll_to_selected = true;
        self.open(path);
    }

    fn log(&mut self, s: impl Into<String>) {
        let s = s.into();
        tracing::info!("{s}");
        self.log.push(s);
        if self.log.len() > 200 {
            self.log.drain(0..100);
        }
    }

    fn weights(&self) -> Weights {
        self.edit.weights
    }

    fn set_weights(&mut self, w: Weights) {
        let r = w.r.clamp(0.0, 1.0);
        let b = w.b.clamp(0.0, 1.0 - r);
        self.edit.weights = Weights { r, g: 1.0 - r - b, b };
    }

    fn rotation(&self) -> u8 {
        self.edit.rotation
    }

    /// Replace the whole development (undo, redo, a loaded recipe) and drop
    /// tool state that pointed into the old one. The edit is taken as it
    /// is, so undo and redo land exactly on the history's entries.
    fn set_edit(&mut self, e: Edit) {
        if e.rotation != self.edit.rotation {
            self.keep_scale = None;
        }
        let angle_changed = e.recipe.angle != self.edit.recipe.angle;
        self.edit = e;
        // The draft frame lay in the old angle's frame; the easel is set
        // up again on the recipe as it now stands.
        if self.crop_open && angle_changed {
            self.arm_crop();
        }
        // The stroke under the pointer may be gone from this state.
        self.burn_drag = None;
        // A recipe without a sheet (the defaults, or an older file) meant
        // the picture's pixels with the rings added outside. That file,
        // said as a sheet: the same pixels, the same TIFF.
        if self.edit.recipe.size.is_none() {
            self.edit.recipe.size = self.sheet_of_pixels(self.edit.recipe.frame_px());
        }
        self.show_sheet();
    }

    /// The sheet that is the picture's pixels plus `frame` pixels of rings
    /// on every side, at the recipe's dpi. `layout` gives these pixels
    /// back exactly. Absent until a file is open.
    fn sheet_of_pixels(&self, frame: usize) -> Option<PrintSize> {
        let (uw, uh) = self.picture_size();
        sheet_of_pixels_at(uw, uh, frame, self.edit.recipe.dpi)
    }

    /// 1:1: the sheet is the picture's pixels, no border, no black edge, no
    /// scaling. The start; a length typed from here is a sheet of its
    /// own, and a ring set from here is taken from these pixels.
    fn set_one_to_one(&mut self) {
        self.edit.recipe.border_mm = 0.0;
        self.edit.recipe.black_mm = 0.0;
        if let Some(sheet) = self.sheet_of_pixels(0) {
            self.edit.recipe.size = Some(sheet);
        }
        self.show_sheet();
    }

    /// Whether the file is the picture's pixels with nothing around them.
    fn is_one_to_one(&self) -> bool {
        let Some(l) = self.print_layout() else { return false };
        let (uw, uh) = self.picture_size();
        (l.out_w, l.out_h) == (uw, uh) && (l.image_w, l.image_h) == (uw, uh)
    }

    /// The file's size in centimetres, from the layout the recipe produces:
    /// the sheet, turned the picture's way.
    fn file_cm(&self) -> Option<[f64; 2]> {
        let l = self.print_layout()?;
        let dpi = self.edit.recipe.dpi;
        if dpi <= 0.0 {
            return None;
        }
        Some([l.out_w as f64 / dpi * 2.54, l.out_h as f64 / dpi * 2.54])
    }

    /// Put the file's size in the sheet fields.
    fn show_sheet(&mut self) {
        if let Some(cm) = self.file_cm() {
            self.sheet_cm = cm;
        }
    }

    /// Write the sheet whose `side` is the field the user typed, exactly;
    /// the other side follows the picture's proportion, so the sheet is
    /// always the crop's shape. The rings are taken from inside it, so
    /// with rings the picture sits centred with paper left over on two
    /// sides. Then show the sheet the file has.
    fn apply_sheet(&mut self, side: SheetSide) {
        let (uw, uh) = self.picture_size();
        let typed = match side {
            SheetSide::Width => self.sheet_cm[0],
            SheetSide::Height => self.sheet_cm[1],
        };
        if let Some(sheet) = sheet_for_side(uw, uh, self.edit.recipe.dpi, typed, side) {
            self.edit.recipe.size = Some(sheet);
        }
        self.show_sheet();
    }

    /// The dpi moved from `dpi_before`. With the px ruler the pixels the
    /// fields show stay and the inches follow; with cm or in the sheet
    /// keeps its size (the recipe holds inches) and the pixels follow.
    /// `rings_before` are the border and the black edge in pixels at the
    /// old dpi.
    fn follow_dpi(&mut self, dpi_before: f64, rings_before: (usize, usize)) {
        let dpi = self.edit.recipe.dpi;
        if dpi <= 0.0 || dpi_before <= 0.0 {
            return;
        }
        if self.ruler == Ruler::Pixels {
            let (border_px, black_px) = rings_before;
            self.edit.recipe.border_mm = mm_of_pixels(border_px, dpi);
            self.edit.recipe.black_mm = mm_of_pixels(black_px, dpi);
            if let Some(sheet) = self.edit.recipe.size {
                self.edit.recipe.size = Some(sheet_keeping_pixels(sheet, dpi_before, dpi));
            }
        }
        self.show_sheet();
    }

    fn undo(&mut self) {
        if let Some(e) = self.history.undo().cloned() {
            self.set_edit(e);
            self.close_crop();
        }
    }

    fn redo(&mut self) {
        if let Some(e) = self.history.redo().cloned() {
            self.set_edit(e);
            self.close_crop();
        }
    }

    /// Record the frame's state once the pointer and keyboard let go of the
    /// widgets, so a slider drag is one history step, not sixty.
    fn commit_history(&mut self, ctx: &egui::Context) {
        if ctx.egui_is_using_pointer() || text_edit_focused(ctx) {
            return;
        }
        self.history.commit(&self.edit);
    }

    fn develop_params(&self) -> DevelopParams {
        let wb = match self.wb_choice {
            WbChoice::AsShot => WhiteBalance::AsShot,
            WbChoice::GrayWorld => WhiteBalance::GrayWorld,
            WbChoice::None => WhiteBalance::None,
            WbChoice::Manual => WhiteBalance::Manual(self.wb_manual),
        };
        DevelopParams { wb, mask: self.mask_mode }
    }

    /// Read a grey card at `(nx, ny)`, a fraction of the picture on the
    /// stage, and put its balance into the manual white balance. The point
    /// goes back through the crop and the turns to the sensor, where a
    /// 32×32 photosite window is averaged per colour. Apply develops with it.
    fn read_card(&mut self, nx: f64, ny: f64) {
        let Some(s) = self.session.clone() else { return };
        if s.is_mono() {
            self.card_status = Some("a grey card needs a colour sensor".into());
            return;
        }
        // Picture on the stage: the crop window unless the easel is up.
        let (pw, ph) = if self.crop_open { self.export_size() } else { self.picture_size() };
        let (ox, oy) = match (self.crop_open, self.print_recipe().crop) {
            (false, Some(c)) => (c.x as usize, c.y as usize),
            _ => (0, 0),
        };
        if pw == 0 || ph == 0 {
            return;
        }
        let ux = ox + ((nx * pw as f64) as usize).min(pw - 1);
        let uy = oy + ((ny * ph as f64) as usize).min(ph - 1);
        // Frame point through the straighten to the upright negative, then
        // to the negative plane, then to the frame.
        let plane = &s.negative.plane;
        let orientation = self.export_orientation();
        let (tw, th) = if matches!(orientation, 5..=8) {
            (plane.height, plane.width)
        } else {
            (plane.width, plane.height)
        };
        let angle = self.view_angle();
        let (ux, uy) = if straighten::turns(angle) {
            let (u, v) = straighten::frame_to_upright(tw, th, angle).apply(ux as f64 + 0.5, uy as f64 + 0.5);
            (
                (u - 0.5).round().clamp(0.0, tw.saturating_sub(1) as f64) as usize,
                (v - 0.5).round().clamp(0.0, th.saturating_sub(1) as f64) as usize,
            )
        } else {
            (ux, uy)
        };
        let spot = crate::session::source_rect(
            orientation,
            plane.width,
            plane.height,
            crate::session::DetailRect { x: ux, y: uy, w: 1, h: 1 },
        );
        let crop = s.negative.info.report.crop;
        let centre = (crop.x + spot.x, crop.y + spot.y);
        let params = mimizan_core::ingest::IngestParams::default();
        match mimizan_core::ingest::card_balance(&s.frame, &params, centre, 16) {
            Ok(r) => {
                self.wb_manual = r.wb;
                self.wb_choice = WbChoice::Manual;
                let [mr, mg, mb] = r.mean;
                let mut status = format!(
                    "card at {}, {} · R {:.3} G {:.3} B {:.3} · {} of {} clipped",
                    centre.0, centre.1, mr, mg, mb, r.clipped, r.total
                );
                if r.clipped > 0 {
                    status.push_str(" · clipped sites left out: a dimmer spot reads truer");
                } else if mg < 0.02 {
                    status.push_str(" · dark: the reading is noisy");
                }
                self.log(format!("grey card: R {:.3} G 1 B {:.3} ({})", r.wb[0], r.wb[2], status));
                self.card_status = Some(status);
            }
            Err(e) => {
                let msg = format!("grey card: {e}");
                self.log(msg.clone());
                self.card_status = Some(msg);
            }
        }
    }

    fn view_params(&self) -> ViewParams {
        let r = &self.edit.recipe;
        ViewParams {
            weights: self.weights(),
            tone: r.curve.clone(),
            place: r.place,
            show_mask: self.show_mask,
            rotation: self.edit.rotation,
            angle: self.view_angle(),
            // While the easel is up the whole negative is shown.
            crop: if self.crop_open { None } else { r.crop },
            burns: r.burns.clone(),
            burn_live: matches!(self.burn_drag, Some(BrushDrag::Paint { .. } | BrushDrag::Push { .. })),
            plate: self.plate_view(),
        }
    }

    /// Resolve the plate's taking filters for the current session, choice
    /// and base mix, once per change. Runs at the top of every frame.
    fn refresh_plate(&mut self) {
        let (Some(s), Some(params)) = (self.session.clone(), self.edit.plate) else {
            self.plate_resolved = None;
            return;
        };
        if s.is_mono() {
            self.plate_resolved = None;
            return;
        }
        // The emulsion behind the Wratten glass is the file's, not the mix.
        let base = s.default_weights();
        let same = self
            .plate_resolved
            .as_ref()
            .is_some_and(|r| r.session == s.id && r.params == params && r.base == base);
        if same {
            return;
        }
        // The camera set without a matrix would log a warning on every
        // resolution; the panel says so instead, and the channels are taken.
        let mut asked = params;
        if asked.filters == PlateFilters::Camera && !s.has_colour_matrix() {
            asked.filters = PlateFilters::Channels;
        }
        let weights = s.plate_weights(&asked, base).map_err(|e| e.to_string());
        if let Err(e) = &weights {
            self.log(format!("plate: {e}"));
        }
        self.plate_resolved = Some(PlateResolved { session: s.id, params, base, weights });
    }

    /// The plate the view, the box and the export render, when it is on
    /// and its filters resolved.
    fn plate_view(&self) -> Option<PlateView> {
        let params = self.edit.plate?;
        let r = self.plate_resolved.as_ref()?;
        let weights = r.weights.as_ref().ok()?;
        Some(PlateView { weights: *weights, space: params.space, lamps: self.edit.lamps_rgb() })
    }

    /// Orientation the export applies: the file's plus the user's turns.
    fn export_orientation(&self) -> u16 {
        let base = self.session.as_ref().map(|s| s.negative.orientation).unwrap_or(1);
        mimizan_core::print::rotated(base, self.rotation())
    }

    /// The straighten the stage shows: the draft while the easel is up,
    /// the recipe's otherwise.
    fn view_angle(&self) -> f64 {
        if self.crop_open {
            self.angle_draft
        } else {
            self.edit.recipe.angle
        }
    }

    /// The picture after the straighten by `angle`: the frame of the
    /// file-upright negative, before the user's turns. The whole negative
    /// at zero.
    fn frame_at(&self, angle: f64) -> (u32, u32) {
        let Some(s) = &self.session else { return (0, 0) };
        let (w, h) = s.frame_size(angle);
        (w as u32, h as u32)
    }

    /// The frame at `angle`, upright, after the user's turns.
    fn export_size_at(&self, angle: f64) -> (usize, usize) {
        let (w, h) = self.frame_at(angle);
        let (w, h) = (w as usize, h as usize);
        if self.rotation() % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// Size of the whole picture on the stage, upright, after the user's
    /// turns: the frame at the view's straighten.
    fn export_size(&self) -> (usize, usize) {
        self.export_size_at(self.view_angle())
    }

    /// File-upright frame at the view's straighten, before the user's turns.
    fn file_size(&self) -> (u32, u32) {
        self.frame_at(self.view_angle())
    }

    /// The recipe the print box uses. While the easel is up the box is the
    /// whole negative, which is what the view shows.
    fn loupe_recipe(&self) -> Recipe {
        let mut recipe = self.print_recipe();
        if self.crop_open {
            recipe.crop = None;
            recipe.angle = self.angle_draft;
        }
        recipe
    }

    /// The recipe with its window and its burns turned into the current
    /// upright picture, the window clamped to it (a sidecar from another
    /// file may be larger).
    fn print_recipe(&self) -> Recipe {
        let mut recipe = self.edit.recipe.clone();
        if let Some(c) = recipe.crop {
            let (fw, fh) = self.frame_at(recipe.angle);
            let (uw, uh) = self.export_size_at(recipe.angle);
            recipe.crop = Some(c.turned(self.rotation(), fw, fh).sanitized(uw as u32, uh as u32));
        }
        if !recipe.burns.is_empty() {
            recipe.burns = burn::turned(&recipe.burns, self.rotation());
        }
        recipe
    }

    /// The window the print is made from (upright, after the user's turns).
    fn picture_size(&self) -> (usize, usize) {
        let (w, h) = self.export_size_at(self.edit.recipe.angle);
        match self.print_recipe().crop {
            Some(c) => (c.w as usize, c.h as usize),
            None => (w, h),
        }
    }

    /// The print box's layout: the whole negative while the easel is up.
    fn loupe_layout(&self) -> Option<mimizan_core::print::Layout> {
        // `layout` takes the whole negative and applies the recipe's
        // straighten itself; the frame is not handed in twice.
        let (w, h) = self.export_size_at(0.0);
        if w == 0 || h == 0 {
            return None;
        }
        let params = mimizan_core::print::PrintParams { recipe: self.loupe_recipe(), ..Default::default() };
        Some(mimizan_core::print::layout(w, h, 1, &params))
    }

    /// Output size of the print TIFF (after size step and border).
    fn print_layout(&self) -> Option<mimizan_core::print::Layout> {
        // The whole negative: `layout` applies the straighten and the crop.
        let (w, h) = self.export_size_at(0.0);
        if w == 0 || h == 0 {
            return None;
        }
        let params = mimizan_core::print::PrintParams { recipe: self.print_recipe(), ..Default::default() };
        // Already upright: orientation 1.
        Some(mimizan_core::print::layout(w, h, 1, &params))
    }

    /// The rings the edit asks for around the texture on the stage. Flat
    /// values, painted under the picture, not rendered into it. The mask
    /// view and the open crop are not a sheet. Only meaningful while the
    /// texture is this session's picture: `held_dress` covers the moment
    /// it is not.
    fn rings(&self) -> Rings {
        let [tw, _] = self.tex_size;
        if self.show_mask || self.crop_open || tw == 0 {
            return Rings::default();
        }
        let Some(l) = self.print_layout() else { return Rings::default() };
        let scale = tw as f32 / l.image_w.max(1) as f32;
        let tone = self.edit.recipe.curve.compile();
        Rings {
            black_tex: stage_black_px(l.border_px, l.black_px, l.picture_x, l.picture_y) as f32 * scale,
            frame_x_tex: l.picture_x as f32 * scale,
            frame_y_tex: l.picture_y as f32 * scale,
            white: (tone.white().clamp(0.0, 1.0) * 255.0).round() as u8,
            black: (tone.eval(0.0).clamp(0.0, 1.0) * 255.0).round() as u8,
        }
    }

    /// The dress the edit asks for: its rings and its crosshair.
    fn dress(&self) -> StageDress {
        StageDress { rings: self.rings(), cross: self.sharpen_at }
    }

    /// The dress to paint now: the held one while the stage still shows
    /// the previous file, otherwise the edit's.
    fn stage_dress(&self) -> StageDress {
        self.held_dress.unwrap_or_else(|| self.dress())
    }

    /// Whether the texture on the stage is this session's picture: no
    /// dress is held over a previous file's pixels, and the preview that
    /// was shown last came from this session.
    fn stage_shows_session(&self) -> bool {
        let Some(s) = &self.session else { return false };
        self.held_dress.is_none() && self.shown_picture.as_ref().is_some_and(|p| p.session == s.id)
    }

    /// Another file is about to take the edit while this one's picture
    /// stays on the stage: keep its dress until the new picture lands. A
    /// third file opened before that keeps the dress already held; the
    /// edit's would belong to the second.
    fn hold_dress(&mut self) {
        self.held_dress = dress_to_hold(self.held_dress, self.texture.is_some(), || self.dress());
    }

    /// Pixel size the 100% view addresses. A centimetre size addresses the
    /// fitted print; otherwise the negative (the crop, when one is committed).
    fn inspect_size(&self) -> (usize, usize) {
        if self.crop_open {
            return self.export_size();
        }
        if !self.show_mask {
            if let Some(lay) = self.print_layout() {
                return (lay.image_w, lay.image_h);
            }
        }
        self.picture_size()
    }

    /// The print recipe when 100% is a window of the fitted picture.
    /// `None` keeps 100% on the negative's own pixels.
    fn inspect_recipe(&self) -> Option<Recipe> {
        if self.session.is_none() || self.crop_open || self.show_mask {
            return None;
        }
        let recipe = self.print_recipe();
        recipe.size?;
        let (nw, nh) = self.picture_size();
        let lay = self.print_layout()?;
        if lay.image_w == nw && lay.image_h == nh {
            None
        } else {
            Some(recipe)
        }
    }

    fn close_crop(&mut self) {
        self.crop_open = false;
        self.crop_drag = None;
        self.line_drag = None;
        self.straighten_on = false;
    }

    /// Crop on: the whole frame at the recipe's straighten, with the
    /// committed window or a fresh 8×10.
    fn arm_crop(&mut self) {
        self.angle_draft = self.edit.recipe.angle;
        let (fw, fh) = self.frame_at(self.angle_draft);
        if fw == 0 || fh == 0 {
            return;
        }
        self.line_drag = None;
        // The easel shows the whole frame; the brush works on the window.
        self.close_burn();
        if let Some(c) = self.edit.recipe.crop {
            let c = c.sanitized(fw, fh);
            let (format, turned) = PaperFormat::match_window(fw, fh, c.w, c.h, self.crop_format);
            self.crop_format = format;
            self.crop_turned = turned;
            self.crop_draft = c;
        } else {
            let (a, b) = self.crop_format.frame(fw, fh, self.crop_turned);
            self.crop_draft = CropRect::inscribed(fw, fh, a, b);
        }
        self.crop_open = true;
        self.crop_drag = None;
        self.cross_drag = false;
        self.sharpen_at = Some([0.5, 0.5]);
        self.loupe_key = None;
        self.loupe_tex = None;
    }

    fn reinscribe(&mut self) {
        let (fw, fh) = self.file_size();
        if fw == 0 || fh == 0 {
            return;
        }
        let (a, b) = self.crop_format.frame(fw, fh, self.crop_turned);
        self.crop_draft = CropRect::inscribed(fw, fh, a, b);
        self.crop_drag = None;
    }

    /// Apply: the straighten and the window go into the recipe together,
    /// one step of the history.
    fn apply_crop(&mut self) {
        let (fw, fh) = self.file_size();
        if fw == 0 || fh == 0 {
            return;
        }
        let was_one_to_one = self.is_one_to_one();
        self.edit.recipe.angle = self.angle_draft;
        self.edit.recipe.crop = Some(self.crop_draft.sanitized(fw, fh));
        self.close_crop();
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.sharpen_at = Some([0.5, 0.5]);
        self.loupe_key = None;
        self.loupe_tex = None;
        self.cross_drag = false;
        // A new window is a new picture, and the sheet takes its
        // proportion. No sheet set yet (1:1 lit): the window's pixels,
        // still 1:1. A sheet set: its long side stays, the short side
        // follows the crop.
        if was_one_to_one {
            self.set_one_to_one();
        } else {
            self.sheet_follows_crop();
        }
    }

    /// The sheet in the crop's proportion, its long side held: the long
    /// side goes where the picture's long side is, the short side is that
    /// times the picture's ratio.
    fn sheet_follows_crop(&mut self) {
        let (uw, uh) = self.picture_size();
        let Some(sheet) = self.edit.recipe.size else { return };
        if let Some(followed) = sheet_following(sheet, uw, uh, self.edit.recipe.dpi) {
            self.edit.recipe.size = Some(followed);
        }
        self.show_sheet();
    }

    /// A new draft straighten. The frame changes with it, and the draft
    /// window follows: the same place and share of the frame, its format
    /// kept. Whole frame stays whole frame.
    fn set_draft_angle(&mut self, angle: f64) {
        if !self.crop_open {
            return;
        }
        let angle = if angle.is_finite() {
            ((angle * 100.0).round() / 100.0).clamp(-straighten::ANGLE_LIMIT, straighten::ANGLE_LIMIT)
        } else {
            0.0
        };
        let old = self.file_size();
        self.angle_draft = angle;
        let new = self.file_size();
        if old.0 == 0 || old.1 == 0 || new.0 == 0 || new.1 == 0 {
            return;
        }
        let aspect = self.crop_format.frame(new.0, new.1, self.crop_turned);
        self.crop_draft = scaled_draft(self.crop_draft, old, new, aspect);
        self.crop_drag = None;
        self.loupe_key = None;
        self.loupe_tex = None;
    }

    /// The line drawn in straighten mode is let go: the turn that puts it
    /// level or upright is added to the draft.
    fn finish_line(&mut self, [a, b]: [Pos2; 2]) {
        if let Some(delta) = straighten::angle_of_line(a.x as f64, a.y as f64, b.x as f64, b.y as f64) {
            if a.distance(b) >= 8.0 {
                self.set_draft_angle(self.angle_draft + delta);
            }
        }
    }

    /// Draft window in the picture currently on screen, and that picture's size.
    fn turned_window(&self) -> Option<(CropRect, u32, u32)> {
        if !self.crop_open {
            return None;
        }
        let (fw, fh) = self.file_size();
        if fw == 0 || fh == 0 {
            return None;
        }
        let q = self.rotation() % 4;
        let (uw, uh) = if q % 2 == 1 { (fh, fw) } else { (fw, fh) };
        Some((self.crop_draft.turned(q, fw, fh).sanitized(uw, uh), uw, uh))
    }

    fn store_turned(&mut self, shown: CropRect) {
        let (fw, fh) = self.file_size();
        let q = self.rotation() % 4;
        let (uw, uh) = if q % 2 == 1 { (fh, fw) } else { (fw, fh) };
        let back = (4 - q) % 4;
        self.crop_draft = shown.turned(back, uw, uh).sanitized(fw, fh);
    }

    fn crop_interact(&mut self, ui: &egui::Ui, response: &egui::Response, rect: Rect) {
        // The preview senses clicks as well as drags, so `dragged()` stays false
        // until the pointer has moved a few points. Hitting the frame only then
        // misses it, and the picture pans instead. A press inside the frame owns the
        // gesture from that frame.
        let down_on = response.is_pointer_button_down_on();
        if !down_on {
            self.crop_drag = None;
        }
        if self.straighten_on {
            self.crop_drag = None;
            self.line_interact(ui, response, rect);
            return;
        }
        self.line_drag = None;
        let Some((shown, uw, uh)) = self.turned_window() else { return };
        let Some(ptr) = ui.input(|i| i.pointer.latest_pos()) else { return };
        if rect.width() <= 1.0 || rect.height() <= 1.0 || uw == 0 || uh == 0 {
            return;
        }
        let (px, py) = image_pos(ptr, rect, uw, uh);
        let px_per_pt = uw as f32 / rect.width();
        let (corner_slop, edge_slop) = (10.0 * px_per_pt, 8.0 * px_per_pt);
        if down_on && ui.input(|i| i.pointer.primary_pressed()) {
            let at = ui.input(|i| i.pointer.press_origin()).unwrap_or(ptr);
            let (sx, sy) = image_pos(at, rect, uw, uh);
            if let Some(hit) = hit_crop(sx, sy, shown, corner_slop, edge_slop) {
                self.crop_drag = Some(CropDrag { hit, origin: shown, start_x: sx, start_y: sy });
            }
        }
        if let Some(drag) = self.crop_drag {
            let (aw, ah) = self.crop_format.frame(uw, uh, self.crop_turned);
            let dx = (px - drag.start_x).round() as i32;
            let dy = (py - drag.start_y).round() as i32;
            self.store_turned(moved_crop(drag.origin, drag.hit, dx, dy, uw, uh, (aw, ah)));
            ui.ctx().set_cursor_icon(crop_cursor(drag.hit, true));
        } else if let Some(hit) = hit_crop(px, py, shown, corner_slop, edge_slop) {
            response.clone().on_hover_cursor(crop_cursor(hit, false));
        }
    }

    /// Straighten mode: a press on the picture starts the line, the drag
    /// draws it, the release turns the draft so the line lies level or
    /// upright.
    fn line_interact(&mut self, ui: &egui::Ui, response: &egui::Response, rect: Rect) {
        let down_on = response.is_pointer_button_down_on();
        let ptr = ui.input(|i| i.pointer.latest_pos());
        if down_on && ui.input(|i| i.pointer.primary_pressed()) {
            let at = ui.input(|i| i.pointer.press_origin()).or(ptr);
            if let Some(at) = at {
                if rect.contains(at) {
                    self.line_drag = Some([at, at]);
                }
            }
        }
        match (self.line_drag, down_on) {
            (Some([a, _]), true) => {
                if let Some(p) = ptr {
                    self.line_drag = Some([a, p]);
                }
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
            }
            (Some(line), false) => {
                self.line_drag = None;
                self.finish_line(line);
            }
            (None, _) => {
                if response.hover_pos().is_some_and(|p| rect.contains(p)) {
                    ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
                }
            }
        }
    }

    /// The line being drawn in straighten mode.
    fn paint_line(&self, painter: &egui::Painter) {
        let Some([a, b]) = self.line_drag else { return };
        let ink = Color32::from_rgb(255, 210, 60);
        painter.line_segment([a, b], egui::Stroke::new(1.5, ink));
        for p in [a, b] {
            painter.circle_filled(p, 3.5, ink);
        }
        if let Some(d) = straighten::angle_of_line(a.x as f64, a.y as f64, b.x as f64, b.y as f64) {
            if a.distance(b) >= 8.0 {
                let txt = format!("{:+.1}°", self.angle_draft + d);
                let galley = painter.layout_no_wrap(txt, egui::FontId::proportional(13.0), ink);
                let pos = b + Vec2::new(10.0, -galley.size().y / 2.0);
                painter.rect_filled(
                    Rect::from_min_size(pos, galley.size()).expand(3.0),
                    3.0,
                    Color32::from_black_alpha(150),
                );
                painter.galley(pos, galley, ink);
            }
        }
    }

    fn paint_crop(&self, painter: &egui::Painter, rect: Rect) {
        let Some((shown, uw, uh)) = self.turned_window() else { return };
        if uw == 0 || uh == 0 || rect.width() <= 0.0 || rect.height() <= 0.0 {
            return;
        }
        let x = rect.left() + shown.x as f32 / uw as f32 * rect.width();
        let y = rect.top() + shown.y as f32 / uh as f32 * rect.height();
        let w = shown.w as f32 / uw as f32 * rect.width();
        let h = shown.h as f32 / uh as f32 * rect.height();
        let frame = Rect::from_min_size(Pos2::new(x, y), Vec2::new(w, h));
        let dim = Color32::from_black_alpha(150);
        let (left, right, top, bottom) = (rect.left(), rect.right(), rect.top(), rect.bottom());
        let fl = frame.left().clamp(left, right);
        let fr = frame.right().clamp(left, right);
        let ft = frame.top().clamp(top, bottom);
        let fb = frame.bottom().clamp(top, bottom);
        painter.rect_filled(Rect::from_min_max(Pos2::new(left, top), Pos2::new(fl, bottom)), 0.0, dim);
        painter.rect_filled(Rect::from_min_max(Pos2::new(fr, top), Pos2::new(right, bottom)), 0.0, dim);
        painter.rect_filled(Rect::from_min_max(Pos2::new(fl, top), Pos2::new(fr, ft)), 0.0, dim);
        painter.rect_filled(Rect::from_min_max(Pos2::new(fl, fb), Pos2::new(fr, bottom)), 0.0, dim);
        let orange = Color32::from_rgb(232, 120, 24);
        painter.rect_stroke(frame, 0.0, egui::Stroke::new(2.0, orange), egui::StrokeKind::Inside);
        for p in [frame.left_top(), frame.right_top(), frame.right_bottom(), frame.left_bottom()] {
            painter.rect_filled(Rect::from_center_size(p, Vec2::splat(12.0)), 1.0, orange);
        }
    }

    // ---- The burn tool: the brush over the picture ------------------------

    /// Burn on. The easel goes down: the strokes are painted on the
    /// picture as it prints, window and all. A brush that was set before
    /// comes up locked, ready to paint; a fresh one comes up open.
    fn arm_burn(&mut self) {
        if self.crop_open {
            self.close_crop();
        }
        self.burn_on = true;
        self.burn_drag = None;
        self.burn_locked = self.brush_set;
    }

    fn close_burn(&mut self) {
        self.burn_on = false;
        self.burn_drag = None;
    }

    /// The brush locked: it is now the tool, and it is kept.
    fn lock_brush(&mut self) {
        self.burn_locked = true;
        self.burn_drag = None;
        self.brush_set = true;
        self.persist();
    }

    fn open_brush(&mut self) {
        self.burn_locked = false;
        self.burn_drag = None;
    }

    /// From a point of the picture on the stage (0..1 across and down it)
    /// to the shown picture's normalised coordinates, and back. The
    /// picture is the window, through the straighten, onto the upright
    /// negative after the user's turns: the same chain the render reads
    /// the strokes through.
    fn burn_map(&self) -> Option<(straighten::Affine, straighten::Affine)> {
        let (pw, ph) = self.picture_size();
        let (tw, th) = self.export_size_at(0.0);
        if pw == 0 || ph == 0 || tw == 0 || th == 0 {
            return None;
        }
        let (ox, oy) = match self.print_recipe().crop {
            Some(c) => (f64::from(c.x), f64::from(c.y)),
            None => (0.0, 0.0),
        };
        let unit_to_window =
            straighten::Affine { a: pw as f64, b: 0.0, c: 0.0, d: ph as f64, tx: ox, ty: oy };
        let to_norm = burn::normalize(tw, th)
            .after(&straighten::frame_to_upright(tw, th, self.edit.recipe.angle))
            .after(&unit_to_window);
        let back = to_norm.inverse()?;
        Some((to_norm, back))
    }

    /// A stroke as shown, stored file-upright.
    fn store_shown_stroke(&mut self, index: usize, s: Stroke) {
        let back = (4 - self.rotation() % 4) % 4;
        if let Some(slot) = self.edit.recipe.burns.get_mut(index) {
            *slot = s.turned(back);
        }
    }

    /// One stroke of the recipe as shown, turned into the picture on view.
    fn shown_stroke(&self, index: usize) -> Option<Stroke> {
        self.edit.recipe.burns.get(index).map(|s| s.turned(self.rotation()))
    }

    /// Where the open brush stands: the middle of the picture on view.
    fn brush_centre(&self, rect: Rect, to_norm: &straighten::Affine) -> (f64, f64) {
        let shown = self.shown_stage(rect);
        brush::stage_to_norm(shown.center(), rect, to_norm)
    }

    /// The part of the picture's `rect` that is on the stage.
    fn shown_stage(&self, rect: Rect) -> Rect {
        let shown = self.last_stage.map_or(rect, |stage| rect.intersect(stage));
        if shown.is_positive() {
            shown
        } else {
            rect
        }
    }

    /// The three lights under the open brush, in screen points: the row
    /// kept on the stage, so it can be read with a large tip or one near
    /// an edge.
    fn light_row(
        &self,
        rect: Rect,
        centre: (f64, f64),
        k: f64,
        back: &straighten::Affine,
    ) -> [(Light, Pos2); 3] {
        let row = brush::lights(&self.brush, centre.0, centre.1, k);
        let mut at = row.map(|(l, x, y)| (l, brush::norm_to_stage(x, y, rect, back)));
        let shown = self.shown_stage(rect);
        let (hw, hh) = (brush::LIGHT_SIZE_PT.0 / 2.0, brush::LIGHT_SIZE_PT.1 / 2.0);
        let (left, right) = (at[0].1.x - hw, at[2].1.x + hw);
        let dx = (shown.left() + 4.0 - left).max(0.0) + (shown.right() - 4.0 - right).min(0.0);
        let dy = (shown.top() + 4.0 - (at[0].1.y - hh)).max(0.0)
            + (shown.bottom() - 4.0 - (at[0].1.y + hh)).min(0.0);
        for (_, p) in &mut at {
            *p += Vec2::new(dx, dy);
        }
        at
    }

    /// The light under a press at `p`, in the open brush's row.
    fn light_at(
        &self,
        rect: Rect,
        centre: (f64, f64),
        k: f64,
        back: &straighten::Affine,
        p: Pos2,
    ) -> Option<Light> {
        let size = Vec2::new(brush::LIGHT_SIZE_PT.0, brush::LIGHT_SIZE_PT.1);
        self.light_row(rect, centre, k, back)
            .into_iter()
            .find(|(_, c)| Rect::from_center_size(*c, size).contains(p))
            .map(|(l, _)| l)
    }

    /// The brush's light chosen on the stage; kept with a brush that is kept.
    fn choose_light(&mut self, light: Light) {
        if self.brush.light() != light {
            self.brush.set_light(light);
            if self.brush_set {
                self.persist();
            }
        }
    }

    /// The burn tool on the stage. Open, a press on a handle of the tip
    /// sets the brush, a click on a light chooses it, and nothing else
    /// moves. Locked, a primary press
    /// starts a stroke: the hand's path follows the pointer, the card is
    /// pushed from the press to where the pointer lets go. Every change
    /// goes straight into the recipe, so the preview follows the hand;
    /// the release is one step of the history.
    fn burn_interact(&mut self, ui: &egui::Ui, response: &egui::Response, rect: Rect) {
        let down_on = response.is_pointer_button_down_on();
        if rect.width() <= 1.0 || rect.height() <= 1.0 {
            self.burn_drag = None;
            return;
        }
        let Some((to_norm, back)) = self.burn_map() else {
            self.burn_drag = None;
            return;
        };
        let k = brush::points_per_unit(rect, &back);
        if k <= 0.0 || k.is_nan() {
            return;
        }
        let slop = f64::from(brush::HANDLE_PT) / k;
        let ptr = ui.input(|i| i.pointer.latest_pos());
        let centre = self.brush_centre(rect, &to_norm);
        if !down_on {
            self.burn_drag = None;
            if let Some(p) = response.hover_pos().filter(|p| rect.contains(*p)) {
                let icon = if self.burn_locked {
                    CursorIcon::None
                } else {
                    let (x, y) = brush::stage_to_norm(p, rect, &to_norm);
                    match brush::hit(&self.brush, centre.0, centre.1, k, x, y, slop) {
                        Some(h) => brush_cursor(h, false),
                        None if self.light_at(rect, centre, k, &back, p).is_some() => {
                            CursorIcon::PointingHand
                        }
                        None => CursorIcon::Default,
                    }
                };
                ui.ctx().set_cursor_icon(icon);
            }
            return;
        }
        let Some(ptr) = ptr else { return };
        if self.burn_drag.is_none() && ui.input(|i| i.pointer.primary_pressed()) {
            let at = ui.input(|i| i.pointer.press_origin()).unwrap_or(ptr);
            if !rect.contains(at) {
                return;
            }
            let (x, y) = brush::stage_to_norm(at, rect, &to_norm);
            if !self.burn_locked {
                if let Some(hit) = brush::hit(&self.brush, centre.0, centre.1, k, x, y, slop) {
                    self.burn_drag =
                        Some(BrushDrag::Handle { hit, origin: self.brush, centre, start: (x, y) });
                } else if let Some(light) = self.light_at(rect, centre, k, &back, at) {
                    // A click, not a drag: the press is used up here.
                    self.choose_light(light);
                    return;
                }
            } else if self.edit.recipe.burns.len() < burn::MAX_STROKES {
                let index = self.edit.recipe.burns.len();
                let (x, y) = (brush::clamp_reach(x), brush::clamp_reach(y));
                let fresh = match self.brush.tool {
                    Tool::Hand => self.brush.hand_stroke(vec![[x, y]]),
                    Tool::Card => self.brush.card_stroke((x, y), (x, y)),
                };
                self.edit.recipe.burns.push(fresh.clone());
                self.store_shown_stroke(index, fresh);
                self.burn_drag = Some(match self.brush.tool {
                    Tool::Hand => BrushDrag::Paint { index, last: (x, y) },
                    Tool::Card => BrushDrag::Push { index, from: (x, y) },
                });
            }
        }
        let Some(d) = self.burn_drag.clone() else { return };
        let (x, y) = brush::stage_to_norm(ptr, rect, &to_norm);
        match d {
            BrushDrag::Handle { hit, origin, centre, start } => {
                let fine = if ui.input(|i| i.modifiers.shift) { 0.1 } else { 1.0 };
                self.brush = brush::dragged(&origin, hit, centre, start, (x, y), k, fine);
                ui.ctx().set_cursor_icon(brush_cursor(hit, true));
            }
            BrushDrag::Paint { index, last } => {
                ui.ctx().set_cursor_icon(CursorIcon::None);
                let Some(Stroke::Hand { mut points, .. }) = self.shown_stroke(index) else {
                    self.burn_drag = None;
                    return;
                };
                let (x, y) = (brush::clamp_reach(x), brush::clamp_reach(y));
                if (x - last.0).hypot(y - last.1) >= self.brush.spacing() && points.len() < burn::MAX_POINTS {
                    points.push([x, y]);
                    let stroke = self.brush.hand_stroke(points);
                    self.store_shown_stroke(index, stroke);
                    self.burn_drag = Some(BrushDrag::Paint { index, last: (x, y) });
                }
            }
            BrushDrag::Push { index, from } => {
                ui.ctx().set_cursor_icon(CursorIcon::None);
                if index >= self.edit.recipe.burns.len() {
                    self.burn_drag = None;
                    return;
                }
                let stroke = self.brush.card_stroke(from, (brush::clamp_reach(x), brush::clamp_reach(y)));
                self.store_shown_stroke(index, stroke);
            }
        }
    }

    /// The brush over the picture. Open: the tip in the middle of the
    /// stage with its handles, its penumbra dashed, its stops at the
    /// centre, the three lights in a row beneath it. Locked: the tip under the pointer, the stroke being made,
    /// and with Alt held the paths of every stroke.
    fn paint_burns(&self, ui: &egui::Ui, painter: &egui::Painter, response: &egui::Response, rect: Rect) {
        let Some((to_norm, back)) = self.burn_map() else { return };
        let k = brush::points_per_unit(rect, &back);
        if k <= 0.0 || k.is_nan() {
            return;
        }
        let painter = painter.with_clip_rect(painter.clip_rect().intersect(rect));
        let at = |x: f64, y: f64| brush::norm_to_stage(x, y, rect, &back);
        let b = &self.brush;
        let ink = brush::ink(b.stops());
        let stroke = egui::Stroke::new(1.5, ink);
        let faint = egui::Stroke::new(1.0, ink.gamma_multiply(0.6));
        // The tip at normalised (cx, cy): outline and penumbra.
        let tip_at = |cx: f64, cy: f64| match b.tool {
            Tool::Hand => {
                let tip = &b.hand.tip;
                painter.add(egui::Shape::closed_line(brush::tip_outline(tip, cx, cy, 0.0, &at), stroke));
                if tip.feather > 0.0 {
                    for off in [-tip.feather, tip.feather] {
                        let mut pts = brush::tip_outline(tip, cx, cy, off, &at);
                        pts.push(pts[0]);
                        painter.extend(egui::Shape::dashed_line(&pts, faint, 4.0, 4.0));
                    }
                }
            }
            Tool::Card => {
                let (s, c) = b.card.angle.to_radians().sin_cos();
                let (dx, dy) = (-s, c);
                let reach = 2.0 * burn::REACH;
                let line = |off: f64| {
                    [
                        at(cx + c * off - dx * reach, cy + s * off - dy * reach),
                        at(cx + c * off + dx * reach, cy + s * off + dy * reach),
                    ]
                };
                painter.line_segment(line(0.0), stroke);
                if b.card.feather > 0.0 {
                    for off in [-b.card.feather, b.card.feather] {
                        painter.extend(egui::Shape::dashed_line(&line(off), faint, 4.0, 4.0));
                    }
                }
                // Ticks into the lit side, every 24 points along the edge.
                let step = 24.0 / k;
                let tick = 8.0 / k;
                let n = (reach / step).ceil() as i32;
                for j in -n..=n {
                    let (px, py) = (cx + dx * step * j as f64, cy + dy * step * j as f64);
                    painter.line_segment([at(px, py), at(px + c * tick, py + s * tick)], faint);
                }
            }
        };
        if !self.burn_locked {
            let (cx, cy) = self.brush_centre(rect, &to_norm);
            tip_at(cx, cy);
            for (hit, hx, hy) in brush::handles(b, cx, cy, k) {
                let p = at(hx, hy);
                match hit {
                    Hit::Rotate(_) => {
                        // A stalk from the tip to the turn handle.
                        let (sx, sy) = match b.tool {
                            Tool::Hand => {
                                let (s, c) = b.hand.tip.angle.to_radians().sin_cos();
                                (cx + b.hand.tip.rx * c, cy + b.hand.tip.rx * s)
                            }
                            Tool::Card => (cx, cy),
                        };
                        painter.line_segment([at(sx, sy), p], faint);
                        painter.circle_stroke(p, 5.0, egui::Stroke::new(1.5, ink));
                    }
                    Hit::Feather => {
                        painter.rect_filled(Rect::from_center_size(p, Vec2::splat(8.0)), 1.0, ink);
                    }
                    Hit::Stops => {
                        brush_label(&painter, p, &brush::label(b.stops(), b.light()), ink, 13.0);
                    }
                    Hit::Axis(_) => {
                        painter.circle_filled(p, 4.5, ink);
                    }
                }
            }
            // The light, chosen here: both passes, or one alone.
            let chosen = b.light();
            for (light, p) in self.light_row(rect, (cx, cy), k, &back) {
                brush_choice(&painter, p, light.name(), ink, light == chosen);
            }
            return;
        }
        // Locked: the strokes' paths while Alt is held, and the card being
        // pushed. A hand being painted shows only in the picture.
        let show_paths = ui.input(|i| i.modifiers.alt);
        let pushing = match &self.burn_drag {
            Some(BrushDrag::Push { index, .. }) => Some(*index),
            _ => None,
        };
        for (i, s) in self.edit.recipe.burns.iter().enumerate() {
            if !show_paths && pushing != Some(i) {
                continue;
            }
            let s = s.turned(self.rotation());
            let ink = brush::ink(s.stops());
            let path = egui::Stroke::new(1.5, ink.gamma_multiply(if pushing == Some(i) { 1.0 } else { 0.8 }));
            match &s {
                Stroke::Hand { points, .. } => {
                    let pts: Vec<Pos2> = points.iter().map(|&[x, y]| at(x, y)).collect();
                    if pts.len() == 1 {
                        painter.circle_filled(pts[0], 3.0, path.color);
                    } else {
                        painter.add(egui::Shape::line(pts.clone(), path));
                    }
                    if let Some(p) = pts.first() {
                        painter.circle_stroke(*p, 4.0, path);
                    }
                }
                Stroke::Card { x, y, angle, travel, .. } => {
                    let (s, c) = angle.to_radians().sin_cos();
                    let half = 40.0 / k;
                    let edge = |px: f64, py: f64| {
                        [at(px + s * half, py - c * half), at(px - s * half, py + c * half)]
                    };
                    painter.line_segment(edge(*x, *y), path);
                    let (ex, ey) = if *travel > 0.0 {
                        (x + c * travel, y + s * travel)
                    } else {
                        (x + c * 12.0 / k, y + s * 12.0 / k)
                    };
                    if *travel > 0.0 {
                        painter.line_segment(edge(ex, ey), path);
                    }
                    painter.arrow(at(*x, *y), at(ex, ey) - at(*x, *y), path);
                }
            }
        }
        // The tip under the pointer.
        if let Some(p) = response.hover_pos().filter(|p| rect.contains(*p)) {
            let (cx, cy) = brush::stage_to_norm(p, rect, &to_norm);
            tip_at(cx, cy);
            brush_label(&painter, p + Vec2::new(14.0, 14.0), &brush::label(b.stops(), b.light()), ink, 11.0);
        }
    }

    fn start_export(&mut self) {
        let stem = self
            .session
            .as_ref()
            .and_then(|s| s.path.file_stem().map(|f| f.to_string_lossy().to_string()))
            .unwrap_or_else(|| "mimizan".into());
        let name = format!("{stem}.{}", self.format.extension());
        self.export_kind = ExportKind::Print;
        if self.save_with_panel(&name) {
            return;
        }
        if let Some(dir) = self.export_directory() {
            self.dialog.config_mut().initial_directory = dir;
        }
        self.dialog.config_mut().default_file_name = name;
        self.dialog.save_file();
    }

    /// The linear export's dialog, beside the RAW, under
    /// [`linear_export_name`].
    fn start_linear_export(&mut self) {
        if let Some(dir) = self.export_directory() {
            self.dialog.config_mut().initial_directory = dir;
        }
        let name = linear_export_name(self.session.as_ref().map(|s| s.path.as_path()), self.linear_is_dng());
        self.export_kind = ExportKind::Linear;
        if self.save_with_panel(&name) {
            return;
        }
        self.dialog.config_mut().default_file_name = name;
        self.dialog.save_file();
    }

    /// The sandboxed Mac build writes through the system save dialog, which
    /// is the grant for that file. Anywhere else the egui dialog stays.
    /// `true` means the system dialog already ran (including a cancel).
    fn save_with_panel(&mut self, name: &str) -> bool {
        #[cfg(target_os = "macos")]
        if sandboxed() {
            // The panel appends the extension itself, so the path it hands
            // back is written as it is: the grant is for that path alone.
            let extensions = export_extensions(self.export_kind, self.linear_is_dng(), self.format);
            if let Some(path) = mimizan_mac::pick_save(self.export_directory().as_deref(), name, extensions) {
                self.export_to(path, true);
            }
            return true;
        }
        #[cfg(not(target_os = "macos"))]
        let _ = name;
        false
    }

    /// A path came from a save dialog. `granted` is a path from the system
    /// save panel: it is written as it is, extension and all, because the
    /// sandbox allows that file and no other. The egui dialog's path gets
    /// the extension it may lack, and with it the name may be a file the
    /// dialog never asked about: that one waits for a yes. The RAW itself
    /// is never the target.
    fn export_to(&mut self, path: PathBuf, granted: bool) {
        let Some(source) = self.session.as_ref().map(|s| s.path.clone()) else { return };
        let extensions = export_extensions(self.export_kind, self.linear_is_dng(), self.format);
        match export_target(path, granted, extensions, &source) {
            ExportTarget::Write(path) => self.export_now(path),
            ExportTarget::Ask(path) => self.pending_replace = Some(path),
            ExportTarget::Source(path) => self.log(format!(
                "not exported: {} is the RAW this picture is developed from. Give the export another name",
                path.display()
            )),
        }
    }

    /// The question [`ExportTarget::Ask`] waits on.
    fn replace_prompt(&mut self, ctx: &egui::Context) {
        let Some(path) = self.pending_replace.clone() else { return };
        let name =
            path.file_name().map_or_else(|| path.display().to_string(), |n| n.to_string_lossy().to_string());
        let mut answer = None;
        egui::Modal::new(egui::Id::new("replace_export")).show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.label(format!("{name} is already there. Replace it?"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Replace").clicked() {
                    answer = Some(true);
                }
                if ui.button("Cancel").clicked() {
                    answer = Some(false);
                }
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                answer = Some(false);
            }
        });
        if let Some(replace) = answer {
            self.pending_replace = None;
            if replace {
                self.export_now(path);
            }
        }
    }

    /// A question or the save dialog is on screen and has the keyboard:
    /// no key reaches the picture or the strip behind it. An arrow in the
    /// dialog's list would open the next file, and the print of that one
    /// would be saved under this one's name.
    /// This frame's keys are not the shortcuts': a question is up, or a
    /// text field has the keyboard or had it when the frame began. The
    /// Enter or Escape that ends typing in a number is still in the input
    /// after the field has given the keyboard back, and would otherwise
    /// keep the crop or put the brush down as well.
    fn keys_are_taken(&self, ctx: &egui::Context) -> bool {
        self.question_open() || self.typing || text_edit_focused(ctx)
    }

    fn question_open(&self) -> bool {
        self.pending_leave.is_some() || self.pending_replace.is_some() || self.dialog_up
    }

    /// Start the export to `path`, which is settled.
    fn export_now(&mut self, path: PathBuf) {
        if self.session.is_none() {
            return;
        }
        // The linear negative writes its mask beside it, a file no dialog
        // named. The sandbox allows that only inside the folder grant.
        if self.export_kind == ExportKind::Linear && !self.linear_is_dng() && !self.may_write_beside(&path) {
            self.log(format!(
                "the mask beside {} may not be written there: the program has no right to that folder, \
                 only to the file named. Save inside the folder opened through Open for the mask",
                path.display()
            ));
        }
        let Some(s) = &self.session else { return };
        match self.export_kind {
            ExportKind::Print => {
                self.busy = Some("exporting".into());
                self.heavy.send(Job::Export {
                    session: Arc::clone(s),
                    weights: self.weights(),
                    plate: self.plate_view(),
                    recipe: self.print_recipe(),
                    path,
                    orientation: self.export_orientation(),
                });
            }
            ExportKind::Linear => {
                let plate = self.linear_is_dng();
                self.busy = Some("exporting".into());
                let recipe = self.print_recipe();
                self.heavy.send(Job::ExportLinear {
                    session: Arc::clone(s),
                    weights: self.weights(),
                    plate,
                    framing: mimizan_core::linear::Framing {
                        orientation: self.export_orientation(),
                        angle: recipe.angle,
                        crop: recipe.crop,
                    },
                    path,
                });
            }
        }
    }

    /// ", straightened +1.20°" for the hover texts; nothing at zero.
    fn straighten_text(&self) -> String {
        let a = self.edit.recipe.angle;
        if straighten::turns(a) {
            format!(", straightened {a:+.2}°")
        } else {
            String::new()
        }
    }

    /// The linear export is a DNG in the plate process of a Bayer file.
    fn linear_is_dng(&self) -> bool {
        self.edit.plate.is_some() && self.session.as_ref().is_some_and(|s| !s.is_mono())
    }

    /// The TIFF belongs next to the RAW. With no file open, the strip's
    /// folder is the place. The process working directory is not used.
    fn export_directory(&self) -> Option<PathBuf> {
        let beside_the_file =
            self.session.as_ref().and_then(|s| s.path.parent()).filter(|p| p.is_dir()).map(Path::to_path_buf);
        beside_the_file.or_else(|| self.folder.as_ref().map(|f| f.path.clone()).filter(|p| p.is_dir()))
    }

    fn open(&mut self, path: PathBuf) {
        // Absolute like the strip's entries, so the tile lights up.
        let path = readable_path(&path);
        if self.session.as_ref().is_some_and(|s| s.path == path) {
            return;
        }
        if self.should_ask_to_save() {
            self.pending_leave = Some(Leave::Open(path));
            return;
        }
        self.start_open(path);
    }

    fn start_open(&mut self, path: PathBuf) {
        self.busy = Some(format!("opening {}", path.display()));
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            if self.folder.as_ref().is_none_or(|f| f.path != parent) {
                self.set_folder(parent);
            }
        }
        self.current_path = Some(path.clone());
        // The size the strip read for this file, when it has.
        let target = self
            .folder
            .as_ref()
            .and_then(|f| f.entries.iter().find(|e| e.path == path))
            .and_then(|e| e.meta.as_ref())
            .map(|m| (m.width, m.height));
        let current = self.session.as_ref().map(|s| develop_memory(&s.frame));
        if let Some(need) = opening_memory(target, current) {
            self.make_room(need);
        }
        self.heavy.send(Job::Open {
            path,
            develop: self.develop_params(),
            cameras_dir: self.cameras_dir.clone(),
        });
    }

    /// Let the session go before a development of `need` bytes when that
    /// would not fit in the free memory beside it. The worker checks the
    /// real frame once it is decoded, and sends the job back if it still
    /// does not fit (`Event::Refused`). The picture stays on screen as its
    /// texture, dressed as it was, until the new one arrives; the `Opened`
    /// event finds the path in `released`.
    fn make_room(&mut self, need: u64) {
        if self.session.is_none() {
            return;
        }
        let available = available_memory();
        if !must_make_room(need, available) {
            return;
        }
        self.release_session(format!(
            "to make room: a development holds about {}, {} is free",
            memory_text(need),
            memory_text(available)
        ));
    }

    /// Drop the session so its planes are freed, keeping its picture on
    /// the stage with the rings and the crosshair it has now. `why`
    /// completes "<file> let go ". A peak measurement still running holds
    /// a clone of the session until it ends, so the memory is not back
    /// before that; the log says so instead of claiming it.
    fn release_session(&mut self, why: String) {
        let Some(s) = self.session.as_ref() else { return };
        let (path, mono) = (s.path.clone(), s.is_mono());
        let name = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
        if self.peak_measure.is_some() {
            self.log(format!(
                "{name} let go {why}; the sharpening measurement still holds it until it finishes"
            ));
        } else {
            self.log(format!("{name} let go {why}"));
        }
        // The dress first, while the session can still measure the rings.
        self.hold_dress();
        self.released = Some(Released { path, mono });
        self.session = None;
    }

    /// The file the edit belongs to: the session's, or the one let go for
    /// room while its development is under way.
    fn developed_path(&self) -> Option<&Path> {
        match (&self.session, &self.released) {
            (Some(s), _) => Some(&s.path),
            (None, Some(r)) => Some(&r.path),
            (None, None) => None,
        }
    }

    /// Whether the file the edit belongs to is a monochrome sensor.
    fn developed_is_mono(&self) -> bool {
        match (&self.session, &self.released) {
            (Some(s), _) => s.is_mono(),
            (None, Some(r)) => r.mono,
            (None, None) => false,
        }
    }

    /// The re-development "Apply" asked for, sent now that no panel holds
    /// a clone of the session.
    fn start_redevelop(&mut self) {
        let Some(develop) = self.redevelop.take() else { return };
        let Some(source) = self.session.as_ref().map(|s| Redevelop::of(s)) else {
            self.busy = None;
            return;
        };
        let need = develop_memory(&source.frame);
        self.make_room(need);
        self.heavy.send(Job::Redevelop { source, develop });
    }

    /// The worker could not hold the development. With the session still
    /// here its planes are what stands in the way: let it go and send the
    /// job once more. Already gone, there is nothing left to free, and the
    /// message is the answer.
    fn retry_refused(&mut self, job: Box<Job>, message: String) {
        if self.session.is_some() && job.developed_path().is_some() {
            self.release_session("after the worker refused: ".to_string() + &message);
            let name = job
                .developed_path()
                .and_then(|p| p.file_name())
                .map(|f| f.to_string_lossy().to_string())
                .unwrap_or_default();
            self.busy = Some(format!("opening {name} again"));
            self.heavy.send(*job);
            return;
        }
        // Nothing of this program's is left to free.
        self.log(format!("error: {message}. Close other programs and open it again"));
        self.busy = None;
        self.picture_gone_if_released();
        self.current_path = self.session.as_ref().map(|s| s.path.clone());
    }

    /// A picture let go for room is gone with its texture once the job it
    /// made room for has failed.
    fn picture_gone_if_released(&mut self) {
        if self.released.take().is_some() {
            self.texture = None;
            self.detail_texture = None;
            self.detail_rect = None;
            self.held_dress = None;
        }
    }

    /// Controls back to the session's defaults; the negative stays. A new
    /// file starts a new history; on the same file the reset is one step.
    fn reset(&mut self, fresh: bool) {
        self.export_warned = None;
        let weights = self.session.as_ref().map(|s| s.default_weights()).unwrap_or_default();
        let mut e = Edit::new(weights);
        // The process is a mode, not a control: a reset keeps it, and a
        // fresh file opens in the process the darkroom is in.
        if self.session.as_ref().is_some_and(|s| !s.is_mono()) && self.plate_default {
            e.plate = Some(self.plate_last);
        }
        // The edit first: `set_edit` gives the defaults their sheet (the
        // picture's pixels, 1:1), and a fresh history starts on that.
        self.set_edit(e);
        if fresh {
            self.history = History::new(self.edit.clone());
            // A pipette armed on the previous file is not meant for this one.
            self.pick_card = false;
            self.card_status = None;
        }
        // The baseline stays the recipe the file was opened with. Reset is a
        // different development, so closing or opening another file still asks.
        // The file next to the RAW is written only when that question is saved.
        self.tex_rotation = 0;
        self.keep_scale = None;
        self.show_mask = false;
        // Drop a measurement that is still running. Fresh opens arm a new
        // one after this. A reset keeps the default radius and amount, and
        // finishes a measurement that has not landed yet so the print reset
        // still has a target. One that already landed stays.
        self.peak_measure = None;
        self.peak_arm =
            !fresh && self.peak_measured.is_none() && self.session.is_some() && self.screenshot_to.is_none();
        self.peak_held = true;
        self.peak_gen = self.peak_gen.wrapping_add(1);
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.close_crop();
        self.close_burn();
        // The easel's own state goes with the development: the draft
        // straighten and the chosen sheet, so the toolbar says what the
        // picture is.
        self.angle_draft = 0.0;
        self.crop_format = PaperFormat::R8x10;
        self.crop_turned = false;
    }

    /// Recipe file next to the RAW: `<name>.recipe.json`.
    fn recipe_sidecar(&self) -> Option<PathBuf> {
        let path = self.developed_path()?;
        let stem = path.file_stem()?.to_string_lossy().to_string();
        Some(path.with_file_name(format!("{stem}.recipe.json")))
    }

    /// Read a recipe file into the edit. The sharpening mode stays off (the
    /// desktop has no control for it); peak in the file is kept. An older
    /// recipe with the neutral look is put on paper; any other look stays a
    /// look (the Enlarger offers the paper instead of the bar).
    fn load_recipe(&mut self, path: &Path) {
        match Recipe::load(path) {
            Ok(mut r) => {
                r.sharpen = Sharpen::Off;
                if let Tone::Look(l) = &r.curve {
                    if mimizan_core::curve::Curve::from_look(l).is_identity() {
                        r.curve = Tone::paper();
                    } else {
                        self.log(format!("recipe prints through the look \"{}\", not on paper", l.name));
                    }
                }
                let mut e = self.edit.clone();
                // A monochrome sensor has no mix and no plate; the file's
                // entries, if any, are dropped.
                if self.session.as_ref().is_some_and(|s| s.is_mono()) {
                    r.mix = None;
                    r.plate = None;
                }
                e.take_recipe(r);
                self.set_edit(e);
                self.follow_process();
                self.close_crop();
                self.note_peak_source();
                self.log(format!("loaded recipe {}", path.display()));
            }
            Err(e) => self.log(format!("recipe not loaded: {e}")),
        }
    }

    /// The recipe as the file beside the RAW gets it: print, mix, plate.
    fn recipe_for_file(&self) -> Recipe {
        let bayer = self.developed_path().is_some() && !self.developed_is_mono();
        self.edit.recipe_for_file(bayer)
    }

    fn save_recipe(&mut self, path: &Path) {
        let r = self.recipe_for_file();
        match r.validate().and_then(|_| r.save(path)) {
            Ok(()) => {
                self.log(format!("saved recipe {}", path.display()));
                if self.recipe_sidecar().as_deref() == Some(path) {
                    self.recipe_baseline = r;
                }
            }
            // The last log line is on the toolbar: a refused write says
            // where and why, not only that it failed. On a sandboxed Mac
            // a RAW dropped or opened on its own brings no right to its
            // folder; the Open button does.
            Err(mimizan_core::Error::Io(e)) if e.kind() == std::io::ErrorKind::PermissionDenied => {
                self.log(format!(
                    "recipe not saved: no permission to write in {}{}",
                    path.parent().unwrap_or(path).display(),
                    if sandboxed() { " (open the folder through Open to allow it)" } else { "" }
                ));
            }
            Err(e) => self.log(format!("recipe not saved: {e}")),
        }
    }

    /// Whether the program may write beside the file: inside the sandbox
    /// only within the folder grant; elsewhere always, as far as this can
    /// tell without trying.
    fn may_write_beside(&self, path: &Path) -> bool {
        match self.grant_root() {
            Some(root) => path.starts_with(&root),
            None => !sandboxed(),
        }
    }

    /// A session let go for room still has its recipe on the edit, so the
    /// question is asked for it as for one that is held.
    fn should_ask_to_save(&self) -> bool {
        self.screenshot_to.is_none()
            && recipe_needs_saving(
                self.developed_path().is_some(),
                &self.recipe_for_file(),
                &self.recipe_baseline,
            )
    }

    /// The window was asked to close. A changed recipe stays until the
    /// question is answered. An unattended screenshot is never asked.
    fn guard_close(&mut self, ctx: &egui::Context) {
        let requested = ctx.input(|i| i.viewport().close_requested());
        if !requested || self.close_allowed || self.screenshot_to.is_some() {
            return;
        }
        if self.should_ask_to_save() {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            self.pending_leave = Some(Leave::Quit);
        }
    }

    fn follow_leave(&mut self, ctx: &egui::Context, leave: Leave) {
        match leave {
            Leave::Open(path) => self.start_open(path),
            Leave::Quit => {
                self.close_allowed = true;
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }

    fn answer_save(&mut self, ctx: &egui::Context, leave: Leave, choice: SaveChoice) {
        match choice {
            SaveChoice::Cancel => self.pending_leave = None,
            SaveChoice::Discard => {
                self.pending_leave = None;
                self.follow_leave(ctx, leave);
            }
            SaveChoice::Save => {
                let Some(path) = self.recipe_sidecar() else {
                    self.log("recipe not saved: no negative open");
                    return;
                };
                self.save_recipe(&path);
                if self.recipe_for_file() == self.recipe_baseline {
                    self.pending_leave = None;
                    self.follow_leave(ctx, leave);
                }
            }
        }
    }

    fn save_prompt(&mut self, ctx: &egui::Context) {
        let Some(leave) = self.pending_leave.clone() else { return };
        let when = match &leave {
            Leave::Quit => "before closing",
            Leave::Open(_) => "before opening another negative",
        };
        let mut choice = None;
        // Inside the sandbox a RAW that came on its own has no folder the
        // program may write in: the button says so instead of failing.
        let writable = self.recipe_sidecar().is_some_and(|p| self.may_write_beside(&p));
        egui::Modal::new(egui::Id::new("save_recipe")).show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.label(format!("Save the recipe next to the RAW {when}?"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui
                    .add_enabled(writable, egui::Button::new("Save"))
                    .on_disabled_hover_text(
                        "The program may not write in this file's folder. Open the folder through \
                         Open, then save.",
                    )
                    .clicked()
                {
                    choice = Some(SaveChoice::Save);
                }
                if ui.button("Don't save").clicked() {
                    choice = Some(SaveChoice::Discard);
                }
                if ui.button("Cancel").clicked() {
                    choice = Some(SaveChoice::Cancel);
                }
            });
            if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                choice = Some(SaveChoice::Cancel);
            }
        });
        if let Some(choice) = choice {
            self.answer_save(ctx, leave, choice);
        }
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Some(ev) = self.heavy.poll() {
            match ev {
                Event::Status(s) => self.busy = Some(s),
                Event::Opened(s) => {
                    let released = self.released.take();
                    let fresh = opened_is_fresh(
                        self.session.as_ref().map(|old| old.path.as_path()),
                        released.as_ref().map(|r| r.path.as_path()),
                        &s.path,
                    );
                    if fresh {
                        // Before the session and the edit are the new file's:
                        // the rings are measured against the old session's size.
                        self.hold_dress();
                        // A preview of the old file still on its way is not
                        // this file's picture: it must not land on the stage
                        // and let the rings go. The next request is numbered
                        // above every one sent so far.
                        self.picture_gen = self.generation + 1;
                    }
                    let t = &s.negative.info.timing;
                    let name =
                        s.path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
                    self.log(if s.is_mono() {
                        format!(
                            "{name}: decode {} ms, ingest {} ms, {}x{}",
                            t.decode_ms, t.ingest_ms, s.negative.plane.width, s.negative.plane.height
                        )
                    } else {
                        format!(
                            "{name}: decode {} ms, ingest {} ms, separate {} ms ({} rounds), {}x{}",
                            t.decode_ms,
                            t.ingest_ms,
                            t.separate_ms,
                            s.negative.info.rounds,
                            s.negative.plane.width,
                            s.negative.plane.height
                        )
                    });
                    self.session = Some(s);
                    if fresh {
                        self.cross_drag = false;
                        self.sharpen_at = Some([0.5, 0.5]);
                        self.loupe_key = None;
                        self.loupe_tex = None;
                        self.loupe_size = [0, 0];
                        self.reset(true);
                        // A recipe saved next to the file comes back with it.
                        if let Some(p) = self.recipe_sidecar().filter(|p| p.is_file()) {
                            self.load_recipe(&p);
                            self.history = History::new(self.edit.clone());
                        }
                        if self.screenshot_to.is_some()
                            && std::env::var_os("MIMIZAN_SCREENSHOT_ZONES").is_some()
                        {
                            // Moved exposures in the unattended capture.
                            self.edit.recipe.curve = Tone::Paper(SplitGrade::new(0.25, -0.5));
                        }
                        if let Some(mm) = self
                            .screenshot_to
                            .as_ref()
                            .and_then(|_| std::env::var("MIMIZAN_SCREENSHOT_BORDER").ok())
                            .and_then(|v| v.parse::<f64>().ok())
                        {
                            // A paper border in the unattended capture,
                            // around the whole picture: the sheet is the
                            // pixels plus the border.
                            self.edit.recipe.border_mm = mm.clamp(0.0, 100.0);
                            self.edit.recipe.size = self.sheet_of_pixels(self.edit.recipe.frame_px());
                        }
                        if let Some(filters) = self
                            .screenshot_to
                            .as_ref()
                            .and_then(|_| std::env::var("MIMIZAN_SCREENSHOT_PLATE").ok())
                            .and_then(|v| PlateFilters::parse(&v))
                        {
                            // The plate in the unattended capture.
                            self.edit.plate = Some(PlateParams { filters, ..PlateParams::default() });
                            self.follow_process();
                        }
                        if let Some(which) =
                            self.screenshot_to.as_ref().and(std::env::var_os("MIMIZAN_SCREENSHOT_BURN"))
                        {
                            // Two strokes in the unattended capture, the brush
                            // open on the stage: a dodge of the hand in the
                            // lower left, a burn of the card pushed into the sky.
                            // `=card` opens the card instead of the hand.
                            if which == "card" {
                                self.brush.tool = Tool::Card;
                            }
                            let mut dodge = self.brush;
                            dodge.hand.stops = -0.5;
                            let mut sky = self.brush;
                            sky.card.stops = 1.0;
                            sky.card.light = Light::Soft;
                            self.edit.recipe.burns = vec![
                                dodge.hand_stroke(vec![[-0.3, 0.12], [-0.22, 0.16], [-0.12, 0.14]]),
                                sky.card_stroke((0.0, -0.1), (0.0, -0.25)),
                            ];
                            self.arm_burn();
                            self.burn_locked = false;
                        }
                        self.recipe_baseline = self.recipe_for_file();
                    }
                    // A re-development has new pixels: the measured radius
                    // belongs to the old ones.
                    self.note_peak_source();
                    self.busy = None;
                    self.last_view = None;
                }
                Event::Exported { path, width, height, ms } => {
                    self.log(format!("wrote {} ({width}x{height}) in {ms} ms", path.display()));
                    self.busy = None;
                }
                Event::Refused { job, message } => self.retry_refused(job, message),
                Event::Failed(e) => {
                    self.log(format!("error: {e}"));
                    self.busy = None;
                    // A failed open leaves the previous picture on the stage;
                    // the strip must point at that one, not at the failure.
                    // A picture let go for room is gone with its texture.
                    self.picture_gone_if_released();
                    self.current_path = self.session.as_ref().map(|s| s.path.clone());
                }
            }
        }
        if let Some(r) = self.loupe.poll() {
            if r.generation == self.loupe_sent {
                self.accept_loupe(ctx, r);
            }
        }
        self.poll_peak();
        for r in self.thumbs.poll() {
            let Some(f) = &mut self.folder else { continue };
            if r.generation != f.generation {
                continue;
            }
            if let Some(e) = f.entries.iter_mut().find(|e| e.path == r.path) {
                match r.result {
                    Ok((thumb, meta)) => {
                        tracing::debug!("thumbnail {} in {} ms", e.name, r.ms);
                        e.thumb = Some(thumb);
                        e.meta = Some(meta);
                        e.state = ThumbState::Ready;
                        e.texture = None;
                    }
                    Err(msg) => {
                        tracing::warn!("thumbnail {}: {msg}", e.name);
                        e.state = ThumbState::Failed;
                    }
                }
            }
        }
        for r in self.preview.poll() {
            if r.generation < self.shown_generation {
                continue;
            }
            self.shown_generation = r.generation;
            if let Some(msg) = r.failed {
                self.log(format!("preview failed: {msg}"));
                continue;
            }
            self.accept_preview(ctx, r);
        }
        let dropped: Vec<PathBuf> =
            ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).collect());
        if let Some(p) = dropped.into_iter().next() {
            if p.is_dir() {
                self.hold_grant(&p);
                self.set_folder(&p);
                self.show_strip = true;
            } else if self.busy.is_none() {
                self.open(p);
            }
        }
        self.screenshot_step(ctx);
    }

    /// `MIMIZAN_SCREENSHOT_STRIP`: the capture waits for the strip's
    /// frames, so the thumbnails are in the picture.
    fn strip_thumbs_settled(&self) -> bool {
        self.folder.as_ref().is_none_or(|f| f.entries.iter().all(|e| e.state != ThumbState::Pending))
    }

    fn screenshot_step(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot_to.clone() else { return };
        match self.screenshot_state {
            // Wait until a preview is on screen, or until the open has
            // failed or there was nothing to open: the capture then shows
            // the empty stage instead of never happening.
            0 if self.busy.is_none()
                && (self.texture.is_some() || self.session.is_none())
                && (std::env::var_os("MIMIZAN_SCREENSHOT_STRIP").is_none()
                    || self.strip_thumbs_settled()) =>
            {
                if std::env::var_os("MIMIZAN_SCREENSHOT_1TO1").is_some() {
                    self.zoom_to_1to1 = true;
                    self.screenshot_state = 4;
                } else {
                    self.screenshot_state = 1;
                }
                ctx.request_repaint();
            }
            // 1:1 requested: wait for the full-resolution window to arrive.
            4 if self.detail_rect.is_some() && self.wanted_detail == self.detail_rect => {
                self.screenshot_state = 1;
                ctx.request_repaint();
            }
            4 => ctx.request_repaint(),
            // One extra frame so the texture is painted before the capture.
            1 => {
                self.screenshot_state = 2;
                ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
            }
            2 => {
                let shot = ctx.input(|i| {
                    i.events.iter().find_map(|e| match e {
                        egui::Event::Screenshot { image, .. } => Some(Arc::clone(image)),
                        _ => None,
                    })
                });
                if let Some(img) = shot {
                    let [w, h] = img.size;
                    // `MIMIZAN_SCREENSHOT_WIDTH`: scaled to that width for
                    // a document. A `.jpg` has no alpha.
                    let width =
                        std::env::var("MIMIZAN_SCREENSHOT_WIDTH").ok().and_then(|v| v.parse::<u32>().ok());
                    let res = image::RgbaImage::from_raw(w as u32, h as u32, img.as_raw().to_vec())
                        .ok_or_else(|| anyhow::anyhow!("screenshot buffer"))
                        .and_then(|im| {
                            let mut im = image::DynamicImage::ImageRgba8(im);
                            if let Some(tw) = width.filter(|tw| *tw > 0 && *tw < im.width()) {
                                let th =
                                    (u64::from(im.height()) * u64::from(tw) / u64::from(im.width())) as u32;
                                im = im.resize_exact(tw, th.max(1), image::imageops::FilterType::Lanczos3);
                            }
                            if path.extension().is_some_and(|e| e.eq_ignore_ascii_case("jpg")) {
                                im = image::DynamicImage::ImageRgb8(im.to_rgb8());
                            }
                            im.save(&path).map_err(Into::into)
                        });
                    match res {
                        Ok(()) => tracing::info!("screenshot {}", path.display()),
                        Err(e) => tracing::error!("screenshot: {e}"),
                    }
                    self.screenshot_state = 3;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
            _ => {}
        }
    }

    /// The exposure both pictures are made from. The crosshair is absent,
    /// so moving the box does not change this.
    fn shared_picture(&self) -> Option<SharedPicture> {
        let session = self.session.as_ref()?;
        let r = &self.edit.recipe;
        Some(SharedPicture {
            session: session.id,
            weights: self.weights(),
            tone: r.curve.clone(),
            place: r.place,
            rotation: self.edit.rotation,
            angle: self.view_angle(),
            crop: if self.crop_open { None } else { r.crop },
            plate: self.plate_view(),
        })
    }

    fn accept_preview(&mut self, ctx: &egui::Context, r: PreviewResult) {
        // A newer exposure is already on its way. This result belongs to the previous one.
        if self.picture_gen > 0 && r.generation < self.picture_gen {
            return;
        }
        let shows_picture = r.image.is_some();
        self.apply_preview(ctx, r);
        if shows_picture {
            // The stage shows this session's pixels: the edit's dress fits them.
            self.held_dress = None;
            self.shown_picture = self.preview_picture.clone();
            self.flush_loupe(ctx);
        }
    }

    fn accept_loupe(&mut self, ctx: &egui::Context, r: LoupeResult) {
        let matches = self.loupe_picture == self.shown_picture;
        if tile_must_wait(self.loupe_picture.is_some(), matches) {
            self.held_loupe = Some(r);
        } else {
            self.apply_loupe(ctx, r);
        }
    }

    /// The box finished before the main picture. Show it once that picture is up.
    fn flush_loupe(&mut self, ctx: &egui::Context) {
        let Some(r) = self.held_loupe.take() else { return };
        if r.generation != self.loupe_sent {
            return;
        }
        if self.loupe_picture == self.shown_picture {
            self.apply_loupe(ctx, r);
        } else {
            self.held_loupe = Some(r);
        }
    }

    fn apply_preview(&mut self, ctx: &egui::Context, r: PreviewResult) {
        if r.image.is_some() {
            self.preview_ms = r.ms;
        } else if r.detail.is_some() {
            self.detail_ms = r.ms;
        }
        if let Some(im) = r.image {
            // A turned picture keeps its magnification and its centre:
            // turn the pan with it, and let the panel re-derive the zoom
            // (which is relative to "fit") from the absolute scale.
            let turns = (4 + im.rotation - self.tex_rotation) % 4;
            for _ in 0..turns {
                self.pan = Vec2::new(-self.pan.y, self.pan.x);
            }
            if turns % 2 == 1 {
                if let Some(f) = self.last_fit {
                    self.keep_scale = Some(f * self.zoom);
                }
            }
            self.tex_rotation = im.rotation;
            if im.width > 0 && im.height > 0 && im.gray.len() == im.width.saturating_mul(im.height) {
                let img = colour_or_gray([im.width, im.height], &im.gray, im.rgb.as_deref());
                let opts = TextureOptions::LINEAR;
                match &mut self.texture {
                    Some(t) if self.tex_size == [im.width, im.height] => t.set(img, opts),
                    _ => self.texture = Some(ctx.load_texture("preview", img, opts)),
                }
                self.tex_size = [im.width, im.height];
            } else {
                self.log("preview was the wrong size");
            }
            self.hist_view = im.hist_view;
            self.hist_negative = im.hist_negative;
            self.hist_rgb = im.hist_rgb;
        }
        if let Some(d) = r.detail {
            let n = d.ask.w.saturating_mul(d.ask.h);
            if d.ask.w == 0 || d.ask.h == 0 || d.gray.len() != n {
                self.log("detail window was the wrong size");
                return;
            }
            let img = colour_or_gray([d.ask.w, d.ask.h], &d.gray, d.rgb.as_deref());
            // One sample is one screen pixel, or one file pixel at 1:1.
            // Nearest keeps that sample; it does not blend it with a neighbour.
            let opts = TextureOptions::NEAREST;
            match &mut self.detail_texture {
                Some(t) if self.detail_rect.is_some_and(|r| (r.w, r.h) == (d.ask.w, d.ask.h)) => {
                    t.set(img, opts)
                }
                _ => self.detail_texture = Some(ctx.load_texture("detail", img, opts)),
            }
            self.detail_rect = Some(d.ask);
        }
    }

    fn apply_loupe(&mut self, ctx: &egui::Context, r: LoupeResult) {
        self.loupe_shown = r.generation;
        if r.image.width > 0 && r.image.height > 0 && r.image.gray.len() == r.image.width * r.image.height {
            let w = r.image.width;
            let h = r.image.height;
            let image = colour_or_gray([w, h], &r.image.gray, r.image.rgb.as_deref());
            self.loupe_tex = Some(ctx.load_texture("sharpen-loupe", image, TextureOptions::NEAREST));
            self.loupe_size = [w, h];
        } else {
            // Same controls are not sent again. The previous box stays
            // up; the next change of size, dpi or kernel asks once more.
            self.log("print box failed");
        }
    }

    fn request_preview_if_needed(&mut self) {
        let Some(s) = &self.session else { return };
        let view = self.view_params();
        let view_changed = self.last_view.as_ref() != Some(&view);
        let detail_changed = self.wanted_detail != self.last_detail_request;
        let inspect = self.inspect_recipe();
        let inspect_changed = self.last_inspect != inspect;
        let need = view_changed || detail_changed || (inspect_changed && self.wanted_detail.is_some());
        if !need {
            return;
        }
        // The sharp window stays up until its replacement is applied.
        // Clearing it here would leave the soft fit copy on screen for the
        // whole wait, and the picture would flicker on every slider step.
        self.generation += 1;
        if view_changed {
            let picture = self.shared_picture();
            if picture != self.preview_picture {
                self.picture_gen = self.generation;
                self.preview_picture = picture;
            }
        }
        self.preview.request(PreviewRequest {
            session: Arc::clone(s),
            view: view.clone(),
            detail: self.wanted_detail,
            inspect: inspect.clone(),
            generation: self.generation,
        });
        self.last_view = Some(view);
        self.last_detail_request = self.wanted_detail;
        self.last_inspect = inspect;
    }

    fn handle_dialog(&mut self, ctx: &egui::Context) {
        // Read before the update: the dialog closes in the frame its name
        // is entered, and that Enter is still not one for the picture.
        self.dialog_up = matches!(self.dialog.state(), DialogState::Open);
        self.dialog.update(ctx);
        let Some(path) = self.dialog.take_picked() else { return };
        self.export_to(path, false);
    }

    /// Two rows. The first is what is shown: the windows and the process
    /// on the left, the way back beside them, the note in what is left.
    /// The second is what is done to the picture: the easel, the brush
    /// and the turn, one well with a bar between the tools. Each group is
    /// a flat well, and the bar shows between them.
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing = egui::vec2(0.0, 4.0);
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            tool_well(ui, |ui| {
                self.toolbar_windows(ui);
                ui.add_space(10.0);
                self.toolbar_process(ui);
            });
            ui.add_space(16.0);
            tool_well(ui, |ui| self.toolbar_history(ui));
            ui.add_space(12.0);
            // The note takes only what is left and is cut, never painted over the wells.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.set_max_width(ui.available_width());
                self.toolbar_status(ui);
            });
        });
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            tool_well(ui, |ui| self.toolbar_picture(ui));
        });
    }

    fn toolbar_windows(&mut self, ui: &mut egui::Ui) {
        if ui
            .selectable_label(self.show_strip, "Strip")
            .on_hover_text(
                "Show or hide the film strip (B): the folder's RAW files, the way in. It closes when you start on an open picture. ←/→ open the neighbouring file either way.",
            )
            .clicked()
        {
            self.toggle_strip();
        }
        // Panel indices: 0 left, 1 right, 2 bottom — shown in the order
        // of the three steps.
        for (i, name, hover) in [
            (0, "Negative", "1 · Judge the negative: file, histograms, development (Tab hides every panel)"),
            (2, "Enlarger", "2 · Expose: mix (the filter), placing, the two paper exposures (the grade)"),
            (1, "Print", "3 · The print: size, border, export"),
        ] {
            let hover = if self.waiting[i] {
                "No room at this size. It returns when the window is larger. Click to close it."
            } else {
                hover
            };
            if ui.selectable_label(self.panels[i], name).on_hover_text(hover).clicked() {
                self.panels[i] = !self.panels[i];
                self.persist();
            }
        }
    }

    /// The process: the monochrome negative or the three-colour plate.
    /// One of the two is always lit. A monochrome sensor has no plate, so
    /// the switch is still while such a file is open.
    fn toolbar_process(&mut self, ui: &mut egui::Ui) {
        let mono_sensor = self.session.as_ref().is_some_and(|s| s.is_mono());
        let plate = self.plate_process() && !mono_sensor;
        let (mono_tip, plate_tip) = if mono_sensor {
            (
                "The sensor has no colour filter: this file is the measurement, and it prints monochrome.",
                "No plate from a monochrome sensor.",
            )
        } else {
            (
                "The monochrome negative: the luminance separated from the mosaic, printed split-grade (P switches)",
                "The three-colour plate: the same separation mixed through three taking filters, printed \
                 through the same paper into one colour file, Prokudin-Gorsky's way (P switches)",
            )
        };
        ui.add_enabled_ui(!mono_sensor, |ui| {
            if ui.selectable_label(!plate, "Monochrome").on_hover_text(mono_tip).clicked() && plate {
                self.set_process(false);
            }
            if ui.selectable_label(plate, "Plate").on_hover_text(plate_tip).clicked() && !plate {
                self.set_process(true);
            }
        });
    }

    /// The tools in the recipe's order: the turn, then the straighten and
    /// the window, then the strokes.
    fn toolbar_picture(&mut self, ui: &mut egui::Ui) {
        let has = self.session.is_some();
        if ui
            .add_enabled(has, egui::Button::new("⟳ 90°"))
            .on_hover_text("Rotate the picture 90° clockwise (preview and export)")
            .clicked()
        {
            self.edit.rotation = (self.edit.rotation + 1) % 4;
        }
        tool_bar(ui);
        let mut crop_on = self.crop_open;
        if ui
            .add_enabled(has, egui::Checkbox::new(&mut crop_on, "Crop"))
            .on_hover_text("Frame the negative. Apply makes that window the picture.")
            .changed()
        {
            if crop_on {
                self.arm_crop();
            } else {
                self.close_crop();
            }
        }
        ui.add_enabled_ui(has && self.crop_open, |ui| {
            let response = egui::ComboBox::from_id_salt("crop_format")
                .width(framed_combo_width(ui))
                .height(280.0)
                .selected_text(self.frame_box_label())
                .show_ui(ui, |ui| self.crop_format_table(ui))
                .response;
            response
                .on_hover_text("The sheet, and which way it stands.")
                .on_disabled_hover_text("The sheet, and which way it stands.");
        });
        // The straighten: a line drawn on the picture, or a number. The
        // frame is the largest rectangle of the negative's proportion that
        // fits inside the turned negative.
        ui.add_enabled_ui(has && self.crop_open, |ui| {
            let mut on = self.straighten_on;
            if ui
                .toggle_value(&mut on, "Straighten")
                .on_hover_text(
                    "Draw a line along something that should be level or upright; the picture turns to \
                     make it so, and the frame is the largest rectangle that fits inside the turned \
                     negative. Drags move the window again once this is off.",
                )
                .on_disabled_hover_text("Open Crop first. The straighten is applied with the window.")
                .changed()
            {
                self.straighten_on = on;
                self.line_drag = None;
                self.crop_drag = None;
            }
            // The draft while the easel is up; the recipe's angle when it is
            // down, so the field never shows a turn the picture has not got.
            let mut angle = self.view_angle();
            if ui
                .add(
                    egui::DragValue::new(&mut angle)
                        .range(-straighten::ANGLE_LIMIT..=straighten::ANGLE_LIMIT)
                        .speed(0.05)
                        .fixed_decimals(2)
                        .suffix("°"),
                )
                .on_hover_text("The turn, degrees clockwise. Drag, or type; 0 is the negative as it is.")
                .changed()
            {
                self.set_draft_angle(angle);
            }
            if ui
                .add_enabled(self.view_angle() != 0.0, egui::Button::new("0°"))
                .on_hover_text("No straighten: the whole negative is the frame again.")
                .clicked()
            {
                self.set_draft_angle(0.0);
            }
        });
        if ui
            .add_enabled(has && self.crop_open, egui::Button::new("Apply"))
            .on_hover_text(
                "Keep this window and straighten (Enter). The print, the histograms and the export use only it.",
            )
            .clicked()
        {
            self.apply_crop();
        }
        // The brush: dodging and burning on the picture as it prints.
        tool_bar(ui);
        let mut burn_on = self.burn_on;
        if ui
            .add_enabled(has, egui::Checkbox::new(&mut burn_on, "Burn"))
            .on_hover_text(
                "Dodge and burn with the brush. Set it on the picture, lock it (Enter), then paint: a drag of \
                 the hand, a push of the card. The strokes print at once; Alt shows them.",
            )
            .changed()
        {
            if burn_on {
                self.arm_burn();
            } else {
                self.close_burn();
            }
        }
        ui.add_enabled_ui(has && self.burn_on, |ui| {
            let tools = [
                (Tool::Hand, "Hand", "The hand: an ellipse with a soft edge, painted along a path."),
                (
                    Tool::Card,
                    "Card",
                    "The card: a straight edge with a soft shadow, pushed from one place to another.",
                ),
            ];
            for (tool, label, hint) in tools {
                if ui.selectable_label(self.brush.tool == tool, label).on_hover_text(hint).clicked()
                    && self.brush.tool != tool
                {
                    self.brush.tool = tool;
                    self.burn_drag = None;
                }
            }
            if ui
                .selectable_label(!self.burn_locked, "Set")
                .on_hover_text(
                    "Open the brush: its handles on the picture, nothing in the picture changes (Escape).",
                )
                .clicked()
            {
                self.open_brush();
            }
            if ui
                .selectable_label(self.burn_locked, "Paint")
                .on_hover_text(
                    "Lock the brush and paint with it (Enter). The brush is kept for every picture.",
                )
                .clicked()
            {
                self.lock_brush();
            }
        });
        // The strokes the picture has, and the way to take them all away.
        let n = self.edit.recipe.burns.len();
        ui.add_space(6.0);
        if n > 0 {
            ui.label(RichText::new(format!("{n} stroke{}", if n == 1 { "" } else { "s" })).weak());
            ui.add_space(2.0);
        }
        if ui
            .add_enabled(has && n > 0, egui::Button::new("Clear"))
            .on_hover_text("Take every stroke away. The brush stays as it is; Undo brings them back.")
            .on_disabled_hover_text("No strokes on this picture.")
            .clicked()
        {
            self.edit.recipe.burns.clear();
            self.burn_drag = None;
        }
    }

    /// What the closed box shows. Papers keep their names; the picture is read from its pixels.
    fn frame_box_label(&self) -> String {
        if self.crop_format != PaperFormat::Image {
            return self.crop_format.framed(self.crop_turned).to_owned();
        }
        let (w, h) = self.export_size();
        let (a, b) = if self.crop_turned { (h, w) } else { (w, h) };
        let (a, b) = picture_terms(a as u32, b as u32);
        format!("{a}:{b}")
    }

    /// Two columns: the written ratio, and the same ratio the other way.
    /// The first row is the picture itself. The square is one cell across both.
    fn crop_format_table(&mut self, ui: &mut egui::Ui) {
        let cell = frame_side_button_width(ui);
        let gap = 4.0;
        ui.set_min_width(cell * 2.0 + gap);
        let (pw, ph) = self.export_size();
        let picture = (pw > 0 && ph > 0).then(|| {
            let (a, b) = picture_terms(pw as u32, ph as u32);
            (format!("{a}:{b}"), format!("{b}:{a}"), format!("The picture is {pw}×{ph}."), a == b)
        });
        if let Some((along, _, hint, true)) = &picture {
            if frame_cell(ui, along, self.crop_format == PaperFormat::Image, cell * 2.0 + gap)
                .on_hover_text(hint)
                .clicked()
            {
                self.choose_frame(PaperFormat::Image, false);
            }
        }
        egui::Grid::new("crop_sides").num_columns(2).spacing(egui::vec2(gap, 2.0)).show(ui, |ui| {
            if let Some((along, across, hint, false)) = &picture {
                self.frame_side_cell(ui, PaperFormat::Image, false, cell, along, Some(hint));
                self.frame_side_cell(ui, PaperFormat::Image, true, cell, across, Some(hint));
                ui.end_row();
            }
            for format in PaperFormat::ALL {
                if format == PaperFormat::Square {
                    continue;
                }
                self.frame_side_cell(ui, format, false, cell, format.framed(false), None);
                self.frame_side_cell(ui, format, true, cell, format.framed(true), None);
                ui.end_row();
            }
        });
        if frame_cell(
            ui,
            PaperFormat::Square.label(),
            self.crop_format == PaperFormat::Square,
            cell * 2.0 + gap,
        )
        .clicked()
        {
            self.choose_frame(PaperFormat::Square, false);
        }
    }

    fn frame_side_cell(
        &mut self,
        ui: &mut egui::Ui,
        format: PaperFormat,
        turned: bool,
        width: f32,
        label: &str,
        hint: Option<&str>,
    ) {
        let active = self.crop_format == format && self.crop_turned == turned;
        let mut response = frame_cell(ui, label, active, width);
        if let Some(hint) = hint {
            response = response.on_hover_text(hint);
        }
        if response.clicked() {
            self.choose_frame(format, turned);
        }
    }

    fn choose_frame(&mut self, format: PaperFormat, turned: bool) {
        if self.crop_format == format && self.crop_turned == turned {
            return;
        }
        self.crop_format = format;
        self.crop_turned = turned;
        self.reinscribe();
    }

    fn toolbar_history(&mut self, ui: &mut egui::Ui) {
        let (pos, len) = self.history.position();
        if ui
            .add_enabled(self.history.can_undo(), egui::Button::new("↺"))
            .on_hover_text(format!("Undo (Ctrl+Z) — step {pos} of {len}"))
            .clicked()
        {
            self.undo();
        }
        if ui
            .add_enabled(self.history.can_redo(), egui::Button::new("↻"))
            .on_hover_text("Redo (Ctrl+Shift+Z / Ctrl+Y)")
            .clicked()
        {
            self.redo();
        }
        if ui
            .button("Reset")
            .on_hover_text(
                "Controls back to defaults; the negative stays. Closing, or opening another file, still asks whether to keep this.",
            )
            .clicked()
        {
            self.reset(false);
        }
    }

    fn toolbar_status(&mut self, ui: &mut egui::Ui) {
        if let Some(b) = self.busy.clone() {
            ui.spinner();
            ui.add(egui::Label::new(b).truncate());
        } else if let Some(msg) = self.log.last().cloned() {
            ui.add(egui::Label::new(RichText::new(msg).weak()).truncate());
        }
    }

    /// Left panel: what the file is, the histograms, the development
    /// settings that recompute the negative, the log, the cores.
    fn analysis_panel(&mut self, ui: &mut egui::Ui) {
        let Some(s) = self.session.clone() else {
            ui.label("Open a RAW file or drop it here.");
            ui.add_space(8.0);
            ui.label(RichText::new("Bayer sensors: Nikon Z f (NEF) and other rawler-supported cameras, in either process, the monochrome negative or the three-colour plate. Monochrome sensors (DNG without CFA) pass through without separation.").weak());
            return;
        };
        let n = &s.negative;
        let info = &n.info;
        // Value cells never widen the panel: long text is elided (the full
        // text is the tooltip), so the header and the body stay one width.
        fn val(ui: &mut egui::Ui, text: impl Into<egui::WidgetText>) -> egui::Response {
            ui.add(egui::Label::new(text).truncate())
        }
        egui::Grid::new("file-info").num_columns(2).spacing([8.0, 3.0]).show(ui, |ui| {
            ui.label("name");
            val(ui, s.path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default())
                .on_hover_text(s.path.display().to_string());
            ui.end_row();
            ui.label("camera");
            val(ui, format!("{} {}", info.camera.make.trim(), info.camera.model.trim()));
            ui.end_row();
            let exposure = info.camera.exposure.summary();
            ui.label("exposure");
            if exposure.is_empty() {
                val(ui, RichText::new("not in file").weak());
            } else {
                val(ui, RichText::new(&exposure).strong());
            }
            ui.end_row();
            if let Some(lens) = &info.camera.exposure.lens {
                ui.label("lens");
                val(ui, lens.trim());
                ui.end_row();
            }
            ui.label("sensor");
            val(ui, format!("{} x {} @ {} bit", n.plane.width, n.plane.height, info.camera.bits));
            ui.end_row();
            ui.label("pattern");
            val(ui, info.phase.clone().map(|p| format!("Bayer {p}")).unwrap_or_else(|| "monochrome".into()));
            ui.end_row();
            ui.label("orientation");
            val(
                ui,
                if self.rotation() == 0 {
                    format!("{}", n.orientation)
                } else {
                    format!("{} + {}° cw", n.orientation, 90 * self.rotation() as u16)
                },
            );
            ui.end_row();
            ui.label("balance");
            val(ui, format!("R {:.3}  B {:.3} ({})", info.wb[0], info.wb[2], info.report.wb_source));
            ui.end_row();
            ui.label("process");
            val(ui, self.process_label(&s)).on_hover_text(self.process_hover(&s));
            ui.end_row();
            let px = n.plane.data.len() as f64;
            ui.label("saturated");
            val(
                ui,
                format!(
                    "{:.2} % ({:.2} % dilated, {:.2} % green)",
                    100.0 * info.report.saturated_raw as f64 / px,
                    100.0 * info.report.saturated_dilated as f64 / px,
                    100.0 * info.report.saturated_green as f64 / px
                ),
            )
            .on_hover_text(
                "Photosites at the sensor's clip, and the mask grown around them where no sharpening is \
                 applied. The green share is where the colour is taken out: green has the lowest clip \
                 of the three balanced channels, so a burnt-out green photosite makes every colour \
                 difference around it false, towards magenta. There the plates, and a filtered \
                 negative, take the luminance alone; the native negative is unchanged.",
            );
            ui.end_row();
            ui.label("defects");
            val(ui, format!("{} ({:.3} %)", info.report.defects, 100.0 * info.report.defects as f64 / px))
                .on_hover_text(
                    "Photosites more than eight sigma outside the range of their same-colour neighbours. \
                     Counted, not touched: every photosite stays as measured. The command's \
                     --repair-defects replaces them for a hot pixel from a long exposure.",
                );
            ui.end_row();
            ui.label("camera file");
            match &s.camera_file_path {
                Some(p) => {
                    val(ui, p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default())
                        .on_hover_text(p.display().to_string())
                }
                None => val(ui, RichText::new("none (defaults)").weak()),
            };
            ui.end_row();
            if s.is_mono() {
                ui.label("colour");
                val(ui, "none: the sensor has no colour filter");
            } else {
                ui.label("mask");
                val(ui, format!("{:?}, {} round(s)", info.mask, info.rounds));
            }
            ui.end_row();
            ui.label("timing");
            val(
                ui,
                if s.is_mono() {
                    format!("decode {} · ingest {} ms", info.timing.decode_ms, info.timing.ingest_ms)
                } else {
                    format!(
                        "decode {} · ingest {} · separate {} ms",
                        info.timing.decode_ms, info.timing.ingest_ms, info.timing.separate_ms
                    )
                },
            );
            ui.end_row();
            ui.label("preview");
            val(
                ui,
                format!(
                    "{} x {} ({:.1} %), {} ms, window {} ms",
                    self.tex_size[0],
                    self.tex_size[1],
                    100.0 * s.preview.scale,
                    self.preview_ms,
                    self.detail_ms
                ),
            );
            ui.end_row();
        });

        ui.add_space(10.0);
        ui.separator();
        self.histogram_section(ui);
        ui.collapsing("Development", |ui| {
            if s.is_mono() {
                ui.label(
                    RichText::new(
                        "This sensor records luminance only: the file is the measurement, there is \
                         nothing to develop. White balance and the mask do not apply.",
                    )
                    .weak(),
                );
                return;
            }
            ui.label(RichText::new("Changes here recompute the picture.").weak());
            ui.label(RichText::new("Separation, no demosaic.").weak());
            ui.horizontal(|ui| {
                egui::ComboBox::from_id_salt("white balance")
                    .selected_text(match self.wb_choice {
                        WbChoice::AsShot => "as shot",
                        WbChoice::GrayWorld => "gray world",
                        WbChoice::None => "none",
                        WbChoice::Manual => "manual",
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.wb_choice, WbChoice::AsShot, "as shot");
                        ui.selectable_value(&mut self.wb_choice, WbChoice::GrayWorld, "gray world");
                        ui.selectable_value(&mut self.wb_choice, WbChoice::None, "none");
                        ui.selectable_value(&mut self.wb_choice, WbChoice::Manual, "manual");
                    });
                ui.label("white balance");
                let card = ui
                    .add_enabled(self.busy.is_none(), egui::Button::selectable(self.pick_card, "card"))
                    .on_hover_text(
                        "Read a grey card: click the card in the picture. The next click averages a \
                         32×32 photosite window, leaving out clipped sites, and puts the balance \
                         into manual. Esc cancels. Apply recomputes.",
                    );
                if card.clicked() {
                    self.pick_card = !self.pick_card;
                }
            });
            if self.pick_card {
                ui.label(RichText::new("click the grey card in the picture (Esc cancels)").strong());
            } else if let Some(status) = &self.card_status {
                ui.label(RichText::new(status).small().weak());
            }
            if self.wb_choice == WbChoice::Manual {
                ui.horizontal(|ui| {
                    ui.label("R");
                    ui.add(egui::DragValue::new(&mut self.wb_manual[0]).range(0.1..=8.0).speed(0.01));
                    ui.label("B");
                    ui.add(egui::DragValue::new(&mut self.wb_manual[2]).range(0.1..=8.0).speed(0.01));
                });
                self.wb_manual[1] = 1.0;
            }
            egui::ComboBox::from_label("mask").selected_text(format!("{:?}", self.mask_mode)).show_ui(
                ui,
                |ui| {
                    ui.selectable_value(&mut self.mask_mode, MaskMode::Dubois, "Dubois");
                    ui.selectable_value(&mut self.mask_mode, MaskMode::Adaptive, "Adaptive");
                    ui.selectable_value(&mut self.mask_mode, MaskMode::Off, "Off");
                },
            );
            let differs = self.develop_params() != s.develop;
            if ui.add_enabled(differs && self.busy.is_none(), egui::Button::new("Apply")).clicked() {
                // Sent at the start of the next frame, when this panel's
                // clone of the session is gone and `make_room` can free it.
                self.busy = Some("re-developing".into());
                self.redevelop = Some(self.develop_params());
            }
        });

        ui.collapsing("Log", |ui| {
            egui::ScrollArea::vertical().max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                for line in self.log.iter().rev().take(30).rev() {
                    ui.label(RichText::new(line).small().weak());
                }
            });
        });
        ui.collapsing("CPU", |ui| self.cpu_section(ui));
    }

    /// The three-colour plate: on or off, the taking filters, the
    /// projection primaries, and the three weight rows they come to. A
    /// re-mix, not a re-development: the separation stays.
    /// What the Negative window says the file is developed in.
    /// What the process line explains: for a plate, the weights the
    /// taking filters resolve to on the balanced channels, and the
    /// emulsion behind the Wratten glass.
    fn process_hover(&self, s: &Session) -> String {
        let mut text = String::from(
            "The process this file is developed in: the monochrome negative, or the three-colour \
             plate with its taking filters. The switch is in the toolbar (P); the filters and the \
             lamps are in the Enlarger, the primaries in Print.",
        );
        let Some(p) = self.edit.plate else {
            // The monochrome mix: R + G + B = 1 on the balanced channels.
            let w = self.weights();
            let raw = w.balanced_to_raw(s.wb());
            text.push_str(&format!(
                "\n\nThe mix on the balanced channels: R {:.3} G {:.3} B {:.3}; on the sensor \
                 channels before white balance R {:.3} G {:.3} B {:.3}. A Wratten glass is an \
                 approximation from typical filter curves, not a measured fit; resolution and grain \
                 are unchanged, only band-limited chroma noise is added.",
                w.r, w.g, w.b, raw.r, raw.g, raw.b
            ));
            return text;
        };
        if p.filters == PlateFilters::Wratten {
            // The emulsion behind the glass is a fact of the file, not a choice.
            match s.camera_file_path.as_ref().and_then(|f| f.file_name()) {
                Some(f) => text.push_str(&format!(
                    "\n\nBehind Wratten 25, 58 and 47: the panchromatic response calibrated for this \
                     sensor ({}). It sets how much the three plates overlap.",
                    f.to_string_lossy()
                )),
                None => text.push_str(
                    "\n\nBehind Wratten 25, 58 and 47: the native 0.25 / 0.50 / 0.25, red and blue equally \
                     sensitive, as no camera file calibrates this sensor. It sets how much the three \
                     plates overlap.",
                ),
            }
        }
        match self.plate_resolved.as_ref().map(|r| &r.weights) {
            Some(Ok(w)) => {
                text.push_str("\n\nThe three taking filters on the balanced channels R G B:");
                for (n, r) in ["red", "green", "blue"].iter().zip(w.rows.iter()) {
                    text.push_str(&format!("\n{n:<6} {:+.3} {:+.3} {:+.3}", r[0], r[1], r[2]));
                }
                if p.filters == PlateFilters::Camera && !s.has_colour_matrix() {
                    text.push_str("\nNo matrix in this file: the channels are taken.");
                }
            }
            Some(Err(e)) => text.push_str(&format!("\n\nNo plate: {e}")),
            None => {}
        }
        text
    }

    fn process_label(&self, s: &Session) -> String {
        if s.is_mono() {
            return "monochrome (the sensor)".into();
        }
        match self.edit.plate {
            None => "monochrome negative".into(),
            Some(p) => {
                let filters = match self.plate_resolved.as_ref().map(|r| &r.weights) {
                    Some(Ok(w)) if w.filters != p.filters => {
                        format!("{} (no matrix: {})", p.filters.name(), w.filters.name())
                    }
                    _ => p.filters.name().to_string(),
                };
                format!("plate · {filters} · {}", space_name(p.space))
            }
        }
    }

    /// The plate's taking filters as the row over the paper, where the
    /// monochrome mix stands: the same decision, the filter in front of
    /// the paper. Camera without a matrix in the file is shown, not
    /// offered: the channels would be taken.
    fn plate_filter_strip(&mut self, ui: &mut egui::Ui, s: &Session, width: f32) {
        let Some(mut params) = self.edit.plate else { return };
        let has_matrix = s.has_colour_matrix();
        let fields: Vec<Field> = PlateFilters::ALL
            .into_iter()
            .map(|f| {
                let hover = match f {
                    PlateFilters::Camera if !has_matrix => {
                        "The camera's colour matrix, as a RAW converter applies it: this file carries none."
                    }
                    PlateFilters::Camera => "The camera's colour matrix, as a RAW converter applies it.",
                    PlateFilters::Wratten => {
                        "Wratten 25, 58 and 47 in front of the panchromatic emulsion: the softer palette of \
                         the historical glass."
                    }
                    PlateFilters::Channels => "The sensor's channels as they are, straight into R, G and B.",
                };
                Field::new(capitalise(f.name()), Color32::WHITE, hover)
                    .dimmed(f == PlateFilters::Camera && !has_matrix)
            })
            .collect();
        let selected = PlateFilters::ALL.iter().position(|f| *f == params.filters);
        if let Some(k) = filter_strip(ui, "plate-strip", &fields, selected, width) {
            params.filters = PlateFilters::ALL[k];
        }
        if self.edit.plate != Some(params) {
            self.edit.plate = Some(params);
        }
    }

    /// The machine's cores. The body runs only while the section is open,
    /// and that is the only time a reading is taken.
    fn cpu_section(&mut self, ui: &mut egui::Ui) {
        ui.ctx().request_repaint_after(crate::cpu::SAMPLE_EVERY);
        ui.label(RichText::new("The machine's cores, four times a second.").weak().small());
        match self.cpu.sample() {
            CpuView::Waiting => {
                ui.label(RichText::new("reading…").weak());
            }
            CpuView::Cores(loads) if loads.is_empty() => {
                ui.label(RichText::new("no cores").weak());
            }
            CpuView::Cores(loads) => {
                for (i, load) in loads.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.add_sized(
                            Vec2::new(28.0, ui.spacing().interact_size.y),
                            egui::Label::new(RichText::new(format!("{i}")).weak().small()),
                        );
                        ui.add(egui::ProgressBar::new((load / 100.0).clamp(0.0, 1.0)).show_percentage());
                    });
                }
            }
        }
    }

    fn histogram_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Histogram");
        ui.horizontal_wrapped(|ui| {
            ui.radio_value(&mut self.hist_source, HistSource::View, "print").on_hover_text(
                "The full-resolution negative after placing and the paper or look, as the TIFF gets it. \
                 black/white: pixels at 0 and 65535 in 16 bit",
            );
            ui.radio_value(&mut self.hist_source, HistSource::Negative, "negative").on_hover_text(
                "The full-resolution linear mix before any placing or curve, drawn on a gamma-2.2 axis. \
                 black/white: pixels at 0 and 65535 in 16 bit",
            );
            let raw_tip = if self.session.as_ref().is_some_and(|s| s.is_mono()) {
                "The sensor as shot, one channel, normalised by its black and white level, on a gamma-2.2 axis. \
                 The percentage is pixels at the white level."
            } else {
                "The four sensor channels as shot (R, G1, G2, B by tile position), each normalised by its \
                 black and white level, on a gamma-2.2 axis. The percentages are pixels at the white level."
            };
            ui.radio_value(&mut self.hist_source, HistSource::Raw, "raw").on_hover_text(raw_tip);
            ui.checkbox(&mut self.hist_log, "log");
        });
        let width = ui.available_width().min(300.0);
        if self.hist_source == HistSource::Raw {
            match &self.session {
                Some(s) => raw_histogram_widget(ui, &s.raw_hist, egui::vec2(width, 96.0), self.hist_log),
                None => {
                    ui.label(RichText::new("no file").weak());
                }
            }
            ui.add_space(10.0);
            ui.separator();
            return;
        }
        let h = match self.hist_source {
            HistSource::Negative => &self.hist_negative,
            _ => &self.hist_view,
        };
        let channels = (self.hist_source == HistSource::View).then_some(self.hist_rgb.as_ref()).flatten();
        histogram_widget(ui, h, channels, egui::vec2(width, 96.0), self.hist_log);
        if h.total > 0 {
            let lo = 100.0 * h.clipped_low();
            let hi = 100.0 * h.clipped_high();
            let warn = |v: f64| v >= 0.5;
            ui.horizontal(|ui| {
                let t = format!("black {lo:.2} %");
                ui.label(if warn(lo) {
                    RichText::new(t).color(Color32::from_rgb(255, 120, 80))
                } else {
                    RichText::new(t).weak()
                });
                ui.label(RichText::new(format!("mean {:.1} %", 100.0 * h.mean)).weak());
                let t = format!("white {hi:.2} %");
                ui.label(if warn(hi) {
                    RichText::new(t).color(Color32::from_rgb(255, 120, 80))
                } else {
                    RichText::new(t).weak()
                });
            });
        } else if self.show_mask && self.hist_source == HistSource::Negative {
            ui.label(RichText::new("mask view: no negative histogram").weak());
        }
        ui.add_space(10.0);
        ui.separator();
    }

    /// The mix as one row over the paper: Native, Pan (the camera file) and
    /// the classic glasses in front of it, each field its explanation as
    /// hover text; no field lit while the weights stand elsewhere.
    fn mix_strip(&mut self, ui: &mut egui::Ui, width: f32) {
        let cam = self.session.as_ref().and_then(|s| {
            s.camera_file.as_ref().map(|c| (s.default_weights(), c.weights(), c.weights_space))
        });
        // Filters sit in front of the calibrated response; native without a file.
        let base = cam.map(|c| c.0).unwrap_or_default();
        let current = self.weights();
        let same = |a: &Weights, b: &Weights| (a.r - b.r).abs() < 5e-4 && (a.b - b.b).abs() < 5e-4;
        let mut fields: Vec<Field> = Vec::with_capacity(MIX_LABELS.len());
        let mut targets: Vec<Option<Weights>> = Vec::with_capacity(MIX_LABELS.len());
        // The field the weights stand on, if any; a later one wins a tie.
        let mut selected: Option<usize> = None;
        let native = Weights::default();
        fields.push(Field::new(
            MIX_LABELS[0],
            Color32::WHITE,
            "¼ ½ ¼: the Bayer luminance, no colour weighed in.",
        ));
        targets.push(Some(native));
        if same(&current, &native) {
            selected = Some(0);
        }
        match cam {
            Some((w, _, _)) => {
                fields.push(Field::new(
                    MIX_LABELS[1],
                    Color32::WHITE,
                    "The panchromatic response, calibrated for this sensor.",
                ));
                targets.push(Some(w));
                if same(&current, &w) {
                    selected = Some(1);
                }
            }
            None => {
                fields.push(
                    Field::new(
                        MIX_LABELS[1],
                        Color32::WHITE,
                        "The panchromatic response, calibrated for the sensor: no camera file for this one.",
                    )
                    .dimmed(true),
                );
                targets.push(None);
            }
        }
        for f in ColorFilter::ALL.into_iter().filter(|f| *f != ColorFilter::None) {
            let w = base.filtered(f);
            let hover = format!("Wratten {}, {}: {}.", f.wratten().unwrap_or(0), f.colour(), f.effect());
            fields.push(Field::new(filter_short(f), filter_ink(f), hover));
            targets.push(Some(w));
            if same(&current, &w) {
                selected = Some(fields.len() - 1);
            }
        }
        debug_assert_eq!(fields.len(), MIX_LABELS.len());
        if let Some(w) = filter_strip(ui, "mix-strip", &fields, selected, width).and_then(|k| targets[k]) {
            self.set_weights(w);
        }
    }

    /// nothing wraps, nothing shrinks, and this window does not scroll.
    fn enlarger_panel(&mut self, ui: &mut egui::Ui) {
        let visible = ui.available_width().max(0.0);
        let width = visible.max(enlarger_content_width(ui));
        let origin = ui.cursor().min;
        // Taller than the glass, so a short window does not squeeze the rows.
        // The clip stays the glass. The used rectangle is what sizes the dock.
        let wide = Rect::from_min_size(origin, Vec2::new(width, 420.0));
        let clip = ui.clip_rect();
        let mut body = ui.new_child(
            egui::UiBuilder::new().max_rect(wide).layout(egui::Layout::top_down(egui::Align::Min)),
        );
        body.set_clip_rect(clip);
        self.zones_column(&mut body, width);
        let used = body.min_rect();
        drop(body);
        ui.advance_cursor_after_rect(used);
    }

    /// The filter as one row, what it leaves to say, the paper strip, the
    /// pair it stands for and the paper's ends.
    fn zones_column(&mut self, ui: &mut egui::Ui, width: f32) {
        let mono = self.session.as_ref().is_some_and(|s| s.is_mono());
        let plate = !mono && self.edit.plate.is_some();
        let session = self.session.clone();
        // The filter, the width of the paper under it.
        if mono {
            ui.label(RichText::new("The sensor has no colour filter. This is the measurement.").weak());
        } else if plate {
            if let Some(s) = &session {
                self.plate_filter_strip(ui, s, width);
            }
        } else {
            self.mix_strip(ui, width);
        }
        ui.add_space(6.0);
        let Some(split) = self.edit.recipe.curve.as_paper().cloned() else {
            // A look from a file or an older recipe: no paper to expose.
            ui.add(
                egui::Label::new(
                    RichText::new(format!(
                        "This recipe prints through the look \"{}\", not on paper.",
                        self.edit.recipe.curve.name()
                    ))
                    .weak(),
                )
                .wrap_mode(egui::TextWrapMode::Extend),
            );
            if ui
                .button("Print on paper")
                .on_hover_text("Replace the look by the paper at its base exposure")
                .clicked()
            {
                self.edit.recipe.curve = Tone::paper();
            }
            return;
        };
        let mut split = split;
        paper_strip(ui, &mut split, width);
        if self.edit.recipe.curve.as_paper() != Some(&split) {
            self.edit.recipe.curve = Tone::Paper(split.clone());
        }
        ui.add_space(4.0);
        let curve = split.curve();
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "≈ grade {} · {:.1} stops of scene",
                    paper::grade_label(curve.grade()),
                    curve.scene_range_stops(),
                ))
                .weak(),
            )
            .wrap_mode(egui::TextWrapMode::Extend),
        )
        .on_hover_text(
            "The single filter grade this pair of exposures equals (by its ISO range), and the scene \
             stops between the paper's first tone and its near-black. The two exposures themselves \
             are in the recipe file.",
        );
        if self.plate_view().is_some() {
            self.lamp_bars(ui, width);
        }
        if self.edit.recipe.place != 0.0 {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("place {:+.2}", self.edit.recipe.place)).weak().small())
                    .on_hover_text(
                        "A placing from the recipe, in scene stops; the strip moves both exposures instead.",
                    );
                if ui.small_button("0").on_hover_text("Drop the placing").clicked() {
                    self.edit.recipe.place = 0.0;
                }
            });
        }
    }

    /// The projector's lamps, under the paper: stops added to the red and
    /// the blue positive. Green is the reference and has none. The only
    /// colour control a plate has: two bars the width of the paper.
    fn lamp_bars(&mut self, ui: &mut egui::Ui, width: f32) {
        ui.add_space(6.0);
        let [mut red, mut blue] = self.edit.lamps;
        lamp_bar(ui, Lamp::Red, &mut red, width);
        ui.add_space(4.0);
        lamp_bar(ui, Lamp::Blue, &mut blue, width);
        let lamps = [red.clamp(-LAMP_LIMIT, LAMP_LIMIT), blue.clamp(-LAMP_LIMIT, LAMP_LIMIT)];
        if lamps != self.edit.lamps {
            self.edit.lamps = lamps;
        }
    }

    /// Right window, step 3: the print and its export.
    fn print_panel(&mut self, ui: &mut egui::Ui) {
        // The window's width, taken once. egui widens a ui to hold a row
        // that runs past it, and every later row would then be laid out
        // against that wider edge: a right-aligned control walks out of the
        // window, a full-width button grows past it. Rows that reach the
        // right edge are measured against this width instead, and a row
        // that still runs past it is reported, once, in the log.
        let width = ui.available_width();
        let left = ui.max_rect().left();
        self.print_section(ui, width);
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        self.peak_controls(ui);
        ui.add_space(6.0);
        let idle = self.busy.is_none();
        let has = self.session.is_some();
        let hover = self.export_hover_text();
        if ui
            .add_enabled(
                idle && has,
                egui::Button::new(RichText::new("Export…").strong()).min_size(Vec2::new(width, 30.0)),
            )
            .on_disabled_hover_text(hover.clone())
            .on_hover_text(hover)
            .clicked()
            && self.export_memory_allows()
        {
            self.start_export();
        }
        ui.add_space(4.0);
        let hover = self.linear_hover_text();
        if ui
            .add_enabled(idle && has, egui::Button::new("Negative…").min_size(Vec2::new(width, 24.0)))
            .on_disabled_hover_text(hover.clone())
            .on_hover_text(hover)
            .clicked()
        {
            self.start_linear_export();
        }
        // The note stays while the estimate it was about is the current one.
        if let Some(need) = self.export_warned.filter(|n| Some(*n) == self.export_memory()) {
            ui.label(
                RichText::new(format!(
                    "About {} at once, more than is free. Export again to go ahead.",
                    memory_text(need)
                ))
                .color(ui.visuals().warn_fg_color),
            );
        }
        let over = ui.min_rect().right() - (left + width);
        if over > 0.5 && !self.print_overflow_logged {
            self.print_overflow_logged = true;
            self.log(format!("the Print window's content runs {over:.0} pt past its right edge"));
        }
    }

    /// The phone's sharpening mode stays off on the desktop. Peak is a
    /// separate field and is not cleared here. Assigned only when it is
    /// not already off, so the history stays still.
    fn clear_desktop_sharpen(&mut self) {
        if self.edit.recipe.sharpen != Sharpen::Off {
            self.edit.recipe.sharpen = Sharpen::Off;
        }
    }

    /// The print: size, resolution, border and resampling. The phone's
    /// sharpening mode stays off. Peak, below the tile, is the desktop
    /// high-pass. The 1:1 box is the print pixels.
    fn print_section(&mut self, ui: &mut egui::Ui, width: f32) {
        // The file the export writes, first: it is the process's. The
        // format is the program's: the same sheet, 16-bit TIFF or 8-bit
        // JPEG, remembered across files. The row has to fit the window
        // with "Adobe RGB" in the box, so the bits are in the hover texts.
        let format_changed = ui
            .horizontal(|ui| {
                print_field_label(ui, "file");
                let mut changed = false;
                for f in ExportFormat::ALL {
                    changed |= ui
                        .selectable_value(&mut self.format, f, f.name())
                        .on_hover_text(match f {
                            ExportFormat::Tiff => {
                                "16 bit, the ICC profile embedded, the recipe in the description. The print \
                                 for the printer and the archive."
                            }
                            ExportFormat::Jpeg => {
                                "8 bit at quality 100, no chroma subsampling, the same ICC profile and the \
                                 dpi embedded, no description. The print for the screen and the web."
                            }
                        })
                        .changed();
                }
                let bits = match self.format {
                    ExportFormat::Tiff => "16 bit",
                    ExportFormat::Jpeg => "8 bit",
                };
                match self.edit.plate {
                    Some(mut params) => {
                        ui.label(RichText::new("RGB ·").weak()).on_hover_text(format!(
                            "The three positives as the channels of one {bits} RGB file, with a matrix/TRC \
                             profile of the primaries beside and the paper's gamma 2.2."
                        ));
                        // The primaries the plates are projected through: the file's profile.
                        egui::ComboBox::from_id_salt("plate-space")
                        .selected_text(space_name(params.space))
                        .show_ui(ui, |ui| {
                            ui.selectable_value(&mut params.space, PlateSpace::AdobeRgb, "Adobe RGB")
                                .on_hover_text(
                                    "Adobe RGB (1998): its curve is exactly the paper's gamma 2.2 and its \
                                     gamut holds the saturated primaries. The file carries the profile.",
                                );
                            ui.selectable_value(&mut params.space, PlateSpace::Srgb, "sRGB").on_hover_text(
                                "sRGB primaries with the paper's gamma 2.2 (not the sRGB curve). The file \
                                 carries the profile.",
                            );
                        })
                            .response
                            .on_hover_text("The projection primaries, and the profile of the file.");
                        if self.edit.plate != Some(params) {
                            self.edit.plate = Some(params);
                        }
                    }
                    None => {
                        ui.label(RichText::new("grey · gamma 2.2").weak()).on_hover_text(format!(
                            "The sheet as one {bits} grey channel with a gamma 2.2 profile."
                        ));
                    }
                }
                changed
            })
            .inner;
        if format_changed {
            // The format is a setting of the program, not of the recipe.
            self.persist();
        }
        // One rule for the sheet: the fields are the sheet in the printer,
        // always in the crop's proportion. Type one side and the other
        // follows the picture; the rings are taken from inside the sheet
        // and the picture fills what they leave, centred. 1:1 is the sheet
        // that is the picture's pixels, nothing more: the start, lit while
        // the file is still that.
        let has_file = self.session.is_some();
        let one_to_one_now = self.is_one_to_one();
        let row_h = ui.spacing().interact_size.y;
        let (one_to_one, ruler_changed) = ui
            .allocate_ui_with_layout(
                Vec2::new(width, row_h),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    print_field_label(ui, "size");
                    let one = ui
                        .add_enabled(has_file, egui::Button::selectable(one_to_one_now, "1:1"))
                        .on_hover_text(
                            "The sheet is the picture's pixels: no border, no black edge, no scaling. \
                         The start; the sheet typed from here stands, and rings, crop and dpi \
                         fit the picture inside it.",
                        )
                        .clicked();
                    // The ruler, at the right of the same row: the unit every
                    // length below is shown and typed in. The recipe keeps inches
                    // and millimetres whichever is chosen.
                    let ruler_changed = ui
                        .with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            let mut changed = false;
                            for r in Ruler::ALL.iter().rev() {
                                let response = ui
                                .selectable_value(&mut self.ruler, *r, r.name())
                                .on_hover_text(match r {
                                Ruler::Metric => {
                                    "Centimetres for the sheet, millimetres for the border and the black edge"
                                }
                                Ruler::Imperial => "Inches",
                                Ruler::Pixels => "Pixels of the file, at the dpi below",
                            });
                                changed |= response.changed();
                            }
                            changed
                        })
                        .inner;
                    (one, ruler_changed)
                },
            )
            .inner;
        if ruler_changed {
            // The ruler is a setting of the program, not of the recipe: it
            // is written down as soon as it is switched.
            self.persist();
        }
        if one_to_one {
            self.set_one_to_one();
        }
        let dpi_before = self.edit.recipe.dpi;
        let rings_before = (self.edit.recipe.border_px(), self.edit.recipe.black_px());
        let (width_changed, height_changed) = ui
            .horizontal(|ui| {
                print_field_label(ui, "");
                let ruler = self.ruler;
                let dpi = self.edit.recipe.dpi;
                // The rings must leave the picture at least a millimetre.
                let least = 2.0 * self.edit.recipe.frame_px() as f64 / dpi.max(1.0) * 2.54 + 0.1;
                let hover = "The sheet in the printer, in the crop's proportion: type one side, the other \
                             follows the picture. Shown the picture's way, the long side where the \
                             picture's long side is. The border and the black edge are taken from inside \
                             it and the picture fills what they leave, centred.";
                let width =
                    length_field(ui, &mut self.sheet_cm[0], Base::Cm, ruler, dpi, least..=2000.0, has_file)
                        .on_hover_text(hover);
                ui.label("×");
                let height =
                    length_field(ui, &mut self.sheet_cm[1], Base::Cm, ruler, dpi, least..=2000.0, has_file)
                        .on_hover_text(hover);
                (width.changed(), height.changed())
            })
            .inner;
        let dpi_changed = ui
            .horizontal(|ui| {
                print_field_label(ui, "dpi");
                // Taken on Enter or when the field is left, as the lengths
                // are: "300" typed is not 36, 36 and then 300 dpi, each
                // with a sheet that follows it.
                ui.add(
                    egui::DragValue::new(&mut self.edit.recipe.dpi)
                        .update_while_editing(false)
                        .range(36.0..=1200.0)
                        .clamp_existing_to_range(false)
                        .speed(1.0)
                        .suffix(" dpi"),
                )
                .on_hover_text(
                    "Pixels per inch, written into the TIFF. With the cm or in ruler the sheet keeps its size \
                     and the pixels follow; with the px ruler the pixels stay and the size follows.",
                )
                .changed()
            })
            .inner;
        if width_changed {
            self.apply_sheet(SheetSide::Width);
        } else if height_changed {
            self.apply_sheet(SheetSide::Height);
        } else if dpi_changed && (dpi_before - self.edit.recipe.dpi).abs() > f64::EPSILON {
            self.follow_dpi(dpi_before, rings_before);
        } else {
            self.show_sheet();
        }
        self.print_sheet_line(ui);
        // border and black share the row. Wrapped, so a long value breaks
        // inside the column instead of widening the dock. The sheet stands
        // under a changed ring; the picture refits inside it.
        let ruler = self.ruler;
        let dpi = self.edit.recipe.dpi;
        ui.horizontal_wrapped(|ui| {
            print_field_label(ui, "border");
            length_field(ui, &mut self.edit.recipe.border_mm, Base::Mm, ruler, dpi, 0.0..=100.0, true)
                .on_hover_text(
                    "White paper around the picture, outside the black edge, taken from the sheet: at least this much; \
                     more on two sides where the sheet's ratio is not the picture's. Zero is off.",
                );
            ui.label(RichText::new("black").weak());
            length_field(ui, &mut self.edit.recipe.black_mm, Base::Mm, ruler, dpi, 0.0..=100.0, true).on_hover_text(
                "Black film edge against the picture, inside the white border when there is one, taken from the sheet. Either ring stands on its own. Zero is off.",
            );
        });
        ui.horizontal(|ui| {
            print_field_label(ui, "resample");
            let r = &mut self.edit.recipe.resample;
            egui::ComboBox::from_id_salt("resample").selected_text(r.name()).show_ui(ui, |ui| {
                for k in ResampleKernel::ALL {
                    ui.selectable_value(r, k, k.name()).on_hover_text(match k {
                        ResampleKernel::Lanczos3 => "Sharpest; rings at hard edges. The benchmark kernel",
                        ResampleKernel::Mitchell => {
                            "Mitchell–Netravali B=C=⅓: slight softness, little ringing"
                        }
                        ResampleKernel::CatmullRom => "Interpolating cubic; rings like a short Lanczos",
                        ResampleKernel::BSpline => "Default. No overshoot, visibly soft",
                        ResampleKernel::Area => {
                            "Box area. Down: the mean of the covered pixels. Up: the pixel, held"
                        }
                    });
                }
            });
        });
        self.sharpen_loupe(ui);
    }

    /// The picture inside the sheet, its scale from the negative's pixels,
    /// and the export's memory. Absent until a file is open.
    fn print_sheet_line(&self, ui: &mut egui::Ui) {
        let Some(l) = self.print_layout() else { return };
        let dpi = self.edit.recipe.dpi;
        if dpi <= 0.0 {
            return;
        }
        // The picture inside the sheet: what the fields above leave it.
        let inches = |px: usize| px as f64 / dpi;
        let physical = match self.ruler {
            Ruler::Metric => {
                format!("{:.1} × {:.1} cm · ", inches(l.image_w) * 2.54, inches(l.image_h) * 2.54)
            }
            Ruler::Imperial => format!("{:.2} × {:.2} in · ", inches(l.image_w), inches(l.image_h)),
            Ruler::Pixels => String::new(),
        };
        // The scale, so a sheet that resamples says so at a glance.
        let (uw, uh) = self.picture_size();
        let scale = if l.image_w == uw && l.image_h == uh {
            "1:1".to_string()
        } else {
            format!("×{:.2}", l.image_w as f64 / uw.max(1) as f64)
        };
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(
                RichText::new(format!("picture {physical}{} × {} px · {scale} · ", l.image_w, l.image_h))
                    .weak(),
            )
            .on_hover_text(
                "The picture inside the rings, and its scale from the negative's pixels. \
                     The fields above are the whole file.",
            );
            // What the export holds at once, red once the machine cannot.
            if let Some(need) = self.export_memory() {
                let text = RichText::new(memory_text(need));
                let text = if self.ram_total > 0 && need > self.ram_total {
                    text.color(ui.visuals().error_fg_color)
                } else {
                    text.weak()
                };
                ui.label(text).on_hover_text(
                    "Memory the export holds at its widest point. \
                     Above what the machine has, the export is refused; \
                     above what is free, it asks twice.",
                );
            }
        });
    }

    /// Bytes the export holds at its widest, from the same layout the
    /// panel shows. Absent until a file is open.
    fn export_memory(&self) -> Option<u64> {
        // The whole negative; the estimate applies the straighten itself.
        let (w, h) = self.export_size_at(0.0);
        if w == 0 || h == 0 {
            return None;
        }
        let params = mimizan_core::print::PrintParams { recipe: self.print_recipe(), ..Default::default() };
        if self.plate_view().is_none() {
            return Some(memory_estimate(w, h, 1, &params));
        }
        // A plate holds its three mixed planes while one of them is
        // printed, and the finished positives beside the third: the same
        // figure the command refuses by.
        Some(mimizan_core::print::plate_memory_estimate(w, h, 1, &params))
    }

    /// Whether the export may start. A print that fits in free memory
    /// goes; one above everything the machine has is refused with the two
    /// figures; in between, the first click warns and the second goes.
    fn export_memory_allows(&mut self) -> bool {
        let Some(need) = self.export_memory() else { return true };
        let mut sys = sysinfo::System::new();
        sys.refresh_memory();
        let (total, available) = (sys.total_memory(), sys.available_memory());
        if total == 0 {
            return true;
        }
        if need as f64 <= available as f64 / 1.3 {
            self.export_warned = None;
            return true;
        }
        if need > total {
            self.export_warned = None;
            self.log(format!(
                "export not started: it would hold about {} at once, this machine has {} in all",
                memory_text(need),
                memory_text(total)
            ));
            return false;
        }
        if self.export_warned == Some(need) {
            self.export_warned = None;
            return true;
        }
        self.export_warned = Some(need);
        self.log(format!(
            "export would hold about {} at once; {} is free. Export again to go ahead",
            memory_text(need),
            memory_text(available)
        ));
        false
    }

    /// The 1:1 box, as wide as this panel. Print pixels after the size
    /// step, the same ones the TIFF gets. Peak is in them only while the
    /// checkbox is on.
    fn sharpen_loupe(&mut self, ui: &mut egui::Ui) {
        self.loupe_open = true;
        let ppp = ui.pixels_per_point().max(f32::MIN_POSITIVE);
        let side = (ui.available_width() * ppp).round() as usize;
        self.loupe_px = side.clamp(64, 960);
        if let Some(scale) = self.loupe_layout().map(|l| l.enlargement) {
            // The same scale the line under the size fields shows; a
            // reduction is a scale too.
            let mut text = if (scale - 1.0).abs() < 1e-9 {
                "1:1 · no scaling".to_string()
            } else {
                format!("×{scale:.2}")
            };
            if self.loupe_shown != self.loupe_sent {
                text.push_str(" · updating");
            }
            ui.label(RichText::new(text).weak());
        }
        let mut drag = None;
        if self.session.is_none() {
            print_frame(ui, "Open a RAW file.");
        } else if let Some(tex) = &self.loupe_tex {
            let ppp = ui.pixels_per_point().max(f32::MIN_POSITIVE);
            let [w, h] = self.loupe_size;
            let size = Vec2::new(w as f32 / ppp, h as f32 / ppp);
            let (rect, resp) = ui.allocate_exact_size(size, Sense::drag());
            if resp.dragged() {
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            } else if resp.hovered() {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
            }
            let id = tex.id();
            ui.painter().image(id, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);
            ui.painter().rect(
                rect,
                0.0,
                Color32::TRANSPARENT,
                egui::Stroke::new(1.0, Color32::from_white_alpha(20)),
                egui::StrokeKind::Inside,
            );
            let resp = resp.on_hover_text(format!("{w} × {h} print px · 1:1 · drag to move"));
            if resp.dragged() {
                drag = Some((resp.drag_delta(), ppp));
            }
        } else {
            print_frame(ui, "rendering the print box…");
        }
        if let Some((delta, ppp)) = drag {
            // The picture follows the hand. One screen pixel is one print pixel.
            self.nudge_loupe(delta, ppp);
        }
    }

    /// Move the loupe centre by a drag. `delta` is in points; the box is one
    /// print pixel per screen pixel, and the picture follows the pointer.
    fn nudge_loupe(&mut self, delta: Vec2, ppp: f32) {
        let Some(l) = self.loupe_layout() else { return };
        let Some(at) = self.sharpen_at.as_mut() else { return };
        let iw = (l.image_w.saturating_sub(1) as f64).max(1.0);
        let ih = (l.image_h.saturating_sub(1) as f64).max(1.0);
        at[0] = (at[0] - f64::from(delta.x * ppp) / iw).clamp(0.0, 1.0);
        at[1] = (at[1] - f64::from(delta.y * ppp) / ih).clamp(0.0, 1.0);
    }

    fn request_loupe(&mut self) {
        let Some([nx, ny]) = self.sharpen_at else { return };
        let Some(session) = self.session.clone() else { return };
        let key = (
            session.id,
            self.weights(),
            self.plate_view(),
            self.loupe_recipe(),
            self.export_orientation(),
            [nx, ny],
            self.loupe_px,
        );
        if self.loupe_key.as_ref() == Some(&key) {
            return;
        }
        self.loupe_key = Some(key);
        self.loupe_picture = self.shared_picture();
        self.loupe_sent = self.loupe_sent.wrapping_add(1);
        self.loupe.request(LoupeRequest {
            session,
            weights: self.weights(),
            plate: self.plate_view(),
            recipe: self.loupe_recipe(),
            orientation: self.export_orientation(),
            nx,
            ny,
            view: self.loupe_px,
            generation: self.loupe_sent,
        });
    }

    /// A picture is open, so the print box has a point. A new file and Apply
    /// put that point in the middle; this only fills a gap.
    fn ensure_loupe_point(&mut self) {
        if self.session.is_some() && self.sharpen_at.is_none() {
            self.sharpen_at = Some([0.5, 0.5]);
        }
    }

    /// What one export writes, for the Export button in the Print window.
    fn export_hover_text(&self) -> String {
        if self.session.is_none() {
            return "Open a RAW file first.".into();
        }
        let look = match &self.edit.recipe.curve {
            Tone::Paper(s) if s.soft == 0.0 && s.hard == 0.0 => "base paper".to_string(),
            Tone::Paper(s) => format!("paper (soft {:+.2}, hard {:+.2})", s.soft, s.hard),
            Tone::Look(l) => format!("look \"{}\"", l.name),
        };
        let (uw, uh) = self.picture_size();
        let (ow, oh) = self.print_layout().map(|l| (l.out_w, l.out_h)).unwrap_or((uw, uh));
        let rot = if self.rotation() != 0 {
            format!(", rotated {}°", 90 * self.rotation() as u16)
        } else {
            String::new()
        };
        let rot = format!("{rot}{}", self.straighten_text());
        let r = &self.edit.recipe;
        let local = if r.place != 0.0 { format!(", place {:+.2}", r.place) } else { String::new() };
        let sharpen = if r.peak.on {
            let radius = peak_print_radius(r.peak.radius, self.peak_scale());
            format!(", radius {radius:.2} px, amount {:.2}", r.peak.amount)
        } else {
            ", no sharpening".to_string()
        };
        // The JPEG has no description: the recipe stays in the file beside the RAW.
        let (ext, bits, described) = match self.format {
            ExportFormat::Tiff => ("tif", "16 bit", true),
            ExportFormat::Jpeg => ("jpg", "8 bit, quality 100", false),
        };
        match self.plate_view() {
            Some(p) => {
                let space = match p.space {
                    PlateSpace::AdobeRgb => "Adobe RGB (1998)",
                    PlateSpace::Srgb => "sRGB",
                };
                let lamps = if p.lamps[0] != 0.0 || p.lamps[2] != 0.0 {
                    format!(", lamps R {:+.2} B {:+.2}", p.lamps[0], p.lamps[2])
                } else {
                    String::new()
                };
                let desc = if described { ", recipe and plate in the description" } else { "" };
                format!(
                    "Writes the three-colour plate on the {look}{rot}{local}{lamps}:\n\
                     <name>.{ext}  {ow} x {oh} px, {bits} RGB, {space} gamma 2.2 ICC, {} filters{sharpen}{desc}",
                    p.weights.filters.name()
                )
            }
            None => {
                let desc = if described { ", recipe in the description" } else { "" };
                format!(
                    "Writes the print on the {look}{rot}{local}:\n\
                     <name>.{ext}  {ow} x {oh} px, {bits}, gamma 2.2 grey ICC{sharpen}{desc}"
                )
            }
        }
    }

    /// What the linear export writes, for the Negative button.
    fn linear_hover_text(&self) -> String {
        let Some(s) = &self.session else { return "Open a RAW file first.".into() };
        let (w, h) = self.picture_size();
        let rot = if self.rotation() != 0 {
            format!(", turned {}°", 90 * self.rotation() as u16)
        } else {
            String::new()
        };
        let rot = format!("{rot}{}", self.straighten_text());
        let crop = if self.edit.recipe.crop.is_some() { ", the crop" } else { ", the whole frame" };
        if self.linear_is_dng() {
            format!(
                "Writes the separation for another program, no print{rot}{crop}:\n\
                 <name>.dng  {w} x {h} px, linear DNG, three 16-bit channels a pixel (no demosaicing left to \
                 do), the camera's colour matrix and as-shot white balance in the tags. A RAW converter opens \
                 it as a RAW of this camera; the colour is band-limited to the carriers under the full \
                 luminance."
            )
        } else {
            // The mask is a second file no dialog names; the sandbox writes
            // it only inside the folder grant.
            let mask = if sandboxed() {
                " (inside the folder opened through Open; elsewhere the sandbox allows only the file named)"
            } else {
                ""
            };
            if s.is_mono() {
                format!(
                    "Writes the negative for another program, no print{rot}{crop}:\n\
                     <name>.tif  {w} x {h} px, 16 bit linear grey, the mask beside it as <name>.mask.tif{mask}"
                )
            } else {
                format!(
                    "Writes the mixed negative for another program, no print{rot}{crop}:\n\
                     <name>.tif  {w} x {h} px, 16 bit linear grey (the mix as it stands, weights in the \
                     description), the mask beside it as <name>.mask.tif{mask}"
                )
            }
        }
    }

    /// Long-side scale of the print, destination / source. The radius field
    /// shows print pixels: the stored negative radius times this scale.
    fn peak_scale(&self) -> f64 {
        self.print_layout().map(|l| l.enlargement).unwrap_or(1.0)
    }

    /// A new file, or a recipe just loaded. A saved peak stays in the fields.
    /// The measurement still runs, so reset can return to it.
    fn note_peak_source(&mut self) {
        self.peak_measure = None;
        self.peak_measured = None;
        self.peak_gen = self.peak_gen.wrapping_add(1);
        self.peak_held = !self.edit.recipe.peak.is_unset();
        self.peak_arm = self.session.is_some() && self.screenshot_to.is_none();
    }

    fn start_peak_tune(&mut self, ctx: &egui::Context) {
        let Some(session) = self.session.clone() else { return };
        let mut recipe = self.print_recipe();
        recipe.sharpen = Sharpen::Off;
        recipe.peak.on = false;
        self.peak_measure = Some(PeakMeasure::start(
            ctx.clone(),
            self.peak_gen,
            session,
            self.weights(),
            recipe,
            self.export_orientation(),
        ));
    }

    fn poll_peak(&mut self) {
        let poll = self.peak_measure.as_ref().map(PeakMeasure::poll);
        match poll {
            Some(PeakPoll::Ready(result)) => {
                self.peak_measure = None;
                if result.generation == self.peak_gen {
                    self.peak_measured = Some((result.radius, result.amount));
                    if !self.peak_held {
                        self.apply_measured_peak(result.radius, result.amount, result.found);
                    }
                }
            }
            Some(PeakPoll::Lost) => {
                self.peak_measure = None;
                self.log("peak measure failed");
            }
            Some(PeakPoll::Waiting) | None => {}
        }
    }

    /// Fill the fields from the measurement. The checkbox is left as it is.
    /// The baseline takes the same numbers, so opening a file does not by
    /// itself ask to save.
    fn apply_measured_peak(&mut self, radius: f64, amount: f64, found: bool) {
        self.edit.recipe.peak.radius = radius;
        self.edit.recipe.peak.amount = amount;
        self.recipe_baseline.peak.radius = radius;
        self.recipe_baseline.peak.amount = amount;
        self.history.fill_peak(radius, amount);
        if found {
            self.log(format!("peak {radius:.2} px, amount {amount:.2}"));
        } else {
            self.log("peak found no edge");
        }
    }

    /// High-pass controls, directly above Export. The method and the
    /// automatic measurement are on the panel, not only in a hover. Radius
    /// and amount share a row. The fields stay editable while the checkbox
    /// is off. The tile and the TIFF sharpen only while it is on.
    fn peak_controls(&mut self, ui: &mut egui::Ui) {
        let has = self.session.is_some();
        ui.horizontal_wrapped(|ui| {
            print_field_label(ui, "sharpen");
            let mut on = self.edit.recipe.peak.on;
            let response = ui
                .add_enabled(has, egui::Checkbox::new(&mut on, ""))
                .on_hover_text(
                    "Sharpen this print. The tile and the TIFF use the radius and the amount. Off leaves both as they were.",
                );
            if response.changed() {
                self.edit.recipe.peak.on = on;
            }
            ui.label("high-pass").on_hover_text(
                "High-pass sharpening: the print plus an amount of itself minus a Gaussian blur. The radius is that blur.",
            );
        });
        let scale = self.peak_scale();
        ui.horizontal_wrapped(|ui| {
            print_field_label(ui, "radius");
            let mut shown = self.edit.recipe.peak.radius * scale;
            let response = ui
                .add_enabled(
                    has,
                    egui::DragValue::new(&mut shown)
                        .range(
                            mimizan_core::recipe::PEAK_RADIUS_MIN * scale
                                ..=mimizan_core::recipe::PEAK_RADIUS_MAX * scale,
                        )
                        .speed(0.05 * scale)
                        .fixed_decimals(2)
                        .suffix(" px"),
                )
                .on_hover_text(
                    "Radius of the Gaussian blur, in print pixels. The print's scale multiplies the radius measured on the negative; the amount stays.",
                );
            if response.changed() {
                let stored = (shown / scale)
                    .clamp(mimizan_core::recipe::PEAK_RADIUS_MIN, mimizan_core::recipe::PEAK_RADIUS_MAX);
                if (stored - self.edit.recipe.peak.radius).abs() > 1e-9 {
                    self.edit.recipe.peak.radius = stored;
                    self.peak_held = true;
                }
            }
            ui.label(RichText::new("amount").weak());
            let response = ui
                .add_enabled(
                    has,
                    egui::DragValue::new(&mut self.edit.recipe.peak.amount)
                        .range(0.0..=mimizan_core::recipe::PEAK_AMOUNT_MAX)
                        .clamp_existing_to_range(false)
                        .speed(0.02)
                        .fixed_decimals(2),
                )
                .on_hover_text(
                    "How much of the high-pass is added back. The print's scale does not change it. Zero leaves the picture as it was.",
                );
            if response.changed() {
                self.peak_held = true;
            }
        });
        self.peak_reset_row(ui, has);
    }

    fn peak_at_measured(&self) -> bool {
        let Some((radius, amount)) = self.peak_measured else {
            return false;
        };
        (self.edit.recipe.peak.radius - radius).abs() < 1e-6
            && (self.edit.recipe.peak.amount - amount).abs() < 1e-6
    }

    fn peak_reset_hint(&self, has: bool) -> &'static str {
        if !has {
            "Open a RAW file first."
        } else if self.peak_measure.is_some() {
            "Peak is still measuring this negative."
        } else if self.peak_measured.is_none() {
            "No measurement for this negative."
        } else {
            "Radius and amount are already the measured values."
        }
    }

    /// The measurement is named on the row, and reset returns to it.
    /// The checkbox stays.
    fn peak_reset_row(&mut self, ui: &mut egui::Ui, has: bool) {
        let status = if self.peak_measure.is_some() { "measuring" } else { "measured automatically" };
        ui.horizontal_wrapped(|ui| {
            print_field_label(ui, "");
            ui.label(RichText::new(status).weak()).on_hover_text(
                "An algorithm measured the radius and the amount on this negative. Reset returns to those values.",
            );
            let ready = has && self.peak_measured.is_some() && !self.peak_at_measured();
            let response = ui
                .add_enabled(ready, egui::Button::new("reset"))
                .on_hover_text("Back to the radius and amount the algorithm measured. The checkbox stays as it is.")
                .on_disabled_hover_text(self.peak_reset_hint(has));
            if response.clicked() {
                if let Some((radius, amount)) = self.peak_measured {
                    self.edit.recipe.peak.radius = radius;
                    self.edit.recipe.peak.amount = amount;
                }
            }
        });
    }

    /// `stage` is the opening between the windows, and the fit is measured
    /// there. Zoomed, the sheet keeps that scale and that centre and runs on
    /// into the room, and the windows lie on it. `docks` are those windows.
    fn preview_panel(&mut self, ui: &mut egui::Ui, stage: Rect, docks: [Option<Rect>; 3]) {
        let room = ui.available_rect_before_wrap();
        let hit = if picture_is_fitted(self.zoom) { stage } else { room };
        let response = ui.allocate_rect(hit, Sense::click_and_drag());
        let on_dock = |p: Pos2| docks.iter().flatten().any(|r| r.contains(p));
        let Some(tex) = self.texture.as_ref().map(|t| t.id()) else {
            let painter = ui.painter_at(stage);
            painter.rect_filled(stage, 0.0, Color32::from_gray(28));
            if self.session.is_none() {
                painter.text(
                    stage.center(),
                    egui::Align2::CENTER_CENTER,
                    "Mimizan Lab",
                    egui::FontId::proportional(28.0),
                    Color32::from_gray(90),
                );
            }
            return;
        };
        let [tw, th] = self.tex_size;
        if tw == 0 || th == 0 {
            return;
        }
        // The rings and the crosshair belong to the pixels on the stage, not
        // to the edit alone: see `held_dress`.
        let StageDress { rings, cross } = self.stage_dress();
        let Rings { white, black, .. } = rings;
        let frame_tex = rings.frame();
        // The sheet (picture plus the paper around it) is what fits the
        // opening. A zoom grows it from this same fit, so the first step
        // does not jump.
        let fit = (stage.width() / (tw as f32 + 2.0 * frame_tex.x))
            .min(stage.height() / (th as f32 + 2.0 * frame_tex.y));
        if let Some(scale) = self.keep_scale.take() {
            self.zoom = (scale / fit).clamp(1.0, MAX_ZOOM);
        } else if let (Some(old_fit), Some(old_stage)) = (self.last_fit, self.last_stage) {
            // A window or the strip changed the opening. A chosen magnification
            // stays, and the sheet stays where it was. Fit still refits.
            if opening_moved(old_stage, stage) {
                let (zoom, pan) =
                    hold_magnification(self.zoom, self.pan, old_fit, fit, old_stage.center(), stage.center());
                self.zoom = zoom;
                self.pan = pan;
            }
        }
        self.last_fit = Some(fit);
        self.last_stage = Some(stage);
        let over_picture = response.hovered() && response.hover_pos().is_none_or(|p| !on_dock(p));
        if over_picture {
            let zoom_delta = ui.input(|i| i.zoom_delta());
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            let factor = if zoom_delta != 1.0 { zoom_delta } else { (scroll / 300.0).exp() };
            if factor != 1.0 {
                let old = self.zoom;
                self.zoom = (self.zoom * factor).clamp(1.0, MAX_ZOOM);
                // Zoom about the pointer: keep the image point under it fixed.
                if let Some(ptr) = response.hover_pos() {
                    let c = stage.center() + self.pan;
                    let rel = ptr - c;
                    self.pan += rel - rel * (self.zoom / old);
                }
            }
        }
        let (uw, uh) = self.inspect_size();
        let ppp = ui.pixels_per_point();
        // Zoom at which one picture pixel is one screen pixel. Without a
        // centimetre size that pixel is a negative pixel; with one, it is a
        // pixel of the fitted print.
        let zoom_1to1 = (uw as f32 / (tw as f32 * fit * ppp)).clamp(1.0, MAX_ZOOM);
        if std::mem::take(&mut self.zoom_to_1to1) {
            // About the viewport centre.
            self.pan *= zoom_1to1 / self.zoom;
            self.zoom = zoom_1to1;
        }
        let place = |zoom: f32, pan: Vec2| {
            let scale = fit * zoom;
            let size = Vec2::new(tw as f32 * scale, th as f32 * scale);
            let frame_pts = frame_tex * scale;
            let black_pts = rings.black_shown() * scale;
            let bounds = pan_bounds(size + 2.0 * frame_pts, stage, room);
            let pan = pan.clamp(bounds.0, bounds.1);
            (sheet_rect(stage, pan, size), frame_pts, black_pts, bounds, pan)
        };
        let (rect_now, _, _, _, _) = place(self.zoom, self.pan);
        // The cross that is drawn is the one that can be grabbed.
        let on_cross = |rect: Rect, p: Pos2| {
            cross.is_some_and(|[nx, ny]| {
                let c =
                    Pos2::new(rect.left() + nx as f32 * rect.width(), rect.top() + ny as f32 * rect.height());
                c.distance(p) <= CROSS_GRAB
            })
        };
        // A double click on the crosshair leaves the zoom alone. One beside
        // it still toggles fit and 1:1.
        let pressed_on_cross =
            ui.input(|i| i.pointer.press_origin()).is_some_and(|p| !on_dock(p) && on_cross(rect_now, p));
        if response.double_clicked() && !pressed_on_cross {
            let old = self.zoom;
            self.zoom = if picture_is_fitted(self.zoom) { zoom_1to1 } else { 1.0 };
            if let Some(ptr) = response.interact_pointer_pos() {
                let c = stage.center() + self.pan;
                let rel = ptr - c;
                self.pan += rel - rel * (self.zoom / old);
            }
            if self.zoom <= 1.0 {
                self.pan = Vec2::ZERO;
            }
        }
        let (mut rect, frame_pts, black_pts, bounds, pan) = place(self.zoom, self.pan);
        self.pan = pan;
        let crosshair_released = self.cross_drag && !response.is_pointer_button_down_on();
        if !response.is_pointer_button_down_on() {
            self.cross_drag = false;
        } else if ui.input(|i| i.pointer.primary_pressed()) {
            self.cross_drag = pressed_on_cross;
            if self.cross_drag {
                self.crop_drag = None;
                self.burn_drag = None;
            }
        }
        if self.cross_drag {
            // The spot follows the pointer. The 1:1 box reads it.
            if let Some(ptr) = ui.input(|i| i.pointer.latest_pos()) {
                if rect.width() > 1.0 && rect.height() > 1.0 {
                    self.sharpen_at = Some([
                        ((ptr.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64,
                        ((ptr.y - rect.top()) / rect.height()).clamp(0.0, 1.0) as f64,
                    ]);
                }
            }
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else {
            if self.crop_open {
                self.crop_interact(ui, &response, rect);
            }
            // The armed pipette has the next click on the picture. The
            // brush under it would take the same press for a stroke, and
            // the card would be read with a burn left in the recipe.
            let brush_on = self.burn_on && !self.pick_card;
            if brush_on {
                self.burn_interact(ui, &response, rect);
            }
            // With the brush locked the first button paints, open it may hold
            // a handle; the others still pan.
            let drawing = brush_on
                && response.dragged_by(egui::PointerButton::Primary)
                && (self.burn_locked || self.burn_drag.is_some());
            if response.dragged() && self.crop_drag.is_none() && self.line_drag.is_none() && !drawing {
                self.pan = (self.pan + response.drag_delta()).clamp(bounds.0, bounds.1);
                (rect, _, _, _, _) = place(self.zoom, self.pan);
            }
        }
        // The strip is the way in. It leaves when a click on the picture is
        // released, or when a drag of the crosshair is released. The close
        // is not written down, so the next launch still opens on the strip.
        let clicked_on_picture = response.clicked()
            && response.interact_pointer_pos().is_some_and(|p| rect.contains(p) && !on_dock(p));
        if !self.question_open() && strip_closes(self.show_strip, clicked_on_picture, crosshair_released) {
            self.show_strip = false;
        }
        // A grey card is read where the picture is clicked while `card` is
        // armed. Esc puts the pipette down. A click lands only while the
        // pixels on the stage are the session's: between a file opening
        // and its first preview the stage still shows the previous one,
        // and the spot clicked there is not a spot of the new negative.
        if self.pick_card {
            // The key is used up here: it puts the pipette down and is not
            // the brush's Escape as well.
            if !self.typing && ui.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                self.pick_card = false;
            } else if clicked_on_picture && self.stage_shows_session() {
                if let Some(ptr) = response.interact_pointer_pos() {
                    let nx = ((ptr.x - rect.left()) / rect.width()).clamp(0.0, 1.0) as f64;
                    let ny = ((ptr.y - rect.top()) / rect.height()).clamp(0.0, 1.0) as f64;
                    self.read_card(nx, ny);
                }
                self.pick_card = false;
            } else if over_picture && response.hover_pos().is_some_and(|p| rect.contains(p)) {
                ui.ctx().set_cursor_icon(CursorIcon::Crosshair);
            }
        }
        // Fitted, the sheet stays in the opening. Zoomed, it paints across
        // the room; the windows are drawn afterwards and lie on it.
        let clip = if picture_is_fitted(self.zoom) { stage } else { room };
        let painter = ui.painter_at(clip);
        ui.painter_at(stage).rect_filled(stage, 0.0, Color32::from_gray(28));
        if frame_pts.x > 0.0 || frame_pts.y > 0.0 {
            painter.rect_filled(rect.expand2(frame_pts), 0.0, Color32::from_gray(white));
        }
        if black_pts.x > 0.0 || black_pts.y > 0.0 {
            painter.rect_filled(rect.expand2(black_pts), 0.0, Color32::from_gray(black));
        }
        painter.image(tex, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);

        // Magnification in screen pixels per picture pixel (negative, or the
        // fitted print when a centimetre size is set).
        let size = rect.size();
        let mag = size.x * ppp / uw.max(1) as f32;
        // The fit copy stays while its texels are smaller than the screen.
        // Past that, the visible window is the file. A fitted print waits
        // until one of its pixels covers a screen pixel: building it earlier
        // would resample the whole sheet on every exposure.
        let pts_per_px = size.x / uw.max(1) as f32;
        let texel_px = size.x * ppp / tw as f32;
        let inspecting = self.inspect_recipe().is_some();
        // 0.999 absorbs the rounding of the 1:1 zoom, which lands on 1.
        let show_file = if inspecting { mag >= 0.999 } else { texel_px > 1.0 };
        // Below 1:1 each screen pixel already shows one file pixel. The step
        // is that ratio, so the window is the size of the screen. At 1:1 the
        // step is 1 and every file pixel in view is rendered.
        let step =
            if inspecting || mag >= 0.999 { 1 } else { (1.0 / mag.max(1.0e-3)).floor().max(1.0) as usize };
        let visible = if show_file && uw > 0 && uh > 0 {
            // Zoomed, this is the whole room, so the sharp picture continues
            // under the windows.
            let vis = clip.intersect(rect);
            let x0 = ((vis.left() - rect.left()) / pts_per_px).floor().max(0.0) as usize;
            let y0 = ((vis.top() - rect.top()) / pts_per_px).floor().max(0.0) as usize;
            let x1 = (((vis.right() - rect.left()) / pts_per_px).ceil() as usize).min(uw);
            let y1 = (((vis.bottom() - rect.top()) / pts_per_px).ceil() as usize).min(uh);
            (x1 > x0 && y1 > y0).then(|| DetailAsk::covering(x0, y0, x1, y1, step, uw, uh)).flatten()
        } else {
            None
        };
        // A pan that stays inside the margin is already rendered. The tile
        // keeps its place on the picture, so the hand just slides it.
        self.wanted_detail = match visible {
            Some(v) if self.detail_rect.is_some_and(|have| have.covers(&v)) => self.detail_rect,
            Some(v) => Some(v.expand(DETAIL_MARGIN, uw, uh)),
            None => None,
        };
        if self.wanted_detail.is_none() {
            self.detail_rect = None;
        }
        if let (Some(dt), Some(dr)) = (&self.detail_texture, self.detail_rect) {
            let step = dr.step() as f32;
            let min = rect.min + Vec2::new(dr.x as f32, dr.y as f32) * pts_per_px;
            let drect =
                Rect::from_min_size(min, Vec2::new(dr.w as f32 * step, dr.h as f32 * step) * pts_per_px);
            painter.image(
                dt.id(),
                drect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        if self.crop_open {
            self.paint_crop(&painter, rect);
            self.paint_line(&painter);
        }
        if self.burn_on {
            self.paint_burns(ui, &painter, &response, rect);
        }
        // After the sharp tile and the crop shade, or the cross disappears
        // under them once the preview would be upscaled.
        if let Some([nx, ny]) = cross {
            let p = Pos2::new(rect.left() + nx as f32 * rect.width(), rect.top() + ny as f32 * rect.height());
            let mark = Color32::from_rgb(255, 210, 60);
            let arm = 7.0;
            painter.line_segment(
                [Pos2::new(p.x - arm, p.y), Pos2::new(p.x + arm, p.y)],
                egui::Stroke::new(1.0, mark),
            );
            painter.line_segment(
                [Pos2::new(p.x, p.y - arm), Pos2::new(p.x, p.y + arm)],
                egui::Stroke::new(1.0, mark),
            );
            let near = response.hover_pos().is_some_and(|h| h.distance(p) <= CROSS_GRAB);
            if self.cross_drag {
                ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
            } else if near && self.crop_drag.is_none() && self.burn_drag.is_none() {
                ui.ctx().set_cursor_icon(CursorIcon::Grab);
                response.clone().on_hover_text("Drag moves the 1:1 box.");
            }
        }

        if self.session.is_some() {
            let txt = if self.detail_rect.is_some() {
                if inspecting {
                    format!("{:.0} % · print pixels", 100.0 * mag)
                } else {
                    format!("{:.0} % · the file's pixels", 100.0 * mag)
                }
            } else {
                format!("{:.0} % of full size", 100.0 * mag)
            };
            let galley =
                painter.layout_no_wrap(txt, egui::FontId::proportional(13.0), Color32::from_gray(220));
            let pos = stage.left_bottom() + Vec2::new(8.0, -8.0 - galley.size().y);
            let bg = Rect::from_min_size(pos, galley.size()).expand(4.0);
            painter.rect_filled(bg, 3.0, Color32::from_black_alpha(150));
            painter.galley(pos, galley, Color32::from_gray(220));
        }
    }
}

fn image_pos(ptr: Pos2, rect: Rect, uw: u32, uh: u32) -> (f32, f32) {
    ((ptr.x - rect.left()) / rect.width() * uw as f32, (ptr.y - rect.top()) / rect.height() * uh as f32)
}

// ---- Burn tool: cursors and labels ---------------------------------------

fn brush_cursor(hit: Hit, held: bool) -> CursorIcon {
    match hit {
        Hit::Axis(0 | 2) => CursorIcon::ResizeHorizontal,
        Hit::Axis(_) => CursorIcon::ResizeVertical,
        Hit::Rotate(_) | Hit::Feather => CursorIcon::PointingHand,
        Hit::Stops => {
            if held {
                CursorIcon::ResizeVertical
            } else {
                CursorIcon::Grab
            }
        }
    }
}

/// One of a row of choices on the stage: the chosen one a pill of the
/// ink with dark text, the others the ink's text on the dark pill.
fn brush_choice(painter: &egui::Painter, p: Pos2, txt: &str, ink: Color32, chosen: bool) {
    let size = Vec2::new(brush::LIGHT_SIZE_PT.0, brush::LIGHT_SIZE_PT.1);
    let pill = Rect::from_center_size(p, size);
    let (fill, text) = if chosen {
        (ink, Color32::from_black_alpha(230))
    } else {
        (Color32::from_black_alpha(150), ink.gamma_multiply(0.8))
    };
    painter.rect_filled(pill, 4.0, fill);
    let galley = painter.layout_no_wrap(txt.to_owned(), egui::FontId::proportional(12.0), text);
    painter.galley(p - galley.size() / 2.0, galley, text);
}

/// Text on a dark pill, centred at `p`.
fn brush_label(painter: &egui::Painter, p: Pos2, txt: &str, ink: Color32, size: f32) {
    let galley = painter.layout_no_wrap(txt.to_owned(), egui::FontId::proportional(size), ink);
    let pos = p - galley.size() / 2.0;
    painter.rect_filled(
        Rect::from_min_size(pos, galley.size()).expand(3.0),
        3.0,
        Color32::from_black_alpha(150),
    );
    painter.galley(pos, galley, ink);
}

/// `corner` and `edge` are half-widths in image pixels. Corners win. Anywhere
/// else inside the frame, including the stroke, moves the window. Outside it
/// the picture pans.
fn hit_crop(px: f32, py: f32, r: CropRect, corner: f32, edge: f32) -> Option<CropHit> {
    let (x, y, w, h) = (r.x as f32, r.y as f32, r.w as f32, r.h as f32);
    let corners = [
        (x, y, CropCorner::Nw),
        (x + w, y, CropCorner::Ne),
        (x + w, y + h, CropCorner::Se),
        (x, y + h, CropCorner::Sw),
    ];
    for (cx, cy, which) in corners {
        if (px - cx).abs() <= corner && (py - cy).abs() <= corner {
            return Some(CropHit::Corner(which));
        }
    }
    let inside = px >= x - edge && px <= x + w + edge && py >= y - edge && py <= y + h + edge;
    if inside {
        Some(CropHit::Move)
    } else {
        None
    }
}

fn corner_xy(r: CropRect, corner: CropCorner) -> (i32, i32) {
    let (x, y, w, h) = (r.x as i32, r.y as i32, r.w as i32, r.h as i32);
    match corner {
        CropCorner::Nw => (x, y),
        CropCorner::Ne => (x + w, y),
        CropCorner::Se => (x + w, y + h),
        CropCorner::Sw => (x, y + h),
    }
}

/// Move or scale `origin` by a pointer delta. A zero delta leaves it where it
/// was grabbed: the corner follows the movement, it does not jump to the pointer.
/// The draft window carried from the frame `old` to the frame `new`: the
/// same share of the frame about the same place, in `aspect`, inside.
/// A window that was the whole frame is the whole new frame.
fn scaled_draft(draft: CropRect, old: (u32, u32), new: (u32, u32), aspect: (u32, u32)) -> CropRect {
    let (ow, oh) = (old.0.max(1) as f64, old.1.max(1) as f64);
    let (nw, nh) = (new.0.max(1), new.1.max(1));
    let (aw, ah) = (u64::from(aspect.0.max(1)), u64::from(aspect.1.max(1)));
    let whole = draft.x == 0 && draft.y == 0 && draft.w >= old.0 && draft.h >= old.1;
    let max = CropRect::inscribed(nw, nh, aspect.0, aspect.1);
    if whole {
        return max;
    }
    let kx = f64::from(nw) / ow;
    let ky = f64::from(nh) / oh;
    let w = ((f64::from(draft.w) * kx).round() as u32).clamp(1, max.w);
    let h = ((u64::from(w) * ah / aw) as u32).clamp(1, max.h);
    let w = ((u64::from(h) * aw / ah) as u32).clamp(1, max.w);
    let cx = (f64::from(draft.x) + f64::from(draft.w) / 2.0) * kx;
    let cy = (f64::from(draft.y) + f64::from(draft.h) / 2.0) * ky;
    let x = (cx - f64::from(w) / 2.0).round().clamp(0.0, f64::from(nw - w)) as u32;
    let y = (cy - f64::from(h) / 2.0).round().clamp(0.0, f64::from(nh - h)) as u32;
    CropRect { x, y, w, h }
}

fn moved_crop(
    origin: CropRect,
    hit: CropHit,
    dx: i32,
    dy: i32,
    img_w: u32,
    img_h: u32,
    aspect: (u32, u32),
) -> CropRect {
    match hit {
        CropHit::Move => origin.shift(dx, dy, img_w, img_h),
        CropHit::Corner(c) => {
            let (cx, cy) = corner_xy(origin, c);
            origin.scale_corner(c, (cx + dx, cy + dy), img_w, img_h, aspect)
        }
    }
}

fn crop_cursor(hit: CropHit, dragging: bool) -> CursorIcon {
    match hit {
        CropHit::Move => {
            if dragging {
                CursorIcon::Grabbing
            } else {
                CursorIcon::Grab
            }
        }
        CropHit::Corner(CropCorner::Nw | CropCorner::Se) => CursorIcon::ResizeNwSe,
        CropHit::Corner(CropCorner::Ne | CropCorner::Sw) => CursorIcon::ResizeNeSw,
    }
}

/// Absolute, and without the Windows `\\?\` prefix `canonicalize` adds.
fn readable_path(path: &Path) -> PathBuf {
    let absolute = std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf());
    without_verbatim_prefix(&absolute)
}

/// `\\?\C:\Users` is `C:\Users`. `\\?\UNC\server\share` is `\\server\share`.
fn without_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest.to_string())
    } else {
        path.to_path_buf()
    }
}

/// `APP_SANDBOX_CONTAINER_ID` is set only inside an App Sandbox. The
/// unsigned Mac zip does not set it, so that build keeps the old folders.
fn sandboxed() -> bool {
    std::env::var_os("APP_SANDBOX_CONTAINER_ID").is_some()
}

/// A breadcrumb step can be opened when it sits inside the sandbox grant.
/// With no grant, a sandboxed build only keeps the current step.
fn crumb_is_open(sandboxed: bool, root: Option<&Path>, anc: &Path, last: bool) -> bool {
    match root {
        Some(root) => anc.starts_with(root),
        None => !sandboxed || last,
    }
}

/// One step of the folder breadcrumb. A directory keeps its name. The root
/// is `/` on Unix and `C:` on Windows.
fn crumb_label(path: &Path) -> String {
    if let Some(name) = path.file_name() {
        let name = name.to_string_lossy();
        if !name.is_empty() {
            return name.into_owned();
        }
    }
    let text = path.to_string_lossy();
    let trimmed = text.trim_end_matches(['\\', '/']);
    if trimmed.is_empty() {
        "/".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Gaps that put the picture group on the centre of the bar and the history
/// group against the status note. A narrow bar keeps a short gap instead.
/// The widest of `8×10` and `10×8`, in the button font.
fn widest_frame_text(ui: &egui::Ui) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    PaperFormat::ALL
        .into_iter()
        .flat_map(|format| [format.label(), format.swapped()])
        .map(|label| ui.painter().layout_no_wrap(label.to_owned(), font.clone(), Color32::WHITE).size().x)
        .fold(0.0_f32, f32::max)
}

/// One cell of the format table. Every ratio fits, so the two columns stay even.
fn frame_side_button_width(ui: &egui::Ui) -> f32 {
    let gaps = ui.spacing().icon_spacing * 2.0;
    let pad = ui.spacing().button_padding.x * 2.0;
    (widest_frame_text(ui) + gaps + pad + 2.0).max(ui.spacing().interact_size.x)
}

/// The closed box. Wide enough for `20×24` and the arrow, so `1:1` does not shrink it.
fn framed_combo_width(ui: &egui::Ui) -> f32 {
    let icon = ui.spacing().icon_spacing + ui.spacing().icon_width;
    let pad = ui.spacing().button_padding.x * 2.0;
    widest_frame_text(ui) + icon + pad + 2.0
}

/// A table cell. The side that is framed is sunk; the others stay ordinary buttons.
fn frame_cell(ui: &mut egui::Ui, label: &str, active: bool, width: f32) -> egui::Response {
    let mut button = egui::Button::new((egui::Atom::grow(), label, egui::Atom::grow())).frame(true);
    if active {
        button =
            button.fill(ui.visuals().extreme_bg_color).stroke(egui::Stroke::new(1.0, Color32::from_gray(96)));
    }
    ui.add_sized([width, ui.spacing().interact_size.y], button)
}

/// One flat tray. The bar stays visible around it, so the three groups separate.
/// The bar between two tools in a well.
fn tool_bar(ui: &mut egui::Ui) {
    ui.add_space(8.0);
    ui.separator();
    ui.add_space(8.0);
}

fn tool_well(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) -> egui::Response {
    let panel = ui.visuals().panel_fill;
    let fill = Color32::from_rgb(
        panel.r().saturating_add(16),
        panel.g().saturating_add(16),
        panel.b().saturating_add(16),
    );
    egui::Frame::new()
        .fill(fill)
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(8, 3))
        .show(ui, |ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            add(ui);
        })
        .response
}

/// Where the strip starts when nothing is remembered: the pictures folder
/// if there is one, else home.
fn start_folder() -> Option<PathBuf> {
    let home = home_dir()?;
    ["Pictures", "Bilder", "Images", "Photos"]
        .iter()
        .map(|d| home.join(d))
        .find(|p| p.is_dir())
        .or(Some(home))
        .filter(|p| p.is_dir())
}

/// The file extensions an export may carry: the first is the one given
/// to a name that has none, the rest are accepted as they are. The print
/// is a TIFF or a JPEG, the chosen format first and the other still
/// accepted when typed; the linear negative is a TIFF under either
/// spelling; the linear plate is a DNG and nothing else.
fn export_extensions(kind: ExportKind, dng: bool, format: ExportFormat) -> &'static [&'static str] {
    match (kind, dng, format) {
        (ExportKind::Linear, true, _) => &["dng"],
        (ExportKind::Linear, false, _) => &["tif", "tiff"],
        (ExportKind::Print, _, ExportFormat::Tiff) => &["tif", "tiff", "jpg", "jpeg"],
        (ExportKind::Print, _, ExportFormat::Jpeg) => &["jpg", "jpeg", "tif", "tiff"],
    }
}

/// Every extension an export can carry. One of these on a name says a
/// format; anything else after a dot is part of the name.
const EXPORT_EXTENSIONS: [&str; 5] = ["tif", "tiff", "jpg", "jpeg", "dng"];

/// `p` with one of `allowed` as its extension: kept when it has one of
/// them already (in any case), else given the first. Another format's
/// extension is replaced by it; a dotted tail that is no format
/// (`frame.v2`) stays, and the extension follows it.
fn with_ext(p: PathBuf, allowed: &[&str]) -> PathBuf {
    let Some(first) = allowed.first() else { return p };
    let among = |list: &[&str]| p.extension().is_some_and(|e| list.iter().any(|a| e.eq_ignore_ascii_case(a)));
    if among(allowed) {
        p
    } else if among(&EXPORT_EXTENSIONS) || p.extension().is_some_and(|e| e.is_empty()) {
        p.with_extension(first)
    } else {
        let mut name = p.into_os_string();
        name.push(".");
        name.push(first);
        PathBuf::from(name)
    }
}

/// Where a named export goes.
#[derive(Debug, PartialEq, Eq)]
enum ExportTarget {
    /// The path is written.
    Write(PathBuf),
    /// The name got its extension after the dialog had asked about the
    /// name as typed, and a file of the full name is there. It is
    /// replaced only on a yes.
    Ask(PathBuf),
    /// The path is the file the picture was developed from. That file is
    /// the negative, and no export writes over it.
    Source(PathBuf),
}

/// The path an export writes for the one `picked` in a dialog, and
/// whether it may. `granted` is a path from the system save panel, which
/// has asked about that very path and is written as it is.
fn export_target(picked: PathBuf, granted: bool, allowed: &[&str], source: &Path) -> ExportTarget {
    let path = if granted { picked.clone() } else { with_ext(picked.clone(), allowed) };
    if same_file(&path, source) {
        ExportTarget::Source(path)
    } else if path != picked && path.exists() {
        ExportTarget::Ask(path)
    } else {
        ExportTarget::Write(path)
    }
}

/// Both paths name one file that is there, whatever the spelling.
fn same_file(a: &Path, b: &Path) -> bool {
    matches!((std::fs::canonicalize(a), std::fs::canonicalize(b)), (Ok(a), Ok(b)) if a == b)
}

/// The linear export's name beside the RAW: `<stem>.dng` for the plate,
/// `<stem>.negative.tif` otherwise. A RAW that is a DNG has that first
/// name itself, so its plate is `<stem>.linear.dng`.
fn linear_export_name(source: Option<&Path>, dng: bool) -> String {
    let stem = source
        .and_then(Path::file_stem)
        .map_or_else(|| "mimizan".into(), |f| f.to_string_lossy().to_string());
    let source_is_dng = source.and_then(Path::extension).is_some_and(|e| e.eq_ignore_ascii_case("dng"));
    match (dng, source_is_dng) {
        (true, true) => format!("{stem}.linear.dng"),
        (true, false) => format!("{stem}.dng"),
        (false, _) => format!("{stem}.negative.tif"),
    }
}

/// Strip: folder bar, film tiles, horizontal scrollbar. A tile is one frame
/// of film: a perforation band above and below the picture, the name as
/// the edge print underneath.
const STRIP_HEIGHT: f32 = 214.0;
/// The strip's top edge is draggable within this range; the frames scale
/// with it (perforation and edge print keep their size, the picture grows).
const STRIP_MIN_H: f32 = 120.0;
const STRIP_MAX_H: f32 = 640.0;
/// Thumbnails further than this many strip widths from the viewport drop
/// their pixels (~150 KB each); the disk cache brings them back. The
/// priority list wants 2.5 widths, so the two never fight.
const THUMB_KEEP_SCREENS: f32 = 4.0;
/// Frame proportions at the default height: 150 x 108 pt picture area.
const TILE_W: f32 = 150.0;
const TILE_IMG_H: f32 = 108.0;
const PERF_H: f32 = 13.0;
const EDGE_H: f32 = 17.0;
const FILM_BASE: Color32 = Color32::from_rgb(16, 15, 14);
const FILM_HOLE: Color32 = Color32::from_rgb(38, 38, 40);

/// Perforation holes along a band.
fn draw_perforation(painter: &egui::Painter, band: Rect) {
    let (hole_w, hole_h, pitch) = (8.0, 6.0, 15.0);
    let n = (band.width() / pitch).floor().max(1.0);
    let start = band.left() + (band.width() - (n - 1.0) * pitch) * 0.5;
    for i in 0..n as usize {
        let c = Pos2::new(start + i as f32 * pitch, band.center().y);
        let r = Rect::from_center_size(c, Vec2::new(hole_w, hole_h));
        painter.rect_filled(r, 1.5, FILM_HOLE);
        painter.rect_stroke(
            r,
            1.5,
            egui::Stroke::new(1.0, Color32::from_white_alpha(10)),
            egui::StrokeKind::Inside,
        );
    }
}

impl App {
    /// `B` toggles the strip; `←`/`→` open the neighbouring file when the
    /// keyboard is not in a text field, whether or not the strip is up.
    fn strip_keys(&mut self, ctx: &egui::Context) {
        if self.keys_are_taken(ctx) {
            return;
        }
        let (b, left, right) = ctx.input(|i| {
            let plain = i.modifiers.is_none();
            (
                i.key_pressed(egui::Key::B) && plain,
                i.key_pressed(egui::Key::ArrowLeft) && plain,
                i.key_pressed(egui::Key::ArrowRight) && plain,
            )
        });
        if b {
            self.toggle_strip();
        }
        if left {
            self.open_neighbour(-1);
        }
        if right {
            self.open_neighbour(1);
        }
    }

    fn strip_bar(&mut self, ui: &mut egui::Ui) {
        let mut go_to: Option<PathBuf> = None;
        let mut rescan = false;
        #[cfg(target_os = "macos")]
        let mut choose = false;
        #[cfg(not(target_os = "macos"))]
        let choose = false;
        let grant_root = self.grant_root();
        ui.horizontal(|ui| {
            #[cfg(target_os = "macos")]
            if ui.small_button("Open").on_hover_text("Choose a folder of negatives").clicked() {
                choose = true;
            }
            match &self.folder {
                None => {
                    let text = if sandboxed() {
                        "No folder. Open a folder, or drop one on the window."
                    } else {
                        "No folder. Drop a folder or a RAW file on the window."
                    };
                    ui.label(RichText::new(text).weak());
                }
                Some(f) => {
                    // Breadcrumb: every ancestor is a button. The root is `/`
                    // on Unix and the drive (`C:`) on Windows. Above a sandbox
                    // grant the steps stay visible and are not buttons.
                    let comps: Vec<PathBuf> = f.path.ancestors().map(Path::to_path_buf).collect();
                    for anc in comps.iter().rev() {
                        let name = crumb_label(anc);
                        if name.is_empty() {
                            continue;
                        }
                        let last = anc == &f.path;
                        let allowed = crumb_is_open(sandboxed(), grant_root.as_deref(), anc, last);
                        if !allowed {
                            ui.label(RichText::new(name).weak());
                        } else {
                            let text = if last { RichText::new(name).strong() } else { RichText::new(name) };
                            if ui.add(egui::Button::new(text).frame(false)).clicked() && !last {
                                go_to = Some(anc.clone());
                            }
                        }
                        if !last {
                            ui.label(RichText::new("›").weak());
                        }
                    }
                    // Going down: the subfolders of the current one as a menu.
                    if !f.subdirs.is_empty() {
                        let n = f.subdirs.len();
                        ui.menu_button(RichText::new(format!("› {n} ⏷")).weak(), |ui| {
                            ui.set_min_width(160.0);
                            egui::ScrollArea::vertical().max_height(320.0).show(ui, |ui| {
                                for d in &f.subdirs {
                                    if ui.button(&d.name).clicked() {
                                        go_to = Some(d.path.clone());
                                        ui.close();
                                    }
                                }
                            });
                        })
                        .response
                        .on_hover_text(format!("{n} subfolder{}", if n == 1 { "" } else { "s" }));
                    }
                    ui.label(RichText::new(format!("{} RAW", f.entries.len())).weak());
                    if ui.small_button("↻").on_hover_text("Read the folder again").clicked() {
                        rescan = true;
                    }
                }
            }
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.small_button("×").on_hover_text("Hide the strip (B)").clicked() {
                    self.toggle_strip();
                }
                let plate = self.plate_process();
                let negative = self.strip_inverted();
                let tip = if plate {
                    "Show the frames as slides, or inverted, a colour negative without its mask"
                } else {
                    "Show the frames as the film holds them (inverted), or as the print"
                };
                if ui.selectable_label(negative, "negative").on_hover_text(tip).clicked() {
                    if plate {
                        self.strip_negative_plate = !negative;
                    } else {
                        self.strip_negative = !negative;
                    }
                    self.strip_textures_drop();
                    self.persist();
                }
                let mut sort = self.sort;
                egui::ComboBox::from_id_salt("strip-sort").selected_text(sort.label()).width(70.0).show_ui(
                    ui,
                    |ui| {
                        for s in Sort::ALL {
                            ui.selectable_value(&mut sort, s, s.label());
                        }
                    },
                );
                if sort != self.sort {
                    self.sort = sort;
                    if let Some(f) = &mut self.folder {
                        f.resort(sort);
                    }
                    self.last_wanted.clear();
                    self.scroll_to_selected = true;
                    self.persist();
                }
                ui.label(RichText::new("sort").weak());
            });
        });
        if choose {
            #[cfg(target_os = "macos")]
            self.choose_folder();
        } else if let Some(p) = go_to {
            self.set_folder(&p);
        } else if rescan {
            if let Some(p) = self.folder.as_ref().map(|f| f.path.clone()) {
                self.set_folder(&p);
            }
        }
    }

    fn strip_panel(&mut self, ui: &mut egui::Ui) {
        ui.add_space(2.0);
        self.strip_bar(ui);
        ui.add_space(2.0);
        // Subfolder counts arriving from the background.
        if let Some(rx) = &self.dir_counts {
            while let Ok((generation, path, n)) = rx.try_recv() {
                if let Some(f) = self.folder.as_mut().filter(|f| f.generation == generation) {
                    if let Some(d) = f.subdirs.iter_mut().find(|d| d.path == path) {
                        d.raws = Some(n);
                    }
                }
            }
        }
        let plate = self.plate_process();
        let negative = self.strip_inverted();
        let Some(folder) = &mut self.folder else { return };
        if folder.entries.is_empty() && folder.subdirs.is_empty() {
            ui.label(RichText::new("No RAW files and no folders here.").weak());
            return;
        }
        let idle = self.busy.is_none();
        let current = self.current_path.clone();
        let generation = folder.generation;
        let scroll_to = std::mem::take(&mut self.scroll_to_selected);
        let mut clicked: Option<PathBuf> = None;
        let mut go_dir: Option<PathBuf> = None;
        // Names whose thumbnail buffer does not match its size. Logged once
        // the strip is done borrowing the folder.
        let mut bad_thumbs: Vec<String> = Vec::new();
        // (distance from the viewport centre, index) of tiles that still
        // need a thumbnail; visible ones first, then two screens around.
        let mut wanted: Vec<(f32, usize)> = Vec::new();
        let ctx = ui.ctx().clone();
        // Frame geometry follows the panel height: whatever is left under
        // the bar (minus the scrollbar) is one film frame.
        let tile_h = (ui.available_height() - ui.spacing().scroll.allocated_width()).max(60.0);
        let img_h = (tile_h - 2.0 * PERF_H - EDGE_H).max(30.0);
        let tile_w = (img_h * TILE_W / TILE_IMG_H).round();
        // The frames scroll on one axis. A vertical wheel over them moves
        // that axis, the same travel as the bar (egui sums both wheel axes
        // when only one direction is enabled). Over the picture the wheel
        // still zooms.
        ui.style_mut().always_scroll_the_only_direction = true;
        egui::ScrollArea::horizontal().id_salt("strip-scroll").show(ui, |ui| {
            let clip = ui.clip_rect();
            let centre = clip.center().x;
            // The film base runs under every frame.
            let film = Rect::from_min_size(
                ui.cursor().min,
                Vec2::new(ui.available_width().max(clip.width()), tile_h),
            );
            ui.painter().rect_filled(film, 0.0, FILM_BASE);
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 0.0;
                // Folders first, as tiles of the frame's size (no perforation:
                // a folder is not film). Click goes down into it.
                for d in &folder.subdirs {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(tile_w, tile_h), Sense::click());
                    if !clip.intersects(rect) {
                        continue;
                    }
                    let painter = ui.painter();
                    painter.rect_filled(rect, 0.0, FILM_BASE);
                    let frame = Rect::from_min_size(
                        Pos2::new(rect.left(), rect.top() + PERF_H),
                        Vec2::new(tile_w, img_h),
                    );
                    if resp.hovered() {
                        painter.rect_filled(frame, 0.0, Color32::from_white_alpha(10));
                    }
                    let flap = draw_folder(
                        painter,
                        frame.shrink2(Vec2::new(tile_w * 0.18, img_h * 0.16)),
                        resp.hovered(),
                    );
                    let count = match d.raws {
                        Some(0) => String::new(),
                        Some(n) => format!("{n} RAW"),
                        None => "…".to_string(),
                    };
                    if !count.is_empty() {
                        // On the front flap, dark on the light card.
                        painter.text(
                            flap.center(),
                            egui::Align2::CENTER_CENTER,
                            count,
                            egui::FontId::proportional(11.0),
                            Color32::from_rgb(50, 38, 18),
                        );
                    }
                    painter.text(
                        Pos2::new(rect.center().x, rect.max.y - EDGE_H * 0.5),
                        egui::Align2::CENTER_CENTER,
                        truncate_middle(&d.name, (tile_w / 7.5) as usize),
                        egui::FontId::proportional(11.0),
                        Color32::from_rgb(200, 190, 170),
                    );
                    painter.line_segment(
                        [rect.right_top(), rect.right_bottom()],
                        egui::Stroke::new(1.0, Color32::from_white_alpha(8)),
                    );
                    if resp.on_hover_text(d.path.display().to_string()).clicked() {
                        go_dir = Some(d.path.clone());
                    }
                }
                for (i, e) in folder.entries.iter_mut().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(tile_w, tile_h), Sense::click());
                    let is_current = current.as_deref() == Some(e.path.as_path());
                    if is_current && scroll_to {
                        ui.scroll_to_rect(rect, Some(egui::Align::Center));
                    }
                    let visible = clip.intersects(rect);
                    let dist = (rect.center().x - centre).abs();
                    if e.state == ThumbState::Pending && dist < clip.width() * 2.5 {
                        wanted.push((dist, i));
                    }
                    if !visible {
                        e.texture = None;
                        // Far off screen the pixels go too; they come back
                        // from the disk cache when the tile scrolls near.
                        if e.state == ThumbState::Ready && dist > clip.width() * THUMB_KEEP_SCREENS {
                            e.thumb = None;
                            e.state = ThumbState::Pending;
                            e.let_go += 1;
                        }
                        continue;
                    }
                    if e.texture.is_none() {
                        let bad = e.thumb.as_ref().is_some_and(|t| {
                            let n = t.width.saturating_mul(t.height);
                            t.width == 0 || t.height == 0 || t.gray.len() != n || t.rgb.len() != 3 * n
                        });
                        if bad {
                            // Same failure as the 1:1 window: from_gray closes
                            // the process. Drop the buffer so the next frame
                            // does not try again.
                            bad_thumbs.push(e.name.clone());
                            e.thumb = None;
                            e.state = ThumbState::Failed;
                        } else if let Some(t) = &e.thumb {
                            let size = [t.width, t.height];
                            let img = match (plate, negative) {
                                (true, false) => ColorImage::from_rgb(size, &t.rgb),
                                (true, true) => {
                                    let inv: Vec<u8> = t.rgb.iter().map(|&v| 255 - v).collect();
                                    ColorImage::from_rgb(size, &inv)
                                }
                                (false, true) => {
                                    ColorImage::from_gray_iter(size, t.gray.iter().map(|&g| 255 - g))
                                }
                                (false, false) => ColorImage::from_gray(size, &t.gray),
                            };
                            e.texture = Some(ctx.load_texture(
                                format!("thumb-{generation}-{i}-{}{}", plate as u8, negative as u8),
                                img,
                                TextureOptions::LINEAR,
                            ));
                        }
                    }
                    let painter = ui.painter();
                    let vis = ui.visuals();
                    painter.rect_filled(rect, 0.0, FILM_BASE);
                    // Perforation above and below the frame.
                    draw_perforation(painter, Rect::from_min_size(rect.min, Vec2::new(tile_w, PERF_H)));
                    draw_perforation(
                        painter,
                        Rect::from_min_size(
                            Pos2::new(rect.left(), rect.top() + PERF_H + img_h),
                            Vec2::new(tile_w, PERF_H),
                        ),
                    );
                    let frame = Rect::from_min_size(
                        Pos2::new(rect.left(), rect.top() + PERF_H),
                        Vec2::new(tile_w, img_h),
                    );
                    let img_rect = frame.shrink2(Vec2::new(5.0, 2.0));
                    if resp.hovered() && !is_current {
                        painter.rect_filled(frame, 0.0, Color32::from_white_alpha(10));
                    }
                    match &e.texture {
                        Some(tex) => {
                            let [w, h] = tex.size();
                            let scale = (img_rect.width() / w as f32).min(img_rect.height() / h as f32);
                            let size = Vec2::new(w as f32 * scale, h as f32 * scale);
                            let r = Rect::from_center_size(img_rect.center(), size);
                            painter.image(
                                tex.id(),
                                r,
                                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                        None => {
                            let (txt, col) = match e.state {
                                ThumbState::Failed => ("?", vis.warn_fg_color),
                                _ => ("…", vis.weak_text_color()),
                            };
                            painter.text(
                                img_rect.center(),
                                egui::Align2::CENTER_CENTER,
                                txt,
                                egui::FontId::proportional(22.0),
                                col,
                            );
                        }
                    }
                    // Frame separator.
                    painter.line_segment(
                        [rect.right_top(), rect.right_bottom()],
                        egui::Stroke::new(1.0, Color32::from_white_alpha(8)),
                    );
                    if is_current {
                        painter.rect_stroke(
                            frame.shrink(1.0),
                            2.0,
                            egui::Stroke::new(2.0, Color32::from_rgb(255, 170, 60)),
                            egui::StrokeKind::Inside,
                        );
                    }
                    // Edge print: the name in the film's orange.
                    let label_pos = Pos2::new(rect.center().x, rect.max.y - EDGE_H * 0.5);
                    let name = truncate_middle(&e.name, (tile_w / 7.5) as usize);
                    painter.text(
                        label_pos,
                        egui::Align2::CENTER_CENTER,
                        name,
                        egui::FontId::proportional(11.0),
                        if is_current {
                            Color32::from_rgb(255, 190, 90)
                        } else {
                            Color32::from_rgb(170, 130, 70)
                        },
                    );
                    let resp = resp.on_hover_ui(|ui| {
                        ui.label(RichText::new(&e.name).strong());
                        if let Some(m) = &e.meta {
                            ui.label(format!("{} {}", m.make.trim(), m.model.trim()));
                            let ex = m.exposure.summary();
                            if !ex.is_empty() {
                                ui.label(ex);
                            }
                            if let Some(d) = &m.exposure.captured {
                                ui.label(d.replacen(':', "-", 2));
                            }
                            ui.label(
                                RichText::new(format!(
                                    "{} x {} px, {:.1} MB",
                                    m.width,
                                    m.height,
                                    e.size as f64 / 1e6
                                ))
                                .weak(),
                            );
                        } else {
                            ui.label(RichText::new(format!("{:.1} MB", e.size as f64 / 1e6)).weak());
                        }
                        if !idle {
                            ui.label(RichText::new("busy — wait for the current file").weak());
                        }
                    });
                    if resp.clicked() && idle && !is_current {
                        clicked = Some(e.path.clone());
                    }
                }
            });
        });
        wanted.sort_by(|a, b| a.0.total_cmp(&b.0));
        // Compared without allocating; the list is rebuilt only when it changed.
        let same = wanted.len() == self.last_wanted.len()
            && wanted.iter().zip(&self.last_wanted).all(|((_, i), p)| folder.entries[*i].path == *p);
        if !same {
            let jobs: Vec<ThumbJob> = wanted
                .iter()
                .map(|(_, i)| {
                    let e = &folder.entries[*i];
                    ThumbJob {
                        generation,
                        path: e.path.clone(),
                        size: e.size,
                        modified: e.modified,
                        let_go: e.let_go,
                    }
                })
                .collect();
            self.last_wanted = jobs.iter().map(|j| j.path.clone()).collect();
            self.thumbs.want(jobs);
        }
        if !bad_thumbs.is_empty() {
            self.log(format!("thumbnail was the wrong size: {}", bad_thumbs.join(", ")));
        }
        if let Some(p) = clicked {
            self.open(p);
        }
        if let Some(p) = go_dir {
            self.set_folder(&p);
        }
    }
}

/// A folder as the file managers draw it: tab, body, a lighter front flap.
/// Paints a folder icon into `r` and returns its front flap, the place for a
/// label.
fn draw_folder(painter: &egui::Painter, r: Rect, hovered: bool) -> Rect {
    let back = if hovered { Color32::from_rgb(150, 120, 70) } else { Color32::from_rgb(120, 96, 56) };
    let front = if hovered { Color32::from_rgb(205, 170, 100) } else { Color32::from_rgb(180, 146, 84) };
    let tab_w = r.width() * 0.38;
    let tab_h = (r.height() * 0.14).max(4.0);
    painter.rect_filled(
        Rect::from_min_size(r.left_top(), Vec2::new(tab_w, tab_h + 3.0)),
        egui::CornerRadius { nw: 3, ne: 3, sw: 0, se: 0 },
        back,
    );
    painter.rect_filled(
        Rect::from_min_max(Pos2::new(r.left(), r.top() + tab_h), r.right_bottom()),
        3.0,
        back,
    );
    let flap = Rect::from_min_max(Pos2::new(r.left(), r.top() + tab_h + r.height() * 0.22), r.right_bottom());
    painter.rect_filled(flap, 3.0, front);
    flap
}

/// `DSC_0001_very_long_name.NEF` -> `DSC_0001_…name.NEF` within `max` chars.
fn truncate_middle(s: &str, max: usize) -> String {
    let n = s.chars().count();
    if n <= max {
        return s.to_string();
    }
    let keep_end = 8.min(max / 2);
    let keep_start = max.saturating_sub(keep_end + 1);
    let start: String = s.chars().take(keep_start).collect();
    let end: String = s.chars().skip(n - keep_end).collect();
    format!("{start}…{end}")
}

impl eframe::App for App {
    /// Worker results are collected here: eframe calls `logic` even while the
    /// window is hidden or covered, so an export finishes and logs regardless.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.start_redevelop();
        self.handle_events(ctx);
        self.guard_close(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.loupe_open = false;
        self.ensure_loupe_point();
        self.follow_process();
        self.refresh_plate();
        self.handle_dialog(&ctx);

        egui::Panel::top("toolbar").show(ui, |ui| {
            ui.add_space(2.0);
            self.toolbar(ui);
            ui.add_space(2.0);
        });
        if self.show_strip {
            let r = egui::Panel::bottom("strip")
                .resizable(true)
                .default_size(self.strip_height)
                .size_range(STRIP_MIN_H..=STRIP_MAX_H)
                .frame(egui::Frame::new().fill(FILM_BASE).inner_margin(egui::Margin::symmetric(6, 0)))
                .show(ui, |ui| self.strip_panel(ui));
            self.strip_height = r.response.rect.height();
            // Written once the drag has ended, not on every frame of it.
            if (self.strip_height - self.strip_height_saved).abs() > 0.5
                && !ctx.input(|i| i.pointer.any_down())
            {
                self.strip_height_saved = self.strip_height;
                self.persist();
            }
        }
        // Fitted, the picture sits in the opening. Zoomed, it fills this
        // room and Negative, Enlarger and Print lie on it. The strip is a
        // panel of its own, outside this room.
        let fill = ui.visuals().panel_fill;
        let docks = egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(fill))
            .show(ui, |ui| {
                let full = ui.available_rect_before_wrap();
                let docks = self.docks(full);
                let covered = [docks.left, docks.right, docks.bottom];
                ui.scope_builder(egui::UiBuilder::new().max_rect(full), |ui| {
                    self.preview_panel(ui, docks.stage, covered);
                });
                self.dock_windows(ui, &docks);
                docks
            })
            .inner;
        self.waiting = [
            self.panels[0] && docks.left.is_none(),
            self.panels[1] && docks.right.is_none(),
            self.panels[2] && docks.bottom.is_none(),
        ];

        self.edit_keys(&ctx);
        self.clear_desktop_sharpen();
        self.commit_history(&ctx);
        // The panels may have changed the plate this frame; resolve before asking.
        self.refresh_plate();
        self.request_preview_if_needed();
        // After every slider. The print window is drawn before the enlarger,
        // so a request from inside it would still carry the previous exposure.
        if self.loupe_open {
            self.request_loupe();
        }
        if self.peak_arm {
            self.peak_arm = false;
            // Always measure. A saved peak stays in the fields (`peak_held`);
            // the result is kept beside them so reset can put it back.
            self.start_peak_tune(&ctx);
        }
        self.strip_keys(&ctx);
        self.save_prompt(&ctx);
        self.replace_prompt(&ctx);
        // For the keys of the next frame.
        self.typing = text_edit_focused(&ctx);
    }
}

/// The docked windows' places for one frame.
struct Docks {
    left: Option<Rect>,
    right: Option<Rect>,
    bottom: Option<Rect>,
    /// What is left for the picture.
    stage: Rect,
}

/// Width of the side panels, points.
const PANEL_W: f32 = 318.0;
/// Width the enlarger controls are drawn at. The glass may be narrower
/// and then clips them. Nothing in the enlarger wraps to fit.
fn enlarger_content_width(ui: &egui::Ui) -> f32 {
    let body = egui::TextStyle::Body.resolve(ui.style());
    let paint =
        |text: &str| ui.painter().layout_no_wrap(text.to_owned(), body.clone(), Color32::WHITE).size().x;
    let grade = paint("≈ grade 00 · 00.0 stops of scene");
    let strip = filter_strip::min_width(ui, &MIX_LABELS);
    grade.max(strip)
}

/// The fields of the monochrome mix, for the width they need.
const MIX_LABELS: [&str; 8] = ["Native", "Pan", "Y8", "YG11", "O16", "R25", "G58", "B47"];

/// Gap between the stage edge and a panel.
const PANEL_GAP: f32 = 12.0;
/// Shortest outer height that still holds the name and the ×.
const DOCK_MIN_H: f32 = 48.0;
/// Narrowest enlarger that still holds its name and the ×.
const ENLARGER_MIN_W: f32 = 64.0;
/// Grab radius of the crosshair, points. Covers its arms.
const CROSS_GRAB: f32 = 14.0;

/// A text field has the keyboard. egui's own `wants_keyboard_input` is
/// also true while a button holds focus (after Tab), which would mute every
/// shortcut; only a text edit should.
fn text_edit_focused(ctx: &egui::Context) -> bool {
    let Some(id) = ctx.memory(|m| m.focused()) else { return false };
    egui::TextEdit::load_state(ctx, id).is_some()
}

/// Name and ×.
fn panel_heading(ui: &mut egui::Ui, title: &str) -> bool {
    let mut hide = false;
    ui.horizontal(|ui| {
        ui.label(RichText::new(title).strong());
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let button = ui.small_button("×").on_hover_text("Hide this panel");
            if button.clicked() {
                hide = true;
            }
        });
    });
    ui.separator();
    hide
}

/// What a length field holds underneath: the recipe's centimetres (the
/// picture) or millimetres (the rings).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Base {
    Cm,
    Mm,
}

impl Base {
    fn per_inch(self) -> f64 {
        match self {
            Base::Cm => 2.54,
            Base::Mm => 25.4,
        }
    }
}

/// Shown = held × factor: from a length held in `base` to the ruler's
/// unit, through `dpi` for pixels. Dividing by it takes a typed length
/// back; a dpi below 1 counts as 1, so the factor is never 0.
fn ruler_factor(ruler: Ruler, base: Base, dpi: f64) -> f64 {
    match ruler {
        Ruler::Metric => 1.0,
        Ruler::Imperial => 1.0 / base.per_inch(),
        Ruler::Pixels => dpi.max(1.0) / base.per_inch(),
    }
}

/// A length on the ruler. `value` stays in `base`; the field shows and
/// takes it in the ruler's unit, converted through `dpi` for pixels.
/// `range` is in `base`. The same widget whichever unit is on, so the
/// rows keep their place when the ruler changes.
fn length_field(
    ui: &mut egui::Ui,
    value: &mut f64,
    base: Base,
    ruler: Ruler,
    dpi: f64,
    range: std::ops::RangeInclusive<f64>,
    enabled: bool,
) -> egui::Response {
    let k = ruler_factor(ruler, base, dpi);
    let (decimals, speed, suffix) = match (ruler, base) {
        (Ruler::Metric, Base::Cm) => (1, 0.1, " cm"),
        (Ruler::Metric, Base::Mm) => (1, 0.2, " mm"),
        (Ruler::Imperial, Base::Cm) => (2, 0.05, " in"),
        (Ruler::Imperial, Base::Mm) => (2, 0.01, " in"),
        (Ruler::Pixels, _) => (0, 1.0, " px"),
    };
    let mut shown = *value * k;
    // Typed text is taken on Enter or when the field is left, not per
    // keystroke: the value is read back from the layout after every
    // change, and egui drops text whose value moved under it, so a live
    // update would rewrite "1" to the clamped minimum before the next
    // digit. Dragging still updates as it moves.
    let response = ui.add_enabled(
        enabled,
        egui::DragValue::new(&mut shown)
            .update_while_editing(false)
            .range(range.start() * k..=range.end() * k)
            .clamp_existing_to_range(false)
            .speed(speed)
            .fixed_decimals(decimals)
            .suffix(suffix),
    );
    if response.changed() {
        *value = shown / k;
    }
    response
}

/// Left column of the print dock. Wide enough for the longest word, so
/// dpi, border and resample start their numbers on the same line.
fn print_field_label(ui: &mut egui::Ui, text: &str) {
    let font = egui::TextStyle::Body.resolve(ui.style());
    let width = ui.painter().layout_no_wrap("resample".to_owned(), font.clone(), Color32::WHITE).size().x;
    let (rect, _) = ui.allocate_exact_size(Vec2::new(width, ui.spacing().interact_size.y), Sense::hover());
    if text.is_empty() {
        return;
    }
    ui.painter().text(
        rect.left_center(),
        egui::Align2::LEFT_CENTER,
        text,
        font,
        ui.visuals().weak_text_color(),
    );
}

/// The 1:1 slot when it has no pixels yet. Same square the picture will fill.
fn print_frame(ui: &mut egui::Ui, text: &str) {
    let w = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(Vec2::new(w, w), Sense::hover());
    ui.painter().rect(
        rect,
        0.0,
        Color32::from_gray(20),
        egui::Stroke::new(1.0, Color32::from_white_alpha(20)),
        egui::StrokeKind::Inside,
    );
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        text,
        egui::TextStyle::Body.resolve(ui.style()),
        ui.visuals().weak_text_color(),
    );
}

/// The body of a side dock, under the title line. The window no longer
/// scrolls as a whole, so the title stays put.
fn dock_scroll(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let height = ui.available_height();
    egui::ScrollArea::vertical().max_height(height).auto_shrink([false, false]).show(ui, |ui| add(ui));
}

/// The negative window scrolls. One quiet line stays centred on its floor.
/// A tall window leaves the dark glass between the CPU section and the name.
/// A window too short for that line scrolls as before, so the name is never cut.
fn negative_body(ui: &mut egui::Ui, add: impl FnOnce(&mut egui::Ui)) {
    let credit_h = ui.text_style_height(&egui::TextStyle::Small);
    let spacing = ui.spacing().item_spacing.y;
    let room = ui.available_height().max(0.0);
    let reserve = credit_h + spacing;
    if room <= reserve {
        dock_scroll(ui, add);
        return;
    }
    // The default minimum would hold the scroll open and push the line
    // through the floor of a short window.
    egui::ScrollArea::vertical()
        .max_height(room - reserve)
        .min_scrolled_height(0.0)
        .auto_shrink([false, false])
        .show(ui, add);
    let width = ui.available_width();
    let (rect, _) = ui.allocate_exact_size(egui::vec2(width, credit_h), egui::Sense::hover());
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        "© 2026 Dragan Bojovic",
        egui::TextStyle::Small.resolve(ui.style()),
        ui.visuals().weak_text_color(),
    );
}

/// The dark translucent glass the panels are made of. Plain alpha, no
/// backdrop blur: it has to look the same on every GPU driver and in software.
/// The shadow stays inside the dock; the clip is set before this is painted.
fn glass_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(14, 14, 16, 222))
        .stroke(egui::Stroke::new(1.0, Color32::from_white_alpha(20)))
        .corner_radius(10.0)
        .inner_margin(12.0)
        .shadow(egui::Shadow { offset: [0, 6], blur: 26, spread: 0, color: Color32::from_black_alpha(150) })
}

/// Glass painted on `rect`, contents clipped to it. Drawn in the darkroom,
/// not as a floating area: an area remembers last frame's size and, while
/// the window is being resized, paints a faint copy beside the new rectangle.
/// The clip is the dock, set before the glass, so the shadow cannot leave it.
/// The returned height is the outer height the contents wanted, frame included.
fn show_dock(
    ui: &mut egui::Ui,
    salt: &str,
    rect: Rect,
    title: &str,
    add: impl FnOnce(&mut egui::Ui),
) -> (bool, f32) {
    let frame = glass_frame();
    let margin = frame.total_margin();
    let inner = rect - margin;
    let mut hide = false;
    let mut content_h = 0.0;
    ui.scope_builder(
        egui::UiBuilder::new()
            .id(egui::Id::new(salt))
            .max_rect(rect)
            .layout(egui::Layout::top_down(egui::Align::Min)),
        |ui| {
            ui.set_clip_rect(rect);
            ui.allocate_rect(rect, Sense::click_and_drag());
            ui.painter().add(frame.paint(inner));
            let mut child = ui.new_child(
                egui::UiBuilder::new().max_rect(inner).layout(egui::Layout::top_down(egui::Align::Min)),
            );
            child.set_clip_rect(inner);
            hide = panel_heading(&mut child, title);
            add(&mut child);
            content_h = child.min_rect().height();
        },
    );
    (hide, content_h + margin.sum().y)
}

/// Zoomed means the sheet has left the fit. The threshold absorbs the
/// rounding of a zoom that lands on 1.
fn picture_is_fitted(zoom: f32) -> bool {
    zoom <= 1.0 + 1e-3
}

/// The opening changed enough that a fitted sheet would move.
fn opening_moved(old: Rect, new: Rect) -> bool {
    (old.min - new.min).length_sq() > 0.25 || (old.max - new.max).length_sq() > 0.25
}

/// Keep a magnification the user set when the opening changes. The absolute
/// scale `fit * zoom` stays, and the pan holds the sheet still on screen.
/// A fitted sheet is left alone, so it can sit in the new opening.
fn hold_magnification(
    zoom: f32,
    pan: Vec2,
    old_fit: f32,
    new_fit: f32,
    old_center: Pos2,
    new_center: Pos2,
) -> (f32, Vec2) {
    if picture_is_fitted(zoom) || !(old_fit > 0.0 && new_fit > 0.0) {
        return (zoom, pan);
    }
    let zoom = ((old_fit * zoom) / new_fit).clamp(1.0, MAX_ZOOM);
    if picture_is_fitted(zoom) {
        (zoom, Vec2::ZERO)
    } else {
        (zoom, pan + (old_center - new_center))
    }
}

/// How far the sheet may slide and still cover the opening. The rest runs
/// past the opening, into the room and under the windows.
fn pan_limit(sheet: Vec2, opening: Vec2) -> Vec2 {
    ((sheet - opening) * 0.5).max(Vec2::ZERO)
}

/// The pan's low and high bound: the sheet covers the opening
/// ([`pan_limit`]), and on each axis where it fits in the room it stays
/// inside the room. Centred on an opening that sits high in the room (the
/// enlarger below it), a sheet taller than the opening would otherwise run
/// out above the room, where nothing paints it, while the room below lay
/// empty: the top of the print was cut off at 1:1. Only when the sheet is
/// taller than the room itself does it run past both edges.
fn pan_bounds(sheet: Vec2, opening: Rect, room: Rect) -> (Vec2, Vec2) {
    let limit = pan_limit(sheet, opening.size());
    let (mut lo, mut hi) = (-limit, limit);
    let c = opening.center();
    // `c + pan ± sheet / 2` within the room.
    let in_lo = room.min - c + sheet * 0.5;
    let in_hi = room.max - c - sheet * 0.5;
    for axis in 0..2 {
        if sheet[axis] <= room.size()[axis] {
            lo[axis] = lo[axis].max(in_lo[axis]);
            hi[axis] = hi[axis].min(in_hi[axis]);
            // An opening that reaches out of the room (it never does) would
            // leave nothing; the nearer bound then stands for both.
            if lo[axis] > hi[axis] {
                let mid = 0.5 * (lo[axis] + hi[axis]);
                lo[axis] = mid;
                hi[axis] = mid;
            }
        }
    }
    (lo, hi)
}

/// The sheet stays centred on the opening. Pan moves it inside [`pan_bounds`].
fn sheet_rect(opening: Rect, pan: Vec2, size: Vec2) -> Rect {
    Rect::from_center_size(opening.center() + pan, size)
}

/// The strip closes on the release of a click on the picture, or on the
/// release of a drag that started on the crosshair. It is not written down.
/// Called only while a picture is up.
fn strip_closes(show: bool, clicked_on_picture: bool, crosshair_released: bool) -> bool {
    show && (clicked_on_picture || crosshair_released)
}

/// True when the rectangles share an interior. A shared edge is room, not a stack.
fn docks_cross(a: Rect, b: Rect) -> bool {
    a.right() > b.left() && b.right() > a.left() && a.bottom() > b.top() && b.bottom() > a.top()
}

/// The three windows, or none where the rectangle would cross another window
/// or leave the darkroom. Print keeps the right edge. Negative yields first.
/// The enlarger exists only in the gap that remains. An omitted window stays
/// open in `open`: the toolbar brings it back when the room returns.
fn place_docks(full: Rect, open: [bool; 3], enlarger_h: f32) -> Docks {
    let gap = PANEL_GAP;
    let h = full.height() - 2.0 * gap;
    let columns = h >= DOCK_MIN_H;
    let left_rect = Rect::from_min_size(full.left_top() + Vec2::splat(gap), Vec2::new(PANEL_W, h));
    let right_rect =
        Rect::from_min_size(Pos2::new(full.right() - gap - PANEL_W, full.top() + gap), Vec2::new(PANEL_W, h));
    let inside = |r: Rect| {
        r.width() > 0.0
            && r.height() > 0.0
            && full.min.x <= r.min.x
            && full.min.y <= r.min.y
            && r.max.x <= full.max.x
            && r.max.y <= full.max.y
    };
    let mut show_left = open[0] && columns && inside(left_rect);
    let show_right = open[1] && columns && inside(right_rect);
    if show_left && show_right && docks_cross(left_rect, right_rect) {
        show_left = false;
    }
    let left = show_left.then_some(left_rect);
    let right = show_right.then_some(right_rect);
    let x0 = left.map(|r| r.right() + gap).unwrap_or(full.left() + gap);
    let x1 = right.map(|r| r.left() - gap).unwrap_or(full.right() - gap);
    let bottom = open[2]
        .then(|| {
            let bh = enlarger_h.min(h);
            Rect::from_min_max(Pos2::new(x0, full.bottom() - gap - bh), Pos2::new(x1, full.bottom() - gap))
        })
        .filter(|r| {
            r.width() >= ENLARGER_MIN_W
                && r.height() >= DOCK_MIN_H
                && inside(*r)
                && left.is_none_or(|l| !docks_cross(l, *r))
                && right.is_none_or(|c| !docks_cross(c, *r))
        });
    let stage_bottom = bottom.map(|r| r.top() - gap).unwrap_or(full.bottom());
    let stage = if x1 > x0 && stage_bottom > full.top() {
        Rect::from_min_max(Pos2::new(x0, full.top()), Pos2::new(x1, stage_bottom))
    } else {
        Rect::from_min_size(full.left_top(), Vec2::ZERO)
    };
    Docks { left, right, bottom, stage }
}

impl App {
    /// Where the three windows stand and what is left for the picture.
    fn docks(&self, full: Rect) -> Docks {
        place_docks(full, self.panels, self.enlarger_h.unwrap_or(190.0))
    }

    /// The three docked windows: the negative left, the print right (both
    /// at the stage's full height), the enlarger at the bottom between them.
    /// A window with no rectangle is left closed on screen and open in
    /// `panels`, so the toolbar can dismiss it or the next wider frame
    /// brings it back. Nothing is movable; × closes a window that is up.
    /// The name sits on one line. Each window is drawn in the darkroom and
    /// clipped to its rectangle, so the contents cannot change its size.
    fn dock_windows(&mut self, ui: &mut egui::Ui, docks: &Docks) {
        let mut open = self.panels;
        let mut hide = [false; 3];
        if let Some(r) = docks.left {
            hide[0] = show_dock(ui, "dock-negative", r, "Negative", |ui| {
                negative_body(ui, |ui| self.analysis_panel(ui));
            })
            .0;
        }
        if let Some(r) = docks.right {
            hide[1] = show_dock(ui, "dock-print", r, "Print", |ui| {
                dock_scroll(ui, |ui| self.print_panel(ui));
            })
            .0;
        }
        if let Some(r) = docks.bottom {
            // Height follows the content; the stage is laid out with the
            // measured value of the last frame. The width stays the gap.
            let (closed, h) = show_dock(ui, "dock-enlarger", r, "Enlarger", |ui| {
                self.enlarger_panel(ui);
            });
            hide[2] = closed;
            if h >= DOCK_MIN_H && self.enlarger_h.is_none_or(|old| (old - h).abs() > 0.5) {
                self.enlarger_h = Some(h);
                ui.ctx().request_repaint();
            }
        }
        for (i, hide) in hide.into_iter().enumerate() {
            if hide {
                open[i] = false;
            }
        }
        if open != self.panels {
            self.panels = open;
            self.persist();
        }
    }

    /// Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y: undo and redo; Tab hides or shows
    /// every panel. Enter keeps the crop. Not while a text field has the keyboard.
    fn edit_keys(&mut self, ctx: &egui::Context) {
        if self.keys_are_taken(ctx) {
            return;
        }
        if self.crop_open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            self.apply_crop();
        }
        // The burn tool: Enter locks the brush, Escape opens it, and puts
        // the tool down when it is open. Not while the pointer holds it.
        if self.burn_on && self.burn_drag.is_none() {
            if !self.burn_locked && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter))
            {
                self.lock_brush();
            }
            if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)) {
                if self.burn_locked {
                    self.open_brush();
                } else {
                    self.close_burn();
                }
            }
        }
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::P)) {
            self.set_process(!self.plate_process());
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Tab)) {
            let any = self.panels.iter().any(|&p| p);
            self.panels = [!any; 3];
            self.persist();
            // egui also hands Tab to the first focusable widget drawn this
            // frame. Keyboard focus on a button would mute ←/→, B and the
            // next Tab, so it is given back at once.
            if let Some(id) = ctx.memory(|m| m.focused()) {
                if egui::TextEdit::load_state(ctx, id).is_none() {
                    ctx.memory_mut(|m| m.surrender_focus(id));
                }
            }
        }
        let (undo, redo) = ctx.input_mut(undo_redo_keys);
        if undo {
            self.undo();
        }
        if redo {
            self.redo();
        }
    }
}

/// `(undo, redo)` of this frame's keys: Ctrl+Z undoes, Ctrl+Shift+Z and
/// Ctrl+Y redo. A shortcut also matches with Shift held on top of it, so
/// the one that names Shift is asked first; asked second, Ctrl+Z had
/// taken Ctrl+Shift+Z for itself and it undid.
fn undo_redo_keys(i: &mut egui::InputState) -> (bool, bool) {
    use egui::{Key, KeyboardShortcut, Modifiers};
    let redo = i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z))
        || i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Y));
    let undo = i.consume_shortcut(&KeyboardShortcut::new(Modifiers::COMMAND, Key::Z));
    (undo, redo)
}

#[cfg(test)]
mod key_tests {
    use super::{text_edit_focused, undo_redo_keys};
    use egui::{Event, InputState, Key, Modifiers};

    /// One frame with a number field in it and `key` pressed: whether a
    /// text field has the keyboard when the frame is drawn, and whether
    /// the key is still there for a shortcut to take.
    fn frame_with_field(ctx: &egui::Context, value: &mut f64, key: Option<Key>) -> (bool, bool) {
        let mut input = egui::RawInput::default();
        if let Some(key) = key {
            let modifiers = Modifiers::NONE;
            input.events.push(Event::Key {
                key,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers,
            });
        }
        let mut seen = (false, false);
        let _ = ctx.run_ui(input, |ui| {
            let field = ui.add(egui::DragValue::new(value));
            if key.is_none() {
                field.request_focus();
            }
            let typing = text_edit_focused(ui.ctx());
            let left = key.is_some_and(|key| ui.input_mut(|i| i.consume_key(Modifiers::NONE, key)));
            seen = (typing, left);
        });
        seen
    }

    /// Why the shortcuts ask who had the keyboard when the frame began:
    /// Enter and Escape end typing in a number, and in the frame of that
    /// key the field no longer has the keyboard and the key is still in
    /// the input.
    #[test]
    fn the_key_that_ends_typing_is_still_there_after_the_field() {
        for key in [Key::Enter, Key::Escape] {
            let ctx = egui::Context::default();
            let mut value = 3.0;
            // The field takes the keyboard and shows its text.
            frame_with_field(&ctx, &mut value, None);
            assert_eq!(frame_with_field(&ctx, &mut value, None), (true, false), "{key:?}");
            assert_eq!(frame_with_field(&ctx, &mut value, Some(key)), (false, true), "{key:?}");
        }
    }

    fn pressed(key: Key, modifiers: Modifiers) -> (bool, bool) {
        let mut i = InputState::default();
        i.modifiers = modifiers;
        i.events.push(Event::Key { key, physical_key: None, pressed: true, repeat: false, modifiers });
        undo_redo_keys(&mut i)
    }

    #[test]
    fn shift_makes_the_undo_key_redo() {
        let shifted = Modifiers::COMMAND | Modifiers::SHIFT;
        assert_eq!(pressed(Key::Z, Modifiers::COMMAND), (true, false));
        assert_eq!(pressed(Key::Z, shifted), (false, true));
        assert_eq!(pressed(Key::Y, Modifiers::COMMAND), (false, true));
        // Neither without the command key, and no other letter.
        assert_eq!(pressed(Key::Z, Modifiers::NONE), (false, false));
        assert_eq!(pressed(Key::X, Modifiers::COMMAND), (false, false));
    }
}

#[cfg(test)]
mod path_tests {
    use super::{
        crumb_label, export_extensions, export_target, linear_export_name, with_ext, without_verbatim_prefix,
        ExportFormat, ExportKind, ExportTarget,
    };
    use std::path::{Path, PathBuf};

    #[test]
    fn a_verbatim_windows_path_is_a_drive_path() {
        assert_eq!(without_verbatim_prefix(Path::new(r"\\?\C:\Users")), PathBuf::from(r"C:\Users"));
        assert_eq!(without_verbatim_prefix(Path::new(r"\\?\UNC\nas\photos")), PathBuf::from(r"\\nas\photos"));
        assert_eq!(without_verbatim_prefix(Path::new("/home/dragan")), PathBuf::from("/home/dragan"));
    }

    #[test]
    fn the_breadcrumb_root_keeps_its_name() {
        assert_eq!(crumb_label(Path::new("/")), "/");
        assert_eq!(crumb_label(Path::new("/home")), "home");
        #[cfg(windows)]
        {
            assert_eq!(crumb_label(Path::new(r"C:\")), "C:");
            assert_eq!(crumb_label(Path::new(r"C:\Users")), "Users");
        }
    }

    #[test]
    fn an_export_name_gets_its_kind_of_extension() {
        let tiff = export_extensions(ExportKind::Print, false, ExportFormat::Tiff);
        let jpeg = export_extensions(ExportKind::Print, false, ExportFormat::Jpeg);
        let negative = export_extensions(ExportKind::Linear, false, ExportFormat::Jpeg);
        let dng = export_extensions(ExportKind::Linear, true, ExportFormat::Tiff);
        // Either spelling of TIFF stands, in any case; a name without one
        // gets the first; a DNG accepts no TIFF and a TIFF no DNG.
        assert_eq!(with_ext(PathBuf::from("print.TIFF"), tiff), PathBuf::from("print.TIFF"));
        assert_eq!(with_ext(PathBuf::from("print.tif"), tiff), PathBuf::from("print.tif"));
        assert_eq!(with_ext(PathBuf::from("print"), tiff), PathBuf::from("print.tif"));
        assert_eq!(with_ext(PathBuf::from("print.dng"), tiff), PathBuf::from("print.tif"));
        // The other print format, typed, is kept; a bare name takes the chosen one.
        assert_eq!(with_ext(PathBuf::from("print.JPG"), tiff), PathBuf::from("print.JPG"));
        assert_eq!(with_ext(PathBuf::from("print"), jpeg), PathBuf::from("print.jpg"));
        assert_eq!(with_ext(PathBuf::from("print.tiff"), jpeg), PathBuf::from("print.tiff"));
        // The linear negative is a TIFF whatever the print's format.
        assert_eq!(with_ext(PathBuf::from("neg.jpg"), negative), PathBuf::from("neg.tif"));
        assert_eq!(with_ext(PathBuf::from("neg.tiff"), negative), PathBuf::from("neg.tiff"));
        assert_eq!(with_ext(PathBuf::from("plate.tiff"), dng), PathBuf::from("plate.dng"));
        assert_eq!(with_ext(PathBuf::from("plate.DNG"), dng), PathBuf::from("plate.DNG"));
        assert_eq!(with_ext(PathBuf::from("plate"), dng), PathBuf::from("plate.dng"));
        // The first of each list is what the save panel appends.
        assert_eq!((tiff[0], negative[0], dng[0]), ("tif", "tif", "dng"));
    }

    /// A dot in a name is not an extension: only another export format's
    /// extension is replaced, anything else is part of the name.
    #[test]
    fn a_dotted_export_name_keeps_its_tail() {
        let tiff = export_extensions(ExportKind::Print, false, ExportFormat::Tiff);
        let jpeg = export_extensions(ExportKind::Print, false, ExportFormat::Jpeg);
        assert_eq!(with_ext(PathBuf::from("frame.v2"), tiff), PathBuf::from("frame.v2.tif"));
        assert_eq!(with_ext(PathBuf::from("2026.11.10 Köln"), jpeg), PathBuf::from("2026.11.10 Köln.jpg"));
        assert_eq!(with_ext(PathBuf::from("a.b/frame.v2"), tiff), PathBuf::from("a.b/frame.v2.tif"));
        assert_eq!(with_ext(PathBuf::from("print."), tiff), PathBuf::from("print.tif"));
        assert_eq!(with_ext(PathBuf::from(".hidden"), tiff), PathBuf::from(".hidden.tif"));
    }

    /// The egui dialog asks about the name as typed. The extension comes
    /// after that, so the file it then names is asked about here, and the
    /// RAW is refused under every spelling.
    #[test]
    fn an_export_replaces_no_file_unasked_and_never_the_raw() {
        use ExportTarget::{Ask, Source, Write};
        let dir = std::env::temp_dir().join(format!("mimizan-gui-export-target-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let raw = dir.join("IMG_1.dng");
        let old = dir.join("old.tif");
        std::fs::write(&raw, b"raw").unwrap();
        std::fs::write(&old, b"print").unwrap();
        let tiff = export_extensions(ExportKind::Print, false, ExportFormat::Tiff);
        let dng = export_extensions(ExportKind::Linear, true, ExportFormat::Tiff);

        // A bare name that becomes a file which is there: asked.
        assert_eq!(export_target(dir.join("old"), false, tiff, &raw), Ask(old.clone()));
        assert_eq!(export_target(dir.join("old.dng"), false, tiff, &raw), Ask(old.clone()));
        // Typed in full, the dialog has asked already; a new name is free.
        assert_eq!(export_target(old.clone(), false, tiff, &raw), Write(old.clone()));
        assert_eq!(export_target(dir.join("new"), false, tiff, &raw), Write(dir.join("new.tif")));
        // The RAW: by its own name, by a bare name, by another extension,
        // and through the system panel too.
        assert_eq!(export_target(raw.clone(), false, dng, &raw), Source(raw.clone()));
        assert_eq!(export_target(dir.join("IMG_1"), false, dng, &raw), Source(raw.clone()));
        assert_eq!(export_target(dir.join("IMG_1.tif"), false, dng, &raw), Source(raw.clone()));
        assert_eq!(
            export_target(dir.join(".").join("IMG_1.dng"), true, dng, &raw),
            Source(dir.join(".").join("IMG_1.dng"))
        );
        // The system panel's path is written as it is and was asked there.
        assert_eq!(export_target(dir.join("old"), true, tiff, &raw), Write(dir.join("old")));
        assert_eq!(export_target(old.clone(), true, tiff, &raw), Write(old));
        // Nothing was written by asking.
        assert_eq!(std::fs::read(&raw).unwrap(), b"raw");
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn the_plate_of_a_dng_is_not_named_like_the_dng() {
        let name = |p: &str, dng| linear_export_name(Some(Path::new(p)), dng);
        assert_eq!(name("a/IMG_1.NEF", true), "IMG_1.dng");
        assert_eq!(name("a/IMG_1.dng", true), "IMG_1.linear.dng");
        assert_eq!(name("a/IMG_1.DNG", true), "IMG_1.linear.dng");
        assert_eq!(name("a/IMG_1.dng", false), "IMG_1.negative.tif");
        assert_eq!(name("a/IMG_1.NEF", false), "IMG_1.negative.tif");
        assert_eq!(linear_export_name(None, true), "mimizan.dng");
    }
}

#[cfg(test)]
mod sheet_tests {
    use super::{
        mm_of_pixels, ruler_factor, sheet_following, sheet_for_side, sheet_keeping_pixels,
        sheet_of_pixels_at, Base, SheetSide,
    };
    use crate::browser::Ruler;
    use mimizan_core::print::PrintSize;
    use mimizan_core::recipe::Recipe;

    fn px(sheet: PrintSize, dpi: f64) -> (usize, usize) {
        sheet.pixels(dpi)
    }

    /// 1:1 is a fixed point: the sheet made of the picture's pixels gives
    /// those pixels back at every dpi the field allows, with and without
    /// a ring taken from inside.
    #[test]
    fn the_sheet_of_the_pixels_is_the_pixels() {
        for &(uw, uh) in &[(6048usize, 4032usize), (4032, 6048), (1, 1), (6001, 3999), (9504, 6336)] {
            for dpi in [36.0, 72.0, 150.0, 254.0, 300.0, 299.7, 600.0, 1200.0] {
                let sheet = sheet_of_pixels_at(uw, uh, 0, dpi).unwrap();
                assert_eq!(px(sheet, dpi), (uw, uh), "{uw}x{uh} at {dpi}");
                let framed = sheet_of_pixels_at(uw, uh, 207, dpi).unwrap();
                assert_eq!(px(framed, dpi), (uw + 414, uh + 414), "{uw}x{uh} at {dpi} framed");
            }
        }
        assert_eq!(sheet_of_pixels_at(0, 4032, 0, 300.0), None);
        assert_eq!(sheet_of_pixels_at(6048, 4032, 0, 0.0), None);
    }

    /// Type one side: that side is exact to the pixel, the other is in the
    /// picture's proportion, and the fields read back what was typed.
    #[test]
    fn a_typed_side_stands_and_the_other_follows_the_picture() {
        let (uw, uh, dpi) = (6048usize, 4032usize, 300.0);
        let wide = sheet_for_side(uw, uh, dpi, 30.0, SheetSide::Width).unwrap();
        let (bw, bh) = px(wide, dpi);
        assert_eq!(bw, (30.0 / 2.54 * 300.0_f64).round() as usize);
        assert_eq!(bh, (bw as f64 * 4032.0 / 6048.0).round() as usize);
        // The centimetres the file then shows round to the typed figure.
        assert!(((bw as f64 / dpi * 2.54) - 30.0).abs() < 0.01);
        let tall = sheet_for_side(uw, uh, dpi, 30.0, SheetSide::Height).unwrap();
        let (bw, bh) = px(tall, dpi);
        assert_eq!(bh, (30.0 / 2.54 * 300.0_f64).round() as usize);
        assert_eq!(bw, (bh as f64 * 6048.0 / 4032.0).round() as usize);
        // A portrait picture gets a portrait sheet.
        let portrait = sheet_for_side(uh, uw, dpi, 30.0, SheetSide::Width).unwrap();
        let (bw, bh) = px(portrait, dpi);
        assert!(bh > bw);
        // Nothing to type against.
        assert_eq!(sheet_for_side(0, 0, dpi, 30.0, SheetSide::Width), None);
        assert_eq!(sheet_for_side(uw, uh, 0.0, 30.0, SheetSide::Width), None);
    }

    /// A new crop keeps the sheet's long side and takes the crop's shape,
    /// the long side where the picture's long side is.
    #[test]
    fn the_sheet_follows_the_crop_by_its_long_side() {
        let dpi = 300.0;
        let sheet = sheet_for_side(6048, 4032, dpi, 40.0, SheetSide::Width).unwrap();
        let (long, _) = px(sheet, dpi);
        // A square crop: both sides the long side.
        assert_eq!(px(sheet_following(sheet, 3000, 3000, dpi).unwrap(), dpi), (long, long));
        // A portrait crop: the long side moves to the height.
        let (bw, bh) = px(sheet_following(sheet, 3000, 4000, dpi).unwrap(), dpi);
        assert_eq!(bh, long);
        assert_eq!(bw, (long as f64 * 3000.0 / 4000.0).round() as usize);
        // A landscape crop keeps it on the width.
        let (bw, bh) = px(sheet_following(sheet, 4000, 3000, dpi).unwrap(), dpi);
        assert_eq!(bw, long);
        assert_eq!(bh, (long as f64 * 3000.0 / 4000.0).round() as usize);
        assert_eq!(sheet_following(sheet, 0, 3000, dpi), None);
    }

    /// With the px ruler a dpi change keeps the pixels: the sheet's and the
    /// rings', the latter through the recipe's own rounding.
    #[test]
    fn the_px_ruler_keeps_the_pixels_when_the_dpi_moves() {
        let sheet = sheet_for_side(6048, 4032, 300.0, 30.0, SheetSide::Width).unwrap();
        let before = px(sheet, 300.0);
        for dpi in [36.0, 72.0, 240.0, 360.0, 1200.0] {
            assert_eq!(px(sheet_keeping_pixels(sheet, 300.0, dpi), dpi), before, "{dpi}");
            for ring in [0usize, 1, 59, 207, 1181] {
                let r = Recipe { dpi, border_mm: mm_of_pixels(ring, dpi), ..Recipe::on_paper() };
                assert_eq!(r.border_px(), ring, "{ring} px at {dpi}");
            }
        }
    }

    /// Shown = held × factor; typed ÷ factor is held again, so a value
    /// shown in any unit comes back as it was, to the pixel for px.
    #[test]
    fn a_length_round_trips_through_every_ruler() {
        let dpi = 300.0;
        for (base, held) in [(Base::Cm, 30.0), (Base::Cm, 29.997), (Base::Mm, 5.0), (Base::Mm, 17.526)] {
            for ruler in Ruler::ALL {
                let k = ruler_factor(ruler, base, dpi);
                let shown = held * k;
                assert!((shown / k - held).abs() < 1e-9, "{ruler:?} {base:?}: {held} -> {shown}");
            }
        }
        // The factors themselves: centimetres to inches, to pixels; the
        // same for millimetres.
        assert_eq!(ruler_factor(Ruler::Metric, Base::Cm, dpi), 1.0);
        assert!((ruler_factor(Ruler::Imperial, Base::Cm, dpi) - 1.0 / 2.54).abs() < 1e-12);
        assert!((ruler_factor(Ruler::Pixels, Base::Cm, dpi) - 300.0 / 2.54).abs() < 1e-12);
        assert!((ruler_factor(Ruler::Imperial, Base::Mm, dpi) - 1.0 / 25.4).abs() < 1e-12);
        assert!((ruler_factor(Ruler::Pixels, Base::Mm, dpi) - 300.0 / 25.4).abs() < 1e-12);
        // 30 cm at 300 dpi shows as 3543 px and 11.81 in.
        assert_eq!((30.0 * ruler_factor(Ruler::Pixels, Base::Cm, dpi)).round(), 3543.0);
        assert!(((30.0 * ruler_factor(Ruler::Imperial, Base::Cm, dpi)) - 11.811).abs() < 1e-3);
        // A dpi under 1 does not make a factor of 0.
        assert!(ruler_factor(Ruler::Pixels, Base::Cm, 0.0) > 0.0);
    }
}

#[cfg(test)]
mod crop_drag_tests {
    use super::{
        dress_to_hold, hit_crop, moved_crop, must_make_room, opened_is_fresh, opening_memory, picture_terms,
        recipe_needs_saving, stage_black_px, tile_must_wait, CropHit, PaperFormat, Rings, StageDress,
    };
    use crate::worker::{develop_memory_of_size, memory_fits, MEMORY_HEADROOM};
    use mimizan_core::recipe::{CropCorner, CropRect, Recipe};
    use std::path::Path;

    #[test]
    fn black_alone_paints_the_whole_margin() {
        // 196 asked, margins 196 and 244: the long side is painted to the sheet.
        assert_eq!(stage_black_px(0, 196, 196, 244), 244);
        let rings = Rings { black_tex: 244.0, frame_x_tex: 196.0, frame_y_tex: 244.0, ..Rings::default() };
        assert_eq!(rings.black_shown(), egui::Vec2::new(196.0, 244.0));
        // Inside a white border the asked ring stays, and the strip beyond it is paper.
        assert_eq!(stage_black_px(207, 196, 403, 451), 196);
    }

    #[test]
    fn the_rings_on_the_stage_stay_with_its_picture() {
        // The picture on the stage has no border; the file taking the edit
        // has one. Until that file's picture lands, the stage keeps its own
        // (empty) rings: the new border must not dress the old pixels.
        let none = StageDress { rings: Rings::default(), cross: Some([0.3, 0.7]) };
        let wide = StageDress {
            rings: Rings { frame_x_tex: 118.8, frame_y_tex: 118.8, ..Rings::default() },
            cross: Some([0.5, 0.5]),
        };
        assert_eq!(dress_to_hold(None, true, || none), Some(none));
        // An empty stage has nothing to hold.
        assert_eq!(dress_to_hold(None, false, || wide), None);
        // A third file opened before the second's picture lands: the stage
        // still shows the first, so the first's dress stays held.
        assert_eq!(dress_to_hold(Some(none), true, || wide), Some(none));
    }

    #[test]
    fn the_session_goes_when_the_next_development_would_not_fit() {
        // A 24 MP development, about 1.8 GB: with 2 GB free the picture on
        // the stage goes first; with 6 GB free it stays; a machine that
        // cannot say keeps everything.
        let need = mimizan_core::pipeline::develop_memory(
            6048,
            4032,
            mimizan_core::decode::SensorKind::Bayer(mimizan_core::cfa::BayerPhase::RGGB),
        );
        assert!(must_make_room(need, 2_000_000_000));
        assert!(!must_make_room(need, 6_000_000_000));
        assert!(!must_make_room(need, 0));
        // The same rule the worker refuses by, from the same figure.
        assert_eq!(must_make_room(need, 2_000_000_000), !memory_fits(need, 2_000_000_000));
        assert!(memory_fits(1_000, (1_000.0 * MEMORY_HEADROOM) as u64 + 1));
        assert!(!memory_fits(1_000, (1_000.0 * MEMORY_HEADROOM) as u64 - 1));
    }

    #[test]
    fn the_room_is_measured_for_the_file_that_opens() {
        // The strip knows the next file is 61 MP: that is the need, not the
        // 24 MP picture on the stage. Without the strip's size the session's
        // own stands in; without either there is nothing to go by.
        let small = develop_memory_of_size(6048, 4032);
        let big = opening_memory(Some((9504, 6336)), Some(small)).unwrap();
        assert!(big > small * 2, "{big} vs {small}");
        assert_eq!(opening_memory(None, Some(small)), Some(small));
        assert_eq!(opening_memory(None, None), None);
    }

    #[test]
    fn a_development_is_fresh_when_it_is_another_file() {
        let a = Path::new("a.nef");
        let b = Path::new("b.nef");
        // The session held decides; the same file developed again is not fresh.
        assert!(!opened_is_fresh(Some(a), None, a));
        assert!(opened_is_fresh(Some(a), None, b));
        // Let go for room, the released file stands for the session.
        assert!(!opened_is_fresh(None, Some(a), a));
        assert!(opened_is_fresh(None, Some(a), b));
        // Nothing open: every file is fresh.
        assert!(opened_is_fresh(None, None, a));
    }

    #[test]
    fn grabbing_does_not_jump() {
        let origin = CropRect::inscribed(100, 100, 10, 8);
        let hits = [
            CropHit::Move,
            CropHit::Corner(CropCorner::Nw),
            CropHit::Corner(CropCorner::Ne),
            CropHit::Corner(CropCorner::Se),
            CropHit::Corner(CropCorner::Sw),
        ];
        for hit in hits {
            assert_eq!(moved_crop(origin, hit, 0, 0, 100, 100, (10, 8)), origin);
        }
    }

    #[test]
    fn the_frame_follows_the_pointer() {
        let origin = CropRect { x: 10, y: 12, w: 40, h: 32 };
        let next = moved_crop(origin, CropHit::Move, 5, -3, 100, 100, (10, 8));
        assert_eq!(next, CropRect { x: 15, y: 9, w: 40, h: 32 });
    }

    #[test]
    fn inside_the_frame_moves_it_and_outside_pans() {
        let r = CropRect { x: 10, y: 20, w: 40, h: 30 };
        assert_eq!(hit_crop(30.0, 35.0, r, 4.0, 3.0), Some(CropHit::Move));
        assert_eq!(hit_crop(8.0, 35.0, r, 4.0, 3.0), Some(CropHit::Move));
        assert_eq!(hit_crop(0.0, 0.0, r, 4.0, 3.0), None);
        assert_eq!(hit_crop(10.0, 20.0, r, 4.0, 3.0), Some(CropHit::Corner(CropCorner::Nw)));
    }

    #[test]
    fn a_corner_follows_the_movement() {
        let origin = CropRect::inscribed(100, 100, 10, 8);
        let next = moved_crop(origin, CropHit::Corner(CropCorner::Se), -20, -16, 100, 100, (10, 8));
        assert_eq!(next.x, origin.x);
        assert_eq!(next.y, origin.y);
        assert_eq!(next.w, origin.w - 20);
        assert_eq!(next.h, origin.h - 16);
    }

    #[test]
    fn turning_the_ratio_stands_it_the_other_way() {
        assert_eq!(PaperFormat::R4x5.frame(100, 80, false), (5, 4));
        assert_eq!(PaperFormat::R4x5.frame(100, 80, true), (4, 5));
        assert_eq!(PaperFormat::R4x5.frame(80, 100, false), (4, 5));
        assert_eq!(PaperFormat::R4x5.frame(80, 100, true), (5, 4));
        assert_eq!(PaperFormat::Square.frame(100, 80, true), (1, 1));
    }

    #[test]
    fn the_closed_box_names_the_side_that_is_framed() {
        assert_eq!(PaperFormat::R8x10.framed(false), "8×10");
        assert_eq!(PaperFormat::R8x10.framed(true), "10×8");
        assert_eq!(PaperFormat::R20x24.framed(true), "24×20");
        assert_eq!(PaperFormat::Square.framed(false), "1:1");
        assert_eq!(PaperFormat::Square.framed(true), "1:1");
    }

    #[test]
    fn the_picture_row_is_read_from_its_pixels() {
        assert_eq!(picture_terms(6048, 4024), (3, 2));
        assert_eq!(picture_terms(4024, 6048), (2, 3));
        assert_eq!(picture_terms(100, 80), (5, 4));
        assert_eq!(PaperFormat::Image.frame(6048, 4024, false), (6048, 4024));
        assert_eq!(PaperFormat::Image.frame(6048, 4024, true), (4024, 6048));
        assert_eq!(
            PaperFormat::match_window(6048, 4024, 6048, 4024, PaperFormat::R8x10),
            (PaperFormat::Image, false)
        );
        assert_eq!(
            PaperFormat::match_window(100, 80, 50, 40, PaperFormat::Square),
            (PaperFormat::R4x5, false)
        );
    }

    /// A window of the picture's own shape, a pixel off after rounding,
    /// is still the picture's format and not the square.
    #[test]
    fn a_window_of_the_pictures_shape_is_the_picture_not_the_square() {
        let now = PaperFormat::R8x10;
        let (format, turned) = PaperFormat::match_window(6048, 4024, 3025, 2012, now);
        assert_eq!((format, turned), (PaperFormat::Image, false));
        let (format, turned) = PaperFormat::match_window(6048, 4024, 2012, 3025, now);
        assert_eq!((format, turned), (PaperFormat::Image, true));
        // A square is a square, and 5×7 is 5×7 on a picture of any size.
        assert_eq!(PaperFormat::match_window(6048, 4024, 2000, 2001, now).0, PaperFormat::Square);
        assert_eq!(PaperFormat::match_window(6048, 4024, 2800, 2001, now), (PaperFormat::R5x7, false));
        assert_eq!(PaperFormat::match_window(60480, 40240, 28000, 20010, now), (PaperFormat::R5x7, false));
    }

    /// 4×5, 8×10 and 16×20 are one shape. The window of an 8×10 is found
    /// as the 8×10 it was cut for, and with none of them chosen as the
    /// first of the three.
    #[test]
    fn formats_of_one_shape_keep_the_one_chosen() {
        for now in [PaperFormat::R4x5, PaperFormat::R8x10, PaperFormat::R16x20] {
            assert_eq!(PaperFormat::match_window(6048, 4024, 5030, 4024, now), (now, false));
            assert_eq!(PaperFormat::match_window(6048, 4024, 3219, 4024, now), (now, true));
        }
        assert_eq!(PaperFormat::match_window(6048, 4024, 5030, 4024, PaperFormat::R5x7).0, PaperFormat::R4x5);
        // A picture that is 5:4 itself: its whole is the picture, a part of
        // it the format chosen, and the picture when that was chosen.
        assert_eq!(PaperFormat::match_window(100, 80, 100, 80, PaperFormat::R8x10).0, PaperFormat::Image);
        assert_eq!(PaperFormat::match_window(100, 80, 50, 40, PaperFormat::R8x10).0, PaperFormat::R8x10);
        assert_eq!(PaperFormat::match_window(100, 80, 50, 40, PaperFormat::Image).0, PaperFormat::Image);
    }

    #[test]
    fn a_portrait_window_on_a_landscape_picture_remembers_the_turn() {
        let (format, turned) = PaperFormat::match_window(100, 80, 40, 50, PaperFormat::R4x5);
        assert_eq!((format, turned), (PaperFormat::R4x5, true));
        let (format, turned) = PaperFormat::match_window(100, 80, 50, 40, PaperFormat::R4x5);
        assert_eq!((format, turned), (PaperFormat::R4x5, false));
    }

    #[test]
    fn a_recipe_is_saved_only_when_it_differs_from_the_one_opened() {
        let opened = Recipe::on_paper();
        assert!(!recipe_needs_saving(false, &opened, &opened));
        assert!(!recipe_needs_saving(true, &opened, &opened));
        let mut changed = opened.clone();
        changed.place = 0.25;
        assert!(recipe_needs_saving(true, &changed, &opened));
        assert!(!recipe_needs_saving(true, &changed, &changed));
    }

    #[test]
    fn the_tile_waits_until_the_picture_shows_its_exposure() {
        assert!(tile_must_wait(true, false));
        assert!(!tile_must_wait(true, true));
        assert!(!tile_must_wait(false, false));
    }
}

#[cfg(test)]
mod dock_tests {
    use super::{
        docks_cross, hold_magnification, opening_moved, pan_bounds, pan_limit, picture_is_fitted,
        place_docks, sheet_rect, strip_closes, DOCK_MIN_H, ENLARGER_MIN_W, PANEL_GAP, PANEL_W,
    };
    use egui::{Pos2, Rect, Vec2};

    fn area(w: f32, h: f32) -> Rect {
        Rect::from_min_size(Pos2::ZERO, Vec2::new(w, h))
    }

    fn apart(dock: Rect, other: Option<Rect>) {
        if let Some(other) = other {
            assert!(!docks_cross(dock, other), "{dock:?} crosses {other:?}");
        }
    }

    #[test]
    fn a_wide_window_keeps_all_three_apart() {
        let full = area(1200.0, 700.0);
        let d = place_docks(full, [true; 3], 190.0);
        let left = d.left.unwrap();
        let right = d.right.unwrap();
        let bottom = d.bottom.unwrap();
        assert!((left.left() - PANEL_GAP).abs() < 0.1);
        assert!((right.right() - (1200.0 - PANEL_GAP)).abs() < 0.1);
        assert_eq!(left.width(), PANEL_W);
        assert_eq!(right.width(), PANEL_W);
        apart(left, Some(right));
        apart(bottom, Some(left));
        apart(bottom, Some(right));
        assert!(bottom.width() >= ENLARGER_MIN_W && bottom.height() >= DOCK_MIN_H);
        apart(d.stage, Some(left));
        apart(d.stage, Some(right));
        apart(d.stage, Some(bottom));
    }

    #[test]
    fn a_narrow_window_drops_the_negative_and_keeps_print() {
        let full = area(500.0, 700.0);
        let d = place_docks(full, [true; 3], 190.0);
        assert!(d.left.is_none());
        let right = d.right.unwrap();
        assert!((right.right() - (500.0 - PANEL_GAP)).abs() < 0.1);
        assert!(right.left() >= full.left());
        if let Some(bottom) = d.bottom {
            apart(bottom, Some(right));
            assert!(bottom.right() <= right.left());
        }
        apart(d.stage, Some(right));
    }

    #[test]
    fn a_window_narrower_than_one_column_shows_no_column() {
        let d = place_docks(area(200.0, 700.0), [true; 3], 190.0);
        assert!(d.left.is_none() && d.right.is_none());
    }

    #[test]
    fn the_enlarger_yields_when_its_gap_cannot_hold_the_heading() {
        // Both columns fit (wider than 660). The gap between them is too
        // small for the enlarger's name and ×.
        let d = place_docks(area(700.0, 700.0), [true; 3], 190.0);
        assert!(d.left.is_some() && d.right.is_some());
        assert!(d.bottom.is_none());
        apart(d.left.unwrap(), d.right);
    }

    #[test]
    fn a_short_window_shows_no_dock() {
        let d = place_docks(area(1200.0, 40.0), [true; 3], 190.0);
        assert!(d.left.is_none() && d.right.is_none() && d.bottom.is_none());
    }

    #[test]
    fn room_brings_the_negative_back_without_a_new_click() {
        let open = [true; 3];
        assert!(place_docks(area(500.0, 700.0), open, 190.0).left.is_none());
        assert!(place_docks(area(1200.0, 700.0), open, 190.0).left.is_some());
    }

    #[test]
    fn the_strip_closes_only_for_a_picture_click_or_the_crosshair() {
        assert!(strip_closes(true, true, false));
        assert!(strip_closes(true, false, true));
        assert!(!strip_closes(true, false, false));
        assert!(!strip_closes(false, true, true));
    }

    #[test]
    fn a_zoomed_sheet_runs_past_the_opening() {
        let room = Rect::from_min_size(Pos2::ZERO, Vec2::new(1000.0, 800.0));
        let opening = Rect::from_min_max(Pos2::new(330.0, 0.0), Pos2::new(670.0, 560.0));
        assert!(picture_is_fitted(1.0));
        assert!(!picture_is_fitted(1.2));
        assert_eq!(pan_limit(opening.size(), opening.size()), Vec2::ZERO);
        let gap = Pos2::new(opening.left() - 6.0, opening.center().y);
        let fitted = sheet_rect(opening, Vec2::ZERO, opening.size());
        assert!(!fitted.contains(gap));
        let sheet = opening.size() * 2.0;
        let zoomed = sheet_rect(opening, Vec2::ZERO, sheet);
        assert!(room.intersect(zoomed).contains(gap));
        assert!(zoomed.contains(opening.center()));
        let limit = pan_limit(sheet, opening.size());
        let slid = sheet_rect(opening, Vec2::new(limit.x, 0.0), sheet);
        assert!((slid.left() - opening.left()).abs() < 0.01);
    }

    #[test]
    fn a_sheet_that_fits_the_room_stays_in_it() {
        // The opening sits high in the room (the enlarger lies below). A
        // 1:1 print taller than the opening, but not than the room, is not
        // cut off above the room: it covers the opening and keeps inside.
        let room = Rect::from_min_max(Pos2::new(0.0, 40.0), Pos2::new(1000.0, 540.0));
        let opening = Rect::from_min_max(Pos2::new(330.0, 40.0), Pos2::new(670.0, 360.0));
        let sheet = Vec2::new(300.0, 400.0);
        let (lo, hi) = pan_bounds(sheet, opening, room);
        // Narrower than the opening: centred on it.
        assert_eq!((lo.x, hi.x), (0.0, 0.0));
        let centred = sheet_rect(opening, Vec2::ZERO.clamp(lo, hi), sheet);
        assert!(centred.top() >= room.top() - 0.01, "{centred:?}");
        assert!(centred.bottom() <= room.bottom() + 0.01);
        // Every bound still covers the opening's height and keeps in the room.
        for pan in [lo, hi] {
            let r = sheet_rect(opening, pan, sheet);
            assert!(r.top() <= opening.top() && r.bottom() >= opening.bottom(), "{r:?} uncovers {opening:?}");
            assert!(room.contains_rect(r), "{r:?} leaves {room:?}");
        }
        // Taller than the room: the old rule, it runs past both edges.
        let tall = Vec2::new(300.0, 900.0);
        let (lo, hi) = pan_bounds(tall, opening, room);
        let limit = pan_limit(tall, opening.size());
        assert_eq!((lo.y, hi.y), (-limit.y, limit.y));
    }

    #[test]
    fn a_chosen_magnification_survives_a_new_opening() {
        let old = Rect::from_min_max(Pos2::new(100.0, 40.0), Pos2::new(700.0, 500.0));
        let shrunk = Rect::from_min_max(Pos2::new(100.0, 40.0), Pos2::new(500.0, 400.0));
        assert!(opening_moved(old, shrunk));
        assert!(!opening_moved(old, old));
        let (fitted, pan) =
            hold_magnification(1.0, Vec2::new(12.0, 4.0), 0.2, 0.1, old.center(), shrunk.center());
        assert!(picture_is_fitted(fitted));
        assert_eq!(pan, Vec2::new(12.0, 4.0));
        let (zoom, pan) = hold_magnification(4.0, Vec2::ZERO, 0.2, 0.1, old.center(), shrunk.center());
        assert!((0.2 * 4.0 - 0.1 * zoom).abs() < 1e-3);
        assert_eq!(pan, old.center() - shrunk.center());
        let (back, pan) = hold_magnification(1.5, Vec2::new(8.0, 8.0), 0.2, 0.5, old.center(), old.center());
        assert!(picture_is_fitted(back));
        assert_eq!(pan, Vec2::ZERO);
    }
}

#[cfg(test)]
mod crumb_tests {
    use super::crumb_is_open;
    use std::path::Path;

    #[test]
    fn a_sandbox_grant_stops_above_its_folder() {
        let root = Path::new("negatives");
        let above = Path::new("other");
        let here = Path::new("negatives");
        let below = Path::new("negatives").join("roll");
        assert!(!crumb_is_open(true, Some(root), above, false));
        assert!(crumb_is_open(true, Some(root), here, true));
        assert!(crumb_is_open(true, Some(root), &below, false));
        assert!(crumb_is_open(false, None, above, false));
        assert!(!crumb_is_open(true, None, above, false));
        assert!(crumb_is_open(true, None, here, true));
    }
}

#[cfg(test)]
mod straighten_tests {
    use super::scaled_draft;
    use mimizan_core::recipe::CropRect;
    use mimizan_core::straighten::frame_size;

    /// The draft follows the frame through a change of the straighten:
    /// the whole frame stays whole, a window keeps its share and its
    /// proportion, and nothing leaves the frame.
    #[test]
    fn the_draft_follows_the_frame() {
        let old = (6048u32, 4032u32);
        let (fw, fh) = frame_size(old.0 as usize, old.1 as usize, 3.0);
        let new = (fw as u32, fh as u32);
        assert!(new.0 < old.0 && new.1 < old.1);
        let whole = CropRect { x: 0, y: 0, w: old.0, h: old.1 };
        assert_eq!(scaled_draft(whole, old, new, new), CropRect { x: 0, y: 0, w: new.0, h: new.1 });
        // An 8×10 window in the picture's middle.
        let win = CropRect::inscribed(old.0, old.1, 10, 8);
        let moved = scaled_draft(win, old, new, (10, 8));
        assert!(moved.x + moved.w <= new.0 && moved.y + moved.h <= new.1);
        assert!((moved.w as f64 / moved.h as f64 - 1.25).abs() < 0.002);
        let share = moved.h as f64 / new.1 as f64;
        assert!((share - 1.0).abs() < 0.002, "the window filled the height before, {share}");
        // A small window in the top left keeps its place.
        let small = CropRect { x: 100, y: 100, w: 1000, h: 800 };
        let s = scaled_draft(small, old, new, (10, 8));
        let k = new.0 as f64 / old.0 as f64;
        assert!((s.x as f64 - 100.0 * k).abs() <= 2.0);
        assert!((s.w as f64 - 1000.0 * k).abs() <= 2.0);
        // And back to zero the whole frame is the whole negative again.
        let back = scaled_draft(scaled_draft(whole, old, new, new), new, old, old);
        assert_eq!(back, whole);
    }
}
