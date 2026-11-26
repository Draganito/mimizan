//! The raw frame the pipeline starts from, and RAW files decoded into it.
//! The only file backend is `rawler` (feature `decode-rawler`); callers
//! may also fill a `RawFrame` directly.
//!
//! A frame is a mosaic, one sample per photosite, or a linear DNG's three
//! channels per pixel ([`SensorKind::Rgb`]): a picture another program
//! has already taken from its mosaic, which is mixed and printed here and
//! not separated again.

use crate::cfa::BayerPhase;
#[cfg(feature = "decode-rawler")]
use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
#[cfg(feature = "decode-rawler")]
use std::path::Path;

/// A rectangle of sensor pixels: origin and size.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// What sits in front of the photosites.
#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum SensorKind {
    /// A 2×2 Bayer filter with this phase at pixel (0, 0).
    Bayer(BayerPhase),
    /// No colour filter: every photosite is the measurement.
    Mono,
    /// No mosaic: three samples per pixel, R, G, B in the camera's own
    /// colours, as a linear DNG holds them. How they came out of the
    /// camera's mosaic is the writing program's affair and cannot be read
    /// from the file.
    Rgb,
}

impl SensorKind {
    /// Samples the frame holds for each pixel: 3 for [`SensorKind::Rgb`],
    /// 1 for a mosaic.
    pub fn samples(self) -> usize {
        match self {
            SensorKind::Bayer(_) | SensorKind::Mono => 1,
            SensorKind::Rgb => 3,
        }
    }
}

/// The sensor samples as the file stores them, row-major: one per
/// photosite, or R, G, B one after the other for each pixel of a
/// three-channel frame.
#[derive(Clone, Debug)]
pub enum RawData {
    Integer(Vec<u16>),
    Float(Vec<f32>),
}

