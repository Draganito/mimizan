//! A Fuji X-Trans RAF in the strip and at open.
//!
//! The mosaic is not separated here. While no DNG sits beside the RAF the
//! strip shows the camera's embedded JPEG. Opening the RAF writes that DNG,
//! with the highlights left as recorded, and the file that then opens is
//! the DNG: three channels, the same preview as any other linear DNG. A
//! Bayer RAF is not this and is left to the ordinary path. A later open
//! reads the DNG and does not look at the RAF again. A file of that name
//! which this program did not write does not stand in for the RAF.

use mimizan_core::decode::Exposure;
use mimizan_core::thumb::Thumb;
use mimizan_lab_xtrans::convert::frame_of;
use mimizan_lab_xtrans::file::{self, Event, Options, Report};
use rawler::decoders::RawDecodeParams;
use std::io::Cursor;
use std::path::{Path, PathBuf};

/// Where an X-Trans RAF stands.
pub enum Place {
    /// Not an X-Trans RAF.
    NotXtrans,
    /// The DNG beside the RAF is the file to open.
    Ready(PathBuf),
    /// The RAF still has to be written out.
    Needs(Head),
    /// `<stem>.dng` is there, and this program did not write it.
    Taken(PathBuf),
}

/// What the file named `<stem>.dng` means, before any RAF is read.
#[derive(Debug, PartialEq, Eq)]
enum Beside {
    /// No file of that name.
    Absent,
    /// This program wrote it. An older version of the converter still counts.
    Ours,
    /// A file is there, and it is not one this program wrote.
    Taken,
}

/// What the head of an X-Trans RAF says, and where its DNG will be.
pub struct Head {
    pub dng: PathBuf,
    pub width: usize,
    pub height: usize,
    pub make: String,
    pub model: String,
    /// Exposure from the RAF's head. The strip shows it while the tile is
    /// still the camera's JPEG.
    pub exposure: Exposure,
}

/// Bytes one photosite of the developed frame holds at the peak of the
/// conversion. Measured on the pinned program: 7.2 to 7.8 GB for 26
/// million photosites, 12 GB for 40 million.
const BYTES_PER_PHOTOSITE: u64 = 300;

/// Memory the conversion of a `width`×`height` frame holds, bytes. The
/// darkroom's Bayer figure is about a quarter of this, and the DNG's own
/// development, afterwards, is smaller still. The two do not overlap:
/// the conversion is finished before the DNG is opened.
pub fn convert_memory(width: usize, height: usize) -> u64 {
    (width as u64).saturating_mul(height as u64).saturating_mul(BYTES_PER_PHOTOSITE)
}

/// One line for the log after the DNG has been written.
pub fn written_note(r: &Report) -> String {
    format!(
        "DNG written in {:.0} s, prior f^{:.2} and f^{:.2}, {:.3}% burnt out and kept",
        r.total,
        r.prior.alpha,
        r.prior.alpha_c,
        100.0 * r.burnt_share()
    )
}

/// The DNG the plate is printed from: the highlights stay in the file.
pub fn plate_options() -> Options {
    Options { keep_highlights: true, ..Options::default() }
}

/// What the toolbar says while [`file::convert`] works.
pub fn words(event: &Event<'_>) -> String {
    match event {
        Event::Decoded { .. } => "reading the X-Trans file".to_owned(),
        Event::Doing(what) => (*what).to_owned(),
        Event::Separated(share) => {
            format!("separating luminance and colour, {:.0}%", share.clamp(0.0, 1.0) * 100.0)
        }
        Event::Writing => "writing the DNG".to_owned(),
    }
}

fn is_raf(path: &Path) -> bool {
    path.extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("raf"))
}

/// The start of [`file::SOFTWARE`], through the space after the program
/// name. An older converter wrote that same start.
fn software_prefix() -> &'static str {
    let software = file::SOFTWARE;
    match software.find(' ') {
        Some(i) => &software[..=i],
        None => software,
    }
}

/// What [`place`] decides from whether a file of the DNG's name is there
/// and what its Software tag says. No RAF is opened.
fn dng_beside(present: bool, software: Option<&str>) -> Beside {
    if !present {
        Beside::Absent
    } else if software.is_some_and(|s| s.starts_with(software_prefix())) {
        Beside::Ours
    } else {
        Beside::Taken
    }
}

/// A name that is taken, or that cannot be ruled out. Only a name that
/// is not there may be written.
pub(crate) fn name_is_there(path: &Path) -> bool {
    !matches!(
        std::fs::symlink_metadata(path),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound
    )
}

/// This program wrote `path`. The Software tag starts as the converter
/// writes it, and an older version of that converter still counts.
pub fn dng_is_ours(path: &Path) -> bool {
    matches!(dng_beside(name_is_there(path), crate::software::software_of(path).as_deref()), Beside::Ours)
}

