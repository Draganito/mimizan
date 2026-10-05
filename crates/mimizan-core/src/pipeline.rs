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
}

impl NegativeParams {
    /// Apply a camera file: noise, weights (unless overridden later), USM is for print.
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

#[derive(Clone, Debug, Serialize)]
pub struct Timing {
    pub decode_ms: u128,
    pub ingest_ms: u128,
    pub separate_ms: u128,
    pub mix_ms: u128,
}

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
    pub rounds: usize,
    pub cutoff: f64,
    /// Nonlinear reconstruction was enabled (`reconstruct.rs`).
    pub reconstruct: bool,
    /// Mean β of that reconstruction: fraction of the picture that was
    /// computed from a direction decision instead of filtered. 0 when off.
    pub computed: f64,
    /// Set when this negative was mixed from a demosaicer instead of the
    /// separation. Absent on the Mimizan path, which does not call one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub demosaic: Option<String>,
    pub report: IngestReport,
    pub timing: Timing,
}

pub struct Negative {
    pub plane: Plane,
    /// 8-bit sidecar: `round(255*mask_max)`, 255 where saturated.
    pub mask8: Vec<u8>,
    pub orientation: u16,
    pub info: NegativeInfo,
    /// Saturation map of the mosaic (same size as `plane`).
    pub saturated: Vec<u8>,
    /// Only with `NegativeParams::keep_separation`.
    pub separation: Option<crate::separate::Separation>,
}

#[cfg(feature = "decode-rawler")]
pub fn decode(path: &Path) -> Result<RawFrame> {
    RawlerDecoder.decode(path)
}

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
    let mosaic = ingest(frame, &p.ingest);
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
        reconstruct: p.separate.reconstruct.is_some(),
        computed: 0.0,
        demosaic: None,
        report: mosaic.report.clone(),
        timing: Timing { decode_ms, ingest_ms, separate_ms: 0, mix_ms: 0 },
    };
    let orientation = mosaic.orientation;

    let (plane, mask8, saturated, separation) = match mosaic.phase {
        Some(_) => {
            let t = Instant::now();
            let sep = separate(&mosaic, &p.separate);
            info.timing.separate_ms = t.elapsed().as_millis();
            info.rounds = sep.rounds;
            info.computed = sep.computed;
            info!("separate {} ms, {} rounds", info.timing.separate_ms, sep.rounds);
            if info.reconstruct {
                info!("reconstruct: {:.1} % of the picture computed", 100.0 * sep.computed);
            }
            // The mosaic plane is no longer needed: free it before mixing.
            let Mosaic { saturated, plane: mosaic_plane, .. } = mosaic;
            drop(mosaic_plane);
            let mask8 = mask_sidecar(&sep.mask_max, &saturated);
            let t = Instant::now();
            let (plane, separation) = if p.keep_separation {
                (mix(&sep, &weights), Some(sep))
            } else {
                (mix_into(sep, &weights), None)
            };
            info.timing.mix_ms = t.elapsed().as_millis();
            (plane, mask8, saturated, separation)
        }
        None => {
            let Mosaic { saturated, plane, .. } = mosaic;
            // No carriers on a mono sensor: sharpening allowed except at saturation.
            let allowed = (crate::separate::FIXED_MASK_VALUE * 255.0).round() as u8;
            let mask8 = saturated.iter().map(|&s| if s != 0 { 255 } else { allowed }).collect();
            (plane, mask8, saturated, None)
        }
    };
    Ok(Negative { plane, mask8, orientation, info, saturated, separation })
}

#[cfg(feature = "decode-rawler")]
pub fn negative(path: &Path, p: &NegativeParams) -> Result<Negative> {
    let t = Instant::now();
    let frame = decode(path)?;
    let decode_ms = t.elapsed().as_millis();
    info!("decode {} ms", decode_ms);
    negative_from_frame(&frame, p, decode_ms)
}

pub fn mask_sidecar(mask_max: &Plane, saturated: &[u8]) -> Vec<u8> {
    mask_max
        .data
        .par_iter()
        .zip(saturated.par_iter())
        .map(|(&m, &s)| if s != 0 { 255 } else { (m.clamp(0.0, 1.0) * 255.0).round() as u8 })
        .collect()
}
