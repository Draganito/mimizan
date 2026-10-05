//! Develop a frame either with the Mimizan separation or with one demosaicer.
//!
//! The separation path is `pipeline::negative_from_frame`, unchanged. A
//! demosaicer runs on the same ingested mosaic, then its red, green and blue
//! are stored as the separation the mixer already understands:
//! `L = (R + 2G + B) / 4`, `C1 = (2G − R − B) / 4`, `C2 = (R − B) / 4`.
//! With those, `mix` is `wR·R + wG·G + wB·B`, and the native weights return
//! `L`. Weight sliders, filters and the look therefore keep working, and the
//! separation code is not involved.

use anyhow::{Context, Result};
use mimizan_core::cfa::{BayerPhase, Color};
use mimizan_core::decode::{RawFrame, SensorKind};
use mimizan_core::ingest::ingest;
use mimizan_core::mix::mix;
use mimizan_core::pipeline::{Negative, NegativeInfo, NegativeParams, Timing};
use mimizan_core::plane::Plane;
use mimizan_core::separate::{Separation, FIXED_MASK_VALUE};
use mimizan_demosaic::{self, Method};
use std::time::Instant;

use crate::session::Converter;

pub fn develop(
    frame: &RawFrame,
    p: &NegativeParams,
    decode_ms: u128,
    converter: Converter,
) -> Result<Negative> {
    let phase = match (converter, frame.kind) {
        (Converter::Mimizan, _) => {
            return Ok(mimizan_core::pipeline::negative_from_frame(frame, p, decode_ms)?)
        }
        (_, SensorKind::Bayer(phase)) => phase,
        (_, SensorKind::Mono) => {
            return Ok(mimizan_core::pipeline::negative_from_frame(frame, p, decode_ms)?);
        }
    };
    if let Some(cam) = &p.camera_file {
        if cam.phase() != phase {
            anyhow::bail!("camera file says {} but the file is {}", cam.bayer_phase, phase);
        }
    }
    p.weights.validate()?;

    let t = Instant::now();
    let mosaic = ingest(frame, &p.ingest);
    let ingest_ms = t.elapsed().as_millis();

    let weights = p.weights.to_balanced(p.weights_space, mosaic.wb);
    weights.validate()?;
    let weights = weights.filtered(p.filter);

    let method = match converter {
        Converter::Mimizan => unreachable!("handled above"),
        Converter::Amaze => Method::Amaze,
        Converter::Rcd => Method::Rcd,
        Converter::Dcb => Method::Dcb,
        Converter::Lmmse => Method::Lmmse,
    };
    let (w, h) = (mosaic.plane.width, mosaic.plane.height);
    let cfa = cfa_codes(phase);
    let t = Instant::now();
    let rgb = mimizan_demosaic::demosaic(method, w, h, &mosaic.plane.data, cfa)
        .with_context(|| format!("demosaicing with {}", method.label()))?;
    let separate_ms = t.elapsed().as_millis();
    let sep = separation_from_rgb(&rgb.r, &rgb.g, &rgb.b, w, h, phase);

    let mut info = NegativeInfo {
        mimizan: env!("CARGO_PKG_VERSION"),
        stage: "negative",
        camera: mosaic.camera.clone(),
        phase: Some(phase.name().to_string()),
        noise: mosaic.noise,
        wb: mosaic.wb,
        weights,
        filter: p.filter,
        weights_raw: weights.balanced_to_raw(mosaic.wb),
        mask: p.separate.mask,
        rounds: 0,
        cutoff: p.separate.cutoff,
        reconstruct: false,
        computed: 0.0,
        demosaic: Some(method.label().to_string()),
        report: mosaic.report.clone(),
        timing: Timing { decode_ms, ingest_ms, separate_ms, mix_ms: 0 },
    };
    let orientation = mosaic.orientation;
    let saturated = mosaic.saturated;
    let allowed = (FIXED_MASK_VALUE * 255.0).round() as u8;
    let mask8 = saturated.iter().map(|&s| if s != 0 { 255 } else { allowed }).collect();
    let t = Instant::now();
    let plane = mix(&sep, &weights);
    info.timing.mix_ms = t.elapsed().as_millis();
    let separation = if p.keep_separation { Some(sep) } else { None };
    Ok(Negative { plane, mask8, orientation, info, saturated, separation })
}

