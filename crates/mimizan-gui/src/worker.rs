//! Background threads of the GUI. Nothing here draws; every thread takes a
//! request, computes, and sends the result back through a channel.
//!
//! * `Heavy`: decode, separate, export (seconds). One job at a time, in
//!   order; a panic in a job is caught and reported as `Event::Failed`.
//! * `Preview`: re-mix and tone the downscaled separation plus the zoomed
//!   detail window (milliseconds). Coalesces: only the newest request is
//!   rendered, older ones are dropped unseen.
//! * `Loupe`: one print box rendered exactly as the TIFF would be
//!   (resize, sharpen, peak). Coalesces like `Preview`.
//! * `PeakMeasure`: one-shot thread per measurement of the high-pass
//!   radius; a newer measurement drops the receiver and the old result is
//!   ignored.
//!
//! The thumbnail thread and the folder counter live in `browser`.

use crate::session::{
    cover_detail, next_session_id, preview_planes, render_detail, render_preview, render_print_detail,
    render_sharpen_loupe, DetailAsk, DetailImage, DetailRect, DetailTile, DevelopParams, HistCache,
    PlateView, PreviewImage, RawHistogram, Session, SharpenLoupe, ToneHold, ViewParams,
};
use anyhow::{Context, Result};
use mimizan_core::calib::CameraFile;
use mimizan_core::mix::{mix, Weights};
use mimizan_core::pipeline::{self, Negative};
use mimizan_core::plane::Plane;
use mimizan_core::plate::{mix_plate, PlateInfo};
use mimizan_core::print::{self as pr, PrintParams, PrintSource};
use mimizan_core::recipe::{Recipe, Sharpen};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use tracing::info;

/// Work for the `Heavy` thread.
#[allow(clippy::large_enum_variant)]
pub enum Job {
    /// Decode and develop a file.
    Open { path: PathBuf, develop: DevelopParams, cameras_dir: PathBuf },
    /// Develop the already decoded frame again with other parameters.
    Redevelop { session: Arc<Session>, develop: DevelopParams },
    /// One export, one file. The 16-bit TIFF is the recipe rendered in one
    /// pass against the negative: placing, paper or look, size and border.
    /// The desktop GUI sends the sharpening mode off; peak, when the recipe
    /// has it on, is part of that same pass.
    Export {
        session: Arc<Session>,
        weights: Weights,
        /// Three positives into one RGB file instead of the grey print.
        plate: Option<PlateView>,
        recipe: Recipe,
        path: PathBuf,
        /// TIFF orientation to apply (the file's, plus the user's turns).
        orientation: u16,
    },
}

/// What the `Heavy` thread reports back.
pub enum Event {
    /// Progress text for the toolbar while a job runs.
    Status(String),
    /// An `Open` or `Redevelop` finished.
    Opened(Arc<Session>),
    /// An `Export` finished.
    Exported { tiff: PathBuf, width: usize, height: usize, ms: u128 },
    /// The job failed or panicked; the text is for the log.
    Failed(String),
}

/// Handle to the `Heavy` thread; restarted if it ever dies.
pub struct Heavy {
    tx: Sender<Job>,
    rx: Receiver<Event>,
    ctx: egui::Context,
}

impl Heavy {
    /// Start the thread.
    pub fn spawn(ctx: egui::Context) -> Self {
        let (tx, rx) = start_heavy(&ctx);
        Self { tx, rx, ctx }
    }

    /// Queue a job.
    pub fn send(&mut self, job: Job) {
        if let Err(std::sync::mpsc::SendError(job)) = self.tx.send(job) {
            self.restart();
            let _ = self.tx.send(job);
        }
    }

    /// The next event, if one is waiting.
    pub fn poll(&mut self) -> Option<Event> {
        match self.rx.try_recv() {
            Ok(e) => Some(e),
            Err(TryRecvError::Empty) => None,
            Err(TryRecvError::Disconnected) => {
                self.restart();
                None
            }
        }
    }

