//! Read back our own grey TIFFs (negatives, masks).

use crate::error::{Error, Result};
use crate::plane::Plane;
use rayon::prelude::*;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use tiff::decoder::{Decoder, DecodingResult, Limits};
use tiff::tags::Tag;

/// Most bytes of samples one of our files can hold: a print of
/// [`SHEET_MAX_PX`](crate::recipe::SHEET_MAX_PX) samples with three 16-bit
/// channels, six gigabytes. A three-colour plate of a 47-megapixel camera
/// is 280 MB, over the decoder's 256 MiB default, so the default would
/// refuse our own files; a file that claims more than this is not ours,
/// and is refused with an error instead of an allocation the machine
/// cannot make.
const SAMPLE_BYTES_MAX: usize = crate::recipe::SHEET_MAX_PX.saturating_mul(3 * 2);

/// Open with limits sized for the largest file the program writes. The
/// IFD limit stays at the decoder's default: our descriptions are a few
/// kilobytes of JSON.
fn open(path: &Path) -> Result<Decoder<BufReader<File>>> {
    let f = BufReader::new(File::open(path)?);
    let mut limits = Limits::default();
    limits.decoding_buffer_size = SAMPLE_BYTES_MAX;
    limits.intermediate_buffer_size = SAMPLE_BYTES_MAX;
    Ok(Decoder::new(f)?.with_limits(limits))
}

/// A grey TIFF as read: pixels, description, and the two tags the print
/// pass needs.
pub struct GrayTiff {
    /// Samples scaled to [0, 1] by the file's bit depth ([`read_gray`]),
    /// or the scene values they stand for ([`read_negative`]).
    pub plane: Plane,
    /// `ImageDescription`, when present.
    pub description: Option<String>,
    /// 8 or 16.
    pub bits: u16,
    /// TIFF orientation 1..=8, 1 when absent.
    pub orientation: u16,
}

/// The file's `ImageDescription`, or `None` when that tag is absent.
/// A tag that is present and cannot be read is an error of this read.
/// Taken for absent, a plate prints as one grey picture three frames
/// tall, and a range the description stated is read as 1.
fn read_description(dec: &mut Decoder<BufReader<File>>) -> Result<Option<String>> {
    match dec.get_tag_ascii_string(Tag::ImageDescription) {
        Ok(text) => Ok(Some(text)),
        Err(tiff::TiffError::FormatError(tiff::TiffFormatError::RequiredTagNotFound(_))) => Ok(None),
        Err(err) => Err(Error::Invalid(format!("the description could not be read: {err}"))),
    }
}

/// 8- or 16-bit single-channel TIFF to a Plane in [0,1]. A description
/// that is absent is `None`. One that is present and cannot be read is
/// an error of this read.
pub fn read_gray(path: &Path) -> Result<GrayTiff> {
    let mut dec = open(path)?;
    let (w, h) = dec.dimensions()?;
    let description = read_description(&mut dec)?;
    let orientation =
        dec.get_tag_u32(Tag::Orientation).ok().filter(|o| (1..=8).contains(o)).unwrap_or(1) as u16;
    let img = dec.read_image()?;
    let (data, bits): (Vec<f64>, u16) = match img {
        DecodingResult::U16(v) => (v.par_iter().map(|&x| f64::from(x) / 65535.0).collect(), 16),
        DecodingResult::U8(v) => (v.par_iter().map(|&x| f64::from(x) / 255.0).collect(), 8),
        _ => return Err(Error::Invalid("only 8/16-bit grey TIFF is readable".into())),
    };
    if data.len() != (w as usize).saturating_mul(h as usize) {
        return Err(Error::Invalid("TIFF is not single-channel".into()));
    }
    Ok(GrayTiff { plane: Plane::try_from_vec(w as usize, h as usize, data)?, description, bits, orientation })
}

/// The scene value a negative's full scale stands for, from its
/// description: the `range` it states, 1 when it states none (every
/// negative up to 0.18.15, and every one whose plane stays within 1) or
/// when there is no description to read. A range that is no number of 1
/// or more is not ours and is refused.
pub fn described_range(description: Option<&str>) -> Result<f64> {
    let Some(v) = description.and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok()) else {
        return Ok(1.0);
    };
    match v.get("range") {
        None => Ok(1.0),
        Some(r) => match r.as_f64() {
            Some(r) if r.is_finite() && r >= 1.0 => Ok(r),
            _ => Err(Error::Invalid(format!("the negative states a range of {r}; it is 1 or more"))),
        },
    }
}

/// A negative as the print takes it: [`read_gray`] with every sample
/// multiplied by the range the description states, so the plane holds
/// scene values, above 1 where the file kept them. A file with no range
/// reads as `read_gray` to the bit.
pub fn read_negative(path: &Path) -> Result<GrayTiff> {
    let mut g = read_gray(path)?;
    let range = described_range(g.description.as_deref())?;
    if range != 1.0 {
        g.plane.data.par_iter_mut().for_each(|v| *v *= range);
    }
    Ok(g)
}

