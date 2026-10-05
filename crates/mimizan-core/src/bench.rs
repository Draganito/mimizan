//! The measurement bench: synthetic scene -> estimator -> numbers (SPEC section 8).

use crate::baseline::bilinear_luminance;
use crate::decode::Rect;
use crate::metrics::{
    carrier_energy, error_stats, region_stats, star_mtf, wedge_fit, CarrierReport, ErrorStats, StarReport,
    WedgeFit,
};
use crate::separate::{separate, SeparateParams};
use crate::synth::{mosaic, wedge_level, Scene, SynthParams, Synthetic, STAR_SPOKES};
use serde::Serialize;
use std::time::Instant;

#[derive(Clone, Debug, Serialize)]
pub struct EstimatorResult {
    pub name: String,
    pub error: ErrorStats,
    /// Error relative to the mean noise sigma of the truth (SPEC: ≤ 1.5 sigma on neutral scenes).
    pub rmse_over_sigma: f64,
    pub star: Option<StarReport>,
    pub carrier_flat_max: Option<f64>,
    pub carrier_per_patch: Option<Vec<CarrierReport>>,
    /// Error restricted to patch interiors (16 px inset), i.e. without the colour edges.
    pub interior_error: Option<ErrorStats>,
    pub wedge: Option<WedgeFit>,
    pub ms: u128,
    pub rounds: usize,
}

#[derive(Clone, Debug, Serialize)]
pub struct SceneResult {
    pub scene: Scene,
    pub width: usize,
    pub height: usize,
    pub synth: SynthParams,
    pub separate: SeparateParams,
    pub truth_star: Option<StarReport>,
    pub mimizan: EstimatorResult,
    pub baseline: EstimatorResult,
}

fn mean_sigma(s: &Synthetic) -> f64 {
    let nm = s.mosaic.noise;
    let n = s.l_true.data.len() as f64;
    s.l_true.data.iter().map(|&v| nm.sigma(v)).sum::<f64>() / n
}

fn patch_rects(w: usize, h: usize, cols: usize, rows: usize, inset: usize) -> Vec<Rect> {
    let pw = w / cols;
    let ph = h / rows;
    let mut v = Vec::new();
    for r in 0..rows {
        for c in 0..cols {
            v.push(Rect { x: c * pw + inset, y: r * ph + inset, w: pw - 2 * inset, h: ph - 2 * inset });
        }
    }
    v
}

fn evaluate(
    name: &str,
    scene: Scene,
    s: &Synthetic,
    est: &crate::plane::Plane,
    rounds: usize,
    ms: u128,
) -> EstimatorResult {
    let (w, h) = (est.width, est.height);
    let border = 40;
    let error = error_stats(est, &s.l_true, border);
    let sigma = mean_sigma(s);
    let mut r = EstimatorResult {
        name: name.into(),
        error,
        rmse_over_sigma: error.rmse / sigma.max(1e-12),
        star: None,
        carrier_flat_max: None,
        carrier_per_patch: None,
        interior_error: None,
        wedge: None,
        ms,
        rounds,
    };
    let carrier_max = |reps: &[CarrierReport]| {
        reps.iter().flat_map(|c| c.ratio.iter().cloned()).filter(|v| v.is_finite()).fold(0.0, f64::max)
    };
    match scene {
        Scene::Star => {
            r.star = Some(star_mtf(
                est,
                w as f64 / 2.0,
                h as f64 / 2.0,
                STAR_SPOKES,
                16.0,
                0.44 * w.min(h) as f64,
            ));
        }
        Scene::Patches => {
            let rects = patch_rects(w, h, 6, 4, 8);
            let reps: Vec<CarrierReport> =
                rects.iter().map(|&rc| carrier_energy(est, &s.mosaic.noise, Some(rc), 0.15)).collect();
            r.carrier_flat_max = Some(carrier_max(&reps));
            r.carrier_per_patch = Some(reps);
            r.interior_error = Some(interior_stats(est, &s.l_true, &patch_rects(w, h, 6, 4, 16)));
        }
        Scene::Wedge => {
            let steps = 16;
            let rects = patch_rects(w, h, steps, 1, 8);
            let samples: Vec<(f64, f64)> = rects
                .iter()
                .enumerate()
                .map(|(i, &rc)| (wedge_level(i, steps), region_stats(est, rc).0))
                .collect();
            r.wedge = Some(wedge_fit(&samples));
            // Carrier blocks need 64 px: use the full patch width (1024/16 = 64).
            let full = patch_rects(w, h, steps, 1, 0);
            let reps: Vec<CarrierReport> =
                full.iter().map(|&rc| carrier_energy(est, &s.mosaic.noise, Some(rc), 0.15)).collect();
            r.carrier_flat_max = Some(carrier_max(&reps));
            r.carrier_per_patch = Some(reps);
            r.interior_error = Some(interior_stats(est, &s.l_true, &patch_rects(w, h, steps, 1, 16)));
        }
        _ => {}
    }
    r
}