impl RawData {
    /// Sample `i` as `f64`.
    #[inline]
    pub fn get(&self, i: usize) -> f64 {
        match self {
            RawData::Integer(v) => f64::from(v[i]),
            RawData::Float(v) => f64::from(v[i]),
        }
    }
    /// Number of samples.
    pub fn len(&self) -> usize {
        match self {
            RawData::Integer(v) => v.len(),
            RawData::Float(v) => v.len(),
        }
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Exposure data from EXIF, informational only (nothing in the pipeline
/// depends on it).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct Exposure {
    pub iso: Option<u32>,
    /// Exposure time as the file's rational (numerator, denominator).
    pub time: Option<(u32, u32)>,
    pub fnumber: Option<f64>,
    pub focal_mm: Option<f64>,
    pub lens: Option<String>,
    /// Capture time as the file writes it (EXIF `DateTimeOriginal`,
    /// `YYYY:MM:DD HH:MM:SS`), if present.
    pub captured: Option<String>,
}

impl Exposure {
    /// `ISO 100 · 1/125 s · f/5.6 · 50 mm`, only the parts that exist.
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        if let Some(i) = self.iso {
            parts.push(format!("ISO {i}"));
        }
        if let Some((n, d)) = self.time {
            if n > 0 && d > 0 {
                let s = n as f64 / d as f64;
                parts.push(if s >= 0.5 {
                    format!("{} s", trim_float(s))
                } else {
                    format!("1/{} s", trim_float(d as f64 / n as f64))
                });
            }
        }
        if let Some(f) = self.fnumber {
            parts.push(format!("f/{}", trim_float(f)));
        }
        if let Some(f) = self.focal_mm {
            parts.push(format!("{} mm", trim_float(f)));
        }
        parts.join(" · ")
    }
}

fn trim_float(v: f64) -> String {
    let s = format!("{v:.1}");
    s.strip_suffix(".0").map(str::to_string).unwrap_or(s)
}

/// Everything the pipeline needs from a RAW file. Levels are in raw DN.
#[derive(Clone, Debug)]
pub struct RawFrame {
    pub make: String,
    pub model: String,
    pub clean_make: String,
    pub clean_model: String,
    pub width: usize,
    pub height: usize,
    pub bits: usize,
    pub kind: SensorKind,
    /// Black level per 2x2 tile position, row-major: (0,0),(1,0),(0,1),(1,1).
    /// Of a three-channel frame: the black level of R, G, B, and G's again.
    pub black_tile: [f64; 4],
    /// White level per colour: R, G, B (index by `Color`), G used twice.
    pub white_rgb: [f64; 3],
    pub crop: Rect,
    /// TIFF orientation value (1..=8), 0 if unknown.
    pub orientation: u16,
    /// XYZ -> camera matrix (rows R,G,B), if the file provides one.
    pub xyz_to_cam: Option<[[f64; 3]; 3]>,
    /// The illuminant `xyz_to_cam` was measured under, as the EXIF
    /// `LightSource` code the DNG `CalibrationIlluminant1` tag takes:
    /// 21 D65, 20 D55, 23 D50, 22 D75, 1 daylight, 9 fine weather, 10
    /// cloudy. `None` when the file does not say.
    pub illuminant: Option<u16>,
    /// As-shot white balance multipliers R,G,B normalised to G = 1, if present.
    pub wb_as_shot: Option<[f64; 3]>,
    pub exposure: Exposure,
    pub data: RawData,
}

/// The `rawler` decoder: every file format rawler reads. The desktop and
/// the command read their frames through it; a mobile build constructs
/// them from the camera API instead.
#[cfg(feature = "decode-rawler")]
pub struct RawlerDecoder;

#[cfg(feature = "decode-rawler")]
pub fn exposure_from(md: &rawler::decoders::RawMetadata) -> Exposure {
    let e = &md.exif;
    let rat = |r: &Option<rawler::formats::tiff::Rational>| {
        r.as_ref().filter(|r| r.d != 0).map(|r| f64::from(r.n) / f64::from(r.d))
    };
    Exposure {
        iso: e.iso_speed_ratings.map(u32::from).or(e.iso_speed),
        time: e.exposure_time.as_ref().map(|r| (r.n, r.d)),
        fnumber: rat(&e.fnumber),
        focal_mm: rat(&e.focal_length),
        lens: md.lens.as_ref().map(|l| l.lens_model.clone()).or_else(|| e.lens_model.clone()),
        captured: e.date_time_original.clone().or_else(|| e.create_date.clone()),
    }
}

/// The file at `path`, read whole, as rawler takes it.
///
/// rawler's own `RawSource::new` maps the file into memory. A card that is
/// pulled or a file another program cuts short while a decoder reads the
/// map ends the whole process with a bus error, which nothing can catch. A
/// read fails with an error instead; the cost is one buffer of the file's
/// size for the length of the decode.
#[cfg(feature = "decode-rawler")]
pub fn raw_source(path: &Path) -> std::io::Result<rawler::rawsource::RawSource> {
    let bytes = std::fs::read(path)?;
    Ok(rawler::rawsource::RawSource::new_from_shared_vec(std::sync::Arc::new(bytes)).with_path(path))
}

/// The orientation the darkroom keeps. Rawler's own value stands, and
/// the EXIF one is used when rawler reports the frame unturned (`Normal`
/// or `Unknown`). `exif` outside 1..=8 is ignored.
#[cfg(feature = "decode-rawler")]
pub fn settled_orientation(own: rawler::Orientation, exif: Option<u16>) -> u16 {
    use rawler::Orientation as O;
    let exif = exif.filter(|o| (1..=8).contains(o));
    match own {
        O::Normal | O::Unknown => exif.unwrap_or(if own == O::Normal { 1 } else { 0 }),
        other => other.to_u16(),
    }
}

/// `work`, with a panic inside it turned into an error.
///
/// rawler's decoders stop on some file contents with a panic (`todo!()`
/// for a layout they do not know, an index past the end of a damaged
/// file). rawler catches those around its own entry point; the calls made
/// here go past it, so they are caught here: a file is never a reason for
/// the program to end.
#[cfg(feature = "decode-rawler")]
pub fn guarded<T>(work: impl FnOnce() -> Result<T>) -> Result<T> {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)) {
        Ok(result) => result,
        Err(payload) => {
            let said = payload
                .downcast_ref::<&str>()
                .map(|s| (*s).to_owned())
                .or_else(|| payload.downcast_ref::<String>().cloned())
                .unwrap_or_else(|| "no message".to_owned());
            Err(Error::Decode(format!("the decoder stopped on this file: {said}")))
        }
    }
}

