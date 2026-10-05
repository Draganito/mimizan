//! Kaiser-windowed sinc low-pass and separable convolution (SPEC section 5.1).

use crate::plane::Plane;
use rayon::prelude::*;
use std::f64::consts::PI;

/// Zeroth-order modified Bessel function, power series.
pub fn bessel_i0(x: f64) -> f64 {
    let y = x * x / 4.0;
    let mut term = 1.0;
    let mut sum = 1.0;
    for k in 1..200 {
        term *= y / (k as f64 * k as f64);
        sum += term;
        if term < sum * 1e-17 {
            break;
        }
    }
    sum
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LowpassSpec {
    /// Half-amplitude cutoff in cycles per pixel.
    pub cutoff: f64,
    /// Transition width in cycles per pixel.
    pub transition: f64,
    /// Stop-band attenuation in dB.
    pub attenuation_db: f64,
}

impl Default for LowpassSpec {
    fn default() -> Self {
        Self { cutoff: 0.15, transition: 0.05, attenuation_db: 60.0 }
    }
}

impl LowpassSpec {
    pub fn kaiser_beta(&self) -> f64 {
        let a = self.attenuation_db;
        if a > 50.0 {
            0.1102 * (a - 8.7)
        } else if a >= 21.0 {
            0.5842 * (a - 21.0).powf(0.4) + 0.07886 * (a - 21.0)
        } else {
            0.0
        }
    }

    /// Odd tap count from the Kaiser length formula.
    pub fn taps(&self) -> usize {
        let dw = 2.0 * PI * self.transition;
        // Kaiser estimate N ≈ (A − 8) / (2.285·Δω); rounded up to an odd length
        // (73 for the SPEC defaults).
        let n = ((self.attenuation_db - 8.0) / (2.285 * dw)).ceil() as usize;
        if n.is_multiple_of(2) {
            n + 1
        } else {
            n
        }
    }

    /// Symmetric odd-length kernel with exactly unit DC gain and an exact null
    /// at Nyquist (0.5 c/px), so a flat colour field separates into a flat
    /// luminance with no 2-px residue at all (the carriers sit at Nyquist).
    pub fn kernel(&self) -> Vec<f64> {
        let n = self.taps();
        let m = (n - 1) as f64 / 2.0;
        let beta = self.kaiser_beta();
        let i0b = bessel_i0(beta);
        let fc = self.cutoff;
        let mut h: Vec<f64> = (0..n)
            .map(|i| {
                let t = i as f64 - m;
                let sinc = if t == 0.0 { 2.0 * fc } else { (2.0 * PI * fc * t).sin() / (PI * t) };
                let r = 2.0 * t / (n as f64 - 1.0);
                let w = bessel_i0(beta * (1.0 - r * r).max(0.0).sqrt()) / i0b;
                sinc * w
            })
            .collect();
        // Solve for a + b·(−1)^i added to every tap so that Σh = 1 and
        // Σh·(−1)^i = 0. For odd n, Σ(−1)^i = 1.
        let s: f64 = h.iter().sum();
        let t: f64 = h.iter().enumerate().map(|(i, v)| if i % 2 == 0 { *v } else { -*v }).sum();
        let nf = n as f64;
        let det = nf * nf - 1.0;
        let a = ((1.0 - s) * nf - (-t)) / det;
        let b = ((-t) * nf - (1.0 - s)) / det;
        for (i, v) in h.iter_mut().enumerate() {
            *v += a + if i % 2 == 0 { b } else { -b };
        }
        h
    }
}

/// reflect-101 index: ... 2 1 | 0 1 2 ... n-2 n-1 | n-2 n-3 ...
#[inline]
fn reflect(i: isize, n: usize) -> usize {
    let n = n as isize;
    if n == 1 {
        return 0;
    }
    let period = 2 * (n - 1);
    let mut j = i.rem_euclid(period);
    if j >= n {
        j = period - j;
    }
    j as usize
}

/// Outputs computed together in registers: the taps stream past a fixed block
/// of accumulators (independent lanes vectorise; a per-pixel dot product does
/// not, since f64 sums are never reassociated). Every output still sums its
/// taps in index order t = 0..n, so results are deterministic and independent
/// of the thread count.
const LANES: usize = 16;
/// Output rows per band in the column pass: the band plus its tap halo of
/// source rows, one x-strip wide, stays in L2.
const Y_BAND: usize = 32;
const X_STRIP: usize = 512;

#[inline(always)]
fn fma_lanes(acc: &mut [f64; LANES], kv: f64, s: &[f64; LANES]) {
    for j in 0..LANES {
        acc[j] += kv * s[j];
    }
}

/// Convolve rows with `k` (odd length), reflect-101 borders, output has the
/// same size. `carrier(x, y)` multiplies each source sample first (the Bayer
/// carriers are ±1, exact), so demodulation costs no extra pass.
pub fn convolve_rows_mod_into(
    src: &Plane,
    k: &[f64],
    carrier: impl Fn(usize, usize) -> f64 + Sync,
    out: &mut Plane,
) {
    let w = src.width;
    let half = k.len() / 2;
    assert!(out.width == w && out.height == src.height, "convolve_rows: size mismatch");
    out.data.par_chunks_mut(w).enumerate().for_each_init(
        || vec![0.0f64; w + 2 * half + LANES],
        |pad, (y, orow)| {
            let row = src.row(y);
            for (i, p) in pad.iter_mut().enumerate().take(w + 2 * half) {
                let xr = reflect(i as isize - half as isize, w);
                *p = carrier(xr, y) * row[xr];
            }
            let mut x0 = 0;
            while x0 < w {
                let n = (w - x0).min(LANES);
                let mut acc = [0.0f64; LANES];
                for (t, &kv) in k.iter().enumerate() {
                    let s: &[f64; LANES] = pad[x0 + t..x0 + t + LANES].try_into().expect("pad slack");
                    fma_lanes(&mut acc, kv, s);
                }
                orow[x0..x0 + n].copy_from_slice(&acc[..n]);
                x0 += LANES;
            }
        },
    );
}

pub fn convolve_rows_mod(src: &Plane, k: &[f64], carrier: impl Fn(usize, usize) -> f64 + Sync) -> Plane {
    let mut out = Plane::zeros(src.width, src.height);
    convolve_rows_mod_into(src, k, carrier, &mut out);
    out
}

pub fn convolve_rows(src: &Plane, k: &[f64]) -> Plane {
    convolve_rows_mod(src, k, |_, _| 1.0)
}

/// Convolve columns with `k` (odd length), reflect-101 borders. `row_sign(sy)`
/// multiplies source row `sy` (±1 for the Bayer carriers, exact). Same tap
/// order per output as the row pass.
pub fn convolve_cols_signed_into(
    src: &Plane,
    k: &[f64],
    row_sign: impl Fn(usize) -> f64 + Sync,
    out: &mut Plane,
) {
    let (w, h) = (src.width, src.height);
    let half = k.len() as isize / 2;
    let taps = k.len();
    assert!(out.width == w && out.height == h, "convolve_cols: size mismatch");
    out.data.par_chunks_mut(w * Y_BAND).enumerate().for_each_init(
        || (Vec::<usize>::new(), Vec::<f64>::new()),
        |(sy_tab, kk), (band, oband)| {
            let y0 = band * Y_BAND;
            let rows = oband.len() / w;
            // Source row and signed tap per (dy, t), once per band.
            sy_tab.clear();
            kk.clear();
            for dy in 0..rows {
                for (t, &kv) in k.iter().enumerate() {
                    let sy = reflect((y0 + dy) as isize + t as isize - half, h);
                    sy_tab.push(sy);
                    kk.push(kv * row_sign(sy));
                }
            }
            for x0 in (0..w).step_by(X_STRIP) {
                let x1 = (x0 + X_STRIP).min(w);
                for dy in 0..rows {
                    let orow = &mut oband[dy * w..(dy + 1) * w];
                    let tab = &sy_tab[dy * taps..(dy + 1) * taps];
                    let kt = &kk[dy * taps..(dy + 1) * taps];
                    let mut xb = x0;
                    while xb < x1 {
                        let n = (x1 - xb).min(LANES);
                        let mut acc = [0.0f64; LANES];
                        if n == LANES {
                            for (&sy, &kv) in tab.iter().zip(kt) {
                                let s: &[f64; LANES] =
                                    src.row(sy)[xb..xb + LANES].try_into().expect("full block");
                                fma_lanes(&mut acc, kv, s);
                            }
                        } else {
                            for (&sy, &kv) in tab.iter().zip(kt) {
                                let s = &src.row(sy)[xb..xb + n];
                                for j in 0..n {
                                    acc[j] += kv * s[j];
                                }
                            }
                        }
                        orow[xb..xb + n].copy_from_slice(&acc[..n]);
                        xb += LANES;
                    }
                }
            }
        },
    );
}

pub fn convolve_cols_signed(src: &Plane, k: &[f64], row_sign: impl Fn(usize) -> f64 + Sync) -> Plane {
    let mut out = Plane::zeros(src.width, src.height);
    convolve_cols_signed_into(src, k, row_sign, &mut out);
    out
}

pub fn convolve_cols(src: &Plane, k: &[f64]) -> Plane {
    convolve_cols_signed(src, k, |_| 1.0)
}

/// Separable low-pass: rows then columns.
pub fn lowpass(src: &Plane, k: &[f64]) -> Plane {
    let t = std::time::Instant::now();
    let tmp = convolve_rows(src, k);
    let t_rows = t.elapsed().as_millis();
    let t = std::time::Instant::now();
    let out = convolve_cols(&tmp, k);
    tracing::trace!(
        "lowpass {}x{} {} taps: rows {} ms, cols {} ms",
        src.width,
        src.height,
        k.len(),
        t_rows,
        t.elapsed().as_millis()
    );
    out
}

/// Naive 2-D reference for tests (same kernel both axes, reflect-101).
pub fn lowpass_reference(src: &Plane, k: &[f64]) -> Plane {
    let (w, h) = (src.width, src.height);
    let half = (k.len() / 2) as isize;
    let mut out = Plane::zeros(w, h);
    for y in 0..h {
        for x in 0..w {
            let mut acc = 0.0;
            for (ty, &ky) in k.iter().enumerate() {
                let sy = reflect(y as isize + ty as isize - half, h);
                for (tx, &kx) in k.iter().enumerate() {
                    let sx = reflect(x as isize + tx as isize - half, w);
                    acc += ky * kx * src.at(sx, sy);
                }
            }
            out.set(x, y, acc);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kernel_defaults() {
        let s = LowpassSpec::default();
        assert_eq!(s.taps(), 73);
        let k = s.kernel();
        assert_eq!(k.len(), 73);
        assert!((k.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        let nyq: f64 = k.iter().enumerate().map(|(i, v)| if i % 2 == 0 { *v } else { -*v }).sum();
        assert!(nyq.abs() < 1e-12, "{nyq}");
        assert!((s.kaiser_beta() - 5.653).abs() < 1e-3);
    }

    /// Frequency response: pass at 0.1, half at cutoff, stop at 0.2.
    #[test]
    fn kernel_response() {
        let s = LowpassSpec::default();
        let k = s.kernel();
        let resp = |f: f64| -> f64 {
            let m = (k.len() - 1) as f64 / 2.0;
            k.iter().enumerate().map(|(i, &v)| v * (2.0 * PI * f * (i as f64 - m)).cos()).sum::<f64>().abs()
        };
        assert!(resp(0.0) > 0.9999);
        assert!(resp(0.10) > 0.995, "{}", resp(0.10));
        assert!((resp(0.15) - 0.5).abs() < 0.05, "{}", resp(0.15));
        assert!(resp(0.20) < 0.002, "{}", resp(0.20));
        assert!(resp(0.5) < 0.002);
    }

    #[test]
    fn reflect_101() {
        assert_eq!(reflect(-1, 5), 1);
        assert_eq!(reflect(-2, 5), 2);
        assert_eq!(reflect(5, 5), 3);
        assert_eq!(reflect(6, 5), 2);
        assert_eq!(reflect(2, 5), 2);
    }

    #[test]
    fn separable_matches_reference() {
        let (w, h) = (23, 17);
        let mut p = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                p.set(x, y, ((x * 7 + y * 13) % 11) as f64 / 11.0 + if (x + y) % 2 == 0 { 0.3 } else { 0.0 });
            }
        }
        let k = LowpassSpec { cutoff: 0.2, transition: 0.1, attenuation_db: 40.0 }.kernel();
        let a = lowpass(&p, &k);
        let b = lowpass_reference(&p, &k);
        for (x, y) in a.data.iter().zip(&b.data) {
            assert!((x - y).abs() < 1e-12);
        }
    }

    #[test]
    fn dc_passes_checkerboard_is_removed() {
        let (w, h) = (256, 256);
        let mut p = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                p.set(x, y, 0.5 + if (x + y) % 2 == 0 { 0.2 } else { -0.2 });
            }
        }
        let k = LowpassSpec::default().kernel();
        let f = lowpass(&p, &k);
        let c = f.at(128, 128);
        assert!((c - 0.5).abs() < 1e-6, "{c}");
    }
}
