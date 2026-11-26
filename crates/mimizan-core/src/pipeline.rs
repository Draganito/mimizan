//! RAW -> negative, the one path both CLI and GUI call.

use crate::calib::CameraFile;
use crate::decode::RawFrame;
#[cfg(feature = "decode-rawler")]
use crate::decode::RawlerDecoder;
use crate::error::Result;
use crate::ingest::{ingest, ingest_rgb, Channels, IngestParams, IngestReport, Mosaic, NoiseModel};
use crate::mix::{mix, mix_into, ColorFilter, WeightSpace, Weights};
use crate::plane::Plane;
use crate::separate::{separate, MaskMode, SeparateParams};
use rayon::prelude::*;
use serde::Serialize;
#[cfg(feature = "decode-rawler")]
use std::path::Path;
use std::time::Instant;
use tracing::info;

/// Everything that decides how a frame becomes a negative.
#[derive(Clone, Debug, Default)]
pub struct NegativeParams {
    pub ingest: IngestParams,
    pub separate: SeparateParams,
    /// Mix weights, in `weights_space`. Raw-space weights are converted with
    /// the image's balance multipliers once ingest has determined them.
    pub weights: Weights,
    pub weights_space: WeightSpace,
    /// Contrast filter applied to the balanced weights (`Weights::filtered`).
    pub filter: ColorFilter,
    pub camera_file: Option<CameraFile>,
    /// Keep `L̂, Ĉ1', Ĉ2'` (three more planes) so the weights can be re-mixed
    /// without separating again (GUI). The CLI leaves this off.
    pub keep_separation: bool,
    /// Make a three-colour plate (`plate.rs`) instead of one negative: the
    /// three taking filters replace the mix, and `Negative::plane` holds
    /// the three frames stacked. `None` is the monochrome negative.
    pub plate: Option<crate::plate::PlateParams>,
}

impl NegativeParams {
    /// Apply a camera file: its noise model and its weights. `with_weights`
    /// afterwards still overrides the weights.
    pub fn with_camera_file(mut self, cam: CameraFile) -> Self {
        self.ingest.noise = cam.noise();
        self.weights = cam.weights();
        self.weights_space = cam.weights_space;
        self.camera_file = Some(cam);
        self
    }

    /// Override the weights (balanced space unless stated otherwise).
    pub fn with_weights(mut self, w: Weights, space: WeightSpace) -> Self {
        self.weights = w;
        self.weights_space = space;
        self
    }
}

/// Wall-clock time of each stage, for the description and the log.
#[derive(Clone, Debug, Serialize)]
pub struct Timing {
    pub decode_ms: u128,
    pub ingest_ms: u128,
    pub separate_ms: u128,
    pub mix_ms: u128,
}

/// The negative's description: what was done to the file, written as JSON
/// into the TIFF.
#[derive(Clone, Debug, Serialize)]
pub struct NegativeInfo {
    pub mimizan: &'static str,
    pub stage: &'static str,
    pub camera: crate::ingest::CameraInfo,
    pub phase: Option<String>,
    pub noise: NoiseModel,
    /// Balance multipliers R,G,B applied before separation (G = 1).
    pub wb: [f64; 3],
    /// Weights the mix used, on the balanced channels (filter included).
    pub weights: Weights,
    /// Contrast filter the weights were derived with.
    pub filter: ColorFilter,
    /// The same weights on the raw channels (`weights × wb`, normalised):
    /// the illuminant-independent form.
    pub weights_raw: Weights,
    pub mask: MaskMode,
    /// Refinement rounds the separation ran.
    pub rounds: usize,
    /// Chroma cutoff of the Kaiser low-pass. The `dubois` mask has its own
    /// bands and does not read it; the value is recorded as configured.
    pub cutoff: f64,
    pub report: IngestReport,
    pub timing: Timing,
    /// The scene value the file's full scale stands for ([`scene_range`]
    /// of the plane that is written): a sample `s` of the file is the
    /// scene value `s · range`. Left out when it is 1, which every file
    /// without the field means.
    #[serde(skip_serializing_if = "is_one")]
    pub range: f64,
    /// Present when the file is a three-colour plate (`plate.rs`): frame
    /// order, frame height, the filter set and its weights.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plate: Option<crate::plate::PlateInfo>,
}

fn is_one(v: &f64) -> bool {
    *v == 1.0
}