/// A stated side past this, or a stated frame of more photosites than
/// [`STATED_PIXELS_MAX`], is refused before the pixels are read. The
/// allocation would otherwise abort the process.
#[cfg(feature = "decode-rawler")]
const STATED_SIDE_MAX: usize = 65_535;
#[cfg(feature = "decode-rawler")]
const STATED_PIXELS_MAX: u64 = 1 << 30;

/// The width and height a file states, without its pixels.
///
/// A DNG states them in the raw IFD. That decoder's `raw_image` ignores
/// the flag that asks for the size alone and decodes the frame
/// (`plain_image_from_ifd` fixes the flag to false), so the IFD is read
/// instead. It is the only decoder that returns the raw IFD. Every other
/// one is asked for the image with the flag.
#[cfg(feature = "decode-rawler")]
pub fn stated_size(path: &Path) -> Result<(usize, usize)> {
    let source = raw_source(path).map_err(|e| Error::Decode(e.to_string()))?;
    guarded(|| stated_size_of(&source))
}

#[cfg(feature = "decode-rawler")]
fn stated_size_of(source: &rawler::rawsource::RawSource) -> Result<(usize, usize)> {
    use rawler::decoders::{RawDecodeParams, WellKnownIFD};
    use rawler::tags::TiffCommonTag;
    let decoder = rawler::get_decoder(source).map_err(|e| Error::Decode(e.to_string()))?;
    if let Some((w, h)) =
        decoder.ifd(WellKnownIFD::Raw).map_err(|e| Error::Decode(e.to_string()))?.and_then(|ifd| {
            let w = ifd.get_entry(TiffCommonTag::ImageWidth)?.force_usize(0);
            let h = ifd.get_entry(TiffCommonTag::ImageLength)?.force_usize(0);
            (w > 0 && h > 0).then_some((w, h))
        })
    {
        return Ok((w, h));
    }
    let img = decoder
        .raw_image(source, &RawDecodeParams::default(), true)
        .map_err(|e| Error::Decode(e.to_string()))?;
    Ok((img.width, img.height))
}

/// Refuse a file whose stated frame is past [`STATED_SIDE_MAX`] on a side
/// or past [`STATED_PIXELS_MAX`] photosites. The sentence names the size.
#[cfg(feature = "decode-rawler")]
pub(crate) fn refuse_stated_frame(path: &Path) -> Result<()> {
    let (w, h) = stated_size(path)?;
    let pixels = (w as u64).saturating_mul(h as u64);
    if w > STATED_SIDE_MAX || h > STATED_SIDE_MAX || pixels > STATED_PIXELS_MAX {
        let name =
            path.file_name().map(|n| n.to_string_lossy()).unwrap_or(std::borrow::Cow::Borrowed("the file"));
        return Err(Error::Unsupported(format!("{name} states {w}x{h} photosites")));
    }
    Ok(())
}

#[cfg(feature = "decode-rawler")]
impl RawlerDecoder {
    pub fn decode(&self, path: &Path) -> Result<RawFrame> {
        let source = raw_source(path).map_err(|e| Error::Decode(e.to_string()))?;
        guarded(|| Self::decode_source(&source))
    }

