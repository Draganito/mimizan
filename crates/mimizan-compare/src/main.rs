//! A loupe for one RAW file.
//!
//! The left pane is one Mimizan algorithm. *Separation* is the Bayer
//! frequency separation. *Least squares* is the Bayer least-squares
//! demosaic. *X-Trans* is that demosaic on an X-Trans file. The right pane
//! is a rival that reads the same mosaic: RCD, LMMSE, DCB, AMaZE and DHT
//! on Bayer, and Markesteijn in four, three, two and one pass on X-Trans.
//! Scroll zooms both panes together, a drag pans both, a click on the
//! right pane steps to the next demosaicer.
//!
//! ```text
//! mimizan-compare photo.NEF
//! ```
//!
//! With no path, Open picks the file. A path on the command line still
//! develops at once, and a file dropped on the window does too.
//!
//! RawTherapee and dcraw are the names on the path, unless *RawTherapee…*
//! or *dcraw…* has pointed at the program. The choice is kept with the
//! darkroom's settings, in `mimizan/compare.tools`.
//!
//! The developed TIFFs land in `photo.mimizan-compare/` beside the RAW and
//! are kept. A second open reads them, as long as the RAW is the file they
//! were developed from. *Develop again* writes them over.
//!
//! The Bayer plate is developed by the `mimizan` command: the one beside
//! this program, the one inside `Mimizan Lab.app` beside it, or the name
//! on the path. `MIMIZAN` names another, `MIMIZAN_CAMERAS` another folder
//! of camera files.
//!
//! Both panes are the demosaic only, then one camera matrix and the sRGB
//! curve. Nothing else: no paper, no lamps, no sharpening, no noise
//! reduction, no highlight recovery, no automatic brightness.
//!

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![forbid(unsafe_code)]

use anyhow::{bail, Context, Result};
use egui_file_dialog::{DialogState, FileDialog, OpeningMode};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;

/// The five, in the order the lens measurement ranked them.
const RIVALS: &[Rival] = &[
    Rival { key: "rcd", label: "RCD", kind: Kind::Bayer("rcd") },
    Rival { key: "lmmse", label: "LMMSE", kind: Kind::Bayer("lmmse") },
    Rival { key: "dcb", label: "DCB", kind: Kind::Bayer("dcb") },
    Rival { key: "amaze", label: "AMaZE", kind: Kind::Bayer("amaze") },
    Rival { key: "dht", label: "DHT", kind: Kind::Dht },
];

/// Markesteijn, the demosaic an X-Trans file is measured against.
const XTRANS_RIVALS: &[Rival] = &[
    Rival { key: "x4", label: "4-pass", kind: Kind::Xtrans("4-pass") },
    Rival { key: "x3", label: "3-pass", kind: Kind::Xtrans("3-pass (best)") },
    Rival { key: "x2", label: "2-pass", kind: Kind::Xtrans("2-pass") },
    Rival { key: "x1", label: "1-pass", kind: Kind::Xtrans("1-pass (medium)") },
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Algo {
    Separation,
    LeastSquares,
    Xtrans,
}

const ALGOS: [Algo; 3] = [Algo::Separation, Algo::LeastSquares, Algo::Xtrans];

impl Algo {
    fn label(self) -> &'static str {
        match self {
            Algo::Separation => "Separation",
            Algo::LeastSquares => "Least squares",
            Algo::Xtrans => "X-Trans",
        }
    }

    fn key(self) -> &'static str {
        match self {
            Algo::Separation => "mimizan",
            Algo::LeastSquares => "bayer-ls",
            Algo::Xtrans => "xtrans",
        }
    }

    fn fits(self, sensor: Sensor) -> bool {
        matches!(
            (self, sensor),
            (Algo::Xtrans, Sensor::Xtrans) | (Algo::Separation | Algo::LeastSquares, Sensor::Bayer)
        )
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Sensor {
    Bayer,
    Xtrans,
}

impl Sensor {
    fn home(self) -> Algo {
        match self {
            Sensor::Bayer => Algo::Separation,
            Sensor::Xtrans => Algo::Xtrans,
        }
    }

    fn rivals(self) -> &'static [Rival] {
        match self {
            Sensor::Bayer => RIVALS,
            Sensor::Xtrans => XTRANS_RIVALS,
        }
    }
}

struct Rival {
    key: &'static str,
    label: &'static str,
    kind: Kind,
}

