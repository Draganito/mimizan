//! The linear export: the separation as another program reads it.
//!
//! The print is Mimizan's darkroom. This module is the way out of it for
//! a photographer who wants the separation, no demosaicing, but their own
//! converter on top: no paper, no lamps, no border.
//!
//! * The monochrome negative is the linear 16-bit grey TIFF the command
//!   writes, upright and cut to the crop, with its mask sidecar beside
//!   it. `mimizan print` prints it; any editor opens it as linear grey.
//! * The plate goes out as a **linear DNG**: the three balanced channels
//!   of the separation with the white balance undone, three samples per
//!   pixel, `PhotometricInterpretation = LinearRaw`, the camera's colour
//!   matrix and its as-shot neutral in the tags. A RAW converter opens it
//!   as a RAW of that camera and skips its own demosaicing, because the
//!   data already has three channels; white balance, profile, tone and
//!   everything else are the converter's. Every sample is still a
//!   weighted sum of measured photosites, and the colour is band-limited
//!   to the carriers, under a full-resolution luminance.
//!
//! Orientation and crop are applied to the pixels: a turn is a
//! permutation, a crop a window, neither resamples. The files carry
//! orientation 1.

use crate::decode::RawFrame;
use crate::error::{Error, Result};
use crate::icc::GrayTrc;
use crate::plane::Plane;
use crate::print::{orient, window_plane};
use crate::recipe::CropRect;
use crate::tiffout::{write_gray16, write_gray8, TiffMeta};
use rayon::prelude::*;
use std::borrow::Cow;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use tiff::encoder::colortype::RGB16;
use tiff::encoder::{Rational, SRational, TiffEncoder, TiffValue};
use tiff::tags::{Tag, Type};

/// `PhotometricInterpretation` of a DNG whose samples are already colour
/// channels (DNG 1.1, "LinearRaw"). The `tiff` encoder has no such
/// colour type; the image is laid out as `RGB16` (three 16-bit samples a
/// pixel, chunky) and the tag is replaced, which the directory allows
/// because it keeps one entry per tag.
pub const LINEAR_RAW: u16 = 34892;

/// A run of `SRATIONAL`s (the encoder writes one at a time).
struct SRationals(Vec<SRational>);

impl TiffValue for SRationals {
    const BYTE_LEN: u8 = 8;
    const FIELD_TYPE: Type = Type::SRATIONAL;

    fn count(&self) -> usize {
        self.0.len()
    }

    fn data(&self) -> Cow<'_, [u8]> {
        let mut out = Vec::with_capacity(8 * self.0.len());
        for r in &self.0 {
            out.extend_from_slice(&r.n.to_ne_bytes());
            out.extend_from_slice(&r.d.to_ne_bytes());
        }
        Cow::Owned(out)
    }
}

/// A run of `RATIONAL`s.
struct Rationals(Vec<Rational>);

impl TiffValue for Rationals {
    const BYTE_LEN: u8 = 8;
    const FIELD_TYPE: Type = Type::RATIONAL;

    fn count(&self) -> usize {
        self.0.len()
    }

    fn data(&self) -> Cow<'_, [u8]> {
        let mut out = Vec::with_capacity(8 * self.0.len());
        for r in &self.0 {
            out.extend_from_slice(&r.n.to_ne_bytes());
            out.extend_from_slice(&r.d.to_ne_bytes());
        }
        Cow::Owned(out)
    }
}

/// Denominator of the matrix and neutral rationals: six decimals, more
/// than any camera table states.
const DENOM: i32 = 1_000_000;

fn srational(v: f64) -> SRational {
    SRational { n: (v * f64::from(DENOM)).round() as i32, d: DENOM }
}

fn rational(v: f64) -> Rational {
    Rational { n: (v.max(0.0) * f64::from(DENOM)).round() as u32, d: DENOM as u32 }
}

/// TIFF asks every value to start on a word boundary; the encoder writes
/// them back to back. An ASCII value of even length (with its NUL) keeps
/// the next one aligned, so odd strings get one trailing space.
fn even(s: &str) -> Cow<'_, str> {
    if (s.len() + 1) % 2 == 0 {
        Cow::Borrowed(s)
    } else {
        Cow::Owned(format!("{s} "))
    }
}