    fn restart(&mut self) {
        tracing::error!("open and export stopped; starting them again");
        let (tx, rx) = start_heavy(&self.ctx);
        self.tx = tx;
        self.rx = rx;
    }
}

fn start_heavy(ctx: &egui::Context) -> (Sender<Job>, Receiver<Event>) {
    let (tx, jobs) = channel::<Job>();
    let (events, rx) = channel::<Event>();
    let ctx = ctx.clone();
    thread::Builder::new()
        .name("mimizan-heavy".into())
        .spawn(move || {
            while let Ok(job) = jobs.recv() {
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_heavy(job, &events, &ctx)));
                let ev = match outcome {
                    Ok(Ok(ev)) => ev,
                    Ok(Err(e)) => Event::Failed(format!("{e:#}")),
                    Err(payload) => Event::Failed(format!("stopped: {}", panic_message(&payload))),
                };
                if events.send(ev).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        })
        .expect("spawn heavy worker");
    (tx, rx)
}

fn run_heavy(job: Job, events: &Sender<Event>, ctx: &egui::Context) -> Result<Event> {
    let status = |s: String| {
        let _ = events.send(Event::Status(s));
        ctx.request_repaint();
    };
    match job {
        Job::Open { path, develop, cameras_dir } => {
            open(&path, develop, &cameras_dir, &status).map(|s| Event::Opened(Arc::new(s)))
        }
        Job::Redevelop { session, develop } => {
            redevelop(&session, develop, &status).map(|s| Event::Opened(Arc::new(s)))
        }
        Job::Export { session, weights, plate, recipe, path, orientation } => match plate {
            Some(plate) => export_plate(&session, weights, &plate, recipe, &path, orientation, &status),
            None => export(&session, weights, recipe, &path, orientation, &status),
        },
    }
}

fn open(path: &Path, develop: DevelopParams, cameras_dir: &Path, status: &dyn Fn(String)) -> Result<Session> {
    status(format!("decoding {}", path.display()));
    let t = Instant::now();
    let frame = pipeline::decode(path).with_context(|| format!("decoding {}", path.display()))?;
    let decode_ms = t.elapsed().as_millis();
    let frame = Arc::new(frame);

    // The camera file only supplies suggested weights and the USM amount;
    // a corrupt one must not keep the picture from opening.
    let camera_file_path = CameraFile::find(cameras_dir, &frame.clean_make, &frame.clean_model);
    let camera_file = camera_file_path.as_ref().and_then(|p| match CameraFile::load(p) {
        Ok(c) => Some(c),
        Err(e) => {
            tracing::warn!("camera file {} ignored: {e}", p.display());
            None
        }
    });
    develop_frame(path.to_path_buf(), frame, decode_ms, camera_file, camera_file_path, develop, status)
}

fn redevelop(s: &Session, develop: DevelopParams, status: &dyn Fn(String)) -> Result<Session> {
    develop_frame(
        s.path.clone(),
        Arc::clone(&s.frame),
        s.negative.info.timing.decode_ms,
        s.camera_file.clone(),
        s.camera_file_path.clone(),
        develop,
        status,
    )
}

fn develop_frame(
    path: PathBuf,
    frame: Arc<mimizan_core::decode::RawFrame>,
    decode_ms: u128,
    camera_file: Option<CameraFile>,
    camera_file_path: Option<PathBuf>,
    develop: DevelopParams,
    status: &dyn Fn(String),
) -> Result<Session> {
    status(if matches!(frame.kind, mimizan_core::decode::SensorKind::Mono) {
        "reading the sensor".into()
    } else {
        "separating".into()
    });
    let params = develop.negative_params(camera_file.clone());
    let negative: Negative = pipeline::negative_from_frame(&frame, &params, decode_ms)?;
    status("building preview".into());
    let t = Instant::now();
    let preview = preview_planes(&negative)?;
    let raw_hist = RawHistogram::of_frame(&frame);
    info!("preview planes + raw histogram {} ms", t.elapsed().as_millis());
    Ok(Session {
        id: next_session_id(),
        path,
        frame,
        negative,
        camera_file,
        camera_file_path,
        develop,
        preview,
        raw_hist,
    })
}

