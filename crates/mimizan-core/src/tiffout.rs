//! TIFF output with embedded ICC and JSON description: the 16-bit grey
//! negative and print, and the print's other files (RGB, 8 bit).

use crate::error::Result;
use crate::icc::{gray_profile, GrayTrc};
use crate::plane::Plane;
use rayon::prelude::*;
use std::borrow::Cow;
use std::fs::File;
use std::io::{BufWriter, Seek, Write};
use std::path::Path;
use tiff::encoder::{colortype, Rational, TiffEncoder, TiffKind, TiffKindBig, TiffKindStandard, TiffValue};
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
    let icc = gray_profile(meta.trc);
    write_image::<colortype::Gray16>(
        path,
        &Image { width, height, rows_per_strip: 64, data },
        &meta.tags(&icc),
    )
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
    let icc = crate::icc::rgb_profile(space, meta.trc);
    write_image::<colortype::RGB16>(
        path,
        &Image { width, height, rows_per_strip: 64, data: &data },
        &meta.tags(&icc),
    )
}

/// The samples of a print as they stand in its file: one channel or three
/// interleaved, 8 or 16 bits each.
pub enum Samples<'a> {
    Gray8(&'a [u8]),
    Gray16(&'a [u16]),
    Rgb8(&'a [u8]),
    Rgb16(&'a [u16]),
}

/// A print's samples, already quantised, with the profile `icc` that says
/// what they are, in strips of 64 rows. `meta.trc` is not read here: the
/// profile is the caller's.
pub fn write_samples(
    path: &Path,
    width: usize,
    height: usize,
    samples: Samples<'_>,
    icc: &[u8],
    meta: &TiffMeta<'_>,
) -> Result<()> {
    let tags = meta.tags(icc);
    match samples {
        Samples::Gray8(data) => {
            write_image::<colortype::Gray8>(path, &Image { width, height, rows_per_strip: 64, data }, &tags)
        }
        Samples::Gray16(data) => {
            write_image::<colortype::Gray16>(path, &Image { width, height, rows_per_strip: 64, data }, &tags)
        }
        Samples::Rgb8(data) => {
            write_image::<colortype::RGB8>(path, &Image { width, height, rows_per_strip: 64, data }, &tags)
        }
        Samples::Rgb16(data) => {
            write_image::<colortype::RGB16>(path, &Image { width, height, rows_per_strip: 64, data }, &tags)
        }
    }
}

/// 8-bit grey TIFF (mask sidecar).
pub fn write_gray8(path: &Path, width: usize, height: usize, data: &[u8], description: &str) -> Result<()> {
    let tags = Tags { description, icc: None, software: false, orientation: 0, dpi: None };
    write_image::<colortype::Gray8>(path, &Image { width, height, rows_per_strip: 256, data }, &tags)
}

/// The samples of one picture and how they are cut into strips.
struct Image<'a, T> {
    width: usize,
    height: usize,
    rows_per_strip: u32,
    data: &'a [T],
}

/// What is written beside the samples.
struct Tags<'a> {
    description: &'a str,
    icc: Option<&'a [u8]>,
    /// `Software`: this program and its version.
    software: bool,
    /// EXIF orientation 1–8; anything else writes no tag.
    orientation: u16,
    dpi: Option<f64>,
}

impl TiffMeta<'_> {
    fn tags<'a>(&'a self, icc: &'a [u8]) -> Tags<'a> {
        Tags {
            description: self.description,
            icc: Some(icc),
            software: true,
            orientation: self.orientation,
            dpi: self.dpi,
        }
    }
}

/// Whether a file of `sample_bytes` of pixels in `strips` strips, with
/// `tag_bytes` of profile and description, is within a classic TIFF. Its
/// offsets are 32 bits, and the directory, the profile, the description
/// and the two lists of the strips are written after the pixels, so the
/// whole file has to end below 4 GiB. Counted with room to spare: 16
/// bytes a strip for the two lists and their padding, and 64 KiB for the
/// header and the directory.
fn fits_classic(sample_bytes: u64, strips: u64, tag_bytes: u64) -> bool {
    sample_bytes
        .checked_add(tag_bytes)
        .and_then(|n| n.checked_add(strips.saturating_mul(16)))
        .and_then(|n| n.checked_add(64 << 10))
        .is_some_and(|n| n <= u64::from(u32::MAX))
}

/// One picture written to `path`: a classic TIFF, which every program
/// reads, when the file ends below its 4 GiB; a BigTIFF, the same tags
/// with offsets of 64 bits, when it does not. The classic encoder fails
/// at the first strip that starts past 4 GiB, with that much of the file
/// written.
fn write_image<C>(path: &Path, image: &Image<'_, C::Inner>, tags: &Tags<'_>) -> Result<()>
where
    C: colortype::ColorType,
    [C::Inner]: TiffValue,
{
    let description = ascii_description(tags.description);
    let sample_bytes = std::mem::size_of_val(image.data) as u64;
    let strips = (image.height as u64).div_ceil(u64::from(image.rows_per_strip.max(1)));
    let tag_bytes = (description.len() + tags.icc.map_or(0, <[u8]>::len)) as u64;
    let file = BufWriter::new(File::create(path)?);
    if fits_classic(sample_bytes, strips, tag_bytes) {
        encode::<_, TiffKindStandard, C>(TiffEncoder::new(file)?, image, &description, tags)
    } else {
        tracing::info!(
            "{}: {:.1} GiB is past a classic TIFF, written as BigTIFF",
            path.display(),
            sample_bytes as f64 / (1u64 << 30) as f64
        );
        encode::<_, TiffKindBig, C>(TiffEncoder::new_big(file)?, image, &description, tags)
    }
}

