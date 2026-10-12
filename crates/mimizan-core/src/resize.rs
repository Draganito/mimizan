//! Separable resampling in f64 (SPEC section 9, "Größe").
//!
//! The kernel is a parameter ([`ResampleKernel`]); Lanczos-3 is the default
//! and what [`resize`] uses. When shrinking, the kernel is stretched by the
//! scale factor so it acts as the anti-alias low-pass at the same time (the
//! standard "support scales with the footprint" rule). Weights are normalised
//! per output sample so the DC gain is exactly one everywhere, including at
//! the borders (which are handled by clamping the source index).
//!
//! There is no kernel that is both sharp and free of overshoot. Every
//! interpolating kernel smoother than linear has negative lobes and rings at a
//! hard edge; the only member of the cubic family without them is the
//! B-spline, and it halves the MTF at 0.4 c/px. The step overshoot and the
//! MTF of each kernel are in `docs/RESULTS.md` ("Resampling kernels"); the
//! choice is recorded in the print's description.

use crate::plane::Plane;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// Resampling kernel of the print stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResampleKernel {
    /// Windowed sinc, support 3: the sharpest (MTF 0.82 at 0.4 c/px), about
    /// 6 % overshoot at a step. The default and the bench's kernel.
    #[default]
    Lanczos3,
    /// Mitchell–Netravali B = C = 1/3: the compromise of that paper, 1.6 %
    /// overshoot, MTF 0.59 at 0.4 c/px.
    Mitchell,
    /// Keys cubic (B = 0, C = 1/2): interpolating, 2.5 % overshoot, MTF 0.72
    /// at 0.4 c/px.
    CatmullRom,
    /// Cubic B-spline (B = 1, C = 0): no negative lobe, no overshoot at any
    /// edge, MTF 0.33 at 0.4 c/px. Not interpolating (it smooths at scale 1).
    BSpline,
}

impl ResampleKernel {
    pub const ALL: [ResampleKernel; 4] = [Self::Lanczos3, Self::Mitchell, Self::CatmullRom, Self::BSpline];

    /// Name as it appears in the description JSON and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Lanczos3 => "lanczos3",
            Self::Mitchell => "mitchell",
            Self::CatmullRom => "catmull-rom",
            Self::BSpline => "b-spline",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim().to_ascii_lowercase();
        Self::ALL.iter().copied().find(|k| k.name() == t || k.name().replace('-', "") == t.replace('-', ""))
    }

    /// Half-width of the support in source pixels (at scale 1).
    pub fn support(self) -> f64 {
        match self {
            Self::Lanczos3 => 3.0,
            Self::Mitchell | Self::CatmullRom | Self::BSpline => 2.0,
        }
    }

    /// Kernel value at `x` (source pixels, scale 1). Not normalised.
    #[inline]
    pub fn eval(self, x: f64) -> f64 {
        match self {
            Self::Lanczos3 => lanczos3(x),
            Self::Mitchell => mitchell_netravali(x, 1.0 / 3.0, 1.0 / 3.0),
            Self::CatmullRom => mitchell_netravali(x, 0.0, 0.5),
            Self::BSpline => mitchell_netravali(x, 1.0, 0.0),
        }
    }

    /// Continuous frequency response in output pixel units, DC = 1 (numeric
    /// integration; for the screen compensation and reports).
    pub fn mtf(self, f: f64) -> f64 {
        const N: usize = 1201;
        let a = self.support();
        let dx = 2.0 * a / (N - 1) as f64;
        let mut num = 0.0;
        let mut den = 0.0;
        for i in 0..N {
            let x = -a + i as f64 * dx;
            let k = self.eval(x);
            num += k * (2.0 * PI * f * x).cos();
            den += k;
        }
        num / den
    }
}

const A: f64 = 3.0;

#[inline]
fn lanczos3(x: f64) -> f64 {
    let x = x.abs();
    if x < 1e-12 {
        1.0
    } else if x >= A {
        0.0
    } else {
        let px = PI * x;
        A * px.sin() * (px / A).sin() / (px * px)
    }
}