/// What the DNG says about the camera and the balance.
pub struct DngMeta<'a> {
    pub make: &'a str,
    pub model: &'a str,
    /// XYZ → camera, rows R, G, B, as the RAW file states it.
    pub xyz_to_cam: [[f64; 3]; 3],
    /// EXIF `LightSource` of that matrix ([`RawFrame::illuminant`]);
    /// `None` writes "unknown" (0), which the converters take as D65.
    pub illuminant: Option<u16>,
    /// Balance multipliers R, G, B the planes carry (G = 1).
    pub wb: [f64; 3],
    /// `ImageDescription`: the negative's JSON.
    pub description: &'a str,
}

impl<'a> DngMeta<'a> {
    /// The camera of `frame`, the balance of the development. `None` when
    /// the file carries no colour matrix: a DNG without one is no RAW.
    pub fn of(frame: &'a RawFrame, wb: [f64; 3], description: &'a str) -> Option<Self> {
        Some(Self {
            make: &frame.make,
            model: &frame.model,
            xyz_to_cam: frame.xyz_to_cam?,
            illuminant: frame.illuminant,
            wb,
            description,
        })
    }
}

/// Dimensions of the picture a writer put in the file.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Written {
    pub width: usize,
    pub height: usize,
}

/// Upright, then the window.
fn framed(p: &Plane, orientation: u16, crop: Option<CropRect>) -> Plane {
    let up = orient(p, orientation);
    match crop {
        Some(c) => window_plane(&up, c.sanitized(up.width as u32, up.height as u32)),
        None => up,
    }
}

/// The monochrome negative as the command writes it: linear 16-bit grey,
/// upright and cut to `crop`, the JSON in the description, orientation 1;
/// beside it `<stem>.mask.tif`, the sharpening mask framed the same way
/// (`mask` is `max(M)` with saturated pixels at 1, the sidecar as a
/// plane). Returns the size written.
pub fn write_negative(
    path: &Path,
    plane: &Plane,
    mask: Option<&Plane>,
    orientation: u16,
    crop: Option<CropRect>,
    description: &str,
) -> Result<Written> {
    let out = framed(plane, orientation, crop);
    write_gray16(path, &out, &TiffMeta { description, trc: GrayTrc::Linear, orientation: 1, dpi: None })?;
    if let Some(m) = mask {
        if (m.width, m.height) != (plane.width, plane.height) {
            return Err(Error::Invalid("the mask and the negative differ in size".into()));
        }
        let fm = framed(m, orientation, crop);
        let bytes: Vec<u8> = fm.data.par_iter().map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
        write_gray8(
            &mask_sidecar(path),
            fm.width,
            fm.height,
            &bytes,
            "mimizan mask: 255*max(M), 255=saturated",
        )?;
    }
    Ok(Written { width: out.width, height: out.height })
}

/// `<stem>.mask.tif` next to `out`.
pub fn mask_sidecar(out: &Path) -> std::path::PathBuf {
    let stem = out.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    out.with_file_name(format!("{stem}.mask.tif"))
}

