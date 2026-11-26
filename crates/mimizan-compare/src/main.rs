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
//! were developed from. *Develop again* writes them over. *Stop* ends a
//! development, and so does closing the window: the tools are ended and
//! what they had not finished is cleared away.
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
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::thread;
use std::time::{Duration, Instant};

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
        picture: Tiled,
    },
    Missed {
        slot: usize,
        error: String,
    },
    Finished,
}

/// What a job's threads hold of the window: where their notes go, the
/// window to wake for one, the flag that ends the job, and how many of its
/// tools are running or still have a file to clear away.
#[derive(Clone)]
struct Out {
    tx: mpsc::Sender<Note>,
    ctx: egui::Context,
    stop: Arc<AtomicBool>,
    tools: Arc<AtomicUsize>,
}

impl Out {
    fn send(&self, note: Note) {
        let _ = self.tx.send(note);
        self.ctx.request_repaint();
    }

    fn status(&self, text: impl Into<String>) {
        self.send(Note::Status(text.into()));
    }

    fn missed(&self, slot: usize, error: String) {
        self.send(Note::Missed { slot, error });
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// A development on its way: what the window holds of it.
struct Job {
    rx: Receiver<Note>,
    stop: Arc<AtomicBool>,
    tools: Arc<AtomicUsize>,
}

/// Counts a tool from its start until its files are cleared away. Closing
/// the window waits for that count.
struct Running<'a>(&'a AtomicUsize);

impl<'a> Running<'a> {
    fn new(count: &'a AtomicUsize) -> Self {
        count.fetch_add(1, Ordering::SeqCst);
        Self(count)
    }
}

impl Drop for Running<'_> {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// `slot` 0 is Mimizan. The rivals follow in [`RIVALS`] order.
struct App {
    ctx: egui::Context,
    /// The text of the path field. It is an input: the file is `shown`.
    path: String,
    /// The file the panes show, or are being developed from.
    shown: Option<PathBuf>,
    /// The window title as last sent. Sending one asks for a frame.
    title: String,
    algo: Algo,
    sensor: Option<Sensor>,
    dialog: FileDialog,
    preview: Preview,
    view: View,
    fitted: bool,
    which: usize,
    textures: Vec<Option<Tiled>>,
    errors: Vec<Option<String>>,
    status: String,
    job: Option<Job>,
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

const TITLE: &str = "Mimizan compare";

fn main() -> eframe::Result {
    let initial = std::env::args_os().nth(1).map(PathBuf::from);
    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([1440.0, 860.0])
        .with_min_inner_size([800.0, 500.0])
        .with_title(TITLE);
    eframe::run_native(
        TITLE,
        eframe::NativeOptions { viewport, ..Default::default() },
        Box::new(move |cc| Ok(Box::new(App::new(initial, cc.egui_ctx.clone())))),
    )
}

impl App {
    fn new(file: Option<PathBuf>, ctx: egui::Context) -> Self {
        let n = 1 + RIVALS.len();
        let mut app = Self {
            preview: Preview::new(ctx.clone()),
            ctx,
            path: String::new(),
            shown: None,
            title: TITLE.into(),
            algo: Algo::Separation,
            sensor: None,
            dialog: file_dialog(),
            view: View { mag: 1.0, pan: egui::Vec2::ZERO },
            fitted: false,
            which: 0,
            textures: (0..n).map(|_| None).collect(),
            errors: (0..n).map(|_| None).collect(),
            status: "Open a RAW file, or drop one here.".into(),
            job: None,
            tools: load_tools(),
            tool_dialog: tool_dialog(),
            asking: None,
        };
        if let Some(file) = file.filter(|f| !f.as_os_str().is_empty()) {
            app.take_file(file);
        }
        app
    }

    fn working(&self) -> bool {
        self.job.is_some()
    }

    fn rivals(&self) -> &'static [Rival] {
        self.sensor.map(Sensor::rivals).unwrap_or(RIVALS)
    }

    /// The folder the dialog opens in: the file on screen, or home.
    fn open_beside(&self) -> PathBuf {
        dialog_directory(&self.named_file())
    }

    fn open_dialog(&mut self) {
        if self.working() {
            return;
        }
        self.dialog.config_mut().initial_directory = self.open_beside();
        self.dialog.pick_file();
    }

    /// A file from the dialog, a drop or the command line.
    fn take_file(&mut self, path: PathBuf) {
        if self.working() {
            return;
        }
        self.path = path.display().to_string();
        self.develop_file(path, false);
    }

    /// The file the path field names: the one on screen while the field
    /// still shows its name, and else what was typed there.
    fn named_file(&self) -> PathBuf {
        let typed = self.path.trim();
        match &self.shown {
            Some(shown) if shown.display().to_string() == typed => shown.clone(),
            _ => PathBuf::from(typed),
        }
    }

    fn develop(&mut self, again: bool) {
        self.develop_file(self.named_file(), again);
    }

    fn develop_file(&mut self, raw: PathBuf, again: bool) {
        if self.working() {
            return;
        }
        if !raw.is_file() {
            self.status = not_a_file(&raw);
            return;
        }
        self.fitted = false;
        self.textures.iter_mut().for_each(|t| *t = None);
        self.errors.fill(None);
        self.status = "Developing…".into();
        self.shown = Some(raw.clone());
        self.start(raw, again, false);
    }

    /// The other Mimizan algorithm, the rivals staying as they are.
    fn develop_left(&mut self) {
        if self.working() {
            return;
        }
        let Some(raw) = self.shown.clone() else { return };
        if !raw.is_file() {
            self.status = not_a_file(&raw);
            return;
        }
        self.fitted = false;
        self.textures[0] = None;
        self.errors[0] = None;
        self.status = format!("Developing {}…", self.algo.label());
        self.start(raw, false, true);
    }

