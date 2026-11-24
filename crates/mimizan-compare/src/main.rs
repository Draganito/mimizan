//! A loupe for one RAW file.
//!
//! The left pane is the Mimizan base plate print. The right pane is one of
//! the five demosaicers that sit closest to the least-squares separation
//! behind a lens: RCD, LMMSE, DCB, AMaZE, then DHT. Scroll zooms both panes
//! together, a drag pans both, a click on the right pane steps to the next
//! demosaicer.
//!
//! ```text
//! mimizan-compare photo.NEF
//! ```
//!
//! With no path, Open picks the file. A path on the command line still
//! develops at once, and a file dropped on the window does too.
//!
//! The developed TIFFs land in `photo.mimizan-compare/` beside the RAW and
//! are kept. A second open reads them. *Develop again* writes them over.
//!
//! Mimizan is `mimizan negative --plate camera` and `mimizan print` at the
//! base paper, 8-bit, with the sharpening off. The four RawTherapee
//! demosaicers use a neutral profile, camera white balance and an sRGB
//! TIFF. DHT is dcraw_emu at the camera white balance in sRGB.

#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
#![forbid(unsafe_code)]

use anyhow::{bail, Context, Result};
use egui_file_dialog::{DialogState, FileDialog, OpeningMode};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver};
use std::thread;

/// The five, in the order the lens measurement ranked them.
const RIVALS: &[Rival] = &[
    Rival { key: "rcd", label: "RCD", kind: Kind::RawTherapee("rcd") },
    Rival { key: "lmmse", label: "LMMSE", kind: Kind::RawTherapee("lmmse") },
    Rival { key: "dcb", label: "DCB", kind: Kind::RawTherapee("dcb") },
    Rival { key: "amaze", label: "AMaZE", kind: Kind::RawTherapee("amaze") },
    Rival { key: "dht", label: "DHT", kind: Kind::Dht },
];

struct Rival {
    key: &'static str,
    label: &'static str,
    kind: Kind,
}