/// The plate as a linear DNG. `balanced` are the three channel planes of
/// the separation by output channel R, G, B (`PlateWeights::channels()`:
/// each `L̂` plus its band-limited chrominance, on the balanced channels).
/// Each is divided by its multiplier, so a neutral reads the camera's
/// as-shot neutral again and every channel clips at the sensor's white;
/// then turned upright, cut to `crop`, and quantised to 16 bits.
///
/// Tags: DNG 1.4 (backward 1.1), `LinearRaw`, `ColorMatrix1` the camera's
/// matrix, `CalibrationIlluminant1` its illuminant, `AsShotNeutral` the
/// inverse multipliers, `WhiteLevel` 65535, black 0, the JSON in the
/// description. One IFD, uncompressed, orientation 1.
pub fn write_dng(
    path: &Path,
    balanced: [&Plane; 3],
    orientation: u16,
    crop: Option<CropRect>,
    meta: &DngMeta<'_>,
) -> Result<Written> {
    let [r, g, b] = balanced;
    if (g.width, g.height) != (r.width, r.height) || (b.width, b.height) != (r.width, r.height) {
        return Err(Error::Invalid("the three channel planes differ in size".into()));
    }
    if meta.wb.iter().any(|k| !k.is_finite() || *k <= 0.0) {
        return Err(Error::Invalid(format!("balance multipliers {:?} are not positive", meta.wb)));
    }
    if meta.xyz_to_cam.iter().flatten().any(|v| !v.is_finite()) {
        return Err(Error::Invalid("the colour matrix is not finite".into()));
    }

    // One channel at a time beside the interleaved output: unbalance,
    // frame, quantise into its lane.
    let mut data: Vec<u16> = Vec::new();
    let (mut width, mut height) = (0usize, 0usize);
    for (c, plane) in balanced.iter().enumerate() {
        let f = framed(plane, orientation, crop);
        if c == 0 {
            width = f.width;
            height = f.height;
            data = vec![0u16; 3 * width * height];
        }
        let k = meta.wb[c];
        data.par_chunks_mut(3 * width).zip(f.data.par_chunks(width)).for_each(|(row, src)| {
            for (x, &v) in src.iter().enumerate() {
                row[3 * x + c] = ((v / k).clamp(0.0, 1.0) * 65535.0).round() as u16;
            }
        });
    }

    let file = BufWriter::new(File::create(path)?);
    let mut enc = TiffEncoder::new(file)?;
    let mut img = enc.new_image::<RGB16>(width as u32, height as u32)?;
    img.rows_per_strip(64)?;
    {
        let d = img.encoder();
        d.write_tag(Tag::PhotometricInterpretation, LINEAR_RAW)?;
        d.write_tag(Tag::NewSubfileType, 0u32)?;
        d.write_tag(Tag::PlanarConfiguration, 1u16)?;
        d.write_tag(Tag::Orientation, 1u16)?;
        d.write_tag(Tag::Make, &*even(meta.make))?;
        d.write_tag(Tag::Model, &*even(meta.model))?;
        d.write_tag(Tag::ImageDescription, &*even(meta.description))?;
        d.write_tag(Tag::Software, &*even(concat!("mimizan ", env!("CARGO_PKG_VERSION"))))?;
        // DNGVersion, DNGBackwardVersion.
        d.write_tag(Tag::Unknown(50706), &[1u8, 4, 0, 0][..])?;
        d.write_tag(Tag::Unknown(50707), &[1u8, 1, 0, 0][..])?;
        // UniqueCameraModel.
        let unique = format!("{} {}", meta.make.trim(), meta.model.trim());
        d.write_tag(Tag::Unknown(50708), &*even(unique.trim()))?;
        // BlackLevel, WhiteLevel: one per sample.
        d.write_tag(Tag::Unknown(50714), &[0u32, 0, 0][..])?;
        d.write_tag(Tag::Unknown(50717), &[65535u32, 65535, 65535][..])?;
        // ColorMatrix1, CalibrationIlluminant1.
        let m: Vec<SRational> = meta.xyz_to_cam.iter().flatten().map(|&v| srational(v)).collect();
        d.write_tag(Tag::Unknown(50721), SRationals(m))?;
        d.write_tag(Tag::Unknown(50778), meta.illuminant.unwrap_or(0))?;
        // AsShotNeutral: the raw value of a neutral, G = 1.
        let n: Vec<Rational> = meta.wb.iter().map(|&k| rational(1.0 / k)).collect();
        d.write_tag(Tag::Unknown(50728), Rationals(n))?;
    }
    img.write_data(&data)?;
    Ok(Written { width, height })
}

#[cfg(test)]
mod tests {
    use super::*;
    use tiff::decoder::{Decoder, DecodingResult};
    use tiff::tags::Tag;