fn cfa_codes(phase: BayerPhase) -> [[u32; 2]; 2] {
    let tile = phase.tile();
    let code = |c: Color| match c {
        Color::R => 0,
        Color::G => 1,
        Color::B => 2,
    };
    [[code(tile[0]), code(tile[1])], [code(tile[2]), code(tile[3])]]
}

/// `L`, `C1`, `C2` such that the existing mix equals a weighted sum of the
/// demosaiced channels. See the module comment.
fn separation_from_rgb(r: &[f64], g: &[f64], b: &[f64], w: usize, h: usize, phase: BayerPhase) -> Separation {
    let n = w * h;
    let mut lum = Vec::with_capacity(n);
    let mut c1 = Vec::with_capacity(n);
    let mut c2 = Vec::with_capacity(n);
    for i in 0..n {
        let (rr, gg, bb) = (r[i], g[i], b[i]);
        lum.push((rr + 2.0 * gg + bb) * 0.25);
        c1.push((2.0 * gg - rr - bb) * 0.25);
        c2.push((rr - bb) * 0.25);
    }
    Separation {
        lum: Plane::from_vec(w, h, lum),
        c1: Plane::from_vec(w, h, c1),
        c2: Plane::from_vec(w, h, c2),
        mask_max: Plane::from_vec(w, h, vec![FIXED_MASK_VALUE; n]),
        rounds: 0,
        phase,
        computed: 0.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use mimizan_core::mix::{mix, Weights};

    #[test]
    fn mix_of_stored_channels_is_the_weighted_sum() {
        let (r, g, b) = (0.2, 0.5, 0.8);
        let sep = separation_from_rgb(&[r], &[g], &[b], 1, 1, BayerPhase::RGGB);
        let w = Weights { r: 0.1, g: 0.2, b: 0.7 };
        let out = mix(&sep, &w);
        let expect = 0.1 * r + 0.2 * g + 0.7 * b;
        assert!((out.data[0] - expect).abs() < 1e-12);
        let native = mix(&sep, &Weights::default());
        assert!((native.data[0] - (r + 2.0 * g + b) / 4.0).abs() < 1e-12);
    }

    /// The wrapper against the published pixel-sharp field, on kodim01.
    /// Skips when the local Kodak files are absent.
    #[test]
    fn kodim01_matches_the_field() {
        let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let dng = root.join("testdata/kodak-pixelsharp/kodim01.dng");
        let png = root.join("testdata/kodak/kodim01.png");
        if !dng.is_file() || !png.is_file() {
            return;
        }
        let frame = mimizan_core::pipeline::decode(&dng).unwrap();
        let mut params = mimizan_core::pipeline::NegativeParams::default();
        params.ingest.fix_defects = false;
        params.ingest.wb = mimizan_core::ingest::WhiteBalance::None;
        let truth = mimizan_core::kodak::luminance(&mimizan_core::kodak::load_srgb8(&png).unwrap());
        // Published RawTherapee luminance PSNR. The bench reads RT's 16-bit
        // sRGB file and inverts the curve, so a direct linear result can sit
        // a little higher.
        let expect = [
            (Converter::Amaze, 38.38),
            (Converter::Rcd, 35.01),
            (Converter::Dcb, 36.66),
            (Converter::Lmmse, 41.43),
        ];
        for (converter, published) in expect {
            let neg = develop(&frame, &params, 0, converter).unwrap();
            let q = mimizan_core::kodak::quality(&neg.plane, &truth, 16);
            let gap = (q.psnr_db - published).abs();
            assert!(gap < 1.2, "{converter:?} {q:.2} dB, published {published:.2}", q = q.psnr_db);
        }
    }
}
