//! Folder browser: the RAW files of one folder as a strip of grey
//! thumbnails. Scanning is synchronous (a directory listing); the
//! thumbnails come from their own worker thread that always works on the
//! newest priority list the strip sent, so the visible tiles fill first
//! and scrolling re-orders the queue instead of growing it.
//!
//! Every thumbnail is the mosaic binned to grey with the file's balance
//! and the camera file's weights (`mimizan_core::thumb`), cached on disk
//! as an 8-bit PNG keyed by path, size and modification time.

use mimizan_core::calib::CameraFile;
use mimizan_core::decode::Exposure;
use mimizan_core::pipeline;
use mimizan_core::plate::{PlateFilters, PlateSpace};
use mimizan_core::thumb::{binned, browser_plate, Thumb};
use serde::{Deserialize, Serialize};
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver, Sender};
use std::thread;
use std::time::{Instant, SystemTime};

/// Long edge of a thumbnail in pixels: enough for a 160-point tile on a
/// HiDPI screen.
const THUMB_LONG_EDGE: usize = 480;

/// Extensions rawler reads (lower case).
const RAW_EXTENSIONS: &[&str] = &[
    "nef", "nrw", "dng", "cr2", "cr3", "crw", "arw", "srf", "sr2", "raf", "rw2", "pef", "orf", "erf", "mef",
    "mos", "iiq", "3fr", "fff", "kdc", "dcr", "dcs", "mrw", "x3f", "tfr", "ari", "srw", "raw",
];

/// Does the extension name a RAW format (case-insensitive)?
fn is_raw(path: &Path) -> bool {
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

/// What the thumbnail step learnt about the file; also the JSON next to
/// the cached PNG.
#[derive(Clone, Debug, Serialize, Deserialize)]
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

/// One RAW file of the folder.
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

/// One folder's listing: its RAW files in the chosen order and its
/// visible subfolders.
pub struct Folder {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    pub sort: Sort,
    /// Bumped on every scan so thumbnails of an old listing are ignored.
    pub generation: u64,
    /// Visible subfolders (not hidden ones), natural order: the strip shows
    /// them as tiles before the frames.
    pub subdirs: Vec<SubDir>,
}

/// A subfolder tile. `raws` arrives from [`count_raws`] in the background.
pub struct SubDir {
    pub path: PathBuf,
    pub name: String,
    pub raws: Option<usize>,
}

impl Folder {
    /// List `path`: one `stat` per entry, and an entry that vanishes while
    /// the listing runs is skipped, not an error. Fails only if the folder
    /// itself cannot be read.
    pub fn scan(path: &Path, sort: Sort, generation: u64) -> std::io::Result<Self> {
        let mut entries = Vec::new();
        let mut subdirs = Vec::new();
        for e in std::fs::read_dir(path)? {
            let Ok(e) = e else { continue };
            let p = e.path();
            // Symlinks are resolved by `metadata`, so a linked folder or
            // file is taken for what it points to.
            let Ok(md) = std::fs::metadata(&p) else { continue };
            if md.is_dir() {
                let hidden = p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
                if !hidden {
                    subdirs.push(p);
                }
                continue;
            }
            if !md.is_file() || !is_raw(&p) {
                continue;
            }
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
        subdirs.sort_by(|a, b| natural_cmp(&dir_name(a), &dir_name(b)));
        let subdirs =
            subdirs.into_iter().map(|p| SubDir { name: dir_name(&p), path: p, raws: None }).collect();
        let mut f = Self { path: path.to_path_buf(), entries, sort, generation, subdirs };
        f.resort(sort);
        Ok(f)
    }

    /// Reorder the entries; the thumbnails stay with their files.
    pub fn resort(&mut self, sort: Sort) {
        self.sort = sort;
        match sort {
            Sort::Name => self.entries.sort_by(|a, b| natural_cmp(&a.name, &b.name)),
            Sort::Date => self
                .entries
                .sort_by(|a, b| b.modified.cmp(&a.modified).then_with(|| natural_cmp(&a.name, &b.name))),
        }
    }

    /// Position of the file in the current order.
    pub fn index_of(&self, path: &Path) -> Option<usize> {
        self.entries.iter().position(|e| e.path == path)
    }
}

/// Counts the RAW files of every folder in `dirs` on a thread and sends
/// `(generation, path, count)` as each is done, so a slow (network) folder
/// never stalls the strip. Dropping the receiver ends the thread quietly.
pub fn count_raws(generation: u64, dirs: Vec<PathBuf>) -> std::sync::mpsc::Receiver<(u64, PathBuf, usize)> {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::Builder::new()
        .name("mimizan-dir-count".into())
        .spawn(move || {
            for d in dirs {
                let n = std::fs::read_dir(&d)
                    .map(|rd| rd.filter_map(|e| e.ok()).filter(|e| is_raw(&e.path())).count())
                    .unwrap_or(0);
                if tx.send((generation, d, n)).is_err() {
                    return;
                }
            }
        })
        .ok();
    rx
}

/// Last path component as text (`/` for the root).
fn dir_name(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "/".to_string())
}

/// `DSC_9.NEF` before `DSC_10.NEF`: digit runs compare by value. The runs
/// are compared as strings (leading zeros stripped, then by length, then
/// lexically), so a run of any length is fine.
fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    fn digits(s: &[u8], from: usize) -> usize {
        from + s[from..].iter().take_while(|c| c.is_ascii_digit()).count()
    }
    fn trim(s: &[u8]) -> &[u8] {
        let z = s.iter().take_while(|&&c| c == b'0').count();
        &s[z.min(s.len().saturating_sub(1))..]
    }
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let (mut i, mut j) = (0, 0);
    loop {
        match (a.get(i), b.get(j)) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(ca), Some(cb)) if ca.is_ascii_digit() && cb.is_ascii_digit() => {
                let (ea, eb) = (digits(a, i), digits(b, j));
                let (ra, rb) = (trim(&a[i..ea]), trim(&b[j..eb]));
                let ord = ra.len().cmp(&rb.len()).then_with(|| ra.cmp(rb));
                if ord != Ordering::Equal {
                    return ord;
                }
                i = ea;
                j = eb;
            }
            (Some(ca), Some(cb)) => {
                let (la, lb) = (ca.to_ascii_lowercase(), cb.to_ascii_lowercase());
                if la != lb {
                    return la.cmp(&lb);
                }
                i += 1;
                j += 1;
            }
        }
    }
}