/// The plane the print is made from. A monochrome sensor is already the
/// picture. Bayer mixes the separation; the weights stay out of that plane.
fn mixed_plane(s: &Session, weights: &Weights) -> Result<Option<Plane>> {
    if s.is_mono() {
        return Ok(None);
    }
    let Some(sep) = s.separation() else {
        return Err(anyhow::anyhow!("this negative has no separation"));
    };
    Ok(Some(mix(sep, weights)))
}

/// Radius and amount on the negative's own pixels. The size step and the
/// borders are left out, so neither the print's scale nor the paper enters
/// the measurement; the print multiplies the radius later, above 1 and
/// below. Sharpening and the peak itself are off for the render.
fn measure_peak(
    s: &Session,
    weights: &Weights,
    mut recipe: Recipe,
    orientation: u16,
) -> Result<(f64, f64, bool)> {
    use rayon::prelude::*;
    recipe.sharpen = Sharpen::Off;
    recipe.peak.on = false;
    recipe.size = None;
    recipe.border_mm = 0.0;
    recipe.black_mm = 0.0;
    let mixed = mixed_plane(s, weights)?;
    let plane = mixed.as_ref().unwrap_or(&s.negative.plane);
    let src =
        PrintSource { plane, mask: None, orientation, name: s.path.display().to_string(), description: None };
    let out = pr::render_planes(&src, &PrintParams { recipe, ..PrintParams::default() });
    let samples: Vec<f32> = out.plane.data.par_iter().map(|&v| v as f32).collect();
    let tuned = mimizan_core::peak::tune(&samples, out.plane.width, out.plane.height);
    let (radius, amount) = mimizan_core::recipe::Peak::clamp_measured(tuned.radius, tuned.amount);
    Ok((radius, amount, tuned.found))
}

/// Result of one peak measurement; `generation` tells the app whether it
/// still belongs to the open picture.
pub struct PeakMeasured {
    pub generation: u64,
    pub radius: f64,
    pub amount: f64,
    pub found: bool,
}

/// State of a `PeakMeasure` as seen from the app.
pub enum PeakPoll {
    Waiting,
    Ready(PeakMeasured),
    /// The thread ended without a result (it logged why).
    Lost,
}

/// One measurement of the open print. A newer one drops this receiver.
pub struct PeakMeasure {
    rx: Receiver<PeakMeasured>,
}

impl PeakMeasure {
    /// Start the measurement on its own thread.
    pub fn start(
        ctx: egui::Context,
        generation: u64,
        session: Arc<Session>,
        weights: Weights,
        recipe: Recipe,
        orientation: u16,
    ) -> Self {
        let (tx, rx) = channel();
        thread::Builder::new()
            .name("mimizan-peak".into())
            .spawn(move || {
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    measure_peak(&session, &weights, recipe, orientation)
                }));
                match outcome {
                    Ok(Ok((radius, amount, found))) => {
                        let _ = tx.send(PeakMeasured { generation, radius, amount, found });
                    }
                    Ok(Err(e)) => tracing::error!("peak measure failed: {e:#}"),
                    Err(_) => tracing::error!("peak measure stopped"),
                }
                ctx.request_repaint();
            })
            .expect("spawn peak measure");
        Self { rx }
    }

    /// Has the measurement arrived?
    pub fn poll(&self) -> PeakPoll {
        match self.rx.try_recv() {
            Ok(v) => PeakPoll::Ready(v),
            Err(TryRecvError::Empty) => PeakPoll::Waiting,
            Err(TryRecvError::Disconnected) => PeakPoll::Lost,
        }
    }
}

