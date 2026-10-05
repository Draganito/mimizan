//! Measurements (SPEC section 8). No number leaves this module without a definition.

use crate::blockfft::{BlockFft, CARRIERS};
use crate::decode::Rect;
use crate::ingest::NoiseModel;
use crate::plane::Plane;
use serde::Serialize;

/// RMSE and 99th percentile of |a-b| ignoring a border.
#[derive(Clone, Copy, Debug, Serialize)]
pub struct ErrorStats {
    pub rmse: f64,
    pub p99: f64,
    pub max: f64,
    pub n: usize,
}

pub fn error_stats(a: &Plane, b: &Plane, border: usize) -> ErrorStats {
    assert_eq!((a.width, a.height), (b.width, b.height));
    let mut errs: Vec<f64> = Vec::new();
    let mut s2 = 0.0;
    for y in border..a.height.saturating_sub(border) {
        for x in border..a.width.saturating_sub(border) {
            let e = (a.at(x, y) - b.at(x, y)).abs();
            s2 += e * e;
            errs.push(e);
        }
    }
    let n = errs.len();
    if n == 0 {
        return ErrorStats { rmse: f64::NAN, p99: f64::NAN, max: f64::NAN, n: 0 };
    }
    errs.sort_by(|p, q| p.partial_cmp(q).unwrap());
    ErrorStats {
        rmse: (s2 / n as f64).sqrt(),
        p99: errs[(0.99 * (n - 1) as f64) as usize],
        max: errs[n - 1],
        n,
    }
}

/// Energy near the three carriers relative to the expected noise energy of
/// the same bins, averaged over 64-px blocks (step 32) inside `region`.
#[derive(Clone, Debug, Serialize)]
pub struct CarrierReport {
    /// [C1 (½,½), C2a (½,0), C2b (0,½)]
    pub ratio: [f64; 3],
    pub blocks: usize,
    pub radius: f64,
}

pub fn carrier_energy(plane: &Plane, noise: &NoiseModel, region: Option<Rect>, radius: f64) -> CarrierReport {
    let n = 64usize;
    let bf = BlockFft::new(n);
    let reg = region.unwrap_or(Rect { x: 0, y: 0, w: plane.width, h: plane.height });
    let mut scratch = Vec::new();
    let mut acc = [0.0f64; 3];
    let mut blocks = 0usize;
    let mut sel: Vec<Vec<usize>> = vec![Vec::new(); 3];
    for (k, &(fx, fy)) in CARRIERS.iter().enumerate() {
        for iy in 0..n {
            for ix in 0..n {
                if bf.dist_inf(ix, iy, fx, fy) < radius {
                    sel[k].push(iy * n + ix);
                }
            }
        }
    }
    if reg.w < n || reg.h < n {
        return CarrierReport { ratio: [f64::NAN; 3], blocks: 0, radius };
    }
    let mut y0 = reg.y;
    while y0 + n <= reg.y + reg.h {
        let mut x0 = reg.x;
        while x0 + n <= reg.x + reg.w {
            let (pw, mean) = bf.power(plane, x0, y0, &mut scratch);
            let noise_bin = noise.variance(mean) * bf.window_energy;
            for k in 0..3 {
                let e: f64 = sel[k].iter().map(|&i| pw[i]).sum();
                acc[k] += e / (noise_bin * sel[k].len() as f64);
            }
            blocks += 1;
            x0 += n / 2;
        }
        y0 += n / 2;
    }
    let inv = 1.0 / blocks.max(1) as f64;
    CarrierReport { ratio: [acc[0] * inv, acc[1] * inv, acc[2] * inv], blocks, radius }
}

/// Mean and std of a rectangle.
pub fn region_stats(plane: &Plane, r: Rect) -> (f64, f64) {
    let mut s = 0.0;
    let mut s2 = 0.0;
    let mut n = 0usize;
    for y in r.y..(r.y + r.h).min(plane.height) {
        for x in r.x..(r.x + r.w).min(plane.width) {
            let v = plane.at(x, y);
            s += v;
            s2 += v * v;
            n += 1;
        }
    }
    let mean = s / n.max(1) as f64;
    let var = (s2 / n.max(1) as f64 - mean * mean).max(0.0);
    (mean, var.sqrt())
}

