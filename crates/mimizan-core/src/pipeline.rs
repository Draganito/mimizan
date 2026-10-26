//! RAW -> negative, the one path both CLI and GUI call.

use crate::calib::CameraFile;
use crate::decode::RawFrame;
#[cfg(feature = "decode-rawler")]
use crate::decode::{RawDecoder, RawlerDecoder};
use crate::error::Result;
use crate::ingest::{ingest, IngestParams, IngestReport, Mosaic, NoiseModel};
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
    /// Present when the file is a three-colour plate (`plate.rs`): frame
    /// order, frame height, the filter set and its weights.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plate: Option<crate::plate::PlateInfo>,
}

/// The developed negative: the linear plane, its sharpening mask, and the
/// description.
pub struct Negative {
    pub plane: Plane,
    /// 8-bit sidecar: `round(255*mask_max)`, 255 where saturated.
    pub mask8: Vec<u8>,
    pub orientation: u16,
    pub info: NegativeInfo,
    /// Saturation map of the mosaic (same size as `plane`).
    pub saturated: Vec<u8>,
    /// Only with `NegativeParams::keep_separation`. Its `mask_max` is
    /// released (a 1×1 plane): `mask8` is the mask.
    pub separation: Option<crate::separate::Separation>,
}

/// Read a RAW file with `rawler`.
#[cfg(feature = "decode-rawler")]
pub fn decode(path: &Path) -> Result<RawFrame> {
    RawlerDecoder.decode(path)
}

/// Develop a decoded frame: ingest, separate (Bayer only), mix. `decode_ms`
/// is recorded in the timing.
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

    let (plane, mask8, saturated, separation) = match mosaic.phase {
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
            (plane, mask8, saturated, separation)
        }
        None => {
            let Mosaic { saturated, plane, .. } = mosaic;
            // The measurement is the picture; the colour mask was not run.
            // No carriers on a mono sensor: sharpening allowed except at saturation.
            let allowed = (crate::separate::FIXED_MASK_VALUE * 255.0).round() as u8;
            let mask8 = saturated.iter().map(|&s| if s != 0 { 255 } else { allowed }).collect();
            (plane, mask8, saturated, None)
        }
    };
    Ok(Negative { plane, mask8, orientation, info, saturated, separation })
}

/// Decode and develop one file.
#[cfg(feature = "decode-rawler")]
pub fn negative(path: &Path, p: &NegativeParams) -> Result<Negative> {
    let t = Instant::now();
    let frame = decode(path)?;
    let decode_ms = t.elapsed().as_millis();
    info!("decode {} ms", decode_ms);
    negative_from_frame(&frame, p, decode_ms)
}

/// The panchromatic emulsion behind the plate's Wratten glass, on the
/// balanced channels: the camera file's calibrated response when there is
/// one, else the native 0.25 / 0.50 / 0.25. A fact of the file, not a
/// choice; the mix is for one negative and does not reach the plate.
pub fn plate_base(camera_file: Option<&CameraFile>, wb: [f64; 3]) -> Weights {
    camera_file.map(|c| c.weights_balanced(wb)).unwrap_or_default()
}

/// The 8-bit sidecar: `round(255 · mask_max)`, 255 where saturated.
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
        .map(|(&m, &s)| if s != 0 { 255 } else { (m.clamp(0.0, 1.0) * 255.0).round() as u8 })
        .collect())
}

/// Bytes a development of a `w×h` frame holds at its widest point. Bayer:
/// the Dubois estimate at its peak, nine f64 planes (`dubois::estimate`),
/// over the 16-bit frame and the saturation map. Mono: the normalised
/// plane beside the measured one, over the same two. A guide for refusing
/// a file the machine cannot hold, measured to about a tenth; what it does
/// not see is the allocator and whatever else the process has open.
pub fn develop_memory(w: usize, h: usize, kind: crate::decode::SensorKind) -> u64 {
    let px = w as u64 * h as u64;
    let planes = match kind {
        crate::decode::SensorKind::Bayer(_) => 9,
        crate::decode::SensorKind::Mono => 2,
    };
    px * (planes * 8 + 2 + 1)
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
    }

    #[test]
    fn a_short_saturation_list_is_an_error() {
        let mask = Plane::zeros(2, 2);
        let err = mask_sidecar(&mask, &[0, 1]).unwrap_err();
        assert!(err.to_string().contains("4"), "{err}");
        assert!(err.to_string().contains("2"), "{err}");
    }
}
