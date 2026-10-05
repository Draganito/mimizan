//! Luminance / chrominance separation (SPEC section 5).

use crate::cfa::BayerPhase;
use crate::filter::{convolve_cols_signed, convolve_rows_mod, convolve_rows_mod_into, LowpassSpec};
use crate::ingest::Mosaic;
use crate::plane::Plane;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaskMode {
    /// Fixed separation, g1 = g2 = 1 (Phase 2 estimator).
    Off,
    /// Block-spectrum mask with Dubois weighting of the two C2 copies.
    Adaptive,
    /// Pixel-adaptive weighting of the two C2 copies and of two
    /// direction-dependent C1 bands, elongated pass bands (`dubois.rs`).
    /// Default.
    Dubois,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeparateParams {
    pub cutoff: f64,
    pub transition: f64,
    pub attenuation_db: f64,
    pub mask: MaskMode,
    pub rounds_max: usize,
    /// Parameters of the `Dubois` mode (ignored otherwise).
    #[serde(default)]
    pub dubois: crate::dubois::DuboisParams,
    /// Optional nonlinear reconstruction applied after the separation
    /// (`reconstruct.rs`). `None` keeps every pixel a fixed-weight sum of
    /// measured values; `Some` lets the detector replace doubtful pixels by
    /// a directional colour-difference estimate.
    #[serde(default)]
    pub reconstruct: Option<crate::reconstruct::ReconstructParams>,
}

impl Default for SeparateParams {
    fn default() -> Self {
        // rounds_max = 1: the cross-term refinement changed no bench figure
        // beyond the fourth digit while doubling the run time (RESULTS.md).
        Self {
            cutoff: 0.15,
            transition: 0.05,
            attenuation_db: 60.0,
            mask: MaskMode::Dubois,
            rounds_max: 1,
            dubois: Default::default(),
            reconstruct: None,
        }
    }
}

impl SeparateParams {
    pub fn lowpass_spec(&self) -> LowpassSpec {
        LowpassSpec { cutoff: self.cutoff, transition: self.transition, attenuation_db: self.attenuation_db }
    }
}

/// Result of the separation: everything the mixer and the writers need.
#[derive(Clone, Debug)]
pub struct Separation {
    /// Estimated luminance `L̂` (full resolution).
    pub lum: Plane,
    /// Low-pass chrominance `Ĉ1'`, mask gain already folded in.
    pub c1: Plane,
    /// Combined low-pass chrominance `Ĉ2'`, mask gain already folded in.
    pub c2: Plane,
    /// `max(M_1, M_2a, M_2b)` per pixel in [0,1]; `FIXED_MASK_VALUE` everywhere
    /// in the `Off` and `Dubois` modes.
    pub mask_max: Plane,
    pub rounds: usize,
    pub phase: BayerPhase,
    /// Mean β of the nonlinear reconstruction: the fraction of the picture
    /// that was computed rather than filtered. 0 without `reconstruct`.
    pub computed: f64,
}

#[inline]
pub fn sign_x(x: usize) -> f64 {
    if x & 1 == 0 {
        1.0
    } else {
        -1.0
    }
}

/// `out[y][x] = f(x, y) * src[y][x]` with f one of the three carriers.
pub fn modulate(src: &Plane, carrier: impl Fn(f64, f64) -> f64 + Sync) -> Plane {
    let w = src.width;
    let mut out = Plane::zeros(w, src.height);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = sign_x(y);
        let s = src.row(y);
        for (x, (o, &v)) in row.iter_mut().zip(s).enumerate() {
            *o = carrier(sign_x(x), sy) * v;
        }
    });
    out
}

/// Fixed-filter estimate of the three chrominance planes from a mosaic `m`:
/// `Ĉ1 = LP(ε·sx·sy·m)`, `Ĉ2a = LP(a·sx·m)`, `Ĉ2b = LP(b·sy·m)`.
///
/// The carriers are ±1 per pixel, so the row pass over `sx·m` serves both Ĉ1
/// and Ĉ2a (the remaining `ε·sy` / `a` factors are constant per row and move
/// into the column pass), and the row pass over `m` serves Ĉ2b. Two row passes
/// and three column passes instead of three modulations and six passes; the
/// per-output tap order is unchanged, so the result is bit-identical.
pub fn chroma_estimates(m: &Plane, phase: BayerPhase, k: &[f64]) -> (Plane, Plane, Plane) {
    let c = phase.carriers();
    let mut r = convolve_rows_mod(m, k, |x, _| sign_x(x));
    let c1 = convolve_cols_signed(&r, k, |y| c.eps * sign_x(y));
    let c2a = convolve_cols_signed(&r, k, |_| c.a);
    convolve_rows_mod_into(m, k, |_, _| 1.0, &mut r);
    let c2b = convolve_cols_signed(&r, k, |y| c.b * sign_x(y));
    (c1, c2a, c2b)
}

/// `L̂ = m − ε·sx·sy·Ĉ1 − (a·sx + b·sy)·Ĉ2` (any gains are already inside Ĉ).
pub fn luminance(m: &Plane, phase: BayerPhase, c1: &Plane, c2: &Plane) -> Plane {
    let c = phase.carriers();
    let w = m.width;
    let mut out = Plane::zeros(w, m.height);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = sign_x(y);
        let (mr, c1r, c2r) = (m.row(y), c1.row(y), c2.row(y));
        for x in 0..w {
            let sx = sign_x(x);
            row[x] = mr[x] - c.eps * sx * sy * c1r[x] - (c.a * sx + c.b * sy) * c2r[x];
        }
    });
    out
}