enum Kind {
    RawTherapee(&'static str),
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
    Ready { slot: usize, picture: Picture },
    Missed { slot: usize, error: String },
    Finished,
}

/// `slot` 0 is Mimizan. The rivals follow in [`RIVALS`] order.
struct App {
    path: String,
    dialog: FileDialog,
    view: View,
    fitted: bool,
    which: usize,
    pictures: Vec<Option<Picture>>,
    textures: Vec<Option<egui::TextureHandle>>,
    errors: Vec<Option<String>>,
    status: String,
    working: bool,
    rx: Option<Receiver<Note>>,
}

struct View {
    /// Screen points per pixel of the Mimizan picture.
    mag: f32,
    pan: egui::Vec2,
}

fn main() -> eframe::Result {
    let initial = std::env::args_os().nth(1).map(|p| PathBuf::from(p).display().to_string()).unwrap_or_default();
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
            dialog: file_dialog(),
            view: View { mag: 1.0, pan: egui::Vec2::ZERO },
            fitted: false,
            which: 0,
            pictures: (0..n).map(|_| None).collect(),
            textures: (0..n).map(|_| None).collect(),
            errors: (0..n).map(|_| None).collect(),
            status: "Open a RAW file, or drop one here.".into(),
            working: false,
            rx: None,
        };
        if !app.path.is_empty() {
            app.develop(false);
        }
        app
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
        self.textures.fill(None);
        self.errors.fill(None);
        self.status = "Developing…".into();
        let (tx, rx) = mpsc::channel();
        self.rx = Some(rx);
        thread::spawn(move || develop_all(&raw, again, tx));
    }

    fn poll(&mut self, ctx: &egui::Context) {
        let Some(rx) = &self.rx else { return };
        let mut finished = false;
        while let Ok(note) = rx.try_recv() {
            match note {
                Note::Status(s) => self.status = s,
                Note::Ready { slot, picture } => {
                    let name = if slot == 0 { "mimizan" } else { RIVALS[slot - 1].key };
                    self.textures[slot] = Some(ctx.load_texture(
                        name,
                        egui::ColorImage::from_rgba_unmultiplied(
                            [picture.width, picture.height],
                            &picture.rgba,
                        ),
                        egui::TextureOptions {
                            magnification: egui::TextureFilter::Nearest,
                            minification: egui::TextureFilter::Linear,
                            ..Default::default()
                        },
                    ));
                    self.pictures[slot] = Some(picture);
                    self.errors[slot] = None;
                }
                Note::Missed { slot, error } => self.errors[slot] = Some(error),
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
        self.dialog.update(&ctx);
        if let Some(path) = self.dialog.take_picked() {
            self.take_file(path);
        }
        let picking = matches!(self.dialog.state(), DialogState::Open);
        let mut open = false;
        ctx.input(|i| {
            if self.working || picking {
                return;
            }
            if i.modifiers.command && i.key_pressed(egui::Key::O) {
                open = true;
            }
            if i.key_pressed(egui::Key::ArrowRight) {
                self.which = (self.which + 1) % RIVALS.len();
            }
            if i.key_pressed(egui::Key::ArrowLeft) {
                self.which = (self.which + RIVALS.len() - 1) % RIVALS.len();
            }
            for (n, key) in [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3, egui::Key::Num4, egui::Key::Num5]
                .into_iter()
                .enumerate()
            {
                if i.key_pressed(key) {
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
                let edit = ui.add(egui::TextEdit::singleline(&mut self.path).desired_width(420.0).hint_text("RAW file"));
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
                for (n, rival) in RIVALS.iter().enumerate() {
                    let on = self.which == n;
                    if ui.add_enabled(!self.working, egui::Button::new(rival.label).selected(on)).clicked() {
                        self.which = n;
                    }
                }
                ui.separator();
                if ui.button("Fit").clicked() {
                    self.fitted = false;
                }
                if ui.button("1:1").clicked() {
                    self.view.mag = 1.0;
                    self.view.pan = egui::Vec2::ZERO;
                    self.fitted = true;
                }
                if let Some(pic) = &self.pictures[0] {
                    let pct = self.view.mag * 100.0;
                    ui.label(format!("{pct:.0}%  {}×{}", pic.width, pic.height));
                }
            });
            ui.label(&self.status);
            if let Some(err) = &self.errors[0] {
                ui.colored_label(egui::Color32::from_rgb(190, 70, 60), err);
            }
            if let Some(err) = &self.errors[self.which + 1] {
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
            if right_hit.clicked() && !right_hit.double_clicked() {
                self.which = (self.which + 1) % RIVALS.len();
            }
            let frame = self.frame();
            paint(ui, left, "Mimizan", self.textures[0].as_ref(), frame, &self.view);
            let label = RIVALS[self.which].label;
            paint(ui, right, label, self.textures[self.which + 1].as_ref(), frame, &self.view);
        });
    }
}

impl App {
    fn frame(&self) -> Option<egui::Vec2> {
        let pic = self.pictures[0].as_ref().or_else(|| self.pictures.get(self.which + 1).and_then(|p| p.as_ref()))?;
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

fn paint(
    ui: &egui::Ui,
    pane: egui::Rect,
    title: &str,
    texture: Option<&egui::TextureHandle>,
    frame: Option<egui::Vec2>,
    view: &View,
) {
    let painter = ui.painter_at(pane);
    painter.rect_filled(pane, 0.0, egui::Color32::from_rgb(18, 18, 18));
    if let (Some(texture), Some(frame)) = (texture, frame) {
        let shown = frame * view.mag;
        let dest = egui::Rect::from_min_size(origin(pane, shown, view.pan), shown);
        let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
        painter.image(texture.id(), dest, uv, egui::Color32::WHITE);
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

fn develop_all(raw: &Path, again: bool, tx: mpsc::Sender<Note>) {
    let work = work_dir(raw);
    if let Err(e) = std::fs::create_dir_all(&work) {
        let _ = tx.send(Note::Missed { slot: 0, error: e.to_string() });
        let _ = tx.send(Note::Finished);
        return;
    }
    let mimizan_tif = work.join("mimizan.tif");

    thread::scope(|scope| {
        scope.spawn(|| {
            let tx_m = tx.clone();
            if !again && mimizan_tif.is_file() {
                let _ = tx_m.send(Note::Status("Reading the Mimizan print…".into()));
            } else {
                let _ = tx_m.send(Note::Status("Developing the Mimizan print…".into()));
                if let Err(e) = develop_mimizan(raw, &work, &mimizan_tif) {
                    let _ = tx_m.send(Note::Missed { slot: 0, error: e.to_string() });
                    return;
                }
            }
            send_picture(&tx_m, 0, &mimizan_tif);
        });
        // Two at a time. Five RawTherapee processes beside the print
        // thrash a small machine.
        let (jobs, job_rx) = mpsc::channel::<usize>();
        for n in 0..RIVALS.len() {
            let _ = jobs.send(n);
        }
        drop(jobs);
        let job_rx = std::sync::Arc::new(std::sync::Mutex::new(job_rx));
        for _ in 0..2 {
            let job_rx = std::sync::Arc::clone(&job_rx);
            let tx_r = tx.clone();
            let work = work.clone();
            let raw = raw.to_path_buf();
            scope.spawn(move || {
                loop {
                    let n = {
                        let Ok(guard) = job_rx.lock() else { break };
                        let Ok(n) = guard.recv() else { break };
                        n
                    };
                    let rival = &RIVALS[n];
                    let slot = n + 1;
                    let out = work.join(format!("{}.tif", rival.key));
                    if !again && out.is_file() {
                        let _ = tx_r.send(Note::Status(format!("Reading {}…", rival.label)));
                    } else {
                        let _ = tx_r.send(Note::Status(format!("Developing {}…", rival.label)));
                        let run = match rival.kind {
                            Kind::RawTherapee(method) => develop_rawtherapee(&raw, &work, method, &out),
                            Kind::Dht => develop_dht(&raw, &out),
                        };
                        if let Err(e) = run {
                            let _ = tx_r.send(Note::Missed { slot, error: format!("{}: {e}", rival.label) });
                            continue;
                        }
                    }
                    send_picture(&tx_r, slot, &out);
                }
            });
        }
    });
    let _ = tx.send(Note::Finished);
}

fn send_picture(tx: &mpsc::Sender<Note>, slot: usize, path: &Path) {
    match load_picture(path) {
        Ok(picture) => {
            let _ = tx.send(Note::Ready { slot, picture });
        }
        Err(e) => {
            let _ = tx.send(Note::Missed { slot, error: e.to_string() });
        }
    }
}

fn develop_mimizan(raw: &Path, work: &Path, out: &Path) -> Result<()> {
    let bin = find_mimizan();
    let negative = work.join("negative.tif");
    let cameras = find_cameras();
    let mut cmd = Command::new(&bin);
    cmd.arg("--log").arg("warn").arg("negative").arg(raw).arg("-o").arg(&negative).arg("--plate").arg("camera");
    if let Some(dir) = &cameras {
        cmd.arg("--cameras").arg(dir);
    }
    run(&mut cmd, "mimizan negative")?;
    let mut cmd = Command::new(&bin);
    cmd.arg("--log")
        .arg("warn")
        .arg("print")
        .arg(&negative)
        .arg("-o")
        .arg(out)
        .arg("--paper")
        .arg("0,0")
        .arg("--usm-amount")
        .arg("0")
        .arg("--bits")
        .arg("8");
    if let Some(dir) = &cameras {
        cmd.arg("--cameras").arg(dir);
    }
    run(&mut cmd, "mimizan print")?;
    if !out.is_file() {
        bail!("mimizan print wrote no {}", out.display());
    }
    Ok(())
}

fn develop_rawtherapee(raw: &Path, work: &Path, method: &str, out: &Path) -> Result<()> {
    let pp3 = work.join(format!("rt_{method}.pp3"));
    std::fs::write(&pp3, rt_profile(method))?;
    let mut cmd = Command::new("rawtherapee-cli");
    cmd.args(["-q", "-Y", "-t", "-b8", "-o"]).arg(out).arg("-p").arg(&pp3).arg("-c").arg(raw);
    run(&mut cmd, "rawtherapee-cli")?;
    if !out.is_file() {
        bail!("rawtherapee-cli wrote no {}", out.display());
    }
    Ok(())
}

fn develop_dht(raw: &Path, out: &Path) -> Result<()> {
    let suffix = ".mimizan-compare-dht.tif";
    let mut name = raw.as_os_str().to_os_string();
    name.push(suffix);
    let produced = PathBuf::from(name);
    let mut cmd = Command::new("dcraw_emu");
    cmd.args(["-w", "-o", "1", "-H", "0", "-q", "11", "-T", "-Z", suffix]).arg(raw);
    run(&mut cmd, "dcraw_emu")?;
    if !produced.is_file() {
        bail!("dcraw_emu wrote no {}", produced.display());
    }
    std::fs::rename(&produced, out).with_context(|| format!("moving {} to {}", produced.display(), out.display()))?;
    Ok(())
}

fn run(cmd: &mut Command, name: &str) -> Result<()> {
    let output = cmd.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).output().with_context(|| name.to_owned())?;
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
    if let Some(home) = std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE")) {
        let home = PathBuf::from(home);
        if home.is_dir() {
            return home;
        }
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
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let sibling = dir.join("mimizan");
            if sibling.is_file() {
                return sibling;
            }
        }
    }
    for candidate in ["target/release/mimizan", "target/debug/mimizan"] {
        let path = PathBuf::from(candidate);
        if path.is_file() {
            return path;
        }
    }
    PathBuf::from("mimizan")
}

fn find_cameras() -> Option<PathBuf> {
    let mut roots = Vec::new();
    if let Some(p) = std::env::var_os("MIMIZAN_CAMERAS") {
        let path = PathBuf::from(p);
        if path.is_dir() {
            return Some(path);
        }
    }
    if let Ok(cwd) = std::env::current_dir() {
        roots.push(cwd);
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            roots.push(dir.to_path_buf());
        }
    }
    for mut dir in roots {
        for _ in 0..6 {
            let cameras = dir.join("cameras");
            if cameras.is_dir() {
                return Some(cameras);
            }
            if !dir.pop() {
                break;
            }
        }
    }
    None
}

/// Neutral RawTherapee profile: the named demosaicer, camera white balance,
/// no sharpening and no noise reduction, sRGB TIFF.
fn rt_profile(method: &str) -> String {
    format!(
        "[Version]\nAppVersion=5.11\nVersion=351\n\n\
[Exposure]\nAuto=false\nClip=0\nCompensation=0\nBrightness=0\nContrast=0\nSaturation=0\nBlack=0\n\
HighlightCompr=0\nHighlightComprThreshold=0\nShadowCompr=0\nHistogramMatching=false\n\
CurveFromHistogramMatching=false\nClampOOG=true\nCurveMode=Standard\nCurveMode2=Standard\nCurve=0\nCurve2=0\n\n\
[HLRecovery]\nEnabled=false\nMethod=Blend\n\n\
[White Balance]\nEnabled=true\nSetting=Camera\nTemperature=6504\nGreen=1\nEqual=1\n\n\
[Color Management]\nInputProfile=(none)\nToneCurve=false\nApplyLookTable=false\nApplyBaselineExposureOffset=false\n\
ApplyHueSatMap=false\nDCPIlluminant=0\nWorkingProfile=sRGB\nOutputProfile=RTv4_sRGB\nOutputProfileIntent=Relative\nOutputBPC=true\n\n\
[Sharpening]\nEnabled=false\n\n[SharpenEdge]\nEnabled=false\n\n[SharpenMicro]\nEnabled=false\n\n\
[Impulse Denoising]\nEnabled=false\n\n[Defringing]\nEnabled=false\n\n[Directional Pyramid Denoising]\nEnabled=false\n\n\
[RAW]\nCA=false\nHotPixelFilter=false\nDeadPixelFilter=false\nPreExposure=1\n\n\
[RAW Bayer]\nMethod={method}\nBorder=0\nCcSteps=0\nPreBlack0=0\nPreBlack1=0\nPreBlack2=0\nPreBlack3=0\n\
PreTwoGreen=true\nLineDenoise=0\nGreenEqThreshold=0\nDCBIterations=2\nDCBEnhance=true\nLMMSEIterations=2\n\
DualDemosaicAutoContrast=false\nDualDemosaicContrast=0\nPDAFLinesFilter=false\n\n\
[RAW X-Trans]\nMethod=3-pass (best)\n"
    )
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

fn pan_keeping(pane: egui::Rect, old: egui::Vec2, new: egui::Vec2, pan: egui::Vec2, cursor: egui::Pos2) -> egui::Vec2 {
    let norm = image_norm(pane, old, pan, cursor);
    (cursor - pane.center()) + new * 0.5 - new * norm
}

#[cfg(test)]
mod tests {
    use super::*;

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
    fn the_profile_names_the_demosaicer() {
        let text = rt_profile("lmmse");
        assert!(text.contains("Method=lmmse"));
        assert!(text.contains("Sharpening]\nEnabled=false"));
    }
}