enum Kind {
    /// A RawTherapee Bayer demosaic. The name is the profile's method.
    Bayer(&'static str),
    /// A RawTherapee X-Trans demosaic. The name is the profile's method.
    Xtrans(&'static str),
    Dht,
}

/// Extensions the darkroom lists as a RAW file. The dialog matches them
/// without caring about case; *All files* stays, for a name it does not know.
const RAW_EXTENSIONS: &[&str] = &[
    "nef", "nrw", "dng", "cr2", "cr3", "crw", "arw", "srf", "sr2", "raf", "rw2", "pef", "orf", "erf", "mef",
    "mos", "iiq", "3fr", "fff", "kdc", "dcr", "dcs", "mrw", "x3f", "tfr", "ari", "srw", "raw",
];

#[derive(Clone)]
struct Picture {
    width: usize,
    height: usize,
    rgba: Vec<u8>,
}

enum Note {
    Status(String),
    /// The file's mosaic, sent before any picture.
    Sensor(Sensor),
    Ready {
        slot: usize,
        picture: Picture,
    },
    Missed {
        slot: usize,
        error: String,
    },
    Finished,
}

/// `slot` 0 is Mimizan. The rivals follow in [`RIVALS`] order.
struct App {
    path: String,
    algo: Algo,
    sensor: Option<Sensor>,
    dialog: FileDialog,
    preview: Preview,
    view: View,
    fitted: bool,
    which: usize,
    pictures: Vec<Option<Picture>>,
    textures: Vec<Option<Tiled>>,
    errors: Vec<Option<String>>,
    status: String,
    working: bool,
    rx: Option<Receiver<Note>>,
    /// The two programs the rivals are developed with, chosen by hand.
    tools: Tools,
    tool_dialog: FileDialog,
    asking: Option<WhichTool>,
}

struct View {
    /// Screen points per pixel of the Mimizan picture.
    mag: f32,
    pan: egui::Vec2,
}

fn main() -> eframe::Result {
    let initial =
        std::env::args_os().nth(1).map(|p| PathBuf::from(p).display().to_string()).unwrap_or_default();
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([1440.0, 860.0])
        .with_min_inner_size([800.0, 500.0])
        .with_title("Mimizan compare");
    eframe::run_native(
        "Mimizan compare",
        eframe::NativeOptions { viewport, ..Default::default() },
        Box::new(move |_cc| Ok(Box::new(App::new(initial)))),
    )
}

impl App {
    fn new(path: String) -> Self {
        let n = 1 + RIVALS.len();
        let mut app = Self {
            path,
            algo: Algo::Separation,
            sensor: None,
            dialog: file_dialog(),
            preview: Preview::default(),
            view: View { mag: 1.0, pan: egui::Vec2::ZERO },
            fitted: false,
            which: 0,
            pictures: (0..n).map(|_| None).collect(),
            textures: (0..n).map(|_| None).collect(),
            errors: (0..n).map(|_| None).collect(),
            status: "Open a RAW file, or drop one here.".into(),
            working: false,
            rx: None,
            tools: load_tools(),
            tool_dialog: tool_dialog(),
            asking: None,
        };
        if !app.path.is_empty() {
            app.develop(false);
        }
        app
    }

    fn rivals(&self) -> &'static [Rival] {
        self.sensor.map(Sensor::rivals).unwrap_or(RIVALS)
    }

    /// The folder the dialog opens in: the file on screen, or home.
    fn open_beside(&self) -> PathBuf {
        dialog_directory(self.path.trim())
    }

    fn open_dialog(&mut self) {
        if self.working {
            return;
        }
        self.dialog.config_mut().initial_directory = self.open_beside();
        self.dialog.pick_file();
    }

    fn take_file(&mut self, path: PathBuf) {
        self.path = path.display().to_string();
        self.develop(false);
    }

    fn develop(&mut self, again: bool) {
        if self.working {
            return;
        }
        let raw = PathBuf::from(self.path.trim());
        if !raw.is_file() {
            self.status = format!("No such file: {}", raw.display());
            return;
        }
        self.working = true;
        self.fitted = false;
        self.pictures.fill(None);
        self.textures.iter_mut().for_each(|t| *t = None);
        self.errors.fill(None);
        self.status = "Developing…".into();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let algo = self.algo;
        let tools = self.tools.clone();
        thread::spawn(move || develop_all(&raw, again, algo, false, tools, tx));
    }

    /// The other Mimizan algorithm, the rivals staying as they are.
    fn develop_left(&mut self) {
        if self.working {
            return;
        }
        let raw = PathBuf::from(self.path.trim());
        if !raw.is_file() {
            self.status = format!("No such file: {}", raw.display());
            return;
        }
        self.working = true;
        self.fitted = false;
        self.pictures[0] = None;
        self.textures[0] = None;
        self.errors[0] = None;
        self.status = format!("Developing {}…", self.algo.label());
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let algo = self.algo;
        let tools = self.tools.clone();
        thread::spawn(move || develop_all(&raw, false, algo, true, tools, tx));
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        let mut finished = false;
        while let Ok(note) = rx.try_recv() {
            match note {
                Note::Status(s) => self.status = s,
                Note::Sensor(sensor) => {
                    self.sensor = Some(sensor);
                    if !self.algo.fits(sensor) {
                        self.algo = sensor.home();
                    }
                    let n = 1 + sensor.rivals().len();
                    self.pictures.resize_with(n, || None);
                    self.textures.resize_with(n, || None);
                    self.errors.resize_with(n, || None);
                    if self.which >= sensor.rivals().len() {
                        self.which = 0;
                    }
                }
                Note::Ready { slot, picture } => {
                    if slot >= self.pictures.len() {
                        continue;
                    }
                    let name = if slot == 0 {
                        self.algo.key().to_string()
                    } else {
                        self.rivals()
                            .get(slot - 1)
                            .map(|r| r.key.to_string())
                            .unwrap_or_else(|| format!("slot{slot}"))
                    };
                    self.textures[slot] = Some(Tiled::upload(ctx, &name, &picture));
                    self.pictures[slot] = Some(picture);
                    self.errors[slot] = None;
                }
                Note::Missed { slot, error } => {
                    if slot < self.errors.len() {
                        self.errors[slot] = Some(error);
                    }
                }
                Note::Finished => finished = true,
            }
        }
        if finished {
            self.working = false;
            self.rx = None;
            if self.status.starts_with("Developing") || self.status.ends_with('…') {
                self.status = "Ready. Scroll zooms, drag pans, click the right pane for the next.".into();
            }
        } else if self.working {
            ctx.request_repaint();
        }
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll(&ctx);
        if let Some(file) = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).next()) {
            if !self.working {
                self.take_file(file);
            }
        }
        if self.asking.is_some() {
            self.tool_dialog.update(&ctx);
        } else {
            let dialog = &mut self.dialog;
            let preview = &mut self.preview;
            dialog.set_right_panel_width(300.0);
            dialog.update_with_right_panel_ui(&ctx, &mut |ui, dia| {
                preview.panel(ui, &ctx, dia);
            });
        }
        if let Some(path) = self.tool_dialog.take_picked() {
            if let Some(which) = self.asking.take() {
                self.tools.set(which, path.clone());
                self.status = match save_tools(&self.tools) {
                    Ok(()) => format!("Using {}", path.display()),
                    Err(e) => format!("Using {}. The choice was not kept: {e}", path.display()),
                };
            }
        } else if self.asking.is_some() && !matches!(self.tool_dialog.state(), DialogState::Open) {
            self.asking = None;
        }
        if self.asking.is_none() {
            if let Some(path) = self.dialog.take_picked() {
                self.take_file(path);
            }
        }
        let picking = matches!(self.dialog.state(), DialogState::Open)
            || matches!(self.tool_dialog.state(), DialogState::Open);
        let mut open = false;
        ctx.input(|i| {
            if self.working || picking {
                return;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::O) {
                open = true;
            }
            let count = self.rivals().len().max(1);
            if i.key_pressed(egui::Key::ArrowRight) {
                self.which = (self.which + 1) % count;
            }
            if i.key_pressed(egui::Key::ArrowLeft) {
                self.which = (self.which + count - 1) % count;
            }
            for (n, key) in
                [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3, egui::Key::Num4, egui::Key::Num5]
                    .into_iter()
                    .enumerate()
            {
                if i.key_pressed(key) && n < self.rivals().len() {
                    self.which = n;
                }
            }
        });
        if open {
            self.open_dialog();
        }
        let title = window_title(self.path.trim());
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(title));

        egui::Panel::top("bar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(!self.working, egui::Button::new("Open…"))
                    .on_hover_text("Pick a RAW file. Ctrl+O, or ⌘O. A file dropped on the window opens too.")
                    .clicked()
                {
                    self.open_dialog();
                }
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.path).desired_width(420.0).hint_text("RAW file"),
                );
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.working {
                    self.develop(false);
                }
                if ui.add_enabled(!self.working, egui::Button::new("Develop")).clicked() {
                    self.develop(false);
                }
                if ui.add_enabled(!self.working, egui::Button::new("Develop again")).clicked() {
                    self.develop(true);
                }
                ui.separator();
                for algo in ALGOS {
                    let on = self.algo == algo;
                    let response =
                        ui.add_enabled(!self.working, egui::Button::new(algo.label()).selected(on));
                    if response.clicked() && self.algo != algo {
                        match self.sensor {
                            Some(sensor) if !algo.fits(sensor) => {
                                self.errors[0] = Some(match sensor {
                                    Sensor::Bayer => {
                                        "This file is Bayer. Separation and Least squares read it.".into()
                                    }
                                    Sensor::Xtrans => "This file is X-Trans. X-Trans reads it.".into(),
                                });
                            }
                            Some(_) => {
                                self.errors[0] = None;
                                self.algo = algo;
                                self.develop_left();
                            }
                            None => self.algo = algo,
                        }
                    }
                }
                ui.separator();
                for (n, rival) in self.rivals().iter().enumerate() {
                    let on = self.which == n;
                    if ui.add_enabled(!self.working, egui::Button::new(rival.label).selected(on)).clicked() {
                        self.which = n;
                    }
                }
                ui.separator();
                self.tool_button(ui, WhichTool::RawTherapee, "RawTherapee…");
                self.tool_button(ui, WhichTool::Dcraw, "dcraw…");
                ui.separator();
                if ui.button("Fit").clicked() {
                    self.fitted = false;
                }
                if ui.button("1:1").clicked() {
                    self.view.mag = 1.0 / dense;
                    self.view.pan = egui::Vec2::ZERO;
                    self.fitted = true;
                }
                if let Some(pic) = &self.textures[0] {
                    let pct = self.view.mag * dense * 100.0;
                    ui.label(format!("{pct:.0}%  {}×{}", pic.width, pic.height));
                }
            });
            ui.label(&self.status);
            if let Some(err) = &self.errors[0] {
                ui.colored_label(egui::Color32::from_rgb(190, 70, 60), err);
            }
            if let Some(err) = self.errors.get(self.which + 1).and_then(|e| e.as_ref()) {
                ui.colored_label(egui::Color32::from_rgb(190, 70, 60), err);
            }
        });

        egui::CentralPanel::default().show(ui, |ui| {
            let rect = ui.available_rect_before_wrap();
            let gap = 6.0;
            let width = ((rect.width() - gap) / 2.0).max(1.0);
            let left = egui::Rect::from_min_size(rect.min, egui::vec2(width, rect.height()));
            let right = egui::Rect::from_min_size(
                egui::pos2(left.right() + gap, rect.min.y),
                egui::vec2(width, rect.height()),
            );
            if !self.fitted {
                if let Some(frame) = self.frame() {
                    self.view.mag = fit_mag(left.size(), frame);
                    self.view.pan = egui::Vec2::ZERO;
                    self.fitted = true;
                }
            }
            let left_id = ui.id().with("left");
            let right_id = ui.id().with("right");
            let left_hit = ui.interact(left, left_id, egui::Sense::click_and_drag());
            let right_hit = ui.interact(right, right_id, egui::Sense::click_and_drag());
            self.gesture(ui, left, &left_hit);
            self.gesture(ui, right, &right_hit);
            let n = self.rivals().len().max(1);
        // A digit or an arrow typed into the path field is text.
        let typing = ctx.egui_wants_keyboard_input();
            if right_hit.clicked() && !right_hit.double_clicked() {
                self.which = (self.which + 1) % n;
            }
            paint(ui, left, self.algo.label(), self.textures[0].as_ref(), &self.view);
            let label = self.rivals().get(self.which).map(|r| r.label).unwrap_or("");
            let right_tex = self.textures.get(self.which + 1).and_then(|t| t.as_ref());
            paint(ui, right, label, right_tex, &self.view);
            if typing {
                return;
            }
        });
    }
}

impl App {
    fn tool_button(&mut self, ui: &mut egui::Ui, which: WhichTool, label: &str) {
        let chosen = self.tools.get(which).to_path_buf();
        let tip = if chosen.as_os_str().is_empty() {
            format!("Choose {}. The name on the path is used until then.", which.name())
        } else {
            chosen.display().to_string()
        };
        if ui.add_enabled(!self.working, egui::Button::new(label)).on_hover_text(tip).clicked() {
            self.open_tool(which);
        }
    }

    fn open_tool(&mut self, which: WhichTool) {
        if self.working {
            return;
        }
        self.tool_dialog.config_mut().initial_directory = tool_directory(self.tools.get(which));
        self.asking = Some(which);
        self.tool_dialog.pick_file();
    }

    fn frame(&self) -> Option<egui::Vec2> {
        let pic = self.pictures[0]
            .as_ref()
            .or_else(|| self.pictures.get(self.which + 1).and_then(|p| p.as_ref()))?;
        Some(egui::vec2(pic.width as f32, pic.height as f32))
    }

    fn gesture(&mut self, ui: &egui::Ui, pane: egui::Rect, hit: &egui::Response) {
        let Some(img) = self.frame() else { return };
        if hit.double_clicked() {
            self.view.mag = fit_mag(pane.size(), img);
            self.view.pan = egui::Vec2::ZERO;
            self.fitted = true;
            return;
        }
        if hit.dragged() {
            self.view.pan += hit.drag_delta();
        }
        if hit.hovered() {
            let scroll = ui.input(|i| i.smooth_scroll_delta.y);
            if scroll != 0.0 {
                let old = img * self.view.mag;
                let factor = (scroll * 0.0015).exp();
                let mag = (self.view.mag * factor).clamp(0.02, 32.0);
                let new = img * mag;
                if let Some(pointer) = hit.hover_pos() {
                    self.view.pan = pan_keeping(pane, old, new, self.view.pan, pointer);
                }
                self.view.mag = mag;
                self.fitted = true;
            }
        }
    }
}

/// A picture on the graphics card: one texture, or a grid of them where
/// the picture is larger than the card's largest. A texture over that
/// limit is not refused by the renderer, it ends the program, and the
/// limit is 8192 pixels with the Windows build: under a 45-megapixel
/// frame.
struct Tiled {
    width: usize,
    height: usize,
    /// Each tile's place in the picture, `[x, y, w, h]`, and its texture.
    tiles: Vec<([usize; 4], egui::TextureHandle)>,
}

