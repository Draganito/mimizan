//! A Bayer RAW written out as a linear DNG.
//!
//! *least-squares demosaic* in the Negative window. The mosaic is separated
//! by the least squares the bench measured, and the picture is written
//! beside the RAW, named `<stem>.mimizan.dng` so the camera file is left
//! alone. The darkroom then opens that DNG and does not separate it again,
//! which is how a Fuji X-Trans file is opened. Off, the checkbox leaves the
//! file on the measured separation.

use mimizan_cli::bayer_ls;
use mimizan_lab_xtrans::convert::Developed;
use mimizan_lab_xtrans::dng;
use rawler::decoders::RawDecodeParams;
use rawler::RawImage;
use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// What a DNG written here ends its name with.
const MARK: &str = ".mimizan.dng";

/// What the DNG says it was written by. The test build names itself: the
/// version is the one of the release before it, and a DNG that release
/// wrote beside a RAW has to be written again to show the new separation.
const SOFTWARE: &str =
    concat!("Mimizan Lab ", env!("CARGO_PKG_VERSION"), " (Bayer least squares, test build bayer2)");

/// Where a Bayer file stands when the checkbox is on.
pub enum Place {
    /// Not a Bayer mosaic, or already the DNG this path writes.
    NotBayer,
    /// The DNG beside the RAW is the file to open.
    Ready(PathBuf),
    /// The RAW still has to be written out.
    Needs(Head),
    /// Another RAW in the folder has this stem. The one DNG would be
    /// both files, so this file stays on the measured separation.
    Shared(PathBuf),
}

/// What the head of a Bayer file says, and where its DNG will be.
pub struct Head {
    pub raw: PathBuf,
    pub dng: PathBuf,
    pub width: usize,
    pub height: usize,
}

/// A DNG this path wrote. The stem before `.mimizan.dng` is the RAW's.
pub fn is_ours(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.len() > MARK.len() && n.to_ascii_lowercase().ends_with(MARK))
}

/// The DNG of `raw`, beside it.
pub fn dng_of(raw: &Path) -> PathBuf {
    let stem = raw.file_stem().map(|s| s.to_string_lossy().into_owned()).unwrap_or_default();
    raw.with_file_name(format!("{stem}.mimizan.dng"))
}

/// A DNG that is there and holds something. An empty file is not one:
/// an empty one would otherwise block the RAW for good.
pub fn dng_is_ready(path: &Path) -> bool {
    fs::metadata(path).is_ok_and(|m| m.is_file() && m.len() > 0)
}

/// This build wrote `path`. An older DNG beside the RAW is written again:
/// the separation has changed, and opening the old file would show the
/// picture the previous version left there.
pub fn dng_is_current(path: &Path) -> bool {
    dng_is_ready(path) && crate::software::software_of(path).as_deref() == Some(SOFTWARE)
}

/// The RAW a DNG written here came from, when that one RAW is still
/// beside it. Two RAWs of the stem are not a choice: the caller is not
/// handed one of them. A path that is not such a DNG is returned as it stands.
pub fn raw_of(path: &Path) -> Option<PathBuf> {
    if !is_ours(path) {
        return Some(path.to_path_buf());
    }
    match raws_of(path).as_slice() {
        [one] => Some(one.clone()),
        _ => None,
    }
}

/// The RAWs a DNG written here could have come from: the files of its
/// stem in the folder, apart from the DNG itself.
pub(crate) fn raws_of(path: &Path) -> Vec<PathBuf> {
    let Some(stem) = stem_of_ours(path) else {
        return Vec::new();
    };
    sources_among(raws_named(path, &stem), is_bayer)
}

/// One file of the name is the RAW, whatever it is. Of several, only a
/// Bayer file can have been written out as this DNG: a linear DNG of the
/// same name, an export for instance, is not a second source.
fn sources_among(named: Vec<PathBuf>, bayer: impl Fn(&Path) -> bool) -> Vec<PathBuf> {
    if named.len() < 2 {
        named
    } else {
        named.into_iter().filter(|p| bayer(p)).collect()
    }
}