    fn start(&mut self, raw: PathBuf, again: bool, only_left: bool) {
        let (tx, rx) = mpsc::channel();
        let stop = Arc::new(AtomicBool::new(false));
        let tools = Arc::new(AtomicUsize::new(0));
        let out = Out { tx, ctx: self.ctx.clone(), stop: Arc::clone(&stop), tools: Arc::clone(&tools) };
        self.job = Some(Job { rx, stop, tools });
        let algo = self.algo;
        let programs = self.tools.clone();
        thread::spawn(move || {
            // A file is never a reason for the window to stay on
            // "Developing…": what stops the job is shown, and it ends.
            let done = catch_unwind(AssertUnwindSafe(|| {
                develop_all(&raw, again, algo, only_left, &programs, &out);
            }));
            if let Err(payload) = done {
                out.missed(0, format!("stopped: {}", panic_text(payload.as_ref())));
            }
            out.send(Note::Finished);
        });
    }

    /// Ends the tools of a running job. A demosaic of this program runs to
    /// its end first.
    fn stop(&mut self) {
        if let Some(job) = &self.job {
            job.stop.store(true, Ordering::Relaxed);
            self.status = "Stopping…".into();
        }
    }

    /// The window closes: the tools are ended and their unfinished files
    /// cleared away before the process is gone and nobody could.
    fn end_job(&mut self) {
        let Some(job) = &self.job else { return };
        job.stop.store(true, Ordering::Relaxed);
        let until = Instant::now() + Duration::from_secs(3);
        while job.tools.load(Ordering::SeqCst) > 0 && Instant::now() < until {
            thread::sleep(Duration::from_millis(20));
        }
    }

    fn poll(&mut self) {
        let Some(job) = self.job.take() else { return };
        let mut finished = false;
        loop {
            let note = match job.rx.try_recv() {
                Ok(note) => note,
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    finished = true;
                    break;
                }
            };
            match note {
                Note::Status(s) => {
                    if !job.stop.load(Ordering::Relaxed) {
                        self.status = s;
                    }
                }
                Note::Sensor(sensor) => {
                    self.sensor = Some(sensor);
                    if !self.algo.fits(sensor) {
                        self.algo = sensor.home();
                    }
                    let n = 1 + sensor.rivals().len();
                    self.textures.resize_with(n, || None);
                    self.errors.resize_with(n, || None);
                    if self.which >= sensor.rivals().len() {
                        self.which = 0;
                    }
                }
                Note::Ready { slot, picture } => {
                    if slot < self.textures.len() {
                        self.textures[slot] = Some(picture);
                        self.errors[slot] = None;
                    }
                }
                Note::Missed { slot, error } => {
                    if slot < self.errors.len() {
                        self.errors[slot] = Some(error);
                    }
                }
                Note::Finished => finished = true,
            }
        }
        if !finished {
            self.job = Some(job);
        } else {
            let erred = self.errors.iter().any(|e| e.is_some());
            self.status = settled_status(job.stop.load(Ordering::Relaxed), erred).to_string();
        }
    }
}

/// What Develop says when the field does not name a file.
fn not_a_file(raw: &Path) -> String {
    if raw.as_os_str().is_empty() {
        "Open a RAW file first.".to_string()
    } else {
        format!("Not a file: {}", raw.display())
    }
}

/// The status line once a job has ended. It says ready only when every
/// pane came through, whatever the last progress text was. A pane's error
/// is shown under the line; the line itself does not say the work goes on.
fn settled_status(stopped: bool, erred: bool) -> &'static str {
    if stopped {
        "Stopped."
    } else if erred {
        "A pane could not be developed."
    } else {
        "Ready. Scroll zooms, drag pans, click the right pane for the next."
    }
}