/// `dng` is the conversion of `raf`: the name the converter gives it,
/// and written by this program.
pub fn stands_in(dng: &Path, raf: &Path) -> bool {
    is_raf(raf) && file::beside(raf) == dng && dng_is_ours(dng)
}

/// The name the conversion is written under before it is linked onto
/// `<stem>.dng`: `<stem>.dng.<pid>.mimizan-new`. The process id keeps
/// two windows from sharing the part the converter writes beside this
/// name. The name is the program's own, so a stale one may be removed:
/// the conversion renames onto whatever its target is at the end.
pub fn staging_dng(dng: &Path) -> PathBuf {
    let mut name = dng.as_os_str().to_owned();
    name.push(format!(".{}.mimizan-new", std::process::id()));
    PathBuf::from(name)
}

/// `<stem>.dng.mimizan-new`, the staging name from before the process
/// id was part of it. A leftover of that name is this program's file
/// and is removed before a conversion.
pub(crate) fn earlier_staging(dng: &Path) -> PathBuf {
    let mut name = dng.as_os_str().to_owned();
    name.push(".mimizan-new");
    PathBuf::from(name)
}

/// What an open says when the folder will not take the DNG. An X-Trans
/// file has no measured separation to open instead.
pub const UNWRITABLE_FOLDER: &str = "The DNG of an X-Trans file is written beside it, and this folder cannot be written to. Copy the RAF to a folder that can.";

/// A folder that refused the write, from the error's kind.
pub(crate) fn folder_denied(kind: std::io::ErrorKind) -> bool {
    matches!(kind, std::io::ErrorKind::PermissionDenied | std::io::ErrorKind::ReadOnlyFilesystem)
}

/// The error an X-Trans open shows. A folder that cannot be written to
/// says where to put the RAF; any other failure keeps its own text.
pub fn write_error(kind: std::io::ErrorKind, text: &str) -> String {
    if folder_denied(kind) {
        UNWRITABLE_FOLDER.to_owned()
    } else {
        text.to_owned()
    }
}

/// [`write_error`] for an error that has already been made into text, as
/// the converter returns. The operating system's wording is recognised.
pub fn write_error_text(text: &str) -> String {
    if text_is_unwritable(text) {
        UNWRITABLE_FOLDER.to_owned()
    } else {
        text.to_owned()
    }
}

fn text_is_unwritable(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    [
        "permission denied",
        "operation not permitted",
        "read-only file system",
        "read-only filesystem",
        "access is denied",
    ]
    .into_iter()
    .any(|mark| lower.contains(mark))
}

/// The sentence an open shows for a `<stem>.dng` this program did not write.
pub fn taken_sentence(dng: &Path) -> String {
    let name = dng
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| dng.display().to_string());
    format!("`{name}` beside this RAF was not written by Mimizan. Move or rename it, and the RAF opens.")
}

/// The head of an X-Trans RAF, or `None` when `path` is not one. The
/// mosaic itself is not decoded.
pub(crate) fn head(path: &Path) -> Option<Head> {
    if !is_raf(path) {
        return None;
    }
    let source = mimizan_core::decode::raw_source(path).ok()?;
    let decoder = rawler::get_decoder(&source).ok()?;
    let img = decoder.raw_image(&source, &RawDecodeParams::default(), true).ok()?;
    let frame = frame_of(&img).ok()?;
    let exposure = decoder
        .raw_metadata(&source, &RawDecodeParams::default())
        .map(|md| mimizan_core::decode::exposure_from(&md))
        .unwrap_or_default();
    Some(Head {
        dng: file::beside(path),
        width: frame.width,
        height: frame.height,
        make: img.clean_make,
        model: img.clean_model,
        exposure,
    })
}

/// The head only. A `<stem>.dng` stands in for the RAF only when its
/// Software tag starts as this program's converter writes it.
pub fn place(path: &Path) -> Place {
    let Some(head) = head(path) else {
        return Place::NotXtrans;
    };
    let dng = head.dng.clone();
    match dng_beside(name_is_there(&dng), crate::software::software_of(&dng).as_deref()) {
        Beside::Ours => Place::Ready(dng),
        Beside::Taken => Place::Taken(dng),
        Beside::Absent => Place::Needs(head),
    }
}

/// The camera's JPEG in `raf`, shrunk so the long side is at most
/// `long_edge`, as the strip's picture until the DNG exists.
pub fn camera_jpeg(raf: &Path, long_edge: usize) -> Result<Thumb, String> {
    let jpeg = embedded_jpeg(raf)?;
    jpeg_thumb(&jpeg, long_edge)
}

