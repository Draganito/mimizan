//! Window layout: toolbar, file column (left), preview (centre), controls
//! (right). Nothing here changes pixels; it only chooses parameters and hands
//! them to the workers.

use crate::browser::{self, Folder, Persisted, Sort, ThumbJob, ThumbState, Thumbs};
use crate::edit::{Edit, History};
use crate::histogram::{histogram_widget, raw_histogram_widget};
use crate::session::{Converter, DetailRect, DevelopParams, Histogram, Session, ViewParams};
use crate::worker::{Event, Heavy, Job, Preview, PreviewRequest};
use crate::zone_bar::zone_bar;
use egui::{Color32, ColorImage, Pos2, Rect, RichText, Sense, TextureHandle, TextureOptions, Vec2};
use egui_file_dialog::FileDialog;
use mimizan_core::deconv::DECONV_ITERATIONS;
use mimizan_core::ingest::WhiteBalance;
use mimizan_core::mix::{ColorFilter, WeightSpace, Weights};
use mimizan_core::paper::{self, SplitGrade};
use mimizan_core::print::PrintSize;
use mimizan_core::recipe::{Recipe, Sharpen, USM_MAX};
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogPurpose {
    Export,
    SaveRecipe,
    LoadRecipe,
}

pub struct App {
    heavy: Heavy,
    preview: Preview,
    thumbs: Thumbs,
    session: Option<Arc<Session>>,

    // Folder strip
    folder: Option<Folder>,
    folder_generation: u64,
    sort: Sort,
    show_strip: bool,
    /// Strip tiles inverted (the film), not the print.
    strip_negative: bool,
    /// Strip panel height in points; `strip_height_saved` is what the
    /// config file holds, so a drag is written once, when it ends.
    strip_height: f32,
    strip_height_saved: f32,
    /// Floating panels shown: analysis, enlarger, print.
    panels: [bool; 3],
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

    /// The development being edited (weights, recipe, turns) and its
    /// history. The export renders exactly `edit`.
    edit: Edit,
    history: History,
    /// Zone under the pointer in the zone bar; tints the preview.
    hover_zone: Option<u8>,

    // Export section
    size_mode: SizeMode,
    paper_cm: [f64; 2],

    /// "1:1" button pressed; applied by the preview panel, which knows the fit scale.
    zoom_to_1to1: bool,

    // Development
    wb_choice: WbChoice,
    wb_manual: [f64; 3],
    mask_mode: MaskMode,
    fix_defects: bool,
    reconstruct: bool,
    converter: Converter,
    /// Last converter a redevelop was sent for. Stops a failed switch from
    /// being sent again on every frame.
    converter_attempt: Option<Converter>,

    // Preview
    show_mask: bool,
    texture: Option<TextureHandle>,
    tex_size: [usize; 2],
    /// Rotation the shown texture was rendered with.
    tex_rotation: u8,
    /// Fit scale (points per texel) of the last frame, and an absolute scale
    /// the next frame must restore after the texture changed shape.
    last_fit: Option<f32>,
    keep_scale: Option<f32>,
    /// 1:1 window of the full negative for the zoomed view (upright, rotated
    /// pixel coordinates) and its texture.
    detail_texture: Option<TextureHandle>,
    detail_rect: Option<DetailRect>,
    /// Window the view currently wants (None when the fit preview suffices).
    wanted_detail: Option<DetailRect>,
    last_detail_request: Option<DetailRect>,
    generation: u64,
    shown_generation: u64,
    last_view: Option<ViewParams>,
    preview_ms: u128,
    hist_view: Histogram,
    hist_negative: Histogram,
    hist_source: HistSource,
    hist_log: bool,
    zoom: f32,
    pan: Vec2,

    dialog: FileDialog,
    dialog_purpose: DialogPurpose,
    cameras_dir: PathBuf,

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

fn resource_dir(name: &str) -> PathBuf {
    mimizan_core::calib::resolve_dir(Path::new(name))
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let cameras_dir = resource_dir("cameras");
        let persisted = Persisted::load();
        let mut app = Self {
            heavy: Heavy::spawn(cc.egui_ctx.clone()),
            preview: Preview::spawn(cc.egui_ctx.clone()),
            thumbs: Thumbs::spawn(cc.egui_ctx.clone(), cameras_dir.clone()),
            session: None,
            folder: None,
            folder_generation: 0,
            sort: persisted.sort,
            show_strip: persisted.strip.unwrap_or(true),
            strip_negative: persisted.strip_negative.unwrap_or(true),
            strip_height: persisted.strip_height.unwrap_or(STRIP_HEIGHT).clamp(STRIP_MIN_H, STRIP_MAX_H),
            strip_height_saved: persisted
                .strip_height
                .unwrap_or(STRIP_HEIGHT)
                .clamp(STRIP_MIN_H, STRIP_MAX_H),
            panels: persisted.panels.unwrap_or([true; 3]),
            enlarger_h: None,
            dir_counts: None,
            last_wanted: Vec::new(),
            scroll_to_selected: false,
            current_path: None,
            busy: None,
            log: Vec::new(),
            edit: Edit::new(Weights::default()),
            history: History::new(Edit::new(Weights::default())),
            hover_zone: None,
            size_mode: SizeMode::Native,
            paper_cm: [30.0, 40.0],
            zoom_to_1to1: false,
            wb_choice: WbChoice::AsShot,
            wb_manual: [2.0, 1.0, 1.5],
            mask_mode: DevelopParams::default().mask,
            fix_defects: DevelopParams::default().fix_defects,
            reconstruct: false,
            converter: Converter::Mimizan,
            converter_attempt: None,
            show_mask: false,
            texture: None,
            tex_size: [0, 0],
            tex_rotation: 0,
            last_fit: None,
            keep_scale: None,
            detail_texture: None,
            detail_rect: None,
            wanted_detail: None,
            last_detail_request: None,
            generation: 0,
            shown_generation: 0,
            last_view: None,
            preview_ms: 0,
            hist_view: Histogram::empty(),
            hist_negative: Histogram::empty(),
            hist_source: HistSource::View,
            hist_log: false,
            zoom: 1.0,
            pan: Vec2::ZERO,
            dialog: FileDialog::new(),
            dialog_purpose: DialogPurpose::Export,
            cameras_dir,
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
            panels: Some(self.panels),
            strip_height: Some(self.strip_height),
        }
        .save();
    }

