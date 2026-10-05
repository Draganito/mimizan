//! Calibration fits (SPEC sections 10 and 11): a look from a grey wedge, mix
//! weights from colour patches, star MTF into the camera file.

use crate::calib::{LookEncoding, LookFile};
use crate::cfa::Color;
use crate::curve::Curve;
use crate::decode::Rect;
use crate::ingest::Mosaic;
use crate::mix::Weights;
use crate::plane::Plane;
use serde::Serialize;

/// `cols × rows` patch rectangles inside `area`, each shrunk by `inset`
/// (fraction of the cell per side) so edges and chart borders stay out.
pub fn grid_rects(area: Rect, cols: usize, rows: usize, inset: f64) -> Vec<Rect> {
    let inset = inset.clamp(0.0, 0.49);
    let cw = area.w as f64 / cols.max(1) as f64;
    let ch = area.h as f64 / rows.max(1) as f64;
    let mut out = Vec::with_capacity(cols * rows);
    for r in 0..rows {
        for c in 0..cols {
            let x0 = area.x as f64 + (c as f64 + inset) * cw;
            let y0 = area.y as f64 + (r as f64 + inset) * ch;
            let w = (cw * (1.0 - 2.0 * inset)).floor().max(1.0);
            let h = (ch * (1.0 - 2.0 * inset)).floor().max(1.0);
            out.push(Rect { x: x0.round() as usize, y: y0.round() as usize, w: w as usize, h: h as usize });
        }
    }
    out
}

/// Per-colour means of a balanced mosaic rectangle, saturated photosites
/// excluded. Returns `(rgb, saturated fraction)`.
pub fn patch_rgb(mosaic: &Mosaic, r: Rect) -> ([f64; 3], f64) {
    let mut sum = [0.0f64; 3];
    let mut cnt = [0usize; 3];
    let mut sat = 0usize;
    let mut n = 0usize;
    let w = mosaic.plane.width;
    for y in r.y..(r.y + r.h).min(mosaic.plane.height) {
        for x in r.x..(r.x + r.w).min(w) {
            n += 1;
            if mosaic.saturated[y * w + x] != 0 {
                sat += 1;
                continue;
            }
            let c = match mosaic.phase {
                Some(ph) => ph.color_at(x, y) as usize,
                None => Color::G as usize,
            };
            sum[c] += mosaic.plane.at(x, y);
            cnt[c] += 1;
        }
    }
    let mut out = [0.0; 3];
    for c in 0..3 {
        out[c] = if cnt[c] > 0 { sum[c] / cnt[c] as f64 } else { f64::NAN };
    }
    if mosaic.phase.is_none() {
        out = [out[1]; 3];
    }
    (out, if n > 0 { sat as f64 / n as f64 } else { 0.0 })
}

/// Mean of a plane rectangle.
pub fn patch_mean(p: &Plane, r: Rect) -> f64 {
    crate::metrics::region_stats(p, r).0
}

// ---------------------------------------------------------------- look fit

#[derive(Clone, Debug, Serialize)]
pub struct LookFit {
    pub look: LookFile,
    /// Encoded input (after the encoding step) and reference per sample, sorted.
    pub samples: Vec<[f64; 2]>,
    /// Samples dropped as saturated or out of range.
    pub dropped: usize,
    /// Largest |curve(neg) − reference| over the samples, in 8-bit steps.
    pub max_err_255: f64,
    pub rms_err_255: f64,
    pub pass: bool,
}