/// Offset and length of the embedded JPEG, at bytes 84 and 88 of a RAF.
fn embedded_jpeg(raf: &Path) -> Result<Vec<u8>, String> {
    use std::io::{Read, Seek, SeekFrom};
    let fail = |e: std::io::Error| format!("the camera's preview cannot be read: {e}");
    let mut file = std::fs::File::open(raf).map_err(fail)?;
    let mut head = [0u8; 92];
    match file.read_exact(&mut head) {
        Err(e) if e.kind() != std::io::ErrorKind::UnexpectedEof => return Err(fail(e)),
        Ok(()) if head.starts_with(b"FUJIFILMCCD-RAW") => {}
        _ => return Err("the file holds no camera preview".into()),
    }
    let at = |i: usize| u64::from(u32::from_be_bytes([head[i], head[i + 1], head[i + 2], head[i + 3]]));
    let (offset, length) = (at(84), at(88));
    if length == 0 || offset.saturating_add(length) > file.metadata().map_err(fail)?.len() {
        return Err("the file holds no camera preview".into());
    }
    let mut jpeg = vec![0u8; length as usize];
    file.seek(SeekFrom::Start(offset)).map_err(fail)?;
    file.read_exact(&mut jpeg).map_err(fail)?;
    Ok(jpeg)
}

