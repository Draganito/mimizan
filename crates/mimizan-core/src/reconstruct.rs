//! Optional nonlinear reconstruction of the last octave (SPEC 5.6).
//!
//! The linear separation cannot tell luminance from chrominance at the
//! carriers: an achromatic line pattern near Nyquist and a sharp colour edge
//! look alike in the spectrum. Demosaicers resolve that with a prior, the
//! local smoothness of the colour differences `G − R`, `G − B`, and with a
//! per-pixel direction decision. This module does the same, for the pixels
//! where the linear estimate is in doubt, and records per pixel how much of
//! the result was computed that way. Off by default: a pixel that passed
//! through here is no longer a weighted sum of measured values with fixed
//! weights but a decision.
//!
//! Estimator: directional colour-difference signals along rows and columns
//! (Hamilton–Adams-like interpolation of the missing channel), smoothed and
//! fused per pixel by their local error variances (the LMMSE scheme of Zhang
//! and Wu 2005, written from the paper), then R and B by colour-difference
//! interpolation. It runs in a compressed domain (`m^(1/gamma)`): colour
//! differences are nearly constant across an edge there and far from it in
//! linear light, which is worth 5 dB on pixel-sharp content. The three
//! planes `L, C1, C2` of that estimate replace the linear ones where the
//! detector asks for it: where the linear luminance still carries energy
//! near the carriers above the noise.

use crate::cfa::{BayerPhase, Color};
use crate::ingest::NoiseModel;
use crate::plane::Plane;
use crate::separate::{sign_x, Separation, FIXED_MASK_VALUE};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Detector {
    /// Every pixel from the nonlinear estimate (β = 1).
    All,
    /// Carrier-band energy left in the linear luminance, relative to noise
    /// (default). Where the linear estimate still carries energy near the
    /// carriers, luminance and chrominance overlapped and the direction
    /// decision has something to decide; elsewhere the linear estimate stays.
    Residue,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct ReconstructParams {
    pub detector: Detector,
    /// β rises from 0 at `lo` to 1 at `hi` (detector units: energy / noise variance).
    pub lo: f64,
    pub hi: f64,
    /// Gaussian σ (px) of the detector window.
    pub sigma: f64,
    /// Tie-breaker of the direction fusion: weight 1/(err + eps + mu·d²),
    /// so where both directions are self-consistent the one implying less
    /// chrominance wins (a neutral Nyquist pattern over a saturated one).
    pub mu: f64,
    /// Processing domain: the mosaic is raised to `1/gamma` before the
    /// estimator and the result decoded again. Colour differences are far more
    /// constant across an edge in a compressed domain than in linear light;
    /// 1.0 leaves everything linear.
    pub gamma: f64,
}

impl Default for ReconstructParams {
    fn default() -> Self {
        Self { detector: Detector::Residue, lo: 20.0, hi: 80.0, sigma: 2.0, mu: 0.01, gamma: 2.2 }
    }
}

/// Directional interpolation of the missing channel: neighbour filter
/// `[-A, 0.5 + A, 0.5 + A, -A]` at ±1/±3 plus `LAP` times the Laplacian of the
/// own channel (Hamilton–Adams is `A = 0, LAP = 0.25`).
const A: f64 = 0.0625;
const LAP: f64 = 0.125;

#[inline]
fn refl(i: isize, n: usize) -> usize {
    let n = n as isize;
    let mut i = i;
    if i < 0 {
        i = -i;
    }
    if i >= n {
        i = 2 * n - 2 - i;
    }
    i.clamp(0, n - 1) as usize
}

/// Directional estimate of the missing channel at `(x, y)` along x.
#[inline]
fn ha_row(r: &[f64], x: usize, w: usize) -> f64 {
    let xi = x as isize;
    (0.5 + A) * (r[refl(xi - 1, w)] + r[refl(xi + 1, w)]) - A * (r[refl(xi - 3, w)] + r[refl(xi + 3, w)])
        + LAP * (2.0 * r[x] - r[refl(xi - 2, w)] - r[refl(xi + 2, w)])
}

#[inline]
fn ha_col(m: &Plane, x: usize, y: usize) -> f64 {
    let yi = y as isize;
    let h = m.height;
    let v = |yy: usize| m.at(x, yy);
    (0.5 + A) * (v(refl(yi - 1, h)) + v(refl(yi + 1, h))) - A * (v(refl(yi - 3, h)) + v(refl(yi + 3, h)))
        + LAP * (2.0 * v(y) - v(refl(yi - 2, h)) - v(refl(yi + 2, h)))
}

/// Directional colour-difference signal `G − X` along rows (`dh`) and
/// columns (`dv`), defined at every pixel: at a G site the X of its line is
/// interpolated, at an X site the G.
fn directional_differences(m: &Plane, phase: BayerPhase) -> (Plane, Plane) {
    let (w, h) = (m.width, m.height);
    let mut dh = Plane::zeros(w, h);
    dh.data.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        let r = m.row(y);
        for x in 0..w {
            let est = ha_row(r, x, w);
            out[x] = match phase.color_at(x, y) {
                Color::G => r[x] - est,
                _ => est - r[x],
            };
        }
    });
    let mut dv = Plane::zeros(w, h);
    dv.data.par_chunks_mut(w).enumerate().for_each(|(y, out)| {
        for (x, o) in out.iter_mut().enumerate() {
            let est = ha_col(m, x, y);
            *o = match phase.color_at(x, y) {
                Color::G => m.at(x, y) - est,
                _ => est - m.at(x, y),
            };
        }
    });
    (dh, dv)
}