impl Tiled {
    fn upload(ctx: &egui::Context, name: &str, picture: &Picture) -> Self {
        let options = egui::TextureOptions {
            magnification: egui::TextureFilter::Nearest,
            minification: egui::TextureFilter::Linear,
            ..Default::default()
        };
        let max_side = ctx.input(|i| i.max_texture_side);
        let tiles = tile_grid(picture.width, picture.height, max_side)
            .into_iter()
            .map(|[x, y, w, h]| {
                let image = if (w, h) == (picture.width, picture.height) {
                    egui::ColorImage::from_rgba_unmultiplied([w, h], &picture.rgba)
                } else {
                    let mut rgba = Vec::with_capacity(4 * w * h);
                    for row in y..y + h {
                        let at = 4 * (row * picture.width + x);
                        rgba.extend_from_slice(&picture.rgba[at..at + 4 * w]);
                    }
                    egui::ColorImage::from_rgba_unmultiplied([w, h], &rgba)
                };
                ([x, y, w, h], ctx.load_texture(format!("{name}-{x}-{y}"), image, options))
            })
            .collect();
        Self { width: picture.width, height: picture.height, tiles }
                // One pixel of the picture on one pixel of the screen: a
                // point is two or three of those on a dense display.
                let dense = ctx.pixels_per_point();
    }
}

/// The tiles, `[x, y, w, h]`, a `width` × `height` picture is cut into so
/// that none has a side over `max_side`: one tile when it fits.
fn tile_grid(width: usize, height: usize, max_side: usize) -> Vec<[usize; 4]> {
    let max_side = max_side.max(1);
    let mut tiles = Vec::new();
    for y in (0..height).step_by(max_side) {
        for x in (0..width).step_by(max_side) {
            tiles.push([x, y, max_side.min(width - x), max_side.min(height - y)]);
        }
    }
    tiles
}

fn paint(ui: &egui::Ui, pane: egui::Rect, title: &str, texture: Option<&Tiled>, view: &View) {
    let painter = ui.painter_at(pane);
    painter.rect_filled(pane, 0.0, egui::Color32::from_rgb(18, 18, 18));
    if let Some(picture) = texture {
        let shown = egui::vec2(picture.width as f32, picture.height as f32) * view.mag;
        let corner = origin(pane, shown, view.pan);
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        // Neighbours share the edge they are computed to, so no gap opens.
        let at = |x: usize, y: usize| corner + egui::vec2(x as f32, y as f32) * view.mag;
        for ([x, y, w, h], texture) in &picture.tiles {
            let dest = egui::Rect::from_min_max(at(*x, *y), at(x + w, y + h));
            if dest.intersects(pane) {
                painter.image(texture.id(), dest, uv, egui::Color32::WHITE);
            }
        }
    }
    let label = egui::Rect::from_min_size(pane.min, egui::vec2(pane.width(), 22.0));
    painter.rect_filled(label, 0.0, egui::Color32::from_black_alpha(160));
    painter.text(
        label.left_center() + egui::vec2(8.0, 0.0),
        egui::Align2::LEFT_CENTER,
        title,
        egui::FontId::proportional(14.0),
        egui::Color32::WHITE,
    );
}

/// The finished pictures were made by this view. An older folder is
/// developed again, because those files went through the paper.
const VIEW: &str = "srgb-matrix-1";

fn develop_all(
    raw: &Path,
    again: bool,
    mut algo: Algo,
    only_left: bool,
    tools: Tools,
    tx: mpsc::Sender<Note>,
) {
    let work = work_dir(raw);
    if let Err(e) = std::fs::create_dir_all(&work) {
        let _ = tx.send(Note::Missed { slot: 0, error: e.to_string() });
        let _ = tx.send(Note::Finished);
        return;
    }
    let sensor = match sensor_of(raw) {
        Ok(sensor) => sensor,
        Err(e) => {
            let _ = tx.send(Note::Missed { slot: 0, error: e.to_string() });
            let _ = tx.send(Note::Finished);
            return;
        }
    };
    let _ = tx.send(Note::Sensor(sensor));
    let orientation = camera_orientation(raw);
    if !algo.fits(sensor) {
        algo = sensor.home();
    }
    let rivals: &'static [Rival] = sensor.rivals();
    let fresh = !again && same_view(&work);
    let shared = Arc::new(Shared { cv: Condvar::new(), meta: Mutex::new(None) });
    let failed = Arc::new(std::sync::atomic::AtomicBool::new(false));

    thread::scope(|scope| {
        let shared_m = Arc::clone(&shared);
        let failed_m = Arc::clone(&failed);
        let tx_m = tx.clone();
        let work_m = work.clone();
        let raw_m = raw.to_path_buf();
        scope.spawn(move || {
            let out = work_m.join(format!("{}.tif", algo.key()));
            let meta = match algo {
                Algo::Separation => {
                    if fresh && out.is_file() {
                        let _ = tx_m.send(Note::Status("Reading the Mimizan separation…".into()));
                        read_plate(&work_m.join("negative.tif"))
                    } else {
                        let _ = tx_m.send(Note::Status("Developing the Mimizan separation…".into()));
                        show_mimizan(&raw_m, &work_m, &out, again)
                    }
                }
                Algo::LeastSquares => {
                    let _ = tx_m.send(Note::Status("Reading the camera matrix…".into()));
                    ensure_plate(&raw_m, &work_m, again)
                }
                Algo::Xtrans => {
                    let _ = tx_m.send(Note::Status("Reading the camera matrix…".into()));
                    xtrans_meta(&raw_m)
                }
            };
            publish(&shared_m, &meta);
            let shown = meta.and_then(|meta| {
                if algo == Algo::Separation || (fresh && out.is_file()) {
                    return Ok(meta);
                }
                let made = match algo {
                    Algo::LeastSquares => develop_least_squares(&raw_m, &meta, &out, &tx_m),
                    Algo::Xtrans => develop_xtrans(&raw_m, &meta, &out, &tx_m),
                    Algo::Separation => Ok(()),
                };
                made.map(|()| meta)
            });
            match shown {
                Ok(_) => send_picture(&tx_m, 0, &out, true, orientation),
                Err(e) => {
                    failed_m.store(true, std::sync::atomic::Ordering::Relaxed);
                    let _ = tx_m.send(Note::Missed { slot: 0, error: e.to_string() });
                }
            }
        });
        if only_left {
            return;
        }
        // Two at a time. The rivals start at once and wait for the matrix
        // only when they apply it.
        let (jobs, job_rx) = mpsc::channel::<usize>();
        for n in 0..rivals.len() {
            let _ = jobs.send(n);
        }
        drop(jobs);
        let job_rx = Arc::new(Mutex::new(job_rx));
        for _ in 0..2 {
            let job_rx = Arc::clone(&job_rx);
            let shared_r = Arc::clone(&shared);
            let failed_r = Arc::clone(&failed);
            let tx_r = tx.clone();
            let work = work.clone();
            let raw = raw.to_path_buf();
            let tools = tools.clone();
            scope.spawn(move || loop {
                let n = {
                    let Ok(guard) = job_rx.lock() else { break };
                    let Ok(n) = guard.recv() else { break };
                    n
                };
                let rival = &rivals[n];
                let slot = n + 1;
                let out = work.join(format!("{}.tif", rival.key));
                if fresh && out.is_file() {
                    let _ = tx_r.send(Note::Status(format!("Reading {}…", rival.label)));
                } else {
                    let _ = tx_r.send(Note::Status(format!("Developing {}…", rival.label)));
                    let linear = work.join(format!("{}.linear.tif", rival.key));
                    let run = match rival.kind {
                        Kind::Dht => develop_dht(&raw, &linear, tools.dcraw()),
                        Kind::Bayer(_) | Kind::Xtrans(_) => {
                            develop_rawtherapee(&raw, &work, rival, &linear, tools.rawtherapee())
                        }
                    };
                    let encoded = !matches!(rival.kind, Kind::Dht);
                    let run = run.and_then(|_| {
                        let meta = wait_plate(&shared_r)?;
                        finish_demosaic(&linear, &meta, &out, encoded)
                    });
                    let _ = std::fs::remove_file(&linear);
                    if let Err(e) = run {
                        failed_r.store(true, std::sync::atomic::Ordering::Relaxed);
                        let _ = tx_r.send(Note::Missed { slot, error: format!("{}: {e}", rival.label) });
                        continue;
                    }
                }
                send_picture(&tx_r, slot, &out, false, orientation);
            });
        }
    });
    if !only_left && !failed.load(std::sync::atomic::Ordering::Relaxed) {
        let _ = std::fs::write(work.join("view.txt"), VIEW);
    }
    let _ = tx.send(Note::Finished);
}

fn same_view(work: &Path) -> bool {
    std::fs::read_to_string(work.join("view.txt")).ok().as_deref() == Some(VIEW)
}

struct Shared {
    cv: Condvar,
    meta: Mutex<Option<Result<PlateMeta, String>>>,
}

fn publish(shared: &Shared, meta: &Result<PlateMeta>) {
    {
        let mut slot = shared.meta.lock().expect("plate lock");
        *slot = Some(match meta {
            Ok(meta) => Ok(meta.clone()),
            Err(e) => Err(e.to_string()),
        });
    }
    shared.cv.notify_all();
}

fn wait_plate(shared: &Shared) -> Result<PlateMeta> {
    let mut slot = shared.meta.lock().expect("plate lock");
    while slot.is_none() {
        slot = shared.cv.wait(slot).expect("plate lock");
    }
    match slot.as_ref().expect("plate was set") {
        Ok(meta) => Ok(meta.clone()),
        Err(e) => bail!("{e}"),
    }
}

