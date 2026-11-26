//! Folder browser: the RAW files of one folder as a strip of grey
//! thumbnails. The listing runs on its own thread ([`list`]); the frame
//! takes the finished folder from a channel, so a slow disk never holds
//! the window. The thumbnails come from their own worker thread that
//! always works on the newest priority list the strip sent, so the
//! visible tiles fill first and scrolling re-orders the queue instead
//! of growing it.
//!
//! Every thumbnail is the mosaic binned to grey with the file's balance
//! and the camera file's weights (`mimizan_core::thumb`), cached on disk
//! as an 8-bit PNG keyed by path, size and modification time. An X-Trans
//! RAF that has no DNG beside it is the camera's JPEG instead
//! (`fuji`); once a DNG this program wrote is there the RAF leaves the
//! strip. A DNG from another program leaves the RAF where it is. A Bayer
//! least-squares DNG is the preview it already carries, not a second
//! decode of its three full-size channels.

use mimizan_core::calib::CameraFile;
use mimizan_core::decode::Exposure;
use mimizan_core::pipeline;
use mimizan_core::plate::{PlateFilters, PlateSpace};
use mimizan_core::thumb::{binned, browser_plate, Thumb};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
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

/// Does the extension name a RAW format (case-insensitive)? A hidden file
/// is none: on a FAT or exFAT card macOS writes `._DSC_1.NEF` beside each
/// RAW, with the RAW's extension and none of its pixels.
pub(crate) fn is_raw(path: &Path) -> bool {
    let hidden = path.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with('.'));
    !hidden
        && path
            .extension()
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
    /// The tile is the camera's JPEG, and opening this file writes the
    /// DNG. The memory estimate is that writing. An older cache leaves it
    /// off; the worker still measures the writing itself.
    #[serde(default)]
    pub xtrans: bool,
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
    /// Which asking this tile is. The thumbnail thread makes a file once
    /// for each asking of one folder scan, so a tile whose pixels were let
    /// go, and a file put back after it had left, is pictured again.
    pub ask: u32,
}

impl Entry {
    /// A file the strip has not pictured yet. `ask` is this asking's
    /// number ([`Folder::next_ask`]).
    fn pending(path: &Path, ask: u32) -> Option<Self> {
        let md = std::fs::metadata(path).ok()?;
        if !md.is_file() {
            return None;
        }
        Some(Self {
            name: path.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
            size: md.len(),
            modified: md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
            path: path.to_path_buf(),
            thumb: None,
            meta: None,
            state: ThumbState::Pending,
            texture: None,
            ask,
        })
    }
}

/// One folder's listing: its RAW files in the chosen order and its
/// visible subfolders.
pub struct Folder {
    pub path: PathBuf,
    pub entries: Vec<Entry>,
    /// Files the checkbox is not showing. The picture stays with the file,
    /// so turning the checkbox the other way shows the tile at once.
    pub aside: Vec<Entry>,
    /// Bumped on every scan so thumbnails of an old listing are ignored.
    pub generation: u64,
    /// The last asking handed out in this scan. A let-go and a file put
    /// back both take the next value, so two askings of one path never
    /// share a number.
    pub asks: u32,
    /// RAWs hidden while least squares is on.
    gives_way_on: HashSet<PathBuf>,
    /// `.mimizan.dng` files hidden while it is off.
    gives_way_off: HashSet<PathBuf>,
    /// What each listed `.mimizan.dng` was written from.
    sources: HashMap<PathBuf, crate::bayer::DngSource>,
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
    /// List `path`. An entry that vanishes while the listing runs is
    /// skipped, not an error. Fails only if the folder itself cannot be
    /// read. A name that is neither a folder nor a RAW is not stated: a
    /// JPEG or a sidecar beside the files costs no further call. A link
    /// is followed, so a linked folder or file is taken for what it
    /// points to.
    pub fn scan(path: &Path, sort: Sort, generation: u64) -> std::io::Result<Self> {
        let mut entries = Vec::new();
        let mut subdirs = Vec::new();
        let mut keep_dir = |p: PathBuf| {
            let hidden = p.file_name().is_some_and(|n| n.to_string_lossy().starts_with('.'));
            if !hidden {
                subdirs.push(p);
            }
        };
        let mut keep_file = |p: PathBuf, md: std::fs::Metadata| {
            entries.push(Entry {
                name: p.file_name().map(|f| f.to_string_lossy().to_string()).unwrap_or_default(),
                size: md.len(),
                modified: md.modified().unwrap_or(SystemTime::UNIX_EPOCH),
                path: p,
                thumb: None,
                meta: None,
                state: ThumbState::Pending,
                texture: None,
                ask: 0,
            });
        };
        for e in std::fs::read_dir(path)? {
            let Ok(e) = e else { continue };
            let p = e.path();
            let Ok(kind) = e.file_type() else { continue };
            if kind.is_symlink() {
                let Ok(md) = std::fs::metadata(&p) else { continue };
                if md.is_dir() {
                    keep_dir(p);
                } else if md.is_file() && is_raw(&p) {
                    keep_file(p, md);
                }
                continue;
            }
            if kind.is_dir() {
                keep_dir(p);
                continue;
            }
            if !kind.is_file() || !is_raw(&p) {
                continue;
            }
            let Ok(md) = e.metadata() else { continue };
            keep_file(p, md);
        }
        subdirs.sort_by(|a, b| natural_cmp(&dir_name(a), &dir_name(b)));
        let subdirs =
            subdirs.into_iter().map(|p| SubDir { name: dir_name(&p), path: p, raws: None }).collect();
        let mut f = Self {
            path: path.to_path_buf(),
            entries,
            aside: Vec::new(),
            generation,
            asks: 0,
            gives_way_on: HashSet::new(),
            gives_way_off: HashSet::new(),
            sources: HashMap::new(),
            subdirs,
        };
        f.resort(sort);
        Ok(f)
    }