/// The range a negative's file needs to hold `plane`: its highest value,
/// and 1 when nothing in it goes above 1. A balanced channel passes 1,
/// the level at which green is full, before its own photosite is, and a
/// burnt-out photosite reads its multiplier; a mix or a plate of them
/// stands above 1 there. The file keeps that: its samples are the plane
/// divided by the range (`tiffout::write_gray16_range`). A plane within
/// 1 is written as it always was.
pub fn scene_range(plane: &Plane) -> f64 {
    let top = plane.data.par_iter().copied().filter(|v| v.is_finite()).reduce(|| 1.0, f64::max);
    top.max(1.0)
}

/// The developed negative: the linear plane, its sharpening mask, and the
/// description.
pub struct Negative {
    pub plane: Plane,
    /// 8-bit sidecar: `round(255*mask_max)`, 255 where saturated.
    pub mask8: Vec<u8>,
    pub orientation: u16,
    pub info: NegativeInfo,
    /// Only with `NegativeParams::keep_separation`. Its `mask_max` is
    /// released (a 1×1 plane): `mask8` is the mask.
    pub separation: Option<crate::separate::Separation>,
}

/// Read a RAW file with `rawler`.
///
/// A frame stated with a side above 65 535, or of more than 2^30
/// photosites, is refused before its pixels are read.
#[cfg(feature = "decode-rawler")]
pub fn decode(path: &Path) -> Result<RawFrame> {
    crate::decode::refuse_stated_frame(path)?;
    RawlerDecoder.decode(path)
}