fn send_picture(tx: &mpsc::Sender<Note>, slot: usize, path: &Path, ours: bool, orientation: u8) {
    match load_picture(path) {
        Ok(mut picture) => {
            // The rivals come back already turned, the way RawTherapee and
            // dcraw show the file. Ours is the sensor as it was read.
            if ours {
                let (rgba, width, height) = turn(&picture.rgba, picture.width, picture.height, orientation);
                picture = Picture { width, height, rgba };
            }
            let _ = tx.send(Note::Ready { slot, picture });
        }
        Err(e) => {
            let _ = tx.send(Note::Missed { slot, error: e.to_string() });
        }
    }
}

/// EXIF orientation, the same choice the negative uses: the raw IFD, and
/// the EXIF value when that IFD says the frame is not turned.
fn camera_orientation(path: &Path) -> u8 {
    let Ok(source) = mimizan_core::decode::raw_source(path) else {
        return 1;
    };
    let Ok(decoder) = rawler::get_decoder(&source) else {
        return 1;
    };
    let params = rawler::decoders::RawDecodeParams::default();
    let Ok(img) = decoder.raw_image(&source, &params, true) else {
        return 1;
    };
    let exif = decoder.raw_metadata(&source, &params).ok().and_then(|md| md.exif.orientation);
    use rawler::Orientation as O;
    match img.orientation {
        O::Normal | O::Unknown => exif.filter(|o| (1..=8).contains(o)).map(|o| o as u8).unwrap_or(1),
        O::HorizontalFlip => 2,
        O::Rotate180 => 3,
        O::VerticalFlip => 4,
        O::Transpose => 5,
        O::Rotate90 => 6,
        O::Transverse => 7,
        O::Rotate270 => 8,
    }
}

/// The stored pixels, turned the way the camera asked. Four bytes a pixel.
fn turn(px: &[u8], width: usize, height: usize, orientation: u8) -> (Vec<u8>, usize, usize) {
    if orientation <= 1 || width == 0 || height == 0 || px.len() != width * height * 4 {
        return (px.to_vec(), width, height);
    }
    let (nw, nh) = match orientation {
        5..=8 => (height, width),
        _ => (width, height),
    };
    let mut out = vec![0u8; nw * nh * 4];
    for y in 0..height {
        for x in 0..width {
            let (dx, dy) = match orientation {
                2 => (width - 1 - x, y),
                3 => (width - 1 - x, height - 1 - y),
                4 => (x, height - 1 - y),
                5 => (y, x),
                6 => (height - 1 - y, x),
                7 => (height - 1 - y, width - 1 - x),
                8 => (y, width - 1 - x),
                _ => (x, y),
            };
            let s = (y * width + x) * 4;
            let d = (dy * nw + dx) * 4;
            out[d..d + 4].copy_from_slice(&px[s..s + 4]);
        }
    }
    (out, nw, nh)
}

/// The plate already on disk is encoded again. `again` writes it over.
fn show_mimizan(raw: &Path, work: &Path, out: &Path, again: bool) -> Result<PlateMeta> {
    let negative = work.join("negative.tif");
    if again || !negative.is_file() {
        return develop_mimizan(raw, work, out);
    }
    let (meta, gray) = read_plate_samples(&negative)?;
    write_display(out, meta.frame_w, meta.frame_h, &encode_plate(&gray, &meta))?;
    Ok(meta)
}

fn develop_mimizan(raw: &Path, work: &Path, out: &Path) -> Result<PlateMeta> {
    let bin = find_mimizan();
    let negative = work.join("negative.tif");
    let cameras = find_cameras(&bin);
    let mut cmd = Command::new(&bin);
    cmd.arg("--log")
        .arg("warn")
        .arg("negative")
        .arg(raw)
        .arg("-o")
        .arg(&negative)
        .arg("--plate")
        .arg("camera");
    if let Some(dir) = &cameras {
        cmd.arg("--cameras").arg(dir);
    }
    run(&mut cmd, "mimizan negative")?;
    let (meta, gray) = read_plate_samples(&negative)?;
    write_display(out, meta.frame_w, meta.frame_h, &encode_plate(&gray, &meta))?;
    Ok(meta)
}

fn read_plate(negative: &Path) -> Result<PlateMeta> {
    Ok(read_plate_samples(negative)?.0)
}

fn develop_rawtherapee(raw: &Path, work: &Path, rival: &Rival, out: &Path, bin: &Path) -> Result<()> {
    let (bayer, xtrans) = match rival.kind {
        Kind::Bayer(method) => (method, "3-pass (best)"),
        Kind::Xtrans(method) => ("rcd", method),
        Kind::Dht => bail!("DHT is developed by dcraw"),
    };
    let pp3 = work.join(format!("rt_{}.pp3", rival.key));
    std::fs::write(&pp3, rt_profile(bayer, xtrans))?;
    let mut cmd = Command::new(bin);
    cmd.args(["-q", "-Y", "-t", "-b16", "-o"]).arg(out).arg("-p").arg(&pp3).arg("-c").arg(raw);
    let name = bin.display().to_string();
    run(&mut cmd, &name)?;
    if !out.is_file() {
        bail!("{name} wrote no {}", out.display());
    }
    Ok(())
}

/// The negative's matrix, developed when the folder does not have it yet.
fn ensure_plate(raw: &Path, work: &Path, again: bool) -> Result<PlateMeta> {
    let negative = work.join("negative.tif");
    if again || !negative.is_file() {
        develop_mimizan(raw, work, &work.join("mimizan.tif"))
    } else {
        read_plate(&negative)
    }
}

fn sensor_of(path: &Path) -> Result<Sensor> {
    let img = load_image(path, true)?;
    match &img.photometric {
        rawler::rawimage::RawPhotometricInterpretation::Cfa(cfg)
            if cfg.cfa.width == 6
                && cfg.cfa.height == 6
                && mimizan_lab_xtrans::xtrans::XTile::from_name(&cfg.cfa.name).is_some() =>
        {
            Ok(Sensor::Xtrans)
        }
        rawler::rawimage::RawPhotometricInterpretation::Cfa(cfg)
            if cfg.cfa.width == 2 && cfg.cfa.height == 2 =>
        {
            Ok(Sensor::Bayer)
        }
        _ => bail!("this file is neither a Bayer nor an X-Trans mosaic"),
    }
}

fn load_image(path: &Path, dummy: bool) -> Result<rawler::RawImage> {
    let source = mimizan_core::decode::raw_source(path).context("cannot be read")?;
    let decoder = rawler::get_decoder(&source).context("not a raw file this program reads")?;
    decoder
        .raw_image(&source, &rawler::decoders::RawDecodeParams::default(), dummy)
        .context("cannot be decoded")
}

/// The matrix the separation uses, taken from the file the same way.
fn camera_rows(img: &rawler::RawImage) -> Result<[[f64; 3]; 3]> {
    use rawler::imgop::xyz::Illuminant as I;
    let from_flat = |m: &[f32]| -> Option<[[f64; 3]; 3]> {
        if m.len() < 9 || m.iter().take(9).all(|v| *v == 0.0) {
            return None;
        }
        let row = |i: usize| [f64::from(m[3 * i]), f64::from(m[3 * i + 1]), f64::from(m[3 * i + 2])];
        Some([row(0), row(1), row(2)])
    };
    let preferred = [I::D65, I::D55, I::Daylight, I::D50, I::D75, I::FineWeather, I::CloudyWeather];
    let found = preferred
        .iter()
        .find_map(|k| img.color_matrix.get(k).and_then(|m| from_flat(m)))
        .or_else(|| {
            let mut keys: Vec<_> = img.color_matrix.keys().collect();
            keys.sort_by_key(|k| **k as u16);
            keys.into_iter().find_map(|k| from_flat(&img.color_matrix[k]))
        })
        .or_else(|| {
            let m = img.xyz_to_cam;
            let nonzero = m.iter().take(3).flatten().any(|v| *v != 0.0);
            nonzero.then(|| {
                let row = |i: usize| [f64::from(m[i][0]), f64::from(m[i][1]), f64::from(m[i][2])];
                [row(0), row(1), row(2)]
            })
        })
        .context("the file has no colour matrix")?;
    mimizan_core::plate::PlateWeights::camera(&found, mimizan_core::plate::PlateSpace::Srgb)
        .map(|w| w.rows)
        .map_err(|e| anyhow::anyhow!("{e}"))
}

fn xtrans_meta(raw: &Path) -> Result<PlateMeta> {
    let head = load_image(raw, true)?;
    match meta_of_xtrans(&head) {
        Ok(meta) => Ok(meta),
        Err(_) => meta_of_xtrans(&load_image(raw, false)?),
    }
}

fn meta_of_xtrans(img: &rawler::RawImage) -> Result<PlateMeta> {
    let frame = mimizan_lab_xtrans::convert::frame_of(img).map_err(anyhow::Error::msg)?;
    Ok(PlateMeta {
        range: 1.0,
        rows: camera_rows(img)?,
        frame_w: frame.width,
        frame_h: frame.height,
        crop: Crop { x: frame.x, y: frame.y, w: frame.width, h: frame.height },
    })
}

fn develop_least_squares(raw: &Path, meta: &PlateMeta, out: &Path, tx: &mpsc::Sender<Note>) -> Result<()> {
    let _ = tx.send(Note::Status("Developing the least-squares demosaic…".into()));
    let img = load_image(raw, false)?;
    let dev = mimizan_cli::bayer_ls::develop(&img, &|s| {
        let _ = tx.send(Note::Status(s));
    })
    .map_err(anyhow::Error::msg)?;
    let rgb = encode_balanced(&dev.data, dev.frame.width, dev.frame.height, dev.balance, meta.rows);
    write_display(out, dev.frame.width, dev.frame.height, &rgb)
}