/// The text of a caught panic.
fn panic_text(payload: &(dyn std::any::Any + Send)) -> String {
    payload
        .downcast_ref::<&str>()
        .map(|s| (*s).to_owned())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "no message".to_owned())
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        self.poll();
        let picking = matches!(self.dialog.state(), DialogState::Open)
            || matches!(self.tool_dialog.state(), DialogState::Open);
        // A drop while a dialog is up would open one file under the
        // dialog and leave the dialog to open another.
        if let Some(file) = ctx.input(|i| i.raw.dropped_files.iter().map(|f| f.path().to_path_buf()).next()) {
            if !picking {
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
                    Err(e) => format!("Using {}. The choice was not kept: {e:#}", path.display()),
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
        // A digit or an arrow typed into the path field is text.
        let typing = ctx.egui_wants_keyboard_input();
        ctx.input(|i| {
            if self.working() || picking {
                return;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::O) {
                open = true;
            }
            if typing {
                return;
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
        let title = window_title(self.shown.as_deref());
        if title != self.title {
            ctx.send_viewport_cmd(egui::ViewportCommand::Title(title.clone()));
            self.title = title;
        }

        egui::Panel::top("bar").show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(!self.working(), egui::Button::new("Open…"))
                    .on_hover_text("Pick a RAW file. Ctrl+O, or ⌘O. A file dropped on the window opens too.")
                    .clicked()
                {
                    self.open_dialog();
                }
                let edit = ui.add(
                    egui::TextEdit::singleline(&mut self.path).desired_width(420.0).hint_text("RAW file"),
                );
                if edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) && !self.working() {
                    self.develop(false);
                }
                if ui.add_enabled(!self.working(), egui::Button::new("Develop")).clicked() {
                    self.develop(false);
                }
                if ui.add_enabled(!self.working(), egui::Button::new("Develop again")).clicked() {
                    self.develop(true);
                }
                if ui
                    .add_enabled(self.working(), egui::Button::new("Stop"))
                    .on_hover_text("End this development. A demosaic of this program runs to its end first.")
                    .clicked()
                {
                    self.stop();
                }
                ui.separator();
                for algo in ALGOS {
                    let on = self.algo == algo;
                    let response =
                        ui.add_enabled(!self.working(), egui::Button::new(algo.label()).selected(on));
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
                    if ui.add_enabled(!self.working(), egui::Button::new(rival.label).selected(on)).clicked()
                    {
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
                // One pixel of the picture on one pixel of the screen: a
                // point is two or three of those on a dense display.
                let dense = ctx.pixels_per_point();
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
            if right_hit.clicked() && !right_hit.double_clicked() {
                self.which = (self.which + 1) % n;
            }
            paint(ui, left, self.algo.label(), self.textures[0].as_ref(), &self.view);
            let label = self.rivals().get(self.which).map(|r| r.label).unwrap_or("");
            let right_tex = self.textures.get(self.which + 1).and_then(|t| t.as_ref());
            paint(ui, right, label, right_tex, &self.view);
        });
    }

    // eframe's renderer decides the signature: OpenGL here, wgpu on Windows.
    #[cfg(not(target_os = "windows"))]
    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        self.end_job();
    }

    #[cfg(target_os = "windows")]
    fn on_exit(&mut self) {
        self.end_job();
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
        if ui.add_enabled(!self.working(), egui::Button::new(label)).on_hover_text(tip).clicked() {
            self.open_tool(which);
        }
    }

    fn open_tool(&mut self, which: WhichTool) {
        if self.working() {
            return;
        }
        self.tool_dialog.config_mut().initial_directory = tool_directory(self.tools.get(which));
        self.asking = Some(which);
        self.tool_dialog.pick_file();
    }

    fn frame(&self) -> Option<egui::Vec2> {
        let pic = self.textures[0]
            .as_ref()
            .or_else(|| self.textures.get(self.which + 1).and_then(|p| p.as_ref()))?;
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

/// Ends the marker of a folder whose pictures were not all written.
const UNFINISHED: &str = "\nunfinished";

/// What a work folder was developed from: the view, and the RAW's name,
/// length and date. Two files of one stem share the folder, and a RAW can
/// be replaced by another of its name.
fn stamp_of(raw: &Path) -> String {
    let name = raw.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let meta = std::fs::metadata(raw).ok();
    let length = meta.as_ref().map_or(0, std::fs::Metadata::len);
    let date = meta
        .and_then(|m| m.modified().ok())
        .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
        .map_or(0, |d| d.as_nanos());
    format!("{VIEW}\n{name}\n{length}\n{date}")
}

/// What a work folder still holds of the file being opened.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kept {
    /// Nothing to read: no marker, another file's pictures, an older
    /// view, or *Develop again*.
    Nothing,
    /// The plate of this file. The pictures were not all written.
    Plate,
    /// Every picture that is there.
    All,
}

fn kept(marker: Option<&str>, stamp: &str, again: bool) -> Kept {
    match marker {
        _ if again => Kept::Nothing,
        Some(text) if text == stamp => Kept::All,
        Some(text) if text.strip_suffix(UNFINISHED) == Some(stamp) => Kept::Plate,
        _ => Kept::Nothing,
    }
}

/// What a development that was cut off left behind: a tool's linear file,
/// and a file this program had not finished writing.
fn clear_leftovers(work: &Path) {
    let Ok(entries) = std::fs::read_dir(work) else { return };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        // A RawTherapee profile and the mask beside the negative are written
        // for the tool and never read again. Older versions left them here.
        if name.ends_with(".linear.tif")
            || name.ends_with(".part")
            || name.ends_with(".pp3")
            || name == "negative.mask.tif"
        {
            let _ = std::fs::remove_file(entry.path());
        }
    }
}

fn develop_all(raw: &Path, again: bool, mut algo: Algo, only_left: bool, tools: &Tools, out: &Out) {
    let work = work_dir(raw);
    if let Err(e) = std::fs::create_dir_all(&work) {
        out.missed(0, cannot_make(&work, &e));
        return;
    }
    let sensor = match sensor_of(raw) {
        Ok(sensor) => sensor,
        Err(e) => {
            out.missed(0, format!("{e:#}"));
            return;
        }
    };
    out.send(Note::Sensor(sensor));
    let orientation = camera_orientation(raw);
    if !algo.fits(sensor) {
        algo = sensor.home();
    }
    let rivals: &'static [Rival] = sensor.rivals();
    let marker = work.join("view.txt");
    let stamp = stamp_of(raw);
    let kept = kept(std::fs::read_to_string(&marker).ok().as_deref(), &stamp, again);
    // Rivals of a file that is no longer the one in the folder would stand
    // beside the new left pane.
    let only_left = only_left && kept != Kept::Nothing;
    clear_leftovers(&work);
    if !only_left && kept != Kept::All {
        let _ = std::fs::write(&marker, format!("{stamp}{UNFINISHED}"));
    }
    let shared = Shared { cv: Condvar::new(), meta: Mutex::new(None) };
    let failed = AtomicBool::new(false);
    let next = AtomicUsize::new(0);

    thread::scope(|scope| {
        scope.spawn(|| {
            let made = guarded(|| {
                let file = develop_ours(raw, &work, algo, kept, &shared, out)?;
                show(out, 0, algo.key(), &file, orientation)
            });
            if let Err(e) = made {
                let error = format!("{e:#}");
                // The rivals wait for the plate. It will not come.
                publish(&shared, Err(error.clone()));
                failed.store(true, Ordering::Relaxed);
                out.missed(0, error);
            }
        });
        if only_left {
            return;
        }
        // Two at a time. The rivals start at once and wait for the matrix
        // only when they apply it.
        for _ in 0..2 {
            scope.spawn(|| loop {
                let n = next.fetch_add(1, Ordering::Relaxed);
                let Some(rival) = rivals.get(n) else { break };
                if out.stopped() {
                    break;
                }
                let made = guarded(|| {
                    let file = develop_rival(raw, &work, rival, kept, tools, &shared, out)?;
                    show(out, n + 1, rival.key, &file, 1)
                });
                if let Err(e) = made {
                    failed.store(true, Ordering::Relaxed);
                    out.missed(n + 1, format!("{}: {e:#}", rival.label));
                }
            });
        }
    });
    if !only_left && !failed.load(Ordering::Relaxed) && !out.stopped() {
        let _ = std::fs::write(&marker, stamp);
    }
}

/// `work`, with a panic inside it as its error.
fn guarded<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    catch_unwind(AssertUnwindSafe(work))
        .unwrap_or_else(|payload| bail!("stopped: {}", panic_text(payload.as_ref())))
}