    fn tmp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("mimizan-linear-{}-{}", std::process::id(), name));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    fn ramp(w: usize, h: usize, scale: f64) -> Plane {
        let mut p = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                p.set(x, y, scale * (x + w * y) as f64 / (w * h) as f64);
            }
        }
        p
    }

    /// The DNG holds the channels divided by their multipliers, three
    /// samples a pixel, with the matrix, the neutral and the levels a
    /// converter reads, and the frame turned and cut as asked.
    #[test]
    fn dng_round_trips_the_unbalanced_channels_and_the_tags() {
        let (w, h) = (8usize, 6usize);
        let wb = [2.0, 1.0, 1.5];
        let r = ramp(w, h, 2.0);
        let g = ramp(w, h, 1.0);
        let b = ramp(w, h, 1.5);
        let m = [[0.7, 0.2, 0.1], [-0.3, 1.1, 0.2], [0.05, -0.1, 0.9]];
        let meta = DngMeta {
            make: "Mimizan",
            model: "Test",
            xyz_to_cam: m,
            illuminant: Some(21),
            wb,
            description: "{}",
        };
        let path = tmp("plate.dng");
        // Orientation 6 (90° cw) and a crop in the upright frame.
        let crop = Some(CropRect { x: 1, y: 2, w: 4, h: 3 });
        let got = write_dng(&path, [&r, &g, &b], 6, crop, &meta).unwrap();
        assert_eq!(got, Written { width: 4, height: 3 });

        // The `tiff` decoder refuses LinearRaw; the first IFD is read by
        // hand for that one tag, which is then set to RGB so the rest of
        // the file can be checked with the decoder.
        let mut bytes = std::fs::read(&path).unwrap();
        assert_eq!(&bytes[0..2], if cfg!(target_endian = "little") { b"II" } else { b"MM" });
        let u16_at = |b: &[u8], o: usize| u16::from_ne_bytes([b[o], b[o + 1]]);
        let u32_at = |b: &[u8], o: usize| u32::from_ne_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        let ifd = u32_at(&bytes, 4) as usize;
        let n = u16_at(&bytes, ifd) as usize;
        let photometric = (0..n)
            .map(|i| ifd + 2 + 12 * i)
            .find(|&e| u16_at(&bytes, e) == 262)
            .expect("PhotometricInterpretation present");
        assert_eq!(u16_at(&bytes, photometric + 2), 3, "SHORT");
        assert_eq!(u16_at(&bytes, photometric + 8), LINEAR_RAW);
        bytes[photometric + 8..photometric + 10].copy_from_slice(&2u16.to_ne_bytes());

        let mut dec = Decoder::new(std::io::Cursor::new(bytes)).unwrap();
        assert_eq!(dec.dimensions().unwrap(), (4, 3));
        assert_eq!(dec.get_tag_u32(Tag::SamplesPerPixel).unwrap(), 3);
        assert_eq!(dec.get_tag_u32(Tag::Orientation).unwrap(), 1);
        assert_eq!(dec.get_tag_u32_vec(Tag::Unknown(50706)).unwrap(), vec![1, 4, 0, 0]);
        assert_eq!(dec.get_tag_u32_vec(Tag::Unknown(50717)).unwrap(), vec![65535; 3]);
        assert_eq!(dec.get_tag_u32(Tag::Unknown(50778)).unwrap(), 21);
        // The decoder hands rationals back as pairs, not as f64.
        let rationals = |v: tiff::decoder::ifd::Value| -> Vec<f64> {
            use tiff::decoder::ifd::Value::*;
            let one = |v: &tiff::decoder::ifd::Value| match v {
                Rational(n, d) => f64::from(*n) / f64::from(*d),
                SRational(n, d) => f64::from(*n) / f64::from(*d),
                other => panic!("not a rational: {other:?}"),
            };
            match v {
                List(vs) => vs.iter().map(one).collect(),
                v => vec![one(&v)],
            }
        };
        let cm = rationals(dec.get_tag(Tag::Unknown(50721)).unwrap());
        assert_eq!(cm.len(), 9);
        for (a, b) in cm.iter().zip(m.iter().flatten()) {
            assert!((a - b).abs() < 1e-6, "{cm:?}");
        }
        let neutral = rationals(dec.get_tag(Tag::Unknown(50728)).unwrap());
        assert_eq!(neutral.len(), 3);
        assert!((neutral[0] - 0.5).abs() < 1e-6 && (neutral[1] - 1.0).abs() < 1e-6);
        assert!((neutral[2] - 1.0 / 1.5).abs() < 1e-6, "{neutral:?}");
        // Trailing space pads an odd string to a word boundary; readers trim it.
        assert_eq!(dec.get_tag_ascii_string(Tag::Unknown(50708)).unwrap().trim_end(), "Mimizan Test");

        let DecodingResult::U16(px) = dec.read_image().unwrap() else { panic!("not u16") };
        assert_eq!(px.len(), 3 * 4 * 3);
        // The same frame, computed here: the three ramps are 2:1:1.5 of one
        // picture, so unbalanced they are equal, and equal to the green.
        let want = framed(&g, 6, crop);
        for (i, p) in px.chunks(3).enumerate() {
            let q = (want.data[i].clamp(0.0, 1.0) * 65535.0).round() as u16;
            assert_eq!(p, &[q, q, q], "pixel {i}");
        }
    }

    /// No matrix, no DNG; a bad multiplier is refused; mismatched planes
    /// are refused.
    #[test]
    fn dng_refuses_what_it_cannot_describe() {
        let p = ramp(4, 4, 1.0);
        let m = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let bad = DngMeta {
            make: "",
            model: "",
            xyz_to_cam: m,
            illuminant: None,
            wb: [0.0, 1.0, 1.0],
            description: "",
        };
        assert!(write_dng(&tmp("bad.dng"), [&p, &p, &p], 1, None, &bad).is_err());
        let ok = DngMeta { wb: [1.0; 3], ..bad };
        let q = ramp(4, 3, 1.0);
        assert!(write_dng(&tmp("bad2.dng"), [&p, &p, &q], 1, None, &ok).is_err());
        let frame_without_matrix = crate::decode::RawFrame {
            make: String::new(),
            model: String::new(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: 4,
            height: 4,
            bits: 12,
            kind: crate::decode::SensorKind::Mono,
            black_tile: [0.0; 4],
            white_rgb: [1.0; 3],
            crop: crate::decode::Rect { x: 0, y: 0, w: 4, h: 4 },
            orientation: 1,
            xyz_to_cam: None,
            illuminant: None,
            wb_as_shot: None,
            exposure: Default::default(),
            data: crate::decode::RawData::Integer(vec![0; 16]),
        };
        assert!(DngMeta::of(&frame_without_matrix, [1.0; 3], "").is_none());
    }

    /// The negative goes out upright and cut, linear, with its mask framed
    /// the same way beside it.
    #[test]
    fn negative_is_framed_with_its_mask() {
        let (w, h) = (8usize, 6usize);
        let plane = ramp(w, h, 1.0);
        let mask = Plane::from_vec(w, h, (0..w * h).map(|i| if i % 2 == 0 { 1.0 } else { 0.5 }).collect());
        let path = tmp("neg.tif");
        let crop = Some(CropRect { x: 2, y: 1, w: 3, h: 4 });
        let got = write_negative(&path, &plane, Some(&mask), 8, crop, r#"{"stage":"negative"}"#).unwrap();
        assert_eq!(got, Written { width: 3, height: 4 });
        let read = crate::tiffin::read_gray(&path).unwrap();
        assert_eq!((read.plane.width, read.plane.height, read.orientation), (3, 4, 1));
        let want = framed(&plane, 8, crop);
        for (a, b) in read.plane.data.iter().zip(&want.data) {
            assert!((a - b).abs() < 1.0 / 65535.0, "{a} vs {b}");
        }
        let (mw, mh, bytes) = crate::tiffin::read_mask8(&mask_sidecar(&path)).unwrap();
        assert_eq!((mw, mh), (3, 4));
        let wm = framed(&mask, 8, crop);
        for (a, b) in bytes.iter().zip(&wm.data) {
            assert_eq!(*a, (b * 255.0).round() as u8);
        }
    }
}