/// Develop a decoded frame: ingest, separate (Bayer only), mix. `decode_ms`
/// is recorded in the timing.
///
/// A three-channel frame (a linear DNG) is not separated: its channels
/// are the separation ([`crate::separate::from_channels`]), and the mix,
/// the plate and everything after them are those of a Bayer file. The
/// separation's settings do nothing there.
pub fn negative_from_frame(frame: &RawFrame, p: &NegativeParams, decode_ms: u128) -> Result<Negative> {
    if let (Some(cam), crate::decode::SensorKind::Bayer(ph)) = (&p.camera_file, frame.kind) {
        if cam.phase() != ph {
            return Err(crate::Error::Invalid(format!(
                "camera file says {} but the file is {}",
                cam.bayer_phase, ph
            )));
        }
    }
    p.weights.validate()?;

    let t = Instant::now();
    if frame.kind == crate::decode::SensorKind::Rgb {
        let channels = ingest_rgb(frame, &p.ingest)?;
        return negative_from_channels(channels, p, decode_ms, t.elapsed().as_millis());
    }
    let mosaic = ingest(frame, &p.ingest)?;
    let ingest_ms = t.elapsed().as_millis();
    info!("ingest {} ms: {}", ingest_ms, serde_json::to_string(&mosaic.report)?);

    let weights = p.weights.to_balanced(p.weights_space, mosaic.wb);
    weights.validate()?;
    if p.weights_space == WeightSpace::Raw {
        info!(
            "weights raw R {:.3} G {:.3} B {:.3} -> balanced R {:.3} G {:.3} B {:.3}",
            p.weights.r, p.weights.g, p.weights.b, weights.r, weights.g, weights.b
        );
    }
    let weights = weights.filtered(p.filter);
    if p.filter != ColorFilter::None {
        weights.validate().map_err(|e| {
            crate::Error::Invalid(format!(
                "filter {} passes none of the weighted channels: {e}",
                p.filter.name()
            ))
        })?;
        info!("filter {}: weights R {:.3} G {:.3} B {:.3}", p.filter.name(), weights.r, weights.g, weights.b);
    }

    let mut info = NegativeInfo {
        mimizan: env!("CARGO_PKG_VERSION"),
        stage: "negative",
        camera: mosaic.camera.clone(),
        phase: mosaic.phase.map(|p| p.name().to_string()),
        noise: mosaic.noise,
        wb: mosaic.wb,
        weights,
        filter: p.filter,
        weights_raw: weights.balanced_to_raw(mosaic.wb),
        mask: p.separate.mask,
        rounds: 0,
        cutoff: p.separate.cutoff,
        report: mosaic.report.clone(),
        timing: Timing { decode_ms, ingest_ms, separate_ms: 0, mix_ms: 0 },
        range: 1.0,
        plate: None,
    };
    let orientation = mosaic.orientation;

    // The plate's taking filters, resolved before the separation so a bad
    // matrix fails early. The emulsion behind the Wratten glass is the
    // camera file's panchromatic response, or the native one: not the
    // mix, which is one negative's filter choice (`plate_base`).
    let plate_weights = match (&p.plate, mosaic.phase) {
        (Some(pp), Some(_)) => {
            let base = plate_base(p.camera_file.as_ref(), mosaic.wb);
            let w = crate::plate::PlateWeights::resolve(pp, base, mosaic.xyz_to_cam.as_ref())?;
            for (name, row) in ["red", "green", "blue"].iter().zip(w.rows) {
                info!(
                    "plate {name}: weights R {:+.3} G {:+.3} B {:+.3} ({})",
                    row[0],
                    row[1],
                    row[2],
                    w.filters.name()
                );
            }
            Some((w, pp.space))
        }
        (Some(_), None) => {
            return Err(crate::Error::Invalid("a three-colour plate needs a Bayer mosaic".into()));
        }
        (None, _) => None,
    };

    let (plane, mask8, separation) = match mosaic.phase {
        Some(_) => {
            let t = Instant::now();
            let sep = separate(&mosaic, &p.separate)?;
            info.timing.separate_ms = t.elapsed().as_millis();
            info.rounds = sep.rounds;
            info!("separate {} ms, {} rounds", info.timing.separate_ms, sep.rounds);
            // The mosaic plane is no longer needed: free it before mixing.
            let Mosaic { saturated, plane: mosaic_plane, .. } = mosaic;
            drop(mosaic_plane);
            let mask8 = mask_sidecar(&sep.mask_max, &saturated)?;
            // The mask lives on as the sidecar; its f64 plane is not
            // needed by anyone who keeps the separation.
            let mut sep = sep;
            sep.mask_max = Plane::zeros(1, 1);
            let t = Instant::now();
            let (plane, separation) = if let Some((w, space)) = plate_weights {
                info.plate = Some(crate::plate::PlateInfo::new(&w, space, sep.lum.height));
                let plate = crate::plate::mix_plate_stacked(&sep, &w)?;
                (plate, p.keep_separation.then_some(sep))
            } else if p.keep_separation {
                (mix(&sep, &weights), Some(sep))
            } else {
                (mix_into(sep, &weights), None)
            };
            info.timing.mix_ms = t.elapsed().as_millis();
            (plane, mask8, separation)
        }
        None => {
            let Mosaic { saturated, plane, .. } = mosaic;
            // The measurement is the picture; the colour mask was not run.
            // No carriers on a mono sensor: sharpening allowed except at saturation.
            let allowed = (crate::separate::FIXED_MASK_VALUE * 255.0).round() as u8;
            let mask8 = saturated.iter().map(|&s| if s != 0 { 255 } else { allowed }).collect();
            (plane, mask8, None)
        }
    };
    info.range = scene_range(&plane);
    if info.range > 1.0 {
        info!("range {:.4}: the negative holds {:.2} stops above 1", info.range, info.range.log2());
    }
    Ok(Negative { plane, mask8, orientation, info, separation })
}