fn interior_stats(est: &crate::plane::Plane, truth: &crate::plane::Plane, rects: &[Rect]) -> ErrorStats {
    let mut errs = Vec::new();
    let mut s2 = 0.0;
    for r in rects {
        for y in r.y..r.y + r.h {
            for x in r.x..r.x + r.w {
                let e = (est.at(x, y) - truth.at(x, y)).abs();
                s2 += e * e;
                errs.push(e);
            }
        }
    }
    let n = errs.len();
    errs.sort_by(|a, b| a.partial_cmp(b).unwrap());
    ErrorStats {
        rmse: (s2 / n.max(1) as f64).sqrt(),
        p99: errs.get((0.99 * (n.saturating_sub(1)) as f64) as usize).cloned().unwrap_or(f64::NAN),
        max: errs.last().cloned().unwrap_or(f64::NAN),
        n,
    }
}

pub fn run_scene(scene: Scene, w: usize, h: usize, sp: &SynthParams, p: &SeparateParams) -> SceneResult {
    let s = mosaic(scene, w, h, sp);
    let t = Instant::now();
    let sep = separate(&s.mosaic, p);
    let ms = t.elapsed().as_millis();
    let ours = evaluate("mimizan", scene, &s, &sep.lum, sep.rounds, ms);

    let t = Instant::now();
    let base = bilinear_luminance(&s.mosaic.plane, sp.phase);
    let ms_b = t.elapsed().as_millis();
    let baseline = evaluate("bilinear-demosaic", scene, &s, &base, 1, ms_b);

    let truth_star = (scene == Scene::Star).then(|| {
        star_mtf(&s.l_true, w as f64 / 2.0, h as f64 / 2.0, STAR_SPOKES, 16.0, 0.44 * w.min(h) as f64)
    });

    SceneResult { scene, width: w, height: h, synth: *sp, separate: *p, truth_star, mimizan: ours, baseline }
}

/// Markdown table row set for RESULTS.md.
pub fn markdown(results: &[SceneResult]) -> String {
    let mut s = String::new();
    s.push_str("| Scene | Size | Estimator | RMSE | RMSE/σ | p99 | Interior RMSE/σ | MTF50 axis | MTF50 diag | MTF10 axis | Ring | Carrier max | Wedge slope | Wedge resid | ms (rounds) |\n");
    s.push_str("|---|---|---|---|---|---|---|---|---|---|---|---|---|---|---|\n");
    let fmt_opt = |v: Option<f64>| v.map(|x| format!("{x:.3}")).unwrap_or_else(|| "-".into());
    for r in results {
        if let Some(ts) = &r.truth_star {
            s.push_str(&format!(
                "| {:?} | {}x{} | truth (pixel aperture) | 0 | 0 | 0 | - | {} | {} | {} | {} | - | - | - | - |\n",
                r.scene,
                r.width,
                r.height,
                fmt_opt(ts.mtf50_axis),
                fmt_opt(ts.mtf50_diag),
                fmt_opt(ts.mtf10_axis),
                ts.ring_axis || ts.ring_diag
            ));
        }
        for e in [&r.mimizan, &r.baseline] {
            let interior = e
                .interior_error
                .map(|ie| format!("{:.2}", ie.rmse / (e.error.rmse / e.rmse_over_sigma).max(1e-12)))
                .unwrap_or_else(|| "-".into());
            let (m50a, m50d, m10a, ring) = match &e.star {
                Some(st) => (
                    fmt_opt(st.mtf50_axis),
                    fmt_opt(st.mtf50_diag),
                    fmt_opt(st.mtf10_axis),
                    format!("{}", st.ring_axis || st.ring_diag),
                ),
                None => ("-".into(), "-".into(), "-".into(), "-".into()),
            };
            let (ws, wr) = match &e.wedge {
                Some(wf) => (format!("{:.4}", wf.slope), format!("{:.2} %", 100.0 * wf.max_rel_residual)),
                None => ("-".into(), "-".into()),
            };
            s.push_str(&format!(
                "| {:?} | {}x{} | {} | {:.5} | {:.2} | {:.4} | {} | {} | {} | {} | {} | {} | {} | {} | {} ({}) |\n",
                r.scene,
                r.width,
                r.height,
                e.name,
                e.error.rmse,
                e.rmse_over_sigma,
                e.error.p99,
                interior,
                m50a,
                m50d,
                m10a,
                ring,
                fmt_opt(e.carrier_flat_max),
                ws,
                wr,
                e.ms,
                e.rounds
            ));
        }
    }
    s
}