const SMOOTH: [f64; 9] = [
    4.0 / 128.0,
    9.0 / 128.0,
    15.0 / 128.0,
    23.0 / 128.0,
    26.0 / 128.0,
    23.0 / 128.0,
    15.0 / 128.0,
    9.0 / 128.0,
    4.0 / 128.0,
];
const STAT_HALF: isize = 4;

/// LMMSE refinement of a directional signal along its own direction: the
/// smoothed signal is the prior mean, the local variance of the smoothed
/// signal the signal power, the local residual variance the noise power.
/// Returns the estimate and its error variance per pixel.
fn lmmse_line(d: &[f64], out: &mut [f64], err: &mut [f64], scratch: &mut Vec<f64>) {
    let n = d.len();
    scratch.clear();
    scratch.resize(n, 0.0);
    for i in 0..n {
        let mut s = 0.0;
        for (t, k) in SMOOTH.iter().enumerate() {
            s += k * d[refl(i as isize + t as isize - 4, n)];
        }
        scratch[i] = s;
    }
    for i in 0..n {
        let (mut ms, mut vs, mut vn) = (0.0, 0.0, 0.0);
        let cnt = (2 * STAT_HALF + 1) as f64;
        for t in -STAT_HALF..=STAT_HALF {
            let j = refl(i as isize + t, n);
            ms += scratch[j];
            vn += (d[j] - scratch[j]).powi(2);
        }
        ms /= cnt;
        for t in -STAT_HALF..=STAT_HALF {
            let j = refl(i as isize + t, n);
            vs += (scratch[j] - ms).powi(2);
        }
        vs /= cnt;
        vn /= cnt;
        let g = if vs + vn > 1e-30 { vs / (vs + vn) } else { 0.0 };
        out[i] = scratch[i] + g * (d[i] - scratch[i]);
        err[i] = if vs + vn > 1e-30 { vs * vn / (vs + vn) } else { 0.0 };
    }
}