    /// List a folder's RAW files in the strip. The thumbnails follow as the
    /// tiles become visible.
    fn set_folder(&mut self, path: &Path) {
        self.folder_generation += 1;
        // Absolute, so the breadcrumb shows the whole way and a relative
        // CLI argument survives a restart from another directory.
        let path = &std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
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
    /// tool state that pointed into the old one. An older recipe with the
    /// neutral look is put on paper; any other look stays a look (the
    /// Enlarger offers the paper instead of the bar).
    fn set_edit(&mut self, mut e: Edit) {
        if e.rotation != self.edit.rotation {
            self.keep_scale = None;
        }
        if let Tone::Look(l) = &e.recipe.curve {
            if mimizan_core::curve::Curve::from_look(l).is_identity() {
                e.recipe.curve = Tone::paper();
            } else {
                self.log(format!("recipe prints through the look \"{}\", not on paper", l.name));
            }
        }
        self.edit = e;
        self.size_mode = match self.edit.recipe.size {
            None => SizeMode::Native,
            Some(PrintSize::Physical { .. }) => SizeMode::Paper,
        };
        if let Some(PrintSize::Physical { width_in, height_in }) = self.edit.recipe.size {
            self.paper_cm = [width_in * 2.54, height_in * 2.54];
        }
    }

    /// `Recipe::size` from the export section's choice.
    fn sync_size(&mut self) {
        self.edit.recipe.size = match self.size_mode {
            SizeMode::Native => None,
            SizeMode::Paper => Some(PrintSize::Physical {
                width_in: self.paper_cm[0] / 2.54,
                height_in: self.paper_cm[1] / 2.54,
            }),
        };
    }

    fn undo(&mut self) {
        if let Some(e) = self.history.undo().cloned() {
            self.set_edit(e);
        }
    }

    fn redo(&mut self) {
        if let Some(e) = self.history.redo().cloned() {
            self.set_edit(e);
        }
    }

    /// Record the frame's state once the pointer and keyboard let go of the
    /// widgets, so a slider drag is one history step, not sixty.
    fn commit_history(&mut self, ctx: &egui::Context) {
        if ctx.egui_is_using_pointer() || ctx.egui_wants_keyboard_input() {
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
        DevelopParams {
            wb,
            mask: self.mask_mode,
            fix_defects: self.fix_defects,
            reconstruct: self.reconstruct,
            converter: self.converter,
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
            hover_zone: self.hover_zone,
        }
    }

    /// Orientation the export applies: the file's plus the user's turns.
    fn export_orientation(&self) -> u16 {
        let base = self.session.as_ref().map(|s| s.negative.orientation).unwrap_or(1);
        mimizan_core::print::rotated(base, self.rotation())
    }

    /// Size of the picture before the print's size step (upright, after the
    /// user's turns).
    fn export_size(&self) -> (usize, usize) {
        let Some(s) = &self.session else { return (0, 0) };
        let (w, h) = s.upright_size();
        if self.rotation() % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// Output size of the print TIFF (after size step and border).
    fn print_layout(&self) -> Option<mimizan_core::print::Layout> {
        let (w, h) = self.export_size();
        if w == 0 || h == 0 {
            return None;
        }
        let params =
            mimizan_core::print::PrintParams { recipe: self.edit.recipe.clone(), ..Default::default() };
        // Already upright: orientation 1.
        Some(mimizan_core::print::layout(w, h, 1, &params))
    }

    fn start_export(&mut self) {
        let stem = self
            .session
            .as_ref()
            .and_then(|s| s.path.file_stem().map(|f| f.to_string_lossy().to_string()))
            .unwrap_or_else(|| "mimizan".into());
        self.start_dialog(DialogPurpose::Export, Some(format!("{stem}.tif")));
    }

    fn open(&mut self, path: PathBuf) {
        // Absolute like the strip's entries, so the tile lights up.
        let path = std::fs::canonicalize(&path).unwrap_or(path);
        self.busy = Some(format!("opening {}", path.display()));
        if let Some(parent) = path.parent().filter(|p| !p.as_os_str().is_empty()) {
            if self.folder.as_ref().is_none_or(|f| f.path != parent) {
                self.set_folder(parent);
            }
        }
        self.current_path = Some(path.clone());
        self.heavy.send(Job::Open {
            path,
            develop: self.develop_params(),
            cameras_dir: self.cameras_dir.clone(),
        });
    }

    /// Controls back to the session's defaults; the negative stays. A new
    /// file starts a new history; on the same file the reset is one step.
    fn reset(&mut self, fresh: bool) {
        let weights = self.session.as_ref().map(|s| s.default_weights()).unwrap_or_default();
        let e = Edit::new(weights);
        if fresh {
            self.history = History::new(e.clone());
        }
        self.set_edit(e);
        self.tex_rotation = 0;
        self.keep_scale = None;
        self.show_mask = false;
        self.hover_zone = None;
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
    }

    /// Recipe file next to the RAW: `<name>.recipe.json`.
    fn recipe_sidecar(&self) -> Option<PathBuf> {
        let s = self.session.as_ref()?;
        let stem = s.path.file_stem()?.to_string_lossy().to_string();
        Some(s.path.with_file_name(format!("{stem}.recipe.json")))
    }

    fn load_recipe(&mut self, path: &Path) {
        match Recipe::load(path) {
            Ok(r) => {
                let mut e = self.edit.clone();
                e.recipe = r;
                self.set_edit(e);
                self.log(format!("loaded recipe {}", path.display()));
            }
            Err(e) => self.log(format!("recipe not loaded: {e}")),
        }
    }

    fn save_recipe(&mut self, path: &Path) {
        match self.edit.recipe.validate().and_then(|_| self.edit.recipe.save(path)) {
            Ok(()) => self.log(format!("saved recipe {}", path.display())),
            Err(e) => self.log(format!("recipe not saved: {e}")),
        }
    }

    fn handle_events(&mut self, ctx: &egui::Context) {
        while let Some(ev) = self.heavy.poll() {
            match ev {
                Event::Status(s) => self.busy = Some(s),
                Event::Opened(s) => {
                    let fresh = self.session.as_ref().is_none_or(|old| old.path != s.path);
                    let t = &s.negative.info.timing;
                    let step = s.negative.info.demosaic.as_deref().unwrap_or("separate");
                    self.log(format!(
                        "{}: decode {} ms, ingest {} ms, {step} {} ms ({} rounds), {}x{}",
                        s.path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                        t.decode_ms,
                        t.ingest_ms,
                        t.separate_ms,
                        s.negative.info.rounds,
                        s.negative.plane.width,
                        s.negative.plane.height
                    ));
                    self.session = Some(s);
                    if fresh {
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
                    }
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
                }
            }
        }
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
                let img = match &im.zone_mask {
                    // Hovered zone: its pixels in the bar's orange over the grey.
                    Some(mask) => {
                        let pixels: Vec<Color32> = im
                            .gray
                            .iter()
                            .zip(mask.iter())
                            .map(|(&g, &m)| {
                                if m != 0 {
                                    let g = g as u16;
                                    Color32::from_rgb(
                                        ((g + 2 * 255) / 3) as u8,
                                        ((g + 2 * 140) / 3) as u8,
                                        ((g + 2 * 30) / 3) as u8,
                                    )
                                } else {
                                    Color32::from_gray(g)
                                }
                            })
                            .collect();
                        ColorImage::new([im.width, im.height], pixels)
                    }
                    None => ColorImage::from_gray([im.width, im.height], &im.gray),
                };
                let opts = TextureOptions::LINEAR;
                match &mut self.texture {
                    Some(t) if self.tex_size == [im.width, im.height] => t.set(img, opts),
                    _ => self.texture = Some(ctx.load_texture("preview", img, opts)),
                }
                self.tex_size = [im.width, im.height];
                self.hist_view = im.hist_view;
                self.hist_negative = im.hist_negative;
            }
            if let Some(d) = r.detail {
                let img = ColorImage::from_gray([d.rect.w, d.rect.h], &d.gray);
                // Shown at >= 1 screen pixel per image pixel: no filtering, the
                // pixels are the point.
                let opts = TextureOptions::NEAREST;
                match &mut self.detail_texture {
                    Some(t) if self.detail_rect.is_some_and(|r| (r.w, r.h) == (d.rect.w, d.rect.h)) => {
                        t.set(img, opts)
                    }
                    _ => self.detail_texture = Some(ctx.load_texture("detail", img, opts)),
                }
                self.detail_rect = Some(d.rect);
            }
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

    fn screenshot_step(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot_to.clone() else { return };
        match self.screenshot_state {
            // Wait until a preview is on screen (or there is nothing to open).
            0 if self.texture.is_some() && self.busy.is_none() => {
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
                    let res = image::RgbaImage::from_raw(w as u32, h as u32, img.as_raw().to_vec())
                        .ok_or_else(|| anyhow::anyhow!("screenshot buffer"))
                        .and_then(|im| im.save(&path).map_err(Into::into));
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

    /// Switching the converter recomputes immediately. Other development
    /// changes still wait for Apply.
    fn flush_converter_redevelop(&mut self) {
        let Some(s) = self.session.clone() else { return };
        if self.busy.is_some() || self.converter == s.develop.converter {
            if self.converter == s.develop.converter {
                self.converter_attempt = None;
            }
            return;
        }
        if self.converter_attempt == Some(self.converter) {
            return;
        }
        self.converter_attempt = Some(self.converter);
        let label = if self.converter == Converter::Mimizan {
            "re-developing".to_string()
        } else {
            format!("demosaicing with {}", self.converter.label())
        };
        self.busy = Some(label);
        self.heavy.send(Job::Redevelop { session: s, develop: self.develop_params() });
    }

    fn request_preview_if_needed(&mut self) {
        let Some(s) = &self.session else { return };
        let view = self.view_params();
        let view_changed = self.last_view.as_ref() != Some(&view);
        let detail_changed = self.wanted_detail != self.last_detail_request;
        if !view_changed && !detail_changed {
            return;
        }
        if view_changed {
            // Everything shown so far belongs to the old view.
            self.detail_rect = None;
        }
        self.generation += 1;
        self.preview.request(PreviewRequest {
            session: Arc::clone(s),
            view: view.clone(),
            detail: self.wanted_detail,
            generation: self.generation,
        });
        self.last_view = Some(view);
        self.last_detail_request = self.wanted_detail;
    }

    fn start_dialog(&mut self, purpose: DialogPurpose, default_name: Option<String>) {
        self.dialog_purpose = purpose;
        if let Some(n) = default_name {
            self.dialog.config_mut().default_file_name = n;
        }
        match purpose {
            DialogPurpose::LoadRecipe => self.dialog.pick_file(),
            DialogPurpose::Export | DialogPurpose::SaveRecipe => self.dialog.save_file(),
        }
    }

    fn handle_dialog(&mut self, ctx: &egui::Context) {
        self.dialog.update(ctx);
        let Some(path) = self.dialog.take_picked() else { return };
        match self.dialog_purpose {
            DialogPurpose::Export => {
                if let Some(s) = &self.session {
                    let path = with_ext(path, "tif");
                    self.busy = Some("exporting".into());
                    self.heavy.send(Job::Export {
                        session: Arc::clone(s),
                        weights: self.weights(),
                        recipe: self.edit.recipe.clone(),
                        path,
                        orientation: self.export_orientation(),
                    });
                }
            }
            DialogPurpose::SaveRecipe => self.save_recipe(&with_ext(path, "json")),
            DialogPurpose::LoadRecipe => self.load_recipe(&path),
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui
                .selectable_label(self.show_strip, "Strip")
                .on_hover_text(
                    "Show or hide the film strip (B): the folder's RAW files, the way in. ←/→ open the \
                     neighbouring file; a file or folder can also be dropped on the window.",
                )
                .clicked()
            {
                self.toggle_strip();
            }
            // Panel indices: 0 left, 1 right, 2 bottom — shown in the order
            // of the three steps.
            for (i, name, hover) in [
                (
                    0,
                    "Negative",
                    "1 · Judge the negative: file, histograms, development (Tab hides every panel)",
                ),
                (2, "Enlarger", "2 · Expose: mix (the filter), placing, the two paper exposures (the grade)"),
                (1, "Print", "3 · The print: size, border, sharpening, export"),
            ] {
                if ui.selectable_label(self.panels[i], name).on_hover_text(hover).clicked() {
                    self.panels[i] = !self.panels[i];
                    self.persist();
                }
            }
            let has = self.session.is_some();
            if ui
                .add_enabled(has, egui::Button::new("⟳ 90°"))
                .on_hover_text("Rotate the picture 90° clockwise (preview and export)")
                .clicked()
            {
                self.edit.rotation = (self.edit.rotation + 1) % 4;
            }
            ui.separator();
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
            if ui.button("Reset").on_hover_text("Controls back to defaults; the negative stays").clicked() {
                self.reset(false);
            }
            ui.separator();
            ui.menu_button("Recipe", |ui| {
                ui.set_min_width(200.0);
                ui.label(RichText::new("The development as a parameter set (JSON).").weak());
                let sidecar = self.recipe_sidecar();
                if ui
                    .add_enabled(has, egui::Button::new("Save next to the RAW"))
                    .on_hover_text(
                        sidecar
                            .as_ref()
                            .map(|p| p.display().to_string())
                            .unwrap_or_else(|| "<name>.recipe.json".into()),
                    )
                    .clicked()
                {
                    if let Some(p) = sidecar {
                        self.save_recipe(&p);
                    }
                    ui.close();
                }
                if ui.add_enabled(has, egui::Button::new("Save as…")).clicked() {
                    let name = self
                        .session
                        .as_ref()
                        .and_then(|s| s.path.file_stem().map(|f| f.to_string_lossy().to_string()))
                        .unwrap_or_else(|| "mimizan".into());
                    self.start_dialog(DialogPurpose::SaveRecipe, Some(format!("{name}.recipe.json")));
                    ui.close();
                }
                if ui.button("Load…").clicked() {
                    self.start_dialog(DialogPurpose::LoadRecipe, None);
                    ui.close();
                }
            });
            ui.separator();
            // The view: fit / 1:1 and the two overlays.
            if ui
                .add_enabled(has, egui::Button::new("fit"))
                .on_hover_text("Whole picture in the window")
                .clicked()
            {
                self.zoom = 1.0;
                self.pan = Vec2::ZERO;
            }
            if ui
                .add_enabled(has, egui::Button::new("1:1"))
                .on_hover_text("One negative pixel per screen pixel (double-click toggles)")
                .clicked()
            {
                self.zoom_to_1to1 = true;
            }
            ui.checkbox(&mut self.show_mask, "mask")
                .on_hover_text("Show the sharpening mask: white = chroma treated as texture");
            // Status text takes only what is left and is cut, never painted
            // over the buttons.
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.set_max_width(ui.available_width());
                if let Some(b) = self.busy.clone() {
                    ui.spinner();
                    ui.add(egui::Label::new(b).truncate());
                } else if let Some(msg) = self.log.last().cloned() {
                    ui.add(egui::Label::new(RichText::new(msg).weak()).truncate());
                }
            });
        });
    }

    /// Left panel: what the file is, the histograms, the development
    /// settings that recompute the negative, the log.
    fn analysis_panel(&mut self, ui: &mut egui::Ui) {
        let Some(s) = self.session.clone() else {
            ui.label("Open a RAW file or drop it here.");
            ui.add_space(8.0);
            ui.label(RichText::new("Bayer sensors: Nikon Z f (NEF) and other rawler-supported cameras. Monochrome sensors (DNG without CFA) pass through without separation.").weak());
            return;
        };
        let n = &s.negative;
        let info = &n.info;
        // Value cells never widen the panel: long text is elided (the full
        // text is the tooltip), so the title bar and the body stay one width.
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
            val(ui, info.phase.clone().map(|p| format!("Bayer {p}")).unwrap_or_else(|| "mono".into()));
            ui.end_row();
            ui.label("orientation");
            val(ui, if self.rotation() == 0 {
                format!("{}", n.orientation)
            } else {
                format!("{} + {}° cw", n.orientation, 90 * self.rotation() as u16)
            });
            ui.end_row();
            ui.label("balance");
            val(ui, format!("R {:.3}  B {:.3} ({})", info.wb[0], info.wb[2], info.report.wb_source));
            ui.end_row();
            let px = n.plane.data.len() as f64;
            ui.label("saturated");
            val(ui, format!(
                "{:.2} % ({:.2} % dilated)",
                100.0 * info.report.saturated_raw as f64 / px,
                100.0 * info.report.saturated_dilated as f64 / px
            ));
            ui.end_row();
            ui.label("defects");
            val(ui, format!("{} ({:.3} %)", info.report.defects, 100.0 * info.report.defects as f64 / px));
            ui.end_row();
            ui.label("camera file");
            match &s.camera_file_path {
                Some(p) => val(ui, p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default())
                    .on_hover_text(p.display().to_string()),
                None => val(ui, RichText::new("none (defaults)").weak()),
            };
            ui.end_row();
            ui.label("converter");
            val(ui, info.demosaic.as_deref().unwrap_or("Mimizan"));
            ui.end_row();
            if info.demosaic.is_none() {
                ui.label("mask");
                val(ui, format!("{:?}, {} round(s)", info.mask, info.rounds));
                ui.end_row();
                if info.reconstruct {
                    ui.label("reconstruct");
                    val(ui,
                        RichText::new(format!("on: {:.1} % of the picture computed", 100.0 * info.computed))
                            .color(egui::Color32::from_rgb(230, 170, 60)),
                    )
                    .on_hover_text("Mean of the per-pixel share that came from the direction decision; the sidecar mask holds it per pixel (204 = measured ... 252 = computed).");
                    ui.end_row();
                }
            }
            ui.label("timing");
            let step = if info.demosaic.is_some() { "demosaic" } else { "separate" };
            val(ui, format!(
                "decode {} · ingest {} · {step} {} ms",
                info.timing.decode_ms, info.timing.ingest_ms, info.timing.separate_ms
            ));
            ui.end_row();
            ui.label("preview");
            val(ui, format!(
                "{} x {} ({:.1} %), {} ms",
                self.tex_size[0],
                self.tex_size[1],
                100.0 * s.preview.scale,
                self.preview_ms
            ));
            ui.end_row();
        });

        ui.add_space(10.0);
        ui.separator();
        self.histogram_section(ui);
        ui.collapsing("Development", |ui| {
            ui.label(RichText::new("Changes here recompute the picture.").weak());
            egui::ComboBox::from_label("converter").selected_text(self.converter.label()).show_ui(ui, |ui| {
                for c in Converter::ALL {
                    ui.selectable_value(&mut self.converter, c, c.label());
                }
            });
            let mimizan = self.converter == Converter::Mimizan;
            if mimizan {
                ui.label(RichText::new("Separation, no demosaic. The default.").weak());
            } else {
                ui.label(
                    RichText::new(
                        "Demosaic, then the same weights and look. Mask and reconstruct stay with Mimizan.",
                    )
                    .weak(),
                );
            }
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
            ui.add_enabled_ui(mimizan, |ui| {
                egui::ComboBox::from_label("mask").selected_text(format!("{:?}", self.mask_mode)).show_ui(
                    ui,
                    |ui| {
                        ui.selectable_value(&mut self.mask_mode, MaskMode::Dubois, "Dubois");
                        ui.selectable_value(&mut self.mask_mode, MaskMode::Adaptive, "Adaptive");
                        ui.selectable_value(&mut self.mask_mode, MaskMode::Off, "Off");
                    },
                );
                ui.checkbox(&mut self.reconstruct, "reconstruct the last octave");
            });
            ui.checkbox(&mut self.fix_defects, "repair defects");
            if mimizan && self.reconstruct {
                ui.label(
                    RichText::new(
                        "Missing pixels are computed, not measured: where luminance and colour \
                         overlap at the pixel grid, a per-pixel direction decision replaces the \
                         linear filter. Sharper fine detail, but no longer a fixed-weight sum of \
                         measured values. The sidecar records where.",
                    )
                    .small()
                    .color(egui::Color32::from_rgb(230, 170, 60)),
                );
            }
            let differs = self.develop_params() != s.develop;
            if ui.add_enabled(differs && self.busy.is_none(), egui::Button::new("Apply")).clicked() {
                self.busy = Some("re-developing".into());
                self.heavy.send(Job::Redevelop { session: Arc::clone(&s), develop: self.develop_params() });
            }
        });

        ui.collapsing("Log", |ui| {
            egui::ScrollArea::vertical().max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                for line in self.log.iter().rev().take(30).rev() {
                    ui.label(RichText::new(line).small().weak());
                }
            });
        });
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
            ui.radio_value(&mut self.hist_source, HistSource::Raw, "raw").on_hover_text(
                "The four sensor channels as shot (R, G1, G2, B by tile position), each normalised by its \
                 black and white level, on a gamma-2.2 axis. The percentages are pixels at the white level.",
            );
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
        histogram_widget(ui, h, egui::vec2(width, 96.0), self.hist_log);
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
    fn enlarger_panel(&mut self, ui: &mut egui::Ui) {
        // The zone bar sets this again while the pointer is over it.
        self.hover_zone = None;
        let width = ui.available_width().max(240.0);
        self.zones_column(ui, width);
    }