fn develop_xtrans(raw: &Path, meta: &PlateMeta, out: &Path, tx: &mpsc::Sender<Note>) -> Result<()> {
    let _ = tx.send(Note::Status("Developing the X-Trans demosaic…".into()));
    let img = load_image(raw, false)?;
    let dev = mimizan_lab_xtrans::convert::develop_watched(
        &img,
        &mimizan_lab_xtrans::convert::Settings { keep_highlights: true },
        &|s| {
            let _ = tx.send(Note::Status(s.to_string()));
        },
        &|share| {
            let _ = tx.send(Note::Status(format!(
                "separating luminance and colour, {:.0}%",
                share.clamp(0.0, 1.0) * 100.0
            )));
        },
    )
    .map_err(anyhow::Error::msg)?;
    let rgb = encode_balanced(&dev.data, dev.frame.width, dev.frame.height, dev.balance, meta.rows);
    write_display(out, dev.frame.width, dev.frame.height, &rgb)
}

/// Unbalanced camera channels, 0 at black and [`WHITE`](mimizan_lab_xtrans::convert::WHITE)
/// at white, times the balance, then the shared matrix and the sRGB curve.
fn encode_balanced(
    data: &[u16],
    width: usize,
    height: usize,
    balance: [f64; 3],
    rows: [[f64; 3]; 3],
) -> Vec<u8> {
    let white = f64::from(mimizan_lab_xtrans::convert::WHITE);
    let mut rgb = vec![0u8; width * height * 3];
    for (i, px) in data.as_chunks::<3>().0.iter().take(width * height).enumerate() {
        let cam = [0, 1, 2].map(|c| f64::from(px[c]) / white * balance[c]);
        let lin = project(cam, rows);
        let o = i * 3;
        rgb[o] = display_byte(lin[0]);
        rgb[o + 1] = display_byte(lin[1]);
        rgb[o + 2] = display_byte(lin[2]);
    }
    rgb
}

fn develop_dht(raw: &Path, out: &Path, bin: &Path) -> Result<()> {
    let mut cmd = Command::new(bin);
    // Linear 16-bit (-4), camera channels (-o 0), camera white balance,
    // highlights clipped. -4 already refuses the automatic brightness.
    cmd.args(["-4", "-o", "0", "-w", "-H", "0", "-q", "11", "-T", "-Z"]).arg(out).arg(raw);
    let name = bin.display().to_string();
    run(&mut cmd, &name)?;
    if !out.is_file() {
        bail!("{name} wrote no {}", out.display());
    }
    Ok(())
}

fn run(cmd: &mut Command, name: &str) -> Result<()> {
    let output = cmd
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .output()
        .with_context(|| name.to_owned())?;
    if output.status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&output.stderr).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&output.stdout).into_owned();
    }
    let tail = text.trim();
    let tail = &tail[tail.len().saturating_sub(1200)..];
    bail!("{name} failed: {tail}");
}

fn load_picture(path: &Path) -> Result<Picture> {
    let image = image::open(path).with_context(|| format!("reading {}", path.display()))?.into_rgba8();
    let (width, height) = image.dimensions();
    Ok(Picture { width: width as usize, height: height as usize, rgba: image.into_raw() })
}

/// The picture beside the file list. The camera's embedded preview, kept
/// for the files already opened this time.
struct Preview {
    shown: PathBuf,
    texture: Option<egui::TextureHandle>,
    missing: bool,
    rx: Option<Receiver<Loaded>>,
    cache: Vec<(PathBuf, Cached)>,
}

struct Loaded {
    path: PathBuf,
    picture: Option<Picture>,
}

enum Cached {
    Image(egui::TextureHandle),
    Missing,
}

impl Default for Preview {
    fn default() -> Self {
        Self { shown: PathBuf::new(), texture: None, missing: false, rx: None, cache: Vec::new() }
    }
}

impl Preview {
    fn panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, dia: &FileDialog) {
        let Some(entry) = dia.selected_entry().filter(|e| e.is_file()) else {
            ui.label("Select a file.");
            return;
        };
        let path = entry.to_path_buf();
        ui.label(entry.file_name());
        self.ensure(ctx, &path);
        self.poll(ctx);
        if let Some(texture) = &self.texture {
            let [w, h] = texture.size();
            let max_w = ui.available_width().max(1.0);
            let max_h = ui.available_height().max(1.0);
            let scale = (max_w / w as f32).min(max_h / h as f32);
            let size = egui::vec2(w as f32 * scale, h as f32 * scale);
            ui.image((texture.id(), size));
        } else if self.missing {
            ui.label("This file has no preview.");
        } else {
            ui.label("Reading the preview…");
            ctx.request_repaint();
        }
    }

    fn ensure(&mut self, ctx: &egui::Context, path: &Path) {
        if self.shown == path && (self.texture.is_some() || self.missing || self.rx.is_some()) {
            return;
        }
        self.shown = path.to_path_buf();
        self.rx = None;
        if let Some((_, cached)) = self.cache.iter().find(|(p, _)| p == path) {
            match cached {
                Cached::Image(texture) => {
                    self.texture = Some(texture.clone());
                    self.missing = false;
                }
                Cached::Missing => {
                    self.texture = None;
                    self.missing = true;
                }
            }
            return;
        }
        self.texture = None;
        self.missing = false;
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        let path = path.to_path_buf();
        thread::spawn(move || {
            let picture = load_preview(&path);
            let _ = tx.send(Loaded { path, picture });
        });
        ctx.request_repaint();
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        let Ok(loaded) = rx.try_recv() else {
            ctx.request_repaint();
            return;
        };
        self.rx = None;
        if loaded.path != self.shown {
            return;
        }
        match loaded.picture {
            Some(picture) => {
                let texture = ctx.load_texture(
                    loaded.path.display().to_string(),
                    egui::ColorImage::from_rgba_unmultiplied([picture.width, picture.height], &picture.rgba),
                    egui::TextureOptions::LINEAR,
                );
                self.texture = Some(texture.clone());
                self.missing = false;
                self.remember(loaded.path, Cached::Image(texture));
            }
            None => {
                self.texture = None;
                self.missing = true;
                self.remember(loaded.path, Cached::Missing);
            }
        }
    }

    fn remember(&mut self, path: PathBuf, cached: Cached) {
        self.cache.retain(|(p, _)| p != &path);
        self.cache.push((path, cached));
        if self.cache.len() > 24 {
            self.cache.remove(0);
        }
    }
}

fn load_preview(path: &Path) -> Option<Picture> {
    let image = if is_raw_path(path) { raw_preview(path)? } else { image::open(path).ok()? };
    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    let (mut rgba, mut width, mut height) = (rgba.into_raw(), width as usize, height as usize);
    if is_raw_path(path) {
        let orientation = camera_orientation(path);
        if matches!(orientation, 5..=8) && width > height {
            (rgba, width, height) = turn(&rgba, width, height, orientation);
        }
    }
    Some(fit_preview(rgba, width, height, 480))
}

fn raw_preview(path: &Path) -> Option<image::DynamicImage> {
    // The largest embedded JPEG is often the full frame. The dialog wants
    // the small one, which a camera keeps beside the raw.
    if let Some(bytes) = embedded_jpeg(path) {
        if let Ok(image) = image::load_from_memory(&bytes) {
            return Some(image);
        }
    }
    let source = mimizan_core::decode::raw_source(path).ok()?;
    let decoder = rawler::get_decoder(&source).ok()?;
    let params = rawler::decoders::RawDecodeParams::default();
    decoder.thumbnail_image(&source, &params).ok().flatten()
}

/// The largest JPEG in the file that is still a preview: at least 8 KB,
/// at most 512 KB. Bigger than that is the full frame.
fn embedded_jpeg(path: &Path) -> Option<Vec<u8>> {
    let bytes = std::fs::read(path).ok()?;
    let mut best: Option<std::ops::Range<usize>> = None;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0xFF && bytes[i + 1] == 0xD8 {
            if let Some(end) = jpeg_end(&bytes, i) {
                let len = end - i;
                if (8 * 1024..=512 * 1024).contains(&len) && best.as_ref().is_none_or(|r| len > r.len()) {
                    best = Some(i..end);
                }
                i = end;
                continue;
            }
        }
        i += 1;
    }
    best.map(|range| bytes[range].to_vec())
}

/// The byte after a JPEG that starts at `start` (the FF D8), if the markers
/// parse. Entropy data uses FF 00 for a literal FF, so a real end marker is
/// not hidden in it.
fn jpeg_end(bytes: &[u8], start: usize) -> Option<usize> {
    let mut j = start + 2;
    while j + 1 < bytes.len() {
        if bytes[j] != 0xFF {
            j += 1;
            continue;
        }
        while j < bytes.len() && bytes[j] == 0xFF {
            j += 1;
        }
        let marker = *bytes.get(j)?;
        j += 1;
        if marker == 0xD9 {
            return Some(j);
        }
        if marker == 0xD8 || marker == 0x00 || (0xD0..=0xD7).contains(&marker) {
            continue;
        }
        let seglen = u16::from_be_bytes([*bytes.get(j)?, *bytes.get(j + 1)?]) as usize;
        if seglen < 2 || j + seglen > bytes.len() {
            return None;
        }
        j += seglen;
        if marker == 0xDA {
            while j + 1 < bytes.len() {
                if bytes[j] != 0xFF {
                    j += 1;
                    continue;
                }
                let next = bytes[j + 1];
                if next == 0x00 {
                    j += 2;
                    continue;
                }
                if next == 0xD9 {
                    return Some(j + 2);
                }
                return None;
            }
            return None;
        }
    }
    None
}