/// Full-resolution `(L, C1, C2)` of the nonlinear estimate.
pub fn estimate_lcc(m: &Plane, phase: BayerPhase, mu: f64, gamma: f64) -> (Plane, Plane, Plane) {
    let (w, h) = (m.width, m.height);
    let enc;
    let m = if gamma != 1.0 {
        enc = Plane::from_vec(w, h, m.data.par_iter().map(|v| v.max(0.0).powf(1.0 / gamma)).collect());
        &enc
    } else {
        m
    };
    let (g, rg, bg) = estimate_grb(m, phase, mu);
    let mut l = Plane::zeros(w, h);
    let mut c1 = Plane::zeros(w, h);
    let mut c2 = Plane::zeros(w, h);
    l.data
        .par_iter_mut()
        .zip(c1.data.par_iter_mut())
        .zip(c2.data.par_iter_mut())
        .zip(&g.data)
        .zip(&rg.data)
        .zip(&bg.data)
        .for_each(|(((((l, c1), c2), g), rg), bg)| {
            let dec = |v: f64| if gamma != 1.0 { v.max(0.0).powf(gamma) } else { v };
            let gg = dec(*g);
            let r = dec(g + rg);
            let b = dec(g + bg);
            *l = 0.25 * (r + 2.0 * gg + b);
            *c1 = 0.25 * (2.0 * gg - r - b);
            *c2 = 0.25 * (r - b);
        });
    (l, c1, c2)
}

/// Green plane plus `R − G` and `B − G` everywhere.
fn estimate_grb(m: &Plane, phase: BayerPhase, mu: f64) -> (Plane, Plane, Plane) {
    let (w, h) = (m.width, m.height);
    let (dh, dv) = directional_differences(m, phase);

    // Row-wise LMMSE on dh.
    let mut eh = Plane::zeros(w, h);
    let mut veh = Plane::zeros(w, h);
    eh.data.par_chunks_mut(w).zip(veh.data.par_chunks_mut(w)).enumerate().for_each_init(
        Vec::new,
        |scratch, (y, (o, e))| {
            lmmse_line(dh.row(y), o, e, scratch);
        },
    );
    drop(dh);
    // Column-wise LMMSE on dv: transpose, process rows, transpose back.
    let (ev, vev) = {
        let dvt = transpose(&dv);
        drop(dv);
        let mut evt = Plane::zeros(h, w);
        let mut vevt = Plane::zeros(h, w);
        evt.data.par_chunks_mut(h).zip(vevt.data.par_chunks_mut(h)).enumerate().for_each_init(
            Vec::new,
            |scratch, (x, (o, e))| {
                lmmse_line(dvt.row(x), o, e, scratch);
            },
        );
        (transpose(&evt), transpose(&vevt))
    };
    // Fused G − X at the X sites, and the green plane.
    let mut g = m.clone();
    g.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, o) in row.iter_mut().enumerate() {
            if phase.color_at(x, y) != Color::G {
                let (a, b) = (eh.at(x, y), ev.at(x, y));
                let wa = 1.0 / (veh.at(x, y) + 1e-12 + mu * a * a);
                let wb = 1.0 / (vev.at(x, y) + 1e-12 + mu * b * b);
                *o += (wa * a + wb * b) / (wa + wb);
            }
        }
    });
    drop((veh, vev));
    // eh / ev are reused as the R − G and B − G planes.
    // First at the measured sites: X − G.
    let (mut rg, mut bg) = (Plane::zeros(w, h), Plane::zeros(w, h));
    rg.data.par_chunks_mut(w).zip(bg.data.par_chunks_mut(w)).enumerate().for_each(|(y, (rr, br))| {
        let (mr, gr) = (m.row(y), g.row(y));
        for x in 0..w {
            match phase.color_at(x, y) {
                Color::R => rr[x] = mr[x] - gr[x],
                Color::B => br[x] = mr[x] - gr[x],
                Color::G => {}
            }
        }
    });
    // At the opposite chroma site: mean of the four diagonal neighbours.
    let fill_diag = |p: &mut Plane, own: Color| {
        let src = p.clone();
        p.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                let c = phase.color_at(x, y);
                if c != Color::G && c != own {
                    let (xi, yi) = (x as isize, y as isize);
                    let s = src.at(refl(xi - 1, w), refl(yi - 1, h))
                        + src.at(refl(xi + 1, w), refl(yi - 1, h))
                        + src.at(refl(xi - 1, w), refl(yi + 1, h))
                        + src.at(refl(xi + 1, w), refl(yi + 1, h));
                    *o = 0.25 * s;
                }
            }
        });
    };
    fill_diag(&mut rg, Color::R);
    fill_diag(&mut bg, Color::B);
    // At G sites: mean of the two neighbours that carry the channel (row or column).
    let fill_g = |p: &mut Plane, own: Color| {
        let src = p.clone();
        p.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            for (x, o) in row.iter_mut().enumerate() {
                if phase.color_at(x, y) == Color::G {
                    let (xi, yi) = (x as isize, y as isize);
                    let horizontal = phase.color_at(refl(xi + 1, w), y) == own
                        || phase.color_at(refl(xi - 1, w), y) == own;
                    *o = if horizontal {
                        0.5 * (src.at(refl(xi - 1, w), y) + src.at(refl(xi + 1, w), y))
                    } else {
                        0.5 * (src.at(x, refl(yi - 1, h)) + src.at(x, refl(yi + 1, h)))
                    };
                }
            }
        });
    };
    fill_g(&mut rg, Color::R);
    fill_g(&mut bg, Color::B);
    drop((eh, ev));
    (g, rg, bg)
}