/// Another Bayer file in the folder with the same stem as `path`,
/// compared without regard to case: the two would be written out as one
/// DNG. A file that is not a Bayer mosaic never is, so it does not count,
/// and neither does the `.mimizan.dng` itself. `None` when there is no
/// such file, or the folder cannot be read.
pub fn other_raw(path: &Path) -> Option<PathBuf> {
    let stem = path.file_stem().and_then(|s| s.to_str())?;
    other_among(raws_named(path, stem), is_bayer)
}

fn other_among(named: Vec<PathBuf>, bayer: impl Fn(&Path) -> bool) -> Option<PathBuf> {
    named.into_iter().find(|p| bayer(p))
}

/// A Bayer mosaic this path writes out. The head is read, not the picture.
pub(crate) fn is_bayer(path: &Path) -> bool {
    head_of(path).is_some()
}

fn stem_of_ours(path: &Path) -> Option<String> {
    if !is_ours(path) {
        return None;
    }
    let name = path.file_name()?.to_str()?;
    Some(name[..name.len() - MARK.len()].to_string())
}

/// `other` is a different RAW of `stem`. The program's own DNG is not a
/// RAW of the stem it was written from.
fn is_raw_of_stem(stem: &str, other: &Path) -> bool {
    if is_ours(other) {
        return false;
    }
    other.file_stem().and_then(|s| s.to_str()).is_some_and(|s| s.eq_ignore_ascii_case(stem))
        && crate::browser::is_raw(other)
}

/// What a listed `.mimizan.dng` was written from, as far as the listing
/// can say. One RAW is the file the checkbox opens. Several are not a
/// choice. None means no RAW of the stem is listed.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum DngSource {
    One(PathBuf),
    None,
    Several,
}

/// Which files the checkbox hides, and the source of every listed
/// `.mimizan.dng`. `current` is this build's DNG; `bayer` is a Bayer
/// mosaic. `current` is asked only of a `.mimizan.dng` listed beside a
/// RAW of its stem, and `bayer` only where two or more files share a stem.
pub(crate) struct Pairs {
    /// RAWs hidden while least squares is on.
    pub gives_way_on: Vec<PathBuf>,
    /// `.mimizan.dng` files hidden while it is off.
    pub gives_way_off: Vec<PathBuf>,
    /// The source of each listed `.mimizan.dng`.
    pub sources: Vec<(PathBuf, DngSource)>,
}

/// The pairs in `paths`, decided from that list. `current` and `bayer`
/// are the two questions that still need the disk; everything else is
/// the names.
pub(crate) fn pairs(
    paths: &[PathBuf],
    current: impl Fn(&Path) -> bool,
    bayer: impl Fn(&Path) -> bool,
) -> Pairs {
    let mut by_stem: HashMap<String, Vec<&Path>> = HashMap::new();
    for path in paths {
        let Some(key) = stem_key(path) else { continue };
        by_stem.entry(key).or_default().push(path);
    }
    let mut gives_way_on = Vec::new();
    let mut gives_way_off = Vec::new();
    let mut sources = Vec::new();
    for (stem, group) in &by_stem {
        let ours: Vec<&Path> = group.iter().copied().filter(|p| is_ours(p)).collect();
        let raws: Vec<PathBuf> =
            group.iter().copied().filter(|p| is_raw_of_stem(stem, p)).map(Path::to_path_buf).collect();
        let found = sources_among(raws.clone(), &bayer);
        let source = match found.as_slice() {
            [one] => DngSource::One(one.clone()),
            [] => DngSource::None,
            _ => DngSource::Several,
        };
        // A DNG with no RAW beside it is not opened to see if it is current.
        let current_dngs: Vec<&Path> = if raws.is_empty() {
            Vec::new()
        } else {
            ours.iter().copied().filter(|dng| current(dng)).collect()
        };
        for dng in &ours {
            if matches!(source, DngSource::One(_)) {
                gives_way_off.push((*dng).to_path_buf());
            }
            sources.push(((*dng).to_path_buf(), source.clone()));
        }
        if current_dngs.is_empty() {
            continue;
        }
        // The same rule as the strip used to apply one file at a time.
        // Several Bayer files of the stem share one DNG, so none gives way.
        // One source gives way, and a file beside it does not. Where no
        // file is a Bayer mosaic, each RAW whose DNG is current gives way.
        match &source {
            DngSource::Several => {}
            DngSource::One(one) => {
                if current_dngs.iter().any(|dng| same_dng(one, dng)) {
                    gives_way_on.push(one.clone());
                }
            }
            DngSource::None => {
                for raw in &raws {
                    if current_dngs.iter().any(|dng| same_dng(raw, dng)) {
                        gives_way_on.push(raw.clone());
                    }
                }
            }
        }
    }
    gives_way_on.sort();
    gives_way_off.sort();
    sources.sort_by(|a, b| a.0.cmp(&b.0));
    Pairs { gives_way_on, gives_way_off, sources }
}