fn fit_preview(rgba: Vec<u8>, width: usize, height: usize, edge: u32) -> Picture {
    let long = width.max(height).max(1) as u32;
    let Some(image) = image::RgbaImage::from_raw(width as u32, height as u32, rgba) else {
        return Picture { width: 0, height: 0, rgba: Vec::new() };
    };
    if long <= edge {
        let (width, height) = (image.width() as usize, image.height() as usize);
        return Picture { width, height, rgba: image.into_raw() };
    }
    let tw = (image.width() * edge / long).max(1);
    let th = (image.height() * edge / long).max(1);
    let out = image::imageops::resize(&image, tw, th, image::imageops::FilterType::Triangle);
    Picture { width: tw as usize, height: th as usize, rgba: out.into_raw() }
}

/// Which of the two rival programs a button is choosing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum WhichTool {
    RawTherapee,
    Dcraw,
}

impl WhichTool {
    fn name(self) -> &'static str {
        match self {
            WhichTool::RawTherapee => "rawtherapee-cli",
            WhichTool::Dcraw => "dcraw_emu",
        }
    }
}

/// The programs the rivals are run with. An empty path means the name on
/// the path.
#[derive(Clone, Default)]
struct Tools {
    rawtherapee: PathBuf,
    dcraw: PathBuf,
}

impl Tools {
    fn get(&self, which: WhichTool) -> &Path {
        match which {
            WhichTool::RawTherapee => &self.rawtherapee,
            WhichTool::Dcraw => &self.dcraw,
        }
    }

    fn set(&mut self, which: WhichTool, path: PathBuf) {
        match which {
            WhichTool::RawTherapee => self.rawtherapee = path,
            WhichTool::Dcraw => self.dcraw = path,
        }
    }

    fn rawtherapee(&self) -> &Path {
        program(&self.rawtherapee, WhichTool::RawTherapee.name())
    }

    fn dcraw(&self) -> &Path {
        program(&self.dcraw, WhichTool::Dcraw.name())
    }
}

fn program<'a>(chosen: &'a Path, name: &'a str) -> &'a Path {
    if chosen.as_os_str().is_empty() {
        Path::new(name)
    } else {
        chosen
    }
}

fn tool_dialog() -> FileDialog {
    FileDialog::new().opening_mode(OpeningMode::AlwaysInitialDir)
}

/// Where the program dialog starts: the folder of the choice, then
/// `Program Files` on Windows, then home.
fn tool_directory(current: &Path) -> PathBuf {
    if let Some(parent) = current.parent() {
        if !parent.as_os_str().is_empty() && parent.is_dir() {
            return parent.to_path_buf();
        }
    }
    if let Some(programs) = std::env::var_os("ProgramFiles") {
        let programs = PathBuf::from(programs);
        if programs.is_dir() {
            return programs;
        }
    }
    dialog_directory("")
}

fn tools_file() -> Option<PathBuf> {
    Some(mimizan_core::home::settings_dir()?.join("compare.tools"))
}

/// Where versions up to 0.20.15 kept it: beside the program.
fn old_tools_file() -> Option<PathBuf> {
    Some(std::env::current_exe().ok()?.with_file_name("mimizan-compare.tools"))
}

fn load_tools() -> Tools {
    [tools_file(), old_tools_file()]
        .into_iter()
        .flatten()
        .find_map(|path| std::fs::read_to_string(path).ok())
        .map(|text| decode_tools(&text))
        .unwrap_or_default()
}

fn save_tools(tools: &Tools) -> Result<()> {
    let path = tools_file().context("there is no settings folder")?;
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).with_context(|| format!("creating {}", dir.display()))?;
    }
    mimizan_core::outfile::write_bytes(&path, encode_tools(&tools.rawtherapee, &tools.dcraw).as_bytes())?;
    Ok(())
}

fn encode_tools(rawtherapee: &Path, dcraw: &Path) -> String {
    let mut out = String::new();
    if !rawtherapee.as_os_str().is_empty() {
        out.push_str("rawtherapee-cli=");
        out.push_str(&rawtherapee.display().to_string());
        out.push('\n');
    }
    if !dcraw.as_os_str().is_empty() {
        out.push_str("dcraw_emu=");
        out.push_str(&dcraw.display().to_string());
        out.push('\n');
    }
    out
}

fn decode_tools(text: &str) -> Tools {
    let mut tools = Tools::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else { continue };
        let path = PathBuf::from(value);
        match key {
            "rawtherapee-cli" => tools.rawtherapee = path,
            "dcraw_emu" => tools.dcraw = path,
            _ => {}
        }
    }
    tools
}

fn file_dialog() -> FileDialog {
    FileDialog::new()
        .opening_mode(OpeningMode::AlwaysInitialDir)
        .default_file_filter("RAW")
        .add_file_filter("RAW", egui_file_dialog::Filter::new(is_raw_path))
}

/// Where Open starts. A typed or open file uses its folder. Otherwise home,
/// and the working directory when there is no home.
fn dialog_directory(typed: &str) -> PathBuf {
    let path = PathBuf::from(typed.trim());
    if path.is_dir() {
        return path;
    }
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() && parent.is_dir() {
            return parent.to_path_buf();
        }
    }
    if let Some(home) = mimizan_core::home::home_dir().filter(|home| home.is_dir()) {
        return home;
    }
    std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))
}

fn window_title(path: &str) -> String {
    let name = Path::new(path).file_name().and_then(|s| s.to_str()).filter(|s| !s.is_empty());
    match name {
        Some(name) => format!("Mimizan compare — {name}"),
        None => "Mimizan compare".to_string(),
    }
}

fn is_raw_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RAW_EXTENSIONS.iter().any(|ext| e.eq_ignore_ascii_case(ext)))
}

fn work_dir(raw: &Path) -> PathBuf {
    let stem = raw.file_stem().and_then(|s| s.to_str()).unwrap_or("picture");
    let parent = raw.parent().unwrap_or_else(|| Path::new("."));
    parent.join(format!("{stem}.mimizan-compare"))
}

fn find_mimizan() -> PathBuf {
    if let Some(p) = std::env::var_os("MIMIZAN") {
        return PathBuf::from(p);
    }
    std::env::current_exe()
        .ok()
        .and_then(|exe| mimizan_beside(exe.parent()?))
        .unwrap_or_else(|| PathBuf::from("mimizan"))
}

/// The command that came with a loupe in `dir`: next to it, or inside the
/// app the Mac download puts next to it.
fn mimizan_beside(dir: &Path) -> Option<PathBuf> {
    [dir.to_path_buf(), dir.join("Mimizan Lab.app").join("Contents").join("MacOS")]
        .into_iter()
        .map(|dir| named(&dir, "mimizan"))
        .find(|p| p.is_file())
}

/// `stem` in `dir`, or `stem.exe` where the program is built that way.
fn named(dir: &Path, stem: &str) -> PathBuf {
    let path = dir.join(stem);
    if path.is_file() {
        return path;
    }
    let exe = dir.join(format!("{stem}.exe"));
    if exe.is_file() {
        return exe;
    }
    path
}

/// The camera files for the command at `bin`: `MIMIZAN_CAMERAS`, or the
/// folder installed with that command. A `cameras` folder of the user's,
/// in whatever folder the loupe was started from, is not the program's;
/// with it the same RAW would come out another picture. A command found
/// on the path looks for its own.
fn find_cameras(bin: &Path) -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("MIMIZAN_CAMERAS").map(PathBuf::from).filter(|p| p.is_dir()) {
        return Some(dir);
    }
    let dir = bin.parent().filter(|dir| dir.is_absolute())?;
    mimizan_core::calib::beside_program(dir, std::ffi::OsStr::new("cameras"))
}

/// Neutral RawTherapee profile: the named demosaicer, camera white balance,
/// camera channels left as they are. No sharpening, no noise reduction, no
/// highlight recovery, no baseline exposure. The sRGB curve is only so the
/// 16-bit file has a known encoding; it is undone before the shared matrix.
fn rt_profile(bayer: &str, xtrans: &str) -> String {
    format!(
        "[Version]\nAppVersion=5.11\nVersion=351\n\n\
[Exposure]\nAuto=false\nClip=0\nCompensation=0\nBrightness=0\nContrast=0\nSaturation=0\nBlack=0\n\
HighlightCompr=0\nHighlightComprThreshold=0\nShadowCompr=0\nHistogramMatching=false\n\
CurveFromHistogramMatching=false\nClampOOG=true\nCurveMode=Standard\nCurveMode2=Standard\nCurve=0\nCurve2=0\n\n\
[HLRecovery]\nEnabled=false\nMethod=Blend\n\n\
[White Balance]\nEnabled=true\nSetting=Camera\nTemperature=6504\nGreen=1\nEqual=1\n\n\
[Color Management]\nInputProfile=(none)\nToneCurve=false\nApplyLookTable=false\nApplyBaselineExposureOffset=false\n\
ApplyHueSatMap=false\nDCPIlluminant=0\nWorkingProfile=sRGB\nOutputProfile=RTv4_sRGB\nOutputProfileIntent=Relative\nOutputBPC=false\n\n\
[Sharpening]\nEnabled=false\n\n[SharpenEdge]\nEnabled=false\n\n[SharpenMicro]\nEnabled=false\n\n\
[Impulse Denoising]\nEnabled=false\n\n[Defringing]\nEnabled=false\n\n[Directional Pyramid Denoising]\nEnabled=false\n\n\
[RAW]\nCA=false\nHotPixelFilter=false\nDeadPixelFilter=false\nPreExposure=1\n\n\
[RAW Bayer]\nMethod={bayer}\nBorder=0\nCcSteps=0\nPreBlack0=0\nPreBlack1=0\nPreBlack2=0\nPreBlack3=0\n\
PreTwoGreen=true\nLineDenoise=0\nGreenEqThreshold=0\nDCBIterations=2\nDCBEnhance=false\nLMMSEIterations=2\n\
DualDemosaicAutoContrast=false\nDualDemosaicContrast=0\nPDAFLinesFilter=false\n\n\
[RAW X-Trans]\nMethod={xtrans}\n"
    )
}

