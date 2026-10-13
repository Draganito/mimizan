//! Background threads. `Heavy` decodes, separates and exports (seconds);
//! `Preview` re-mixes the downscaled separation (milliseconds) and always
//! works on the newest request only. `Loupe` renders one sharpened print
//! box the same way.

use crate::session::{
    preview_planes, render_detail, render_preview, render_sharpen_loupe, DetailImage, DetailRect,
    DevelopParams, HistCache, PreviewImage, RawHistogram, Session, SharpenLoupe, ViewParams,
};
use anyhow::{Context, Result};
use mimizan_core::calib::CameraFile;
use mimizan_core::mix::{mix, Weights};
use mimizan_core::pipeline::{self, Negative};
use mimizan_core::print::{self as pr, PrintParams, PrintSource};
use mimizan_core::recipe::Recipe;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender, TryRecvError};
use std::sync::Arc;
use std::thread;
use std::time::Instant;
use tracing::info;

pub enum Job {
    Open {
        path: PathBuf,
        develop: DevelopParams,
        cameras_dir: PathBuf,
    },
    Redevelop {
        session: Arc<Session>,
        develop: DevelopParams,
    },
    /// One export, one file: `path` (16-bit TIFF, the recipe rendered in one
    /// pass against the negative: placing, paper or look, size, border and
    /// sharpening as set).
    Export {
        session: Arc<Session>,
        weights: Weights,
        recipe: Recipe,
        path: PathBuf,
        /// TIFF orientation to apply (the file's, plus the user's turns).
        orientation: u16,
    },
}

pub enum Event {
    Status(String),
    Opened(Arc<Session>),
    Exported { tiff: PathBuf, width: usize, height: usize, ms: u128 },
    Failed(String),
}

pub struct Heavy {
    tx: Sender<Job>,
    rx: Receiver<Event>,
}

impl Heavy {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (tx, jobs) = channel::<Job>();
        let (events, rx) = channel::<Event>();
        thread::Builder::new()
            .name("mimizan-heavy".into())
            .spawn(move || {
                while let Ok(job) = jobs.recv() {
                    let status = |s: String| {
                        let _ = events.send(Event::Status(s));
                        ctx.request_repaint();
                    };
                    let r = match job {
                        Job::Open { path, develop, cameras_dir } => {
                            open(&path, develop, &cameras_dir, &status).map(|s| Event::Opened(Arc::new(s)))
                        }
                        Job::Redevelop { session, develop } => {
                            redevelop(&session, develop, &status).map(|s| Event::Opened(Arc::new(s)))
                        }
                        Job::Export { session, weights, recipe, path, orientation } => {
                            export(&session, weights, recipe, &path, orientation, &status)
                        }
                    };
                    let ev = match r {
                        Ok(ev) => ev,
                        Err(e) => Event::Failed(format!("{e:#}")),
                    };
                    let _ = events.send(ev);
                    ctx.request_repaint();
                }
            })
            .expect("spawn heavy worker");
        Self { tx, rx }
    }

    pub fn send(&self, job: Job) {
        let _ = self.tx.send(job);
    }

    pub fn poll(&self) -> Option<Event> {
        match self.rx.try_recv() {
            Ok(e) => Some(e),
            Err(TryRecvError::Empty) | Err(TryRecvError::Disconnected) => None,
        }
    }
}

