//! Output sharpening for screens (SPEC section 9, "Bildschirm").
//!
//! For a print the unknown is the viewer (distance, eye), and the USM radius
//! follows a viewing model. For an image shown pixel-for-pixel on a screen the
//! losses are *known*: the Lanczos-3 resampling that produced the pixels and
//! the display's pixel aperture (a 100 %-fill square pixel, MTF = sinc(f)).
//! Both have closed-form transfer functions, so the compensation is the
//! regularised inverse of their product, approximated by a short symmetric
//! FIR that is designed here by least squares. Nothing in it depends on the
//! picture; what it cannot know is a browser's own rescaling (HiDPI zoom),
//! so it assumes one image pixel = one device pixel.

use crate::filter::lowpass;
use crate::plane::Plane;
use crate::usm::MASK_THRESHOLD;
use rayon::prelude::*;
use std::f64::consts::PI;

/// Half-length of the symmetric kernel: 9 taps.
pub const HALF: usize = 4;
/// Knee of the 1/(f + knee) fit weight (cycles per pixel).
const FIT_WEIGHT_KNEE: f64 = 0.1;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScreenSharpen {
    /// 0 = off, 1 = full compensation of the known losses.
    pub amount: f64,
    /// Cap of the inverse filter's gain (Wiener regularisation): the response
    /// near Nyquist is limited to this factor instead of 1/MTF.
    pub max_gain: f64,
}

impl Default for ScreenSharpen {
    fn default() -> Self {
        Self { amount: 1.0, max_gain: 2.0 }
    }
}

/// Lanczos-3 continuous frequency response in output pixel units (DC = 1).
fn lanczos3_mtf(f: f64) -> f64 {
    const N: usize = 1201;
    let dx = 6.0 / (N - 1) as f64;
    let mut num = 0.0;
    let mut den = 0.0;
    for i in 0..N {
        let x = -3.0 + i as f64 * dx;
        let k = if x.abs() < 1e-12 {
            1.0
        } else {
            let px = PI * x;
            3.0 * px.sin() * (px / 3.0).sin() / (px * px)
        };
        num += k * (2.0 * PI * f * x).cos();
        den += k;
    }
    num / den
}

/// Square pixel aperture, 100 % fill.
fn aperture_mtf(f: f64) -> f64 {
    if f.abs() < 1e-12 {
        1.0
    } else {
        (PI * f).sin() / (PI * f)
    }
}

/// Combined known loss at output frequency `f` (cycles per output pixel).
pub fn known_mtf(f: f64, downscaled: bool) -> f64 {
    let a = aperture_mtf(f);
    if downscaled {
        a * lanczos3_mtf(f)
    } else {
        a
    }
}

/// Wiener inverse of the known MTF, gain capped at `max_gain`, scaled so the
/// DC gain is exactly one (the regularisation alone would give 1/(1+ε)).
pub fn target_response(f: f64, downscaled: bool, max_gain: f64) -> f64 {
    let m = known_mtf(f, downscaled);
    let eps = 1.0 / (4.0 * max_gain * max_gain);
    (1.0 + eps) * m / (m * m + eps)
}

/// Symmetric 7-tap kernel `h[-3..=3]` whose response fits the target over
/// 0..0.5 c/px in least squares, normalised to DC gain 1 and blended with the
/// identity by `amount`.
pub fn kernel(s: &ScreenSharpen, downscaled: bool) -> [f64; 2 * HALF + 1] {
    // Unknowns c0..c3 with H(f) = c0 + 2·Σ c_k cos(2πkf).
    const M: usize = HALF + 1;
    let fs: Vec<f64> = (0..=50).map(|i| i as f64 * 0.01).collect();
    let basis = |f: f64, k: usize| if k == 0 { 1.0 } else { 2.0 * (2.0 * PI * k as f64 * f).cos() };
    let mut ata = [[0.0; M]; M];
    let mut atb = [0.0; M];
    for &f in &fs {
        let t = target_response(f, downscaled, s.max_gain);
        // Natural images have ~1/f amplitude spectra: weight the fit so the
        // error is small where the picture has energy, and let the few taps
        // compromise near Nyquist instead of in the mid band.
        let w = 1.0 / (f + FIT_WEIGHT_KNEE);
        for (i, row) in ata.iter_mut().enumerate() {
            let bi = basis(f, i);
            atb[i] += w * bi * t;
            for (j, cell) in row.iter_mut().enumerate() {
                *cell += w * bi * basis(f, j);
            }
        }
    }
    let c = solve(ata, atb);
    // DC gain exactly one.
    let dc: f64 = c[0] + 2.0 * c[1..].iter().sum::<f64>();
    let mut h = [0.0; 2 * HALF + 1];
    for k in 0..M {
        let v = c[k] / dc;
        h[HALF + k] = v;
        h[HALF - k] = v;
    }
    // Blend with the identity.
    for (i, v) in h.iter_mut().enumerate() {
        let delta = if i == HALF { 1.0 } else { 0.0 };
        *v = delta + s.amount * (*v - delta);
    }
    h
}

