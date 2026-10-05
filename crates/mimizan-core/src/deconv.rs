//! Richardson–Lucy deconvolution with a Gaussian PSF.
//!
//! Applied to every pixel the same way: no saturation mask, no local noise
//! weight. The blur width is the print-geometry radius USM would have used,
//! so this is that blur inverted rather than boosted.

use crate::plane::Plane;
use crate::usm::gaussian_blur;
use rayon::prelude::*;

/// Iterations for the optional print path. Enough to undo a ~1.5 px blur
/// and to show the noise the inverse brings back.
pub const DECONV_ITERATIONS: usize = 10;

/// `estimate ← estimate · blur(image / blur(estimate))`, `iterations` times.
/// The PSF is symmetric, so the correction blur is the same Gaussian.
/// Non-negative samples only; values above 1 are left for the encoder.
pub fn richardson_lucy(image: &Plane, sigma: f64, iterations: usize) -> Plane {
    if sigma <= 0.0 || iterations == 0 {
        return image.clone();
    }
    let w = image.width;
    let mut est = image.clone();
    for v in &mut est.data {
        if *v < 0.0 {
            *v = 0.0;
        }
    }
    for _ in 0..iterations {
        let blurred = gaussian_blur(&est, sigma);
        let mut ratio = Plane::zeros(w, image.height);
        ratio.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let img = image.row(y);
            let b = blurred.row(y);
            for x in 0..w {
                row[x] = img[x].max(0.0) / b[x].max(1e-12);
            }
        });
        let corr = gaussian_blur(&ratio, sigma);
        est.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let c = corr.row(y);
            for x in 0..w {
                row[x] *= c[x];
            }
        });
    }
    est
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_stays_flat() {
        let p = Plane::from_vec(32, 16, vec![0.4; 32 * 16]);
        let out = richardson_lucy(&p, 1.4, DECONV_ITERATIONS);
        for &v in &out.data {
            assert!((v - 0.4).abs() < 1e-6, "{v}");
        }
    }

    #[test]
    fn blurred_step_gets_steeper() {
        let (w, h) = (64, 16);
        let mut sharp = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                sharp.set(x, y, if x < 32 { 0.2 } else { 0.8 });
            }
        }
        let blurred = gaussian_blur(&sharp, 1.4);
        let out = richardson_lucy(&blurred, 1.4, DECONV_ITERATIONS);
        let contrast = |p: &Plane| p.at(35, 8) - p.at(28, 8);
        assert!(contrast(&out) > contrast(&blurred), "{} vs {}", contrast(&out), contrast(&blurred));
        assert!(out.at(28, 8).is_finite() && out.at(35, 8).is_finite());
    }
}