fn transpose(p: &Plane) -> Plane {
    let (w, h) = (p.width, p.height);
    let mut t = Plane::zeros(h, w);
    // Blocks of 32 output rows: the strided source reads then stay in cache.
    const B: usize = 32;
    t.data.par_chunks_mut(h * B).enumerate().for_each(|(bx, block)| {
        let x0 = bx * B;
        let nx = block.len() / h;
        for y0 in (0..h).step_by(B) {
            let y1 = (y0 + B).min(h);
            for y in y0..y1 {
                let row = p.row(y);
                for i in 0..nx {
                    block[i * h + y] = row[x0 + i];
                }
            }
        }
    });
    t
}

fn gaussian_kernel(sigma: f64) -> Vec<f64> {
    let r = (3.0 * sigma).ceil().max(1.0) as usize;
    let k: Vec<f64> = (0..=2 * r)
        .map(|i| {
            let d = i as f64 - r as f64;
            (-d * d / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let s: f64 = k.iter().sum();
    k.into_iter().map(|v| v / s).collect()
}

/// Demodulate the linear luminance by the three carriers and low-pass each:
/// summed squared amplitude of what is left near the carriers. Same pass
/// sharing as `separate::chroma_estimates`: two row passes serve three
/// column passes.
fn carrier_residue(lum: &Plane, c: &crate::cfa::Carriers, g: &[f64]) -> Plane {
    use crate::filter::{
        convolve_cols_signed, convolve_cols_signed_into, convolve_rows_mod, convolve_rows_mod_into,
    };
    let mut r = convolve_rows_mod(lum, g, |x, _| sign_x(x));
    let mut e = convolve_cols_signed(&r, g, |y| c.eps * sign_x(y));
    let mut sum = Plane::from_vec(lum.width, lum.height, e.data.par_iter().map(|v| v * v).collect());
    convolve_cols_signed_into(&r, g, |_| c.a, &mut e);
    sum.data.par_iter_mut().zip(&e.data).for_each(|(s, v)| *s += v * v);
    convolve_rows_mod_into(lum, g, |_, _| 1.0, &mut r);
    convolve_cols_signed_into(&r, g, |y| c.b * sign_x(y), &mut e);
    sum.data.par_iter_mut().zip(&e.data).for_each(|(s, v)| *s += v * v);
    sum
}

/// Detector statistic per pixel, before the ramp (energy / noise variance).
pub fn detector_plane(
    phase: BayerPhase,
    noise: &NoiseModel,
    lum_linear: &Plane,
    p: &ReconstructParams,
) -> Plane {
    let (w, h) = (lum_linear.width, lum_linear.height);
    let g = gaussian_kernel(p.sigma);
    let c = phase.carriers();
    match p.detector {
        Detector::All => Plane::from_vec(w, h, vec![f64::INFINITY; w * h]),
        Detector::Residue => {
            let mut sum = carrier_residue(lum_linear, &c, &g);
            // The low-pass of the demodulated signal is an amplitude; its
            // square per carrier, summed, against the noise variance in the
            // same band (window energy of the Gaussian).
            let we: f64 = g.iter().map(|v| v * v).sum::<f64>().powi(2);
            sum.data.par_iter_mut().zip(&lum_linear.data).for_each(|(s, l)| {
                let nv = noise.variance(*l).max(1e-18) * we;
                *s /= nv;
            });
            sum
        }
    }
}

/// β per pixel from the detector statistic: 0 below `lo`, 1 above `hi`.
pub fn beta_from(stat: &Plane, p: &ReconstructParams) -> Plane {
    let (lo, hi) = (p.lo, p.hi.max(p.lo + 1e-9));
    Plane::from_vec(
        stat.width,
        stat.height,
        stat.data.par_iter().map(|s| ((s - lo) / (hi - lo)).clamp(0.0, 1.0)).collect(),
    )
}

/// Apply the reconstruction to a separation in place: blend `L, C1, C2`
/// towards the nonlinear estimate by β and record β in `mask_max`
/// (`FIXED_MASK_VALUE` at β = 0, 0.99 at β = 1, below the saturation code).
pub fn apply(sep: &mut Separation, m: &Plane, noise: &NoiseModel, p: &ReconstructParams) -> f64 {
    let phase = sep.phase;
    let (l, c1, c2) = estimate_lcc(m, phase, p.mu, p.gamma);
    let stat = detector_plane(phase, noise, &sep.lum, p);
    let beta = beta_from(&stat, p);
    drop(stat);
    let blend = |dst: &mut Plane, src: &Plane| {
        dst.data.par_iter_mut().zip(&src.data).zip(&beta.data).for_each(|((d, s), b)| *d += b * (s - *d));
    };
    blend(&mut sep.lum, &l);
    blend(&mut sep.c1, &c1);
    blend(&mut sep.c2, &c2);
    let span = 0.99 - FIXED_MASK_VALUE;
    sep.mask_max.data.par_iter_mut().zip(&beta.data).for_each(|(mk, b)| *mk = FIXED_MASK_VALUE + span * b);
    beta.data.par_iter().sum::<f64>() / beta.data.len() as f64
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mosaic_of(w: usize, h: usize, rgb: impl Fn(usize, usize) -> (f64, f64, f64)) -> Plane {
        let phase = BayerPhase::RGGB;
        let mut m = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = rgb(x, y);
                m.set(
                    x,
                    y,
                    match phase.color_at(x, y) {
                        Color::R => r,
                        Color::G => g,
                        Color::B => b,
                    },
                );
            }
        }
        m
    }

    #[test]
    fn flat_colour_is_exact() {
        let (r, g, b) = (0.6, 0.3, 0.1);
        let m = mosaic_of(64, 48, |_, _| (r, g, b));
        let (l, c1, c2) = estimate_lcc(&m, BayerPhase::RGGB, 0.01, 2.2);
        for y in 8..40 {
            for x in 8..56 {
                assert!((l.at(x, y) - (r + 2.0 * g + b) / 4.0).abs() < 1e-12);
                assert!((c1.at(x, y) - (2.0 * g - r - b) / 4.0).abs() < 1e-12);
                assert!((c2.at(x, y) - (r - b) / 4.0).abs() < 1e-12);
            }
        }
    }

    /// A neutral vertical line pattern at Nyquist is pure luminance; the
    /// colour-difference model must reproduce it exactly.
    #[test]
    fn neutral_nyquist_lines_are_exact() {
        let f = |x: usize| if x.is_multiple_of(2) { 0.8 } else { 0.2 };
        let m = mosaic_of(64, 48, |x, _| (f(x), f(x), f(x)));
        let (l, _, _) = estimate_lcc(&m, BayerPhase::RGGB, 0.01, 2.2);
        for y in 8..40 {
            for x in 8..56 {
                assert!((l.at(x, y) - f(x)).abs() < 1e-9, "({x},{y}) {} vs {}", l.at(x, y), f(x));
            }
        }
    }
}
