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
    cover_detail, negative_params, next_session_id, preview_planes, render_detail_linear, render_preview,
    render_print_linear, render_sharpen_loupe, tone_tile, DetailAsk, DetailImage, DetailRect, DetailTile,
    HistCache, LinearTile, PlateView, PreviewImage, PreviewStages, RawHistogram, Session, SharpenLoupe,
    ToneHold, ViewParams,
};
use anyhow::{Context, Result};
use mimizan_core::calib::CameraFile;
use mimizan_core::cfa::BayerPhase;
use mimizan_core::decode::SensorKind;
use mimizan_core::linear::Framing;
use mimizan_core::mix::{mix, Weights};
use mimizan_core::pipeline::{self, Negative};
use mimizan_core::plane::Plane;
use mimizan_core::plate::{mix_plate, PlateInfo, PlateSpace, PlateWeights};
use mimizan_core::print::{self as pr, PrintParams, PrintSource};
use mimizan_core::recipe::{CropRect, Develop, Recipe, Sharpen};
use mimizan_core::tone::Tone;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use tracing::info;

/// Work for the `Heavy` thread.
#[allow(clippy::large_enum_variant)]
pub enum Job {
    /// Decode and develop a file.
    Open { path: PathBuf, develop: Develop, cameras_dir: PathBuf },
    /// Develop the already decoded frame again with other parameters. The
    /// frame alone, not the session: the UI may let the session go while
    /// this runs, so its planes are not held beside the new ones.
    Redevelop { source: Redevelop, develop: Develop },
    /// One export, one file. The sheet is the recipe rendered in one pass
    /// against the negative: placing, paper or look, size and border. A
    /// `.jpg` path writes the 8-bit JPEG, anything else the 16-bit TIFF
    /// (`print::write`).
    /// The desktop GUI sends the sharpening mode off; peak, when the recipe
    /// has it on, is part of that same pass. The recipe is the edit's own:
    /// its 90° turns are carried out by the print, on the file's
    /// orientation.
    Export {
        session: Arc<Session>,
        weights: Weights,
        /// Three positives into one RGB file instead of the grey print.
        plate: Option<PlateView>,
        recipe: Recipe,
        path: PathBuf,
    },
    /// The linear export: the separation for another program, no print.
    /// In the monochrome process the mixed negative as a linear grey TIFF
    /// with its mask beside it; in the plate process the channels as a
    /// linear DNG. Only the framing is applied: the turns, the straighten
    /// and the crop, as the print recipe carries them.
    ExportLinear {
        session: Arc<Session>,
        weights: Weights,
        /// The DNG of the channels plate instead of the grey negative.
        plate: bool,
        /// The turns, the straighten with its kernel, and the window on
        /// the turned frame.
        framing: Framing,
        path: PathBuf,
    },
}

/// What the `Heavy` thread reports back.
pub enum Event {
    /// Progress text for the toolbar while a job runs.
    Status(String),
    /// An `Open` or `Redevelop` finished.
    Opened(Arc<Session>),
    /// An `Export` or `ExportLinear` finished: the file written and its size.
    Exported { path: PathBuf, width: usize, height: usize, ms: u128 },
    /// The development would not fit in free memory and was not started.
    /// The job comes back so the UI can let the picture it holds go and
    /// send it once more; `message` is for the log when it cannot.
    Refused { job: Box<Job>, message: String },
    /// The job failed or panicked; the text is for the log.
    Failed(String),
}

impl Job {
    /// Whether the job writes a file.
    fn is_export(&self) -> bool {
        matches!(self, Job::Export { .. } | Job::ExportLinear { .. })
    }

    /// The file an `Open` or `Redevelop` is about; none for an export.
    pub fn developed_path(&self) -> Option<&Path> {
        match self {
            Job::Open { path, .. } => Some(path),
            Job::Redevelop { source, .. } => Some(&source.path),
            Job::Export { .. } | Job::ExportLinear { .. } => None,
        }
    }
}

