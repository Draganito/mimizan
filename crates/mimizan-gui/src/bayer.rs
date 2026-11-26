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
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

/// What a DNG written here ends its name with.
const MARK: &str = ".mimizan.dng";

/// What the DNG says it was written by.
const SOFTWARE: &str = concat!("Mimizan Lab ", env!("CARGO_PKG_VERSION"), " (Bayer least squares)");

/// Where a Bayer file stands when the checkbox is on.
pub enum Place {
    /// Not a Bayer mosaic, or already the DNG this path writes.
    NotBayer,
    /// The DNG beside the RAW is the file to open.
    Ready(PathBuf),
    /// The RAW still has to be written out.
    Needs(Head),
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

/// The RAW a DNG written here came from, when that RAW is still beside it.
/// A path that is not such a DNG is returned as it stands.
pub fn raw_of(path: &Path) -> Option<PathBuf> {
    if !is_ours(path) {
        return Some(path.to_path_buf());
    }
    let name = path.file_name()?.to_str()?;
    let stem = &name[..name.len() - MARK.len()];
    let dir = path.parent()?;
    for entry in fs::read_dir(dir).ok()?.flatten() {
        let p = entry.path();
        if p == path {
            continue;
        }
        let Some(file_stem) = p.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        if file_stem.eq_ignore_ascii_case(stem) && crate::browser::is_raw(&p) {
            return Some(p);
        }
    }
    None
}

/// The head only. A file this path does not write out is [`Place::NotBayer`].
/// A DNG this build already wrote is that too: it opens as it stands. One
/// an older build wrote is written again from the RAW beside it.
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
pub fn write(head: &Head, tell: &dyn Fn(String)) -> Result<String, String> {
    if dng_is_current(&head.dng) {
        return Err(format!("{} is there already", head.dng.display()));
    }
    let started = Instant::now();
    tell("reading the Bayer file".into());
    let source = mimizan_core::decode::raw_source(&head.raw).map_err(|e| format!("cannot be read: {e}"))?;
    let decoder =
        rawler::get_decoder(&source).map_err(|e| format!("not a raw file this program reads: {e}"))?;
    let img = decoder
        .raw_image(&source, &RawDecodeParams::default(), false)
        .map_err(|e| format!("cannot be decoded: {e}"))?;
    let dev = develop(&img, tell)?;
    let burnt = dev.burnt as f64 / (dev.frame.width * dev.frame.height) as f64;
    let (alpha, alpha_c) = (dev.prior.alpha, dev.prior.alpha_c);

    tell("writing the DNG".into());
    // Whole under its name or not there: a DNG cut off half way would be
    // taken for the finished one at the next open.
    mimizan_core::outfile::write_whole(&head.dng, |out| {
        dng::write(out, &source, decoder.as_ref(), img, dev, true, SOFTWARE).map_err(std::io::Error::other)
    })
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "DNG written in {:.0} s, prior f^{alpha:.2} and f^{alpha_c:.2}, {:.3}% burnt out and kept",
        started.elapsed().as_secs_f64(),
        100.0 * burnt
    ))
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
    fn a_name_that_is_not_a_bayer_file_is_left_alone() {
        assert!(matches!(place(Path::new("DSC.NEF")), Place::NotBayer));
        assert!(matches!(place(Path::new("no-such-file.RAF")), Place::NotBayer));
    }
}
