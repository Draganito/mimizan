//! Folder browser: the RAW files of one folder as a strip of grey
//! thumbnails. Scanning is synchronous (a directory listing); the
//! thumbnails come from a third worker thread that always works on the
//! newest priority list the strip sent, so the visible tiles fill first
//! and scrolling re-orders the queue instead of growing it.
//!
//! Every thumbnail is the mosaic binned to grey with the file's balance
//! and the camera file's weights (`mimizan_core::thumb`), cached on disk
//! as an 8-bit PNG keyed by path, size and modification time.

use mimizan_core::calib::CameraFile;
use mimizan_core::decode::Exposure;
use mimizan_core::pipeline;
use mimizan_core::thumb::{binned_gray, Thumb};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Instant, SystemTime};

/// Long edge of a thumbnail in pixels: enough for a 160-point tile on a
/// HiDPI screen.
pub const THUMB_LONG_EDGE: usize = 240;

/// Extensions rawler reads (lower case).
const RAW_EXTENSIONS: &[&str] = &[
    "nef", "nrw", "dng", "cr2", "cr3", "crw", "arw", "srf", "sr2", "raf", "rw2", "pef", "orf", "erf", "mef",
    "mos", "iiq", "3fr", "fff", "kdc", "dcr", "dcs", "mrw", "x3f", "tfr", "ari", "srw", "raw",
];

pub fn is_raw(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| RAW_EXTENSIONS.contains(&e.to_ascii_lowercase().as_str()))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, Default)]
pub enum Sort {
    #[default]
    Name,
    /// File modification time, newest first; a camera's file time is the capture time.
    Date,
}

impl Sort {
    pub const ALL: [Sort; 2] = [Sort::Name, Sort::Date];
    pub fn label(self) -> &'static str {
        match self {
            Sort::Name => "name",
            Sort::Date => "date",
        }
    }
}

/// What the thumbnail step learnt about the file.
#[derive(Clone, Debug)]
pub struct Meta {
    pub make: String,
    pub model: String,
    pub exposure: Exposure,
    pub width: usize,
    pub height: usize,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ThumbState {
    Pending,
    Ready,
    /// rawler could not read it; the tile stays, opening will show why.
    Failed,
}

pub struct Entry {
    pub path: PathBuf,
    pub name: String,
    pub size: u64,
    pub modified: SystemTime,
    pub thumb: Option<Thumb>,
    pub meta: Option<Meta>,
    pub state: ThumbState,
    /// Created when the tile is visible, dropped when it scrolls away.
    pub texture: Option<egui::TextureHandle>,
}

pub struct Folder {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    pub sort: Sort,
    /// Bumped on every scan so thumbnails of an old listing are ignored.
    pub generation: u64,
}

impl Folder {
    pub fn scan(path: &Path, sort: Sort, generation: u64) -> std::io::Result<Self> {
        let mut entries = Vec::new();
        for e in std::fs::read_dir(path)? {
            let e = e?;
            let p = e.path();
            if !p.is_file() || !is_raw(&p) {
                continue;
            }
            let md = e.metadata()?;
            entries.push(Entry {
                name: p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                size: md.len(),
                modified: md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                path: p,
                thumb: None,
                meta: None,
                state: ThumbState::Pending,
                texture: None,
            });
        }
        let mut f = Self { path: path.to_path_buf(), entries, sort, generation };
        f.resort(sort);
        Ok(f)
    }

    pub fn resort(&mut self, sort: Sort) {
        self.sort = sort;
        match sort {
            Sort::Name => self.entries.sort_by(|a, b| natural_cmp(&a.name, &b.name)),
            Sort::Date => self
                .entries
                .sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| natural_cmp(&a.name, &b.name))),
        }
    }

    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.path == path)
    }
}