#[derive(Clone, Debug, Serialize)]
pub struct WedgeFit {
    pub slope: f64,
    pub intercept: f64,
    pub max_rel_residual: f64,
    pub n: usize,
    pub pass: bool,
}

/// log-log fit of patch means against relative exposure. Pass: slope 1.00 ± 0.02
/// and max relative residual ≤ 2 %.
pub fn wedge_fit(samples: &[(f64, f64)]) -> WedgeFit {
    let pts: Vec<(f64, f64)> =
        samples.iter().filter(|(e, m)| *e > 0.0 && *m > 0.0).map(|(e, m)| (e.ln(), m.ln())).collect();
    let n = pts.len();
    if n < 2 {
        return WedgeFit { slope: f64::NAN, intercept: f64::NAN, max_rel_residual: f64::NAN, n, pass: false };
    }
    let mx = pts.iter().map(|p| p.0).sum::<f64>() / n as f64;
    let my = pts.iter().map(|p| p.1).sum::<f64>() / n as f64;
    let sxy: f64 = pts.iter().map(|p| (p.0 - mx) * (p.1 - my)).sum();
    let sxx: f64 = pts.iter().map(|p| (p.0 - mx) * (p.0 - mx)).sum();
    let slope = sxy / sxx;
    let intercept = my - slope * mx;
    let max_rel_residual =
        pts.iter().map(|p| ((p.1 - (intercept + slope * p.0)).exp() - 1.0).abs()).fold(0.0, f64::max);
    let pass = (slope - 1.0).abs() <= 0.02 && max_rel_residual <= 0.02;
    WedgeFit { slope, intercept, max_rel_residual, n, pass }
}

#[derive(Clone, Debug, Serialize)]
pub struct StarSample {
    pub radius: f64,
    /// Spoke frequency at this radius in c/px.
    pub freq: f64,
    pub axis: f64,
    pub diag: f64,
    pub all: f64,
}

#[derive(Clone, Debug, Serialize)]
pub struct StarReport {
    pub samples: Vec<StarSample>,
    pub mtf50_axis: Option<f64>,
    pub mtf50_diag: Option<f64>,
    pub mtf10_axis: Option<f64>,
    pub mtf10_diag: Option<f64>,
    /// Contrast rising again beyond the first minimum: aliasing ring.
    pub ring_axis: bool,
    pub ring_diag: bool,
}