fn export(
    s: &Session,
    weights: Weights,
    recipe: Recipe,
    path: &Path,
    orientation: u16,
    status: &dyn Fn(String),
) -> Result<Event> {
    let t = Instant::now();
    recipe.validate()?;
    status(if s.is_mono() { "placing".into() } else { "mixing".into() });
    let mixed = mixed_plane(s, &weights)?;
    let plane = mixed.as_ref().unwrap_or(&s.negative.plane);
    let params = PrintParams { recipe, ..Default::default() };
    // The mask (a full f64 plane) is only read by the sharpening step.
    let mask = if params.sharpens() { Some(s.mask_plane()?) } else { None };
    let mut info = s.negative.info.clone();
    info.weights = weights;
    info.weights_raw = weights.balanced_to_raw(s.wb());
    let src = PrintSource {
        plane,
        mask: mask.as_ref(),
        orientation,
        name: s.path.display().to_string(),
        description: serde_json::to_value(&info).ok(),
    };

    // The print: the recipe in one pass against the negative.
    status("rendering".into());
    let out = pr::render_planes(&src, &params);
    status(format!("writing {}", path.display()));
    pr::write(&out, path, &params)?;
    Ok(Event::Exported {
        tiff: path.to_path_buf(),
        width: out.info.width,
        height: out.info.height,
        ms: t.elapsed().as_millis(),
    })
}

/// The three-colour plate: three mixes of the separation, each through
/// the same pass as the grey print plus its lamp, into one 16-bit RGB
/// file with the primaries' profile. What `mimizan print` does to a plate
/// file, without the file.
fn export_plate(
    s: &Session,
    weights: Weights,
    plate: &PlateView,
    recipe: Recipe,
    path: &Path,
    orientation: u16,
    status: &dyn Fn(String),
) -> Result<Event> {
    let t = Instant::now();
    recipe.validate()?;
    let Some(sep) = s.separation() else {
        return Err(anyhow::anyhow!("a three-colour plate needs a Bayer separation"));
    };
    status("mixing the three plates".into());
    let planes = mix_plate(sep, &plate.weights);
    let params = PrintParams { recipe, lamps: plate.lamps, ..Default::default() };
    let mask = if params.sharpens() { Some(s.mask_plane()?) } else { None };
    let pi = PlateInfo::new(&plate.weights, plate.space, sep.lum.height);
    let mut info = s.negative.info.clone();
    info.weights = weights;
    info.weights_raw = weights.balanced_to_raw(s.wb());
    info.plate = Some(pi.clone());
    let description = serde_json::to_value(&info).ok();
    let [r, g, b] = &planes;
    let sources = [r, g, b].map(|plane| PrintSource {
        plane,
        mask: mask.as_ref(),
        orientation,
        name: s.path.display().to_string(),
        description: description.clone(),
    });
    status("rendering the three positives".into());
    let out = pr::render_plate(sources, &pi, &params);
    status(format!("writing {}", path.display()));
    pr::write(&out, path, &params)?;
    Ok(Event::Exported {
        tiff: path.to_path_buf(),
        width: out.info.width,
        height: out.info.height,
        ms: t.elapsed().as_millis(),
    })
}

pub struct PreviewRequest {
    pub session: Arc<Session>,
    pub view: ViewParams,
    /// Window the zoomed view needs, if any. Without `inspect` the
    /// coordinates are negative pixels and `step` may skip to the screen
    /// grid. With `inspect` they are pixels of the fitted print, step 1.
    pub detail: Option<DetailAsk>,
    /// Set when a centimetre size makes 100% a window of the fitted print.
    pub inspect: Option<Recipe>,
    pub generation: u64,
}

pub struct PreviewResult {
    pub generation: u64,
    /// New whole-image preview, when the view changed.
    pub image: Option<PreviewImage>,
    /// New 1:1 window, when one was requested and changed.
    pub detail: Option<DetailImage>,
    pub ms: u128,
    /// Set when this request panicked. The picture stays as it was.
    pub failed: Option<String>,
}