/// The camera plate: its matrix, the range the negative was stored with,
/// and the crop of the active area.
#[derive(Clone)]
struct PlateMeta {
    range: f64,
    rows: [[f64; 3]; 3],
    frame_w: usize,
    frame_h: usize,
    crop: Crop,
}

#[derive(Clone, Copy)]
struct Crop {
    x: usize,
    y: usize,
    w: usize,
    h: usize,
}

fn read_plate_samples(path: &Path) -> Result<(PlateMeta, Vec<u16>)> {
    let (width, height, samples, desc) = read_tiff_u16(path, 1)?;
    let meta = plate_meta(&desc, width, height)?;
    Ok((meta, samples))
}

fn plate_meta(desc: &str, width: usize, height: usize) -> Result<PlateMeta> {
    let v: serde_json::Value = serde_json::from_str(desc).context("the negative has no plate description")?;
    // A plate whose samples stay within 1 omits `range`. The print reads
    // that as 1, and so does this view.
    let range = match v.get("range") {
        None => 1.0,
        Some(n) => n
            .as_f64()
            .filter(|r| r.is_finite() && *r >= 1.0)
            .context("the negative states a range below 1")?,
    };
    let plate = v.get("plate").context("the negative is not a plate")?;
    let frames = plate.get("frames").and_then(|f| f.as_array()).context("the plate states no frames")?;
    let names: Vec<&str> = frames.iter().filter_map(|f| f.as_str()).collect();
    if names != ["blue", "green", "red"] {
        bail!("the plate frames are {}, and this view reads blue, green, red", names.join(", "));
    }
    let frame_h =
        plate.get("frame_height").and_then(|n| n.as_u64()).context("the plate states no frame height")?
            as usize;
    if frame_h == 0 || frame_h.checked_mul(3) != Some(height) {
        bail!("the plate is {width}x{height}, not three frames of {frame_h}");
    }
    let weights = plate.get("weights").and_then(|w| w.as_array()).context("the plate states no matrix")?;
    if weights.len() != 3 {
        bail!("the plate matrix has {} rows", weights.len());
    }
    let mut rows = [[0.0; 3]; 3];
    for (row, values) in rows.iter_mut().zip(weights) {
        let values = values.as_array().context("a plate matrix row is not a row")?;
        if values.len() != 3 {
            bail!("a plate matrix row has {} numbers", values.len());
        }
        for (out, value) in row.iter_mut().zip(values) {
            *out = value.as_f64().filter(|n| n.is_finite()).context("a plate matrix number is not finite")?;
        }
    }
    let crop = v.get("report").and_then(|r| r.get("crop")).map(read_crop).transpose()?.unwrap_or(Crop {
        x: 0,
        y: 0,
        w: width,
        h: frame_h,
    });
    Ok(PlateMeta { range, rows, frame_w: width, frame_h, crop })
}

fn read_crop(v: &serde_json::Value) -> Result<Crop> {
    let n = |key: &str| -> Result<usize> {
        v.get(key)
            .and_then(|n| n.as_u64())
            .map(|n| n as usize)
/// Where the choice is kept: with the darkroom's settings. An installed
/// program cannot write beside itself.
            .with_context(|| format!("the crop has no {key}"))
    };
    Ok(Crop { x: n("x")?, y: n("y")?, w: n("w")?, h: n("h")? })
}

/// The three stored frames, blue, green, red, as 8-bit sRGB. They are
/// already the camera matrix. `range` undoes the scaling of the file.
fn encode_plate(gray: &[u16], meta: &PlateMeta) -> Vec<u8> {
    let (w, h) = (meta.frame_w, meta.frame_h);
    let mut rgb = vec![0u8; w * h * 3];
    for y in 0..h {
        for x in 0..w {
            let at = |frame: usize| {
                let v = f64::from(gray[(frame * h + y) * w + x]) / 65535.0 * meta.range;
                display_byte(v)
            };
            let i = (y * w + x) * 3;
            rgb[i] = at(2);
            rgb[i + 1] = at(1);
            rgb[i + 2] = at(0);
        }
    }
    rgb
}

/// A rival's linear camera channels through the plate matrix and the sRGB
/// curve, cropped to the active area the negative kept.
fn finish_demosaic(linear: &Path, meta: &PlateMeta, out: &Path, encoded: bool) -> Result<()> {
    let (width, height, samples, _) = read_tiff_u16(linear, 3)?;
    let crop = placed(width, height, meta.crop);
    let mut rgb = vec![0u8; crop.w * crop.h * 3];
    for y in 0..crop.h {
        for x in 0..crop.w {
            let p = ((crop.y + y) * width + crop.x + x) * 3;
            let cam = [samples[p], samples[p + 1], samples[p + 2]].map(|v| {
                let x = f64::from(v) / 65535.0;
                if encoded {
                    srgb_decode(x)
                } else {
                    x
                }
            });
            let lin = project(cam, meta.rows);
            let i = (y * crop.w + x) * 3;
            rgb[i] = display_byte(lin[0]);
            rgb[i + 1] = display_byte(lin[1]);
            rgb[i + 2] = display_byte(lin[2]);
        }
    }
    write_display(out, crop.w, crop.h, &rgb)
}

/// The crop, when it lies inside `width`×`height`. A file that is already
/// the active area is kept whole.
fn placed(width: usize, height: usize, crop: Crop) -> Crop {
    if crop.w == width && crop.h == height {
        return Crop { x: 0, y: 0, w: width, h: height };
    }
    if crop.w > 0
        && crop.h > 0
        && crop.x.saturating_add(crop.w) <= width
        && crop.y.saturating_add(crop.h) <= height
    {
        return crop;
    }
    Crop { x: 0, y: 0, w: width, h: height }
}

fn project(cam: [f64; 3], rows: [[f64; 3]; 3]) -> [f64; 3] {
    [
        rows[0][0] * cam[0] + rows[0][1] * cam[1] + rows[0][2] * cam[2],
        rows[1][0] * cam[0] + rows[1][1] * cam[1] + rows[1][2] * cam[2],
        rows[2][0] * cam[0] + rows[2][1] * cam[1] + rows[2][2] * cam[2],
    ]
}

/// sRGB inverse electro-optical transfer, the curve the RawTherapee file
/// was written with.
fn srgb_decode(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}
/// The command that develops the plate: `MIMIZAN`, the one that came
/// with this program, or the name on the path.

