//! TIFF output with embedded ICC and JSON description: the 16-bit grey
//! negative and print, and the print's other files (RGB, 8 bit).

use crate::error::Result;
use crate::icc::{gray_profile, GrayTrc};
use crate::plane::Plane;
use rayon::prelude::*;
use std::borrow::Cow;
use std::io::{Seek, Write};
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

/// One trailing space when the length plus the NUL the encoder writes is
/// odd. TIFF wants the next value on a word boundary, and the encoder
/// writes it immediately after this one. The text is already ASCII; in a
/// JSON description the space is white space.
fn with_even_nul(s: &str) -> Cow<'_, str> {
    if (s.len() + 1).is_multiple_of(2) {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(format!("{s} "))
    }
}

/// Quantise `clamp(v,0,1)*65535` and write a 16-bit grey TIFF in strips of
/// 64 rows.
pub fn write_gray16(path: &Path, plane: &Plane, meta: &TiffMeta<'_>) -> Result<()> {
    write_gray16_range(path, plane, 1.0, meta)
}

/// [`write_gray16`] for a negative whose plane goes above 1: a sample is
/// `v / range`, so the file's full scale stands for the scene value
/// `range` (`pipeline::scene_range`), and the description has to say so
/// (`NegativeInfo::range`). A range of 1 is `write_gray16` to the bit.
pub fn write_gray16_range(path: &Path, plane: &Plane, range: f64, meta: &TiffMeta<'_>) -> Result<()> {
    if !(range.is_finite() && range >= 1.0) {
        return Err(crate::Error::Invalid(format!("a negative's range is 1 or more, not {range}")));
    }
    let data: Vec<u16> =
        plane.data.par_iter().map(|&v| ((v / range).clamp(0.0, 1.0) * 65535.0).round() as u16).collect();
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
    crate::outfile::write_whole(path, |file| {
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
    })
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
    let description = with_even_nul(description);
    {
        let d = img.encoder();
        if let Some(icc) = tags.icc {
            d.write_tag(Tag::IccProfile, icc)?;
        }
        d.write_tag(Tag::ImageDescription, description.as_ref())?;
        if tags.software {
            let software = with_even_nul(concat!("mimizan ", env!("CARGO_PKG_VERSION")));
            d.write_tag(Tag::Software, software.as_ref())?;
        }
        if tags.orientation >= 1 && tags.orientation <= 8 {
            d.write_tag(Tag::Orientation, tags.orientation)?;
        }
        // Tag by tag: the crate's `resolution` unwraps its three writes.
        if let Some(dpi) = tags.dpi {
            let per_inch = || Rational { n: (dpi * 1000.0).round() as u32, d: 1000 };
            d.write_tag(Tag::ResolutionUnit, ResolutionUnit::Inch.to_u16())?;
            d.write_tag(Tag::XResolution, per_inch())?;
            d.write_tag(Tag::YResolution, per_inch())?;
        }
    }
    // `write_data` finishes the directory when the strips are in, and a
    // pad byte has to be written before that.
    let mut at = 0usize;
    loop {
        let n = usize::try_from(img.next_strip_sample_count())
            .map_err(|_| crate::Error::Invalid("a strip does not fit in memory".into()))?;
        if n == 0 {
            break;
        }
        let Some(end) = at.checked_add(n).filter(|end| *end <= image.data.len()) else {
            return Err(crate::Error::Invalid("the picture has fewer samples than its size".into()));
        };
        img.write_strip(&image.data[at..end])?;
        at = end;
    }
    // The header and every value written before the strips have an even
    // length, so an odd number of sample bytes leaves the directory on an
    // odd offset. The zero byte is not a sample and not a tag.
    if !std::mem::size_of_val(image.data).is_multiple_of(2) {
        img.encoder().write_data(0u8)?;
    }
    img.finish()?;
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
        crate::outfile::write_whole(&big, |file| {
            encode::<_, TiffKindBig, colortype::Gray16>(
                TiffEncoder::new_big(file)?,
                &image,
                &ascii_description(DESCRIPTION),
                &meta.tags(&icc),
            )
        })
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
            let mut dec = tiff::decoder::Decoder::new(std::fs::File::open(p).unwrap()).unwrap();
            dec.get_tag_u8_vec(t).ok()
        };
        assert_eq!(tag(&big, Tag::IccProfile), Some(icc.clone()));
        assert_eq!(tag(&big, Tag::IccProfile), tag(&classic, Tag::IccProfile));
    }

    /// TIFF wants a value and a directory on a word boundary. The encoder
    /// writes the next value straight after this one and the NUL it adds,
    /// so a description of even length takes one trailing space and one
    /// that is already even with the NUL does not. An 8-bit strip with an
    /// odd number of bytes is followed by one zero byte, which is not a
    /// sample. The description reads back as the same JSON.
    #[test]
    fn a_value_and_the_directory_start_on_a_word_boundary() {
        // Even length: the NUL makes it odd, so the file gains one space.
        // Odd length: even once the NUL is counted, so no space.
        let even = r#"{"k":"vv"}"#;
        let odd = r#"{"k":"v"}"#;
        assert_eq!(even.len() % 2, 0);
        assert!((odd.len() + 1).is_multiple_of(2));

        let meta = |description| TiffMeta {
            description,
            trc: GrayTrc::Gamma(2.2),
            orientation: 1,
            dpi: Some(300.0),
        };
        let grey = tmp("even-grey16.tif");
        write_gray16(&grey, &Plane::zeros(4, 2), &meta(even)).unwrap();
        // 4 × 2 samples, two bytes each: an even strip.
        on_a_word_boundary(&grey, 4 * 2 * 2);
        let raw = ascii_tag(&grey, Tag::ImageDescription);
        assert_eq!(raw, format!("{even} "));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&raw).unwrap(), serde_json::json!({"k": "vv"}));
        let software = concat!("mimizan ", env!("CARGO_PKG_VERSION"));
        assert!((software.len() + 1).is_multiple_of(2), "{software}");
        assert_eq!(ascii_tag(&grey, Tag::Software), software);

        let mask = tmp("odd-grey8.tif");
        write_gray8(&mask, 3, 1, &[1, 2, 3], even).unwrap();
        on_a_word_boundary(&mask, 3);
        let raw = ascii_tag(&mask, Tag::ImageDescription);
        assert_eq!(raw, format!("{even} "));
        assert_eq!(serde_json::from_str::<serde_json::Value>(&raw).unwrap(), serde_json::json!({"k": "vv"}));

        let plain = tmp("odd-text.tif");
        write_gray16(&plain, &Plane::zeros(2, 2), &meta(odd)).unwrap();
        on_a_word_boundary(&plain, 2 * 2 * 2);
        assert_eq!(ascii_tag(&plain, Tag::ImageDescription), odd);
        assert_eq!(ascii_tag(&plain, Tag::Software), software);
    }

    fn ascii_tag(path: &Path, tag: Tag) -> String {
        let mut dec = tiff::decoder::Decoder::new(std::fs::File::open(path).unwrap()).unwrap();
        dec.get_tag_ascii_string(tag).unwrap()
    }

    /// Every out-of-line value, the first strip and the directory start on
    /// an even offset. One strip, so the directory follows the samples
    /// (and one zero byte when that length is odd). The strip's own count
    /// stays the samples.
    fn on_a_word_boundary(path: &Path, sample_bytes: usize) {
        let bytes = std::fs::read(path).unwrap();
        assert_eq!(&bytes[0..2], if cfg!(target_endian = "little") { b"II" } else { b"MM" });
        let u16_at = |o: usize| u16::from_ne_bytes([bytes[o], bytes[o + 1]]);
        let u32_at = |o: usize| u32::from_ne_bytes([bytes[o], bytes[o + 1], bytes[o + 2], bytes[o + 3]]);
        assert_eq!(u16_at(2), 42, "classic TIFF");
        let ifd = u32_at(4) as usize;
        assert_eq!(ifd % 2, 0, "directory at {ifd}");
        let n = u16_at(ifd) as usize;
        let entries: Vec<usize> = (0..n).map(|i| ifd + 2 + 12 * i).collect();
        let mut out_of_line = 0;
        for &e in &entries {
            let size = match u16_at(e + 2) {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 => 4,
                5 | 10 | 12 => 8,
                t => panic!("tag {} has type {t}", u16_at(e)),
            };
            if size * u32_at(e + 4) as usize > 4 {
                let offset = u32_at(e + 8);
                assert_eq!(offset % 2, 0, "tag {} at {offset}", u16_at(e));
                out_of_line += 1;
            }
        }
        assert!(out_of_line >= 1, "{out_of_line} values out of line");

        let strips = entries.iter().copied().find(|&e| u16_at(e) == Tag::StripOffsets.to_u16()).unwrap();
        assert_eq!(u32_at(strips + 4), 1, "one strip, so the directory follows the samples");
        let strip = u32_at(strips + 8) as usize;
        assert_eq!(strip % 2, 0, "first strip at {strip}");
        let counts = entries.iter().copied().find(|&e| u16_at(e) == Tag::StripByteCounts.to_u16()).unwrap();
        assert_eq!(u32_at(counts + 8) as usize, sample_bytes, "the pad byte is not a sample");
        if sample_bytes % 2 == 1 {
            assert_eq!(ifd, strip + sample_bytes + 1, "one zero byte after the odd strip");
            assert_eq!(bytes[strip + sample_bytes], 0);
        } else {
            assert_eq!(ifd, strip + sample_bytes, "no pad byte after an even strip");
        }
    }
}