fn jpeg_thumb(jpeg: &[u8], long_edge: usize) -> Result<Thumb, String> {
    use image::ImageDecoder;
    let mut decoder = image::ImageReader::with_format(Cursor::new(jpeg), image::ImageFormat::Jpeg)
        .into_decoder()
        .map_err(|e| format!("the camera's preview cannot be read: {e}"))?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut picture = image::DynamicImage::from_decoder(decoder)
        .map_err(|e| format!("the camera's preview cannot be read: {e}"))?;
    picture.apply_orientation(orientation);
    let (w, h) = (picture.width() as usize, picture.height() as usize);
    let long = w.max(h).max(1);
    let edge = long_edge.max(1);
    if long > edge {
        let tw = (w * edge / long).max(1) as u32;
        let th = (h * edge / long).max(1) as u32;
        picture = picture.resize(tw, th, image::imageops::FilterType::Triangle);
    }
    let rgb_img = picture.to_rgb8();
    let (width, height) = (rgb_img.width() as usize, rgb_img.height() as usize);
    let rgb = rgb_img.into_raw();
    let mut gray = vec![0u8; width * height];
    for (g, px) in gray.iter_mut().zip(rgb.as_chunks::<3>().0) {
        *g =
            (0.2126 * f32::from(px[0]) + 0.7152 * f32::from(px[1]) + 0.0722 * f32::from(px[2])).round() as u8;
    }
    Ok(Thumb { width, height, gray, rgb })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_staging_name_belongs_to_this_process() {
        let dng = Path::new("/photos/DSCF1.dng");
        assert_eq!(
            staging_dng(dng),
            PathBuf::from(format!("/photos/DSCF1.dng.{}.mimizan-new", std::process::id()))
        );
        assert_eq!(earlier_staging(dng), PathBuf::from("/photos/DSCF1.dng.mimizan-new"));
    }

    #[test]
    fn the_dng_for_the_plate_keeps_the_highlights() {
        let o = plate_options();
        assert!(o.keep_highlights);
        assert!(!o.force);
        assert!(!o.uncompressed);
    }

    #[test]
    fn a_bayer_name_is_not_an_xtrans_raf() {
        assert!(matches!(place(Path::new("DSC.NEF")), Place::NotXtrans));
        assert!(matches!(place(Path::new("no-such-file.RAF")), Place::NotXtrans));
    }

    #[test]
    fn a_dng_stands_in_for_the_raf_only_when_this_program_wrote_it() {
        let ours = file::SOFTWARE;
        let prefix = software_prefix();
        assert!(ours.starts_with(prefix) && prefix.ends_with(' ') && ours.len() > prefix.len());
        let older = format!("{prefix}0.1.0 (older)");
        assert_eq!(dng_beside(false, None), Beside::Absent);
        assert_eq!(dng_beside(false, Some(ours)), Beside::Absent);
        assert_eq!(dng_beside(true, Some(ours)), Beside::Ours);
        assert_eq!(dng_beside(true, Some(&older)), Beside::Ours);
        assert_eq!(dng_beside(true, Some("Adobe DNG Converter")), Beside::Taken);
        assert_eq!(dng_beside(true, Some("")), Beside::Taken);
        assert_eq!(dng_beside(true, None), Beside::Taken);
    }

    #[test]
    fn a_conversion_stands_in_for_its_raf_and_another_dng_does_not() {
        let dir = std::env::temp_dir().join(format!("mimizan-fuji-stands-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir(&dir).unwrap();
        let raf = dir.join("DSCF1.RAF");
        let dng = dir.join("DSCF1.dng");
        std::fs::write(&dng, crate::software::tiff_with_software(file::SOFTWARE)).unwrap();
        assert!(stands_in(&dng, &raf));
        assert!(!stands_in(&dng, &dir.join("DSCF1.NEF")), "only a RAF is converted");
        assert!(!stands_in(&dng, &dir.join("DSCF2.RAF")), "another picture");
        std::fs::write(&dng, crate::software::tiff_with_software("Adobe DNG Converter")).unwrap();
        assert!(!stands_in(&dng, &raf), "another program's DNG is its own file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn an_unwritable_folder_says_to_copy_the_raf() {
        let sentence = UNWRITABLE_FOLDER;
        assert!(sentence.contains("Copy the RAF"));
        for kind in [std::io::ErrorKind::PermissionDenied, std::io::ErrorKind::ReadOnlyFilesystem] {
            let err = std::io::Error::from(kind);
            assert_eq!(write_error(kind, &err.to_string()), sentence);
            assert_eq!(write_error_text(&err.to_string()), sentence);
        }
        // The wording the operating system puts on the error the converter
        // returns as text. Codes differ by system; the kind does not.
        for code in 1..96 {
            let err = std::io::Error::from_raw_os_error(code);
            if folder_denied(err.kind()) {
                assert_eq!(write_error_text(&err.to_string()), sentence, "{code}: {err}");
            }
        }
        let other = "no space left on the device";
        assert_eq!(write_error(std::io::ErrorKind::StorageFull, other), other);
        assert_eq!(write_error_text(other), other);
    }

    #[test]
    fn a_foreign_dng_is_named_and_left_for_the_user_to_move() {
        let text = taken_sentence(Path::new("/photos/DSCF1234.dng"));
        assert_eq!(
            text,
            "`DSCF1234.dng` beside this RAF was not written by Mimizan. Move or rename it, and the RAF opens."
        );
    }

    #[test]
    fn the_jpeg_is_taken_from_the_raf_head() {
        let path = std::env::temp_dir().join(format!("mimizan-fuji-head-{}.raf", std::process::id()));
        let mut raf = vec![0u8; 200];
        raf[..15].copy_from_slice(b"FUJIFILMCCD-RAW");
        raf[84..88].copy_from_slice(&150u32.to_be_bytes());
        raf[88..92].copy_from_slice(&4u32.to_be_bytes());
        raf[150..154].copy_from_slice(b"JPEG");
        std::fs::write(&path, &raf).unwrap();
        assert_eq!(embedded_jpeg(&path).unwrap(), b"JPEG");
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn the_conversion_holds_more_than_a_bayer_development() {
        let px = 26_000_000usize;
        let side = (px as f64).sqrt() as usize;
        let writing = convert_memory(side, px / side);
        let bayer = mimizan_core::pipeline::develop_memory(
            side,
            px / side,
            mimizan_core::decode::SensorKind::Bayer(mimizan_core::cfa::BayerPhase::RGGB),
        );
        assert!(writing > bayer * 3, "{writing} vs {bayer}");
        assert_eq!(convert_memory(0, 100), 0);
    }

    #[test]
    fn the_note_names_the_prior_and_the_burnt_share() {
        let note = written_note(&Report {
            make: String::new(),
            model: String::new(),
            photosites: (10, 10),
            pixels: (10, 10),
            prior: mimizan_lab_xtrans::xtrans::Prior { alpha: 1.25, alpha_c: 2.5, ..Default::default() },
            balance_from_file: true,
            burnt: 5,
            below: 0,
            sums: [0.0; 3],
            seconds: [0.0; 3],
            total: 43.2,
        });
        assert_eq!(note, "DNG written in 43 s, prior f^1.25 and f^2.50, 5.000% burnt out and kept");
    }

    #[test]
    fn a_jpeg_becomes_the_strip_picture() {
        let image = image::RgbImage::from_fn(8, 4, |x, _| image::Rgb([if x < 4 { 220 } else { 10 }, 20, 30]));
        let mut jpeg = Vec::new();
        image::DynamicImage::ImageRgb8(image)
            .write_to(&mut Cursor::new(&mut jpeg), image::ImageFormat::Jpeg)
            .unwrap();
        let thumb = jpeg_thumb(&jpeg, 480).unwrap();
        assert_eq!((thumb.width, thumb.height), (8, 4));
        assert!(thumb.rgb[0] > thumb.rgb[3 * 7]);
        assert_eq!(thumb.gray.len(), 8 * 4);
        let small = jpeg_thumb(&jpeg, 4).unwrap();
        assert_eq!(small.width.max(small.height), 4);
    }
}