fn encode<W, K, C>(
    mut enc: TiffEncoder<W, K>,
    image: &Image<'_, C::Inner>,
    description: &str,
    tags: &Tags<'_>,
) -> Result<()>
where
    W: Write + Seek,
    K: TiffKind,
    C: colortype::ColorType,
    [C::Inner]: TiffValue,
{
    let mut img = enc.new_image::<C>(image.width as u32, image.height as u32)?;
    img.rows_per_strip(image.rows_per_strip)?;
    {
        let d = img.encoder();
        if let Some(icc) = tags.icc {
            d.write_tag(Tag::IccProfile, icc)?;
        }
        d.write_tag(Tag::ImageDescription, description)?;
        if tags.software {
            d.write_tag(Tag::Software, concat!("mimizan ", env!("CARGO_PKG_VERSION")))?;
        }
        if tags.orientation >= 1 && tags.orientation <= 8 {
            d.write_tag(Tag::Orientation, tags.orientation)?;
        }
    }
    if let Some(dpi) = tags.dpi {
        img.resolution(ResolutionUnit::Inch, Rational { n: (dpi * 1000.0).round() as u32, d: 1000 });
    }
    img.write_data(image.data)?;
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

    /// A classic TIFF ends below 4 GiB. A plate's print is six bytes a
    /// pixel: 715 megapixels fit, 716 do not. A grey one is two.
    #[test]
    fn a_picture_past_four_gigabytes_is_no_classic_tiff() {
        let strips = |h: u64| h.div_ceil(64);
        assert!(fits_classic(6 * 26_000 * 27_500, strips(27_500), 4096));
        assert!(!fits_classic(6 * 26_000 * 27_540, strips(27_540), 4096));
        assert!(fits_classic(2 * 40_000 * 53_000, strips(53_000), 4096));
        assert!(!fits_classic(2 * 40_000 * 54_000, strips(54_000), 4096));
        // What is written after the pixels counts too.
        let full = u64::from(u32::MAX) - (64 << 10) - 16;
        assert!(fits_classic(full, 1, 0));
        assert!(!fits_classic(full, 1, 1));
        assert!(!fits_classic(full, 2, 0));
        assert!(!fits_classic(u64::MAX, 1, 1));
    }

    /// The BigTIFF a picture past 4 GiB is written as, at a size a test
    /// can write: the same pixels and the same tags read back.
    #[test]
    fn a_bigtiff_reads_back_as_the_classic_one() {
        let meta =
            TiffMeta { description: DESCRIPTION, trc: GrayTrc::Gamma(2.2), orientation: 6, dpi: Some(300.0) };
        let (w, h) = (40usize, 150usize);
        let data: Vec<u16> = (0..w * h).map(|i| (i * 11 % 65536) as u16).collect();
        let icc = gray_profile(meta.trc);
        let image = Image { width: w, height: h, rows_per_strip: 64, data: &data[..] };

        let classic = tmp("classic.tif");
        write_gray16_u16(&classic, w, h, &data, &meta).unwrap();
        let big = tmp("big.tif");
        let file = BufWriter::new(File::create(&big).unwrap());
        encode::<_, TiffKindBig, colortype::Gray16>(
            TiffEncoder::new_big(file).unwrap(),
            &image,
            &ascii_description(DESCRIPTION),
            &meta.tags(&icc),
        )
        .unwrap();

        // The two differ in the header: 42 for the classic, 43 for the big.
        let head = |p: &Path| std::fs::read(p).unwrap()[..4].to_vec();
        assert_eq!(head(&classic), [b'I', b'I', 42, 0]);
        assert_eq!(head(&big), [b'I', b'I', 43, 0]);
        let (a, b) = (read_gray(&classic).unwrap(), read_gray(&big).unwrap());
        assert_eq!((b.plane.width, b.plane.height, b.bits, b.orientation), (w, h, 16, 6));
        assert_eq!(a.plane.data, b.plane.data);
        assert_eq!(a.orientation, b.orientation);
        same_json(b.description);
        let tag = |p: &Path, t: Tag| {
            let mut dec = tiff::decoder::Decoder::new(File::open(p).unwrap()).unwrap();
            dec.get_tag_u8_vec(t).ok()
        };
        assert_eq!(tag(&big, Tag::IccProfile), Some(icc.clone()));
        assert_eq!(tag(&big, Tag::IccProfile), tag(&classic, Tag::IccProfile));
    }
}