    fn decode_source(source: &rawler::rawsource::RawSource) -> Result<RawFrame> {
        use rawler::rawimage::RawPhotometricInterpretation as P;
        let decoder = rawler::get_decoder(source).map_err(|e| Error::Decode(e.to_string()))?;
        let params = rawler::decoders::RawDecodeParams::default();
        let img = decoder.raw_image(source, &params, false).map_err(|e| Error::Decode(e.to_string()))?;
        // rawler hands the photosites of a Fuji Super CCD over on a grid
        // turned by 45 degrees, under a 2x2 name: read as an upright
        // Bayer frame it would develop into a turned picture in a square.
        if img.fuji_rotation_width.is_some() {
            return Err(Error::Unsupported(
                "Fuji Super CCD: its photosites lie on a diagonal grid, not on a 2x2 Bayer one".into(),
            ));
        }
        // Before the metadata is asked for: a file that is neither a
        // mosaic nor a linear DNG is refused here whatever its metadata
        // is. rawler 0.8 has none for a Sigma X3F (three components per
        // pixel) and panics when asked.
        let linear_dng = img.cpp == 3
            && img.photometric == P::LinearRaw
            && decoder.format_hint() == rawler::decoders::FormatHint::DNG;
        if img.cpp != 1 && !linear_dng {
            return Err(Error::Unsupported(format!(
                "{} components per pixel; expected a mosaic or a linear DNG",
                img.cpp
            )));
        }
        // Metadata is informational; a file without usable EXIF still decodes.
        let (exposure, exif_orientation) = match decoder.raw_metadata(source, &params) {
            Ok(md) => (exposure_from(&md), md.exif.orientation.filter(|o| (1..=8).contains(o))),
            Err(err) => {
                tracing::debug!("no exif metadata: {err}");
                (Exposure::default(), None)
            }
        };
        let kind = match &img.photometric {
            P::Cfa(cfg) => {
                if cfg.cfa.width != 2 || cfg.cfa.height != 2 {
                    return Err(Error::Unsupported(format!(
                        "CFA '{}' is {}x{}; only 2x2 Bayer is supported",
                        cfg.cfa.name, cfg.cfa.width, cfg.cfa.height
                    )));
                }
                SensorKind::Bayer(BayerPhase::from_name(&cfg.cfa.name)?)
            }
            P::LinearRaw if linear_dng => SensorKind::Rgb,
            P::LinearRaw | P::BlackIsZero => SensorKind::Mono,
        };

        let bl = &img.blacklevel;
        let black_tile: [f64; 4] = if bl.levels.len() == 1 {
            [f64::from(bl.levels[0].as_f32()); 4]
        } else if linear_dng && bl.width == 1 && bl.height == 1 && bl.cpp == 3 && bl.levels.len() == 3 {
            // One level per channel: R, G, B.
            let level = |c: usize| f64::from(bl.levels[c].as_f32());
            [level(0), level(1), level(2), level(1)]
        } else if !linear_dng && bl.width == 2 && bl.height == 2 && bl.cpp == 1 && bl.levels.len() == 4 {
            [
                f64::from(bl.levels[0].as_f32()),
                f64::from(bl.levels[1].as_f32()),
                f64::from(bl.levels[2].as_f32()),
                f64::from(bl.levels[3].as_f32()),
            ]
        } else {
            return Err(Error::Unsupported(format!(
                "black level repeat {}x{} cpp {} not handled",
                bl.width, bl.height, bl.cpp
            )));
        };

        // rawler's white level is per component. A mosaic has one, so the
        // one-entry form is what real files give; three or four entries are
        // read as R, G, B in case a decoder lists them per colour.
        //
        // A DNG that states no WhiteLevel means 1.0 when its samples are
        // floating point (DNG specification); rawler fills in 2^bits - 1
        // whatever the sample format, which would divide such a picture
        // down to black.
        let float_without_white = matches!(img.data, rawler::RawImageData::Float(_))
            && decoder.format_hint() == rawler::decoders::FormatHint::DNG
            && decoder
                .ifd(rawler::decoders::WellKnownIFD::Raw)
                .ok()
                .flatten()
                .is_some_and(|ifd| ifd.get_entry(rawler::tags::TiffCommonTag::WhiteLevel).is_none());
        let wl = &img.whitelevel.0;
        let white_rgb: [f64; 3] = match wl.len() {
            _ if float_without_white => [1.0; 3],
            1 => [f64::from(wl[0]); 3],
            3 | 4 => [f64::from(wl[0]), f64::from(wl[1]), f64::from(wl[2])],
            n => return Err(Error::Unsupported(format!("white level with {n} entries"))),
        };
        if black_tile.iter().any(|b| white_rgb.iter().any(|w| w <= b)) {
            return Err(Error::Unsupported(format!(
                "white level {:?} is not above black level {:?}",
                white_rgb, black_tile
            )));
        }

        let to_rect = |r: &rawler::imgop::Rect| Rect { x: r.p.x, y: r.p.y, w: r.d.w, h: r.d.h };
        let full = Rect { x: 0, y: 0, w: img.width, h: img.height };
        let crop = img
            .crop_area
            .as_ref()
            .map(to_rect)
            .or_else(|| img.active_area.as_ref().map(to_rect))
            .filter(|r| r.w > 0 && r.h > 0 && r.x + r.w <= img.width && r.y + r.h <= img.height)
            .unwrap_or(full);

        // rawler's own orientation comes from the raw IFD; some cameras (the
        // Z f among them) write it only into EXIF, so that is the fallback
        // when rawler reports nothing turned.
        let orientation = settled_orientation(img.orientation, exif_orientation);

        // The camera's XYZ → camera matrix: rawler's per-illuminant table
        // first (D65, the daylight matrix the plate wants; else the other
        // daylight-like entries; else whatever there is), then the older
        // single matrix.
        let (xyz_to_cam, illuminant) = {
            use rawler::imgop::xyz::Illuminant as I;
            let from_flat = |m: &[f32]| -> Option<[[f64; 3]; 3]> {
                if m.len() < 9 || m.iter().take(9).all(|v| *v == 0.0) {
                    return None;
                }
                let r = |i: usize| [f64::from(m[3 * i]), f64::from(m[3 * i + 1]), f64::from(m[3 * i + 2])];
                Some([r(0), r(1), r(2)])
            };
            // The illuminant as the EXIF LightSource code (what a DNG's
            // CalibrationIlluminant1 holds); rawler's enum carries it.
            let code = |k: &I| *k as u16;
            let preferred = [I::D65, I::D55, I::Daylight, I::D50, I::D75, I::FineWeather, I::CloudyWeather];
            let found = preferred
                .iter()
                .find_map(|k| img.color_matrix.get(k).and_then(|m| from_flat(m)).map(|m| (m, Some(code(k)))))
                .or_else(|| {
                    let mut keys: Vec<_> = img.color_matrix.keys().collect();
                    keys.sort_by_key(|k| **k as u16);
                    keys.into_iter().find_map(|k| from_flat(&img.color_matrix[k]).map(|m| (m, Some(code(k)))))
                })
                .or_else(|| {
                    let m = img.xyz_to_cam;
                    let nonzero = m.iter().take(3).flatten().any(|v| *v != 0.0);
                    nonzero.then(|| {
                        let r = |i: usize| [f64::from(m[i][0]), f64::from(m[i][1]), f64::from(m[i][2])];
                        ([r(0), r(1), r(2)], None)
                    })
                });
            match found {
                Some((m, i)) => (Some(m), i),
                None => (None, None),
            }
        };

        let wb_as_shot = {
            let c = img.wb_coeffs;
            let ok = c[..3].iter().all(|v| v.is_finite() && *v > 0.0);
            ok.then(|| {
                let g = f64::from(c[1]);
                [f64::from(c[0]) / g, 1.0, f64::from(c[2]) / g]
            })
        };

        let data = match img.data {
            rawler::RawImageData::Integer(v) => RawData::Integer(v),
            rawler::RawImageData::Float(v) => RawData::Float(v),
        };
        if data.len() != img.width * img.height * kind.samples() {
            return Err(Error::Decode(format!(
                "data length {} does not match {}x{}",
                data.len(),
                img.width,
                img.height
            )));
        }

        // A TIFF string may be padded to a word boundary (the DNG this
        // crate writes is): the name is what stands before the padding.
        let name = |s: String| s.trim().to_string();
        Ok(RawFrame {
            make: name(img.make),
            model: name(img.model),
            clean_make: name(img.clean_make),
            clean_model: name(img.clean_model),
            width: img.width,
            height: img.height,
            bits: img.bps,
            kind,
            black_tile,
            white_rgb,
            crop,
            orientation,
            xyz_to_cam,
            illuminant,
            wb_as_shot,
            exposure,
            data,
        })
    }
}

