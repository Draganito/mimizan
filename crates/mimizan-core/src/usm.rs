//! Unsharp mask with Gaussian σ = r_px, applied only where the mask allows
//! (SPEC section 9, "USM").

use crate::plane::Plane;
use rayon::prelude::*;

/// Default `usm_k`: 1 arc minute at the viewing distance, in px per (mm·DPI).
pub const USM_K: f64 = 1.15e-5;
/// Sidecar threshold: USM only where `mask ≥ 0.8` (204/255).
pub const MASK_THRESHOLD: f64 = 0.8;

/// `r_px = usm_k · d_mm · dpi`
pub fn radius_px(usm_k: f64, viewing_distance_mm: f64, dpi: f64) -> f64 {
    usm_k * viewing_distance_mm * dpi
}

fn gauss_kernel(sigma: f64) -> Vec<f64> {
    let r = (3.0 * sigma).ceil().max(1.0) as usize;
    let mut k: Vec<f64> = (0..=2 * r)
        .map(|i| {
            let t = i as f64 - r as f64;
            (-0.5 * t * t / (sigma * sigma)).exp()
        })
        .collect();
    let s: f64 = k.iter().sum();
    for v in &mut k {
        *v /= s;
    }
    k
}

/// Separable Gaussian blur with reflect-101 borders (reuses the FIR engine).
pub fn gaussian_blur(p: &Plane, sigma: f64) -> Plane {
    let k = gauss_kernel(sigma);
    crate::filter::lowpass(p, &k)
}

/// `out = x + amount·(x − blur(x))` where `mask ≥ MASK_THRESHOLD` and the
/// pixel is not flagged saturated (mask == 1.0 exactly, sidecar 255); else `x`.
/// `mask == None` applies USM everywhere.
pub fn unsharp(p: &Plane, sigma: f64, amount: f64, mask: Option<&Plane>) -> Plane {
    if amount <= 0.0 || sigma <= 0.0 {
        return p.clone();
    }
    let blur = gaussian_blur(p, sigma);
    let w = p.width;
    let mut out = Plane::zeros(w, p.height);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (s, b) = (p.row(y), blur.row(y));
        let m = mask.map(|m| m.row(y));
        for x in 0..w {
            let allow = match m {
                None => true,
                Some(mr) => mr[x] >= MASK_THRESHOLD && mr[x] < 1.0,
            };
            row[x] = if allow { s[x] + amount * (s[x] - b[x]) } else { s[x] };
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn radius_example_from_spec() {
        // 400 mm viewing distance at 300 DPI -> 1.4 px
        let r = radius_px(USM_K, 400.0, 300.0);
        assert!((r - 1.38).abs() < 0.01, "{r}");
    }

    #[test]
    fn flat_is_untouched_and_edge_is_sharpened() {
        let (w, h) = (64, 16);
        let mut p = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                p.set(x, y, if x < 32 { 0.2 } else { 0.8 });
            }
        }
        let out = unsharp(&p, 1.4, 1.0, None);
        assert!((out.at(5, 5) - 0.2).abs() < 1e-9);
        assert!(out.at(31, 5) < 0.2, "{}", out.at(31, 5));
        assert!(out.at(32, 5) > 0.8, "{}", out.at(32, 5));
        // Masked out: identical to the input.
        let mask = Plane::from_vec(w, h, vec![0.3; w * h]);
        let masked = unsharp(&p, 1.4, 1.0, Some(&mask));
        assert_eq!(masked.data, p.data);
        // Saturated (mask exactly 1.0) is also left alone.
        let sat = Plane::from_vec(w, h, vec![1.0; w * h]);
        assert_eq!(unsharp(&p, 1.4, 1.0, Some(&sat)).data, p.data);
    }
}