/// `dng` is the file `raw` would be written out as, apart from case.
fn same_dng(raw: &Path, dng: &Path) -> bool {
    let wanted = dng_of(raw);
    match (wanted.file_name(), dng.file_name()) {
        (Some(a), Some(b)) => a.eq_ignore_ascii_case(b),
        _ => false,
    }
}

/// The lower-cased stem files of one picture share. A `.mimizan.dng`
/// is named for the RAW it came from, not for the extra suffix.
fn stem_key(path: &Path) -> Option<String> {
    if let Some(stem) = stem_of_ours(path) {
        return Some(stem.to_ascii_lowercase());
    }
    path.file_stem().and_then(|s| s.to_str()).map(|s| s.to_ascii_lowercase())
}

/// `other` is another RAW of `path`'s stem, in a listing the caller
/// already holds.
pub(crate) fn listed_other(path: &Path, other: &Path) -> bool {
    path != other && path.file_stem().and_then(|s| s.to_str()).is_some_and(|stem| is_raw_of_stem(stem, other))
}

fn raws_named(path: &Path, stem: &str) -> Vec<PathBuf> {
    let Some(dir) = path.parent() else {
        return Vec::new();
    };
    let Ok(entries) = fs::read_dir(dir) else {
        return Vec::new();
    };
    entries.flatten().map(|entry| entry.path()).filter(|p| p != path && is_raw_of_stem(stem, p)).collect()
}

/// What the log says when least squares is not used because of `other`.
pub fn shared_note(other: &Path) -> String {
    let name = other
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| other.display().to_string());
    format!(
        "least squares is off for this file: `{name}` beside it has the same name, and the DNG would be one file for both"
    )
}

/// The head only. A file this path does not write out is [`Place::NotBayer`].
/// A DNG this build already wrote is that too: it opens as it stands. One
/// an older build wrote is written again from the RAW beside it. Two RAWs
/// of one stem are [`Place::Shared`]: the DNG would be one file for both.
pub fn place(path: &Path) -> Place {
    let raw = if is_ours(path) {
        if dng_is_current(path) {
            return Place::NotBayer;
        }
        match raw_of(path) {
            Some(raw) if raw.is_file() => raw,
            _ => return Place::NotBayer,
        }
    } else {
        path.to_path_buf()
    };
    let Some(head) = head_of(&raw) else {
        return Place::NotBayer;
    };
    if let Some(other) = other_raw(&raw) {
        return Place::Shared(other);
    }
    if dng_is_current(&head.dng) {
        return Place::Ready(head.dng);
    }
    Place::Needs(head)
}

/// Width and height of a Bayer file this path writes out.
fn head_of(raw: &Path) -> Option<Head> {
    let source = mimizan_core::decode::raw_source(raw).ok()?;
    let decoder = rawler::get_decoder(&source).ok()?;
    let img = decoder.raw_image(&source, &RawDecodeParams::default(), true).ok()?;
    bayer_ls::bayer_phase(&img)?;
    Some(Head { raw: raw.to_path_buf(), dng: dng_of(raw), width: img.width, height: img.height })
}