    /// Mix as one row, placing, the bar, the grades and the paper's ends.
    fn zones_column(&mut self, ui: &mut egui::Ui, width: f32) {
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("mix").weak());
            self.mix_combo(ui);
            let cur = self.weights();
            let (mut r, mut b) = (cur.r, cur.b);
            ui.label(RichText::new("R").weak());
            if ui.add(egui::DragValue::new(&mut r).range(0.0..=1.0).speed(0.002).fixed_decimals(3)).changed()
            {
                self.set_weights(Weights { r, g: 0.0, b: cur.b.min(1.0 - r) });
            }
            ui.label(RichText::new("B").weak());
            if ui.add(egui::DragValue::new(&mut b).range(0.0..=1.0).speed(0.002).fixed_decimals(3)).changed()
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
        });
        ui.add_space(6.0);
        let Some(split) = self.edit.recipe.curve.as_paper().cloned() else {
            // A look from a file or an older recipe: no paper to expose.
            ui.label(
                RichText::new(format!(
                    "This recipe prints through the look \"{}\", not on paper.",
                    self.edit.recipe.curve.name()
                ))
                .weak(),
            );
            if ui
                .button("Print on paper")
                .on_hover_text("Replace the look by the paper at its base exposure")
                .clicked()
            {
                self.edit.recipe.curve = Tone::paper();
            }
            self.hover_zone = None;
            return;
        };
        let mut split = split;
        self.hover_zone = zone_bar(ui, &mut split, width);
        ui.add_space(4.0);
        let slider_w = (width - 170.0).max(120.0);
        let exposure = |ui: &mut egui::Ui, label: &str, filter: &str, v: &mut f64, hint: &str| {
            ui.horizontal(|ui| {
                ui.spacing_mut().slider_width = slider_w;
                ui.label(RichText::new(label).weak()).on_hover_text(hint);
                let changed = ui
                    .add(
                        egui::Slider::new(v, -SLIDER_STOPS..=SLIDER_STOPS)
                            .fixed_decimals(2)
                            .step_by(1.0 / 12.0)
                            .suffix(" stops"),
                    )
                    .on_hover_text(hint)
                    .changed();
                ui.label(RichText::new(filter).weak().small());
                changed
            });
        };
        exposure(
            ui,
            "soft",
            "filter 00",
            &mut split.soft,
            "The soft exposure, in stops of time from the base print. More soft light greys the \
             highlights and hardly touches the shadows: the highlights' exposure.",
        );
        exposure(
            ui,
            "hard",
            "filter 5",
            &mut split.hard,
            "The hard exposure, in stops of time from the base print. More hard light deepens the \
             shadows and the mid tones with them, the highlights stay: the shadows' exposure.",
        );
        if split != *self.edit.recipe.curve.as_paper().unwrap() {
            self.edit.recipe.curve = Tone::Paper(split.clone());
        }
        let curve = split.curve();
        ui.horizontal(|ui| {
            ui.label(
                RichText::new(format!(
                    "≈ grade {} · {:.1} stops of scene · grey card prints at {:.2}",
                    paper::grade_label(curve.grade()),
                    curve.scene_range_stops(),
                    curve.eval(zone::GRAY_CARD)
                ))
                .weak(),
            )
            .on_hover_text(
                "The single filter grade this pair of exposures equals (by its ISO range), the scene \
                 stops between the paper's first tone and its near-black, and where Zone V lands.",
            );
            if (split.soft != 0.0 || split.hard != 0.0)
                && ui.small_button("base").on_hover_text("Back to the base exposure").clicked()
            {
                self.edit.recipe.curve = Tone::paper();
            }
            if self.edit.recipe.place != 0.0 {
                ui.label(RichText::new(format!("place {:+.2}", self.edit.recipe.place)).weak().small())
                    .on_hover_text(
                        "A placing from the recipe, in scene stops; the bar moves both exposures instead.",
                    );
                if ui.small_button("0").on_hover_text("Drop the placing").clicked() {
                    self.edit.recipe.place = 0.0;
                }
            }
        });
    }

    /// Right window, step 3: the print and its export.
    fn print_panel(&mut self, ui: &mut egui::Ui) {
        self.print_section(ui);
        ui.add_space(12.0);
        ui.separator();
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
        {
            self.start_export();
        }
        ui.label(
            RichText::new(
                "Renders this recipe against the negative in one pass: the print as a 16-bit TIFF.",
            )
            .weak(),
        );
    }

    /// The print: size, resolution, border, resampling and sharpening. The
    /// export renders exactly this recipe in one pass.
    fn print_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Print");
        let mut mode = self.size_mode;
        ui.horizontal_wrapped(|ui| {
            ui.selectable_value(&mut mode, SizeMode::Native, "native").on_hover_text("The negative's pixels");
            ui.selectable_value(&mut mode, SizeMode::Paper, "paper")
                .on_hover_text("Fit into a paper size at the dpi");
        });
        self.size_mode = mode;
        match mode {
            SizeMode::Paper => {
                ui.horizontal(|ui| {
                    ui.add(
                        egui::DragValue::new(&mut self.paper_cm[0])
                            .range(1.0..=300.0)
                            .speed(0.1)
                            .suffix(" cm"),
                    );
                    ui.label("×");
                    ui.add(
                        egui::DragValue::new(&mut self.paper_cm[1])
                            .range(1.0..=300.0)
                            .speed(0.1)
                            .suffix(" cm"),
                    );
                    ui.label(RichText::new("at").weak());
                    ui.add(
                        egui::DragValue::new(&mut self.edit.recipe.dpi)
                            .range(36.0..=1200.0)
                            .speed(1.0)
                            .suffix(" dpi"),
                    );
                    if ui.small_button("⇄").on_hover_text("Swap width and height").clicked() {
                        self.paper_cm.swap(0, 1);
                    }
                });
            }
            SizeMode::Native => {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("dpi for the border").weak());
                    ui.add(egui::DragValue::new(&mut self.edit.recipe.dpi).range(36.0..=1200.0).speed(1.0));
                });
            }
        }
        self.sync_size();
        ui.horizontal(|ui| {
            ui.label(RichText::new("border").weak());
            ui.add(
                egui::DragValue::new(&mut self.edit.recipe.border_mm)
                    .range(0.0..=100.0)
                    .speed(0.2)
                    .suffix(" mm"),
            )
            .on_hover_text("Paper white around the picture, taken from the paper size");
            ui.label(RichText::new("resample").weak());
            let r = &mut self.edit.recipe.resample;
            egui::ComboBox::from_id_salt("resample").selected_text(r.name()).show_ui(ui, |ui| {
                for k in ResampleKernel::ALL {
                    ui.selectable_value(r, k, k.name()).on_hover_text(match k {
                        ResampleKernel::Lanczos3 => "Sharpest; rings at hard edges (the benchmark default)",
                        ResampleKernel::Mitchell => {
                            "Mitchell–Netravali B=C=⅓: slight softness, little ringing"
                        }
                        ResampleKernel::CatmullRom => "Interpolating cubic; rings like a short Lanczos",
                        ResampleKernel::BSpline => "No overshoot at all, visibly soft",
                    });
                }
            });
        });
        let s = &mut self.edit.recipe.sharpen;
        let mut kind = match s {
            Sharpen::Off => 0,
            Sharpen::Usm { .. } => 1,
            Sharpen::Deconv { .. } => 2,
        };
        ui.horizontal_wrapped(|ui| {
            ui.label(RichText::new("sharpen").weak());
            ui.selectable_value(&mut kind, 0, "off");
            ui.selectable_value(&mut kind, 1, "USM")
                .on_hover_text("Unsharp mask for the viewing distance (radius from size and dpi)");
            ui.selectable_value(&mut kind, 2, "deconvolution")
                .on_hover_text("Richardson–Lucy against a Gaussian of the USM radius (an assumption, not a measured PSF), then optional USM");
        });
        let current = match s {
            Sharpen::Off => 0,
            Sharpen::Usm { .. } => 1,
            Sharpen::Deconv { .. } => 2,
        };
        if kind != current {
            *s = match kind {
                1 => Sharpen::Usm { amount: 0.5 },
                2 => Sharpen::Deconv { passes: 10, usm_amount: 0.0 },
                _ => Sharpen::Off,
            };
        }
        match s {
            Sharpen::Usm { amount } => {
                ui.add(egui::Slider::new(amount, 0.0..=USM_MAX).text("amount").fixed_decimals(2));
            }
            Sharpen::Deconv { passes, usm_amount } => {
                ui.add(egui::Slider::new(passes, 1..=DECONV_ITERATIONS as u32).text("passes"));
                ui.add(egui::Slider::new(usm_amount, 0.0..=USM_MAX).text("USM after").fixed_decimals(2));
            }
            Sharpen::Off => {}
        }
        if let Some(l) = self.print_layout() {
            let (uw, uh) = self.export_size();
            let txt = if l.border_px > 0 {
                format!(
                    "{uw} × {uh} → {} × {} px picture + {} px border = {} × {} px",
                    l.image_w, l.image_h, l.border_px, l.out_w, l.out_h
                )
            } else if (l.image_w, l.image_h) != (uw, uh) {
                format!("{uw} × {uh} → {} × {} px", l.image_w, l.image_h)
            } else {
                format!("{uw} × {uh} px, no size step")
            };
            ui.label(RichText::new(txt).weak());
        }
    }

    /// What one export writes, for the toolbar button.
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
        let sharpen = match r.sharpen {
            Sharpen::Off => "unsharpened".to_string(),
            Sharpen::Usm { amount } => format!("USM {amount:.2}"),
            Sharpen::Deconv { passes, .. } => format!("deconvolution {passes} passes"),
        };
        format!(
            "Writes the print on the {look}{rot}{local}:\n\
             <name>.tif  {ow} x {oh} px, 16 bit, gamma 2.2 grey ICC, {sharpen}, recipe in the description"
        )
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(avail, Sense::click_and_drag());
        let painter = ui.painter_at(avail);
        painter.rect_filled(avail, 0.0, Color32::from_gray(28));
        let Some(tex) = self.texture.as_ref().map(|t| t.id()) else {
            if self.session.is_none() {
                painter.text(
                    avail.center(),
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
        // The paper border, in texture pixels: the print's border against
        // its picture (`layout`), so it keeps its proportion at any size and
        // dpi. A flat value, so it is painted under the picture, not
        // rendered into it; the mask view is not a print and has none.
        let (border_tex, white) = if self.show_mask {
            (0.0, 0)
        } else {
            let b = self
                .print_layout()
                .filter(|l| l.border_px > 0)
                .map(|l| l.border_px as f32 * tw as f32 / l.image_w.max(1) as f32)
                .unwrap_or(0.0);
            let w = self.edit.recipe.curve.compile().white();
            (b, (w.clamp(0.0, 1.0) * 255.0).round() as u8)
        };
        // The sheet (picture plus border) is what fits the stage.
        let fit = (avail.width() / (tw as f32 + 2.0 * border_tex))
            .min(avail.height() / (th as f32 + 2.0 * border_tex));
        self.last_fit = Some(fit);
        if let Some(scale) = self.keep_scale.take() {
            self.zoom = (scale / fit).clamp(1.0, MAX_ZOOM);
        }
        if response.hovered() {
            let zoom_delta = ui.input(|i| i.zoom_delta());
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            let factor = if zoom_delta != 1.0 { zoom_delta } else { (scroll / 300.0).exp() };
            if factor != 1.0 {
                let old = self.zoom;
                self.zoom = (self.zoom * factor).clamp(1.0, MAX_ZOOM);
                // Zoom about the pointer: keep the image point under it fixed.
                if let Some(ptr) = response.hover_pos() {
                    let c = avail.center() + self.pan;
                    let rel = ptr - c;
                    self.pan += rel - rel * (self.zoom / old);
                }
            }
        }
        if response.dragged() {
            self.pan += response.drag_delta();
        }
        let (uw, uh) = self.export_size();
        let ppp = ui.pixels_per_point();
        // Zoom at which one negative pixel is one screen pixel.
        let zoom_1to1 = (uw as f32 / (tw as f32 * fit * ppp)).clamp(1.0, MAX_ZOOM);
        if std::mem::take(&mut self.zoom_to_1to1) {
            // About the viewport centre.
            self.pan *= zoom_1to1 / self.zoom;
            self.zoom = zoom_1to1;
        }
        if response.double_clicked() {
            // Toggle between fit and 1:1 about the pointer.
            let old = self.zoom;
            self.zoom = if (self.zoom - 1.0).abs() < 1e-3 { zoom_1to1 } else { 1.0 };
            if let Some(ptr) = response.interact_pointer_pos() {
                let c = avail.center() + self.pan;
                let rel = ptr - c;
                self.pan += rel - rel * (self.zoom / old);
            }
            if self.zoom <= 1.0 {
                self.pan = Vec2::ZERO;
            }
        }
        let scale = fit * self.zoom;
        let size = Vec2::new(tw as f32 * scale, th as f32 * scale);
        let border_pts = border_tex * scale;
        // Keep the sheet covering the viewport when zoomed, centred otherwise.
        let max_pan = ((size + Vec2::splat(2.0 * border_pts) - avail.size()) * 0.5).max(Vec2::ZERO);
        self.pan = self.pan.clamp(-max_pan, max_pan);
        let center = avail.center() + self.pan;
        let rect = Rect::from_center_size(center, size);
        if border_pts > 0.0 {
            painter.rect_filled(rect.expand(border_pts), 0.0, Color32::from_gray(white));
        }
        painter.image(tex, rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);

        // Magnification in screen pixels per negative pixel.
        let mag = size.x * ppp / uw.max(1) as f32;
        // Once the fit preview would be upscaled, show the real pixels of the
        // visible window instead (rendered by the worker, never sharpened).
        let pts_per_px = size.x / uw.max(1) as f32;
        let texel_px = size.x * ppp / tw as f32;
        self.wanted_detail = if texel_px > 1.0 && uw > 0 && uh > 0 {
            let vis = avail.intersect(rect);
            let x0 = ((vis.left() - rect.left()) / pts_per_px).floor().max(0.0) as usize;
            let y0 = ((vis.top() - rect.top()) / pts_per_px).floor().max(0.0) as usize;
            let x1 = (((vis.right() - rect.left()) / pts_per_px).ceil() as usize).min(uw);
            let y1 = (((vis.bottom() - rect.top()) / pts_per_px).ceil() as usize).min(uh);
            (x1 > x0 && y1 > y0).then_some(DetailRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 })
        } else {
            None
        };
        if self.wanted_detail.is_none() {
            self.detail_rect = None;
        }
        if let (Some(dt), Some(dr)) = (&self.detail_texture, self.detail_rect) {
            let min = rect.min + Vec2::new(dr.x as f32, dr.y as f32) * pts_per_px;
            let drect = Rect::from_min_size(min, Vec2::new(dr.w as f32, dr.h as f32) * pts_per_px);
            painter.image(
                dt.id(),
                drect,
                Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)),
                Color32::WHITE,
            );
        }

        if self.session.is_some() {
            let txt = if self.detail_rect.is_some() {
                format!("{:.0} % · full-resolution negative pixels, unsharpened", 100.0 * mag)
            } else {
                format!("{:.0} % of full size", 100.0 * mag)
            };
            let galley =
                painter.layout_no_wrap(txt, egui::FontId::proportional(13.0), Color32::from_gray(220));
            let pos = avail.left_bottom() + Vec2::new(8.0, -8.0 - galley.size().y);
            let bg = Rect::from_min_size(pos, galley.size()).expand(4.0);
            painter.rect_filled(bg, 3.0, Color32::from_black_alpha(150));
            painter.galley(pos, galley, Color32::from_gray(220));
        }
    }
}