/// The exports sent and not yet written out. The program ends with its
/// window, and the thread that writes with the program: a file still on
/// its way then stays cut off on the disk, so the window asks here first.
#[derive(Clone, Default)]
struct Writing(Arc<AtomicUsize>);

/// One export on its way. It travels with its job and is counted until it
/// is dropped: when the file is written, when the export failed or
/// panicked, or when the queue it waited in is gone.
struct WritingOne(Arc<AtomicUsize>);

impl Writing {
    fn begin(&self) -> WritingOne {
        self.0.fetch_add(1, Ordering::SeqCst);
        WritingOne(Arc::clone(&self.0))
    }

    fn any(&self) -> bool {
        self.0.load(Ordering::SeqCst) > 0
    }
}

impl Drop for WritingOne {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// A job in the queue, with its count when it is an export.
type Queued = (Job, Option<WritingOne>);

/// Handle to the `Heavy` thread; restarted if it ever dies.
pub struct Heavy {
    tx: Sender<Queued>,
    rx: Receiver<Event>,
    ctx: egui::Context,
    writing: Writing,
}

impl Heavy {
    /// Start the thread.
    pub fn spawn(ctx: egui::Context) -> Self {
        let (tx, rx) = start_heavy(&ctx);
        Self { tx, rx, ctx, writing: Writing::default() }
    }

    /// Queue a job.
    pub fn send(&mut self, job: Job) {
        let writing = job.is_export().then(|| self.writing.begin());
        if let Err(std::sync::mpsc::SendError(queued)) = self.tx.send((job, writing)) {
            self.restart();
            let _ = self.tx.send(queued);
        }
    }

