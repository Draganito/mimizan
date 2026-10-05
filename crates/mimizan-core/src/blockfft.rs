//! Windowed 2-D block spectra in f64 (SPEC sections 5.2 and 8).

use crate::plane::Plane;
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::sync::Arc;

pub struct BlockFft {
    pub n: usize,
    fft: Arc<dyn Fft<f64>>,
    /// 2-D Hann window, row-major.
    pub window: Vec<f64>,
    /// Σ w² over the block.
    pub window_energy: f64,
}

impl BlockFft {
    pub fn new(n: usize) -> Self {
        let mut planner = FftPlanner::<f64>::new();
        let fft = planner.plan_fft_forward(n);
        let hann: Vec<f64> =
            (0..n).map(|i| 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / n as f64).cos()).collect();
        let mut window = vec![0.0; n * n];
        for y in 0..n {
            for x in 0..n {
                window[y * n + x] = hann[y] * hann[x];
            }
        }
        let window_energy = window.iter().map(|w| w * w).sum();
        Self { n, fft, window, window_energy }
    }

    /// Frequency (c/px) of DFT bin `i`, in (-0.5, 0.5].
    #[inline]
    pub fn freq(&self, i: usize) -> f64 {
        let n = self.n;
        if i <= n / 2 {
            i as f64 / n as f64
        } else {
            (i as f64 - n as f64) / n as f64
        }
    }

    /// Windowed power spectrum |X|² of the n×n block at (x0, y0). The block mean
    /// is removed before windowing so the DC leakage does not dominate.
    /// Returns (power, block_mean).
    pub fn power(
        &self,
        plane: &Plane,
        x0: usize,
        y0: usize,
        scratch: &mut Vec<Complex<f64>>,
    ) -> (Vec<f64>, f64) {
        let n = self.n;
        scratch.clear();
        scratch.resize(n * n, Complex::new(0.0, 0.0));
        let mut mean = 0.0;
        for y in 0..n {
            let row = &plane.row(y0 + y)[x0..x0 + n];
            for v in row {
                mean += v;
            }
        }
        mean /= (n * n) as f64;
        for y in 0..n {
            let row = &plane.row(y0 + y)[x0..x0 + n];
            for x in 0..n {
                scratch[y * n + x] = Complex::new((row[x] - mean) * self.window[y * n + x], 0.0);
            }
        }
        // rows
        for y in 0..n {
            self.fft.process(&mut scratch[y * n..(y + 1) * n]);
        }
        // columns
        let mut col = vec![Complex::new(0.0, 0.0); n];
        for x in 0..n {
            for y in 0..n {
                col[y] = scratch[y * n + x];
            }
            self.fft.process(&mut col);
            for y in 0..n {
                scratch[y * n + x] = col[y];
            }
        }
        let power: Vec<f64> = scratch.iter().map(|c| c.norm_sqr()).collect();
        (power, mean)
    }

    /// Sup-norm distance of bin (ix, iy) to carrier (fx, fy), with wrap-around.
    #[inline]
    pub fn dist_inf(&self, ix: usize, iy: usize, fx: f64, fy: f64) -> f64 {
        let dx = wrap_dist(self.freq(ix), fx);
        let dy = wrap_dist(self.freq(iy), fy);
        dx.max(dy)
    }
}

/// Distance between two frequencies on the circle of period 1.
#[inline]
pub fn wrap_dist(a: f64, b: f64) -> f64 {
    let d = (a - b).abs() % 1.0;
    d.min(1.0 - d)
}

/// The three carrier positions (fx, fy).
pub const CARRIERS: [(f64, f64); 3] = [(0.5, 0.5), (0.5, 0.0), (0.0, 0.5)];

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checkerboard_energy_sits_at_half_half() {
        let n = 64;
        let b = BlockFft::new(n);
        let mut p = Plane::zeros(n, n);
        for y in 0..n {
            for x in 0..n {
                p.set(x, y, if (x + y) % 2 == 0 { 0.6 } else { 0.4 });
            }
        }
        let mut s = Vec::new();
        let (pw, mean) = b.power(&p, 0, 0, &mut s);
        assert!((mean - 0.5).abs() < 1e-12);
        // A Hann-windowed tone exactly on a bin occupies that bin and its
        // direct neighbours (3x3 in 2-D), nothing else.
        let total: f64 = pw.iter().sum();
        let mut near = 0.0;
        for dy in -1i64..=1 {
            for dx in -1i64..=1 {
                near += pw[((n as i64 / 2 + dy) * n as i64 + n as i64 / 2 + dx) as usize];
            }
        }
        assert!(near / total > 0.999, "{}", near / total);
        let centre = pw[(n / 2) * n + n / 2] / total;
        assert!((centre - 4.0 / 9.0).abs() < 1e-6, "{centre}");
    }

    #[test]
    fn noise_energy_matches_window_energy() {
        // Deterministic pseudo-noise with unit variance.
        let n = 64;
        let b = BlockFft::new(n);
        let mut p = Plane::zeros(n, n);
        let mut state = 0x9E3779B97F4A7C15u64;
        let mut acc = 0.0;
        for v in &mut p.data {
            // xorshift + sum of 12 uniforms -> ~N(0,1)
            let mut s = 0.0;
            for _ in 0..12 {
                state ^= state << 13;
                state ^= state >> 7;
                state ^= state << 17;
                s += (state >> 11) as f64 / (1u64 << 53) as f64;
            }
            *v = s - 6.0;
            acc += *v * *v;
        }
        let var = acc / (n * n) as f64;
        let mut s = Vec::new();
        let (pw, _) = b.power(&p, 0, 0, &mut s);
        let mean_bin: f64 = pw.iter().sum::<f64>() / (n * n) as f64;
        let expected = var * b.window_energy;
        assert!((mean_bin / expected - 1.0).abs() < 0.1, "{mean_bin} vs {expected}");
    }
}