impl RawFrame {
    /// Black level at absolute sensor position, of a mosaic.
    #[inline]
    pub fn black_at(&self, x: usize, y: usize) -> f64 {
        self.black_tile[(y & 1) * 2 + (x & 1)]
    }

    /// White level at absolute sensor position, of a mosaic.
    #[inline]
    pub fn white_at(&self, x: usize, y: usize) -> f64 {
        match self.kind {
            SensorKind::Bayer(p) => match p.color_at(x, y) {
                crate::cfa::Color::R => self.white_rgb[0],
                crate::cfa::Color::G => self.white_rgb[1],
                crate::cfa::Color::B => self.white_rgb[2],
            },
            SensorKind::Mono | SensorKind::Rgb => self.white_rgb[1],
        }
    }

    /// Black and white level of channel `c` (0 R, 1 G, 2 B) of a
    /// three-channel frame.
    #[inline]
    pub fn channel_levels(&self, c: usize) -> (f64, f64) {
        (self.black_tile[c], self.white_rgb[c])
    }

    /// The three samples of pixel (x, y) of a three-channel frame, as
    /// stored.
    #[inline]
    pub fn rgb_at(&self, x: usize, y: usize) -> [f64; 3] {
        let i = 3 * (y * self.width + x);
        [self.data.get(i), self.data.get(i + 1), self.data.get(i + 2)]
    }