/// `DSC_9.NEF` before `DSC_10.NEF`: digit runs compare by value.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    let (mut ia, mut ib) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ia.peek().copied(), ib.peek().copied()) {
            (None, None) => return std::cmp::Ordering::Equal,
            (None, Some(_)) => return std::cmp::Ordering::Less,
            (Some(_), None) => return std::cmp::Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let mut na = 0u128;
                while let Some(c) = ia.peek().filter(|c| c.is_ascii_digit()) {
                    na = na * 10 + c.to_digit(10).unwrap() as u128;
                    ia.next();
                }
                let mut nb = 0u128;
                while let Some(c) = ib.peek().filter(|c| c.is_ascii_digit()) {
                    nb = nb * 10 + c.to_digit(10).unwrap() as u128;
                    ib.next();
                }
                if na != nb {
                    return na.cmp(&nb);
                }
            }
            (Some(ca), Some(cb)) => {
                let (la, lb) = (ca.to_ascii_lowercase(), cb.to_ascii_lowercase());
                if la != lb {
                    return la.cmp(&lb);
                }
                ia.next();
                ib.next();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Disk cache

/// `$XDG_CACHE_HOME/mimizan/thumbs` or `~/.cache/mimizan/thumbs`.
pub fn cache_dir() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".cache")))?;
    Some(base.join("mimizan").join("thumbs"))
}

/// FNV-1a over path, size and modification time: a renamed or re-saved
/// file gets a new picture.
fn cache_key(path: &Path, size: u64, modified: SystemTime) -> String {
    let mut h: u64 = 0xcbf2_9ce4_8422_2325;
    let mut feed = |bytes: &[u8]| {
        for b in bytes {
            h ^= u64::from(*b);
            h = h.wrapping_mul(0x0100_0000_01b3);
        }
    };
    feed(path.to_string_lossy().as_bytes());
    feed(&size.to_le_bytes());
    let secs = modified.duration_since(SystemTime::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    feed(&secs.to_le_bytes());
    format!("{h:016x}")
}

#[derive(Serialize, Deserialize)]
struct CachedMeta {
    make: String,
    model: String,
    exposure: Exposure,
    width: usize,
    height: usize,
}

fn cache_load(dir: &Path, key: &str) -> Option<(Thumb, Meta)> {
    let png = image::open(dir.join(format!("{key}.png"))).ok()?.into_luma8();
    let meta: CachedMeta =
        serde_json::from_slice(&std::fs::read(dir.join(format!("{key}.json"))).ok()?).ok()?;
    let (w, h) = (png.width() as usize, png.height() as usize);
    Some((
        Thumb { width: w, height: h, gray: png.into_raw() },
        Meta {
            make: meta.make,
            model: meta.model,
            exposure: meta.exposure,
            width: meta.width,
            height: meta.height,
        },
    ))
}

fn cache_store(dir: &Path, key: &str, thumb: &Thumb, meta: &Meta) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let tmp = dir.join(format!("{key}.png.tmp"));
    let ok = image::save_buffer_with_format(
        &tmp,
        &thumb.gray,
        thumb.width as u32,
        thumb.height as u32,
        image::ColorType::L8,
        image::ImageFormat::Png,
    )
    .is_ok()
        && std::fs::rename(&tmp, dir.join(format!("{key}.png"))).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
        return;
    }
    let cm = CachedMeta {
        make: meta.make.clone(),
        model: meta.model.clone(),
        exposure: meta.exposure.clone(),
        width: meta.width,
        height: meta.height,
    };
    if let Ok(s) = serde_json::to_vec(&cm) {
        let _ = std::fs::write(dir.join(format!("{key}.json")), s);
    }
}

// ---------------------------------------------------------------------------
// Worker

#[derive(Clone)]
pub struct ThumbJob {
    pub generation: u64,
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

pub struct ThumbResult {
    pub generation: u64,
    pub path: PathBuf,
    pub result: Result<(Thumb, Meta), String>,
    pub ms: u128,
}

pub struct Thumbs {
    tx: Sender<Vec<ThumbJob>>,
    rx: Receiver<ThumbResult>,
}

impl Thumbs {
    pub fn spawn(ctx: egui::Context, cameras_dir: PathBuf) -> Self {
        let (tx, queues) = channel::<Vec<ThumbJob>>();
        let (results, rx) = channel::<ThumbResult>();
        let cache = cache_dir();
        thread::Builder::new()
            .name("mimizan-thumbs".into())
            .spawn(move || {
                let mut queue: Vec<ThumbJob> = Vec::new();
                let mut done: HashSet<(u64, PathBuf)> = HashSet::new();
                loop {
                    // Newest list wins; block only when there is nothing to do.
                    let newest = if queue.is_empty() {
                        match queues.recv() {
                            Ok(q) => Some(q),
                            Err(_) => return,
                        }
                    } else {
                        None
                    };
                    let mut latest = newest;
                    while let Ok(q) = queues.try_recv() {
                        latest = Some(q);
                    }
                    if let Some(q) = latest {
                        if let Some(first) = q.first() {
                            done.retain(|(g, _)| *g == first.generation);
                        }
                        queue = q;
                        queue.reverse(); // pop() takes the front
                    }
                    let Some(job) = queue.pop() else { continue };
                    if !done.insert((job.generation, job.path.clone())) {
                        continue;
                    }
                    let t = Instant::now();
                    let result = make_thumb(&job, cache.as_deref(), &cameras_dir);
                    let r = ThumbResult {
                        generation: job.generation,
                        path: job.path,
                        result,
                        ms: t.elapsed().as_millis(),
                    };
                    if results.send(r).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                }
            })
            .expect("spawn thumbs worker");
        Self { tx, rx }
    }