/// Mitchell–Netravali cubic family (B, C); support 2.
#[inline]
fn mitchell_netravali(x: f64, b: f64, c: f64) -> f64 {
    let x = x.abs();
    if x < 1.0 {
        ((12.0 - 9.0 * b - 6.0 * c) * x * x * x + (-18.0 + 12.0 * b + 6.0 * c) * x * x + (6.0 - 2.0 * b))
            / 6.0
    } else if x < 2.0 {
        ((-b - 6.0 * c) * x * x * x
            + (6.0 * b + 30.0 * c) * x * x
            + (-12.0 * b - 48.0 * c) * x
            + (8.0 * b + 24.0 * c))
            / 6.0
    } else {
        0.0
    }
}

/// Per-output-sample taps: (first source index, weights).
struct Taps {
    start: Vec<usize>,
    weights: Vec<Vec<f64>>,
}

fn build_taps(src_len: usize, dst_len: usize, kernel: ResampleKernel) -> Taps {
    let scale = src_len as f64 / dst_len as f64; // > 1 when shrinking
    let stretch = scale.max(1.0);
    let support = kernel.support() * stretch;
    let mut start = Vec::with_capacity(dst_len);
    let mut weights = Vec::with_capacity(dst_len);
    for i in 0..dst_len {
        // Centre of destination pixel i mapped into source coordinates.
        let centre = (i as f64 + 0.5) * scale - 0.5;
        let lo = (centre - support).floor() as isize;
        let hi = (centre + support).ceil() as isize;
        let mut w = Vec::with_capacity((hi - lo + 1) as usize);
        let mut sum = 0.0;
        for j in lo..=hi {
            let v = kernel.eval((j as f64 - centre) / stretch);
            w.push(v);
            sum += v;
        }
        for v in &mut w {
            *v /= sum;
        }
        // Clamp indices at the borders by folding the weights onto the edge sample.
        let first = lo.max(0) as usize;
        let last = (hi.min(src_len as isize - 1)) as usize;
        let mut folded = vec![0.0; last - first + 1];
        for (k, j) in (lo..=hi).enumerate() {
            let jc = j.clamp(0, src_len as isize - 1) as usize;
            folded[jc - first] += w[k];
        }
        start.push(first);
        weights.push(folded);
    }
    Taps { start, weights }
}

fn resize_rows(src: &Plane, dst_w: usize, kernel: ResampleKernel) -> Plane {
    let taps = build_taps(src.width, dst_w, kernel);
    let mut out = Plane::zeros(dst_w, src.height);
    out.data.par_chunks_mut(dst_w).enumerate().for_each(|(y, row)| {
        let s = src.row(y);
        for (x, o) in row.iter_mut().enumerate() {
            let st = taps.start[x];
            let w = &taps.weights[x];
            let mut acc = 0.0;
            for (k, wk) in w.iter().enumerate() {
                acc += wk * s[st + k];
            }
            *o = acc;
        }
    });
    out
}

fn resize_cols(src: &Plane, dst_h: usize, kernel: ResampleKernel) -> Plane {
    let taps = build_taps(src.height, dst_h, kernel);
    let w = src.width;
    let mut out = Plane::zeros(w, dst_h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let st = taps.start[y];
        let wts = &taps.weights[y];
        row.iter_mut().for_each(|o| *o = 0.0);
        for (k, wk) in wts.iter().enumerate() {
            let s = src.row(st + k);
            for x in 0..w {
                row[x] += wk * s[x];
            }
        }
    });
    out
}

/// Resample to exactly `dst_w × dst_h` with Lanczos-3 (the default kernel;
/// the bench and the preview use this one).
pub fn resize(src: &Plane, dst_w: usize, dst_h: usize) -> Plane {
    resize_with(src, dst_w, dst_h, ResampleKernel::Lanczos3)
}

