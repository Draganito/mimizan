//! 16-bit grey TIFF output with embedded ICC and JSON description.

use crate::error::Result;
use crate::icc::{gray_profile, GrayTrc};
use crate::plane::Plane;
use rayon::prelude::*;
use std::borrow::Cow;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use tiff::encoder::{colortype, Rational, TiffEncoder};
use tiff::tags::{ResolutionUnit, Tag};

/// Tags written next to the pixels.
pub struct TiffMeta<'a> {
    /// `ImageDescription`: the JSON the GUI and `print` read back.
    pub description: &'a str,
    /// Which grey ICC profile to embed.
    pub trc: GrayTrc,
    /// EXIF orientation 1–8; anything else writes no tag.
    pub orientation: u16,
    /// `XResolution`/`YResolution` in dots per inch, when known.
    pub dpi: Option<f64>,
}

/// A description as the `tiff` crate takes it. The crate refuses a text
/// with a byte outside ASCII or a NUL, and it says so only once the file
/// is created: a source path with an umlaut failed the whole export and
/// left an empty file where a print may have been. Such a character
/// becomes its JSON escape, `\uXXXX` (two of them above U+FFFF). The
/// descriptions are JSON, and there these characters stand only inside
/// strings, so the text reads back as the same value.
pub fn ascii_description(s: &str) -> Cow<'_, str> {
    if s.is_ascii() && !s.contains('\0') {
        return Cow::Borrowed(s);
    }
    let mut out = String::with_capacity(s.len() + 16);
    let mut units = [0u16; 2];
    for c in s.chars() {
        if c.is_ascii() && c != '\0' {
            out.push(c);
        } else {
            for u in c.encode_utf16(&mut units) {
                out.push_str(&format!("\\u{u:04x}"));
            }
        }
    }
    Cow::Owned(out)
}

/// Quantise `clamp(v,0,1)*65535` and write a 16-bit grey TIFF in strips of
/// 64 rows.
pub fn write_gray16(path: &Path, plane: &Plane, meta: &TiffMeta<'_>) -> Result<()> {
    let data: Vec<u16> =
        plane.data.par_iter().map(|&v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16).collect();
    write_gray16_u16(path, plane.width, plane.height, &data, meta)
}

/// Write already quantised 16-bit samples; `data.len()` must be
/// `width * height` (the encoder checks and errors otherwise).
pub fn write_gray16_u16(
    path: &Path,
    width: usize,
    height: usize,
    data: &[u16],
    meta: &TiffMeta<'_>,
) -> Result<()> {
    let description = ascii_description(meta.description);
    let file = BufWriter::new(File::create(path)?);
    let mut enc = TiffEncoder::new(file)?;
    let mut img = enc.new_image::<colortype::Gray16>(width as u32, height as u32)?;
    img.rows_per_strip(64)?;
    let icc = gray_profile(meta.trc);
    {
        let d = img.encoder();
        d.write_tag(Tag::IccProfile, icc.as_slice())?;
        d.write_tag(Tag::ImageDescription, &*description)?;
        d.write_tag(Tag::Software, concat!("mimizan ", env!("CARGO_PKG_VERSION")))?;
        if meta.orientation >= 1 && meta.orientation <= 8 {
            d.write_tag(Tag::Orientation, meta.orientation)?;
        }
    }
    if let Some(dpi) = meta.dpi {
        img.resolution(ResolutionUnit::Inch, Rational { n: (dpi * 1000.0).round() as u32, d: 1000 });
    }
    img.write_data(data)?;
    Ok(())
}