/// [`negative_from_frame`] for a file that has its three channels: the
/// weights and the plate as there, the channels in place of a separation.
fn negative_from_channels(
    channels: Channels,
    p: &NegativeParams,
    decode_ms: u128,
    ingest_ms: u128,
) -> Result<Negative> {
    info!("ingest {} ms: {}", ingest_ms, serde_json::to_string(&channels.report)?);
    let weights = p.weights.to_balanced(p.weights_space, channels.wb);
    weights.validate()?;
    let weights = weights.filtered(p.filter);
    if p.filter != ColorFilter::None {
        weights.validate().map_err(|e| {
            crate::Error::Invalid(format!(
                "filter {} passes none of the weighted channels: {e}",
                p.filter.name()
            ))
        })?;
    }
    let mut info = NegativeInfo {
        mimizan: env!("CARGO_PKG_VERSION"),
        stage: "negative",
        camera: channels.camera.clone(),
        phase: None,
        noise: channels.noise,
        wb: channels.wb,
        weights,
        filter: p.filter,
        weights_raw: weights.balanced_to_raw(channels.wb),
        mask: p.separate.mask,
        rounds: 0,
        cutoff: p.separate.cutoff,
        report: channels.report.clone(),
        timing: Timing { decode_ms, ingest_ms, separate_ms: 0, mix_ms: 0 },
        range: 1.0,
        plate: None,
    };
    let plate_weights = match &p.plate {
        Some(pp) => {
            let base = plate_base(p.camera_file.as_ref(), channels.wb);
            Some((crate::plate::PlateWeights::resolve(pp, base, channels.xyz_to_cam.as_ref())?, pp.space))
        }
        None => None,
    };
    let Channels { rgb, saturated, colour_ceiling, orientation, .. } = channels;
    // No carriers to weigh: sharpening allowed except at saturation, as
    // on a monochrome sensor.
    let allowed = (crate::separate::FIXED_MASK_VALUE * 255.0).round() as u8;
    let mask8 = saturated.iter().map(|&s| if s != 0 { 255 } else { allowed }).collect();
    drop(saturated);
    let t = Instant::now();
    let sep = crate::separate::from_channels(rgb, colour_ceiling);
    let (plane, separation) = if let Some((w, space)) = plate_weights {
        info.plate = Some(crate::plate::PlateInfo::new(&w, space, sep.lum.height));
        (crate::plate::mix_plate_stacked(&sep, &w)?, p.keep_separation.then_some(sep))
    } else if p.keep_separation {
        (mix(&sep, &weights), Some(sep))
    } else {
        (mix_into(sep, &weights), None)
    };
    info.timing.mix_ms = t.elapsed().as_millis();
    info.range = scene_range(&plane);
    if info.range > 1.0 {
        info!("range {:.4}: the negative holds {:.2} stops above 1", info.range, info.range.log2());
    }
    Ok(Negative { plane, mask8, orientation, info, separation })
}

/// The panchromatic emulsion behind the plate's Wratten glass, on the
/// balanced channels: the camera file's calibrated response when there is
/// one, else the native 0.25 / 0.50 / 0.25. A fact of the file, not a
/// choice; the mix is for one negative and does not reach the plate.
pub fn plate_base(camera_file: Option<&CameraFile>, wb: [f64; 3]) -> Weights {
    camera_file.map(|c| c.weights_balanced(wb)).unwrap_or_default()
}

/// The 8-bit sidecar: `round(255 · mask_max)`, 255 where saturated and
/// nowhere else. A mask that rounds to 255 is written as 254: every
/// reader takes 255 for a burnt-out photosite.
pub fn mask_sidecar(mask_max: &Plane, saturated: &[u8]) -> Result<Vec<u8>> {
    if mask_max.data.len() != saturated.len() {
        return Err(crate::Error::Invalid(format!(
            "mask has {} samples and saturation has {}",
            mask_max.data.len(),
            saturated.len()
        )));
    }
    Ok(mask_max
        .data
        .par_iter()
        .zip(saturated.par_iter())
        .map(|(&m, &s)| if s != 0 { 255 } else { ((m.clamp(0.0, 1.0) * 255.0).round() as u8).min(254) })
        .collect())
}

/// Bytes a development of `frame` holds at its widest point. Bayer: the
/// Dubois estimate at its peak, nine f64 planes (`dubois::estimate`), over
/// the frame as decoded (two bytes a sample, four for a float file) and the
/// saturation map, a byte. The second estimate of the chrominance, below
/// the colour ceiling, runs in windows of a megapixel beside five held
/// planes and stays under that peak on any frame above 3 MP. Mono: the
/// normalised plane beside the measured one, over the frame and the
/// saturation map. Three channels: a plate at its widest, the three planes
/// (which became the separation in place), the plate's three and the
/// frame being mixed, over the frame's three samples a pixel and the
/// saturation map; nothing is estimated, and one negative needs less.
/// A guide for refusing a file the machine cannot hold, measured to about
/// a tenth; what it does not see is the allocator and whatever else the
/// process has open.
pub fn develop_memory_of(frame: &RawFrame) -> u64 {
    let sample = match frame.data {
        crate::decode::RawData::Integer(_) => 2,
        crate::decode::RawData::Float(_) => 4,
    };
    develop_memory_for(frame.width, frame.height, frame.kind, sample)
}

/// [`develop_memory_of`] for a frame of `w×h` with 16-bit samples, the
/// common case: the figure before the file is decoded, or for a caller
/// that holds the size and not the frame.
pub fn develop_memory(w: usize, h: usize, kind: crate::decode::SensorKind) -> u64 {
    develop_memory_for(w, h, kind, 2)
}