    /// Reorder the entries; the thumbnails stay with their files.
    pub fn resort(&mut self, sort: Sort) {
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

    /// The next asking in this scan. Two askings of one path never carry
    /// the same number, so the thumbnail thread makes the later one.
    pub fn next_ask(&mut self) -> u32 {
        self.asks = self.asks.wrapping_add(1);
        self.asks
    }

    /// Remember which files give way, from the listing already in hand.
    /// `current` and `bayer` are the two questions that still read a file.
    pub fn pair(&mut self, current: impl Fn(&Path) -> bool, bayer: impl Fn(&Path) -> bool) {
        let paths: Vec<PathBuf> =
            self.entries.iter().chain(self.aside.iter()).map(|e| e.path.clone()).collect();
        let decided = crate::bayer::pairs(&paths, current, bayer);
        self.gives_way_on = decided.gives_way_on.into_iter().collect();
        self.gives_way_off = decided.gives_way_off.into_iter().collect();
        self.sources = decided.sources.into_iter().collect();
    }

    /// Show the files the checkbox opens. `on` hides the RAWs that give
    /// way to their DNG; off hides those DNGs. The pictures move with the
    /// files. Nothing is read from the disk.
    pub fn retile(&mut self, on: bool, sort: Sort) {
        let mut all = std::mem::take(&mut self.entries);
        all.append(&mut self.aside);
        let hide = if on { &self.gives_way_on } else { &self.gives_way_off };
        let (shown, hidden): (Vec<Entry>, Vec<Entry>) =
            all.into_iter().partition(|e| !hide.contains(&e.path));
        self.entries = shown;
        self.aside = hidden;
        self.resort(sort);
    }

    /// A finished thumbnail goes to its file, shown or aside: a file the
    /// checkbox moved aside while its picture was being made has asked
    /// under this number already, and would stay "…" when it came back.
    /// An older scan's result is ignored.
    pub fn take_thumb(&mut self, r: ThumbResult) {
        if r.generation != self.generation {
            return;
        }
        let entry = self.entries.iter_mut().chain(self.aside.iter_mut()).find(|e| e.path == r.path);
        match r.result {
            Ok(ThumbReady::Covered) => {
                self.entries.retain(|e| e.path != r.path);
            }
            Ok(ThumbReady::Picture(thumb, meta)) => {
                if let Some(e) = entry {
                    tracing::debug!("thumbnail {} in {} ms", e.name, r.ms);
                    e.thumb = Some(thumb);
                    e.meta = Some(*meta);
                    e.state = ThumbState::Ready;
                    e.texture = None;
                }
            }
            Err(msg) => {
                if let Some(e) = entry {
                    tracing::warn!("thumbnail {}: {msg}", e.name);
                    e.state = ThumbState::Failed;
                }
            }
        }
    }

    /// What the listing remembers of a `.mimizan.dng`, when this folder
    /// lists it.
    pub fn dng_source(&self, path: &Path) -> Option<&crate::bayer::DngSource> {
        self.sources.get(path)
    }

    /// `asked` left the strip and `opened` is the file in its place. An
    /// X-Trans RAF is dropped. A Bayer RAW stays aside, with its DNG, so
    /// the next flip of the checkbox needs no disk. A file put back takes
    /// a new asking.
    pub fn show_instead(&mut self, asked: &Path, opened: &Path, sort: Sort) {
        let Some(index) = self.index_of(asked) else {
            return;
        };
        if crate::bayer::is_ours(opened) {
            let raw = self.entries.remove(index);
            self.gives_way_on.insert(raw.path.clone());
            self.aside.push(raw);
            self.gives_way_off.insert(opened.to_path_buf());
            self.sources.insert(opened.to_path_buf(), crate::bayer::DngSource::One(asked.to_path_buf()));
            if self.index_of(opened).is_none() {
                if let Some(i) = self.aside.iter().position(|e| e.path == opened) {
                    let entry = self.aside.remove(i);
                    self.entries.push(entry);
                } else {
                    let ask = self.next_ask();
                    if let Some(entry) = Entry::pending(opened, ask) {
                        self.entries.push(entry);
                    }
                }
                self.resort(sort);
            }
            return;
        }
        self.entries.retain(|e| e.path != asked);
        if self.index_of(opened).is_none() {
            let ask = self.next_ask();
            if let Some(entry) = Entry::pending(opened, ask) {
                self.entries.push(entry);
                self.resort(sort);
            }
        }
    }
}

/// One folder read by [`list`].
pub struct Listed {
    pub generation: u64,
    /// The path that was asked for, before it was made readable.
    pub asked: PathBuf,
    /// The path that was read.
    pub path: PathBuf,
    pub result: std::io::Result<Folder>,
}

/// The folder a listing sends: the scan, which files give way, and the
/// checkbox's choice. `current` and `bayer` are the questions that read
/// a file.
fn listed_folder(
    path: &Path,
    sort: Sort,
    generation: u64,
    bayer_ls: bool,
    current: impl Fn(&Path) -> bool,
    bayer: impl Fn(&Path) -> bool,
) -> std::io::Result<Folder> {
    let mut folder = Folder::scan(path, sort, generation)?;
    folder.pair(current, bayer);
    folder.retile(bayer_ls, sort);
    Ok(folder)
}

/// Read `path` on a thread named `mimizan-folder` and send the folder.
/// The thread makes the path readable, lists it, and pairs a RAW with
/// the DNG beside it. Dropping the receiver ends the thread quietly: the
/// send finds nobody waiting and the thread returns.
pub fn list(
    generation: u64,
    path: PathBuf,
    sort: Sort,
    bayer_ls: bool,
    ctx: egui::Context,
) -> Receiver<Listed> {
    let (tx, rx) = channel();
    thread::Builder::new()
        .name("mimizan-folder".into())
        .spawn(move || {
            let asked = path.clone();
            let path = readable_path(&path);
            let result = listed_folder(
                &path,
                sort,
                generation,
                bayer_ls,
                crate::bayer::dng_is_current,
                crate::bayer::is_bayer,
            );
            let _ = tx.send(Listed { generation, asked, path, result });
            ctx.request_repaint();
        })
        .ok();
    rx
}

/// A result belongs to the folder the window asked for last. An older
/// generation, or a path the window is no longer asking for, is a folder
/// it has already left.
pub(crate) fn listing_is_current(listed: u64, now: u64, listed_path: &Path, asked: Option<&Path>) -> bool {
    listed == now && asked.is_some_and(|p| p == listed_path)
}

/// The folder the settings file names. The one on screen, or the one the
/// file already named while a listing is still being read. A path that was
/// asked for and has not been read is not stored.
pub(crate) fn folder_to_store(open: Option<&Path>, stored: Option<&Path>) -> Option<PathBuf> {
    open.map(Path::to_path_buf).or_else(|| stored.map(Path::to_path_buf))
}

/// The file's folder is the one on screen, or the one the strip is reading.
/// A parent spelled the way `canonicalize` spells it and the path that was
/// asked for are the same folder either way.
pub(crate) fn folder_is_asked(open: Option<&Path>, asked: Option<&Path>, parent: &Path) -> bool {
    open.is_some_and(|p| p == parent) || asked.is_some_and(|p| p == parent)
}

/// What a failed listing leaves. `shown` is the folder that stays on
/// screen. `asked` is empty: the bar is no longer waiting. `mark` is
/// whether the settings are written for this failure.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct FailedRead {
    pub shown: Option<PathBuf>,
    pub asked: Option<PathBuf>,
    pub mark: bool,
}