/// Interleave three planes of equal size into 16-bit RGB samples.
pub fn interleave_rgb16(rgb: [&Plane; 3]) -> Result<Vec<u16>> {
    let [r, g, b] = rgb;
    if (g.width, g.height) != (r.width, r.height) || (b.width, b.height) != (r.width, r.height) {
        return Err(crate::Error::Invalid("the three print planes differ in size".into()));
    }
    let q = |v: f64| (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
    let mut data = vec![0u16; 3 * r.data.len()];
    data.par_chunks_mut(3 * r.width).enumerate().for_each(|(y, row)| {
        let (rr, gr, br) = (r.row(y), g.row(y), b.row(y));
        for x in 0..r.width {
            row[3 * x] = q(rr[x]);
            row[3 * x + 1] = q(gr[x]);
            row[3 * x + 2] = q(br[x]);
        }
    });
    Ok(data)
}

/// The plate's print: a 16-bit RGB TIFF with the profile of `space`.
pub fn write_rgb16(
    path: &Path,
    rgb: [&Plane; 3],
    space: crate::plate::PlateSpace,
    meta: &TiffMeta<'_>,
) -> Result<()> {
    let data = interleave_rgb16(rgb)?;
    let (width, height) = (rgb[0].width, rgb[0].height);
    let description = ascii_description(meta.description);
    let file = BufWriter::new(File::create(path)?);
    let mut enc = TiffEncoder::new(file)?;
    let mut img = enc.new_image::<colortype::RGB16>(width as u32, height as u32)?;
    img.rows_per_strip(64)?;
    let icc = crate::icc::rgb_profile(space, meta.trc);
    {
        let d = img.encoder();
        d.write_tag(Tag::IccProfile, icc.as_slice())?;
        d.write_tag(Tag::ImageDescription, &*description)?;
        d.write_tag(Tag::Software, concat!("mimizan ", env!("CARGO_PKG_VERSION")))?;
        if meta.orientation >= 1 && meta.orientation <= 8 {
            d.write_tag(Tag::Orientation, meta.orientation)?;
        }
    }
    if let Some(dpi) = meta.dpi {
        img.resolution(ResolutionUnit::Inch, Rational { n: (dpi * 1000.0).round() as u32, d: 1000 });
    }
    img.write_data(&data)?;
    Ok(())
}

/// 8-bit grey TIFF (mask sidecar).
pub fn write_gray8(path: &Path, width: usize, height: usize, data: &[u8], description: &str) -> Result<()> {
    let file = BufWriter::new(File::create(path)?);
    let mut enc = TiffEncoder::new(file)?;
    let mut img = enc.new_image::<colortype::Gray8>(width as u32, height as u32)?;
    img.rows_per_strip(256)?;
    img.encoder().write_tag(Tag::ImageDescription, &*ascii_description(description))?;
    img.write_data(data)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiffin::{read_gray, read_gray_header};

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mimizan-tiffout-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// The description carries the source path, and a path is not ASCII:
    /// an umlaut, an accent, a letter outside the basic plane.
    const DESCRIPTION: &str = r#"{"source":"/home/jörg/Köln/été/𝄞 01.NEF","stage":"print"}"#;

    fn same_json(read: Option<String>) {
        let read: serde_json::Value = serde_json::from_str(&read.expect("a description")).unwrap();
        let written: serde_json::Value = serde_json::from_str(DESCRIPTION).unwrap();
        assert_eq!(read, written);
    }

    #[test]
    fn a_description_is_made_ascii_by_json_escapes_alone() {
        assert!(matches!(ascii_description(r#"{"a":"plain"}"#), Cow::Borrowed(_)));
        assert_eq!(ascii_description("Kö"), "K\\u00f6");
        assert_eq!(ascii_description("𝄞"), "\\ud834\\udd1e");
        assert_eq!(ascii_description("a\0b"), "a\\u0000b");
        assert!(ascii_description(DESCRIPTION).is_ascii());
    }

    #[test]
    fn a_description_outside_ascii_is_written_and_read_back_the_same() {
        let meta =
            TiffMeta { description: DESCRIPTION, trc: GrayTrc::Gamma(2.2), orientation: 1, dpi: Some(300.0) };
        let mut p = Plane::zeros(8, 6);
        p.data.iter_mut().enumerate().for_each(|(i, v)| *v = i as f64 / 48.0);

        let grey = tmp("grey.tif");
        write_gray16(&grey, &p, &meta).unwrap();
        let back = read_gray(&grey).unwrap();
        assert_eq!((back.plane.width, back.plane.height, back.bits), (8, 6, 16));
        same_json(back.description);

        let rgb = tmp("rgb.tif");
        write_rgb16(&rgb, [&p, &p, &p], crate::plate::PlateSpace::Srgb, &meta).unwrap();
        same_json(read_gray_header(&rgb).unwrap());

        let mask = tmp("mask.tif");
        write_gray8(&mask, 8, 6, &[0u8; 48], DESCRIPTION).unwrap();
        same_json(read_gray_header(&mask).unwrap());
    }
}