fn develop_memory_for(w: usize, h: usize, kind: crate::decode::SensorKind, sample: u64) -> u64 {
    let px = w as u64 * h as u64;
    let (planes, maps) = match kind {
        crate::decode::SensorKind::Bayer(_) => (9, 1),
        crate::decode::SensorKind::Mono => (2, 1),
        crate::decode::SensorKind::Rgb => (7, 1),
    };
    px * (planes * 8 + kind.samples() as u64 * sample + maps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn develop_memory_is_the_dubois_peak() {
        // 24 MP Bayer: nine planes of 195 MB plus the frame, measured 2.1 GB
        // over the process baseline. Mono is a fraction of that.
        let bayer =
            develop_memory(6048, 4032, crate::decode::SensorKind::Bayer(crate::cfa::BayerPhase::RGGB));
        assert!((1.8e9..2.0e9).contains(&(bayer as f64)), "{bayer}");
        let mono = develop_memory(6048, 4032, crate::decode::SensorKind::Mono);
        assert!(mono * 3 < bayer, "{mono} vs {bayer}");
        // Three channels, 26 MP as a plate: measured 63 bytes a pixel,
        // seven planes, six bytes of frame and the map. Nothing is
        // estimated, so it stays under the Bayer figure.
        let channels = develop_memory(6048, 4032, crate::decode::SensorKind::Rgb);
        assert_eq!(channels, 6048 * 4032 * 63);
        assert!(mono < channels && channels < bayer, "{mono} {channels} {bayer}");
        // A float file holds two more bytes a sample; an integer one is
        // the figure above.
        let (w, h) = (6048usize, 4032usize);
        let frame = |data: crate::decode::RawData| RawFrame {
            make: String::new(),
            model: String::new(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: w,
            height: h,
            bits: 14,
            kind: crate::decode::SensorKind::Bayer(crate::cfa::BayerPhase::RGGB),
            black_tile: [0.0; 4],
            white_rgb: [1.0; 3],
            crop: crate::decode::Rect { x: 0, y: 0, w, h },
            orientation: 1,
            xyz_to_cam: None,
            illuminant: None,
            wb_as_shot: None,
            exposure: Default::default(),
            data,
        };
        let integer = frame(crate::decode::RawData::Integer(Vec::new()));
        assert_eq!(develop_memory_of(&integer), bayer);
        let float = frame(crate::decode::RawData::Float(Vec::new()));
        assert_eq!(develop_memory_of(&float), bayer + 2 * (w * h) as u64);
    }

    /// The mix is one negative's filter and does not reach the plate: the
    /// same frame developed as a Wratten plate under two different
    /// `weights` is the same plate to the bit, while the monochrome
    /// negative under those weights is not.
    #[test]
    fn the_plate_does_not_see_the_mix() {
        use crate::cfa::{BayerPhase, Color};
        use crate::decode::{RawData, Rect, SensorKind};
        use crate::ingest::WhiteBalance;
        use crate::plate::{PlateFilters, PlateParams, PlateSpace};

        let (w, h) = (96usize, 96usize);
        let phase = BayerPhase::RGGB;
        // A neutral field with a warm square in it.
        let raw: Vec<u16> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let warm = (30..66).contains(&x) && (30..66).contains(&y);
                match (phase.color_at(x, y), warm) {
                    (Color::R, false) => 200,
                    (Color::G, false) => 400,
                    (Color::B, false) => 300,
                    (Color::R, true) => 500,
                    (Color::G, true) => 400,
                    (Color::B, true) => 150,
                }
            })
            .collect();
        let frame = RawFrame {
            make: String::new(),
            model: String::new(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: w,
            height: h,
            bits: 12,
            kind: SensorKind::Bayer(phase),
            black_tile: [0.0; 4],
            white_rgb: [1000.0; 3],
            crop: Rect { x: 0, y: 0, w, h },
            orientation: 1,
            xyz_to_cam: None,
            illuminant: None,
            wb_as_shot: None,
            exposure: Default::default(),
            data: RawData::Integer(raw),
        };
        let develop = |weights: Weights, plate: Option<PlateParams>| {
            let mut p = NegativeParams::default();
            p.ingest.wb = WhiteBalance::Manual([2.0, 1.0, 4.0 / 3.0]);
            p.weights = weights;
            p.plate = plate;
            negative_from_frame(&frame, &p, 0).unwrap()
        };
        let native = Weights::default();
        let red = Weights { r: 0.7, g: 0.2, b: 0.1 };
        let wratten = Some(PlateParams { filters: PlateFilters::Wratten, space: PlateSpace::Srgb });
        let a = develop(native, wratten);
        let b = develop(red, wratten);
        assert_eq!(a.plane.data, b.plane.data, "the plate under two mixes");
        assert_eq!(a.info.plate, b.info.plate);
        // The monochrome negative does see the mix.
        let m1 = develop(native, None);
        let m2 = develop(red, None);
        assert_ne!(m1.plane.data, m2.plane.data);
        // Without a camera file the emulsion is the native one.
        assert_eq!(plate_base(None, [2.0, 1.0, 1.5]), Weights::default());
    }

    /// A three-channel frame develops without a separation: the negative
    /// is the mix of its balanced channels, the plate is those channels,
    /// and the mask allows sharpening everywhere but at saturation. A
    /// burnt-out pixel has the luminance of its reading and no colour.
    #[test]
    fn three_channels_develop_as_they_are() {
        use crate::decode::{RawData, Rect, SensorKind};
        use crate::ingest::WhiteBalance;
        use crate::plate::{PlateFilters, PlateParams, PlateSpace};

        let (w, h) = (24usize, 16usize);
        let burnt = (12usize, 8usize);
        let px = |x: usize, y: usize| -> [u16; 3] {
            if (x, y) == burnt {
                [1000, 1000, 1000]
            } else {
                [100 + 10 * x as u16, 300 + 20 * y as u16, 400 - 5 * (x + y) as u16]
            }
        };
        let raw: Vec<u16> = (0..w * h).flat_map(|i| px(i % w, i / w)).collect();
        let frame = RawFrame {
            make: String::new(),
            model: String::new(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: w,
            height: h,
            bits: 16,
            kind: SensorKind::Rgb,
            black_tile: [0.0; 4],
            white_rgb: [1000.0; 3],
            crop: Rect { x: 0, y: 0, w, h },
            orientation: 1,
            xyz_to_cam: None,
            illuminant: None,
            wb_as_shot: None,
            exposure: Default::default(),
            data: RawData::Integer(raw),
        };
        let wb = [2.0, 1.0, 1.5];
        let develop = |weights: Weights, plate: Option<PlateParams>, keep: bool| {
            let mut p = NegativeParams::default();
            p.ingest.wb = WhiteBalance::Manual(wb);
            p.ingest.sat_dilate = 0;
            p.weights = weights;
            p.plate = plate;
            p.keep_separation = keep;
            negative_from_frame(&frame, &p, 0).unwrap()
        };
        let balanced = |x: usize, y: usize| {
            let [r, g, b] = px(x, y);
            [wb[0] * r as f64 / 1000.0, g as f64 / 1000.0, wb[2] * b as f64 / 1000.0]
        };
        let here = |x: usize, y: usize| (x, y) != burnt;

        let n = develop(Weights::default(), None, false);
        assert_eq!((n.plane.width, n.plane.height), (w, h));
        assert_eq!((n.info.phase.as_deref(), n.info.rounds, n.info.timing.separate_ms), (None, 0, 0));
        assert!(n.separation.is_none());
        let allowed = (crate::separate::FIXED_MASK_VALUE * 255.0).round() as u8;
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = balanced(x, y);
                let want = 0.25 * (r + 2.0 * g + b);
                assert!((n.plane.at(x, y) - want).abs() < 1e-12, "{x},{y}");
                assert_eq!(n.mask8[y * w + x], if here(x, y) { allowed } else { 255 });
            }
        }
        // The burnt-out pixel reads (2 + 2 + 1.5)/4, above 1, and the
        // negative says so.
        assert!((n.info.range - 1.375).abs() < 1e-12, "{}", n.info.range);

        // One channel's weights are that channel, below the ceiling.
        let red = develop(Weights { r: 1.0, g: 0.0, b: 0.0 }, None, true);
        assert!(red.separation.is_some());
        for y in 0..h {
            for x in (0..w).filter(|&x| here(x, y)) {
                assert!((red.plane.at(x, y) - balanced(x, y)[0]).abs() < 1e-12, "{x},{y}");
            }
        }
        // The plate of the bare channels is the three of them, stacked
        // blue, green, red; the burnt-out pixel is neutral in it.
        let channels = Some(PlateParams { filters: PlateFilters::Channels, space: PlateSpace::Srgb });
        let plate = develop(Weights::default(), channels, false);
        assert_eq!((plate.plane.width, plate.plane.height), (w, 3 * h));
        assert_eq!(plate.info.plate.as_ref().map(|p| p.frame_height), Some(h));
        for y in 0..h {
            for x in 0..w {
                let [r, g, b] = if here(x, y) { balanced(x, y) } else { [1.375; 3] };
                for (i, want) in [b, g, r].into_iter().enumerate() {
                    assert!((plate.plane.at(x, i * h + y) - want).abs() < 1e-12, "{x},{y} frame {i}");
                }
            }
        }
        assert_eq!(plate.mask8, n.mask8);
    }

    /// A negative keeps what stands above 1. A burnt-out neutral under a
    /// daylight balance reads (k_R + 2 + k_B)/4 in the luminance: the
    /// description states that as the range, the file's samples are the
    /// plane divided by it, and read back as a negative they are the plane
    /// again to half a 16-bit step of that range. Read as plain samples
    /// the file ends at its full scale. A negative with nothing above 1
    /// has range 1, says nothing of it and is written as it always was.
    #[test]
    fn a_negative_keeps_what_stands_above_one() {
        use crate::cfa::{BayerPhase, Color};
        use crate::decode::{RawData, Rect, SensorKind};
        use crate::icc::GrayTrc;
        use crate::ingest::WhiteBalance;
        use crate::tiffin::{described_range, read_gray, read_negative};
        use crate::tiffout::{write_gray16, write_gray16_range, TiffMeta};

        let (w, h) = (96usize, 96usize);
        let phase = BayerPhase::RGGB;
        let frame = |burnt: bool| RawFrame {
            make: String::new(),
            model: String::new(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: w,
            height: h,
            bits: 12,
            kind: SensorKind::Bayer(phase),
            black_tile: [0.0; 4],
            white_rgb: [1000.0; 3],
            crop: Rect { x: 0, y: 0, w, h },
            orientation: 1,
            xyz_to_cam: None,
            illuminant: None,
            wb_as_shot: None,
            exposure: Default::default(),
            // A neutral grey, and a burnt-out square in it.
            data: RawData::Integer(
                (0..w * h)
                    .map(|i| {
                        let (x, y) = (i % w, i / w);
                        if burnt && (30..66).contains(&x) && (30..66).contains(&y) {
                            return 1000;
                        }
                        match phase.color_at(x, y) {
                            Color::R => 200,
                            Color::G => 400,
                            Color::B => 267,
                        }
                    })
                    .collect(),
            ),
        };
        let develop = |burnt: bool| {
            let mut p = NegativeParams::default();
            p.ingest.wb = WhiteBalance::Manual([2.0, 1.0, 1.5]);
            negative_from_frame(&frame(burnt), &p, 0).unwrap()
        };
        let dir = std::env::temp_dir().join(format!("mimizan-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let meta = |description: &'static str| TiffMeta {
            description: Box::leak(description.to_string().into_boxed_str()),
            trc: GrayTrc::Linear,
            orientation: 1,
            dpi: None,
        };

        let neg = develop(true);
        let top = neg.plane.data.iter().copied().fold(0.0, f64::max);
        assert!((neg.plane.at(48, 48) - 1.375).abs() < 1e-9, "{}", neg.plane.at(48, 48));
        assert_eq!(neg.info.range, top);
        assert_eq!(neg.info.range, scene_range(&neg.plane));
        // The highest value is the estimator's overshoot at the square's
        // hard edge, a little above the square itself.
        assert!(neg.info.range >= 1.375 && neg.info.range < 1.7, "{}", neg.info.range);
        let json = serde_json::to_string(&neg.info).unwrap();
        assert_eq!(described_range(Some(&json)).unwrap(), neg.info.range);
        let path = dir.join("burnt.tif");
        write_gray16_range(
            &path,
            &neg.plane,
            neg.info.range,
            &TiffMeta { description: &json, trc: GrayTrc::Linear, orientation: 1, dpi: None },
        )
        .unwrap();
        let back = read_negative(&path).unwrap();
        let step = neg.info.range / 65535.0;
        for (a, b) in back.plane.data.iter().zip(&neg.plane.data) {
            assert!((a - b.max(0.0)).abs() <= 0.5 * step + 1e-12, "{a} vs {b}");
        }
        let plain = read_gray(&path).unwrap();
        assert_eq!(plain.plane.data.iter().copied().fold(0.0, f64::max), 1.0);
        assert!((plain.plane.at(48, 48) * neg.info.range - 1.375).abs() <= step);

        // Nothing above 1: no range in the description, the file as before.
        let calm = develop(false);
        assert!(calm.plane.data.iter().all(|&v| v <= 1.0));
        assert_eq!(calm.info.range, 1.0);
        let json = serde_json::to_string(&calm.info).unwrap();
        assert!(!json.contains("range"), "{json}");
        assert_eq!(described_range(Some(&json)).unwrap(), 1.0);
        let (a, b) = (dir.join("calm-range.tif"), dir.join("calm.tif"));
        write_gray16_range(&a, &calm.plane, calm.info.range, &meta("{}")).unwrap();
        write_gray16(&b, &calm.plane, &meta("{}")).unwrap();
        assert_eq!(std::fs::read(&a).unwrap(), std::fs::read(&b).unwrap());
        assert_eq!(read_negative(&b).unwrap().plane.data, read_gray(&b).unwrap().plane.data);

        // What a description may say.
        assert_eq!(described_range(None).unwrap(), 1.0);
        assert_eq!(described_range(Some("not json")).unwrap(), 1.0);
        assert_eq!(described_range(Some(r#"{"stage":"negative"}"#)).unwrap(), 1.0);
        assert_eq!(described_range(Some(r#"{"range":1.25}"#)).unwrap(), 1.25);
        for bad in [r#"{"range":0.5}"#, r#"{"range":"2"}"#, r#"{"range":null}"#, r#"{"range":-3}"#] {
            assert!(described_range(Some(bad)).is_err(), "{bad}");
        }
        assert!(write_gray16_range(&a, &calm.plane, 0.5, &meta("{}")).is_err());
        assert!(write_gray16_range(&a, &calm.plane, f64::NAN, &meta("{}")).is_err());
        // A plane with a value that is no number: the range is of the rest.
        let odd = Plane::from_vec(3, 1, vec![0.5, f64::NAN, 1.25]);
        assert_eq!(scene_range(&odd), 1.25);
        assert_eq!(scene_range(&Plane::from_vec(2, 1, vec![0.2, 0.9])), 1.0);
        std::fs::remove_dir_all(&dir).ok();
    }

    /// 255 in the sidecar says "burnt out" and nothing else. A mask of 1,
    /// which the block mask gives where a band holds nothing but noise, is
    /// the place sharpening is most allowed: written as 255 it was read as
    /// burnt out and left unsharpened.
    #[test]
    fn a_full_mask_is_not_written_as_burnt_out() {
        let mask = Plane::from_vec(5, 1, vec![1.0, 0.999, 0.8, 0.5, 1.0]);
        let bytes = mask_sidecar(&mask, &[0, 0, 0, 0, 1]).unwrap();
        assert_eq!(bytes, [254, 254, 204, 128, 255]);
        // Read back as the print reads it, the unsharp mask sharpens under
        // the full mask and under the fixed estimator's, and not where
        // the sensor was burnt out.
        let read = Plane::from_vec(5, 1, bytes.iter().map(|&b| f64::from(b) / 255.0).collect());
        let step = Plane::from_vec(5, 1, vec![0.2, 0.8, 0.2, 0.8, 0.2]);
        let sharp = crate::usm::unsharp(&step, 1.0, 1.0, Some(&read));
        for x in [0, 1, 2] {
            assert_ne!(sharp.at(x, 0), step.at(x, 0), "pixel {x} under mask {}", read.at(x, 0));
        }
        assert_eq!(sharp.at(3, 0), step.at(3, 0));
        assert_eq!(sharp.at(4, 0), step.at(4, 0));
    }

    #[test]
    fn a_short_saturation_list_is_an_error() {
        let mask = Plane::zeros(2, 2);
        let err = mask_sidecar(&mask, &[0, 1]).unwrap_err();
        assert!(err.to_string().contains("4"), "{err}");
        assert!(err.to_string().contains("2"), "{err}");
    }
}