#[inline]
fn bilinear(p: &Plane, x: f64, y: f64) -> f64 {
    let x0 = x.floor().max(0.0) as usize;
    let y0 = y.floor().max(0.0) as usize;
    let x1 = (x0 + 1).min(p.width - 1);
    let y1 = (y0 + 1).min(p.height - 1);
    let fx = x - x0 as f64;
    let fy = y - y0 as f64;
    let a = p.at(x0.min(p.width - 1), y0.min(p.height - 1));
    let b = p.at(x1, y0.min(p.height - 1));
    let c = p.at(x0.min(p.width - 1), y1);
    let d = p.at(x1, y1);
    (a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy
}

/// Modulation of the `spokes`-th angular harmonic on circles of growing radius,
/// split into sectors within ±10° of the axes and of the diagonals. Contrast is
/// normalised to the outermost (lowest-frequency) ring of the same sector.
pub fn star_mtf(plane: &Plane, cx: f64, cy: f64, spokes: usize, r_min: f64, r_max: f64) -> StarReport {
    let k = (spokes * 16).max(720);
    let mut samples = Vec::new();
    let mut r = r_max;
    let step = (r_max - r_min) / 60.0;
    let sector = 10f64.to_radians();
    while r >= r_min {
        let (mut ca, mut sa, mut na) = (0.0, 0.0, 0usize);
        let (mut cd, mut sd, mut nd) = (0.0, 0.0, 0usize);
        let (mut cl, mut sl, mut nl) = (0.0, 0.0, 0usize);
        for j in 0..k {
            let th = 2.0 * std::f64::consts::PI * j as f64 / k as f64;
            let v = bilinear(plane, cx + r * th.cos(), cy + r * th.sin());
            let (s, c) = (spokes as f64 * th).sin_cos();
            cl += v * c;
            sl += v * s;
            nl += 1;
            let m = th.rem_euclid(std::f64::consts::FRAC_PI_2);
            let to_axis = m.min(std::f64::consts::FRAC_PI_2 - m);
            let to_diag = (m - std::f64::consts::FRAC_PI_4).abs();
            if to_axis <= sector {
                ca += v * c;
                sa += v * s;
                na += 1;
            }
            if to_diag <= sector {
                cd += v * c;
                sd += v * s;
                nd += 1;
            }
        }
        let amp = |c: f64, s: f64, n: usize| 2.0 * (c * c + s * s).sqrt() / n.max(1) as f64;
        samples.push(StarSample {
            radius: r,
            freq: spokes as f64 / (2.0 * std::f64::consts::PI * r),
            axis: amp(ca, sa, na),
            diag: amp(cd, sd, nd),
            all: amp(cl, sl, nl),
        });
        r -= step;
    }
    // Normalise to the first (outermost) sample.
    if let Some(first) = samples.first().cloned() {
        for s in &mut samples {
            s.axis /= first.axis.max(1e-12);
            s.diag /= first.diag.max(1e-12);
            s.all /= first.all.max(1e-12);
        }
    }
    let crossing = |sel: fn(&StarSample) -> f64, level: f64| -> Option<f64> {
        for w in samples.windows(2) {
            let (a, b) = (sel(&w[0]), sel(&w[1]));
            if a >= level && b < level {
                let t = (a - level) / (a - b);
                return Some(w[0].freq + t * (w[1].freq - w[0].freq));
            }
        }
        None
    };
    let ring = |sel: fn(&StarSample) -> f64| -> bool {
        let mut min_seen = f64::INFINITY;
        let mut below = false;
        for s in &samples {
            let v = sel(s);
            if v < 0.1 {
                below = true;
            }
            if below && v > min_seen + 0.15 {
                return true;
            }
            min_seen = min_seen.min(v);
        }
        false
    };
    StarReport {
        mtf50_axis: crossing(|s| s.axis, 0.5),
        mtf50_diag: crossing(|s| s.diag, 0.5),
        mtf10_axis: crossing(|s| s.axis, 0.1),
        mtf10_diag: crossing(|s| s.diag, 0.1),
        ring_axis: ring(|s| s.axis),
        ring_diag: ring(|s| s.diag),
        samples,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wedge_fit_detects_gamma() {
        let lin: Vec<(f64, f64)> =
            (0..16).map(|i| (2f64.powf(-i as f64 / 3.0), 0.9 * 2f64.powf(-i as f64 / 3.0))).collect();
        let f = wedge_fit(&lin);
        assert!(f.pass, "{f:?}");
        let gam: Vec<(f64, f64)> = lin.iter().map(|(e, m)| (*e, m.powf(1.2))).collect();
        let g = wedge_fit(&gam);
        assert!(!g.pass);
        assert!((g.slope - 1.2).abs() < 1e-9);
    }

    #[test]
    fn star_on_perfect_truth_has_high_mtf50() {
        use crate::synth::{render, Scene, STAR_SPOKES};
        let n = 512;
        let rgb = render(Scene::Star, n, n);
        let l = rgb.luminance();
        let rep = star_mtf(&l, n as f64 / 2.0, n as f64 / 2.0, STAR_SPOKES, 20.0, 230.0);
        // The pixel aperture (4x4 supersampling) plus the contrast sampling of
        // the star limit the truth itself to about 0.27 c/px (RESULTS.md).
        assert!(rep.mtf50_axis.unwrap() > 0.25, "{:?}", rep.mtf50_axis);
        assert!(rep.mtf50_diag.unwrap() > 0.25, "{:?}", rep.mtf50_diag);
    }
}
