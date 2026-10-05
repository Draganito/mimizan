//! RAW decoding behind a trait. The only file backend today is `rawler`
//! (feature `decode-rawler`); callers may also fill a `RawFrame` directly.

use crate::cfa::BayerPhase;
#[cfg(feature = "decode-rawler")]
use crate::error::Error;
use crate::error::Result;
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct Rect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize)]
pub enum SensorKind {
    Bayer(BayerPhase),
    Mono,
}

#[derive(Clone, Debug)]
pub enum RawData {
    Integer(Vec<u16>),
    Float(Vec<f32>),
}

impl RawData {
    #[inline]
    pub fn get(&self, i: usize) -> f64 {
        match self {
            RawData::Integer(v) => f64::from(v[i]),
            RawData::Float(v) => f64::from(v[i]),
        }
    }
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
    pub black_tile: [f64; 4],
    /// White level per colour: R, G, B (index by `Color`), G used twice.
    pub white_rgb: [f64; 3],
    pub crop: Rect,
    /// TIFF orientation value (1..8), 0 if unknown.
    pub orientation: u16,
    /// XYZ -> camera matrix (rows R,G,B), if the file provides one.
    pub xyz_to_cam: Option<[[f64; 3]; 3]>,
    /// As-shot white balance multipliers R,G,B normalised to G = 1, if present.
    pub wb_as_shot: Option<[f64; 3]>,
    pub exposure: Exposure,
    pub data: RawData,
}

pub trait RawDecoder {
    fn decode(&self, path: &Path) -> Result<RawFrame>;
}

#[cfg(feature = "decode-rawler")]
pub struct RawlerDecoder;

#[cfg(feature = "decode-rawler")]
pub(crate) fn exposure_from(md: &rawler::decoders::RawMetadata) -> Exposure {
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

#[cfg(feature = "decode-rawler")]
impl RawDecoder for RawlerDecoder {
    fn decode(&self, path: &Path) -> Result<RawFrame> {
        use rawler::rawimage::RawPhotometricInterpretation as P;
        let source = rawler::rawsource::RawSource::new(path).map_err(|e| Error::Decode(e.to_string()))?;
        let decoder = rawler::get_decoder(&source).map_err(|e| Error::Decode(e.to_string()))?;
        let params = rawler::decoders::RawDecodeParams::default();
        let img = decoder.raw_image(&source, &params, false).map_err(|e| Error::Decode(e.to_string()))?;
        // Metadata is informational; a file without usable EXIF still decodes.
        let (exposure, exif_orientation) = match decoder.raw_metadata(&source, &params) {
            Ok(md) => (exposure_from(&md), md.exif.orientation.filter(|o| (1..=8).contains(o))),
            Err(err) => {
                tracing::debug!("no exif metadata: {err}");
                (Exposure::default(), None)
            }
        };
        if img.cpp != 1 {
            return Err(Error::Unsupported(format!("{} components per pixel; expected a mosaic", img.cpp)));
        }
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
            P::LinearRaw | P::BlackIsZero => SensorKind::Mono,
        };

        let bl = &img.blacklevel;
        let black_tile: [f64; 4] = if bl.levels.len() == 1 {
            [f64::from(bl.levels[0].as_f32()); 4]
        } else if bl.width == 2 && bl.height == 2 && bl.cpp == 1 {
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

        let wl = &img.whitelevel.0;
        let white_rgb: [f64; 3] = match wl.len() {
            1 => [f64::from(wl[0]); 3],
            3 | 4 => [f64::from(wl[0]), f64::from(wl[1]), f64::from(wl[2])],
            n => return Err(Error::Unsupported(format!("white level with {n} entries"))),
        };

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
        let orientation = {
            use rawler::Orientation as O;
            match img.orientation {
                O::Normal | O::Unknown => {
                    exif_orientation.unwrap_or(if img.orientation == O::Normal { 1 } else { 0 })
                }
                O::HorizontalFlip => 2,
                O::Rotate180 => 3,
                O::VerticalFlip => 4,
                O::Transpose => 5,
                O::Rotate90 => 6,
                O::Transverse => 7,
                O::Rotate270 => 8,
            }
        };

        let xyz_to_cam = {
            let m = img.xyz_to_cam;
            let nonzero = m.iter().take(3).flatten().any(|v| *v != 0.0);
            nonzero.then(|| {
                let r = |i: usize| [f64::from(m[i][0]), f64::from(m[i][1]), f64::from(m[i][2])];
                [r(0), r(1), r(2)]
            })
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
        if data.len() != img.width * img.height {
            return Err(Error::Decode(format!(
                "data length {} does not match {}x{}",
                data.len(),
                img.width,
                img.height
            )));
        }

        Ok(RawFrame {
            make: img.make,
            model: img.model,
            clean_make: img.clean_make,
            clean_model: img.clean_model,
            width: img.width,
            height: img.height,
            bits: img.bps,
            kind,
            black_tile,
            white_rgb,
            crop,
            orientation,
            xyz_to_cam,
            wb_as_shot,
            exposure,
            data,
        })
    }
}

impl RawFrame {
    /// Black level at absolute sensor position.
    #[inline]
    pub fn black_at(&self, x: usize, y: usize) -> f64 {
        self.black_tile[(y & 1) * 2 + (x & 1)]
    }

    /// White level at absolute sensor position.
    #[inline]
    pub fn white_at(&self, x: usize, y: usize) -> f64 {
        match self.kind {
            SensorKind::Bayer(p) => match p.color_at(x, y) {
                crate::cfa::Color::R => self.white_rgb[0],
                crate::cfa::Color::G => self.white_rgb[1],
                crate::cfa::Color::B => self.white_rgb[2],
            },
            SensorKind::Mono => self.white_rgb[1],
        }
    }

    /// Effective bit depth from the white level (container depth may be 16).
    pub fn effective_bits(&self) -> usize {
        let w = self.white_rgb.iter().cloned().fold(0.0f64, f64::max);
        if w <= 0.0 {
            return self.bits;
        }
        (w + 1.0).log2().ceil() as usize
    }

    /// Minimum/maximum raw sample value inside the crop.
    pub fn raw_range(&self) -> (f64, f64) {
        let mut lo = f64::INFINITY;
        let mut hi = f64::NEG_INFINITY;
        for y in self.crop.y..self.crop.y + self.crop.h {
            for x in self.crop.x..self.crop.x + self.crop.w {
                let v = self.data.get(y * self.width + x);
                lo = lo.min(v);
                hi = hi.max(v);
            }
        }
        (lo, hi)
    }
}