/// Fit a look from `(negative linear, reference encoded)` pairs. Points are
/// sorted, near-duplicates merged, forced monotone (pool adjacent violators)
/// and anchored at (0,0) and (1,1). Pass: ≤ 3/255 on the fitted samples.
pub fn fit_look(
    name: &str,
    pairs: &[(f64, f64)],
    encoding: LookEncoding,
    fitted_against: Option<String>,
) -> LookFit {
    let enc = |v: f64| match encoding {
        LookEncoding::Gamma22 => v.clamp(0.0, 1.0).powf(1.0 / 2.2),
        LookEncoding::None => v.clamp(0.0, 1.0),
    };
    let mut pts: Vec<[f64; 2]> = pairs
        .iter()
        .filter(|(n, r)| n.is_finite() && r.is_finite() && *n > 0.0 && *n < 1.0 && (0.0..=1.0).contains(r))
        .map(|&(n, r)| [enc(n), r])
        .collect();
    let dropped = pairs.len() - pts.len();
    pts.sort_by(|a, b| a[0].total_cmp(&b[0]));

    // Merge x within 1e-3 (same wedge step measured twice).
    let mut merged: Vec<([f64; 2], usize)> = Vec::new();
    for p in &pts {
        match merged.last_mut() {
            Some((q, k)) if p[0] - q[0] < 1e-3 => {
                let kf = *k as f64;
                q[0] = (q[0] * kf + p[0]) / (kf + 1.0);
                q[1] = (q[1] * kf + p[1]) / (kf + 1.0);
                *k += 1;
            }
            _ => merged.push((*p, 1)),
        }
    }
    let mut ys: Vec<f64> = merged.iter().map(|(p, _)| p[1]).collect();
    let ws: Vec<f64> = merged.iter().map(|(_, k)| *k as f64).collect();
    pava(&mut ys, &ws);
    let mut points: Vec<[f64; 2]> = merged.iter().zip(&ys).map(|((p, _), y)| [p[0], *y]).collect();

    if points.first().is_none_or(|p| p[0] > 0.0) {
        points.insert(0, [0.0, 0.0]);
    }
    if points.last().is_none_or(|p| p[0] < 1.0) {
        points.push([1.0, 1.0]);
    }
    // After anchoring, y must still be monotone (anchor y = 0 / 1 always is).
    let look = LookFile { schema: 1, name: name.to_string(), points, encoding, fitted_against };
    let curve = Curve::from_look(&look);
    let errs: Vec<f64> = pairs
        .iter()
        .filter(|(n, r)| n.is_finite() && r.is_finite() && *n > 0.0 && *n < 1.0 && (0.0..=1.0).contains(r))
        .map(|&(n, r)| (curve.eval(n) - r).abs() * 255.0)
        .collect();
    let max_err_255 = errs.iter().copied().fold(0.0, f64::max);
    let rms_err_255 = if errs.is_empty() {
        0.0
    } else {
        (errs.iter().map(|e| e * e).sum::<f64>() / errs.len() as f64).sqrt()
    };
    LookFit { look, samples: pts, dropped, max_err_255, rms_err_255, pass: max_err_255 <= 3.0 }
}

/// Weighted isotonic regression (non-decreasing), pool adjacent violators.
pub fn pava(y: &mut [f64], w: &[f64]) {
    let n = y.len();
    if n < 2 {
        return;
    }
    // Blocks: (value, weight, count)
    let mut blocks: Vec<(f64, f64, usize)> = Vec::with_capacity(n);
    for i in 0..n {
        blocks.push((y[i], w[i], 1));
        while blocks.len() >= 2 {
            let m = blocks.len();
            if blocks[m - 2].0 <= blocks[m - 1].0 {
                break;
            }
            let (v1, w1, c1) = blocks[m - 2];
            let (v2, w2, c2) = blocks[m - 1];
            let w = w1 + w2;
            blocks.truncate(m - 2);
            blocks.push(((v1 * w1 + v2 * w2) / w, w, c1 + c2));
        }
    }
    let mut i = 0;
    for (v, _, c) in blocks {
        for _ in 0..c {
            y[i] = v;
            i += 1;
        }
    }
}

// ------------------------------------------------------------- weights fit

#[derive(Clone, Debug, Serialize)]
pub struct WeightsFit {
    pub weights: Weights,
    /// Exposure scale between the mix and the reference (free parameter).
    pub scale: f64,
    /// RMSE of the fitted brightness relative to the mean reference brightness.
    pub rmse_rel: f64,
    pub max_rel: f64,
    /// Relative residual per patch, (fit − ref) / mean(ref).
    pub residuals: Vec<f64>,
    pub n: usize,
    pub pass: bool,
}