/// sRGB electro-optical transfer.
fn srgb_encode(v: f64) -> f64 {
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

fn display_byte(linear: f64) -> u8 {
    let v = srgb_encode(linear.clamp(0.0, 1.0));
    (v * 255.0).round().clamp(0.0, 255.0) as u8
}

fn read_tiff_u16(path: &Path, samples: usize) -> Result<(usize, usize, Vec<u16>, String)> {
    let file = std::io::BufReader::new(
        std::fs::File::open(path).with_context(|| format!("reading {}", path.display()))?,
    );
    let mut limits = tiff::decoder::Limits::default();
    limits.decoding_buffer_size = SAMPLE_BYTES_MAX;
    limits.intermediate_buffer_size = SAMPLE_BYTES_MAX;
    let mut decoder = tiff::decoder::Decoder::new(file)
        .map_err(|e| anyhow::anyhow!("{e}"))
        .with_context(|| format!("reading {}", path.display()))?
        .with_limits(limits);
    let desc = decoder.get_tag_ascii_string(tiff::tags::Tag::ImageDescription).unwrap_or_default();
    let (width, height) = decoder.dimensions().map_err(|e| anyhow::anyhow!("{e}"))?;
    let got = decoder.colortype().map_err(|e| anyhow::anyhow!("{e}"))?;
    let image = decoder.read_image().map_err(|e| anyhow::anyhow!("{e}"))?;
    let tiff::decoder::DecodingResult::U16(data) = image else {
        bail!("{} is not 16-bit", path.display());
    };
    let (width, height) = (width as usize, height as usize);
    if Some(data.len()) != width.checked_mul(height).and_then(|n| n.checked_mul(samples)) {
        bail!(
            "{} has {} samples a pixel ({got:?}), and this view reads {samples}",
            path.display(),
            data.len() / width.max(1) / height.max(1)
        );
    }
    Ok((width, height, data, desc))
}

fn write_display(path: &Path, width: usize, height: usize, rgb: &[u8]) -> Result<()> {
    let image = image::RgbImage::from_raw(width as u32, height as u32, rgb.to_vec())
        .with_context(|| format!("the display is {width}x{height}"))?;
    image.save(path).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

fn fit_mag(pane: egui::Vec2, image: egui::Vec2) -> f32 {
    if image.x <= 0.0 || image.y <= 0.0 {
        return 1.0;
    }
    (pane.x / image.x).min(pane.y / image.y).clamp(0.02, 32.0)
}

fn origin(pane: egui::Rect, shown: egui::Vec2, pan: egui::Vec2) -> egui::Pos2 {
    pane.center() - shown * 0.5 + pan
}

fn image_norm(pane: egui::Rect, shown: egui::Vec2, pan: egui::Vec2, screen: egui::Pos2) -> egui::Vec2 {
    let top_left = origin(pane, shown, pan);
    (screen - top_left) / shown
}

fn pan_keeping(
    pane: egui::Rect,
    old: egui::Vec2,
    new: egui::Vec2,
    pan: egui::Vec2,
    cursor: egui::Pos2,
) -> egui::Vec2 {
    let norm = image_norm(pane, old, pan, cursor);
    (cursor - pane.center()) + new * 0.5 - new * norm
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Whatever the picture and the card, no tile is larger than the
    /// card takes, the tiles cover every pixel once, and a picture that
    /// fits is one texture as before.
    #[test]
    fn a_picture_is_cut_into_textures_the_card_takes() {
        assert_eq!(tile_grid(6048, 4032, 8192), vec![[0, 0, 6048, 4032]]);
        assert_eq!(tile_grid(8192, 8192, 8192), vec![[0, 0, 8192, 8192]]);
        assert_eq!(
            tile_grid(8256, 5504, 8192),
            vec![[0, 0, 8192, 5504], [8192, 0, 64, 5504]],
            "a 45 MP frame on the Windows build"
        );
        for (w, h, max) in [(9504, 6336, 8192), (11648, 8736, 4096), (5, 3, 2), (1, 1, 1), (7, 7, 0)] {
            let tiles = tile_grid(w, h, max);
            let mut seen = vec![0u8; w * h];
            for [x, y, tw, th] in tiles {
                assert!(tw >= 1 && th >= 1 && tw <= max.max(1) && th <= max.max(1));
                for row in y..y + th {
                    for col in x..x + tw {
                        seen[row * w + col] += 1;
                    }
                }
            }
            assert!(seen.iter().all(|n| *n == 1), "{w}x{h} at {max}");
        }
        assert!(tile_grid(0, 10, 8192).is_empty());
    }

    #[test]
    fn a_missing_file_has_no_preview() {
        assert!(load_preview(Path::new("/no/such/file.nef")).is_none());
    }

    #[test]
    fn the_preview_fits_the_panel() {
        let px = vec![0u8; 8 * 4 * 4];
        let fitted = fit_preview(px, 8, 4, 4);
        assert_eq!((fitted.width, fitted.height), (4, 2));
        assert_eq!(fitted.rgba.len(), 4 * 2 * 4);
    }

    #[test]
    fn a_turned_camera_stands_up() {
        // Two pixels, red then green. Orientation 8 is rotate 270° clockwise,
        // so the green one is on top.
        let px = [255, 0, 0, 255, 0, 255, 0, 255];
        let (out, w, h) = turn(&px, 2, 1, 8);
        assert_eq!((w, h), (1, 2));
        assert_eq!(out, [0, 255, 0, 255, 255, 0, 0, 255]);
        let (same, w, h) = turn(&px, 2, 1, 1);
        assert_eq!((w, h), (2, 1));
        assert_eq!(same, px);
    }

    #[test]
    fn the_centre_of_the_pane_is_the_centre_of_the_picture() {
        let pane = egui::Rect::from_min_size(egui::pos2(0.0, 0.0), egui::vec2(400.0, 200.0));
        let shown = egui::vec2(100.0, 50.0);
        let norm = image_norm(pane, shown, egui::Vec2::ZERO, pane.center());
        assert!((norm.x - 0.5).abs() < 1e-4);
        assert!((norm.y - 0.5).abs() < 1e-4);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_cursor() {
        let pane = egui::Rect::from_min_size(egui::pos2(10.0, 20.0), egui::vec2(800.0, 600.0));
        let old = egui::vec2(200.0, 150.0);
        let pan = egui::vec2(30.0, -10.0);
        let cursor = egui::pos2(420.0, 280.0);
        let before = image_norm(pane, old, pan, cursor);
        let new = old * 2.0;
        let kept = pan_keeping(pane, old, new, pan, cursor);
        let after = image_norm(pane, new, kept, cursor);
        assert!((before.x - after.x).abs() < 1e-4);
        assert!((before.y - after.y).abs() < 1e-4);
    }

    #[test]
    fn a_raw_name_is_case_insensitive_and_a_jpeg_is_not() {
        assert!(is_raw_path(Path::new("/photos/DSC_100.NEF")));
        assert!(is_raw_path(Path::new("shot.raf")));
        assert!(is_raw_path(Path::new("plate.dng")));
        assert!(!is_raw_path(Path::new("plate.jpg")));
        assert!(!is_raw_path(Path::new("noext")));
    }

    #[test]
    fn the_dialog_opens_beside_the_file() {
        let dir = std::env::temp_dir().join(format!("mimizan-compare-open-{}", std::process::id()));
        let _ = std::fs::create_dir(&dir);
        let file = dir.join("DSC.NEF");
        assert_eq!(dialog_directory(&file.display().to_string()), dir);
        assert_eq!(dialog_directory(&dir.display().to_string()), dir);
        assert!(dialog_directory("").is_dir());
        assert_eq!(window_title(""), "Mimizan compare");
        assert_eq!(window_title("/photos/DSC.NEF"), "Mimizan compare — DSC.NEF");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_chosen_program_is_read_back_and_an_empty_one_is_the_name() {
        let rt = Path::new("C:/Program Files/RawTherapee/5.11/rawtherapee-cli.exe");
        let dcraw = Path::new("D:/libraw/bin/dcraw_emu.exe");
        let text = encode_tools(rt, dcraw);
        let tools = decode_tools(&text);
        assert_eq!(tools.rawtherapee(), rt);
        assert_eq!(tools.dcraw(), dcraw);
        let only = decode_tools("dcraw_emu=D:/libraw/bin/dcraw_emu.exe\n");
        assert_eq!(only.rawtherapee(), Path::new("rawtherapee-cli"));
        assert_eq!(only.dcraw(), dcraw);
        assert_eq!(decode_tools("").rawtherapee(), Path::new("rawtherapee-cli"));
        assert_eq!(decode_tools("").dcraw(), Path::new("dcraw_emu"));
    }

    #[test]
    fn an_algorithm_reads_the_mosaic_it_was_measured_on() {
        assert!(Algo::Separation.fits(Sensor::Bayer));
        assert!(Algo::LeastSquares.fits(Sensor::Bayer));
        assert!(!Algo::Xtrans.fits(Sensor::Bayer));
        assert!(Algo::Xtrans.fits(Sensor::Xtrans));
        assert!(!Algo::Separation.fits(Sensor::Xtrans));
        assert_eq!(Sensor::Bayer.home(), Algo::Separation);
        assert_eq!(Sensor::Xtrans.home(), Algo::Xtrans);
        assert_eq!(Sensor::Xtrans.rivals().len(), 4);
        assert!(rt_profile("rcd", "1-pass (medium)").contains("[RAW X-Trans]\nMethod=1-pass (medium)"));
    }

    #[test]
    fn balanced_camera_channels_take_the_matrix_once() {
        let white = mimizan_lab_xtrans::convert::WHITE;
        let rows = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let rgb = encode_balanced(&[white, 0, 0], 1, 1, [1.0, 1.0, 1.0], rows);
        assert_eq!(rgb, vec![255, 0, 0]);
        let doubled = encode_balanced(&[white / 2, 0, 0], 1, 1, [2.0, 1.0, 1.0], rows);
        assert_eq!(doubled[0], 255);
    }

    #[test]
    fn the_profile_names_the_demosaicer() {
        let text = rt_profile("lmmse", "3-pass (best)");
        assert!(text.contains("Method=lmmse"));
        assert!(text.contains("Method=3-pass (best)"));
        assert!(text.contains("Sharpening]\nEnabled=false"));
        assert!(text.contains("DCBEnhance=false"));
        assert!(text.contains("HLRecovery]\nEnabled=false"));
        assert!(text.contains("ApplyBaselineExposureOffset=false"));
        assert!(text.contains("OutputBPC=false"));
    }

    #[test]
    fn the_curve_is_the_srgb_curve_and_white_is_white() {
        assert_eq!(display_byte(0.0), 0);
        assert_eq!(display_byte(1.0), 255);
        assert_eq!(display_byte(2.0), 255);
        assert_eq!(display_byte(-1.0), 0);
        let mid = srgb_encode(0.5);
        assert!((mid - 0.735356983).abs() < 1e-6);
        assert!((srgb_decode(mid) - 0.5).abs() < 1e-9);
    }

    #[test]
    fn the_matrix_is_applied_once_and_the_active_area_is_kept() {
        let rows = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        assert_eq!(project([0.2, 0.4, 0.6], rows), [0.2, 0.4, 0.6]);
        let full = placed(100, 80, Crop { x: 8, y: 4, w: 100, h: 80 });
        assert_eq!((full.x, full.y), (0, 0));
        let crop = placed(200, 100, Crop { x: 8, y: 4, w: 100, h: 80 });
        assert_eq!((crop.x, crop.y, crop.w, crop.h), (8, 4, 100, 80));
    }

    #[test]
    fn the_plate_description_gives_the_matrix_and_the_crop() {
        let desc = r#"{"range":1.5,"report":{"crop":{"x":8,"y":4,"w":6,"h":4}},"plate":{"frames":["blue","green","red"],"frame_height":4,"weights":[[1,0,0],[0,1,0],[0,0,1]]}}"#;
        let meta = plate_meta(desc, 6, 12).unwrap();
        assert_eq!(meta.range, 1.5);
        assert_eq!((meta.crop.x, meta.crop.y, meta.crop.w, meta.crop.h), (8, 4, 6, 4));
/// Most bytes of samples a file in the work folder can hold: the largest
/// sheet the program writes, three 16-bit channels. A file that claims
/// more is not one of ours or a tool's, and is refused with an error and
/// not with an allocation the machine cannot make.
const SAMPLE_BYTES_MAX: usize = mimizan_core::recipe::SHEET_MAX_PX.saturating_mul(3 * 2);

        let gray = vec![0u16; 6 * 12];
        let rgb = encode_plate(&gray, &meta);
        assert_eq!(rgb.len(), 6 * 4 * 3);
        assert!(rgb.iter().all(|&v| v == 0));
        let bare = r#"{"report":{"crop":{"x":8,"y":4,"w":6,"h":4}},"plate":{"frames":["blue","green","red"],"frame_height":4,"weights":[[1,0,0],[0,1,0],[0,0,1]]}}"#;
        assert_eq!(plate_meta(bare, 6, 12).unwrap().range, 1.0);
    }
}