/// `a ← (a + b) / 2` in place.
pub fn average_in_place(a: &mut Plane, b: &Plane) {
    a.data.par_iter_mut().zip(&b.data).for_each(|(x, &y)| *x = 0.5 * (*x + y));
}

/// Mask value the fixed estimator reports everywhere: the carriers are
/// suppressed by the 60 dB low-pass in every block, so output sharpening is
/// allowed everywhere except at saturation (the sidecar's 255). The value is
/// exactly the sharpening gate's threshold, so the gate reads it as "allowed".
pub const FIXED_MASK_VALUE: f64 = crate::usm::MASK_THRESHOLD;

fn fixed(m: &Plane, phase: BayerPhase, c1: Plane, mut c2a: Plane, c2b: Plane) -> Separation {
    average_in_place(&mut c2a, &c2b);
    drop(c2b);
    let lum = luminance(m, phase, &c1, &c2a);
    let mask_max = Plane::from_vec(m.width, m.height, vec![FIXED_MASK_VALUE; m.width * m.height]);
    Separation { lum, c1, c2: c2a, mask_max, rounds: 1, phase, computed: 0.0 }
}

pub fn separate(mosaic: &Mosaic, p: &SeparateParams) -> Separation {
    let mut sep = separate_linear(mosaic, p);
    if let Some(r) = &p.reconstruct {
        let t = std::time::Instant::now();
        sep.computed = crate::reconstruct::apply(&mut sep, &mosaic.plane, &mosaic.noise, r);
        tracing::debug!("reconstruct {} ms, computed {:.3}", t.elapsed().as_millis(), sep.computed);
    }
    sep
}

fn separate_linear(mosaic: &Mosaic, p: &SeparateParams) -> Separation {
    let phase = mosaic.phase.expect("separate() needs a Bayer mosaic");
    let m = &mosaic.plane;
    if p.mask == MaskMode::Dubois {
        return crate::dubois::separate_dubois(mosaic, phase, &p.dubois);
    }
    let k = p.lowpass_spec().kernel();
    let t = std::time::Instant::now();
    let (c1, c2a, c2b) = chroma_estimates(m, phase, &k);
    tracing::debug!("chroma estimates ({} taps) {} ms", k.len(), t.elapsed().as_millis());

    match p.mask {
        MaskMode::Off => fixed(m, phase, c1, c2a, c2b),
        MaskMode::Dubois => unreachable!(),
        MaskMode::Adaptive => {
            if crate::mask::grid_dims(m.width, m.height) == (0, 0) {
                // Too small for a block grid: fall back to the fixed estimator.
                return fixed(m, phase, c1, c2a, c2b);
            }
            crate::mask::separate_adaptive(mosaic, phase, &k, c1, c2a, c2b, p)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfa::Color;

    fn mosaic_of(
        phase: BayerPhase,
        w: usize,
        h: usize,
        rgb: impl Fn(usize, usize) -> (f64, f64, f64),
    ) -> Plane {
        let mut m = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = rgb(x, y);
                let v = match phase.color_at(x, y) {
                    Color::R => r,
                    Color::G => g,
                    Color::B => b,
                };
                m.set(x, y, v);
            }
        }
        m
    }

    /// A flat colour field must come out as flat L = (R+2G+B)/4 with no 2-px grid.
    #[test]
    fn flat_colour_field_separates_exactly() {
        let (w, h) = (192, 160);
        let (r, g, b) = (0.6, 0.3, 0.1);
        let l_true = (r + 2.0 * g + b) / 4.0;
        let k = LowpassSpec::default().kernel();
        for phase in BayerPhase::ALL {
            let m = mosaic_of(phase, w, h, |_, _| (r, g, b));
            let (c1, mut c2, c2b) = chroma_estimates(&m, phase, &k);
            average_in_place(&mut c2, &c2b);
            let lum = luminance(&m, phase, &c1, &c2);
            for y in 40..h - 40 {
                for x in 40..w - 40 {
                    assert!((lum.at(x, y) - l_true).abs() < 1e-9, "{phase} ({x},{y}) {}", lum.at(x, y));
                    assert!((c1.at(x, y) - (-r + 2.0 * g - b) / 4.0).abs() < 1e-9);
                    assert!((c2.at(x, y) - (r - b) / 4.0).abs() < 1e-9);
                }
            }
        }
    }

    /// A neutral low-frequency ramp is luminance only and must pass untouched.
    #[test]
    fn neutral_ramp_is_preserved() {
        let (w, h) = (256, 128);
        let k = LowpassSpec::default().kernel();
        let m = mosaic_of(BayerPhase::RGGB, w, h, |x, _| {
            let v = 0.2 + 0.6 * (x as f64 / w as f64);
            (v, v, v)
        });
        let (c1, mut c2, c2b) = chroma_estimates(&m, BayerPhase::RGGB, &k);
        average_in_place(&mut c2, &c2b);
        let lum = luminance(&m, BayerPhase::RGGB, &c1, &c2);
        for y in 40..h - 40 {
            for x in 40..w - 40 {
                assert!((lum.at(x, y) - m.at(x, y)).abs() < 1e-9);
            }
        }
    }
}
