//! Window layout: the toolbar, the film strip along the bottom, and the
//! stage with three docked windows on it (Negative left, Print right,
//! Enlarger below). Nothing here changes pixels; it only chooses
//! parameters and hands them to the workers.

use crate::browser::{self, home_dir, Folder, Persisted, Sort, ThumbJob, ThumbState, Thumbs};
use crate::cpu::{CpuMeter, CpuView};
use crate::edit::{Edit, History};
use crate::histogram::{histogram_widget, raw_histogram_widget};
use crate::session::{DetailAsk, DevelopParams, Histogram, PlateView, Session, ViewParams, DETAIL_MARGIN};
use crate::worker::{
    available_memory, develop_memory, Event, Heavy, Job, Loupe, LoupeRequest, LoupeResult, PeakMeasure,
    PeakPoll, Preview, PreviewRequest, PreviewResult, Redevelop,
};
use crate::zone_bar::zone_bar;
use egui::{
    Color32, ColorImage, CursorIcon, Pos2, Rect, RichText, Sense, TextureHandle, TextureOptions, Vec2,
};
use egui_file_dialog::{FileDialog, OpeningMode};
use mimizan_core::ingest::WhiteBalance;
use mimizan_core::mix::{ColorFilter, WeightSpace, Weights};
use mimizan_core::paper::{self, SplitGrade};
use mimizan_core::plate::{PlateFilters, PlateParams, PlateSpace, PlateWeights};
use mimizan_core::print::{
    box_for_picture_height, box_for_picture_width, memory_estimate, memory_text, peak_print_radius,
    PrintSize, LAMP_LIMIT,
};
use mimizan_core::recipe::{CropCorner, CropRect, Recipe, Sharpen};
use mimizan_core::resize::ResampleKernel;
use mimizan_core::separate::MaskMode;
use mimizan_core::tone::Tone;
use mimizan_core::zone;
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Print size choice of the export section; `Recipe::size` is derived.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum SizeMode {
    Native,
    Paper,
}

/// Which centimetre field the user is editing. The other follows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PictureSide {
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
    /// against the picture's long side. The whole picture is its own format.
    fn match_window(img_w: u32, img_h: u32, cw: u32, ch: u32) -> (Self, bool) {
        let (format, turned, err) = Self::ALL
            .into_iter()
            .map(|f| {
                let (a, b) = f.oriented(img_w, img_h);
                let (turned, err) = ratio_turn(a, b, cw, ch);
                (f, turned, err)
            })
            .min_by_key(|(_, _, err)| *err)
            .unwrap_or((Self::R8x10, false, i64::MAX));
        let (image_turned, image_err) = ratio_turn(img_w, img_h, cw, ch);
        let whole = cw == img_w && ch == img_h;
        if image_err < err || (image_err == err && whole) {
            (Self::Image, image_turned)
        } else {
            (format, turned)
        }
    }
}

/// Whether `cw`:`ch` sits across `a`:`b`, and how far the two ratios are.
fn ratio_turn(a: u32, b: u32, cw: u32, ch: u32) -> (bool, i64) {
    let along = (i64::from(cw) * i64::from(b) - i64::from(ch) * i64::from(a)).abs();
    let across = (i64::from(cw) * i64::from(a) - i64::from(ch) * i64::from(b)).abs();
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
    crop: Option<CropRect>,
    plate: Option<PlateView>,
}

/// The two rings painted around the picture on the stage, in texture
/// pixels, with their flat values: the print's black film edge and white
/// paper against its picture, so they keep their proportion at any size
/// and dpi. Zero widths are no sheet.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct Rings {
    black_tex: f32,
    white_tex: f32,
    white: u8,
    black: u8,
}