/// Where the strip starts when nothing is remembered: the pictures folder
/// if there is one, else home.
fn start_folder() -> Option<PathBuf> {
    let home = PathBuf::from(std::env::var_os("HOME")?);
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
    /// keyboard is not in a text field.
    fn strip_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        let (b, left, right) = ctx.input(|i| {
            (
                i.key_pressed(egui::Key::B) && i.modifiers.is_none(),
                i.key_pressed(egui::Key::ArrowLeft),
                i.key_pressed(egui::Key::ArrowRight),
            )
        });
        if b {
            self.toggle_strip();
        }
        if self.show_strip && left {
            self.open_neighbour(-1);
        }
        if self.show_strip && right {
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
                    // Breadcrumb: every ancestor is a button.
                    let comps: Vec<PathBuf> = f.path.ancestors().map(Path::to_path_buf).collect();
                    for (i, anc) in comps.iter().rev().enumerate() {
                        let name = if i == 0 {
                            "/".to_string()
                        } else {
                            anc.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_default()
                        };
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
                if ui
                    .selectable_label(self.strip_negative, "negative")
                    .on_hover_text("Show the frames as the film holds them (inverted), or as the print")
                    .clicked()
                {
                    self.strip_negative = !self.strip_negative;
                    if let Some(f) = &mut self.folder {
                        for e in &mut f.entries {
                            e.texture = None;
                        }
                    }
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
        let Some(folder) = &mut self.folder else { return };
        if folder.entries.is_empty() && folder.subdirs.is_empty() {
            ui.label(RichText::new("No RAW files and no folders here.").weak());
            return;
        }
        let idle = self.busy.is_none();
        let current = self.current_path.clone();
        let generation = folder.generation;
        let negative = self.strip_negative;
        let scroll_to = std::mem::take(&mut self.scroll_to_selected);
        let mut clicked: Option<PathBuf> = None;
        let mut go_dir: Option<PathBuf> = None;
        // (distance from the viewport centre, index) of tiles that still
        // need a thumbnail; visible ones first, then two screens around.
        let mut wanted: Vec<(f32, usize)> = Vec::new();
        let ctx = ui.ctx().clone();
        // Frame geometry follows the panel height: whatever is left under
        // the bar (minus the scrollbar) is one film frame.
        let tile_h = (ui.available_height() - ui.spacing().scroll.allocated_width()).max(60.0);
        let img_h = (tile_h - 2.0 * PERF_H - EDGE_H).max(30.0);
        let tile_w = (img_h * TILE_W / TILE_IMG_H).round();
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
                        continue;
                    }
                    if e.texture.is_none() {
                        if let Some(t) = &e.thumb {
                            let img = if negative {
                                ColorImage::from_gray_iter(
                                    [t.width, t.height],
                                    t.gray.iter().map(|&g| 255 - g),
                                )
                            } else {
                                ColorImage::from_gray([t.width, t.height], &t.gray)
                            };
                            e.texture = Some(ctx.load_texture(
                                format!("thumb-{generation}-{i}-{}", negative as u8),
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
        let paths: Vec<PathBuf> = wanted.iter().map(|(_, i)| folder.entries[*i].path.clone()).collect();
        if paths != self.last_wanted {
            let jobs: Vec<ThumbJob> = wanted
                .iter()
                .map(|(_, i)| {
                    let e = &folder.entries[*i];
                    ThumbJob { generation, path: e.path.clone(), size: e.size, modified: e.modified }
                })
                .collect();
            self.thumbs.want(jobs);
            self.last_wanted = paths;
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
        self.handle_events(ctx);
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
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
        // What is left is the darkroom: the three docked windows and, in
        // the room between them, the picture — never under a window.
        let docks = egui::CentralPanel::default()
            .frame(egui::Frame::NONE)
            .show(ui, |ui| {
                let docks = self.docks(ui.available_rect_before_wrap());
                ui.scope_builder(egui::UiBuilder::new().max_rect(docks.stage), |ui| self.preview_panel(ui));
                docks
            })
            .inner;
        self.dock_windows(&ctx, &docks);

        self.edit_keys(&ctx);
        self.commit_history(&ctx);
        self.request_preview_if_needed();
        self.flush_converter_redevelop();
        self.strip_keys(&ctx);
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
/// Gap between the stage edge and a panel.
const PANEL_GAP: f32 = 12.0;

/// The dark translucent glass the panels are made of. Plain alpha, no
/// blur: it has to look the same on every GPU driver and in software.
fn glass_frame() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::from_rgba_unmultiplied(14, 14, 16, 222))
        .stroke(egui::Stroke::new(1.0, Color32::from_white_alpha(20)))
        .corner_radius(10.0)
        .inner_margin(12.0)
        .shadow(egui::Shadow { offset: [0, 6], blur: 26, spread: 0, color: Color32::from_black_alpha(150) })
}

impl App {
    /// Where the three windows stand and what is left for the picture.
    fn docks(&self, full: Rect) -> Docks {
        let gap = PANEL_GAP;
        let h = (full.height() - 2.0 * gap).max(120.0);
        let left = self.panels[0]
            .then(|| Rect::from_min_size(full.left_top() + Vec2::splat(gap), Vec2::new(PANEL_W, h)));
        let right = self.panels[1].then(|| {
            Rect::from_min_size(
                Pos2::new(full.right() - gap - PANEL_W, full.top() + gap),
                Vec2::new(PANEL_W, h),
            )
        });
        let x0 = left.map(|r| r.right() + gap).unwrap_or(full.left() + gap);
        let x1 = right.map(|r| r.left() - gap).unwrap_or(full.right() - gap);
        let bottom = self.panels[2].then(|| {
            let bh = self.enlarger_h.unwrap_or(190.0).min(h);
            Rect::from_min_max(Pos2::new(x0, full.bottom() - gap - bh), Pos2::new(x1, full.bottom() - gap))
        });
        let stage = Rect::from_min_max(
            Pos2::new(x0, full.top()),
            Pos2::new(x1, bottom.map(|r| r.top() - gap).unwrap_or(full.bottom())),
        );
        Docks { left, right, bottom, stage }
    }

    /// The three docked windows: the negative left, the print right (both
    /// at the stage's full height), the enlarger at the bottom between them.
    /// Nothing is movable; × closes, the toolbar or Tab brings one back.
    fn dock_windows(&mut self, ctx: &egui::Context, docks: &Docks) {
        let mut open = self.panels;
        if let Some(r) = docks.left {
            egui::Window::new("Negative")
                .frame(glass_frame())
                .open(&mut open[0])
                .fixed_pos(r.min)
                .movable(false)
                .collapsible(false)
                .default_width(PANEL_W)
                .min_width(PANEL_W)
                .max_width(PANEL_W)
                .default_height(r.height())
                .min_height(r.height())
                .max_height(r.height())
                .resizable([false, true])
                .vscroll(true)
                .show(ctx, |ui| self.analysis_panel(ui));
        }
        if let Some(r) = docks.right {
            egui::Window::new("Print")
                .frame(glass_frame())
                .open(&mut open[1])
                .fixed_pos(r.min)
                .movable(false)
                .collapsible(false)
                .default_width(PANEL_W)
                .min_width(PANEL_W)
                .max_width(PANEL_W)
                .default_height(r.height())
                .min_height(r.height())
                .max_height(r.height())
                .resizable([false, true])
                .vscroll(true)
                .show(ctx, |ui| self.print_panel(ui));
        }
        if let Some(r) = docks.bottom {
            // Height follows the content; the stage is laid out with the
            // measured value of the last frame.
            let shown = egui::Window::new("Enlarger")
                .frame(glass_frame())
                .open(&mut open[2])
                .fixed_pos(r.min)
                .movable(false)
                .collapsible(false)
                .default_width(r.width())
                .min_width(r.width())
                .max_width(r.width())
                .resizable(false)
                .show(ctx, |ui| self.enlarger_panel(ui));
            if let Some(s) = shown {
                let h = s.response.rect.height();
                if self.enlarger_h.is_none_or(|old| (old - h).abs() > 0.5) {
                    self.enlarger_h = Some(h);
                    ctx.request_repaint();
                }
            }
        }
        if open != self.panels {
            self.panels = open;
            self.persist();
        }
    }

    /// Ctrl+Z / Ctrl+Shift+Z / Ctrl+Y: undo and redo; Tab hides or shows
    /// every panel. Not while a text field has the keyboard.
    fn edit_keys(&mut self, ctx: &egui::Context) {
        if ctx.egui_wants_keyboard_input() {
            return;
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Tab)) {
            let any = self.panels.iter().any(|&p| p);
            self.panels = [!any; 3];
            self.persist();
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