fn open(path: &Path, develop: DevelopParams, cameras_dir: &Path, status: &dyn Fn(String)) -> Result<Session> {
    status(format!("decoding {}", path.display()));
    let t = Instant::now();
    let frame = pipeline::decode(path).with_context(|| format!("decoding {}", path.display()))?;
    let decode_ms = t.elapsed().as_millis();
    let frame = Arc::new(frame);

    let camera_file_path = CameraFile::find(cameras_dir, &frame.clean_make, &frame.clean_model);
    let camera_file = match &camera_file_path {
        Some(p) => Some(CameraFile::load(p).with_context(|| format!("camera file {}", p.display()))?),
        None => None,
    };
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
    status(match develop.converter {
        crate::session::Converter::Mimizan => "separating".into(),
        other => format!("demosaicing with {}", other.label()),
    });
    let params = develop.negative_params(camera_file.clone());
    let negative: Negative = crate::convert::develop(&frame, &params, decode_ms, develop.converter)?;
    status("building preview".into());
    let t = Instant::now();
    let preview = preview_planes(&negative);
    let raw_hist = RawHistogram::of_frame(&frame);
    info!("preview planes + raw histogram {} ms", t.elapsed().as_millis());
    Ok(Session { path, frame, negative, camera_file, camera_file_path, develop, preview, raw_hist })
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
    status("mixing".into());
    let plane = mix(s.separation(), &weights);
    let mask = s.mask_plane();
    let mut info = s.negative.info.clone();
    info.weights = weights;
    info.weights_raw = weights.balanced_to_raw(s.wb());
    let src = PrintSource {
        plane: &plane,
        mask: Some(&mask),
        orientation,
        name: s.path.display().to_string(),
        description: serde_json::to_value(&info).ok(),
    };

    // The print: the recipe in one pass against the negative.
    let params = PrintParams { recipe, ..Default::default() };
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

pub struct PreviewRequest {
    pub session: Arc<Session>,
    pub view: ViewParams,
    /// Full-resolution window the zoomed view needs, if any.
    pub detail: Option<DetailRect>,
    pub generation: u64,
}

pub struct PreviewResult {
    pub generation: u64,
    /// New whole-image preview, when the view changed.
    pub image: Option<PreviewImage>,
    /// New 1:1 window, when one was requested and changed.
    pub detail: Option<DetailImage>,
    pub ms: u128,
}

pub struct Preview {
    tx: Sender<PreviewRequest>,
    rx: Receiver<PreviewResult>,
}

impl Preview {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (tx, reqs) = channel::<PreviewRequest>();
        let (results, rx) = channel::<PreviewResult>();
        thread::Builder::new()
            .name("mimizan-preview".into())
            .spawn(move || {
                // What the last render was based on, so a pan only re-renders
                // the 1:1 window and a slider only the whole image.
                let mut last: Option<(*const Session, ViewParams)> = None;
                let mut last_detail: Option<DetailRect> = None;
                let mut hists = HistCache::default();
                while let Ok(mut req) = reqs.recv() {
                    // Coalesce: only the newest request matters.
                    while let Ok(newer) = reqs.try_recv() {
                        req = newer;
                    }
                    let t = Instant::now();
                    let key = (Arc::as_ptr(&req.session), req.view.clone());
                    let view_changed = last.as_ref() != Some(&key);
                    let image = view_changed.then(|| render_preview(&req.session, &req.view, &mut hists));
                    let detail = match req.detail {
                        Some(r) if view_changed || last_detail != Some(r) => {
                            render_detail(&req.session, &req.view, r)
                        }
                        _ => None,
                    };
                    last = Some(key);
                    last_detail = req.detail;
                    if image.is_none() && detail.is_none() {
                        continue;
                    }
                    let r = PreviewResult {
                        generation: req.generation,
                        image,
                        detail,
                        ms: t.elapsed().as_millis(),
                    };
                    if results.send(r).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("spawn preview worker");
        Self { tx, rx }
    }

    pub fn request(&self, r: PreviewRequest) {
        let _ = self.tx.send(r);
    }

    /// All finished results since the last poll, oldest first (an image and
    /// a window may arrive as separate results).
    pub fn poll(&self) -> Vec<PreviewResult> {
        let mut all = Vec::new();
        while let Ok(r) = self.rx.try_recv() {
            all.push(r);
        }
        all
    }
}

/// One 1:1 box of the print. Coalesced like the preview: a slider drag
/// keeps only the newest point and recipe.
pub struct LoupeRequest {
    pub session: Arc<Session>,
    pub weights: Weights,
    pub recipe: Recipe,
    pub orientation: u16,
    pub nx: f64,
    pub ny: f64,
    pub generation: u64,
}

pub struct LoupeResult {
    pub generation: u64,
    pub image: SharpenLoupe,
}

pub struct Loupe {
    tx: Sender<LoupeRequest>,
    rx: Receiver<LoupeResult>,
}

impl Loupe {
    pub fn spawn(ctx: egui::Context) -> Self {
        let (tx, reqs) = channel::<LoupeRequest>();
        let (results, rx) = channel::<LoupeResult>();
        thread::Builder::new()
            .name("mimizan-loupe".into())
            .spawn(move || {
                while let Ok(mut req) = reqs.recv() {
                    while let Ok(newer) = reqs.try_recv() {
                        req = newer;
                    }
                    let generation = req.generation;
                    // A panic used to kill this thread. The box then kept the
                    // last texture forever, and every later size, dpi or
                    // kernel change looked frozen.
                    let rendered = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        render_sharpen_loupe(
                            &req.session,
                            &req.weights,
                            &req.recipe,
                            req.orientation,
                            req.nx,
                            req.ny,
                        )
                    }));
                    let image = match rendered {
                        Ok(image) => image,
                        Err(payload) => {
                            tracing::error!("loupe render failed: {}", panic_message(&payload));
                            SharpenLoupe { gray: Vec::new(), width: 0, height: 0 }
                        }
                    };
                    if results.send(LoupeResult { generation, image }).is_err() {
                        break;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("spawn loupe worker");
        Self { tx, rx }
    }

    pub fn request(&self, r: LoupeRequest) {
        let _ = self.tx.send(r);
    }

    /// The newest finished box since the last poll. Older ones are dropped:
    /// a slider drag may have completed several, and only the latest matches
    /// the controls.
    pub fn poll(&self) -> Option<LoupeResult> {
        let mut last = None;
        while let Ok(r) = self.rx.try_recv() {
            last = Some(r);
        }
        last
    }
}

fn panic_message(payload: &(dyn std::any::Any + Send)) -> String {
    if let Some(s) = payload.downcast_ref::<&str>() {
        (*s).to_string()
    } else if let Some(s) = payload.downcast_ref::<String>() {
        s.clone()
    } else {
        "unknown panic".to_string()
    }
}