/// Write the DNG of `head`. The highlights stay in the file, as they do for
/// an X-Trans plate. `tell` hears each step, and the share of the separation.
/// An I/O error keeps its kind, so a folder that cannot be written is told
/// apart from a file the separation refused.
pub fn write(head: &Head, tell: &dyn Fn(String)) -> Result<String, std::io::Error> {
    if dng_is_current(&head.dng) {
        return Err(std::io::Error::other(format!("{} is there already", head.dng.display())));
    }
    let started = Instant::now();
    tell("reading the Bayer file".into());
    let source = mimizan_core::decode::raw_source(&head.raw)
        .map_err(|e| std::io::Error::other(format!("cannot be read: {e}")))?;
    let decoder = rawler::get_decoder(&source)
        .map_err(|e| std::io::Error::other(format!("not a raw file this program reads: {e}")))?;
    let img = decoder
        .raw_image(&source, &RawDecodeParams::default(), false)
        .map_err(|e| std::io::Error::other(format!("cannot be decoded: {e}")))?;
    let dev = develop(&img, tell).map_err(std::io::Error::other)?;
    let burnt = dev.burnt as f64 / (dev.frame.width * dev.frame.height) as f64;
    let (alpha, alpha_c) = (dev.prior.alpha, dev.prior.alpha_c);

    tell("writing the DNG".into());
    // Whole under its name or not there: a DNG cut off half way would be
    // taken for the finished one at the next open. Two windows each
    // finish their own part; the later rename replaces one finished DNG
    // with another of the same bytes.
    mimizan_core::outfile::write_whole(&head.dng, |out| {
        dng::write(out, &source, decoder.as_ref(), img, dev, true, SOFTWARE).map_err(std::io::Error::other)
    })?;
    Ok(format!(
        "DNG written in {:.0} s, prior f^{alpha:.2} and f^{alpha_c:.2}, {:.3}% burnt out and kept",
        started.elapsed().as_secs_f64(),
        100.0 * burnt
    ))
}

/// What the log says when the least-squares DNG was not written. A folder
/// that will not take the file is named as that, and any other failure
/// keeps its own text. A refusal for memory is not this: that stays an error.
pub fn unwritten_note(err: &std::io::Error) -> String {
    let reason = match err.kind() {
        std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem => {
            "this folder cannot be written to".to_owned()
        }
        _ => err.to_string(),
    };
    format!("the least-squares DNG was not written ({reason}); this file is on the measured separation")
}