// ---------------------------------------------------------------------------
// Disk cache

/// Login directory. `$HOME` on Linux. On Windows, `$USERPROFILE` when a
/// Unix shell has not set `$HOME`.
pub(crate) fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .filter(|h| !h.is_empty())
        .map(PathBuf::from)
        .or_else(|| std::env::var_os("USERPROFILE").filter(|h| !h.is_empty()).map(PathBuf::from))
}

/// `$XDG_*` when it is absolute, otherwise the Windows known folder, otherwise
/// `$HOME/<fallback>`.
fn xdg_or_home(xdg: &str, windows: &str, fallback: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(xdg).map(PathBuf::from).filter(|p| p.is_absolute()) {
        return Some(path);
    }
    if cfg!(windows) {
        if let Some(path) = std::env::var_os(windows).filter(|h| !h.is_empty()) {
            return Some(PathBuf::from(path));
        }
    }
    Some(home_dir()?.join(fallback))
}

/// `$XDG_CACHE_HOME/mimizan/thumbs`, `%LOCALAPPDATA%/mimizan/thumbs`, or
/// `~/.cache/mimizan/thumbs`.
fn cache_dir() -> Option<PathBuf> {
    Some(xdg_or_home("XDG_CACHE_HOME", "LOCALAPPDATA", ".cache")?.join("mimizan").join("thumbs"))
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
    // A larger thumbnail size must not pick up the old, smaller pictures.
    feed(&(THUMB_LONG_EDGE as u64).to_le_bytes());
    format!("{h:016x}")
}