pub struct Preview {
    tx: Sender<PreviewRequest>,
    rx: Receiver<PreviewResult>,
    ctx: egui::Context,
}

impl Preview {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (tx, rx) = start_preview(&ctx);
        Self { tx, rx, ctx }
    }

    pub fn request(&mut self, r: PreviewRequest) {
        if let Err(std::sync::mpsc::SendError(r)) = self.tx.send(r) {
            self.restart();
            let _ = self.tx.send(r);
        }
    }

    /// All finished results since the last poll, oldest first (an image and
    /// a window may arrive as separate results).
    pub fn poll(&mut self) -> Vec<PreviewResult> {
        let mut all = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(r) => all.push(r),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.restart();
                    break;
                }
            }
        }
        all
    }

    fn restart(&mut self) {
        tracing::error!("preview stopped; starting it again");
        let (tx, rx) = start_preview(&self.ctx);
        self.tx = tx;
        self.rx = rx;
    }
}

/// What the preview thread keeps between requests, so an unchanged view
/// costs nothing and a pan reuses the overlap of the last detail tile.
#[derive(Default)]
struct PreviewHold {
    last: Option<(u64, ViewParams)>,
    last_detail: Option<DetailAsk>,
    last_inspect: Option<Recipe>,
    held: Option<DetailTile>,
    held_print: bool,
    hists: HistCache,
    tones: ToneHold,
}

fn start_preview(ctx: &egui::Context) -> (Sender<PreviewRequest>, Receiver<PreviewResult>) {
    let (tx, reqs) = channel::<PreviewRequest>();
    let (results, rx) = channel::<PreviewResult>();
    let ctx = ctx.clone();
    thread::Builder::new()
        .name("mimizan-preview".into())
        .spawn(move || {
            let mut hold = PreviewHold::default();
            while let Ok(mut req) = reqs.recv() {
                while let Ok(newer) = reqs.try_recv() {
                    req = newer;
                }
                let generation = req.generation;
                let started = Instant::now();
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| paint_preview(&mut hold, req)));
                let result = match outcome {
                    Ok(image) => image,
                    Err(payload) => {
                        // The hold may be half updated. Drop it and start clean.
                        hold = PreviewHold::default();
                        Some(PreviewResult {
                            generation,
                            image: None,
                            detail: None,
                            ms: started.elapsed().as_millis(),
                            failed: Some(format!("stopped: {}", panic_message(&payload))),
                        })
                    }
                };
                let Some(r) = result else { continue };
                if results.send(r).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        })
        .expect("spawn preview worker");
    (tx, rx)
}

fn paint_preview(hold: &mut PreviewHold, req: PreviewRequest) -> Option<PreviewResult> {
    let t = Instant::now();
    let key = (req.session.id, req.view.clone());
    let view_changed = hold.last.as_ref() != Some(&key);
    // The held window is the previous exposure. Drop it even when this
    // frame shows no sharp window, or the next zoom would reuse those pixels.
    if view_changed {
        hold.held = None;
    }
    let table = hold.tones.get(&req.view.tone);
    let image = view_changed.then(|| render_preview(&req.session, &req.view, &mut hold.hists, table));
    let detail_due = req.detail.is_some()
        && (view_changed || hold.last_detail != req.detail || hold.last_inspect != req.inspect);
    let detail = if detail_due {
        let ask = req.detail?;
        let print = req.inspect.is_some();
        if view_changed || print != hold.held_print {
            hold.held = None;
        }
        hold.held_print = print;
        let tile = if let Some(recipe) = &req.inspect {
            let recipe = recipe.clone();
            cover_detail(hold.held.as_ref(), ask, |sub| {
                let rect = DetailRect { x: sub.x, y: sub.y, w: sub.w, h: sub.h };
                render_print_detail(&req.session, &req.view, &recipe, rect, table)
            })
        } else {
            let codes = (req.view.place == 0.0 && !req.view.show_mask)
                .then(|| hold.hists.codes_for(&req.session, &req.view.weights));
            cover_detail(hold.held.as_ref(), ask, |sub| {
                render_detail(&req.session, &req.view, sub, table, codes)
            })
        };
        let image = DetailImage { ask: tile.ask, gray: tile.gray.clone(), rgb: tile.rgb.clone() };
        hold.held = Some(tile);
        Some(image)
    } else {
        None
    };
    hold.last = Some(key);
    hold.last_detail = req.detail;
    hold.last_inspect = req.inspect;
    if image.is_none() && detail.is_none() {
        return None;
    }
    Some(PreviewResult {
        generation: req.generation,
        image,
        detail,
        ms: t.elapsed().as_millis(),
        failed: None,
    })
}