/// A folder that could not be read leaves the one on screen. The path
/// that was asked for is no longer asked, so the bar returns to that
/// folder. The settings are not marked: the unread folder is not stored,
/// and a stored folder is not cleared.
pub(crate) fn failed_read(shown: Option<&Path>, _asked: Option<&Path>) -> FailedRead {
    FailedRead { shown: shown.map(Path::to_path_buf), asked: None, mark: false }
}

/// Absolute, and without the Windows `\\?\` prefix `canonicalize` adds.
pub(crate) fn readable_path(path: &Path) -> PathBuf {
    let absolute = std::fs::canonicalize(path)
        .or_else(|_| std::path::absolute(path))
        .unwrap_or_else(|_| path.to_path_buf());
    without_verbatim_prefix(&absolute)
}

/// `\\?\C:\Users` is `C:\Users`. `\\?\UNC\server\share` is `\\server\share`.
fn without_verbatim_prefix(path: &Path) -> PathBuf {
    let text = path.as_os_str().to_string_lossy();
    if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
        PathBuf::from(format!(r"\\{rest}"))
    } else if let Some(rest) = text.strip_prefix(r"\\?\") {
        PathBuf::from(rest.to_string())
    } else {
        path.to_path_buf()
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

/// `$XDG_CACHE_HOME/mimizan/thumbs`, `%LOCALAPPDATA%/mimizan/thumbs`, or
/// `~/.cache/mimizan/thumbs`.
fn cache_dir() -> Option<PathBuf> {
    Some(mimizan_core::home::cache_dir()?.join("thumbs"))
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
    // A DNG preview was cached as the sensor stored it, on its side when the picture was taken upright.
    feed(b"upright");
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
    /// [`Entry::ask`] of the tile when it asked.
    pub ask: u32,
    /// The Negative window's least-squares demosaic. A RAW whose DNG is
    /// written is then the DNG, and with it off that DNG is not a tile.
    pub bayer_ls: bool,
}

/// What the thumbnail thread has made in the folder scan it is on. The
/// strip sends its whole list again whenever the list changes, and a list
/// may still name a file whose result is on its way back: that one is not
/// made twice. A tile the strip has let go, and a file put back into the
/// strip, ask under a new number and are made again; without it the tile
/// stayed "…" until the folder was read anew.
#[derive(Default)]
struct Made(HashSet<(u64, PathBuf, u32)>);

impl Made {
    /// A list of folder scan `generation` arrived: what was made for any
    /// other scan is forgotten.
    fn scan(&mut self, generation: u64) {
        self.0.retain(|(g, _, _)| *g == generation);
    }

    /// `true` the first time `job` is seen: it is to be made.
    fn first(&mut self, job: &ThumbJob) -> bool {
        self.0.insert((job.generation, job.path.clone(), job.ask))
    }
}

/// A thumbnail the strip can show, or the news that this RAF is the DNG
/// beside it.
pub enum ThumbReady {
    Picture(Thumb, Box<Meta>),
    /// Drop this RAF. The DNG beside it is the file.
    Covered,
}

/// One finished thumbnail (or the reason it failed).
pub struct ThumbResult {
    pub generation: u64,
    pub path: PathBuf,
    pub result: Result<ThumbReady, String>,
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
            let mut made = Made::default();
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
                        made.scan(first.generation);
                    }
                    queue = q;
                    queue.reverse(); // pop() takes the front
                }
                let Some(job) = queue.pop() else { continue };
                if !made.first(&job) {
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

/// Cache hit, or decode and bin the file and cache the result. An
/// X-Trans RAF is the camera's JPEG until its DNG is written, and then
/// it is not a tile of its own.
fn make_thumb(job: &ThumbJob, cache: Option<&Path>, cameras_dir: &Path) -> Result<ThumbReady, String> {
    // Ready is the program's own DNG: the RAF is not a tile. Anything
    // else of that name leaves the RAF, with the camera's JPEG, as while
    // the DNG is still unwritten. The foreign DNG is its own file.
    let xtrans_head = match crate::fuji::place(&job.path) {
        crate::fuji::Place::Ready(_) => return Ok(ThumbReady::Covered),
        crate::fuji::Place::Needs(head) => Some(head),
        crate::fuji::Place::Taken(_) => crate::fuji::head(&job.path),
        crate::fuji::Place::NotXtrans => None,
    };
    if let Some(head) = xtrans_head {
        let key = cache_key(&job.path, job.size, job.modified);
        if let Some(hit) = cache.and_then(|d| cache_load(d, &key)) {
            return Ok(ThumbReady::Picture(hit.0, Box::new(hit.1)));
        }
        let thumb = crate::fuji::camera_jpeg(&job.path, THUMB_LONG_EDGE)?;
        let meta = Meta {
            make: head.make,
            model: head.model,
            exposure: head.exposure,
            width: head.width,
            height: head.height,
            xtrans: true,
        };
        if let Some(d) = cache {
            cache_store(d, &key, &thumb, &meta);
        }
        return Ok(ThumbReady::Picture(thumb, Box::new(meta)));
    }
    // The least-squares path: the RAW gives way to its DNG while the
    // checkbox is on, and that DNG is not a tile while it is off. Two
    // RAWs of one stem both keep their tiles: the DNG would be one file.
    if job.bayer_ls {
        // The folder is read for another file of the stem only when the
        // DNG is there.
        if !crate::bayer::is_ours(&job.path)
            && crate::bayer::dng_is_current(&crate::bayer::dng_of(&job.path))
            && crate::bayer::other_raw(&job.path).is_none()
        {
            return Ok(ThumbReady::Covered);
        }
    } else if crate::bayer::is_ours(&job.path) {
        return Ok(ThumbReady::Covered);
    }
    let key = cache_key(&job.path, job.size, job.modified);
    if let Some(hit) = cache.and_then(|d| cache_load(d, &key)) {
        return Ok(ThumbReady::Picture(hit.0, Box::new(hit.1)));
    }
    // The three full-size channels are what the darkroom opens. The strip
    // shows the preview the DNG was written with, which is the camera's
    // JPEG. Decoding the linear picture here left the tile on "…" for as
    // long as that decode took, and a failure left it blank.
    if crate::bayer::is_ours(&job.path) {
        if let Some((thumb, meta)) = dng_preview(&job.path) {
            if let Some(d) = cache {
                cache_store(d, &key, &thumb, &meta);
            }
            return Ok(ThumbReady::Picture(thumb, Box::new(meta)));
        }
    }
    // A header can state a frame whose 16-bit samples alone are more than
    // half the free memory. Reading those samples is the allocation the
    // open path refuses, and the tile is the failure mark.
    if let Ok((w, h)) = mimizan_core::decode::stated_size(&job.path) {
        let samples = (w as u64).saturating_mul(h as u64).saturating_mul(2);
        let free = crate::worker::available_memory();
        if free > 0 && samples > free / 2 {
            let name = job
                .path
                .file_name()
                .map(|n| n.to_string_lossy())
                .unwrap_or(std::borrow::Cow::Borrowed("the file"));
            return Err(format!(
                "{name} states {w}x{h}, and its 16-bit samples alone are more than half the free memory"
            ));
        }
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
        xtrans: false,
    };
    if let Some(d) = cache {
        cache_store(d, &key, &thumb, &meta);
    }
    Ok(ThumbReady::Picture(thumb, Box::new(meta)))
}

/// The preview stored in a Bayer least-squares DNG, shrunk for the strip,
/// and the size of the linear picture behind it. `None` when the file has
/// neither a preview nor a thumbnail: the caller then bins the channels.
fn dng_preview(path: &Path) -> Option<(Thumb, Meta)> {
    use rawler::decoders::{RawDecodeParams, WellKnownIFD};
    use rawler::tags::TiffCommonTag;
    let source = mimizan_core::decode::raw_source(path).ok()?;
    let decoder = rawler::get_decoder(&source).ok()?;
    let params = RawDecodeParams::default();
    let image = decoder
        .preview_image(&source, &params)
        .ok()
        .flatten()
        .or_else(|| decoder.thumbnail_image(&source, &params).ok().flatten())?;
    let md = decoder.raw_metadata(&source, &params).ok();
    // The same orientation the darkroom settles for this file. The preview
    // is stored as the sensor held it; the tile is the picture upright.
    let own = decoder
        .ifd(WellKnownIFD::Root)
        .ok()
        .flatten()
        .and_then(|ifd| {
            ifd.get_entry(TiffCommonTag::Orientation)
                .map(|entry| rawler::Orientation::from_u16(entry.force_usize(0) as u16))
        })
        .unwrap_or(rawler::Orientation::Unknown);
    let exif = md.as_ref().and_then(|m| m.exif.orientation);
    let image = turn_preview(image, mimizan_core::decode::settled_orientation(own, exif));
    let thumb = thumb_from_image(image, THUMB_LONG_EDGE);
    let (width, height) = decoder
        .ifd(WellKnownIFD::Raw)
        .ok()
        .flatten()
        .and_then(|ifd| {
            let w = ifd.get_entry(TiffCommonTag::ImageWidth)?.force_usize(0);
            let h = ifd.get_entry(TiffCommonTag::ImageLength)?.force_usize(0);
            (w > 0 && h > 0).then_some((w, h))
        })
        .unwrap_or((thumb.width, thumb.height));
    Some((
        thumb,
        Meta {
            make: md.as_ref().map(|m| m.make.clone()).unwrap_or_default(),
            model: md.as_ref().map(|m| m.model.clone()).unwrap_or_default(),
            exposure: md.as_ref().map(mimizan_core::decode::exposure_from).unwrap_or_default(),
            width,
            height,
            xtrans: false,
        },
    ))
}

/// `picture` turned upright by a TIFF orientation (1..=8). An unknown
/// value leaves the stored pixels, which is how an untagged preview is shown.
fn turn_preview(mut picture: image::DynamicImage, orientation: u16) -> image::DynamicImage {
    let Some(turn) = u8::try_from(orientation).ok().and_then(image::metadata::Orientation::from_exif) else {
        return picture;
    };
    picture.apply_orientation(turn);
    picture
}

/// `picture` fitted so its long side is at most `long_edge`, as grey and
/// as colour. The grey is the same weights the camera JPEG uses.
fn thumb_from_image(mut picture: image::DynamicImage, long_edge: usize) -> Thumb {
    let (w, h) = (picture.width() as usize, picture.height() as usize);
    let long = w.max(h).max(1);
    let edge = long_edge.max(1);
    if long > edge {
        let tw = (w * edge / long).max(1) as u32;
        let th = (h * edge / long).max(1) as u32;
        picture = picture.resize(tw, th, image::imageops::FilterType::Triangle);
    }
    let rgb_img = picture.into_rgb8();
    let (width, height) = (rgb_img.width() as usize, rgb_img.height() as usize);
    let rgb = rgb_img.into_raw();
    let mut gray = vec![0u8; width * height];
    for (g, px) in gray.iter_mut().zip(rgb.as_chunks::<3>().0) {
        *g =
            (0.2126 * f32::from(px[0]) + 0.7152 * f32::from(px[1]) + 0.0722 * f32::from(px[2])).round() as u8;
    }
    Thumb { width, height, gray, rgb }
}

// ---------------------------------------------------------------------------
// Settings that survive a restart

/// The ruler of the Print window: what its lengths are shown and typed
/// in. The recipe keeps inches and millimetres whatever the ruler says.
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Ruler {
    /// Centimetres for the picture, millimetres for the rings.
    #[default]
    Metric,
    /// Inches for both.
    Imperial,
    /// Pixels at the print's dpi for both.
    Pixels,
}

impl Ruler {
    pub const ALL: [Ruler; 3] = [Ruler::Metric, Ruler::Imperial, Ruler::Pixels];

    /// The label on the switch.
    pub fn name(self) -> &'static str {
        match self {
            Ruler::Metric => "cm",
            Ruler::Imperial => "in",
            Ruler::Pixels => "px",
        }
    }
}