    /// The frame holds a sample for every photosite, or three for every
    /// pixel of a three-channel frame.
    pub fn holds_its_samples(&self) -> bool {
        self.width
            .checked_mul(self.height)
            .and_then(|n| n.checked_mul(self.kind.samples()))
            .is_some_and(|n| n == self.data.len())
    }

    /// Effective bit depth from the white level (container depth may be 16).
    pub fn effective_bits(&self) -> usize {
        let w = self.white_rgb.iter().cloned().fold(0.0f64, f64::max);
        if w <= 0.0 {
            return self.bits;
        }
        (w + 1.0).log2().ceil() as usize
    }

    /// Minimum/maximum raw sample value inside the crop. An empty or
    /// out-of-frame crop gives `(inf, -inf)`.
    pub fn raw_range(&self) -> (f64, f64) {
        use rayon::prelude::*;
        let c = self.crop;
        let n = self.kind.samples();
        if c.x + c.w > self.width || c.y + c.h > self.height || self.data.len() < n * self.width * self.height
        {
            return (f64::INFINITY, f64::NEG_INFINITY);
        }
        (c.y..c.y + c.h)
            .into_par_iter()
            .map(|y| {
                let mut lo = f64::INFINITY;
                let mut hi = f64::NEG_INFINITY;
                for i in n * (y * self.width + c.x)..n * (y * self.width + c.x + c.w) {
                    let v = self.data.get(i);
                    lo = lo.min(v);
                    hi = hi.max(v);
                }
                (lo, hi)
            })
            .reduce(|| (f64::INFINITY, f64::NEG_INFINITY), |a, b| (a.0.min(b.0), a.1.max(b.1)))
    }
}

#[cfg(all(test, feature = "decode-rawler"))]
mod tests {
    use super::*;

    #[test]
    fn the_settled_orientation_is_rawlers_own_else_the_exif_one() {
        use rawler::Orientation as O;
        assert_eq!(settled_orientation(O::Rotate90, Some(1)), 6);
        assert_eq!(settled_orientation(O::Rotate270, None), 8);
        assert_eq!(settled_orientation(O::Normal, Some(6)), 6);
        assert_eq!(settled_orientation(O::Unknown, Some(3)), 3);
        assert_eq!(settled_orientation(O::Normal, None), 1);
        assert_eq!(settled_orientation(O::Unknown, None), 0);
        assert_eq!(settled_orientation(O::Normal, Some(0)), 1);
        assert_eq!(settled_orientation(O::Unknown, Some(9)), 0);
    }

    /// A DNG of 16 x 16 photosites, written by hand: one uncompressed
    /// strip, 32-bit floating point or 16-bit integer samples, with or
    /// without a WhiteLevel, and the CFA pattern given.
    fn tiny_dng(float: bool, white: Option<u32>, cfa: &[u8]) -> Vec<u8> {
        const SIDE: u32 = 16;
        let pixels: Vec<u8> = (0..SIDE * SIDE)
            .flat_map(|i| {
                let step = (i % SIDE + i / SIDE) as f32 / (2 * SIDE - 2) as f32;
                if float {
                    (0.25 + 0.5 * step).to_le_bytes().to_vec()
                } else {
                    ((16384.0 + 32768.0 * step) as u16).to_le_bytes().to_vec()
                }
            })
            .collect();
        dng_file(SIDE, SIDE, float, white, cfa, pixels)
    }