/// 8- or 16-bit three-channel TIFF (a plate print) to `[r, g, b]` in [0,1].
pub fn read_rgb16(path: &Path) -> Result<[Plane; 3]> {
    let mut dec = open(path)?;
    let (w, h) = dec.dimensions()?;
    let (w, h) = (w as usize, h as usize);
    let data: Vec<f64> = match dec.read_image()? {
        DecodingResult::U16(v) => v.par_iter().map(|&x| f64::from(x) / 65535.0).collect(),
        DecodingResult::U8(v) => v.par_iter().map(|&x| f64::from(x) / 255.0).collect(),
        _ => return Err(Error::Invalid("only 8/16-bit RGB TIFF is readable".into())),
    };
    if data.len() != 3usize.saturating_mul(w).saturating_mul(h) {
        return Err(Error::Invalid("TIFF is not three-channel".into()));
    }
    let mut planes = [Plane::zeros(w, h), Plane::zeros(w, h), Plane::zeros(w, h)];
    for (i, px) in data.as_chunks::<3>().0.iter().enumerate() {
        planes[0].data[i] = px[0];
        planes[1].data[i] = px[1];
        planes[2].data[i] = px[2];
    }
    Ok(planes)
}

/// Only the ImageDescription, without decoding pixels. `None` when the
/// tag is absent. An error when the tag is present and cannot be read.
pub fn read_gray_header(path: &Path) -> Result<Option<String>> {
    let mut dec = open(path)?;
    read_description(&mut dec)
}

/// Mask sidecar as raw u8 values, `(width, height, samples)`, exactly one
/// sample per pixel.
pub fn read_mask8(path: &Path) -> Result<(usize, usize, Vec<u8>)> {
    let mut dec = open(path)?;
    let (w, h) = dec.dimensions()?;
    let (w, h) = (w as usize, h as usize);
    match dec.read_image()? {
        DecodingResult::U8(v) if v.len() == w.saturating_mul(h) => Ok((w, h, v)),
        _ => Err(Error::Invalid("mask sidecar must be 8-bit single-channel".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tiffout::write_gray8;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mimizan-tiffin-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// A one-pixel grey file with no `ImageDescription` tag.
    fn write_bare(path: &Path) {
        let file = std::fs::File::create(path).unwrap();
        let mut enc = tiff::encoder::TiffEncoder::new(file).unwrap();
        enc.write_image::<tiff::encoder::colortype::Gray8>(1, 1, &[0u8]).unwrap();
    }

    /// The encoder wrote this description as ASCII. Make that tag BYTE.
    fn description_type_to_byte(path: &Path) {
        let mut bytes = std::fs::read(path).unwrap();
        let at = image_description_type_byte(&bytes);
        assert_eq!(bytes[at], 2, "the encoder writes ASCII");
        bytes[at] = 1;
        std::fs::write(path, bytes).unwrap();
    }

    /// Index of the type byte that holds ASCII (2) on the description tag.
    fn image_description_type_byte(bytes: &[u8]) -> usize {
        assert!(bytes.len() > 8, "not a TIFF");
        let be = match &bytes[..2] {
            b"II" => false,
            b"MM" => true,
            other => panic!("not a TIFF: {other:?}"),
        };
        let u16_at = |i: usize| {
            let raw = [bytes[i], bytes[i + 1]];
            if be {
                u16::from_be_bytes(raw)
            } else {
                u16::from_le_bytes(raw)
            }
        };
        let u32_at = |i: usize| {
            let raw = [bytes[i], bytes[i + 1], bytes[i + 2], bytes[i + 3]];
            if be {
                u32::from_be_bytes(raw)
            } else {
                u32::from_le_bytes(raw)
            }
        };
        assert_eq!(u16_at(2), 42, "a one-pixel file is a classic TIFF");
        let mut at = u32_at(4) as usize;
        let count = usize::from(u16_at(at));
        at += 2;
        for _ in 0..count {
            if u16_at(at) == tiff::tags::Tag::ImageDescription.to_u16() {
                assert_eq!(u16_at(at + 2), tiff::tags::Type::ASCII.to_u16(), "the encoder writes text");
                return if be { at + 3 } else { at + 2 };
            }
            at += 12;
        }
        panic!("the file has no ImageDescription");
    }

    fn names_the_description<T>(result: Result<T>) {
        match result {
            Ok(_) => panic!("a description that is not text was read"),
            Err(err) => {
                let msg = err.to_string();
                assert!(msg.contains("the description could not be read"), "{msg}");
            }
        }
    }

    /// A file with no description still reads, and the description is
    /// absent. Text that is not JSON still reads, and its range is 1.
    /// The same text with the tag's type changed from ASCII to BYTE does
    /// not: the tag is there and is not text.
    #[test]
    fn a_missing_description_reads_and_one_that_is_not_text_does_not() {
        let bare = tmp("bare.tif");
        write_bare(&bare);
        let read = read_gray(&bare).unwrap();
        assert_eq!((read.plane.width, read.plane.height, read.bits), (1, 1, 8));
        assert_eq!(read.plane.data, vec![0.0]);
        assert_eq!(read.description, None);
        assert_eq!(read_gray_header(&bare).unwrap(), None);
        assert_eq!(read_negative(&bare).unwrap().description, None);
        assert_eq!(described_range(None).unwrap(), 1.0);

        let text = tmp("text.tif");
        write_gray8(&text, 1, 1, &[0], "not json").unwrap();
        let read = read_gray(&text).unwrap();
        assert_eq!(read.description.as_deref(), Some("not json"));
        assert_eq!(described_range(read.description.as_deref()).unwrap(), 1.0);
        assert_eq!(read_gray_header(&text).unwrap().as_deref(), Some("not json"));
        assert_eq!(read_negative(&text).unwrap().description.as_deref(), Some("not json"));

        let bad = tmp("byte.tif");
        write_gray8(&bad, 1, 1, &[0], "not json").unwrap();
        description_type_to_byte(&bad);
        names_the_description(read_gray(&bad));
        names_the_description(read_gray_header(&bad));
        names_the_description(read_negative(&bad));
    }
}