/// The file the Export button writes. A setting of the program, not of
/// the recipe: the recipe is the development, the format is the export's.
/// The sheet is the same either way; only the container differs. The
/// TIFF's depth and a grey sheet as RGB are two more settings beside it
/// (`Persisted::tiff_eight_bit`, `Persisted::grey_as_rgb`).
#[derive(Serialize, Deserialize, Default, Clone, Copy, Debug, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum ExportFormat {
    /// TIFF of 16 or 8 bits, the recipe in the description, the ICC
    /// profile embedded.
    #[default]
    Tiff,
    /// 8-bit JPEG at quality 100, the same ICC profile embedded, no
    /// chroma subsampling.
    Jpeg,
}

impl ExportFormat {
    /// The extension a name without one is given.
    pub fn extension(self) -> &'static str {
        match self {
            ExportFormat::Tiff => "tif",
            ExportFormat::Jpeg => "jpg",
        }
    }
}

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
    /// Bayer files open through the least-squares demosaic. `None` means
    /// the measured separation.
    pub bayer_ls: Option<bool>,
    /// The plate's taking filters and primaries last used.
    pub plate_filters: Option<PlateFilters>,
    pub plate_space: Option<PlateSpace>,
    /// The Print window's ruler. `None` means metric.
    pub ruler: Option<Ruler>,
    /// The file the Export button writes. `None` means TIFF.
    pub format: Option<ExportFormat>,
    /// The exported TIFF has 8 bits a sample, not 16. `None` means 16.
    pub tiff_eight_bit: Option<bool>,
    /// A grey print is written as RGB with three equal channels, in sRGB.
    /// `None` means one grey channel.
    pub grey_as_rgb: Option<bool>,
    /// The sheet's two sides are typed apart instead of one following the
    /// picture's proportion. `None` means the sheet follows the picture.
    pub sheet_free: Option<bool>,
    /// The dodging brush: the hand and the card as last set. `None` on a
    /// fresh install means the defaults, and that the brush is still to
    /// be set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub brush: Option<crate::brush::Brush>,
    /// Security-scoped bookmark for the folder opened on macOS. Absent on
    /// Linux and Windows, and on a Mac build that is not sandboxed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder_bookmark: Option<Vec<u8>>,
}