/// One 1:1 box of the print. Coalesced like the preview: a slider drag
/// keeps only the newest point and recipe.
pub struct LoupeRequest {
    pub session: Arc<Session>,
    pub weights: Weights,
    /// The box in colour: the three positives with their lamps.
    pub plate: Option<PlateView>,
    pub recipe: Recipe,
    pub orientation: u16,
    pub nx: f64,
    pub ny: f64,
    /// Box side in print pixels. One screen pixel each.
    pub view: usize,
    pub generation: u64,
}

pub struct LoupeResult {
    pub generation: u64,
    pub image: SharpenLoupe,
}

pub struct Loupe {
    tx: Sender<LoupeRequest>,
    rx: Receiver<LoupeResult>,
    ctx: egui::Context,
}

impl Loupe {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (tx, rx) = start_loupe(&ctx);
        Self { tx, rx, ctx }
    }

    pub fn request(&mut self, r: LoupeRequest) {
        if let Err(std::sync::mpsc::SendError(r)) = self.tx.send(r) {
            self.restart();
            let _ = self.tx.send(r);
        }
    }

    /// The newest finished box since the last poll. Older ones are dropped:
    /// a slider drag may have completed several, and only the latest matches
    /// the controls.
    pub fn poll(&mut self) -> Option<LoupeResult> {
        let mut last = None;
        loop {
            match self.rx.try_recv() {
                Ok(r) => last = Some(r),
                Err(TryRecvError::Empty) => break,
                Err(TryRecvError::Disconnected) => {
                    self.restart();
                    break;
                }
            }
        }
        last
    }

    fn restart(&mut self) {
        tracing::error!("print box stopped; starting it again");
        let (tx, rx) = start_loupe(&self.ctx);
        self.tx = tx;
        self.rx = rx;
    }
}

fn start_loupe(ctx: &egui::Context) -> (Sender<LoupeRequest>, Receiver<LoupeResult>) {
    let (tx, reqs) = channel::<LoupeRequest>();
    let (results, rx) = channel::<LoupeResult>();
    let ctx = ctx.clone();
    thread::Builder::new()
        .name("mimizan-loupe".into())
        .spawn(move || {
            while let Ok(mut req) = reqs.recv() {
                while let Ok(newer) = reqs.try_recv() {
                    req = newer;
                }
                let generation = req.generation;
                // A panic used to kill this thread. The box then kept the
                // last texture forever, and every later size, dpi or kernel
                // change looked frozen.
                let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    render_sharpen_loupe(
                        &req.session,
                        &req.weights,
                        req.plate.as_ref(),
                        &req.recipe,
                        req.orientation,
                        req.nx,
                        req.ny,
                        req.view,
                    )
                }));
                let image = match rendered {
                    Ok(image) => image,
                    Err(payload) => {
                        tracing::error!("loupe render failed: {}", panic_message(&payload));
                        SharpenLoupe { gray: Vec::new(), rgb: None, width: 0, height: 0 }
                    }
                };
                if results.send(LoupeResult { generation, image }).is_err() {
                    break;
                }
                ctx.request_repaint();
            }
        })
        .expect("spawn loupe worker");
    (tx, rx)
}

