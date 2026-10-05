//! Two background threads. `Heavy` decodes, separates and exports (seconds);
//! `Preview` re-mixes the downscaled separation (milliseconds) and always
//! works on the newest request only.

use crate::session::{
    preview_planes, render_detail, render_preview, DetailImage, DetailRect, DevelopParams, HistCache,
    PreviewImage, Session, ViewParams,
};
use anyhow::{Context, Result};
use mimizan_core::calib::{CameraFile, LookFile};
use mimizan_core::mix::{mix, Weights};
use mimizan_core::pipeline::{self, Negative};
use mimizan_core::print::{self as pr, PrintParams, PrintSize, PrintSource};
use mimizan_core::screen::ScreenSharpen;
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
    /// One export, two files: `path` (16-bit TIFF, full resolution, look
    /// applied, unsharpened) and `<stem>_<px>px.jpg` (long edge `screen_px`,
    /// screen compensation, quality 100).
    Export {
        session: Arc<Session>,
        weights: Weights,
        look: LookFile,
        path: PathBuf,
        screen_px: usize,
        /// TIFF orientation to apply (the file's, plus the user's turns).
        orientation: u16,
    },
}

pub enum Event {
    Status(String),
    Opened(Arc<Session>),
    Exported {
        tiff: PathBuf,
        jpeg: PathBuf,
        width: usize,
        height: usize,
        jpeg_width: usize,
        jpeg_height: usize,
        ms: u128,
    },
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
                        Job::Export { session, weights, look, path, screen_px, orientation } => {
                            export(&session, weights, look, &path, screen_px, orientation, &status)
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
    info!("preview planes {} ms", t.elapsed().as_millis());
    Ok(Session { path, frame, negative, camera_file, camera_file_path, develop, preview })
}

/// JPEG path next to the TIFF: `<stem>_<px>px.jpg`.
pub fn jpeg_path(tiff: &Path, screen_px: usize) -> PathBuf {
    let stem = tiff.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "mimizan".into());
    tiff.with_file_name(format!("{stem}_{screen_px}px.jpg"))
}

fn export(
    s: &Session,
    weights: Weights,
    look: LookFile,
    path: &Path,
    screen_px: usize,
    orientation: u16,
    status: &dyn Fn(String),
) -> Result<Event> {
    let t = Instant::now();
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

    // 1. Full resolution, look applied, nothing sharpened: the file to work on.
    let tiff_params = PrintParams { look: look.clone(), ..Default::default() };
    status("rendering TIFF".into());
    let tiff = pr::render_planes(&src, &tiff_params);
    status(format!("writing {}", path.display()));
    pr::write(&tiff, path, &tiff_params)?;
    let (width, height) = (tiff.info.width, tiff.info.height);
    drop(tiff);

    // 2. Screen JPEG: long edge, exact compensation of the known losses.
    let jpeg = jpeg_path(path, screen_px);
    let jpeg_params = PrintParams {
        look,
        size: Some(PrintSize::LongEdgePx(screen_px)),
        screen: Some(ScreenSharpen::default()),
        ..Default::default()
    };
    status("rendering JPEG".into());
    let out = pr::render_planes(&src, &jpeg_params);
    drop(plane);
    drop(mask);
    status(format!("writing {}", jpeg.display()));
    pr::write(&out, &jpeg, &jpeg_params)?;
    Ok(Event::Exported {
        tiff: path.to_path_buf(),
        jpeg,
        width,
        height,
        jpeg_width: out.info.width,
        jpeg_height: out.info.height,
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