/// `$XDG_CONFIG_HOME/mimizan/gui.json`, `%APPDATA%/mimizan/gui.json`, or
/// `~/.config/mimizan/gui.json`.
fn config_path() -> Option<PathBuf> {
    Some(mimizan_core::home::settings_dir()?.join("gui.json"))
}

/// What [`Persisted::load`] read from `gui.json`.
pub struct ReadSettings {
    pub settings: Persisted,
    /// The file that holds settings this run must not write over. Bytes
    /// that were not a JSON object, or a file that is there and could not
    /// be read. Absent when `gui.json` was missing, or it was a JSON object.
    pub unread: Option<PathBuf>,
}

/// Where a settings file that is not a JSON object was left.
enum UnreadKept {
    /// The bytes were renamed to `gui.json.unread`.
    Moved(PathBuf),
    /// Both files stay. `gui.json.unread` was already a file, or the
    /// rename did not succeed, so the bytes are still in `gui.json`.
    Left(PathBuf),
}

impl UnreadKept {
    fn path(self) -> PathBuf {
        match self {
            UnreadKept::Moved(path) | UnreadKept::Left(path) => path,
        }
    }
}

/// `true` when `bytes` are one JSON object. An array, a bare value, and
/// bytes that are not JSON are not.
fn is_json_object(bytes: &[u8]) -> bool {
    matches!(serde_json::from_slice::<serde_json::Value>(bytes), Ok(serde_json::Value::Object(_)))
}

/// Move `path` to the same name with `.unread` when that name is free.
/// When `gui.json.unread` is already a file, leave both: the bytes
/// already kept there are not replaced, and neither is the file that
/// could not be read.
fn keep_unread(path: &Path) -> UnreadKept {
    let mut name = path.as_os_str().to_owned();
    name.push(".unread");
    let aside = PathBuf::from(name);
    if aside.is_file() {
        return UnreadKept::Left(path.to_path_buf());
    }
    match std::fs::rename(path, &aside) {
        Ok(()) => UnreadKept::Moved(aside),
        Err(_) => UnreadKept::Left(path.to_path_buf()),
    }
}

/// The settings in `path`. A missing file is the defaults. A JSON object
/// is read, and a value this version cannot read is left at its default.
/// Bytes that are not a JSON object are the defaults for this run, and
/// the file is kept: moved aside when `gui.json.unread` is free, left in
/// place when it is not. A file that is there and cannot be read is left
/// where it is, and this run does not write over it.
fn read_settings(path: &Path) -> ReadSettings {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            return ReadSettings { settings: Persisted::default(), unread: None };
        }
        Err(_) => {
            return ReadSettings { settings: Persisted::default(), unread: Some(path.to_path_buf()) };
        }
    };
    if is_json_object(&bytes) {
        return ReadSettings { settings: Persisted::from_json(&bytes), unread: None };
    }
    ReadSettings { settings: Persisted::default(), unread: Some(keep_unread(path).path()) }
}

impl Persisted {
    /// The saved settings. A missing file is the defaults, and the window
    /// may write. A JSON object is read as [`Self::from_json`] reads it.
    /// Bytes that are not a JSON object are the defaults for this run.
    /// They are moved to `gui.json.unread` when that name is free, and
    /// left in `gui.json` when it is not. A file that is there and cannot
    /// be read stays where it is. `unread` names the file this run does
    /// not write over.
    pub fn load() -> ReadSettings {
        match config_path() {
            Some(path) => read_settings(&path),
            None => ReadSettings { settings: Self::default(), unread: None },
        }
    }

    /// The settings in `bytes`. A value this version cannot read (from a
    /// newer one, or a key edited by hand) is left at its default and the
    /// others are kept. Bytes that are not a JSON object are the defaults.
    fn from_json(bytes: &[u8]) -> Self {
        if let Ok(all) = serde_json::from_slice(bytes) {
            return all;
        }
        let Ok(serde_json::Value::Object(mut settings)) = serde_json::from_slice(bytes) else {
            return Self::default();
        };
        settings.retain(|name, value| {
            let alone = serde_json::Map::from_iter([(name.clone(), value.clone())]);
            serde_json::from_value::<Self>(serde_json::Value::Object(alone)).is_ok()
        });
        serde_json::from_value(serde_json::Value::Object(settings)).unwrap_or_default()
    }

