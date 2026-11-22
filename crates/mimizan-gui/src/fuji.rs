//! A Fuji X-Trans RAF in the strip and at open.
//!
//! The mosaic is not separated here. While no DNG sits beside the RAF the
//! strip shows the camera's embedded JPEG. Opening the RAF writes that DNG,
//! with the highlights left as recorded, and the file that then opens is
//! the DNG: three channels, the same preview as any other linear DNG. A
//! Bayer RAF is not this and is left to the ordinary path. A later open
//! reads the DNG and does not look at the RAF again.

use mimizan_core::thumb::Thumb;
use mimizan_lab_xtrans::convert::frame_of;
use mimizan_lab_xtrans::file::{self, Event, Options};
use rawler::decoders::RawDecodeParams;
use rawler::rawsource::RawSource;
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
}

/// What the head of an X-Trans RAF says, and where its DNG will be.
pub struct Head {
    pub dng: PathBuf,
    pub width: usize,
    pub height: usize,
    pub make: String,
    pub model: String,
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

/// The head only: `true` asks rawler not to decode the mosaic.
pub fn place(path: &Path) -> Place {
    if !is_raf(path) {
        return Place::NotXtrans;
    }
    let Ok(source) = RawSource::new(path) else {
        return Place::NotXtrans;
    };
    let Ok(decoder) = rawler::get_decoder(&source) else {
        return Place::NotXtrans;
    };
    let Ok(img) = decoder.raw_image(&source, &RawDecodeParams::default(), true) else {
        return Place::NotXtrans;
    };
    let Ok(frame) = frame_of(&img) else {
        return Place::NotXtrans;
    };
    let dng = file::beside(path);
    if dng.is_file() {
        return Place::Ready(dng);
    }
    Place::Needs(Head {
        dng,
        width: frame.width,
        height: frame.height,
        make: img.clean_make,
        model: img.clean_model,
    })
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