    /// Replace the worker's list: the first entries are wanted first.
    pub fn want(&self, jobs: Vec<ThumbJob>) {
        let _ = self.tx.send(jobs);
    }

    pub fn poll(&self) -> Vec<ThumbResult> {
        let mut all = Vec::new();
        while let Ok(r) = self.rx.try_recv() {
            all.push(r);
        }
        all
    }
}

fn make_thumb(job: &ThumbJob, cache: Option<&Path>, cameras_dir: &Path) -> Result<(Thumb, Meta), String> {
    let key = cache_key(&job.path, job.size, job.modified);
    if let Some(hit) = cache.and_then(|d| cache_load(d, &key)) {
        return Ok(hit);
    }
    let frame = pipeline::decode(&job.path).map_err(|e| e.to_string())?;
    let wb = frame.wb_as_shot.unwrap_or([1.0; 3]);
    let weights = CameraFile::find(cameras_dir, &frame.clean_make, &frame.clean_model)
        .and_then(|p| CameraFile::load(&p).ok())
        .map(|c| c.weights_balanced(wb))
        .unwrap_or_default();
    let thumb = binned_gray(&frame, weights, wb, THUMB_LONG_EDGE);
    let meta = Meta {
        make: frame.clean_make.clone(),
        model: frame.clean_model.clone(),
        exposure: frame.exposure.clone(),
        width: frame.crop.w,
        height: frame.crop.h,
    };
    if let Some(d) = cache {
        cache_store(d, &key, &thumb, &meta);
    }
    Ok((thumb, meta))
}

// ---------------------------------------------------------------------------
// Settings that survive a restart

#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
pub struct Persisted {
    pub folder: Option<PathBuf>,
    pub sort: Sort,
    /// Strip shown; `None` on a fresh install means "yes".
    pub strip: Option<bool>,
}

/// `$XDG_CONFIG_HOME/mimizan/gui.json` or `~/.config/mimizan/gui.json`.
pub fn config_path() -> Option<PathBuf> {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .filter(|p| p.is_absolute())
        .or_else(|| std::env::var_os("HOME").map(|h| PathBuf::from(h).join(".config")))?;
    Some(base.join("mimizan").join("gui.json"))
}

impl Persisted {
    pub fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    pub fn save(&self) {
        let Some(p) = config_path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_vec_pretty(self) {
            let _ = std::fs::write(p, s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_counts_digits() {
        let mut v = vec!["DSC_10.NEF", "DSC_9.NEF", "dsc_100.nef", "a.dng"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a.dng", "DSC_9.NEF", "DSC_10.NEF", "dsc_100.nef"]);
    }

    #[test]
    fn raw_extensions_are_case_insensitive() {
        assert!(is_raw(Path::new("x/DSC.NEF")));
        assert!(is_raw(Path::new("x/a.dng")));
        assert!(!is_raw(Path::new("x/a.jpg")));
        assert!(!is_raw(Path::new("x/noext")));
    }

    #[test]
    fn cache_key_changes_with_the_file() {
        let t = SystemTime::UNIX_EPOCH;
        let a = cache_key(Path::new("/a.nef"), 10, t);
        assert_eq!(a, cache_key(Path::new("/a.nef"), 10, t));
        assert_ne!(a, cache_key(Path::new("/b.nef"), 10, t));
        assert_ne!(a, cache_key(Path::new("/a.nef"), 11, t));
        assert_ne!(a, cache_key(Path::new("/a.nef"), 10, t + std::time::Duration::from_secs(1)));
    }
}