/// Resample to exactly `dst_w × dst_h` with `kernel`. Rows first when
/// shrinking in x more than in y (fewer operations), otherwise columns first.
/// An unchanged size returns a copy, whatever the kernel (the B-spline would
/// otherwise smooth at scale 1).
pub fn resize_with(src: &Plane, dst_w: usize, dst_h: usize, kernel: ResampleKernel) -> Plane {
    assert!(dst_w > 0 && dst_h > 0, "target size must be positive");
    if dst_w == src.width && dst_h == src.height {
        return src.clone();
    }
    let cost_rows_first = src.height * dst_w + dst_w * dst_h;
    let cost_cols_first = src.width * dst_h + dst_w * dst_h;
    if cost_rows_first <= cost_cols_first {
        let t = if dst_w != src.width { resize_rows(src, dst_w, kernel) } else { src.clone() };
        if dst_h != t.height {
            resize_cols(&t, dst_h, kernel)
        } else {
            t
        }
    } else {
        let t = if dst_h != src.height { resize_cols(src, dst_h, kernel) } else { src.clone() };
        if dst_w != t.width {
            resize_rows(&t, dst_w, kernel)
        } else {
            t
        }
    }
}

/// Largest overshoot above 1 and undershoot below 0 when a unit step is
/// resampled by `scale` with `kernel` (a measurement for the reports: what
/// the kernel does at a hard edge).
pub fn step_overshoot(kernel: ResampleKernel, scale: f64) -> (f64, f64) {
    let n = 64;
    let mut src = Plane::zeros(n, 1);
    for x in n / 2..n {
        src.set(x, 0, 1.0);
    }
    let out = resize_with(&src, (n as f64 * scale).round() as usize, 1, kernel);
    let hi = out.data.iter().cloned().fold(0.0, f64::max) - 1.0;
    let lo = out.data.iter().cloned().fold(0.0, f64::min);
    (hi.max(0.0), lo.min(0.0))
}