    /// Write the settings; a failure is silent (read-only home, no config dir).
    pub fn save(&self) {
        let Some(p) = config_path() else { return };
        if let Some(dir) = p.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(s) = serde_json::to_vec_pretty(self) {
            let _ = mimizan_core::outfile::write_bytes(&p, &s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn named(name: &str) -> Entry {
        Entry {
            path: PathBuf::from("/photos").join(name),
            name: name.to_owned(),
            size: 1,
            modified: SystemTime::UNIX_EPOCH,
            thumb: None,
            meta: None,
            state: ThumbState::Pending,
            texture: None,
            ask: 0,
        }
    }

    #[test]
    fn an_xtrans_raf_gives_way_to_the_dng_beside_it() {
        let mut folder = Folder {
            path: PathBuf::from("/photos"),
            entries: vec![named("DSCF1110.RAF"), named("DSCF1110.dng"), named("DSCF1111.RAF")],
            aside: Vec::new(),
            generation: 1,
            asks: 0,
            gives_way_on: HashSet::new(),
            gives_way_off: HashSet::new(),
            sources: HashMap::new(),
            subdirs: Vec::new(),
        };
        let raf = PathBuf::from("/photos/DSCF1110.RAF");
        let dng = PathBuf::from("/photos/DSCF1110.dng");
        folder.show_instead(&raf, &dng, Sort::Name);
        let names: Vec<&str> = folder.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["DSCF1110.dng", "DSCF1111.RAF"]);
        // A file the strip does not list is left alone.
        folder.show_instead(Path::new("/photos/other.RAF"), Path::new("/photos/other.dng"), Sort::Name);
        assert_eq!(folder.entries.len(), 2);
    }

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

    /// What a listing keeps: a RAW, a link to one, a folder and a link to
    /// a folder. A JPEG, a hidden `._` file and a hidden folder are left out.
    #[test]
    fn a_listing_keeps_raws_folders_and_what_a_link_points_to() {
        let dir = std::env::temp_dir().join(format!("mimizan-scan-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let raw = dir.join("photo.NEF");
        std::fs::write(&raw, b"raw-bytes").unwrap();
        std::fs::write(dir.join("note.jpg"), b"jpeg").unwrap();
        std::fs::write(dir.join("._X.NEF"), b"apple").unwrap();
        std::fs::create_dir(dir.join("album")).unwrap();
        std::fs::create_dir(dir.join(".hidden")).unwrap();
        #[cfg(unix)]
        {
            std::os::unix::fs::symlink("photo.NEF", dir.join("link.NEF")).unwrap();
            std::os::unix::fs::symlink("album", dir.join("linked")).unwrap();
        }
        let folder = Folder::scan(&dir, Sort::Name, 1).unwrap();
        let names: Vec<&str> = folder.entries.iter().map(|e| e.name.as_str()).collect();
        let dirs: Vec<&str> = folder.subdirs.iter().map(|d| d.name.as_str()).collect();
        #[cfg(unix)]
        {
            assert_eq!(names, ["link.NEF", "photo.NEF"]);
            assert_eq!(dirs, ["album", "linked"]);
            let link = folder.entries.iter().find(|e| e.name == "link.NEF").unwrap();
            assert_eq!(link.size, b"raw-bytes".len() as u64);
        }
        #[cfg(not(unix))]
        {
            assert_eq!(names, ["photo.NEF"]);
            assert_eq!(dirs, ["album"]);
        }
        let photo = folder.entries.iter().find(|e| e.name == "photo.NEF").unwrap();
        assert_eq!(photo.size, b"raw-bytes".len() as u64);
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn marked() -> mimizan_core::thumb::Thumb {
        mimizan_core::thumb::Thumb { width: 1, height: 1, gray: vec![7], rgb: vec![1, 2, 3] }
    }

    /// Turning the checkbox and turning it back keeps each file's picture
    /// and the name order.
    #[test]
    fn retile_keeps_the_pictures_and_the_order() {
        let dir = std::env::temp_dir().join(format!("mimizan-retile-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let raw = dir.join("a.NEF");
        let dng = dir.join("a.mimizan.dng");
        std::fs::write(&raw, b"r").unwrap();
        std::fs::write(&dng, b"d").unwrap();
        std::fs::write(dir.join("b.NEF"), b"b").unwrap();
        let mut folder = Folder::scan(&dir, Sort::Name, 1).unwrap();
        for e in &mut folder.entries {
            e.thumb = Some(marked());
        }
        folder.pair(|p| p == dng, |p| p == raw);
        folder.retile(true, Sort::Name);
        let names: Vec<&str> = folder.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a.mimizan.dng", "b.NEF"]);
        assert_eq!(folder.aside.len(), 1);
        assert_eq!(folder.aside[0].thumb.as_ref().map(|t| t.gray.as_slice()), Some(&[7][..]));
        folder.retile(false, Sort::Name);
        let names: Vec<&str> = folder.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(names, ["a.NEF", "b.NEF"]);
        assert!(folder.entries.iter().all(|e| e.thumb.as_ref().is_some_and(|t| t.gray == [7])));
        assert_eq!(folder.aside[0].name, "a.mimizan.dng");
        assert_eq!(folder.aside[0].thumb.as_ref().map(|t| t.gray.as_slice()), Some(&[7][..]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A picture that arrives after the checkbox moved its file aside is
    /// kept with the file, and is there when the file comes back.
    #[test]
    fn a_thumbnail_reaches_a_file_the_checkbox_moved_aside() {
        let dir = std::env::temp_dir().join(format!("mimizan-aside-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let raw = dir.join("a.NEF");
        let dng = dir.join("a.mimizan.dng");
        std::fs::write(&raw, b"r").unwrap();
        std::fs::write(&dng, b"d").unwrap();
        let mut folder = listed_folder(&dir, Sort::Name, 3, false, |p| p == dng, |p| p == raw).unwrap();
        assert_eq!(folder.entries[0].path, raw);
        // The RAW's picture is asked for, then the checkbox goes on.
        folder.retile(true, Sort::Name);
        assert_eq!(folder.aside[0].path, raw);
        let picture = |generation, path: &Path| ThumbResult {
            generation,
            path: path.to_path_buf(),
            result: Ok(ThumbReady::Picture(
                marked(),
                Box::new(Meta {
                    make: String::new(),
                    model: String::new(),
                    width: 1,
                    height: 1,
                    exposure: Exposure::default(),
                    xtrans: false,
                }),
            )),
            ms: 0,
        };
        folder.take_thumb(picture(2, &raw));
        assert!(folder.aside[0].thumb.is_none(), "an older scan's picture is not this file's");
        folder.take_thumb(picture(3, &raw));
        assert_eq!(folder.aside[0].state, ThumbState::Ready);
        folder.retile(false, Sort::Name);
        let back = folder.entries.iter().find(|e| e.path == raw).unwrap();
        assert_eq!(back.thumb.as_ref().map(|t| t.gray.as_slice()), Some(&[7][..]));
        // A failure is kept too, so the tile says "?" and does not wait.
        folder.retile(true, Sort::Name);
        folder.take_thumb(ThumbResult { generation: 3, path: raw.clone(), result: Err("no".into()), ms: 0 });
        assert_eq!(folder.aside[0].state, ThumbState::Failed);
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A RAW replaced by the DNG just written comes back, picture and all,
    /// when the checkbox goes off. It is not read again.
    #[test]
    fn show_instead_then_retile_brings_the_raw_back() {
        let dir = std::env::temp_dir().join(format!("mimizan-instead-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let raw = dir.join("a.NEF");
        let dng = dir.join("a.mimizan.dng");
        std::fs::write(&raw, b"r").unwrap();
        std::fs::write(&dng, b"d").unwrap();
        let mut folder = Folder::scan(&dir, Sort::Name, 1).unwrap();
        // The DNG was written after the listing.
        folder.entries.retain(|e| e.path == raw);
        folder.entries[0].thumb = Some(marked());
        folder.show_instead(&raw, &dng, Sort::Name);
        assert!(folder.aside.iter().any(|e| e.path == raw && e.thumb.is_some()));
        folder.retile(false, Sort::Name);
        let back = folder.entries.iter().find(|e| e.path == raw).unwrap();
        assert_eq!(back.ask, 0);
        assert_eq!(back.thumb.as_ref().map(|t| t.gray.as_slice()), Some(&[7][..]));
        assert!(folder.aside.iter().any(|e| e.path == dng));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn raw_extensions_are_case_insensitive() {
        assert!(is_raw(Path::new("x/DSC.NEF")));
        assert!(is_raw(Path::new("x/a.dng")));
        assert!(!is_raw(Path::new("x/a.jpg")));
        assert!(!is_raw(Path::new("x/noext")));
    }

    /// A file the Finder wrote beside a RAW on a card (`._DSC_1.NEF`) has
    /// the RAW's extension and none of its pixels.
    #[test]
    fn a_hidden_file_is_no_raw() {
        assert!(!is_raw(Path::new("x/._DSC_1.NEF")));
        assert!(!is_raw(Path::new("x/.hidden.dng")));
        assert!(is_raw(Path::new(".hidden/DSC_1.NEF")));
    }

    /// One value this version cannot read (written by a newer one, or by
    /// hand) costs that setting, not the folder and the windows with it.
    #[test]
    fn one_unreadable_setting_leaves_the_others() {
        let json = br#"{
            "folder": "/photos/2026",
            "sort": "by-moon",
            "strip": false,
            "panels": [true, false, true],
            "strip_height": "tall",
            "brush": { "tool": "hand" },
            "a_setting_of_tomorrow": 3
        }"#;
        let p = Persisted::from_json(json);
        assert_eq!(p.folder, Some(PathBuf::from("/photos/2026")));
        assert_eq!(p.strip, Some(false));
        assert_eq!(p.panels, Some([true, false, true]));
        assert_eq!(p.sort, Sort::default());
        assert_eq!(p.strip_height, None);
        assert_eq!(p.brush, None);
        // What is whole reads as before, and what is no JSON is the defaults.
        let whole = Persisted {
            folder: Some("/a".into()),
            ruler: Some(Ruler::default()),
            tiff_eight_bit: Some(true),
            grey_as_rgb: Some(true),
            sheet_free: Some(true),
            ..Persisted::default()
        };
        assert_eq!(Persisted::from_json(&serde_json::to_vec(&whole).unwrap()), whole);
        // A file from before the print service's settings: the archive's
        // file and the sheet in the picture's proportion.
        let older = Persisted::from_json(br#"{"format": "jpeg", "ruler": "pixels"}"#);
        assert_eq!(older.format, Some(ExportFormat::Jpeg));
        assert_eq!((older.tiff_eight_bit, older.grey_as_rgb, older.sheet_free), (None, None, None));
        assert_eq!(Persisted::from_json(b"not json"), Persisted::default());
        assert_eq!(Persisted::from_json(b"[1, 2]"), Persisted::default());
    }

    /// `{` is not an object, so it moves to `gui.json.unread` when that
    /// name is free and stays in `gui.json` when the name is taken. An
    /// object is read and left where it is, including one key this
    /// version does not know. A missing file is the defaults, and nothing
    /// is written beside it.
    #[test]
    fn settings_that_are_not_json_are_kept_and_an_object_is_read() {
        let dir = std::env::temp_dir().join(format!("mimizan-settings-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let path = dir.join("gui.json");
        let aside = dir.join("gui.json.unread");

        let missing = read_settings(&dir.join("absent.json"));
        assert_eq!(missing.settings, Persisted::default());
        assert!(missing.unread.is_none());
        assert!(!aside.exists());

        std::fs::write(&path, b"{").unwrap();
        let moved = read_settings(&path);
        assert_eq!(moved.settings, Persisted::default());
        assert_eq!(moved.unread.as_deref(), Some(aside.as_path()));
        assert!(!path.exists());
        assert_eq!(std::fs::read(&aside).unwrap(), b"{");

        std::fs::write(&path, b"{").unwrap();
        let left = read_settings(&path);
        assert_eq!(left.settings, Persisted::default());
        assert_eq!(left.unread.as_deref(), Some(path.as_path()));
        assert_eq!(std::fs::read(&path).unwrap(), b"{");
        assert_eq!(std::fs::read(&aside).unwrap(), b"{");

        let object = br#"{"folder": "/photos", "strip": false, "a_setting_of_tomorrow": 1}"#;
        std::fs::write(&path, object).unwrap();
        let read = read_settings(&path);
        assert_eq!(read.settings.folder, Some(PathBuf::from("/photos")));
        assert_eq!(read.settings.strip, Some(false));
        assert!(read.unread.is_none());
        assert_eq!(std::fs::read(&path).unwrap(), object);
        assert_eq!(std::fs::read(&aside).unwrap(), b"{");

        // A path that is there and cannot be read stays, and this run
        // does not write over it. A directory is not a file of settings.
        let blocked = dir.join("blocked.json");
        std::fs::create_dir(&blocked).unwrap();
        let unread = read_settings(&blocked);
        assert_eq!(unread.settings, Persisted::default());
        assert_eq!(unread.unread.as_deref(), Some(blocked.as_path()));
        assert!(blocked.is_dir());

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Orientation 6 turns the stored preview a quarter clockwise: a 2×3
    /// picture becomes 3×2, and the top-left pixel is the top-right one.
    #[test]
    fn a_portrait_preview_stands_up() {
        let picture = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(2, 3, |x, y| {
            image::Rgb([if (x, y) == (0, 0) { 200 } else { 10 }, 0, 0])
        }));
        let turned = turn_preview(picture, 6);
        assert_eq!((turned.width(), turned.height()), (3, 2));
        assert_eq!(turned.to_rgb8().get_pixel(2, 0).0, [200, 0, 0]);
        assert_eq!(turned.to_rgb8().get_pixel(0, 0).0, [10, 0, 0]);
        let flat =
            image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(2, 3, |_, _| image::Rgb([1, 2, 3])));
        let stayed = turn_preview(flat, 1);
        assert_eq!((stayed.width(), stayed.height()), (2, 3));
    }

    #[test]
    fn a_preview_is_shrunk_to_the_strip_and_keeps_its_grey() {
        let picture = image::DynamicImage::ImageRgb8(image::RgbImage::from_fn(8, 4, |x, _| {
            image::Rgb([if x < 4 { 220 } else { 10 }, 20, 30])
        }));
        let thumb = thumb_from_image(picture, 4);
        assert_eq!(thumb.width.max(thumb.height), 4);
        assert_eq!(thumb.gray.len(), thumb.width * thumb.height);
        assert_eq!(thumb.rgb.len(), 3 * thumb.gray.len());
        assert!(thumb.rgb[0] > thumb.rgb[thumb.rgb.len() - 3]);
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

    /// A list that names a file again is not a second job; a tile the
    /// strip let go is one, and so is the same file in a new scan.
    #[test]
    fn a_thumbnail_is_made_once_and_again_when_it_was_let_go() {
        let job = |generation, name: &str, ask| ThumbJob {
            generation,
            path: PathBuf::from(name),
            size: 1,
            modified: SystemTime::UNIX_EPOCH,
            ask,
            bayer_ls: false,
        };
        let mut made = Made::default();
        made.scan(1);
        assert!(made.first(&job(1, "a.nef", 0)));
        assert!(made.first(&job(1, "b.nef", 0)));
        // The next list still names both: neither is made twice.
        made.scan(1);
        assert!(!made.first(&job(1, "a.nef", 0)));
        assert!(!made.first(&job(1, "b.nef", 0)));
        // `a` scrolled far away, lost its pixels and came back.
        assert!(made.first(&job(1, "a.nef", 1)));
        assert!(!made.first(&job(1, "a.nef", 1)));
        assert!(!made.first(&job(1, "b.nef", 0)));
        // The folder read anew: every file is made for the new scan.
        made.scan(2);
        assert!(made.first(&job(2, "a.nef", 0)));
        assert!(made.first(&job(2, "b.nef", 0)));
    }

    /// A file pictured, taken out of the strip and put back in the same
    /// scan asks again: the first picture is not the one to show.
    #[test]
    fn a_file_put_back_asks_under_a_key_the_worker_has_not_seen() {
        let dir = std::env::temp_dir().join(format!("mimizan-ask-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let raw = dir.join("a.NEF");
        let dng = dir.join("a.dng");
        std::fs::write(&raw, b"raw").unwrap();
        std::fs::write(&dng, b"dng").unwrap();
        let mut folder = Folder::scan(&dir, Sort::Name, 1).unwrap();
        let generation = folder.generation;
        let mut made = Made::default();
        made.scan(generation);
        let job = |e: &Entry| ThumbJob {
            generation,
            path: e.path.clone(),
            size: e.size,
            modified: e.modified,
            ask: e.ask,
            bayer_ls: false,
        };
        let pictured = folder.entries.iter().find(|e| e.path == raw).unwrap();
        assert!(made.first(&job(pictured)));
        // The DNG takes the tile, then the file comes back.
        folder.show_instead(&raw, &dng, Sort::Name);
        folder.show_instead(&dng, &raw, Sort::Name);
        let back = folder.entries.iter().find(|e| e.path == raw).unwrap();
        assert!(made.first(&job(back)), "a file put back asks again");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_verbatim_windows_path_is_a_drive_path() {
        assert_eq!(without_verbatim_prefix(Path::new(r"\\?\C:\Users")), PathBuf::from(r"C:\Users"));
        assert_eq!(without_verbatim_prefix(Path::new(r"\\?\UNC\nas\photos")), PathBuf::from(r"\\nas\photos"));
        assert_eq!(without_verbatim_prefix(Path::new("/home/dragan")), PathBuf::from("/home/dragan"));
    }

    /// The thread lists the folder it was asked for. An empty DNG is not
    /// one this build wrote, so the RAW stays when least squares is on.
    /// The DNG gives way when it is off, because its one RAW is listed.
    #[test]
    fn a_listing_thread_sends_the_folder_it_was_asked_for() {
        let dir = std::env::temp_dir().join(format!("mimizan-list-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("a.NEF"), b"raw").unwrap();
        std::fs::write(dir.join("a.mimizan.dng"), b"dng").unwrap();
        std::fs::write(dir.join("note.jpg"), b"jpeg").unwrap();
        for (on, shown) in [(false, vec!["a.NEF"]), (true, vec!["a.mimizan.dng", "a.NEF"])] {
            let rx = list(7, dir.clone(), Sort::Name, on, egui::Context::default());
            let listed = rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap();
            assert!(listing_is_current(listed.generation, 7, &listed.asked, Some(&dir)));
            assert_eq!(listed.asked, dir);
            let folder = listed.result.unwrap();
            let names: Vec<&str> = folder.entries.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(names, shown);
            let hidden: Vec<&str> = folder.aside.iter().map(|e| e.name.as_str()).collect();
            assert_eq!(hidden, if on { Vec::new() } else { vec!["a.mimizan.dng"] });
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The same listing hides the RAW when least squares is on and the
    /// DNG when it is off. The questions are answered here, not from the
    /// disk: the files are empty.
    #[test]
    fn a_listing_hides_the_file_the_checkbox_does_not_open() {
        let dir = std::env::temp_dir().join(format!("mimizan-listed-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let raw = dir.join("a.NEF");
        let dng = dir.join("a.mimizan.dng");
        std::fs::write(&raw, b"raw").unwrap();
        std::fs::write(&dng, b"dng").unwrap();
        let on = listed_folder(&dir, Sort::Name, 1, true, |p| p == dng, |p| p == raw).unwrap();
        assert_eq!(on.entries.len(), 1);
        assert_eq!(on.entries[0].path, dng);
        assert_eq!(on.aside.len(), 1);
        assert_eq!(on.aside[0].path, raw);
        let off = listed_folder(&dir, Sort::Name, 1, false, |p| p == dng, |p| p == raw).unwrap();
        assert_eq!(off.entries.len(), 1);
        assert_eq!(off.entries[0].path, raw);
        assert_eq!(off.aside[0].path, dng);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_older_listing_is_not_the_folder_now() {
        let photos = Path::new("/photos");
        let other = Path::new("/other");
        assert!(!listing_is_current(1, 2, photos, Some(photos)));
        assert!(listing_is_current(2, 2, photos, Some(photos)));
        assert!(!listing_is_current(2, 2, photos, Some(other)));
        assert!(!listing_is_current(2, 2, photos, None));
    }

    /// A quit stores the folder on screen, or the one the file already
    /// named. A path that was asked for and has not been read is not it.
    #[test]
    fn a_quit_stores_the_folder_on_screen_or_the_one_already_named() {
        let open = Path::new("/photos/canon");
        let stored = Path::new("/photos");
        assert_eq!(folder_to_store(Some(open), Some(stored)).as_deref(), Some(open));
        assert_eq!(folder_to_store(None, Some(stored)).as_deref(), Some(stored));
        assert_eq!(folder_to_store(None, None), None);
    }

    /// A screen on `/photos`, asked for `/photos/lost+found`, still shows
    /// `/photos` when that folder cannot be read. It is no longer asking,
    /// and the settings are not marked. A screen showing nothing stays
    /// empty and does not mark them either.
    #[test]
    fn a_folder_that_cannot_be_read_leaves_the_one_on_screen() {
        let photos = Path::new("/photos");
        let lost = Path::new("/photos/lost+found");
        let kept = failed_read(Some(photos), Some(lost));
        assert_eq!(kept.shown.as_deref(), Some(photos));
        assert_eq!(kept.asked, None);
        assert!(!kept.mark);
        let empty = failed_read(None, Some(lost));
        assert_eq!(empty.shown, None);
        assert_eq!(empty.asked, None);
        assert!(!empty.mark);
    }

    #[test]
    fn a_file_in_the_folder_being_read_does_not_ask_for_it_again() {
        let canon = Path::new("/photos/canon");
        let asked = Path::new("/photos");
        assert!(folder_is_asked(Some(canon), Some(asked), canon));
        assert!(folder_is_asked(None, Some(asked), asked));
        assert!(!folder_is_asked(None, Some(asked), canon));
        assert!(!folder_is_asked(None, None, asked));
    }
}