/// The picture, in the camera's own channels. The frame's corner is a red
/// photosite, so the tile is the RGGB one the bench measured.
fn develop(img: &RawImage, tell: &dyn Fn(String)) -> Result<Developed, String> {
    bayer_ls::develop(img, tell)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dng_sits_beside_the_raw_and_names_it() {
        let raw = PathBuf::from("/photos/DSC_100.NEF");
        let dng = dng_of(&raw);
        assert_eq!(dng, PathBuf::from("/photos/DSC_100.mimizan.dng"));
        assert!(!is_ours(&raw));
        assert!(is_ours(&dng));
        assert!(is_ours(Path::new("/photos/DSC_100.MIMIZAN.DNG")));
    }

    #[test]
    fn two_raws_of_one_stem_do_not_share_a_dng() {
        let dir = std::env::temp_dir().join(format!("mimizan-bayer-stem-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir(&dir).unwrap();
        let nef = dir.join("a.NEF");
        let cr2 = dir.join("A.cr2");
        let dng = dir.join("a.mimizan.dng");
        let lone = dir.join("b.NEF");
        let lone_dng = dir.join("b.mimizan.dng");
        fs::write(&nef, b"x").unwrap();
        fs::write(&cr2, b"y").unwrap();
        fs::write(&dng, b"d").unwrap();
        fs::write(dir.join("a.txt"), b"t").unwrap();
        fs::write(&lone, b"z").unwrap();
        fs::write(&lone_dng, b"d").unwrap();
        fs::write(dir.join("c.NEF"), b"x").unwrap();
        fs::write(dir.join("c.mimizan.dng"), b"d").unwrap();
        // The files here are not pictures, so the question "is it a Bayer
        // mosaic" is answered by the test.
        let any = |_: &Path| true;
        let none = |_: &Path| false;
        assert_eq!(other_among(raws_named(&nef, "a"), any).unwrap(), cr2);
        assert_eq!(other_among(raws_named(&cr2, "A"), any).unwrap(), nef);
        assert!(
            other_among(raws_named(&nef, "a"), none).is_none(),
            "a file that is no mosaic is not another"
        );
        assert!(other_among(raws_named(&lone, "b"), any).is_none(), "a lone RAW has no other");
        assert!(
            other_among(raws_named(&dir.join("c.NEF"), "c"), any).is_none(),
            "the program's DNG is not another RAW"
        );
        assert!(sources_among(raws_named(&dng, "a"), any).len() == 2, "two RAWs are not a choice");
        let only_nef = |p: &Path| p == nef;
        assert_eq!(
            sources_among(raws_named(&dng, "a"), only_nef),
            std::slice::from_ref(&nef),
            "an export is not a source"
        );
        assert!(raw_of(&dng).is_none(), "of two files that are no mosaic neither is the source");
        assert_eq!(raw_of(&lone_dng).unwrap(), lone);
        assert!(listed_other(&nef, &cr2));
        assert!(!listed_other(&nef, &dng));
        assert!(!listed_other(&nef, &dir.join("a.txt")));
        let note = shared_note(&cr2);
        assert_eq!(
            note,
            "least squares is off for this file: `A.cr2` beside it has the same name, and the DNG would be one file for both"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_raw_is_found_beside_its_dng() {
        let dir = std::env::temp_dir().join(format!("mimizan-bayer-{}", std::process::id()));
        let _ = fs::create_dir(&dir);
        let raw = dir.join("DSC.NEF");
        let dng = dir.join("DSC.mimizan.dng");
        fs::write(&raw, b"x").unwrap();
        fs::write(&dng, b"x").unwrap();
        assert_eq!(raw_of(&dng).unwrap(), raw);
        assert_eq!(raw_of(&raw).unwrap(), raw);
        assert!(dng_is_ready(&dng));
        fs::write(&dng, b"").unwrap();
        assert!(!dng_is_ready(&dng));
        let _ = fs::remove_dir_all(&dir);
    }

    fn tiff_with_software(text: &str) -> Vec<u8> {
        let bytes = format!("{text}\0").into_bytes();
        let mut out = Vec::new();
        out.extend_from_slice(b"II");
        out.extend_from_slice(&42u16.to_le_bytes());
        out.extend_from_slice(&8u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&305u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&(bytes.len() as u32).to_le_bytes());
        out.extend_from_slice(&26u32.to_le_bytes());
        out.extend_from_slice(&0u32.to_le_bytes());
        out.extend_from_slice(&bytes);
        out
    }

    #[test]
    fn an_older_dng_is_not_this_version() {
        let dir = std::env::temp_dir().join(format!("mimizan-bayer-sw-{}", std::process::id()));
        let _ = fs::create_dir(&dir);
        let path = dir.join("shot.dng");
        fs::write(&path, tiff_with_software("Mimizan Lab 0.20.5 (Bayer least squares)")).unwrap();
        assert!(!dng_is_current(&path));
        fs::write(&path, tiff_with_software(SOFTWARE)).unwrap();
        assert!(dng_is_current(&path));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_folder_that_cannot_be_written_is_said_in_plain_words() {
        let plain = "the least-squares DNG was not written (this folder cannot be written to); this file is on the measured separation";
        let denied = std::io::Error::from(std::io::ErrorKind::PermissionDenied);
        let readonly = std::io::Error::from(std::io::ErrorKind::ReadOnlyFilesystem);
        assert_eq!(unwritten_note(&denied), plain);
        assert_eq!(unwritten_note(&readonly), plain);
        let full = std::io::Error::other("no space left on the device");
        assert_eq!(
            unwritten_note(&full),
            "the least-squares DNG was not written (no space left on the device); this file is on the measured separation"
        );
    }

    fn p(name: &str) -> PathBuf {
        PathBuf::from("/photos").join(name)
    }

    /// The checkbox's pairs, from the names alone. `current` and `bayer`
    /// stand in for the two questions that would otherwise read the disk.
    #[test]
    fn a_raw_and_its_dng_give_way_to_each_other() {
        let raw = p("a.NEF");
        let dng = p("a.mimizan.dng");
        let current_n = std::cell::Cell::new(0u32);
        let bayer_n = std::cell::Cell::new(0u32);
        let got = pairs(
            &[raw.clone(), dng.clone(), p("a.jpg")],
            |path| {
                current_n.set(current_n.get() + 1);
                path == dng
            },
            |_| {
                bayer_n.set(bayer_n.get() + 1);
                true
            },
        );
        assert_eq!(got.gives_way_on, vec![raw.clone()]);
        assert_eq!(got.gives_way_off, vec![dng.clone()]);
        assert_eq!(got.sources, vec![(dng.clone(), DngSource::One(raw.clone()))]);
        assert_eq!(current_n.get(), 1, "the DNG beside its RAW is the one asked");
        assert_eq!(bayer_n.get(), 0, "one RAW of the stem is the source whatever it is");

        // The same stem in the other case is the same pair.
        let upper = p("A.mimizan.dng");
        let got = pairs(&[raw.clone(), upper.clone()], |path| path == upper, |path| path == raw);
        assert_eq!(got.gives_way_on, vec![raw.clone()]);
        assert_eq!(got.gives_way_off, vec![upper.clone()]);
        assert_eq!(got.sources, vec![(upper, DngSource::One(raw.clone()))]);

        // Two Bayer files of one stem are not a choice, on or off.
        let cr2 = p("A.cr2");
        let got = pairs(&[raw.clone(), cr2.clone(), dng.clone()], |_| true, |_| true);
        assert!(got.gives_way_on.is_empty());
        assert!(got.gives_way_off.is_empty());
        assert_eq!(got.sources, vec![(dng.clone(), DngSource::Several)]);

        // A linear export beside the RAW is not a second source, and not
        // another Bayer file, so the RAW still gives way.
        let export = p("a.dng");
        let got = pairs(&[raw.clone(), export.clone(), dng.clone()], |path| path == dng, |path| path == raw);
        assert_eq!(got.gives_way_on, vec![raw.clone()]);
        assert_eq!(got.gives_way_off, vec![dng.clone()]);
        assert_eq!(got.sources, vec![(dng.clone(), DngSource::One(raw.clone()))]);

        // A DNG with no RAW is left as it is, and not opened to check it.
        let lone = p("b.mimizan.dng");
        let got = pairs(
            std::slice::from_ref(&lone),
            |_| panic!("a DNG with no RAW is not asked whether it is current"),
            |_| panic!("a lone file is not asked whether it is a mosaic"),
        );
        assert!(got.gives_way_on.is_empty());
        assert!(got.gives_way_off.is_empty());
        assert_eq!(got.sources, vec![(lone, DngSource::None)]);

        // An older build's DNG gives way when the box is off, and does not
        // take the RAW's place when the box is on.
        let got = pairs(&[raw.clone(), dng.clone()], |_| false, |_| panic!("one RAW is not asked"));
        assert!(got.gives_way_on.is_empty());
        assert_eq!(got.gives_way_off, vec![dng.clone()]);
        assert_eq!(got.sources, vec![(dng, DngSource::One(raw))]);
    }

    #[test]
    fn a_name_that_is_not_a_bayer_file_is_left_alone() {
        assert!(matches!(place(Path::new("DSC.NEF")), Place::NotBayer));
        assert!(matches!(place(Path::new("no-such-file.RAF")), Place::NotBayer));
    }
}