/// Pixel size that fits `(w, h)` into `(box_w, box_h)` keeping the aspect
/// ratio; the box is rotated to match the image orientation.
pub fn fit_dims(w: usize, h: usize, box_w: usize, box_h: usize) -> (usize, usize) {
    let (bw, bh) = if (w > h) != (box_w > box_h) { (box_h, box_w) } else { (box_w, box_h) };
    let s = (bw as f64 / w as f64).min(bh as f64 / h as f64);
    let dw = ((w as f64 * s).round() as usize).clamp(1, bw.max(1));
    let dh = ((h as f64 * s).round() as usize).clamp(1, bh.max(1));
    (dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_stays_flat_everywhere() {
        let src = Plane::from_vec(100, 60, vec![0.37; 6000]);
        for (w, h) in [(50, 30), (33, 17), (200, 120), (7, 90)] {
            let out = resize(&src, w, h);
            for v in &out.data {
                assert!((v - 0.37).abs() < 1e-12, "{v}");
            }
        }
    }

    #[test]
    fn identity_and_fit() {
        let src = Plane::from_vec(10, 5, (0..50).map(|i| i as f64 / 50.0).collect());
        let same = resize(&src, 10, 5);
        assert_eq!(same.data, src.data);
        // The paper box is rotated to the image orientation.
        assert_eq!(fit_dims(6000, 4000, 3000, 4500), (4500, 3000));
        assert_eq!(fit_dims(4000, 6000, 3000, 4500), (3000, 4500));
        assert_eq!(fit_dims(6000, 4000, 4500, 3000), (4500, 3000));
        // 3:2 image on 4:5 paper: the long side limits.
        assert_eq!(fit_dims(6000, 4000, 2400, 3000), (3000, 2000));
    }

    #[test]
    fn downscale_low_frequency_ramp_is_preserved() {
        let (w, h) = (256, 64);
        let mut src = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                src.set(x, y, 0.1 + 0.8 * x as f64 / (w - 1) as f64);
            }
        }
        let out = resize(&src, 64, 16);
        for y in 0..16 {
            for x in 4..60 {
                let expected = 0.1 + 0.8 * ((x as f64 + 0.5) * 4.0 - 0.5) / (w - 1) as f64;
                assert!((out.at(x, y) - expected).abs() < 2e-3, "{} vs {expected}", out.at(x, y));
            }
        }
    }

    #[test]
    fn every_kernel_keeps_flat_flat_and_dc_one() {
        let src = Plane::from_vec(100, 60, vec![0.37; 6000]);
        for k in ResampleKernel::ALL {
            for (w, h) in [(50, 30), (33, 17), (200, 120), (7, 90)] {
                let out = resize_with(&src, w, h, k);
                for v in &out.data {
                    assert!((v - 0.37).abs() < 1e-12, "{k:?} {v}");
                }
            }
            assert!((k.mtf(0.0) - 1.0).abs() < 1e-9, "{k:?}");
            assert_eq!(ResampleKernel::parse(k.name()), Some(k));
        }
        assert_eq!(ResampleKernel::parse("Catmull-Rom"), Some(ResampleKernel::CatmullRom));
        assert_eq!(ResampleKernel::parse("bspline"), Some(ResampleKernel::BSpline));
        assert_eq!(ResampleKernel::parse("nearest"), None);
    }

    /// The claim in the docs: the B-spline never overshoots, the others do,
    /// and the sharper the kernel the more.
    #[test]
    fn overshoot_orders_the_kernels() {
        let over = |k| {
            let (hi, lo) = step_overshoot(k, 1.37);
            hi.max(-lo)
        };
        // Zero up to the rounding of the normalised taps.
        for s in [1.37, 0.61, 2.0] {
            let (hi, lo) = step_overshoot(ResampleKernel::BSpline, s);
            assert!(hi < 1e-12 && lo > -1e-12, "b-spline at {s}: {hi} {lo}");
        }
        let (m, c, l) = (
            over(ResampleKernel::Mitchell),
            over(ResampleKernel::CatmullRom),
            over(ResampleKernel::Lanczos3),
        );
        assert!(m > 0.0 && m < c && c < l, "mitchell {m} catmull {c} lanczos {l}");
        assert!(l > 0.05 && l < 0.15, "{l}");
        // Sharpness follows the same order (MTF at 0.4 c/px).
        let mtf = |k: ResampleKernel| k.mtf(0.4);
        assert!(mtf(ResampleKernel::BSpline) < mtf(ResampleKernel::Mitchell));
        assert!(mtf(ResampleKernel::Mitchell) < mtf(ResampleKernel::CatmullRom));
        assert!(mtf(ResampleKernel::CatmullRom) < mtf(ResampleKernel::Lanczos3));
    }

    /// Interpolating kernels pass through the samples when the grid lines up
    /// (2× magnification puts every other output pixel on a source sample).
    #[test]
    fn interpolating_kernels_hit_the_samples_at_integer_scale() {
        let src = Plane::from_vec(16, 1, (0..16).map(|i| ((i * 7919) % 101) as f64 / 101.0).collect());
        for k in [ResampleKernel::Lanczos3, ResampleKernel::CatmullRom] {
            // Output pixel 2i+0.5 maps to source (2i+0.5)/2-0.5 = i-0.25: not on
            // a sample. Use 3×: output 3i+1 -> (3i+1.5)/3-0.5 = i exactly.
            let out = resize_with(&src, 48, 1, k);
            for i in 2..14 {
                assert!((out.at(3 * i + 1, 0) - src.at(i, 0)).abs() < 1e-12, "{k:?} at {i}");
            }
        }
        // The B-spline smooths even there.
        let out = resize_with(&src, 48, 1, ResampleKernel::BSpline);
        assert!((2..14).any(|i| (out.at(3 * i + 1, 0) - src.at(i, 0)).abs() > 1e-3));
    }

    #[test]
    fn downscale_kills_nyquist_checkerboard() {
        let (w, h) = (128, 128);
        let mut src = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                src.set(x, y, if (x + y) % 2 == 0 { 0.9 } else { 0.1 });
            }
        }
        let out = resize(&src, 32, 32);
        for y in 4..28 {
            for x in 4..28 {
                assert!((out.at(x, y) - 0.5).abs() < 1e-3, "{}", out.at(x, y));
            }
        }
    }
}