    /// The same hand-written DNG with the size and the strip given.
    /// `pixels` is the strip; it need not be `width * height` samples,
    /// so a header can state a frame the test must not allocate.
    fn dng_file(
        width: u32,
        height: u32,
        float: bool,
        white: Option<u32>,
        cfa: &[u8],
        pixels: Vec<u8>,
    ) -> Vec<u8> {
        let short = |v: u16| v.to_le_bytes().to_vec();
        let long = |v: u32| v.to_le_bytes().to_vec();
        let text = |s: &str| [s.as_bytes(), &[0]].concat();
        // (tag, type, count, value); the strip offset is filled in below.
        let mut tags: Vec<(u16, u16, u32, Vec<u8>)> = vec![
            (254, 4, 1, long(0)),
            (256, 4, 1, long(width)),
            (257, 4, 1, long(height)),
            (258, 3, 1, short(if float { 32 } else { 16 })),
            (259, 3, 1, short(1)),
            (262, 3, 1, short(32803)),
            (271, 2, 5, text("Test")),
            (272, 2, 5, text("Tiny")),
            (273, 4, 1, Vec::new()),
            (277, 3, 1, short(1)),
            (278, 4, 1, long(height)),
            (279, 4, 1, long(pixels.len() as u32)),
            (284, 3, 1, short(1)),
            (339, 3, 1, short(if float { 3 } else { 1 })),
            (33421, 3, 2, [short((cfa.len() / 2) as u16), short(2)].concat()),
            (33422, 1, cfa.len() as u32, cfa.to_vec()),
            (50706, 1, 4, vec![1, 4, 0, 0]),
            (50708, 2, 10, text("Test Tiny")),
            (50721, 10, 9, {
                let unit = [1i32, 0, 0, 0, 1, 0, 0, 0, 1];
                unit.iter()
                    .flat_map(|v| [(v * 10000).to_le_bytes(), 10000i32.to_le_bytes()].concat())
                    .collect()
            }),
            (50728, 5, 3, [long(1), long(1)].concat().repeat(3)),
        ];
        if let Some(level) = white {
            tags.push((50717, 4, 1, long(level)));
        }
        tags.sort_by_key(|t| t.0);
        let values_at = 8 + 2 + 12 * tags.len() + 4;
        let mut values = Vec::new();
        let mut entries = Vec::new();
        for (tag, kind, count, value) in &tags {
            entries.extend([short(*tag), short(*kind), long(*count)].concat());
            if *tag == 273 {
                entries.extend([0xAA; 4]);
            } else if value.len() <= 4 {
                entries.extend(value.iter().copied().chain(std::iter::repeat(0)).take(4));
            } else {
                entries.extend(long((values_at + values.len()) as u32));
                values.extend(value);
                if values.len() % 2 == 1 {
                    values.push(0);
                }
            }
        }
        let strip = long((values_at + values.len()) as u32);
        let at = entries.windows(4).position(|w| w == [0xAA; 4]).unwrap();
        entries[at..at + 4].copy_from_slice(&strip);
        [b"II*\0".to_vec(), long(8), short(tags.len() as u16), entries, long(0), values, pixels].concat()
    }

    fn decoded(name: &str, bytes: &[u8]) -> Result<RawFrame> {
        let path = std::env::temp_dir().join(format!("mimizan-decode-{}-{name}.dng", std::process::id()));
        std::fs::write(&path, bytes).unwrap();
        let frame = RawlerDecoder.decode(&path);
        let _ = std::fs::remove_file(&path);
        frame
    }

    const RGGB: [u8; 4] = [0, 1, 1, 2];

    #[test]
    fn the_hand_written_dng_decodes() {
        let frame = decoded("int", &tiny_dng(false, Some(65535), &RGGB)).unwrap();
        assert_eq!((frame.width, frame.height), (16, 16));
        assert_eq!(frame.kind, SensorKind::Bayer(BayerPhase::RGGB));
        assert_eq!(frame.white_rgb, [65535.0; 3]);
        assert!(matches!(frame.data, RawData::Integer(_)));
    }