/// The left pane's file, developed or found, and the plate published for
/// the rivals on the way.
fn develop_ours(
    raw: &Path,
    work: &Path,
    algo: Algo,
    kept: Kept,
    shared: &Shared,
    out: &Out,
) -> Result<PathBuf> {
    let file = work.join(format!("{}.tif", algo.key()));
    let fresh = kept == Kept::All && file.is_file();
    let anew = kept == Kept::Nothing;
    let meta = match algo {
        Algo::Separation if fresh => {
            out.status("Reading the Mimizan separation…");
            read_plate(&work.join("negative.tif"))
        }
        Algo::Separation => {
            out.status("Developing the Mimizan separation…");
            show_mimizan(raw, work, &file, anew, out)
        }
        Algo::LeastSquares => {
            out.status("Reading the camera matrix…");
            ensure_plate(raw, work, anew, out)
        }
        Algo::Xtrans => {
            out.status("Reading the camera matrix…");
            xtrans_meta(raw)
        }
    };
    publish(shared, meta.as_ref().map(PlateMeta::clone).map_err(|e| format!("{e:#}")));
    let meta = meta?;
    if !fresh && algo != Algo::Separation {
        if out.stopped() {
            bail!("stopped before the demosaic");
        }
        match algo {
            Algo::LeastSquares => develop_least_squares(raw, &meta, &file, out)?,
            Algo::Xtrans => develop_xtrans(raw, &meta, &file, out)?,
            Algo::Separation => {}
        }
    }
    Ok(file)
}

/// A rival's file, developed by its tool or found.
fn develop_rival(
    raw: &Path,
    work: &Path,
    rival: &Rival,
    kept: Kept,
    tools: &Tools,
    shared: &Shared,
    out: &Out,
) -> Result<PathBuf> {
    let file = work.join(format!("{}.tif", rival.key));
    if kept == Kept::All && file.is_file() {
        out.status(format!("Reading {}…", rival.label));
        return Ok(file);
    }
    out.status(format!("Developing {}…", rival.label));
    let linear = work.join(format!("{}.linear.tif", rival.key));
    let _running = Running::new(&out.tools);
    let made = match rival.kind {
        Kind::Dht => develop_dht(raw, &linear, tools.dcraw(), out),
        Kind::Bayer(_) | Kind::Xtrans(_) => {
            develop_rawtherapee(raw, work, rival, &linear, tools.rawtherapee(), out)
        }
    }
    .and_then(|()| {
        let meta = wait_plate(shared)?;
        finish_demosaic(&linear, &meta, &file, !matches!(rival.kind, Kind::Dht))
    });
    let _ = std::fs::remove_file(&linear);
    made.map(|()| file)
}

struct Shared {
    cv: Condvar,
    meta: Mutex<Option<Result<PlateMeta, String>>>,
}

/// The plate for the rivals, or why there is none. The first word stands.
fn publish(shared: &Shared, meta: Result<PlateMeta, String>) {
    {
        let mut slot = shared.meta.lock().unwrap_or_else(PoisonError::into_inner);
        if slot.is_none() {
            *slot = Some(meta);
        }
    }
    shared.cv.notify_all();
}

fn wait_plate(shared: &Shared) -> Result<PlateMeta> {
    let mut slot = shared.meta.lock().unwrap_or_else(PoisonError::into_inner);
    loop {
        match slot.as_ref() {
            Some(Ok(meta)) => return Ok(meta.clone()),
            Some(Err(e)) => bail!("{e}"),
            None => slot = shared.cv.wait(slot).unwrap_or_else(PoisonError::into_inner),
        }
    }
}

/// Reads a developed file and puts it on the graphics card, on the job's
/// thread. Ours is the sensor as it was read and is turned here; the
/// rivals come back already turned, the way RawTherapee and dcraw show
/// the file.
fn show(out: &Out, slot: usize, name: &str, path: &Path, orientation: u8) -> Result<()> {
    let picture = load_picture(path)?;
    let (rgba, width, height) = turn(picture.rgba, picture.width, picture.height, orientation);
    let picture = Tiled::upload(&out.ctx, name, &Picture { width, height, rgba });
    out.send(Note::Ready { slot, picture });
    Ok(())
}

/// EXIF orientation, the same choice the negative uses: the raw IFD, and
/// the EXIF value when that IFD says the frame is not turned.
fn camera_orientation(path: &Path) -> u8 {
    mimizan_core::decode::raw_source(path).map_or(1, |source| orientation_of(&source))
}