fn eval_weights(samples: &[([f64; 3], f64)], w: [f64; 3]) -> (f64, f64) {
    let (mut sm, mut smr) = (0.0, 0.0);
    for (rgb, r) in samples {
        let m = w[0] * rgb[0] + w[1] * rgb[1] + w[2] * rgb[2];
        sm += m * m;
        smr += m * r;
    }
    let s = if sm > 0.0 { smr / sm } else { 0.0 };
    let sse: f64 = samples
        .iter()
        .map(|(rgb, r)| {
            let m = s * (w[0] * rgb[0] + w[1] * rgb[1] + w[2] * rgb[2]);
            (m - r) * (m - r)
        })
        .sum();
    (sse, s)
}

/// Fit non-negative weights summing to 1 so that `scale · (w·rgb)` matches the
/// linear reference brightness. Exhaustive simplex grid (step 0.01) refined to
/// 0.001: deterministic, no local minima. Pass: RMSE ≤ 5 % of the mean
/// reference brightness.
pub fn fit_weights(samples: &[([f64; 3], f64)]) -> WeightsFit {
    let samples: Vec<([f64; 3], f64)> = samples
        .iter()
        .filter(|(rgb, r)| rgb.iter().all(|v| v.is_finite()) && r.is_finite() && *r > 0.0)
        .copied()
        .collect();
    let n = samples.len();
    let mean_ref = samples.iter().map(|s| s.1).sum::<f64>() / n.max(1) as f64;
    let mut best = ([0.25, 0.5, 0.25], f64::INFINITY, 1.0);
    if n > 0 {
        let try_w = |wr: f64, wb: f64, best: &mut ([f64; 3], f64, f64)| {
            let wg = 1.0 - wr - wb;
            if wg < -1e-12 {
                return;
            }
            let w = [wr, wg.max(0.0), wb];
            let (sse, s) = eval_weights(&samples, w);
            if sse < best.1 {
                *best = (w, sse, s);
            }
        };
        for i in 0..=100 {
            for j in 0..=(100 - i) {
                try_w(i as f64 / 100.0, j as f64 / 100.0, &mut best);
            }
        }
        let (cr, cb) = (best.0[0], best.0[2]);
        for i in -10..=10 {
            for j in -10..=10 {
                let wr = cr + i as f64 / 1000.0;
                let wb = cb + j as f64 / 1000.0;
                if wr >= 0.0 && wb >= 0.0 && wr + wb <= 1.0 + 1e-12 {
                    try_w(wr, wb, &mut best);
                }
            }
        }
    }
    let (w, sse, s) = best;
    let sum = w[0] + w[1] + w[2];
    let weights = Weights { r: w[0] / sum, g: w[1] / sum, b: w[2] / sum };
    let residuals: Vec<f64> = samples
        .iter()
        .map(|(rgb, r)| (s * (w[0] * rgb[0] + w[1] * rgb[1] + w[2] * rgb[2]) - r) / mean_ref)
        .collect();
    let rmse_rel = if n > 0 { (sse / n as f64).sqrt() / mean_ref } else { f64::NAN };
    let max_rel = residuals.iter().map(|r| r.abs()).fold(0.0, f64::max);
    WeightsFit { weights, scale: s, rmse_rel, max_rel, residuals, n, pass: n >= 3 && rmse_rel <= 0.05 }
}

// ----------------------------------------------------- reference encodings

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RefEncoding {
    Srgb,
    Gamma22,
    Linear,
}

impl RefEncoding {
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "srgb" => Some(Self::Srgb),
            "gamma22" | "gamma2.2" | "2.2" => Some(Self::Gamma22),
            "none" | "linear" => Some(Self::Linear),
            _ => None,
        }
    }

    /// Encoded [0,1] -> linear [0,1].
    pub fn to_linear(self, v: f64) -> f64 {
        let v = v.clamp(0.0, 1.0);
        match self {
            Self::Srgb => {
                if v <= 0.04045 {
                    v / 12.92
                } else {
                    ((v + 0.055) / 1.055).powf(2.4)
                }
            }
            Self::Gamma22 => v.powf(2.2),
            Self::Linear => v,
        }
    }
}

// ------------------------------------------------------------- self tests