    /// The DNG default for floating-point samples is 1.0, not 2^32 - 1:
    /// the picture would otherwise be divided down to black.
    #[test]
    fn a_float_dng_without_a_white_level_is_white_at_one() {
        let frame = decoded("float", &tiny_dng(true, None, &RGGB)).unwrap();
        assert!(matches!(frame.data, RawData::Float(_)));
        assert_eq!(frame.white_rgb, [1.0; 3]);
        let (lo, hi) = frame.raw_range();
        assert!((lo - 0.25).abs() < 1e-6 && (hi - 0.75).abs() < 1e-6, "{lo} {hi}");
    }

    #[test]
    fn a_float_dng_keeps_the_white_level_it_states() {
        let frame = decoded("float-white", &tiny_dng(true, Some(4), &RGGB)).unwrap();
        assert_eq!(frame.white_rgb, [4.0; 3]);
        // An integer file without the tag keeps rawler's 2^bits - 1.
        let frame = decoded("int-default", &tiny_dng(false, None, &RGGB)).unwrap();
        assert_eq!(frame.white_rgb, [65535.0; 3]);
    }

    /// rawler panics on a CFA pattern of eight entries. The file is
    /// refused; the program goes on.
    #[test]
    fn a_file_the_decoder_panics_on_is_an_error() {
        let err = decoded("cfa8", &tiny_dng(false, Some(65535), &[0, 1, 1, 2, 1, 2, 0, 1])).unwrap_err();
        assert!(matches!(&err, Error::Decode(text) if text.contains("the decoder stopped")), "{err}");
    }

    #[test]
    fn a_file_that_is_not_there_or_not_a_raw_is_an_error() {
        let missing = std::env::temp_dir().join("mimizan-decode-no-such-file.dng");
        assert!(matches!(RawlerDecoder.decode(&missing), Err(Error::Decode(_))));
        assert!(matches!(decoded("text", b"not a raw file"), Err(Error::Decode(_))));
        // Cut off in the middle of its pixels.
        let whole = tiny_dng(false, Some(65535), &RGGB);
        assert!(decoded("cut", &whole[..whole.len() - 200]).is_err());
    }

    /// 60 000 × 60 000 is more than 2^30 photosites. The header says so
    /// and the strip is a few bytes: the refusal comes back without the
    /// frame, which would be 7 GB.
    #[test]
    fn a_header_of_60000_square_is_refused_before_the_frame_is_read() {
        let bytes = dng_file(60_000, 60_000, false, Some(65535), &RGGB, vec![0; 8]);
        let path = std::env::temp_dir().join(format!("mimizan-decode-{}-huge.dng", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        assert_eq!(stated_size(&path).unwrap(), (60_000, 60_000));
        let err = crate::pipeline::decode(&path).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(matches!(&err, Error::Unsupported(text) if text.contains("60000x60000")), "{err}");
    }

    /// A side past 65 535 is refused on its own, under the pixel count.
    #[test]
    fn a_side_stated_above_65535_is_refused() {
        let bytes = dng_file(70_000, 2, false, Some(65535), &RGGB, vec![0; 8]);
        let path = std::env::temp_dir().join(format!("mimizan-decode-{}-wide.dng", std::process::id()));
        std::fs::write(&path, &bytes).unwrap();
        let err = crate::pipeline::decode(&path).unwrap_err();
        let _ = std::fs::remove_file(&path);
        assert!(matches!(&err, Error::Unsupported(text) if text.contains("70000x2")), "{err}");
    }

    #[test]
    fn the_stated_size_of_the_hand_written_dng_is_its_frame() {
        let path = std::env::temp_dir().join(format!("mimizan-decode-{}-stated.dng", std::process::id()));
        std::fs::write(&path, tiny_dng(false, Some(65535), &RGGB)).unwrap();
        let got = stated_size(&path).unwrap();
        let frame = crate::pipeline::decode(&path).unwrap();
        let _ = std::fs::remove_file(&path);
        assert_eq!(got, (16, 16));
        assert_eq!((frame.width, frame.height), (16, 16));
    }
}