fn orientation_of(source: &rawler::rawsource::RawSource) -> u8 {
    let Ok(decoder) = rawler::get_decoder(source) else {
        return 1;
    };
    let params = rawler::decoders::RawDecodeParams::default();
    let Ok(img) = decoder.raw_image(source, &params, true) else {
        return 1;
    };
    let exif = decoder.raw_metadata(source, &params).ok().and_then(|md| md.exif.orientation);
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
fn turn(px: Vec<u8>, width: usize, height: usize, orientation: u8) -> (Vec<u8>, usize, usize) {
    if orientation <= 1 || width == 0 || height == 0 || px.len() != width * height * 4 {
        return (px, width, height);
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

/// The plate already on disk is encoded again. `anew` develops it first.
fn show_mimizan(raw: &Path, work: &Path, file: &Path, anew: bool, out: &Out) -> Result<PlateMeta> {
    let negative = work.join("negative.tif");
    if anew || !negative.is_file() {
        return develop_mimizan(raw, work, file, out);
    }
    let (meta, gray) = read_plate_samples(&negative)?;
    write_display(file, meta.frame_w, meta.frame_h, &encode_plate(&gray, &meta))?;
    Ok(meta)
}

fn develop_mimizan(raw: &Path, work: &Path, file: &Path, out: &Out) -> Result<PlateMeta> {
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
    {
        let _running = Running::new(&out.tools);
        let ran = run(&mut cmd, "mimizan", "It belongs beside this program.", out);
        // The mask is for a print. This view reads the plate and nothing else.
        let _ = std::fs::remove_file(mimizan_core::print::sidecar_path(&negative));
        ran?;
    }
    let (meta, gray) = read_plate_samples(&negative)?;
    write_display(file, meta.frame_w, meta.frame_h, &encode_plate(&gray, &meta))?;
    Ok(meta)
}

fn read_plate(negative: &Path) -> Result<PlateMeta> {
    Ok(read_plate_samples(negative)?.0)
}

fn develop_rawtherapee(
    raw: &Path,
    work: &Path,
    rival: &Rival,
    file: &Path,
    bin: &Path,
    out: &Out,
) -> Result<()> {
    let (bayer, xtrans) = match rival.kind {
        Kind::Bayer(method) => (method, "3-pass (best)"),
        Kind::Xtrans(method) => ("rcd", method),
        Kind::Dht => bail!("DHT is developed by dcraw"),
    };
    let pp3 = work.join(format!("rt_{}.pp3", rival.key));
    std::fs::write(&pp3, rt_profile(bayer, xtrans))?;
    let mut cmd = Command::new(bin);
    cmd.args(["-q", "-Y", "-t", "-b16", "-o"]).arg(file).arg("-p").arg(&pp3).arg("-c").arg(raw);
    let name = bin.display().to_string();
    let ran = run(&mut cmd, &name, "RawTherapee… chooses the program.", out);
    // The profile was the tool's instructions. Nothing reads it afterwards.
    let _ = std::fs::remove_file(&pp3);
    ran?;
    if !file.is_file() {
        bail!("{name} wrote no {}", file.display());
    }
    Ok(())
}

/// The negative's matrix, developed when the folder does not have it yet.
fn ensure_plate(raw: &Path, work: &Path, anew: bool, out: &Out) -> Result<PlateMeta> {
    let negative = work.join("negative.tif");
    if anew || !negative.is_file() {
        develop_mimizan(raw, work, &work.join("mimizan.tif"), out)
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

fn develop_least_squares(raw: &Path, meta: &PlateMeta, file: &Path, out: &Out) -> Result<()> {
    out.status("Developing the least-squares demosaic…");
    let img = load_image(raw, false)?;
    let dev = mimizan_cli::bayer_ls::develop(&img, &|s| out.status(s)).map_err(anyhow::Error::msg)?;
    let rgb = encode_balanced(&dev.data, dev.frame.width, dev.frame.height, dev.balance, meta.rows);
    write_display(file, dev.frame.width, dev.frame.height, &rgb)
}

fn develop_xtrans(raw: &Path, meta: &PlateMeta, file: &Path, out: &Out) -> Result<()> {
    out.status("Developing the X-Trans demosaic…");
    let img = load_image(raw, false)?;
    let dev = mimizan_lab_xtrans::convert::develop_watched(
        &img,
        &mimizan_lab_xtrans::convert::Settings { keep_highlights: true },
        &|s| out.status(s),
        &|share| {
            out.status(format!("separating luminance and colour, {:.0}%", share.clamp(0.0, 1.0) * 100.0));
        },
    )
    .map_err(anyhow::Error::msg)?;
    let rgb = encode_balanced(&dev.data, dev.frame.width, dev.frame.height, dev.balance, meta.rows);
    write_display(file, dev.frame.width, dev.frame.height, &rgb)
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

fn develop_dht(raw: &Path, file: &Path, bin: &Path, out: &Out) -> Result<()> {
    let mut cmd = Command::new(bin);
    // Linear 16-bit (-4), camera channels (-o 0), camera white balance,
    // highlights clipped. -4 already refuses the automatic brightness.
    cmd.args(["-4", "-o", "0", "-w", "-H", "0", "-q", "11", "-T", "-Z"]).arg(file).arg(raw);
    let name = bin.display().to_string();
    run(&mut cmd, &name, "dcraw… chooses the program.", out)?;
    if !file.is_file() {
        bail!("{name} wrote no {}", file.display());
    }
    Ok(())
}

/// Runs a tool to its end, or ends it when the job is stopped. `advice`
/// is what to do when the program is not there.
fn run(cmd: &mut Command, name: &str, advice: &str, out: &Out) -> Result<()> {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // CREATE_NO_WINDOW. A console program started by a window program
        // opens a console of its own otherwise, pipes or not.
        cmd.creation_flags(0x0800_0000);
    }
    let mut child = match cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn() {
        Ok(child) => child,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => bail!("{name} was not found. {advice}"),
        Err(e) => bail!("{name} could not be started: {e}"),
    };
    // Read as they are written: a tool that fills a pipe nobody reads
    // waits for ever.
    let stdout = drain(child.stdout.take());
    let stderr = drain(child.stderr.take());
    let status = loop {
        if let Some(status) = child.try_wait().with_context(|| name.to_owned())? {
            break status;
        }
        if out.stopped() {
            let _ = child.kill();
            let _ = child.wait();
            bail!("{name} was stopped");
        }
        thread::sleep(Duration::from_millis(40));
    };
    if status.success() {
        return Ok(());
    }
    let mut text = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
    if text.trim().is_empty() {
        text = String::from_utf8_lossy(&stdout.join().unwrap_or_default()).into_owned();
    }
    bail!("{name} failed: {}", tail_of(text.trim(), 1200));
}

fn drain(pipe: Option<impl std::io::Read + Send + 'static>) -> thread::JoinHandle<Vec<u8>> {
    thread::spawn(move || {
        let mut bytes = Vec::new();
        if let Some(mut pipe) = pipe {
            let _ = pipe.read_to_end(&mut bytes);
        }
        bytes
    })
}

/// The end of `text`, at most `most` bytes of it, cut between two
/// characters.
fn tail_of(text: &str, most: usize) -> &str {
    let mut at = text.len().saturating_sub(most);
    while !text.is_char_boundary(at) {
        at += 1;
    }
    &text[at..]
}

fn load_picture(path: &Path) -> Result<Picture> {
    let image = image::open(path).with_context(|| format!("reading {}", path.display()))?.into_rgba8();
    let (width, height) = image.dimensions();
    Ok(Picture { width: width as usize, height: height as usize, rgba: image.into_raw() })
}

/// The picture beside the file list. The camera's embedded preview, kept
/// for the files already seen this time. One thread reads them and takes
/// the newest request: the selection moves faster than a card is read.
struct Preview {
    shown: PathBuf,
    texture: Option<egui::TextureHandle>,
    missing: bool,
    asks: mpsc::Sender<PathBuf>,
    rx: Receiver<Loaded>,
    cache: Vec<(PathBuf, Cached)>,
}

struct Loaded {
    path: PathBuf,
    picture: Option<Picture>,
}

#[derive(Clone)]
enum Cached {
    Image(egui::TextureHandle),
    Missing,
}

impl Preview {
    fn new(ctx: egui::Context) -> Self {
        let (asks, asked) = mpsc::channel::<PathBuf>();
        let (tx, rx) = mpsc::channel();
        thread::spawn(move || {
            while let Ok(mut path) = asked.recv() {
                while let Ok(newer) = asked.try_recv() {
                    path = newer;
                }
                // A file a decoder stops on has no preview.
                let picture = catch_unwind(|| load_preview(&path)).ok().flatten();
                if tx.send(Loaded { path, picture }).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        });
        Self { shown: PathBuf::new(), texture: None, missing: false, asks, rx, cache: Vec::new() }
    }

    fn panel(&mut self, ui: &mut egui::Ui, ctx: &egui::Context, dia: &FileDialog) {
        let Some(entry) = dia.selected_entry().filter(|e| e.is_file()) else {
            ui.label("Select a file.");
            return;
        };
        let path = entry.to_path_buf();
        ui.label(entry.file_name());
        self.poll(ctx);
        self.ensure(&path);
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
        }
    }

    fn ensure(&mut self, path: &Path) {
        if self.shown == path {
            return;
        }
        self.shown = path.to_path_buf();
        self.texture = None;
        self.missing = false;
        match self.cache.iter().find(|(p, _)| p == path).map(|(_, cached)| cached.clone()) {
            Some(cached) => self.set(cached),
            // The reader is gone only with the program.
            None => self.missing = self.asks.send(path.to_path_buf()).is_err(),
        }
    }

    fn set(&mut self, cached: Cached) {
        match cached {
            Cached::Image(texture) => {
                self.texture = Some(texture);
                self.missing = false;
            }
            Cached::Missing => {
                self.texture = None;
                self.missing = true;
            }
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        while let Ok(loaded) = self.rx.try_recv() {
            let cached = match loaded.picture {
                Some(picture) => Cached::Image(ctx.load_texture(
                    loaded.path.display().to_string(),
                    egui::ColorImage::from_rgba_unmultiplied([picture.width, picture.height], &picture.rgba),
                    egui::TextureOptions::LINEAR,
                )),
                None => Cached::Missing,
            };
            if loaded.path == self.shown {
                self.set(cached.clone());
            }
            self.remember(loaded.path, cached);
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
    let (image, orientation) = if is_raw_path(path) {
        let source = mimizan_core::decode::raw_source(path).ok()?;
        (raw_preview(&source)?, orientation_of(&source))
    } else {
        (image::open(path).ok()?, 1)
    };
    let rgba = image.into_rgba8();
    let (width, height) = rgba.dimensions();
    let (mut rgba, mut width, mut height) = (rgba.into_raw(), width as usize, height as usize);
    if matches!(orientation, 5..=8) && width > height {
        (rgba, width, height) = turn(rgba, width, height, orientation);
    }
    Some(fit_preview(rgba, width, height, 480))
}

fn raw_preview(source: &rawler::rawsource::RawSource) -> Option<image::DynamicImage> {
    // The largest embedded JPEG is often the full frame. The dialog wants
    // the small one, which a camera keeps beside the raw.
    if let Some(bytes) = embedded_jpeg(source.buf()) {
        if let Ok(image) = image::load_from_memory(bytes) {
            return Some(image);
        }
    }
    let decoder = rawler::get_decoder(source).ok()?;
    let params = rawler::decoders::RawDecodeParams::default();
    decoder.thumbnail_image(source, &params).ok().flatten()
}

/// The largest JPEG in the file that is still a preview: at least 8 KB,
/// at most 512 KB. Bigger than that is the full frame.
fn embedded_jpeg(bytes: &[u8]) -> Option<&[u8]> {
    let mut best: Option<std::ops::Range<usize>> = None;
    let mut i = 0;
    while i + 1 < bytes.len() {
        if bytes[i] == 0xFF && bytes[i + 1] == 0xD8 {
            if let Some(end) = jpeg_end(bytes, i) {
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
    best.map(|range| &bytes[range])
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
    dialog_directory(Path::new(""))
}

/// Where the choice is kept: with the darkroom's settings. An installed
/// program cannot write beside itself.
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
fn dialog_directory(path: &Path) -> PathBuf {
    if path.is_dir() {
        return path.to_path_buf();
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

fn window_title(shown: Option<&Path>) -> String {
    match shown.and_then(Path::file_name) {
        Some(name) => format!("{TITLE} — {}", name.to_string_lossy()),
        None => TITLE.to_string(),
    }
}

fn is_raw_path(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RAW_EXTENSIONS.iter().any(|ext| e.eq_ignore_ascii_case(ext)))
}

/// The status for a work folder that could not be made: the folder, the
/// system's reason, and that the pictures are kept beside the RAW.
fn cannot_make(work: &Path, err: &std::io::Error) -> String {
    format!("{} cannot be made: {err}. The loupe keeps its pictures beside the RAW.", work.display())
}

/// `<stem>.mimizan-compare` beside the RAW. The stem is the file's own
/// name, bytes and all: two files that are not UTF-8 do not share a folder.
fn work_dir(raw: &Path) -> PathBuf {
    let mut name = raw.file_stem().map(std::ffi::OsStr::to_os_string).unwrap_or_default();
    name.push(".mimizan-compare");
    let parent = raw.parent().unwrap_or_else(|| Path::new("."));
    parent.join(name)
}

/// The command that develops the plate: `MIMIZAN`, the one that came
/// with this program, or the name on the path.
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

/// Most bytes of samples a file in the work folder can hold: the largest
/// sheet the program writes, three 16-bit channels. A file that claims
/// more is not one of ours or a tool's, and is refused with an error and
/// not with an allocation the machine cannot make.
const SAMPLE_BYTES_MAX: usize = mimizan_core::recipe::SHEET_MAX_PX.saturating_mul(3 * 2);

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

/// An 8-bit RGB TIFF, there whole or not at all: a file cut off half way
/// would be read as a picture the next time.
fn write_display(path: &Path, width: usize, height: usize, rgb: &[u8]) -> Result<()> {
    let (w, h) = (u32::try_from(width)?, u32::try_from(height)?);
    mimizan_core::outfile::write_whole(path, |sink| {
        image::write_buffer_with_format(
            sink,
            rgb,
            w,
            h,
            image::ExtendedColorType::Rgb8,
            image::ImageFormat::Tiff,
        )
        .with_context(|| format!("writing {}", path.display()))
    })
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

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("mimizan-compare-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn out_for_a_test() -> (Out, Receiver<Note>) {
        let (tx, rx) = mpsc::channel();
        let out = Out {
            tx,
            ctx: egui::Context::default(),
            stop: Arc::new(AtomicBool::new(false)),
            tools: Arc::new(AtomicUsize::new(0)),
        };
        (out, rx)
    }

    /// A tool's message is cut between two characters, wherever the cut
    /// falls: cutting inside one stopped the thread, and with it the job.
    #[test]
    fn the_end_of_a_tools_message_is_cut_between_characters() {
        assert_eq!(tail_of("short", 1200), "short");
        assert_eq!(tail_of("abcdef", 3), "def");
        let text = "Datei nicht gefunden: Übung für Größe…";
        for most in 0..text.len() + 2 {
            let tail = tail_of(text, most);
            assert!(tail.len() <= most && text.ends_with(tail), "{most}");
            assert!(text.len() - tail.len() >= text.len().saturating_sub(most));
        }
        assert_eq!(tail_of("aé", 1), "", "half a character is no character");
    }

    /// The folder is read only for the file it was developed from. The
    /// same name with another length or date, another file of the same
    /// stem, and an older view are developed; a folder that was not
    /// finished keeps its plate and no more.
    #[test]
    fn a_work_folder_is_read_only_for_the_file_it_was_made_from() {
        let dir = scratch("stamp");
        let nef = dir.join("photo.NEF");
        let dng = dir.join("photo.DNG");
        std::fs::write(&nef, b"one").unwrap();
        std::fs::write(&dng, b"one").unwrap();
        assert_eq!(work_dir(&nef), work_dir(&dng), "one folder for both");
        assert_eq!(work_dir(&nef).file_name().and_then(|n| n.to_str()), Some("photo.mimizan-compare"));
        let stamp = stamp_of(&nef);
        assert_ne!(stamp, stamp_of(&dng));
        assert_eq!(kept(Some(&stamp), &stamp, false), Kept::All);
        assert_eq!(kept(Some(&stamp), &stamp, true), Kept::Nothing, "Develop again");
        assert_eq!(kept(Some(&format!("{stamp}{UNFINISHED}")), &stamp, false), Kept::Plate);
        assert_eq!(kept(Some(&stamp_of(&dng)), &stamp, false), Kept::Nothing);
        assert_eq!(kept(Some(VIEW), &stamp, false), Kept::Nothing, "a folder of 0.20.15");
        assert_eq!(kept(None, &stamp, false), Kept::Nothing);
        std::fs::write(&nef, b"another picture").unwrap();
        assert_eq!(kept(Some(&stamp), &stamp_of(&nef), false), Kept::Nothing, "the RAW was replaced");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Two files whose names are not text each get the folder of their own stem.
    #[cfg(unix)]
    #[test]
    fn a_name_that_is_not_utf8_keeps_its_own_work_folder() {
        use std::os::unix::ffi::OsStrExt;
        let dir = scratch("stem");
        let raw = |byte: u8| {
            let mut name = std::ffi::OsString::from("shot");
            name.push(std::ffi::OsStr::from_bytes(&[byte]));
            name.push(".NEF");
            dir.join(name)
        };
        let first = work_dir(&raw(0xff));
        let second = work_dir(&raw(0xfe));
        assert_ne!(first, second);
        assert_eq!(first.file_name().unwrap().as_bytes(), b"shot\xff.mimizan-compare");
        assert_eq!(second.file_name().unwrap().as_bytes(), b"shot\xfe.mimizan-compare");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_status_line_says_ready_only_when_every_pane_came_through() {
        let ready = "Ready. Scroll zooms, drag pans, click the right pane for the next.";
        assert_eq!(settled_status(false, false), ready);
        assert_eq!(settled_status(true, false), "Stopped.");
        assert_eq!(settled_status(true, true), "Stopped.");
        assert_eq!(settled_status(false, true), "A pane could not be developed.");
    }

    #[test]
    fn an_empty_field_and_a_folder_are_named_as_not_a_file() {
        assert_eq!(not_a_file(Path::new("")), "Open a RAW file first.");
        assert_eq!(not_a_file(Path::new("/tmp/no-such-raw")), "Not a file: /tmp/no-such-raw");
        let dir = scratch("notfile");
        assert_eq!(not_a_file(&dir), format!("Not a file: {}", dir.display()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_work_folder_that_cannot_be_made_names_the_folder_and_the_reason() {
        let dir = scratch("nowork");
        let work = work_dir(&dir.join("photo.NEF"));
        std::fs::write(&work, b"not a directory").unwrap();
        let err = std::fs::create_dir_all(&work).unwrap_err();
        let said = cannot_make(&work, &err);
        assert_eq!(
            said,
            format!("{} cannot be made: {err}. The loupe keeps its pictures beside the RAW.", work.display())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn what_a_cut_off_development_left_is_cleared_and_the_pictures_stay() {
        let dir = scratch("leftovers");
        for name in [
            "rcd.linear.tif",
            "amaze.tif.part",
            "rt_rcd.pp3",
            "negative.mask.tif",
            "rcd.tif",
            "negative.tif",
            "view.txt",
        ] {
            std::fs::write(dir.join(name), b"x").unwrap();
        }
        clear_leftovers(&dir);
        let mut left: Vec<_> =
            std::fs::read_dir(&dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
        left.sort();
        assert_eq!(left, ["negative.tif", "rcd.tif", "view.txt"]);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The Mac download puts the loupe beside the app, and the command is
    /// inside the app.
    #[test]
    fn the_command_is_found_beside_the_loupe_and_inside_the_mac_app() {
        let dir = scratch("beside");
        assert_eq!(mimizan_beside(&dir), None);
        let inside = dir.join("Mimizan Lab.app").join("Contents").join("MacOS");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::write(inside.join("mimizan"), b"").unwrap();
        assert_eq!(mimizan_beside(&dir), Some(inside.join("mimizan")));
        let resources = dir.join("Mimizan Lab.app").join("Contents").join("Resources").join("cameras");
        std::fs::create_dir_all(&resources).unwrap();
        let found = find_cameras(&inside.join("mimizan")).expect("the app's camera files");
        assert_eq!(found.canonicalize().unwrap(), resources.canonicalize().unwrap());
        std::fs::write(dir.join("mimizan"), b"").unwrap();
        assert_eq!(mimizan_beside(&dir), Some(dir.join("mimizan")), "next to it comes first");
        assert_eq!(find_cameras(Path::new("mimizan")), None, "a command on the path looks for its own");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A rival waits for the plate. When the plate fails, or the thread
    /// that makes it stops, the rival gets the reason and does not wait
    /// for ever.
    #[test]
    fn a_rival_is_told_when_the_plate_will_not_come() {
        let shared = Shared { cv: Condvar::new(), meta: Mutex::new(None) };
        thread::scope(|scope| {
            let waiting = scope.spawn(|| wait_plate(&shared));
            let made: Result<()> = guarded(|| panic!("a decoder gave up"));
            let error = format!("{:#}", made.unwrap_err());
            assert_eq!(error, "stopped: a decoder gave up");
            publish(&shared, Err(error));
            let told = waiting.join().unwrap().err().expect("no plate").to_string();
            assert_eq!(told, "stopped: a decoder gave up");
        });
        let meta = PlateMeta {
            range: 1.0,
            rows: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]],
            frame_w: 2,
            frame_h: 2,
            crop: Crop { x: 0, y: 0, w: 2, h: 2 },
        };
        publish(&shared, Ok(meta));
        assert!(wait_plate(&shared).is_err(), "the first word stands");
    }

    /// A tool that is not installed says so and what to do. A tool that
    /// runs when the job is stopped is ended, and does not run on.
    #[test]
    fn a_missing_tool_is_named_and_a_running_one_is_ended() {
        let (out, _rx) = out_for_a_test();
        let mut missing = Command::new("mimizan-no-such-tool-anywhere");
        let said = run(&mut missing, "rawtherapee-cli", "RawTherapee… chooses the program.", &out)
            .unwrap_err()
            .to_string();
        assert_eq!(said, "rawtherapee-cli was not found. RawTherapee… chooses the program.");
        if cfg!(unix) {
            let mut fails = Command::new("sh");
            fails.args(["-c", "echo Größe >&2; exit 3"]);
            let said = run(&mut fails, "tool", "", &out).unwrap_err().to_string();
            assert_eq!(said, "tool failed: Größe");
            let mut sleeps = Command::new("sleep");
            sleeps.arg("30");
            let stop = Arc::clone(&out.stop);
            let stopper = thread::spawn(move || {
                thread::sleep(Duration::from_millis(150));
                stop.store(true, Ordering::Relaxed);
            });
            let began = Instant::now();
            let said = run(&mut sleeps, "tool", "", &out).unwrap_err().to_string();
            stopper.join().unwrap();
            assert_eq!(said, "tool was stopped");
            assert!(began.elapsed() < Duration::from_secs(10));
        }
    }

    /// The window is told that a job is over even when the job's thread
    /// is lost, and a count of running tools comes back to none.
    #[test]
    fn a_running_tool_is_counted_until_it_is_cleared_away() {
        let count = AtomicUsize::new(0);
        {
            let _one = Running::new(&count);
            let _two = Running::new(&count);
            assert_eq!(count.load(Ordering::SeqCst), 2);
        }
        assert_eq!(count.load(Ordering::SeqCst), 0);
        let unwound = catch_unwind(AssertUnwindSafe(|| {
            let _one = Running::new(&count);
            panic!("stopped half way");
        }));
        assert!(unwound.is_err());
        assert_eq!(count.load(Ordering::SeqCst), 0);
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
        let (out, w, h) = turn(px.to_vec(), 2, 1, 8);
        assert_eq!((w, h), (1, 2));
        assert_eq!(out, [0, 255, 0, 255, 255, 0, 0, 255]);
        let (same, w, h) = turn(px.to_vec(), 2, 1, 1);
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
        assert_eq!(dialog_directory(&file), dir);
        assert_eq!(dialog_directory(&dir), dir);
        assert!(dialog_directory(Path::new("")).is_dir());
        assert_eq!(window_title(None), "Mimizan compare");
        assert_eq!(window_title(Some(Path::new("/photos/DSC.NEF"))), "Mimizan compare — DSC.NEF");
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
        let gray = vec![0u16; 6 * 12];
        let rgb = encode_plate(&gray, &meta);
        assert_eq!(rgb.len(), 6 * 4 * 3);
        assert!(rgb.iter().all(|&v| v == 0));
        let bare = r#"{"report":{"crop":{"x":8,"y":4,"w":6,"h":4}},"plate":{"frames":["blue","green","red"],"frame_height":4,"weights":[[1,0,0],[0,1,0],[0,0,1]]}}"#;
        assert_eq!(plate_meta(bare, 6, 12).unwrap().range, 1.0);
    }
}