#[derive(Clone, Debug, Serialize)]
pub struct WedgeSelfTest {
    pub fit_max_err_255: f64,
    /// Error on levels between the wedge steps (held out), in 8-bit steps.
    pub holdout_max_err_255: f64,
    pub linearity_slope: f64,
    pub linearity_max_rel_residual: f64,
    pub pass: bool,
}

/// A plausible out-of-camera tone curve for the synthetic reference: gamma 2.2
/// with a gentle S (shoulder and toe).
pub fn synthetic_camera_curve(linear: f64) -> f64 {
    let g = linear.clamp(0.0, 1.0).powf(1.0 / 2.2);
    // Smoothstep blend keeps the ends at 0 and 1 and the middle slightly steeper.
    let s = g * g * (3.0 - 2.0 * g);
    (0.7 * g + 0.3 * s).clamp(0.0, 1.0)
}

/// Synthetic wedge: run the real separation on the `Wedge` scene, read patch
/// means, build the reference from the ground truth through
/// `synthetic_camera_curve`, fit, and check held-out mid-levels.
pub fn wedge_self_test(size: usize, with_noise: bool) -> WedgeSelfTest {
    use crate::separate::{separate, SeparateParams};
    use crate::synth::{self, Scene, SynthParams};
    let sp =
        SynthParams { noise: if with_noise { Some(Default::default()) } else { None }, ..Default::default() };
    let s = synth::mosaic(Scene::Wedge, size, size, &sp);
    let sep = separate(&s.mosaic, &SeparateParams::default());
    let steps = 16;
    let rects = grid_rects(Rect { x: 0, y: 0, w: size - size % steps, h: size }, steps, 1, 0.3);
    let pairs: Vec<(f64, f64)> = rects
        .iter()
        .map(|r| (patch_mean(&sep.lum, *r), synthetic_camera_curve(patch_mean(&s.l_true, *r))))
        .collect();
    let fit = fit_look("selftest", &pairs, LookEncoding::Gamma22, None);
    let curve = Curve::from_look(&fit.look);
    let mut holdout = 0.0f64;
    for i in 0..steps - 1 {
        let a = synth::wedge_level(i, steps);
        let b = synth::wedge_level(i + 1, steps);
        let mid = (a * b).sqrt();
        holdout = holdout.max((curve.eval(mid) - synthetic_camera_curve(mid)).abs() * 255.0);
    }
    let lin: Vec<(f64, f64)> = (0..steps).map(|i| (synth::wedge_level(i, steps) / 0.9, pairs[i].0)).collect();
    let wf = crate::metrics::wedge_fit(&lin);
    WedgeSelfTest {
        fit_max_err_255: fit.max_err_255,
        holdout_max_err_255: holdout,
        linearity_slope: wf.slope,
        linearity_max_rel_residual: wf.max_rel_residual,
        pass: fit.pass && holdout <= 3.0 && wf.pass,
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct WeightsSelfTest {
    pub truth: [f64; 3],
    pub fitted: [f64; 3],
    pub max_abs_dev: f64,
    pub rmse_rel: f64,
    pub pass: bool,
}

/// Synthetic patches: balanced patch means from the mosaic, reference brightness
/// `w_true · rgb` with an arbitrary exposure factor; the fit must recover `w_true`.
pub fn weights_self_test(size: usize, truth: [f64; 3], with_noise: bool) -> WeightsSelfTest {
    use crate::synth::{self, Scene, SynthParams};
    let sp =
        SynthParams { noise: if with_noise { Some(Default::default()) } else { None }, ..Default::default() };
    let s = synth::mosaic(Scene::Patches, size, size, &sp);
    let (cols, rows) = (6, 4);
    let rects =
        grid_rects(Rect { x: 0, y: 0, w: size - size % cols, h: size - size % rows }, cols, rows, 0.3);
    let samples: Vec<([f64; 3], f64)> = rects
        .iter()
        .map(|r| {
            let (rgb, _) = patch_rgb(&s.mosaic, *r);
            let t = [patch_mean(&s.rgb.r, *r), patch_mean(&s.rgb.g, *r), patch_mean(&s.rgb.b, *r)];
            (rgb, 0.37 * (truth[0] * t[0] + truth[1] * t[1] + truth[2] * t[2]))
        })
        .collect();
    let f = fit_weights(&samples);
    let fitted = [f.weights.r, f.weights.g, f.weights.b];
    let max_abs_dev = (0..3).map(|i| (fitted[i] - truth[i]).abs()).fold(0.0, f64::max);
    WeightsSelfTest { truth, fitted, max_abs_dev, rmse_rel: f.rmse_rel, pass: f.pass && max_abs_dev <= 0.01 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_rects_stay_inside_and_inset() {
        let r = grid_rects(Rect { x: 10, y: 20, w: 600, h: 400 }, 6, 4, 0.25);
        assert_eq!(r.len(), 24);
        assert_eq!(r[0], Rect { x: 35, y: 45, w: 50, h: 50 });
        for q in &r {
            assert!(q.x + q.w <= 610 && q.y + q.h <= 420);
        }
    }

    #[test]
    fn pava_pools_violators() {
        let mut y = vec![1.0, 3.0, 2.0, 4.0];
        pava(&mut y, &[1.0; 4]);
        assert_eq!(y, vec![1.0, 2.5, 2.5, 4.0]);
        let mut z = vec![0.1, 0.2, 0.3];
        pava(&mut z, &[1.0; 3]);
        assert_eq!(z, vec![0.1, 0.2, 0.3]);
    }

    #[test]
    fn look_fit_recovers_a_known_curve() {
        let pairs: Vec<(f64, f64)> =
            (1..30).map(|i| i as f64 / 30.0).map(|l| (l, synthetic_camera_curve(l))).collect();
        let f = fit_look("t", &pairs, LookEncoding::Gamma22, None);
        assert!(f.pass, "{f:?}");
        let c = Curve::from_look(&f.look);
        for i in 1..60 {
            let l = i as f64 / 60.0;
            assert!((c.eval(l) - synthetic_camera_curve(l)).abs() * 255.0 < 1.0, "{l}");
        }
        f.look.validate().unwrap();
    }

    #[test]
    fn look_fit_tolerates_noise_and_duplicates() {
        let mut pairs: Vec<(f64, f64)> = Vec::new();
        for i in 1..16 {
            let l = i as f64 / 16.0;
            let r = synthetic_camera_curve(l);
            pairs.push((l, r + 0.004 * if i % 2 == 0 { 1.0 } else { -1.0 }));
            pairs.push((l * 1.0001, r));
        }
        // Mild violator: below the 7/16 reference, so PAVA has to pool.
        pairs.push((0.5, synthetic_camera_curve(7.0 / 16.0) - 0.01));
        let f = fit_look("t", &pairs, LookEncoding::Gamma22, None);
        f.look.validate().unwrap();
        let c = Curve::from_look(&f.look);
        for i in 1..16 {
            let l = i as f64 / 16.0;
            let e = (c.eval(l) - synthetic_camera_curve(l)).abs() * 255.0;
            // The contaminated level itself is averaged with the bad sample (≈4/255);
            // its neighbours must stay clean.
            let limit = if i == 8 { 6.0 } else { 3.0 };
            assert!(e < limit, "l {l}: {e}");
        }
    }

    #[test]
    fn weights_fit_recovers_truth_without_noise() {
        let r = weights_self_test(384, [0.30, 0.50, 0.20], false);
        assert!(r.pass, "{r:?}");
        assert!(r.max_abs_dev <= 0.002, "{r:?}");
    }

    #[test]
    fn weights_fit_recovers_truth_with_noise() {
        let r = weights_self_test(384, [0.60, 0.30, 0.10], true);
        assert!(r.pass, "{r:?}");
    }

    #[test]
    fn wedge_self_test_passes() {
        let r = wedge_self_test(512, true);
        assert!(r.pass, "{r:?}");
    }

    #[test]
    fn reference_encodings() {
        assert!((RefEncoding::Srgb.to_linear(0.5) - 0.2140).abs() < 1e-3);
        assert!((RefEncoding::Gamma22.to_linear(0.5) - 0.2176).abs() < 1e-3);
        assert_eq!(RefEncoding::Linear.to_linear(0.5), 0.5);
        assert_eq!(RefEncoding::parse("sRGB"), Some(RefEncoding::Srgb));
    }
}