/// The cached grey and colour PNGs and the JSON, if all three are there and
/// readable and the same size. A cache from before the colour picture has
/// no `.rgb.png` and is made again.
fn cache_load(dir: &Path, key: &str) -> Option<(Thumb, Meta)> {
    let png = image::open(dir.join(format!("{key}.png"))).ok()?.into_luma8();
    let rgb = image::open(dir.join(format!("{key}.rgb.png"))).ok()?.into_rgb8();
    let meta: Meta = serde_json::from_slice(&std::fs::read(dir.join(format!("{key}.json"))).ok()?).ok()?;
    let (w, h) = (png.width() as usize, png.height() as usize);
    if (rgb.width() as usize, rgb.height() as usize) != (w, h) {
        return None;
    }
    Some((Thumb { width: w, height: h, gray: png.into_raw(), rgb: rgb.into_raw() }, meta))
}

/// Write `name` under `dir` through a temporary file and a rename, so a
/// reader never sees a half-written file.
fn write_atomic(dir: &Path, name: &str, write: impl FnOnce(&Path) -> bool) -> bool {
    let tmp = dir.join(format!("{name}.tmp"));
    let ok = write(&tmp) && std::fs::rename(&tmp, dir.join(name)).is_ok();
    if !ok {
        let _ = std::fs::remove_file(&tmp);
    }
    ok
}

/// Cache a thumbnail; failures are silent, the cache is only a shortcut.
fn cache_store(dir: &Path, key: &str, thumb: &Thumb, meta: &Meta) {
    if std::fs::create_dir_all(dir).is_err() {
        return;
    }
    let png_ok = write_atomic(dir, &format!("{key}.png"), |tmp| {
        image::save_buffer_with_format(
            tmp,
            &thumb.gray,
            thumb.width as u32,
            thumb.height as u32,
            image::ColorType::L8,
            image::ImageFormat::Png,
        )
        .is_ok()
    });
    let rgb_ok = png_ok
        && write_atomic(dir, &format!("{key}.rgb.png"), |tmp| {
            image::save_buffer_with_format(
                tmp,
                &thumb.rgb,
                thumb.width as u32,
                thumb.height as u32,
                image::ColorType::Rgb8,
                image::ImageFormat::Png,
            )
            .is_ok()
        });
    if !rgb_ok {
        return;
    }
    if let Ok(s) = serde_json::to_vec(meta) {
        write_atomic(dir, &format!("{key}.json"), |tmp| std::fs::write(tmp, &s).is_ok());
    }
}

// ---------------------------------------------------------------------------
// Worker

/// One thumbnail to make; `size` and `modified` key the disk cache.
pub struct ThumbJob {
    pub generation: u64,
    pub path: PathBuf,
    pub size: u64,
    pub modified: SystemTime,
}

/// One finished thumbnail (or the reason it failed).
pub struct ThumbResult {
    pub generation: u64,
    pub path: PathBuf,
    pub result: Result<(Thumb, Meta), String>,
    pub ms: u128,
}

/// Handle to the thumbnail thread. The thread is restarted if it ever
/// dies, so the strip never stays blank.
pub struct Thumbs {
    tx: Sender<Vec<ThumbJob>>,
    rx: Receiver<ThumbResult>,
    ctx: egui::Context,
    cameras_dir: PathBuf,
}

impl Thumbs {
    /// Start the thread.
    pub fn spawn(ctx: egui::Context, cameras_dir: PathBuf) -> Self {
        let (tx, rx) = start_thumbs(&ctx, &cameras_dir);
        Self { tx, rx, ctx, cameras_dir }
    }

    /// Replace the worker's list: the first entries are wanted first.
    pub fn want(&mut self, jobs: Vec<ThumbJob>) {
        if let Err(std::sync::mpsc::SendError(jobs)) = self.tx.send(jobs) {
            self.restart();
            let _ = self.tx.send(jobs);
        }
    }