/// The text of a caught panic, for the log and the status line.
pub(crate) fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::session::{next_session_id, preview_planes, RawHistogram};
    use mimizan_core::cfa::BayerPhase;
    use mimizan_core::decode::{RawData, RawFrame, Rect, SensorKind};
    use mimizan_core::pipeline::NegativeParams;
    use mimizan_core::plate::{PlateSpace, PlateWeights};

    /// The export of a plate from a grey mosaic is a 16-bit RGB file whose
    /// three channels are equal, the same size the grey export has, and a
    /// blue lamp lifts the blue channel alone.
    #[test]
    fn plate_export_writes_grey_as_equal_channels() {
        let (w, h) = (40usize, 24usize);
        let raw: Vec<u16> =
            (0..w * h).map(|i| (150.0 + 700.0 * ((i / w) as f64 / h as f64)) as u16).collect();
        let frame = RawFrame {
            make: "Test".into(),
            model: "Bayer".into(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: w,
            height: h,
            bits: 10,
            kind: SensorKind::Bayer(BayerPhase::RGGB),
            black_tile: [0.0; 4],
            white_rgb: [1000.0; 3],
            crop: Rect { x: 0, y: 0, w, h },
            orientation: 1,
            xyz_to_cam: None,
            wb_as_shot: Some([1.0, 1.0, 1.0]),
            exposure: Default::default(),
            data: RawData::Integer(raw),
        };
        let params = NegativeParams { keep_separation: true, ..NegativeParams::default() };
        let negative = pipeline::negative_from_frame(&frame, &params, 0).unwrap();
        let preview = preview_planes(&negative).unwrap();
        let raw_hist = RawHistogram::of_frame(&frame);
        let session = Session {
            id: next_session_id(),
            path: "grey.nef".into(),
            frame: Arc::new(frame),
            negative,
            camera_file: None,
            camera_file_path: None,
            develop: DevelopParams::default(),
            preview,
            raw_hist,
        };
        let dir = std::env::temp_dir().join(format!("mimizan-gui-plate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let quiet = |_s: String| {};
        let grey = dir.join("grey.tif");
        let recipe = Recipe::on_paper();
        let ev = export(&session, Weights::default(), recipe.clone(), &grey, 1, &quiet).unwrap();
        let Event::Exported { width, height, .. } = ev else { panic!("not exported") };

        let plate =
            PlateView { weights: PlateWeights::channels(), space: PlateSpace::AdobeRgb, lamps: [0.0; 3] };
        let path = dir.join("plate.tif");
        let ev =
            export_plate(&session, Weights::default(), &plate, recipe.clone(), &path, 1, &quiet).unwrap();
        let Event::Exported { width: pw, height: ph, .. } = ev else { panic!("not exported") };
        assert_eq!((pw, ph), (width, height));
        let [r, g, b] = mimizan_core::tiffin::read_rgb16(&path).unwrap();
        assert_eq!((r.width, r.height), (width, height));
        assert_eq!(r.data, g.data, "grey prints grey");
        assert_eq!(g.data, b.data, "grey prints grey");
        let mono = mimizan_core::tiffin::read_gray(&grey).unwrap();
        assert_eq!(mono.plane.data, g.data, "the green positive is the grey print");

        let lit = PlateView { lamps: [0.0, 0.0, 0.5], ..plate };
        let path = dir.join("plate_blue.tif");
        export_plate(&session, Weights::default(), &lit, recipe, &path, 1, &quiet).unwrap();
        let [r2, g2, b2] = mimizan_core::tiffin::read_rgb16(&path).unwrap();
        assert_eq!(r2.data, r.data);
        assert_eq!(g2.data, g.data);
        assert!(b2.data.iter().zip(&b.data).all(|(x, y)| x >= y));
        assert!(b2.data.iter().zip(&b.data).any(|(x, y)| x > y));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
