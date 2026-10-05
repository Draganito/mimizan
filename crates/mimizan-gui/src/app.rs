//! Window layout: toolbar, file column (left), preview (centre), controls
//! (right). Nothing here changes pixels; it only chooses parameters and hands
//! them to the workers.

use crate::browser::{Folder, Persisted, Sort, ThumbJob, ThumbState, Thumbs};
use crate::curve_editor::curve_editor;
use crate::histogram::histogram_widget;
use crate::session::{Converter, DetailRect, DevelopParams, Histogram, Session, ViewParams};
use crate::worker::{Event, Heavy, Job, Preview, PreviewRequest};
use egui::{Color32, ColorImage, Pos2, Rect, RichText, Sense, TextureHandle, TextureOptions, Vec2};
use egui_file_dialog::FileDialog;
use mimizan_core::calib::{LookEncoding, LookFile};
use mimizan_core::ingest::WhiteBalance;
use mimizan_core::mix::{ColorFilter, WeightSpace, Weights};
use mimizan_core::separate::MaskMode;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LookChoice {
    Preset(usize),
    Custom,
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
    /// The 8-bit view as shown (after look and screen compensation, or the mask).
    View,
    /// The linear mix before the curve, on a gamma-2.2 axis.
    Negative,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DialogPurpose {
    Open,
    Folder,
    Export,
    SaveLook,
    LoadLook,
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
    /// Priority list last sent to the thumbnail worker.
    last_wanted: Vec<PathBuf>,
    /// Keyboard moved the selection: bring the tile into view once.
    scroll_to_selected: bool,
    /// Picture the strip should treat as current (the one opening or open).
    current_path: Option<PathBuf>,

    busy: Option<String>,
    log: Vec<String>,

    // Mix
    w_r: f64,
    w_b: f64,

    // Look
    looks: Vec<LookFile>,
    look_choice: LookChoice,
    custom_points: Vec<[f64; 2]>,
    custom_name: String,

    // Export: long edge of the screen JPEG; the TIFF is always full size.
    /// "1:1" button pressed; applied by the preview panel, which knows the fit scale.
    zoom_to_1to1: bool,
    /// Clockwise quarter turns added to the file's orientation (0..3).
    rotation: u8,

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
    look_dir: PathBuf,

    /// `MIMIZAN_SCREENSHOT=<png>`: capture the window once the first preview
    /// is on screen, then quit. Used for unattended checks and the README.
    screenshot_to: Option<PathBuf>,
    screenshot_state: u8,
}

/// Zoom relative to "fit"; enough for 1:1 on a 100 MP file in a small window.
const MAX_ZOOM: f32 = 40.0;

/// Long edge of the screen JPEG: the size most galleries display 1:1.
const SCREEN_LONG_EDGE: usize = 2048;

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

fn load_looks(dir: &Path) -> Vec<LookFile> {
    let mut looks = vec![LookFile::neutral()];
    if let Ok(rd) = std::fs::read_dir(dir) {
        let mut paths: Vec<PathBuf> = rd
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| p.extension().is_some_and(|e| e == "json"))
            .collect();
        paths.sort();
        for p in paths {
            match LookFile::load(&p) {
                Ok(l) if l.name != "neutral" => looks.push(l),
                Ok(_) => {}
                Err(e) => tracing::warn!("look {}: {e}", p.display()),
            }
        }
    }
    looks
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>, initial: Option<PathBuf>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        let look_dir = resource_dir("look");
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
            last_wanted: Vec::new(),
            scroll_to_selected: false,
            current_path: None,
            busy: None,
            log: Vec::new(),
            w_r: 0.25,
            w_b: 0.25,
            looks: load_looks(&look_dir),
            look_choice: LookChoice::Preset(0),
            custom_points: vec![[0.0, 0.0], [0.25, 0.22], [0.75, 0.78], [1.0, 1.0]],
            custom_name: "custom".into(),
            zoom_to_1to1: false,
            rotation: 0,
            wb_choice: WbChoice::AsShot,
            wb_manual: [2.0, 1.0, 1.5],
            mask_mode: DevelopParams::default().mask,
            fix_defects: true,
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
            dialog_purpose: DialogPurpose::Open,
            cameras_dir,
            look_dir,
            screenshot_to: std::env::var_os("MIMIZAN_SCREENSHOT").map(PathBuf::from),
            screenshot_state: 0,
        };
        match initial {
            Some(p) if p.is_dir() => app.set_folder(&p),
            Some(p) => app.open(p),
            None => {
                if let Some(f) = persisted.folder.filter(|f| f.is_dir()) {
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
                self.log(format!("{}: {} RAW files", f.path.display(), f.entries.len()));
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
        let r = self.w_r.clamp(0.0, 1.0);
        let b = self.w_b.clamp(0.0, 1.0 - r);
        Weights { r, g: 1.0 - r - b, b }
    }

    fn set_weights(&mut self, w: Weights) {
        self.w_r = w.r;
        self.w_b = w.b;
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

    fn current_look(&self) -> LookFile {
        match self.look_choice {
            LookChoice::Preset(i) => self.looks.get(i).cloned().unwrap_or_else(LookFile::neutral),
            LookChoice::Custom => LookFile {
                schema: 1,
                name: self.custom_name.clone(),
                points: self.custom_points.clone(),
                encoding: LookEncoding::Gamma22,
                fitted_against: None,
            },
        }
    }

    fn view_params(&self) -> ViewParams {
        ViewParams {
            weights: self.weights(),
            look: self.current_look(),
            show_mask: self.show_mask,
            rotation: self.rotation,
        }
    }

    /// Orientation the export applies: the file's plus the user's turns.
    fn export_orientation(&self) -> u16 {
        let base = self.session.as_ref().map(|s| s.negative.orientation).unwrap_or(1);
        mimizan_core::print::rotated(base, self.rotation)
    }

    /// Size of the exported TIFF (upright, after the user's turns).
    fn export_size(&self) -> (usize, usize) {
        let Some(s) = &self.session else { return (0, 0) };
        let (w, h) = s.upright_size();
        if self.rotation % 2 == 1 {
            (h, w)
        } else {
            (w, h)
        }
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

    /// Controls back to the session's defaults; the negative stays.
    fn reset(&mut self) {
        if let Some(s) = self.session.clone() {
            self.set_weights(s.default_weights());
        } else {
            self.set_weights(Weights::default());
        }
        self.look_choice = LookChoice::Preset(0);
        self.rotation = 0;
        self.tex_rotation = 0;
        self.keep_scale = None;
        self.show_mask = false;
        self.zoom = 1.0;
        self.pan = Vec2::ZERO;
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
                        self.reset();
                        if self.screenshot_to.is_some()
                            && std::env::var_os("MIMIZAN_SCREENSHOT_CUSTOM").is_some()
                        {
                            // Show the curve editor in the unattended capture.
                            self.look_choice = LookChoice::Custom;
                        }
                    }
                    self.busy = None;
                    self.last_view = None;
                }
                Event::Exported { tiff, jpeg, width, height, jpeg_width, jpeg_height, ms } => {
                    self.log(format!(
                        "wrote {} ({width}x{height}) and {} ({jpeg_width}x{jpeg_height}) in {ms} ms",
                        tiff.display(),
                        jpeg.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default()
                    ));
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
                let img = ColorImage::from_gray([im.width, im.height], &im.gray);
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
            DialogPurpose::Open | DialogPurpose::LoadLook => self.dialog.pick_file(),
            DialogPurpose::Folder => self.dialog.pick_directory(),
            _ => self.dialog.save_file(),
        }
    }

    fn handle_dialog(&mut self, ctx: &egui::Context) {
        self.dialog.update(ctx);
        let Some(path) = self.dialog.take_picked() else { return };
        match self.dialog_purpose {
            DialogPurpose::Open => self.open(path),
            DialogPurpose::Folder => {
                self.set_folder(&path);
                self.show_strip = true;
                self.persist();
            }
            DialogPurpose::Export => {
                if let Some(s) = &self.session {
                    let path = with_ext(path, "tif");
                    self.busy = Some("exporting".into());
                    self.heavy.send(Job::Export {
                        session: Arc::clone(s),
                        weights: self.weights(),
                        look: self.current_look(),
                        path,
                        screen_px: SCREEN_LONG_EDGE,
                        orientation: self.export_orientation(),
                    });
                }
            }
            DialogPurpose::SaveLook => {
                let path = with_ext(path, "json");
                let mut look = self.current_look();
                if let Some(stem) = path.file_stem() {
                    look.name = stem.to_string_lossy().to_string();
                }
                match look.validate().and_then(|_| look.save(&path)) {
                    Ok(()) => {
                        self.log(format!("saved look {}", path.display()));
                        self.custom_name = look.name;
                    }
                    Err(e) => self.log(format!("look not saved: {e}")),
                }
            }
            DialogPurpose::LoadLook => match LookFile::load(&path) {
                Ok(l) => {
                    self.custom_points = l.points.clone();
                    self.custom_name = l.name.clone();
                    self.look_choice = LookChoice::Custom;
                    self.log(format!("loaded look {} ({} points)", l.name, l.points.len()));
                }
                Err(e) => self.log(format!("look not loaded: {e}")),
            },
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let idle = self.busy.is_none();
            if ui.add_enabled(idle, egui::Button::new("Open RAW…")).clicked() {
                self.start_dialog(DialogPurpose::Open, None);
            }
            if ui
                .button("Folder…")
                .on_hover_text("Show a folder's RAW files as thumbnails in the strip below")
                .clicked()
            {
                self.start_dialog(DialogPurpose::Folder, None);
            }
            if ui
                .selectable_label(self.show_strip, "Strip")
                .on_hover_text("Show or hide the folder strip (B). ←/→ open the neighbouring file.")
                .clicked()
            {
                self.toggle_strip();
            }
            let has = self.session.is_some();
            let export_hover = self.export_hover_text();
            if ui
                .add_enabled(idle && has, egui::Button::new("Export…"))
                .on_disabled_hover_text(export_hover.clone())
                .on_hover_text(export_hover)
                .clicked()
            {
                self.start_export();
            }
            if ui
                .add_enabled(has, egui::Button::new("⟳ 90°"))
                .on_hover_text("Rotate the picture 90° clockwise (preview and export)")
                .clicked()
            {
                self.rotation = (self.rotation + 1) % 4;
            }
            if ui.button("Reset").on_hover_text("Controls back to defaults; the negative stays").clicked() {
                self.reset();
            }
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

    fn file_column(&mut self, ui: &mut egui::Ui) {
        ui.heading("File");
        ui.add_space(4.0);
        let Some(s) = self.session.clone() else {
            ui.label("Open a RAW file or drop it here.");
            ui.add_space(8.0);
            ui.label(RichText::new("Bayer sensors: Nikon Z f (NEF) and other rawler-supported cameras. Monochrome sensors (DNG without CFA) pass through without separation.").weak());
            return;
        };
        let n = &s.negative;
        let info = &n.info;
        egui::Grid::new("file-info").num_columns(2).spacing([8.0, 3.0]).show(ui, |ui| {
            ui.label("name");
            ui.label(s.path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default())
                .on_hover_text(s.path.display().to_string());
            ui.end_row();
            ui.label("camera");
            ui.label(format!("{} {}", info.camera.make.trim(), info.camera.model.trim()));
            ui.end_row();
            let exposure = info.camera.exposure.summary();
            ui.label("exposure");
            if exposure.is_empty() {
                ui.label(RichText::new("not in file").weak());
            } else {
                ui.label(RichText::new(&exposure).strong());
            }
            ui.end_row();
            if let Some(lens) = &info.camera.exposure.lens {
                ui.label("lens");
                ui.label(lens.trim());
                ui.end_row();
            }
            ui.label("sensor");
            ui.label(format!("{} x {} @ {} bit", n.plane.width, n.plane.height, info.camera.bits));
            ui.end_row();
            ui.label("pattern");
            ui.label(info.phase.clone().map(|p| format!("Bayer {p}")).unwrap_or_else(|| "mono".into()));
            ui.end_row();
            ui.label("orientation");
            ui.label(if self.rotation == 0 {
                format!("{}", n.orientation)
            } else {
                format!("{} + {}° cw", n.orientation, 90 * self.rotation as u16)
            });
            ui.end_row();
            ui.label("balance");
            ui.label(format!("R {:.3}  B {:.3} ({})", info.wb[0], info.wb[2], info.report.wb_source));
            ui.end_row();
            let px = n.plane.data.len() as f64;
            ui.label("saturated");
            ui.label(format!(
                "{:.2} % ({:.2} % dilated)",
                100.0 * info.report.saturated_raw as f64 / px,
                100.0 * info.report.saturated_dilated as f64 / px
            ));
            ui.end_row();
            ui.label("defects");
            ui.label(format!("{} ({:.3} %)", info.report.defects, 100.0 * info.report.defects as f64 / px));
            ui.end_row();
            ui.label("camera file");
            match &s.camera_file_path {
                Some(p) => ui
                    .label(p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default())
                    .on_hover_text(p.display().to_string()),
                None => ui.label(RichText::new("none (defaults)").weak()),
            };
            ui.end_row();
            ui.label("converter");
            ui.label(info.demosaic.as_deref().unwrap_or("Mimizan"));
            ui.end_row();
            if info.demosaic.is_none() {
                ui.label("mask");
                ui.label(format!("{:?}, {} round(s)", info.mask, info.rounds));
                ui.end_row();
                if info.reconstruct {
                    ui.label("reconstruct");
                    ui.label(
                        RichText::new(format!("on: {:.1} % of the picture computed", 100.0 * info.computed))
                            .color(egui::Color32::from_rgb(230, 170, 60)),
                    )
                    .on_hover_text("Mean of the per-pixel share that came from the direction decision; the sidecar mask holds it per pixel (204 = measured ... 252 = computed).");
                    ui.end_row();
                }
            }
            ui.label("timing");
            let step = if info.demosaic.is_some() { "demosaic" } else { "separate" };
            ui.label(format!(
                "decode {} · ingest {} · {step} {} ms",
                info.timing.decode_ms, info.timing.ingest_ms, info.timing.separate_ms
            ));
            ui.end_row();
            ui.label("preview");
            ui.label(format!(
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

        ui.with_layout(egui::Layout::bottom_up(egui::Align::Min), |ui| {
            ui.add_space(4.0);
            egui::ScrollArea::vertical().max_height(160.0).stick_to_bottom(true).show(ui, |ui| {
                for line in self.log.iter().rev().take(12).rev() {
                    ui.label(RichText::new(line).small().weak());
                }
            });
            ui.separator();
        });
    }

    fn histogram_section(&mut self, ui: &mut egui::Ui) {
        ui.heading("Histogram");
        ui.horizontal(|ui| {
            ui.radio_value(&mut self.hist_source, HistSource::View, "with curve (TIFF)").on_hover_text(
                "The full-resolution negative after the look curve, as the TIFF gets it. \
                 black/white: pixels at 0 and 65535 in 16 bit",
            );
            ui.radio_value(&mut self.hist_source, HistSource::Negative, "negative").on_hover_text(
                "The full-resolution linear mix before the curve, drawn on a gamma-2.2 axis. \
                 black/white: pixels at 0 and 65535 in 16 bit",
            );
            ui.checkbox(&mut self.hist_log, "log");
        });
        let h = match self.hist_source {
            HistSource::View => &self.hist_view,
            HistSource::Negative => &self.hist_negative,
        };
        let width = ui.available_width().min(300.0);
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

    /// One row of weight presets: Native, Panchromatic (the camera file's
    /// calibrated weights) and the classic contrast filters derived from it.
    /// All of them are only a weight triple; the sliders below are the free
    /// form, and moving one leaves every preset unselected.
    fn mix_presets(&mut self, ui: &mut egui::Ui) {
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
        ui.horizontal_wrapped(|ui| {
            let native = Weights::default();
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

    fn controls_column(&mut self, ui: &mut egui::Ui) {
        self.histogram_section(ui);
        ui.heading("Mix");
        let wb = self.session.as_ref().map(|s| s.wb()).unwrap_or([1.0; 3]);
        self.mix_presets(ui);
        ui.add_space(2.0);
        let mut r = self.w_r;
        let mut b = self.w_b;
        if ui.add(egui::Slider::new(&mut r, 0.0..=1.0).text("R").fixed_decimals(3)).changed() {
            self.w_r = r;
            self.w_b = self.w_b.min(1.0 - r);
        }
        if ui.add(egui::Slider::new(&mut b, 0.0..=1.0).text("B").fixed_decimals(3)).changed() {
            self.w_b = b;
            self.w_r = self.w_r.min(1.0 - b);
        }
        let w = self.weights();
        ui.label(format!("G = {:.3}   (R + G + B = 1, balanced channels)", w.g));
        let raw = w.balanced_to_raw(wb);
        ui.label(RichText::new(format!("raw channels: R {:.3} G {:.3} B {:.3}", raw.r, raw.g, raw.b)).weak())
            .on_hover_text(
                "The same mix on the sensor channels before white balance: the illuminant-independent form",
            );

        ui.add_space(10.0);
        ui.separator();
        ui.heading("Look");
        for i in 0..self.looks.len() {
            let name = capitalise(&self.looks[i].name.replace('_', "-"));
            let label = if self.looks[i].encoding == LookEncoding::Gamma22 {
                name
            } else {
                format!("{name} (linear)")
            };
            let hover = match self.looks[i].name.as_str() {
                "neutral" => "Plain gamma 2.2 encoding, no tone curve".to_string(),
                "reference" => {
                    "The tone rendering of the monochrome reference camera, fitted from its own JPEG"
                        .to_string()
                }
                _ => self.looks[i]
                    .fitted_against
                    .clone()
                    .map(|f| format!("fitted against {f}"))
                    .unwrap_or_default(),
            };
            ui.radio_value(&mut self.look_choice, LookChoice::Preset(i), label).on_hover_text(hover);
        }
        ui.radio_value(
            &mut self.look_choice,
            LookChoice::Custom,
            format!("Custom curve ({})", self.custom_name),
        );
        if self.look_choice == LookChoice::Custom {
            ui.add_space(4.0);
            curve_editor(ui, &mut self.custom_points, 236.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("straight").clicked() {
                    self.custom_points = vec![[0.0, 0.0], [1.0, 1.0]];
                }
                if ui.button("S-curve").clicked() {
                    self.custom_points = vec![[0.0, 0.0], [0.25, 0.20], [0.5, 0.5], [0.75, 0.80], [1.0, 1.0]];
                }
                if ui.button("Load…").clicked() {
                    self.start_dialog(DialogPurpose::LoadLook, None);
                }
                if ui.button("Save…").clicked() {
                    let dir = self.look_dir.clone();
                    self.dialog.config_mut().initial_directory = dir;
                    self.start_dialog(DialogPurpose::SaveLook, Some(format!("{}.json", self.custom_name)));
                }
            });
            ui.label(
                RichText::new("Points apply after gamma 2.2 encoding (x = encoded input, y = output).")
                    .weak(),
            );
        }

        ui.add_space(10.0);
        ui.separator();
        ui.heading("View");
        ui.checkbox(&mut self.show_mask, "show mask (white = chroma treated as texture)");
        ui.horizontal(|ui| {
            if ui.button("fit").clicked() {
                self.zoom = 1.0;
                self.pan = Vec2::ZERO;
            }
            if ui.button("1:1").on_hover_text("One negative pixel per screen pixel").clicked() {
                self.zoom_to_1to1 = true;
            }
            ui.label(RichText::new("scroll to zoom, drag to pan, double-click toggles").weak());
        });
        ui.label(
            RichText::new("The preview is never sharpened: it shows the negative as the TIFF gets it.")
                .weak(),
        );
    }

    /// What one export writes, for the toolbar button.
    fn export_hover_text(&self) -> String {
        if self.session.is_none() {
            return "Open a RAW file first.".into();
        }
        let look = match self.look_choice {
            LookChoice::Preset(_) => capitalise(&self.current_look().name.replace('_', "-")),
            LookChoice::Custom => format!("custom curve \"{}\"", self.custom_name),
        };
        let (uw, uh) = self.export_size();
        let (jw, jh) = mimizan_core::resize::fit_dims(uw, uh, SCREEN_LONG_EDGE, SCREEN_LONG_EDGE);
        let rot = if self.rotation != 0 {
            format!(", rotated {}°", 90 * self.rotation as u16)
        } else {
            String::new()
        };
        format!(
            "Writes two files with the {look} look{rot}:\n\
             <name>.tif  {uw} x {uh} px, 16 bit, gamma 2.2 grey ICC, unsharpened — for further work\n\
             <name>_{SCREEN_LONG_EDGE}px.jpg  {jw} x {jh} px, quality 100 — screen compensation for a 1:1 view \
             (the known resampling and display losses inverted, gain capped at 2x; nothing to adjust)"
        )
    }

    fn preview_panel(&mut self, ui: &mut egui::Ui) {
        let avail = ui.available_rect_before_wrap();
        let response = ui.allocate_rect(avail, Sense::click_and_drag());
        let painter = ui.painter_at(avail);
        painter.rect_filled(avail, 0.0, Color32::from_gray(28));
        let Some(tex) = &self.texture else {
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
        let fit = (avail.width() / tw as f32).min(avail.height() / th as f32);
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
        // Keep the image covering the viewport when zoomed, centred otherwise.
        let max_pan = ((size - avail.size()) * 0.5).max(Vec2::ZERO);
        self.pan = self.pan.clamp(-max_pan, max_pan);
        let center = avail.center() + self.pan;
        let rect = Rect::from_center_size(center, size);
        painter.image(tex.id(), rect, Rect::from_min_max(Pos2::ZERO, Pos2::new(1.0, 1.0)), Color32::WHITE);

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

fn with_ext(p: PathBuf, ext: &str) -> PathBuf {
    if p.extension().is_some_and(|e| e.eq_ignore_ascii_case(ext) || e.eq_ignore_ascii_case("tiff")) {
        p
    } else {
        p.with_extension(ext)
    }
}

/// Strip: folder bar, tiles, horizontal scrollbar.
const STRIP_HEIGHT: f32 = 196.0;
const TILE_W: f32 = 150.0;
const TILE_IMG_H: f32 = 112.0;
const TILE_H: f32 = TILE_IMG_H + 20.0;

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
                    ui.label(RichText::new("No folder. Folder… above, or drop a folder here.").weak());
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
        let Some(folder) = &mut self.folder else { return };
        if folder.entries.is_empty() {
            ui.label(RichText::new("No RAW files in this folder.").weak());
            return;
        }
        let idle = self.busy.is_none();
        let current = self.current_path.clone();
        let generation = folder.generation;
        let scroll_to = std::mem::take(&mut self.scroll_to_selected);
        let mut clicked: Option<PathBuf> = None;
        // (distance from the viewport centre, index) of tiles that still
        // need a thumbnail; visible ones first, then two screens around.
        let mut wanted: Vec<(f32, usize)> = Vec::new();
        let ctx = ui.ctx().clone();
        egui::ScrollArea::horizontal().id_salt("strip-scroll").show(ui, |ui| {
            let clip = ui.clip_rect();
            let centre = clip.center().x;
            ui.horizontal(|ui| {
                ui.spacing_mut().item_spacing.x = 6.0;
                for (i, e) in folder.entries.iter_mut().enumerate() {
                    let (rect, resp) = ui.allocate_exact_size(Vec2::new(TILE_W, TILE_H), Sense::click());
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
                            let img = ColorImage::from_gray([t.width, t.height], &t.gray);
                            e.texture = Some(ctx.load_texture(
                                format!("thumb-{generation}-{i}"),
                                img,
                                TextureOptions::LINEAR,
                            ));
                        }
                    }
                    let painter = ui.painter();
                    let vis = ui.visuals();
                    let bg = if is_current {
                        vis.selection.bg_fill.gamma_multiply(0.35)
                    } else if resp.hovered() {
                        vis.widgets.hovered.bg_fill
                    } else {
                        vis.widgets.noninteractive.bg_fill
                    };
                    painter.rect_filled(rect, 4.0, bg);
                    let img_rect = Rect::from_min_size(rect.min, Vec2::new(TILE_W, TILE_IMG_H)).shrink(3.0);
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
                    if is_current {
                        painter.rect_stroke(
                            rect,
                            4.0,
                            egui::Stroke::new(2.0, vis.selection.stroke.color),
                            egui::StrokeKind::Inside,
                        );
                    }
                    let label_pos = Pos2::new(rect.center().x, rect.max.y - 10.0);
                    let name = truncate_middle(&e.name, 20);
                    painter.text(
                        label_pos,
                        egui::Align2::CENTER_CENTER,
                        name,
                        egui::FontId::proportional(11.5),
                        if is_current { vis.strong_text_color() } else { vis.text_color() },
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
    }
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
            egui::Panel::bottom("strip")
                .resizable(false)
                .exact_size(STRIP_HEIGHT)
                .show(ui, |ui| self.strip_panel(ui));
        }
        egui::Panel::left("file").resizable(true).default_size(310.0).min_size(240.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.file_column(ui));
        });
        egui::Panel::right("controls").resizable(true).default_size(310.0).min_size(260.0).show(ui, |ui| {
            egui::ScrollArea::vertical().show(ui, |ui| self.controls_column(ui));
        });
        egui::CentralPanel::default().frame(egui::Frame::NONE).show(ui, |ui| self.preview_panel(ui));

        self.request_preview_if_needed();
        self.flush_converter_redevelop();
        self.strip_keys(&ctx);
    }
}