    /// Every result that arrived since the last poll.
    pub fn poll(&mut self) -> Vec<ThumbResult> {
        let mut all = Vec::new();
        loop {
            match self.rx.try_recv() {
                Ok(r) => all.push(r),
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    self.restart();
                    break;
                }
            }
        }
        all
    }

    fn restart(&mut self) {
        tracing::error!("thumbnails stopped; starting them again");
        let (tx, rx) = start_thumbs(&self.ctx, &self.cameras_dir);
        self.tx = tx;
        self.rx = rx;
    }
}

fn start_thumbs(ctx: &egui::Context, cameras_dir: &Path) -> (Sender<Vec<ThumbJob>>, Receiver<ThumbResult>) {
    let (tx, queues) = channel::<Vec<ThumbJob>>();
    let (results, rx) = channel::<ThumbResult>();
    let ctx = ctx.clone();
    let cameras_dir = cameras_dir.to_path_buf();
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
                let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    make_thumb(&job, cache.as_deref(), &cameras_dir)
                }));
                let result = match result {
                    Ok(r) => r,
                    Err(payload) => Err(format!("stopped: {}", crate::worker::panic_message(&payload))),
                };
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
    (tx, rx)
}

/// Cache hit, or decode and bin the file and cache the result.
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
    let thumb = binned(&frame, weights, &browser_plate(&frame), wb, THUMB_LONG_EDGE);
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

/// `gui.json`. Every field has a default, so a file from an older or newer
/// version still loads.
#[derive(Serialize, Deserialize, Default, Clone, Debug, PartialEq)]
#[serde(default)]
pub struct Persisted {
    pub folder: Option<PathBuf>,
    pub sort: Sort,
    /// Strip shown; `None` on a fresh install means "yes".
    pub strip: Option<bool>,
    /// Strip tiles shown as negatives (the film), not as the print, in the
    /// monochrome process. `None` means yes.
    pub strip_negative: Option<bool>,
    /// The same for the plate: inverted colour (a colour negative without
    /// its mask) or the slide. `None` means the slide.
    pub strip_negative_plate: Option<bool>,
    /// Docked windows open: 0 Negative, 1 Print, 2 Enlarger. `None` means all.
    pub panels: Option<[bool; 3]>,
    /// Height of the strip panel in points (the user drags its top edge).
    pub strip_height: Option<f32>,
    /// The process a file opens in when it has no recipe: the plate
    /// (`true`) or the monochrome negative. `None` means monochrome.
    pub plate: Option<bool>,
    /// The plate's taking filters and primaries last used.
    pub plate_filters: Option<PlateFilters>,
    pub plate_space: Option<PlateSpace>,
}

/// `$XDG_CONFIG_HOME/mimizan/gui.json`, `%APPDATA%/mimizan/gui.json`, or
/// `~/.config/mimizan/gui.json`.
fn config_path() -> Option<PathBuf> {
    Some(xdg_or_home("XDG_CONFIG_HOME", "APPDATA", ".config")?.join("mimizan").join("gui.json"))
}

impl Persisted {
    /// The saved settings, or the defaults when there is no readable file.
    pub fn load() -> Self {
        config_path()
            .and_then(|p| std::fs::read(p).ok())
            .and_then(|b| serde_json::from_slice(&b).ok())
            .unwrap_or_default()
    }

    /// Write the settings; a failure is silent (read-only home, no config dir).
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
        let mut v = vec!["DSC_10.NEF", "DSC_9.NEF", "dsc_100.nef", "a.dng", "DSC_009.NEF"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["a.dng", "DSC_9.NEF", "DSC_009.NEF", "DSC_10.NEF", "dsc_100.nef"]);
        // Digit runs longer than any machine integer still order by value.
        let long_a = format!("x{}.nef", "9".repeat(60));
        let long_b = format!("x1{}.nef", "0".repeat(60));
        assert_eq!(natural_cmp(&long_a, &long_b), std::cmp::Ordering::Less);
        assert_eq!(natural_cmp(&long_b, &long_a), std::cmp::Ordering::Greater);
        assert_eq!(natural_cmp("a0", "a00"), std::cmp::Ordering::Equal);
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