/// What is drawn over and around the picture on the stage from the edit:
/// the sheet's rings and the crosshair (the 1:1 box's spot, in picture
/// fractions). Held as one while the picture belongs to the previous file.
#[derive(Clone, Copy, PartialEq, Debug, Default)]
struct StageDress {
    rings: Rings,
    cross: Option<[f64; 2]>,
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
/// memory first: it does not fit in `available` with a third to spare.
/// Unknown free memory (0) never releases anything.
fn must_make_room(need: u64, available: u64) -> bool {
    available != 0 && need as f64 > available as f64 / 1.3
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
    /// Path of a session let go to make room for the development under
    /// way (`make_room`). The picture stays on screen as its texture; the
    /// result decides whether it was the same file.
    released: Option<PathBuf>,
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
    size_mode: SizeMode,
    paper_cm: [f64; 2],
    /// The export estimate the user was warned about. A second click with
    /// the same estimate goes ahead; a different one warns again.
    export_warned: Option<u64>,
    /// Memory in the machine, bytes, read once. Zero when unknown.
    ram_total: u64,
    /// Where the loupe looks, as a fraction of the upright picture.
    sharpen_at: Option<[f64; 2]>,
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
    /// Draft window, in file-upright pixels (before the user's turns).
    crop_draft: CropRect,
    crop_drag: Option<CropDrag>,

    dialog: FileDialog,
    cameras_dir: PathBuf,
    /// Recipe as opened, as reset, or as last saved next to the RAW.
    recipe_baseline: Recipe,
    /// A file change or a close is waiting on the save question.
    pending_leave: Option<Leave>,
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
            size_mode: SizeMode::Native,
            export_warned: None,
            ram_total: {
                let mut sys = sysinfo::System::new();
                sys.refresh_memory();
                sys.total_memory()
            },
            paper_cm: [30.0, 40.0],
            sharpen_at: None,
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
            // A launch from the Store has its working directory in System32.
            // The save dialog always opens on the directory set for this export.
            dialog: {
                let mut dialog = FileDialog::new();
                dialog.config_mut().opening_mode = OpeningMode::AlwaysInitialDir;
                dialog
            },
            cameras_dir,
            recipe_baseline: Recipe::on_paper(),
            pending_leave: None,
            close_allowed: false,
            screenshot_to: std::env::var_os("MIMIZAN_SCREENSHOT").map(PathBuf::from),
            screenshot_state: 0,
        };
        match initial {
            Some(p) if p.is_dir() => app.set_folder(&p),
            Some(p) => app.open(p),
            None => {
                // The strip is the only way in: the last folder, else the
                // pictures folder, else home.
                let last = persisted.folder.filter(|f| f.is_dir());
                if let Some(f) = last.or_else(start_folder) {
                    app.set_folder(&f);
                }
            }
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
        }
        .save();
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
        self.edit = e;
        self.size_mode = match self.edit.recipe.size {
            None => SizeMode::Native,
            Some(PrintSize::Physical { .. }) => SizeMode::Paper,
        };
        if self.edit.recipe.size.is_some() {
            if let Some(cm) = self.picture_cm() {
                self.paper_cm = cm;
            }
        }
    }

    /// The picture's size in centimetres, from the layout the recipe produces.
    fn picture_cm(&self) -> Option<[f64; 2]> {
        let l = self.print_layout()?;
        let dpi = self.edit.recipe.dpi;
        if dpi <= 0.0 {
            return None;
        }
        Some([l.image_w as f64 / dpi * 2.54, l.image_h as f64 / dpi * 2.54])
    }

    /// Write a paper box whose picture is the centimetre field on `side`,
    /// the other side following at the same scale, black and white outside it.
    /// Then show the centimetres the file actually has.
    fn apply_picture(&mut self, side: PictureSide) {
        let (uw, uh) = self.picture_size();
        let dpi = self.edit.recipe.dpi;
        if uw == 0 || uh == 0 || dpi <= 0.0 {
            return;
        }
        let b = self.edit.recipe.frame_px();
        let (bw, bh) = match side {
            PictureSide::Width => {
                let px = (self.paper_cm[0] / 2.54 * dpi).round().max(1.0) as usize;
                box_for_picture_width(uw, uh, px, b)
            }
            PictureSide::Height => {
                let px = (self.paper_cm[1] / 2.54 * dpi).round().max(1.0) as usize;
                box_for_picture_height(uw, uh, px, b)
            }
        };
        self.edit.recipe.size =
            Some(PrintSize::Physical { width_in: bw as f64 / dpi, height_in: bh as f64 / dpi });
        if let Some(cm) = self.picture_cm() {
            self.paper_cm = cm;
        }
    }

    /// First paper size: 40 cm on the long side, the short side from the negative.
    fn set_long_edge_cm(&mut self, long_cm: f64) {
        let (uw, uh) = self.picture_size();
        if uh >= uw && uh > 0 {
            self.paper_cm = [long_cm * uw as f64 / uh as f64, long_cm];
            self.apply_picture(PictureSide::Height);
        } else if uw > 0 {
            self.paper_cm = [long_cm, long_cm * uh as f64 / uw as f64];
            self.apply_picture(PictureSide::Width);
        }
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

    fn view_params(&self) -> ViewParams {
        let r = &self.edit.recipe;
        ViewParams {
            weights: self.weights(),
            tone: r.curve.clone(),
            place: r.place,
            show_mask: self.show_mask,
            rotation: self.edit.rotation,
            // While the easel is up the whole negative is shown.
            crop: if self.crop_open { None } else { r.crop },
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

    /// Size of the whole negative, upright, after the user's turns.
    fn export_size(&self) -> (usize, usize) {
        let Some(s) = &self.session else { return (0, 0) };
        let (w, h) = s.upright_size();
        if self.rotation() % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// File-upright size, before the user's turns.
    fn file_size(&self) -> (u32, u32) {
        let Some(s) = &self.session else { return (0, 0) };
        let (w, h) = s.upright_size();
        (w as u32, h as u32)
    }

    /// The recipe the print box uses. While the easel is up the box is the
    /// whole negative, which is what the view shows.
    fn loupe_recipe(&self) -> Recipe {
        let mut recipe = self.print_recipe();
        if self.crop_open {
            recipe.crop = None;
        }
        recipe
    }

    /// The recipe with its window turned into the current upright picture
    /// and clamped to it (a sidecar from another file may be larger).
    fn print_recipe(&self) -> Recipe {
        let mut recipe = self.edit.recipe.clone();
        if let Some(c) = recipe.crop {
            let (fw, fh) = self.file_size();
            let (uw, uh) = self.export_size();
            recipe.crop = Some(c.turned(self.rotation(), fw, fh).sanitized(uw as u32, uh as u32));
        }
        recipe
    }

    /// The window the print is made from (upright, after the user's turns).
    fn picture_size(&self) -> (usize, usize) {
        let (w, h) = self.export_size();
        match self.print_recipe().crop {
            Some(c) => (c.w as usize, c.h as usize),
            None => (w, h),
        }
    }

    /// The print box's layout: the whole negative while the easel is up.
    fn loupe_layout(&self) -> Option<mimizan_core::print::Layout> {
        let (w, h) = self.export_size();
        if w == 0 || h == 0 {
            return None;
        }
        let params = mimizan_core::print::PrintParams { recipe: self.loupe_recipe(), ..Default::default() };
        Some(mimizan_core::print::layout(w, h, 1, &params))
    }

    /// Output size of the print TIFF (after size step and border).
    fn print_layout(&self) -> Option<mimizan_core::print::Layout> {
        let (w, h) = self.export_size();
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
            black_tex: l.black_px as f32 * scale,
            white_tex: l.border_px as f32 * scale,
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
    }

    /// Crop on: the whole negative, with the committed window or a fresh 8×10.
    fn arm_crop(&mut self) {
        let (fw, fh) = self.file_size();
        if fw == 0 || fh == 0 {
            return;
        }
        if let Some(c) = self.edit.recipe.crop {
            let c = c.sanitized(fw, fh);
            let (format, turned) = PaperFormat::match_window(fw, fh, c.w, c.h);
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

    fn apply_crop(&mut self) {
        let (fw, fh) = self.file_size();
        if fw == 0 || fh == 0 {
            return;
        }
        self.edit.recipe.crop = Some(self.crop_draft.sanitized(fw, fh));
        self.close_crop();
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
        self.sharpen_at = Some([0.5, 0.5]);
        self.loupe_key = None;
        self.loupe_tex = None;
        self.cross_drag = false;
        if self.edit.recipe.size.is_some() {
            self.apply_picture(PictureSide::Width);
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

    fn start_export(&mut self) {
        let stem = self
            .session
            .as_ref()
            .and_then(|s| s.path.file_stem().map(|f| f.to_string_lossy().to_string()))
            .unwrap_or_else(|| "mimizan".into());
        if let Some(dir) = self.export_directory() {
            self.dialog.config_mut().initial_directory = dir;
        }
        self.dialog.config_mut().default_file_name = format!("{stem}.tif");
        self.dialog.save_file();
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
        self.make_room();
        self.heavy.send(Job::Open {
            path,
            develop: self.develop_params(),
            cameras_dir: self.cameras_dir.clone(),
        });
    }

    /// Let the session go before a development when the two would not fit
    /// in free memory together. The next file is most often from the same
    /// camera, so the measure is this session's own development; the
    /// worker checks the real frame once it is decoded. The picture stays
    /// on screen as its texture until the new one arrives; the `Opened`
    /// event finds the path in `released`.
    fn make_room(&mut self) {
        let Some(s) = self.session.as_ref() else { return };
        let (need, path) = (develop_memory(&s.frame), s.path.clone());
        let available = available_memory();
        if !must_make_room(need, available) {
            return;
        }
        let name = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
        self.log(format!(
            "{name} let go to make room: a development holds about {}, {} is free",
            memory_text(need),
            memory_text(available)
        ));
        self.released = Some(path);
        self.session = None;
    }

    /// The re-development "Apply" asked for, sent now that no panel holds
    /// a clone of the session.
    fn start_redevelop(&mut self) {
        let Some(develop) = self.redevelop.take() else { return };
        let Some(source) = self.session.as_ref().map(|s| Redevelop::of(s)) else {
            self.busy = None;
            return;
        };
        self.make_room();
        self.heavy.send(Job::Redevelop { source, develop });
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
        if fresh {
            self.history = History::new(e.clone());
        }
        self.set_edit(e);
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
    }

    /// Recipe file next to the RAW: `<name>.recipe.json`.
    fn recipe_sidecar(&self) -> Option<PathBuf> {
        let s = self.session.as_ref()?;
        let stem = s.path.file_stem()?.to_string_lossy().to_string();
        Some(s.path.with_file_name(format!("{stem}.recipe.json")))
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
        let bayer = self.session.as_ref().is_some_and(|s| !s.is_mono());
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
            Err(e) => self.log(format!("recipe not saved: {e}")),
        }
    }

    fn should_ask_to_save(&self) -> bool {
        self.screenshot_to.is_none()
            && recipe_needs_saving(self.session.is_some(), &self.recipe_for_file(), &self.recipe_baseline)
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
        egui::Modal::new(egui::Id::new("save_recipe")).show(ctx, |ui| {
            ui.set_min_width(320.0);
            ui.label(format!("Save the recipe next to the RAW {when}?"));
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if ui.button("Save").clicked() {
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
                    let before = self.session.as_ref().map(|old| old.path.clone()).or(self.released.take());
                    let fresh = before.is_none_or(|old| old != s.path);
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
                            // A paper border in the unattended capture.
                            self.edit.recipe.border_mm = mm.clamp(0.0, 100.0);
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
                        self.recipe_baseline = self.recipe_for_file();
                    }
                    // A re-development has new pixels: the measured radius
                    // belongs to the old ones.
                    self.note_peak_source();
                    self.busy = None;
                    self.last_view = None;
                }
                Event::Exported { tiff, width, height, ms } => {
                    self.log(format!("wrote {} ({width}x{height}) in {ms} ms", tiff.display()));
                    self.busy = None;
                }
                Event::Failed(e) => {
                    self.log(format!("error: {e}"));
                    self.busy = None;
                    // A failed open leaves the previous picture on the stage;
                    // the strip must point at that one, not at the failure.
                    // A picture let go for room is gone with its texture.
                    if self.released.take().is_some() {
                        self.texture = None;
                        self.detail_texture = None;
                        self.detail_rect = None;
                        self.held_dress = None;
                    }
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
        self.preview_ms = r.ms;
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
        self.dialog.update(ctx);
        let Some(path) = self.dialog.take_picked() else { return };
        if let Some(s) = &self.session {
            let path = with_ext(path, "tif");
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
    }

    /// Three always-visible groups. The windows sit on the left, the picture
    /// in the middle, the way back on the right. Each group is a flat well,
    /// and the bar shows between them.
    fn toolbar(&mut self, ui: &mut egui::Ui) {
        let id = ui.id();
        let full = ui.available_width();
        let lw = ui.ctx().data(|d| d.get_temp::<f32>(id.with("win")).unwrap_or(300.0));
        let pw = ui.ctx().data(|d| d.get_temp::<f32>(id.with("pic")).unwrap_or(340.0));
        let hw = ui.ctx().data(|d| d.get_temp::<f32>(id.with("hist")).unwrap_or(150.0));
        let (gap_l, gap_r) = toolbar_gaps(full, lw, pw, hw);
        ui.spacing_mut().item_spacing.x = 0.0;
        ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            let windows = tool_well(ui, |ui| {
                self.toolbar_windows(ui);
                ui.add_space(10.0);
                self.toolbar_process(ui);
            });
            ui.add_space(gap_l);
            let picture = tool_well(ui, |ui| self.toolbar_picture(ui));
            ui.add_space(gap_r);
            let history = tool_well(ui, |ui| self.toolbar_history(ui));
            ui.add_space(12.0);
            // The note takes only what is left and is cut, never painted over the wells.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.set_max_width(ui.available_width());
                self.toolbar_status(ui);
            });
            ui.ctx().data_mut(|d| {
                d.insert_temp(id.with("win"), windows.rect.width());
                d.insert_temp(id.with("pic"), picture.rect.width());
                d.insert_temp(id.with("hist"), history.rect.width());
            });
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

    fn toolbar_picture(&mut self, ui: &mut egui::Ui) {
        let has = self.session.is_some();
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
        if ui
            .add_enabled(has && self.crop_open, egui::Button::new("Apply"))
            .on_hover_text("Keep this window (Enter). The print, the histograms and the export use only it.")
            .clicked()
        {
            self.apply_crop();
        }
        // The picture's turn, apart from the frame.
        ui.add_space(16.0);
        if ui
            .add_enabled(has, egui::Button::new("⟳ 90°"))
            .on_hover_text("Rotate the picture 90° clockwise (preview and export)")
            .clicked()
        {
            self.edit.rotation = (self.edit.rotation + 1) % 4;
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
            val(ui, self.process_label(&s)).on_hover_text(
                "The process this file is developed in: the monochrome negative, or the three-colour \
                 plate with its taking filters and primaries. The switch is in the toolbar (P); the \
                 filters and the lamps are in the Enlarger.",
            );
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
                    "{} x {} ({:.1} %), {} ms",
                    self.tex_size[0],
                    self.tex_size[1],
                    100.0 * s.preview.scale,
                    self.preview_ms
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
            egui::ComboBox::from_label("white balance")
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

    /// The plate's taking filters and primaries, in the Enlarger where the
    /// monochrome mix stands: the same decision, the filter in front of
    /// the paper. The Wratten set follows the mix, so the mix stays with it.
    fn plate_filters_row(&mut self, ui: &mut egui::Ui, s: &Session) {
        let Some(mut params) = self.edit.plate else { return };
        ui.label(RichText::new("filters").weak()).on_hover_text(
            "The three taking filters: three weight triples on the balanced channels, each summing to \
             one, so grey gives three equal plates. Every plate is the one separation mixed again; \
             nothing is demosaiced.",
        );
        egui::ComboBox::from_id_salt("plate-filters").selected_text(params.filters.name()).show_ui(
            ui,
            |ui| {
                for f in PlateFilters::ALL {
                    let hover = match f {
                    PlateFilters::Camera => {
                        "The camera's colour matrix towards the primaries, inverted: the colorimetric set, \
                         as a RAW converter applies it. Weights go negative off the diagonal. Without a \
                         matrix in the file the channels are taken."
                    }
                    PlateFilters::Wratten => {
                        "Wratten 25, 58 and 47 in front of the mix, crosstalk included: the softer palette \
                         of the historical glass. Follows the mix beside it."
                    }
                    PlateFilters::Channels => {
                        "The balanced channels as they are, straight into R, G and B. What the Library of \
                         Congress does with the scanned plates."
                    }
                };
                    ui.selectable_value(&mut params.filters, f, f.name()).on_hover_text(hover);
                }
            },
        );
        ui.label(RichText::new("primaries").weak())
            .on_hover_text("The projection filters, and the profile of the file.");
        egui::ComboBox::from_id_salt("plate-space").selected_text(space_name(params.space)).show_ui(ui, |ui| {
            ui.selectable_value(&mut params.space, PlateSpace::AdobeRgb, "Adobe RGB").on_hover_text(
                "Adobe RGB (1998): its curve is exactly the paper's gamma 2.2 and its gamut holds the \
                 saturated primaries. The file carries the profile.",
            );
            ui.selectable_value(&mut params.space, PlateSpace::Srgb, "sRGB").on_hover_text(
                "sRGB primaries with the paper's gamma 2.2 (not the sRGB curve). The file carries the profile.",
            );
        });
        if self.edit.plate != Some(params) {
            self.edit.plate = Some(params);
        }
        if params.filters == PlateFilters::Wratten {
            // The emulsion behind the glass is a fact of the file, not a
            // choice: the camera file's calibrated response, or the native.
            let (name, hover) = match s.camera_file_path.as_ref().and_then(|p| p.file_name()) {
                Some(f) => (
                    "Panchromatic",
                    format!(
                        "The emulsion behind Wratten 25, 58 and 47: the panchromatic response calibrated \
                         for this sensor ({}). It sets how much the three plates overlap.",
                        f.to_string_lossy()
                    ),
                ),
                None => (
                    "Native",
                    "The emulsion behind Wratten 25, 58 and 47: the native 0.25 / 0.50 / 0.25, red and \
                     blue equally sensitive, as no camera file calibrates this sensor. It sets how much \
                     the three plates overlap."
                        .to_string(),
                ),
            };
            ui.label(RichText::new("behind").weak());
            ui.label(RichText::new(name).weak()).on_hover_text(hover);
        }
        match self.plate_resolved.as_ref().map(|r| &r.weights) {
            Some(Ok(w)) => {
                let rows: Vec<String> = ["red", "green", "blue"]
                    .iter()
                    .zip(w.rows.iter())
                    .map(|(n, r)| format!("{n:<6} {:+.3} {:+.3} {:+.3}", r[0], r[1], r[2]))
                    .collect();
                let note = if params.filters == PlateFilters::Camera && !s.has_colour_matrix() {
                    "no matrix in this file: the channels are taken"
                } else {
                    "weights"
                };
                ui.label(RichText::new(note).weak().small()).on_hover_text(format!(
                    "The three taking filters on the balanced channels R G B:\n{}",
                    rows.join("\n")
                ));
            }
            Some(Err(e)) => {
                ui.label(RichText::new(format!("no plate: {e}")).small().color(ui.visuals().error_fg_color));
            }
            None => {}
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

    /// The mix as one drop-down: Native, Panchromatic (the camera file) and
    /// the classic filters in front of it; "custom" while the sliders stand
    /// elsewhere. Each entry keeps its explanation as hover text.
    fn mix_combo(&mut self, ui: &mut egui::Ui) {
        let cam = self.session.as_ref().and_then(|s| {
            s.camera_file.as_ref().map(|c| (s.default_weights(), c.weights(), c.weights_space))
        });
        let cam_path = self
            .session
            .as_ref()
            .and_then(|s| s.camera_file_path.as_ref())
            .and_then(|p| p.file_name())
            .map(|f| f.to_string_lossy().to_string())
            .unwrap_or_default();
        // Filters sit in front of the calibrated response; native without a file.
        let base = cam.map(|c| c.0).unwrap_or_default();
        let base_name = if cam.is_some() { "Panchromatic" } else { "Native" };
        let current = self.weights();
        let same = |a: &Weights, b: &Weights| (a.r - b.r).abs() < 5e-4 && (a.b - b.b).abs() < 5e-4;
        let mut pick: Option<Weights> = None;
        // Name of the preset the sliders stand on.
        let mut selected = "custom".to_string();
        let native = Weights::default();
        if same(&current, &native) {
            selected = "Native".into();
        }
        if let Some((w, _, _)) = cam {
            if same(&current, &w) {
                selected = "Panchromatic".into();
            }
        }
        for f in ColorFilter::ALL.into_iter().filter(|f| *f != ColorFilter::None) {
            if same(&current, &base.filtered(f)) {
                selected = format!("{} {}", capitalise(f.colour()), f.wratten().unwrap_or(0));
            }
        }
        egui::ComboBox::from_id_salt("mix-combo").selected_text(selected).width(118.0).show_ui(ui, |ui| {
            if ui
                .selectable_label(same(&current, &native), "Native")
                .on_hover_text("¼ ½ ¼: exactly the Bayer luminance, no chroma mixed in")
                .clicked()
            {
                pick = Some(native);
            }
            if let Some((w, stored, space)) = cam {
                let hover = match space {
                    WeightSpace::Raw => format!(
                        "Calibrated against the monochrome reference sensor ({cam_path}).\n\
                         raw channels R {:.3} G {:.3} B {:.3}; for this balance R {:.3} G {:.3} B {:.3}",
                        stored.r, stored.g, stored.b, w.r, w.g, w.b
                    ),
                    WeightSpace::Balanced => format!(
                        "Calibrated against the monochrome reference sensor ({cam_path}).\nR {:.3} G {:.3} B {:.3} (balanced)",
                        w.r, w.g, w.b
                    ),
                };
                if ui.selectable_label(same(&current, &w), "Panchromatic").on_hover_text(hover).clicked() {
                    pick = Some(w);
                }
            }
            for f in ColorFilter::ALL.into_iter().filter(|f| *f != ColorFilter::None) {
                let w = base.filtered(f);
                let t = f.transmission();
                let label = format!("{} {}", capitalise(f.colour()), f.wratten().unwrap_or(0));
                let hover = format!(
                    "≈ Wratten {} ({}): {}\ntransmission R {:.2} G {:.2} B {:.2} in front of {base_name}\n→ R {:.3} G {:.3} B {:.3}\n\
                     Approximation from typical filter curves, not a measured fit. Resolution and grain unchanged; \
                     only band-limited chroma noise is added.",
                    f.wratten().unwrap_or(0),
                    f.colour(),
                    f.effect(),
                    t[0],
                    t[1],
                    t[2],
                    w.r,
                    w.g,
                    w.b
                );
                if ui.selectable_label(same(&current, &w), label).on_hover_text(hover).clicked() {
                    pick = Some(w);
                }
            }
        });
        if let Some(w) = pick {
            self.set_weights(w);
        }
    }

    /// Bottom window, step 2: everything that exposes the print: the mix
    /// (the filter), placing and the two paper exposures (the grade).
    ///
    /// The controls keep the width they need. A narrower glass clips them;
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

    /// Mix as one row, placing, the bar, the grades and the paper's ends.
    fn zones_column(&mut self, ui: &mut egui::Ui, width: f32) {
        let mono = self.session.as_ref().is_some_and(|s| s.is_mono());
        let plate = !mono && self.edit.plate.is_some();
        ui.horizontal(|ui| {
            if mono {
                ui.label(RichText::new("The sensor has no colour filter. This is the measurement.").weak());
            } else if plate {
                if let Some(s) = self.session.clone() {
                    self.plate_filters_row(ui, &s);
                }
            } else {
                ui.label(RichText::new("mix").weak());
                self.mix_combo(ui);
                let cur = self.weights();
                let (mut r, mut b) = (cur.r, cur.b);
                ui.label(RichText::new("R").weak());
                if ui
                    .add(egui::DragValue::new(&mut r).range(0.0..=1.0).speed(0.002).fixed_decimals(3))
                    .changed()
                {
                    self.set_weights(Weights { r, g: 0.0, b: cur.b.min(1.0 - r) });
                }
                ui.label(RichText::new("B").weak());
                if ui
                    .add(egui::DragValue::new(&mut b).range(0.0..=1.0).speed(0.002).fixed_decimals(3))
                    .changed()
                {
                    self.set_weights(Weights { r: cur.r.min(1.0 - b), g: 0.0, b });
                }
                let w = self.weights();
                let wb = self.session.as_ref().map(|s| s.wb()).unwrap_or([1.0; 3]);
                let raw = w.balanced_to_raw(wb);
                ui.label(RichText::new(format!("G {:.3}", w.g)).weak()).on_hover_text(format!(
                    "R + G + B = 1 on the balanced channels.\nOn the sensor channels before white balance: \
                 R {:.3} G {:.3} B {:.3}",
                    raw.r, raw.g, raw.b
                ));
            }
            // Stays in this row, so the window does not grow when the exposures
            // leave the base. Right edge of the glass, not of the unclipped row.
            if self.edit.recipe.curve.as_paper().is_some() {
                let at_base =
                    self.edit.recipe.curve.as_paper().is_some_and(|s| s.soft == 0.0 && s.hard == 0.0);
                let mut slot = ui.available_rect_before_wrap();
                let edge = ui.clip_rect().right();
                if edge > slot.left() {
                    slot.set_right(edge.min(slot.right()));
                    ui.scope_builder(
                        egui::UiBuilder::new()
                            .max_rect(slot)
                            .layout(egui::Layout::right_to_left(egui::Align::Center)),
                        |ui| {
                            self.base_button(ui, at_base);
                        },
                    );
                }
            }
        });
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
        zone_bar(ui, &mut split, width);
        ui.add_space(4.0);
        // One rail width for both exposures, taken before either slider is
        // built. The number sits beside the rail; sizing the rail to the whole
        // row lets that number widen the column, and the slider below then
        // puts its zero further right. The reservation is the widest reading,
        // so the rail stays put as the number changes.
        let value_w = exposure_value_width(ui);
        let gap = ui.spacing().item_spacing.x;
        let rail = exposure_rail_width(width, value_w, gap);
        exposure_slider(
            ui,
            rail,
            "00",
            &mut split.soft,
            "Filter 00, the soft exposure, in stops of time from the base print. More soft light \
             greys the highlights and hardly touches the shadows: the highlights' exposure.",
        );
        exposure_slider(
            ui,
            rail,
            "5",
            &mut split.hard,
            "Filter 5, the hard exposure, in stops of time from the base print. More hard light \
             deepens the shadows and the mid tones with them, the highlights stay: the shadows' \
             exposure.",
        );
        if self.edit.recipe.curve.as_paper() != Some(&split) {
            self.edit.recipe.curve = Tone::Paper(split.clone());
        }
        let curve = split.curve();
        ui.add(
            egui::Label::new(
                RichText::new(format!(
                    "≈ grade {} · {:.1} stops of scene · grey card prints at {:.2}",
                    paper::grade_label(curve.grade()),
                    curve.scene_range_stops(),
                    curve.eval(zone::GRAY_CARD)
                ))
                .weak(),
            )
            .wrap_mode(egui::TextWrapMode::Extend),
        )
        .on_hover_text(
            "The single filter grade this pair of exposures equals (by its ISO range), the scene \
             stops between the paper's first tone and its near-black, and where Zone V lands.",
        );
        if self.plate_view().is_some() {
            self.lamp_sliders(ui, rail);
        }
        if self.edit.recipe.place != 0.0 {
            ui.horizontal(|ui| {
                ui.label(RichText::new(format!("place {:+.2}", self.edit.recipe.place)).weak().small())
                    .on_hover_text(
                        "A placing from the recipe, in scene stops; the bar moves both exposures instead.",
                    );
                if ui.small_button("0").on_hover_text("Drop the placing").clicked() {
                    self.edit.recipe.place = 0.0;
                }
            });
        }
    }

    /// The projector's lamps, under the paper's two exposures: stops added
    /// to the red and the blue positive. Green is the reference and has
    /// none. The only colour control a plate has.
    fn lamp_sliders(&mut self, ui: &mut egui::Ui, rail: f32) {
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.label(RichText::new("lamps").weak()).on_hover_text(
                "Prokudin-Gorsky's projector had three lamps and the register of the side lenses, \
                 nothing else. These are the red and the blue lamp, in stops of exposure added to \
                 that positive; the green one is the reference. Grey stays grey at zero.",
            );
            let off = self.edit.lamps == [0.0; 2];
            if ui
                .add_enabled(!off, egui::Button::new(RichText::new("0")).small())
                .on_hover_text("Both lamps back to zero")
                .clicked()
            {
                self.edit.lamps = [0.0; 2];
            }
        });
        let [mut red, mut blue] = self.edit.lamps;
        exposure_slider(
            ui,
            rail,
            "R",
            &mut red,
            "The red lamp, in stops. More red warms the whole print; less cools it. \
             A grey card stays grey only at zero.",
        );
        exposure_slider(
            ui,
            rail,
            "B",
            &mut blue,
            "The blue lamp, in stops. More blue cools the print; less turns it towards yellow.",
        );
        let lamps = [red.clamp(-LAMP_LIMIT, LAMP_LIMIT), blue.clamp(-LAMP_LIMIT, LAMP_LIMIT)];
        if lamps != self.edit.lamps {
            self.edit.lamps = lamps;
        }
    }

    /// Grey and quiet at the base exposure, orange when a click returns there.
    fn base_button(&mut self, ui: &mut egui::Ui, at_base: bool) {
        let label = if at_base {
            RichText::new("base")
        } else {
            RichText::new("base").color(Color32::from_rgb(28, 18, 6))
        };
        let mut button = egui::Button::new(label).small();
        if !at_base {
            button = button.fill(Color32::from_rgb(255, 170, 60));
        }
        if ui
            .add_enabled(!at_base, button)
            .on_hover_text(if at_base { "The base exposure" } else { "Back to the base exposure" })
            .clicked()
        {
            self.edit.recipe.curve = Tone::paper();
        }
    }

    /// Right window, step 3: the print and its export.
    fn print_panel(&mut self, ui: &mut egui::Ui) {
        self.print_section(ui);
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
                egui::Button::new(RichText::new("Export…").strong())
                    .min_size(Vec2::new(ui.available_width(), 30.0)),
            )
            .on_disabled_hover_text(hover.clone())
            .on_hover_text(hover)
            .clicked()
            && self.export_memory_allows()
        {
            self.start_export();
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
    fn print_section(&mut self, ui: &mut egui::Ui) {
        // The file the export writes, first: it is the process's.
        ui.horizontal(|ui| {
            print_field_label(ui, "file");
            let (text, tip) = match self.plate_view() {
                Some(p) => (
                    format!("16-bit RGB TIFF · {}", space_name(p.space)),
                    "The three positives as the channels of one RGB file, with a matrix/TRC profile of \
                     these primaries and the paper's gamma 2.2.",
                ),
                None => (
                    "16-bit grey TIFF · gamma 2.2".to_string(),
                    "The sheet as one grey channel with a gamma 2.2 profile.",
                ),
            };
            ui.label(RichText::new(text).weak()).on_hover_text(tip);
        });
        let mut mode = self.size_mode;
        ui.horizontal(|ui| {
            print_field_label(ui, "size");
            ui.selectable_value(&mut mode, SizeMode::Native, "native")
                .on_hover_text("The negative's pixels, printed at the dpi below");
            ui.selectable_value(&mut mode, SizeMode::Paper, "paper")
                .on_hover_text("Fit into a paper size at the dpi");
        });
        self.size_mode = mode;
        // The centimetre row stays in both modes, so the lines under it do not
        // jump when native and paper trade places.
        if mode == SizeMode::Paper {
            // Entering paper with no size yet. The struct default 30×40 is a
            // loose box, so the first time takes 40 cm on the long side;
            // coming back keeps the picture the fields already show.
            if self.edit.recipe.size.is_none() && self.export_size().0 > 0 {
                if self.paper_cm == [30.0, 40.0] {
                    self.set_long_edge_cm(40.0);
                } else {
                    self.apply_picture(PictureSide::Width);
                }
            }
        } else {
            self.edit.recipe.size = None;
        }
        let dpi_before = self.edit.recipe.dpi;
        let (width_changed, height_changed) = ui
            .horizontal(|ui| {
                print_field_label(ui, "");
                match self.size_mode {
                    SizeMode::Paper => {
                        let width = ui
                            .add(
                                egui::DragValue::new(&mut self.paper_cm[0])
                                    .range(0.1..=2000.0)
                                    .clamp_existing_to_range(false)
                                    .speed(0.1)
                                    .fixed_decimals(1)
                                    .suffix(" cm"),
                            )
                            .on_hover_text(
                                "Width of the picture. The height follows the negative, one scale for both sides. The black edge and the white border are added around this.",
                            );
                        ui.label("×");
                        let height = ui
                            .add(
                                egui::DragValue::new(&mut self.paper_cm[1])
                                    .range(0.1..=2000.0)
                                    .clamp_existing_to_range(false)
                                    .speed(0.1)
                                    .fixed_decimals(1)
                                    .suffix(" cm"),
                            )
                            .on_hover_text(
                                "Height of the picture. The width follows the negative, one scale for both sides. The black edge and the white border are added around this.",
                            );
                        (width.changed(), height.changed())
                    }
                    SizeMode::Native => {
                        let mut cm = self.picture_cm().unwrap_or([0.0, 0.0]);
                        let hover = "The picture at this dpi, in centimetres. Paper is where the size is set.";
                        ui.add_enabled(
                            false,
                            egui::DragValue::new(&mut cm[0])
                                .range(0.0..=2000.0)
                                .fixed_decimals(1)
                                .suffix(" cm"),
                        )
                        .on_disabled_hover_text(hover);
                        ui.label("×");
                        ui.add_enabled(
                            false,
                            egui::DragValue::new(&mut cm[1])
                                .range(0.0..=2000.0)
                                .fixed_decimals(1)
                                .suffix(" cm"),
                        )
                        .on_disabled_hover_text(hover);
                        (false, false)
                    }
                }
            })
            .inner;
        let dpi_changed = ui
            .horizontal(|ui| {
                print_field_label(ui, "dpi");
                let response = ui.add(
                    egui::DragValue::new(&mut self.edit.recipe.dpi)
                        .range(36.0..=1200.0)
                        .clamp_existing_to_range(false)
                        .speed(1.0)
                        .suffix(" dpi"),
                );
                if self.size_mode == SizeMode::Native {
                    response
                        .on_hover_text("Written into the TIFF. These pixels print at this size.")
                        .changed()
                } else {
                    response.changed()
                }
            })
            .inner;
        if self.size_mode == SizeMode::Paper {
            if width_changed {
                self.apply_picture(PictureSide::Width);
            } else if height_changed {
                self.apply_picture(PictureSide::Height);
            } else if dpi_changed && (dpi_before - self.edit.recipe.dpi).abs() > f64::EPSILON {
                self.apply_picture(PictureSide::Width);
            } else if let Some(cm) = self.picture_cm() {
                self.paper_cm = cm;
            }
        }
        self.print_sheet_line(ui);
        let border_before = self.edit.recipe.border_mm;
        let black_before = self.edit.recipe.black_mm;
        // border and black share the row. Wrapped, so a long value breaks
        // inside the column instead of widening the dock.
        ui.horizontal_wrapped(|ui| {
            print_field_label(ui, "border");
            ui.add(
                egui::DragValue::new(&mut self.edit.recipe.border_mm)
                    .range(0.0..=100.0)
                    .clamp_existing_to_range(false)
                    .speed(0.2)
                    .suffix(" mm"),
            )
            .on_hover_text("White paper around the picture, outside the black edge");
            if self.edit.recipe.black_mm > self.edit.recipe.border_mm {
                self.edit.recipe.black_mm = self.edit.recipe.border_mm.max(0.0);
            }
            let white_mm = self.edit.recipe.border_mm.max(0.0);
            ui.label(RichText::new("black").weak());
            ui.add(
                egui::DragValue::new(&mut self.edit.recipe.black_mm)
                    .range(0.0..=white_mm)
                    .clamp_existing_to_range(false)
                    .speed(0.2)
                    .suffix(" mm"),
            )
            .on_hover_text(
                "Black film edge between the picture and the white border. It cannot be wider than the white border. Zero is off.",
            );
        });
        let border_moved = (border_before - self.edit.recipe.border_mm).abs() > f64::EPSILON;
        let black_moved = (black_before - self.edit.recipe.black_mm).abs() > f64::EPSILON;
        if self.size_mode == SizeMode::Paper && (border_moved || black_moved) {
            self.apply_picture(PictureSide::Width);
        }
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

    /// Pixels of the TIFF and the sheet in centimetres. Absent until a file is open.
    fn print_sheet_line(&self, ui: &mut egui::Ui) {
        let Some(l) = self.print_layout() else { return };
        let dpi = self.edit.recipe.dpi;
        if dpi <= 0.0 {
            return;
        }
        let cm = |px: usize| px as f64 / dpi * 2.54;
        ui.horizontal_wrapped(|ui| {
            ui.spacing_mut().item_spacing.x = 0.0;
            ui.label(
                RichText::new(format!(
                    "{} × {} px · {:.1} × {:.1} cm · ",
                    l.out_w,
                    l.out_h,
                    cm(l.out_w),
                    cm(l.out_h)
                ))
                .weak(),
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
        let (w, h) = self.export_size();
        if w == 0 || h == 0 {
            return None;
        }
        let params = mimizan_core::print::PrintParams { recipe: self.print_recipe(), ..Default::default() };
        let one = memory_estimate(w, h, 1, &params);
        if self.plate_view().is_none() {
            return Some(one);
        }
        // A plate holds its three mixed planes while one of them is
        // printed, and the finished positives beside the third.
        let lay = mimizan_core::print::layout(w, h, 1, &params);
        let plane = (w as u64).saturating_mul(h as u64).saturating_mul(8);
        let sheet = (lay.out_w as u64).saturating_mul(lay.out_h as u64).saturating_mul(8);
        Some(one.saturating_add(plane.saturating_mul(2)).saturating_add(sheet.saturating_mul(2)))
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
            let mut text = if scale > 1.0 { format!("{scale:.2}×") } else { "no enlargement".to_string() };
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
        let (uw, uh) = self.export_size();
        let (ow, oh) = self.print_layout().map(|l| (l.out_w, l.out_h)).unwrap_or((uw, uh));
        let rot = if self.rotation() != 0 {
            format!(", rotated {}°", 90 * self.rotation() as u16)
        } else {
            String::new()
        };
        let r = &self.edit.recipe;
        let local = if r.place != 0.0 { format!(", place {:+.2}", r.place) } else { String::new() };
        let sharpen = if r.peak.on {
            let radius = peak_print_radius(r.peak.radius, self.peak_scale());
            format!(", radius {radius:.2} px, amount {:.2}", r.peak.amount)
        } else {
            ", no sharpening".to_string()
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
                format!(
                    "Writes the three-colour plate on the {look}{rot}{local}{lamps}:\n\
                     <name>.tif  {ow} x {oh} px, 16 bit RGB, {space} gamma 2.2 ICC, {} filters{sharpen}, \
                     recipe and plate in the description",
                    p.weights.filters.name()
                )
            }
            None => format!(
                "Writes the print on the {look}{rot}{local}:\n\
                 <name>.tif  {ow} x {oh} px, 16 bit, gamma 2.2 grey ICC{sharpen}, recipe in the description"
            ),
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
        let StageDress { rings: Rings { black_tex, white_tex, white, black }, cross } = self.stage_dress();
        let border_tex = black_tex + white_tex;
        // The sheet (picture plus both rings) is what fits the opening.
        // A zoom grows it from this same fit, so the first step does not jump.
        let fit = (stage.width() / (tw as f32 + 2.0 * border_tex))
            .min(stage.height() / (th as f32 + 2.0 * border_tex));
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
            let white_pts = white_tex * scale;
            let black_pts = black_tex * scale;
            let border_pts = white_pts + black_pts;
            let bounds = pan_bounds(size + Vec2::splat(2.0 * border_pts), stage, room);
            let pan = pan.clamp(bounds.0, bounds.1);
            (sheet_rect(stage, pan, size), white_pts, black_pts, bounds, pan)
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
        let (mut rect, white_pts, black_pts, bounds, pan) = place(self.zoom, self.pan);
        self.pan = pan;
        let crosshair_released = self.cross_drag && !response.is_pointer_button_down_on();
        if !response.is_pointer_button_down_on() {
            self.cross_drag = false;
        } else if ui.input(|i| i.pointer.primary_pressed()) {
            self.cross_drag = pressed_on_cross;
            if self.cross_drag {
                self.crop_drag = None;
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
            if response.dragged() && self.crop_drag.is_none() {
                self.pan = (self.pan + response.drag_delta()).clamp(bounds.0, bounds.1);
                (rect, _, _, _, _) = place(self.zoom, self.pan);
            }
        }
        // The strip is the way in. It leaves when a click on the picture is
        // released, or when a drag of the crosshair is released. The close
        // is not written down, so the next launch still opens on the strip.
        let clicked_on_picture = response.clicked()
            && response.interact_pointer_pos().is_some_and(|p| rect.contains(p) && !on_dock(p));
        if self.pending_leave.is_none()
            && strip_closes(self.show_strip, clicked_on_picture, crosshair_released)
        {
            self.show_strip = false;
        }
        // Fitted, the sheet stays in the opening. Zoomed, it paints across
        // the room; the windows are drawn afterwards and lie on it.
        let clip = if picture_is_fitted(self.zoom) { stage } else { room };
        let painter = ui.painter_at(clip);
        ui.painter_at(stage).rect_filled(stage, 0.0, Color32::from_gray(28));
        let border_pts = white_pts + black_pts;
        if border_pts > 0.0 {
            painter.rect_filled(rect.expand(border_pts), 0.0, Color32::from_gray(white));
        }
        if black_pts > 0.0 {
            painter.rect_filled(rect.expand(black_pts), 0.0, Color32::from_gray(black));
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
            } else if near && self.crop_drag.is_none() {
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
fn toolbar_gaps(full: f32, windows: f32, picture: f32, history: f32) -> (f32, f32) {
    let min_gap = 16.0;
    let status = (full * 0.2).clamp(40.0, 240.0);
    let spare = full - windows - picture - history - status;
    if spare < 2.0 * min_gap {
        return (8.0, 8.0);
    }
    let picture_left =
        ((full - picture) * 0.5).clamp(windows + min_gap, full - status - history - min_gap - picture);
    let gap_l = picture_left - windows;
    let gap_r = (full - status - history) - (picture_left + picture);
    (gap_l, gap_r)
}

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

fn with_ext(p: PathBuf, ext: &str) -> PathBuf {
    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext) || e.eq_ignore_ascii_case("tiff")) {
        p
    } else {
        p.with_extension(ext)
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
        if self.pending_leave.is_some() || text_edit_focused(ctx) {
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
        ui.horizontal(|ui| {
            match &self.folder {
                None => {
                    ui.label(RichText::new("No folder. Drop a folder or a RAW file on the window.").weak());
                }
                Some(f) => {
                    // Breadcrumb: every ancestor is a button. The root is `/`
                    // on Unix and the drive (`C:`) on Windows.
                    let comps: Vec<PathBuf> = f.path.ancestors().map(Path::to_path_buf).collect();
                    for anc in comps.iter().rev() {
                        let name = crumb_label(anc);
                        if name.is_empty() {
                            continue;
                        }
                        let last = anc == &f.path;
                        let text = if last { RichText::new(name).strong() } else { RichText::new(name) };
                        if ui.add(egui::Button::new(text).frame(false)).clicked() && !last {
                            go_to = Some(anc.clone());
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
        if let Some(p) = go_to {
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
                    ThumbJob { generation, path: e.path.clone(), size: e.size, modified: e.modified }
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
/// Reach of the exposure sliders, stops of time either way (the recipe
/// allows `paper::STOP_LIMIT`; beyond two stops the print is all black or
/// all grey).
const SLIDER_STOPS: f64 = 2.0;
/// Twelfth of a stop, the step of an exposure handle.
const EXPOSURE_STEP: f64 = 1.0 / 12.0;
/// Rail the two exposures keep. A wider enlarger may lengthen it.
/// A narrower one clips the row; the rail does not shrink.
const EXPOSURE_RAIL: f32 = 168.0;

/// Width the enlarger controls are drawn at. The glass may be narrower
/// and then clips them. Nothing in the enlarger wraps to fit.
fn enlarger_content_width(ui: &egui::Ui) -> f32 {
    let body = egui::TextStyle::Body.resolve(ui.style());
    let paint =
        |text: &str| ui.painter().layout_no_wrap(text.to_owned(), body.clone(), Color32::WHITE).size().x;
    let gap = ui.spacing().item_spacing.x;
    let value = exposure_value_width(ui);
    let grade = paint("≈ grade 00 · 00.0 stops of scene · grey card prints at 0.00");
    let slider = EXPOSURE_RAIL + gap + value;
    let mix = paint("mix")
        + gap
        + 118.0
        + gap
        + paint("R")
        + gap
        + value
        + gap
        + paint("B")
        + gap
        + value
        + gap
        + paint("G 0.000");
    grade.max(slider).max(mix)
}

/// Width of the number beside an exposure slider: the widest reading the
/// range can show, in the button font the slider uses, plus its padding.
/// A little extra keeps the button stroke inside the row.
fn exposure_value_width(ui: &egui::Ui) -> f32 {
    let font = egui::TextStyle::Button.resolve(ui.style());
    let text =
        ui.painter().layout_no_wrap(format!("-{SLIDER_STOPS:.2} stops"), font, egui::Color32::WHITE).size().x;
    let padded = text + ui.spacing().button_padding.x * 2.0 + 2.0;
    padded.max(ui.spacing().interact_size.x)
}

/// Rail shared by the two exposure sliders. `value` is the number's width,
/// `gap` the space between rail and number. Below [`EXPOSURE_RAIL`] the
/// row is clipped instead of the rail being shortened.
fn exposure_rail_width(row: f32, value: f32, gap: f32) -> f32 {
    (row - value - gap).max(EXPOSURE_RAIL)
}

/// One exposure. The filter number sits in the handle; the stops sit beside the rail.
fn exposure_slider(ui: &mut egui::Ui, rail: f32, mark: &str, v: &mut f64, hint: &str) {
    let row = ui.horizontal(|ui| {
        let thickness = ui.text_style_height(&egui::TextStyle::Body).max(ui.spacing().interact_size.y);
        let (rect, response) = ui.allocate_exact_size(Vec2::new(rail, thickness), Sense::click_and_drag());
        let handle = exposure_handle_size(ui, thickness);
        if response.has_focus() {
            ui.memory_mut(|m| {
                m.set_focus_lock_filter(
                    response.id,
                    egui::EventFilter { horizontal_arrows: true, ..Default::default() },
                );
            });
            let step = ui.input(|i| {
                i.num_presses(egui::Key::ArrowRight) as i32 - i.num_presses(egui::Key::ArrowLeft) as i32
            });
            if step != 0 {
                *v = (*v + f64::from(step) * EXPOSURE_STEP).clamp(-SLIDER_STOPS, SLIDER_STOPS);
            }
        }
        if let Some(pos) = response.interact_pointer_pos() {
            *v = exposure_stops_from_pointer(rect.left(), rect.width(), handle.x, pos.x);
        }
        if response.dragged() {
            ui.ctx().set_cursor_icon(CursorIcon::Grabbing);
        } else if response.hovered() {
            ui.ctx().set_cursor_icon(CursorIcon::Grab);
        }
        if ui.is_rect_visible(rect) {
            paint_exposure_slider(ui, rect, handle, *v, mark, &response);
        }
        let stepping =
            ui.input(|i| i.num_presses(egui::Key::ArrowUp) > 0 || i.num_presses(egui::Key::ArrowDown) > 0);
        let speed = if stepping { EXPOSURE_STEP } else { (2.0 * SLIDER_STOPS) / f64::from(rail.max(1.0)) };
        ui.add(
            egui::DragValue::new(v)
                .range(-SLIDER_STOPS..=SLIDER_STOPS)
                .clamp_existing_to_range(false)
                .speed(speed)
                .fixed_decimals(2)
                .suffix(" stops"),
        );
    });
    row.response.on_hover_text(hint);
}

fn exposure_mark_font() -> egui::FontId {
    egui::FontId::proportional(12.0)
}

/// Width for the wider mark, "00", so both handles travel the same distance.
fn exposure_handle_size(ui: &egui::Ui, row_h: f32) -> Vec2 {
    let text = ui.painter().layout_no_wrap("00".to_owned(), exposure_mark_font(), Color32::WHITE).size().x;
    let w = text + 12.0;
    let h = (row_h - 2.0).clamp(12.0, row_h);
    Vec2::new(w, h)
}

fn exposure_handle_left(rail_left: f32, rail_w: f32, handle_w: f32, stops: f64) -> f32 {
    let t = ((stops + SLIDER_STOPS) / (2.0 * SLIDER_STOPS)).clamp(0.0, 1.0);
    let travel = (rail_w - handle_w).max(0.0);
    rail_left + (t as f32) * travel
}

fn exposure_stops_from_pointer(rail_left: f32, rail_w: f32, handle_w: f32, x: f32) -> f64 {
    let travel = (rail_w - handle_w).max(1.0);
    let t = ((x - handle_w * 0.5 - rail_left) / travel).clamp(0.0, 1.0);
    let raw = -SLIDER_STOPS + (t as f64) * (2.0 * SLIDER_STOPS);
    ((raw / EXPOSURE_STEP).round() * EXPOSURE_STEP).clamp(-SLIDER_STOPS, SLIDER_STOPS)
}

fn paint_exposure_slider(
    ui: &egui::Ui,
    rect: Rect,
    handle: Vec2,
    stops: f64,
    mark: &str,
    response: &egui::Response,
) {
    let visuals = ui.style().interact(response);
    let inactive = &ui.visuals().widgets.inactive;
    let rail_h = ui.spacing().slider_rail_height;
    let rail = Rect::from_center_size(rect.center(), Vec2::new(rect.width(), rail_h));
    let painter = ui.painter();
    painter.rect_filled(rail, inactive.corner_radius, inactive.bg_fill);
    let left = exposure_handle_left(rect.left(), rect.width(), handle.x, stops);
    let knob = Rect::from_min_size(Pos2::new(left, rect.center().y - handle.y * 0.5), handle)
        .expand(visuals.expansion);
    painter.rect(knob, visuals.corner_radius, visuals.bg_fill, visuals.fg_stroke, egui::StrokeKind::Inside);
    painter.text(
        knob.center(),
        egui::Align2::CENTER_CENTER,
        mark,
        exposure_mark_font(),
        visuals.fg_stroke.color,
    );
}

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
        if self.pending_leave.is_some() || text_edit_focused(ctx) {
            return;
        }
        if self.crop_open && ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, egui::Key::Enter)) {
            self.apply_crop();
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
        let (undo, redo) = ctx.input_mut(|i| {
            let undo =
                i.consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Z));
            let redo = i.consume_shortcut(&egui::KeyboardShortcut::new(
                egui::Modifiers::COMMAND | egui::Modifiers::SHIFT,
                egui::Key::Z,
            )) || i
                .consume_shortcut(&egui::KeyboardShortcut::new(egui::Modifiers::COMMAND, egui::Key::Y));
            (undo, redo)
        });
        if undo {
            self.undo();
        }
        if redo {
            self.redo();
        }
    }
}

#[cfg(test)]
mod path_tests {
    use super::{crumb_label, toolbar_gaps, without_verbatim_prefix};
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
    fn the_picture_group_sits_in_the_middle_when_there_is_room() {
        let (left, right) = toolbar_gaps(1200.0, 300.0, 340.0, 150.0);
        let picture_center = 300.0 + left + 170.0;
        assert!((picture_center - 600.0).abs() < 1.0);
        assert!(left >= 16.0 && right >= 16.0);
    }

    #[test]
    fn a_narrow_bar_keeps_a_short_gap() {
        assert_eq!(toolbar_gaps(400.0, 300.0, 340.0, 150.0), (8.0, 8.0));
    }
}

#[cfg(test)]
mod crop_drag_tests {
    use super::{
        dress_to_hold, hit_crop, moved_crop, must_make_room, picture_terms, recipe_needs_saving,
        tile_must_wait, CropHit, PaperFormat, Rings, StageDress,
    };
    use mimizan_core::recipe::{CropCorner, CropRect, Recipe};

    #[test]
    fn the_rings_on_the_stage_stay_with_its_picture() {
        // The picture on the stage has no border; the file taking the edit
        // has one. Until that file's picture lands, the stage keeps its own
        // (empty) rings: the new border must not dress the old pixels.
        let none = StageDress { rings: Rings::default(), cross: Some([0.3, 0.7]) };
        let wide =
            StageDress { rings: Rings { white_tex: 118.8, ..Rings::default() }, cross: Some([0.5, 0.5]) };
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
        assert_eq!(PaperFormat::match_window(6048, 4024, 6048, 4024), (PaperFormat::Image, false));
        assert_eq!(PaperFormat::match_window(100, 80, 50, 40), (PaperFormat::R4x5, false));
    }

    #[test]
    fn a_portrait_window_on_a_landscape_picture_remembers_the_turn() {
        let (format, turned) = PaperFormat::match_window(100, 80, 40, 50);
        assert_eq!((format, turned), (PaperFormat::R4x5, true));
        let (format, turned) = PaperFormat::match_window(100, 80, 50, 40);
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
        docks_cross, exposure_handle_left, exposure_rail_width, exposure_stops_from_pointer,
        hold_magnification, opening_moved, pan_bounds, pan_limit, picture_is_fitted, place_docks, sheet_rect,
        strip_closes, DOCK_MIN_H, ENLARGER_MIN_W, EXPOSURE_RAIL, PANEL_GAP, PANEL_W, SLIDER_STOPS,
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
    fn the_two_exposure_rails_share_one_width() {
        let wide = exposure_rail_width(400.0, 100.0, 8.0);
        assert!((wide - 292.0).abs() < 0.01);
        assert_eq!(exposure_rail_width(400.0, 100.0, 8.0), wide);
        assert_eq!(exposure_rail_width(40.0, 100.0, 8.0), EXPOSURE_RAIL);
        assert_eq!(exposure_rail_width(80.0, 100.0, 8.0), EXPOSURE_RAIL);
    }

    #[test]
    fn the_exposure_handle_sits_on_the_stops() {
        let (left, rail, handle) = (10.0, 200.0, 28.0);
        assert!((exposure_handle_left(left, rail, handle, -SLIDER_STOPS) - left).abs() < 0.01);
        let mid = exposure_handle_left(left, rail, handle, 0.0);
        assert!((mid - (left + (rail - handle) / 2.0)).abs() < 0.01);
        let end = exposure_handle_left(left, rail, handle, SLIDER_STOPS);
        assert!((end + handle - (left + rail)).abs() < 0.01);
        assert!(exposure_stops_from_pointer(left, rail, handle, left + rail / 2.0).abs() < 1e-9);
        assert!(
            (exposure_stops_from_pointer(left, rail, handle, left + handle / 2.0) + SLIDER_STOPS).abs()
                < 1e-9
        );
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