fn solve(mut a: [[f64; HALF + 1]; HALF + 1], mut b: [f64; HALF + 1]) -> [f64; HALF + 1] {
    const M: usize = HALF + 1;
    for col in 0..M {
        let mut piv = col;
        for r in col + 1..M {
            if a[r][col].abs() > a[piv][col].abs() {
                piv = r;
            }
        }
        a.swap(col, piv);
        b.swap(col, piv);
        let d = a[col][col];
        let pivot_row = a[col];
        for r in 0..M {
            if r != col {
                let f = a[r][col] / d;
                for (cell, &pv) in a[r].iter_mut().zip(pivot_row.iter()).skip(col) {
                    *cell -= f * pv;
                }
                b[r] -= f * b[col];
            }
        }
    }
    let mut x = [0.0; M];
    for i in 0..M {
        x[i] = b[i] / a[i][i];
    }
    x
}

/// Response of a symmetric kernel at frequency `f` (for reports and tests).
pub fn response(h: &[f64], f: f64) -> f64 {
    let half = h.len() / 2;
    h.iter().enumerate().map(|(i, &v)| v * (2.0 * PI * f * (i as f64 - half as f64)).cos()).sum()
}

/// Apply the compensation where the mask allows (same gate as USM: mask ≥ 0.8
/// and not saturated); `mask == None` applies it everywhere.
pub fn sharpen(p: &Plane, h: &[f64], mask: Option<&Plane>) -> Plane {
    let filtered = lowpass(p, h);
    let Some(m) = mask else { return filtered };
    let w = p.width;
    let mut out = Plane::zeros(w, p.height);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (s, f, mr) = (p.row(y), filtered.row(y), m.row(y));
        for x in 0..w {
            let allow = mr[x] >= MASK_THRESHOLD && mr[x] < 1.0;
            row[x] = if allow { f[x] } else { s[x] };
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_losses_match_closed_forms() {
        assert!((aperture_mtf(0.5) - 2.0 / PI).abs() < 1e-9);
        assert!((lanczos3_mtf(0.0) - 1.0).abs() < 1e-9);
        // Lanczos-3 passes 0.2 c/px essentially unattenuated and rolls off by 0.4.
        assert!(lanczos3_mtf(0.2) > 0.99);
        assert!(lanczos3_mtf(0.4) < 0.9 && lanczos3_mtf(0.4) > 0.7);
    }

    #[test]
    fn kernel_compensates_within_the_passband_and_stays_capped() {
        let s = ScreenSharpen::default();
        let h = kernel(&s, true);
        assert!((h.iter().sum::<f64>() - 1.0).abs() < 1e-9);
        assert!((response(&h, 0.0) - 1.0).abs() < 1e-9);
        // Within the band the product of loss and compensation is close to one.
        for f in [0.1, 0.2, 0.3] {
            let p = response(&h, f) * known_mtf(f, true);
            assert!((p - 1.0).abs() < 0.08, "f {f}: {p}");
        }
        // Gain at Nyquist is bounded by the cap (with slack for the fit).
        assert!(response(&h, 0.5) < s.max_gain * 1.15);
        // Amount 0 is the identity.
        let id = kernel(&ScreenSharpen { amount: 0.0, max_gain: 2.0 }, true);
        assert!(
            (id[HALF] - 1.0).abs() < 1e-12
                && id.iter().enumerate().all(|(i, &v)| i == HALF || v.abs() < 1e-12)
        );
    }

    #[test]
    fn flat_untouched_edge_sharpened_mask_respected() {
        let (w, hgt) = (64, 8);
        let mut p = Plane::zeros(w, hgt);
        for y in 0..hgt {
            for x in 0..w {
                p.set(x, y, if x < 32 { 0.2 } else { 0.8 });
            }
        }
        let h = kernel(&ScreenSharpen::default(), true);
        let out = sharpen(&p, &h, None);
        assert!((out.at(5, 4) - 0.2).abs() < 1e-9);
        assert!(out.at(31, 4) < 0.2 && out.at(32, 4) > 0.8);
        let mask = Plane::from_vec(w, hgt, vec![0.3; w * hgt]);
        assert_eq!(sharpen(&p, &h, Some(&mask)).data, p.data);
    }
}