    /// An export is queued or being written. False again by the time its
    /// `Exported` or `Failed` can be polled.
    pub fn writing(&self) -> bool {
        self.writing.any()
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

fn start_heavy(ctx: &egui::Context) -> (Sender<Queued>, Receiver<Event>) {
    let (tx, jobs) = channel::<Queued>();
    let (events, rx) = channel::<Event>();
    let ctx = ctx.clone();
    thread::Builder::new()
        .name("mimizan-heavy".into())
        .spawn(move || {
            while let Ok((job, writing)) = jobs.recv() {
                let outcome =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| run_heavy(job, &events, &ctx)));
                let ev = match outcome {
                    Ok(Ok(ev)) => ev,
                    Ok(Err(e)) => Event::Failed(format!("{e:#}")),
                    Err(payload) => Event::Failed(format!("stopped: {}", panic_message(&payload))),
                };
                // The file is closed: no longer counted before the UI
                // hears of it.
                drop(writing);
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
        Job::Open { path, develop, cameras_dir } => match open(&path, develop, &cameras_dir, &status) {
            Ok(s) => Ok(Event::Opened(Arc::new(s))),
            // The decoded frame is gone with the error; the UI decodes it
            // again once it has made room.
            Err(e) if e.is::<Refused>() => Ok(Event::Refused {
                job: Box::new(Job::Open { path, develop, cameras_dir }),
                message: format!("{e:#}"),
            }),
            Err(e) => Err(e),
        },
        Job::Redevelop { source, develop } => match check_memory(&source.path, &source.frame, &develop) {
            Ok(()) => redevelop(source, develop, &status).map(|s| Event::Opened(Arc::new(s))),
            Err(e) => Ok(Event::Refused {
                job: Box::new(Job::Redevelop { source, develop }),
                message: format!("{e:#}"),
            }),
        },
        Job::Export { session, weights, plate, recipe, path } => match plate {
            Some(plate) => export_plate(&session, weights, &plate, recipe, &path, &status),
            None => export(&session, weights, recipe, &path, &status),
        },
        Job::ExportLinear { session, weights, plate, framing, path } => {
            export_linear(&session, weights, plate, &framing, &path, &status)
        }
    }
}

/// What a re-development needs from the session it replaces.
pub struct Redevelop {
    pub path: PathBuf,
    pub frame: Arc<mimizan_core::decode::RawFrame>,
    pub decode_ms: u128,
    pub camera_file: Option<CameraFile>,
    pub camera_file_path: Option<PathBuf>,
}

impl Redevelop {
    pub fn of(s: &Session) -> Self {
        Self {
            path: s.path.clone(),
            frame: Arc::clone(&s.frame),
            decode_ms: s.negative.info.timing.decode_ms,
            camera_file: s.camera_file.clone(),
            camera_file_path: s.camera_file_path.clone(),
        }
    }
}

/// Free memory as the system reports it; 0 when it cannot say.
pub fn available_memory() -> u64 {
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    sys.available_memory()
}

/// Bytes the development of this frame holds at its peak, developed as
/// `develop` says: the moiré net holds more than the separation alone.
pub fn develop_memory(frame: &mimizan_core::decode::RawFrame, develop: &Develop) -> u64 {
    pipeline::develop_memory_of(frame, develop.runs_moire_net())
}

/// The peak of a development of a `width`×`height` file that is not
/// decoded yet, so its sensor kind is unknown: the Bayer figure, which
/// is the larger. The strip's thumbnail metadata gives the size.
pub fn develop_memory_of_size(width: usize, height: usize, develop: &Develop) -> u64 {
    pipeline::develop_memory(width, height, SensorKind::Bayer(BayerPhase::RGGB), develop.runs_moire_net())
}

/// How much of the free memory a development may take: a third is kept
/// over, for the allocator and whatever else the process has open. The
/// same figure decides when the UI lets the current picture go and when
/// the worker refuses to start.
pub const MEMORY_HEADROOM: f64 = 1.3;

/// Whether `need` bytes fit in `available` with [`MEMORY_HEADROOM`] to
/// spare. Unknown free memory (0) fits everything: there is nothing to
/// measure against.
pub fn memory_fits(need: u64, available: u64) -> bool {
    available == 0 || need as f64 <= available as f64 / MEMORY_HEADROOM
}

/// A development the machine cannot hold, refused before it started.
#[derive(Debug)]
pub struct Refused(String);

impl std::fmt::Display for Refused {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Refused {}

/// Refuse a development the machine cannot hold. Past the free memory it
/// ends in the OOM killer or a frozen machine, not in an error, and the
/// panic catcher sees nothing of it. The UI lets the picture it holds go
/// and sends the job again; the message states the figures, and the UI
/// adds what to do when that was not enough.
fn check_memory(path: &Path, frame: &mimizan_core::decode::RawFrame, develop: &Develop) -> Result<()> {
    let (need, available) = (develop_memory(frame, develop), available_memory());
    if memory_fits(need, available) {
        return Ok(());
    }
    let name = path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default();
    Err(anyhow::Error::new(Refused(format!(
        "{name} not developed: {}x{} would hold about {} while separating, {} is free",
        frame.width,
        frame.height,
        pr::memory_text(need),
        pr::memory_text(available)
    ))))
}

fn open(path: &Path, develop: Develop, cameras_dir: &Path, status: &dyn Fn(String)) -> Result<Session> {
    status(format!("decoding {}", path.display()));
    let t = Instant::now();
    let frame = pipeline::decode(path).with_context(|| format!("decoding {}", path.display()))?;
    let decode_ms = t.elapsed().as_millis();
    check_memory(path, &frame, &develop)?;
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

/// The memory was checked by the caller, which still has the job to send
/// back when it does not fit.
fn redevelop(r: Redevelop, develop: Develop, status: &dyn Fn(String)) -> Result<Session> {
    develop_frame(r.path, r.frame, r.decode_ms, r.camera_file, r.camera_file_path, develop, status)
}

fn develop_frame(
    path: PathBuf,
    frame: Arc<mimizan_core::decode::RawFrame>,
    decode_ms: u128,
    camera_file: Option<CameraFile>,
    camera_file_path: Option<PathBuf>,
    develop: Develop,
    status: &dyn Fn(String),
) -> Result<Session> {
    status(if matches!(frame.kind, mimizan_core::decode::SensorKind::Mono) {
        "reading the sensor".into()
    } else {
        "separating".into()
    });
    let params = negative_params(&develop, camera_file.clone());
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

/// Result of one peak measurement.
pub struct PeakMeasured {
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

/// One measurement of the open print. The app drops it with the picture
/// it was started on, so a result that arrives is that picture's.
pub struct PeakMeasure {
    rx: Receiver<PeakMeasured>,
}

impl PeakMeasure {
    /// Start the measurement on its own thread.
    pub fn start(
        ctx: egui::Context,
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
                        let _ = tx.send(PeakMeasured { radius, amount, found });
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
        orientation: s.negative.orientation,
        name: s.path.display().to_string(),
        description: serde_json::to_value(&info).ok(),
    };

    // The print: the recipe in one pass against the negative.
    status("rendering".into());
    let out = pr::render_planes(&src, &params);
    status(format!("writing {}", path.display()));
    pr::write(&out, path, &params)?;
    Ok(Event::Exported {
        path: path.to_path_buf(),
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
        orientation: s.negative.orientation,
        name: s.path.display().to_string(),
        description: description.clone(),
    });
    status("rendering the three positives".into());
    let out = pr::render_plate(sources, &pi, &params);
    status(format!("writing {}", path.display()));
    pr::write(&out, path, &params)?;
    Ok(Event::Exported {
        path: path.to_path_buf(),
        width: out.info.width,
        height: out.info.height,
        ms: t.elapsed().as_millis(),
    })
}

/// The linear export. Monochrome process: the mixed negative (the file's
/// own plane on a monochrome sensor) as a linear grey TIFF, the mask
/// beside it. Plate process: the channels plate, white balance undone, as
/// a linear DNG with the camera's matrix. Both framed as `framing` says.
fn export_linear(
    s: &Session,
    weights: Weights,
    plate: bool,
    framing: &Framing,
    path: &Path,
    status: &dyn Fn(String),
) -> Result<Event> {
    use mimizan_core::linear::{write_dng, write_negative, DngMeta};
    let t = Instant::now();
    let mut info = s.negative.info.clone();
    info.weights = weights;
    info.weights_raw = weights.balanced_to_raw(s.wb());
    let written = if plate {
        let Some(sep) = s.separation() else {
            return Err(anyhow::anyhow!("a linear DNG needs a Bayer separation"));
        };
        status("mixing the three channels".into());
        let channels = PlateWeights::channels();
        let planes = mix_plate(sep, &channels);
        info.plate = Some(PlateInfo::new(&channels, PlateSpace::default(), sep.lum.height));
        let desc = serde_json::to_string(&info)?;
        let meta = DngMeta::of(&s.frame, s.wb(), &desc)
            .context("the file carries no colour matrix, so no DNG can name the camera's colour")?;
        status(format!("writing {}", path.display()));
        let [r, g, b] = &planes;
        write_dng(path, [r, g, b], framing, &meta)?
    } else {
        status(if s.is_mono() { "framing".into() } else { "mixing".into() });
        let mixed = mixed_plane(s, &weights)?;
        let plane = mixed.as_ref().unwrap_or(&s.negative.plane);
        let mask = s.mask_plane()?;
        let desc = serde_json::to_string(&info)?;
        status(format!("writing {}", path.display()));
        write_negative(path, plane, Some(&mask), framing, &desc)?
    };
    Ok(Event::Exported {
        path: path.to_path_buf(),
        width: written.width,
        height: written.height,
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
    /// The session whose pixels these are.
    pub session: u64,
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
    /// The newest generation asked for. The worker reads it between its
    /// stages and drops what no one wants any more.
    latest: Arc<AtomicU64>,
}

impl Preview {
    pub fn spawn(ctx: egui::Context) -> Self {
        let latest = Arc::new(AtomicU64::new(0));
        let (tx, rx) = start_preview(&ctx, Arc::clone(&latest));
        Self { tx, rx, ctx, latest }
    }

    pub fn request(&mut self, r: PreviewRequest) {
        self.latest.store(r.generation, Ordering::Relaxed);
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
        let (tx, rx) = start_preview(&self.ctx, Arc::clone(&self.latest));
        self.tx = tx;
        self.rx = rx;
    }
}

/// What the preview thread keeps between requests, so an unchanged view
/// costs nothing, a view that changes only the tone pays only the tone,
/// and a pan reuses the overlap of the last detail tile.
#[derive(Default)]
struct PreviewHold {
    last: Option<(u64, ViewParams)>,
    last_detail: Option<DetailAsk>,
    last_inspect: Option<Recipe>,
    held: Option<DetailTile>,
    held_print: bool,
    /// The linear picture of the held tile, when it was rendered whole:
    /// the next tone, placing or stroke re-tones it instead of rendering.
    linear: Option<(LinearKey, LinearTile)>,
    hists: HistCache,
    tones: ToneHold,
    stages: PreviewStages,
}

/// Everything the linear picture of a detail tile depends on: the picture,
/// the ask, the geometry, the mix, and the print's fit when it is one —
/// not the placing, the tone or the strokes.
#[derive(Clone, PartialEq)]
struct LinearKey {
    session: u64,
    ask: DetailAsk,
    rotation: u8,
    angle: f64,
    crop: Option<CropRect>,
    weights: Weights,
    plate: Option<PlateWeights>,
    /// The print's recipe with what the tone stage reads taken out.
    print: Option<Recipe>,
}

impl LinearKey {
    fn of(req: &PreviewRequest, ask: DetailAsk) -> Self {
        let print = req.inspect.as_ref().map(|r| Recipe {
            place: 0.0,
            curve: Tone::default(),
            burns: Vec::new(),
            ..r.clone()
        });
        Self {
            session: req.session.id,
            ask,
            rotation: req.view.rotation,
            angle: req.view.angle,
            crop: req.view.crop,
            weights: req.view.weights,
            plate: req.view.plate.map(|p| p.weights),
            print,
        }
    }
}

fn start_preview(
    ctx: &egui::Context,
    latest: Arc<AtomicU64>,
) -> (Sender<PreviewRequest>, Receiver<PreviewResult>) {
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
                let session = req.session.id;
                let started = Instant::now();
                let stale = || latest.load(Ordering::Relaxed) != generation;
                let send = |r: PreviewResult| {
                    let _ = results.send(r);
                    ctx.request_repaint();
                };
                let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    paint_preview(&mut hold, req, &stale, &send)
                }));
                if let Err(payload) = outcome {
                    // The hold may be half updated. Drop it and start clean.
                    hold = PreviewHold::default();
                    send(PreviewResult {
                        generation,
                        session,
                        image: None,
                        detail: None,
                        ms: started.elapsed().as_millis(),
                        failed: Some(format!("stopped: {}", panic_message(&payload))),
                    });
                }
            }
        })
        .expect("spawn preview worker");
    (tx, rx)
}

/// The held window's samples may be reused only while they sit on the same
/// grid as the ask: the same exposure, the same kind of pixels (print or
/// file), and the same print. A new border, size, dpi or kernel refits the
/// picture, so the print's pixels move under the window; pieced together
/// with fresh strips, the old ones showed two pictures at two scales until
/// the view changed.
fn held_tile_stale(view_changed: bool, print_kind_changed: bool, inspect_changed: bool) -> bool {
    view_changed || print_kind_changed || inspect_changed
}

/// One request: the whole picture first, sent as soon as it is done, then
/// the detail window, unless a newer request has arrived meanwhile
/// (`stale`), which will ask for its own.
fn paint_preview(
    hold: &mut PreviewHold,
    req: PreviewRequest,
    stale: &dyn Fn() -> bool,
    send: &dyn Fn(PreviewResult),
) {
    let t = Instant::now();
    let key = (req.session.id, req.view.clone());
    let view_changed = hold.last.as_ref() != Some(&key);
    // The held window is the previous exposure. Drop it even when this
    // frame shows no sharp window, or the next zoom would reuse those pixels.
    if view_changed {
        hold.held = None;
    }
    let table = hold.tones.get(&req.view.tone);
    // Under a stroke being painted with a sharp window up, the window
    // covers what is seen: the small picture waits for the release (the
    // view differs then, and it is rendered), the window follows the hand.
    let window_covers = req.view.burn_live && req.detail.is_some();
    if view_changed && !window_covers {
        let image = render_preview(&req.session, &req.view, &mut hold.hists, &mut hold.stages, table);
        hold.last = Some(key);
        send(PreviewResult {
            generation: req.generation,
            session: req.session.id,
            image: Some(image),
            detail: None,
            ms: t.elapsed().as_millis(),
            failed: None,
        });
    }
    let Some(ask) = req.detail else {
        // No window asked for: what was held answers nothing now, so the
        // next ask renders. A print that changed here is written down as
        // the last one, and the next ask would find it unchanged: the
        // window held from the print before goes now.
        hold.last_detail = None;
        if hold.last_inspect != req.inspect {
            hold.held = None;
        }
        hold.last_inspect = req.inspect;
        return;
    };
    let detail_due = view_changed || hold.last_detail != req.detail || hold.last_inspect != req.inspect;
    if !detail_due {
        return;
    }
    if stale() {
        // A newer request will ask for its own window; this one's is not
        // rendered, and must not count as held.
        hold.last_detail = None;
        return;
    }
    let t = Instant::now();
    let print = req.inspect.is_some();
    if held_tile_stale(view_changed, print != hold.held_print, hold.last_inspect != req.inspect) {
        hold.held = None;
    }
    hold.held_print = print;
    let linear_key = LinearKey::of(&req, ask);
    let same_linear = hold.linear.as_ref().is_some_and(|(k, _)| *k == linear_key);
    let tile = if same_linear {
        // Only the tone, the placing or the strokes changed: re-tone the
        // held linear picture.
        let (_, lin) = hold.linear.as_ref().unwrap();
        let d = tone_tile(lin, &req.view, req.inspect.as_ref(), table);
        DetailTile { ask: d.ask, gray: d.gray, rgb: d.rgb }
    } else {
        // Rendered whole, the linear picture is kept; a strip of a pan is
        // not, and the next change renders whole once.
        let mut linear: Option<LinearTile> = None;
        let tile = if let Some(recipe) = &req.inspect {
            let recipe = recipe.clone();
            cover_detail(hold.held.as_ref(), ask, |sub| {
                let rect = DetailRect { x: sub.x, y: sub.y, w: sub.w, h: sub.h };
                let lin = render_print_linear(&req.session, &req.view, &recipe, rect)?;
                let d = tone_tile(&lin, &req.view, Some(&recipe), table);
                if sub == ask {
                    linear = Some(lin);
                }
                Some(d)
            })
        } else {
            cover_detail(hold.held.as_ref(), ask, |sub| {
                let lin = render_detail_linear(&req.session, &req.view, sub)?;
                let d = tone_tile(&lin, &req.view, None, table);
                if sub == ask {
                    linear = Some(lin);
                }
                Some(d)
            })
        };
        hold.linear = linear.map(|l| (linear_key, l));
        tile
    };
    let image = DetailImage { ask: tile.ask, gray: tile.gray.clone(), rgb: tile.rgb.clone() };
    hold.held = Some(tile);
    hold.last_detail = req.detail;
    hold.last_inspect = req.inspect;
    send(PreviewResult {
        generation: req.generation,
        session: req.session.id,
        image: None,
        detail: Some(image),
        ms: t.elapsed().as_millis(),
        failed: None,
    });
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

    /// A wider border refits the print: the held window's samples no longer
    /// sit on the picture's grid, and must not be pieced into the new one.
    #[test]
    fn a_new_print_drops_the_held_window() {
        assert!(held_tile_stale(false, false, true));
        assert!(held_tile_stale(true, false, false));
        assert!(held_tile_stale(false, true, false));
        // A pan of the same print reuses what is rendered.
        assert!(!held_tile_stale(false, false, false));
    }

    /// A small Bayer session of a grey ramp, top to bottom.
    fn grey_session() -> Session {
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
            illuminant: None,
            wb_as_shot: Some([1.0, 1.0, 1.0]),
            exposure: Default::default(),
            data: RawData::Integer(raw),
        };
        let params = NegativeParams { keep_separation: true, ..NegativeParams::default() };
        let negative = pipeline::negative_from_frame(&frame, &params, 0).unwrap();
        let preview = preview_planes(&negative).unwrap();
        let raw_hist = RawHistogram::of_frame(&frame);
        Session {
            id: next_session_id(),
            path: "grey.nef".into(),
            frame: Arc::new(frame),
            negative,
            camera_file: None,
            camera_file_path: None,
            develop: Develop::default(),
            preview,
            raw_hist,
        }
    }

    /// The 1:1 window of a print is asked for, the view goes back to the
    /// fit while the print gets another dpi, and the window is asked for
    /// again. The window held from the first print is on another grid:
    /// what comes back is the new print, as if nothing had been held.
    #[test]
    fn a_print_changed_while_no_window_is_up_drops_the_held_one() {
        let session = Arc::new(grey_session());
        let view = ViewParams {
            weights: Weights::default(),
            tone: Tone::paper(),
            place: 0.0,
            rotation: 0,
            angle: 0.0,
            crop: None,
            burns: Vec::new(),
            burn_live: false,
            plate: None,
        };
        let print = |dpi: f64| Recipe {
            size: Some(pr::PrintSize::parse("8x4.8cm").unwrap()),
            dpi,
            ..Recipe::on_paper()
        };
        let ask = DetailAsk { x: 8, y: 8, w: 48, h: 32, step: 1 };
        let window = |hold: &mut PreviewHold, detail: Option<DetailAsk>, dpi: f64, generation: u64| {
            let sent = std::cell::RefCell::new(Vec::new());
            let req = PreviewRequest {
                session: Arc::clone(&session),
                view: view.clone(),
                detail,
                inspect: Some(print(dpi)),
                generation,
            };
            paint_preview(hold, req, &|| false, &|r| sent.borrow_mut().push(r));
            sent.into_inner().into_iter().find_map(|r| r.detail).map(|d| d.gray)
        };

        let mut hold = PreviewHold::default();
        let first = window(&mut hold, Some(ask), 100.0, 1).expect("the first window");
        assert!(window(&mut hold, None, 200.0, 2).is_none());
        let again = window(&mut hold, Some(ask), 200.0, 3).expect("the window of the new print");

        let fresh = window(&mut PreviewHold::default(), Some(ask), 200.0, 1).expect("a fresh window");
        assert_ne!(first, fresh, "the two prints differ under the window");
        assert_eq!(again, fresh);

        // The same print asked for again is still served from what is held.
        assert!(window(&mut hold, None, 200.0, 4).is_none());
        assert!(hold.held.is_some());
    }

    fn export_job(session: &Arc<Session>, path: PathBuf) -> Job {
        Job::Export {
            session: Arc::clone(session),
            weights: Weights::default(),
            plate: None,
            recipe: Recipe::on_paper(),
            path,
        }
    }

    /// The window waits for an export that is counted. It is counted from
    /// the moment it is sent, and no longer when its queue is gone or the
    /// work on it panicked, or the window could never close.
    #[test]
    fn an_export_is_counted_while_it_is_on_its_way() {
        let session = Arc::new(grey_session());
        // A queue nobody works on: what is sent stays in it.
        let (tx, jobs) = channel::<Queued>();
        let (_events, rx) = channel::<Event>();
        let mut heavy = Heavy { tx, rx, ctx: egui::Context::default(), writing: Writing::default() };
        assert!(!heavy.writing());
        heavy.send(Job::Open {
            path: "none.nef".into(),
            develop: Develop::default(),
            cameras_dir: PathBuf::new(),
        });
        assert!(!heavy.writing(), "opening a file writes none");
        heavy.send(export_job(&session, "a.tif".into()));
        heavy.send(export_job(&session, "b.tif".into()));
        assert!(heavy.writing());
        let (_open, _) = jobs.recv().unwrap();
        let (_first, one) = jobs.recv().unwrap();
        drop(one);
        assert!(heavy.writing(), "the second is still queued");
        drop(jobs);
        assert!(!heavy.writing(), "nothing is written from a queue that is gone");

        let writing = Writing::default();
        let stopped = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let _one = writing.begin();
            assert!(writing.any());
            panic!("the export stopped");
        }));
        assert!(stopped.is_err());
        assert!(!writing.any());
    }

    /// Through the thread itself: when the UI hears that the file is
    /// written, or that it could not be, the export is no longer counted
    /// and the file is whole.
    #[test]
    fn an_export_is_no_longer_counted_when_its_event_arrives() {
        let session = Arc::new(grey_session());
        let dir = std::env::temp_dir().join(format!("mimizan-gui-writing-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut heavy = Heavy::spawn(egui::Context::default());
        let settled = |heavy: &mut Heavy| loop {
            match heavy.poll() {
                Some(Event::Status(_)) | None => std::thread::sleep(std::time::Duration::from_millis(2)),
                Some(ev) => return (ev, heavy.writing()),
            }
        };

        let path = dir.join("print.tif");
        heavy.send(export_job(&session, path.clone()));
        let (ev, writing) = settled(&mut heavy);
        assert!(matches!(ev, Event::Exported { .. }));
        assert!(!writing);
        let print = mimizan_core::tiffin::read_gray(&path).unwrap();
        assert!(print.plane.width > 0);

        // A folder that is not there: the export fails and is let go.
        heavy.send(export_job(&session, dir.join("none").join("print.tif")));
        let (ev, writing) = settled(&mut heavy);
        assert!(matches!(ev, Event::Failed(_)));
        assert!(!writing);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The export of a plate from a grey mosaic is a 16-bit RGB file whose
    /// three channels are equal, the same size the grey export has, and a
    /// blue lamp lifts the blue channel alone.
    #[test]
    fn plate_export_writes_grey_as_equal_channels() {
        let session = grey_session();
        let dir = std::env::temp_dir().join(format!("mimizan-gui-plate-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let quiet = |_s: String| {};
        let grey = dir.join("grey.tif");
        let recipe = Recipe::on_paper();
        let ev = export(&session, Weights::default(), recipe.clone(), &grey, &quiet).unwrap();
        let Event::Exported { width, height, .. } = ev else { panic!("not exported") };

        let plate =
            PlateView { weights: PlateWeights::channels(), space: PlateSpace::AdobeRgb, lamps: [0.0; 3] };
        let path = dir.join("plate.tif");
        let ev = export_plate(&session, Weights::default(), &plate, recipe.clone(), &path, &quiet).unwrap();
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
        export_plate(&session, Weights::default(), &lit, recipe, &path, &quiet).unwrap();
        let [r2, g2, b2] = mimizan_core::tiffin::read_rgb16(&path).unwrap();
        assert_eq!(r2.data, r.data);
        assert_eq!(g2.data, g.data);
        assert!(b2.data.iter().zip(&b.data).all(|(x, y)| x >= y));
        assert!(b2.data.iter().zip(&b.data).any(|(x, y)| x > y));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
