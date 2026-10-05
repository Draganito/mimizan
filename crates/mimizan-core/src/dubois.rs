//! Pixel-adaptive separation after Dubois (2005).
//!
//! The chrominance `C2` is modulated onto two carriers, (0.5, 0) and (0, 0.5),
//! with the same content in both copies. Luminance leaks into a copy only
//! where the picture has energy near that carrier: vertical line patterns
//! (horizontal frequencies near Nyquist) spoil the (0.5, 0) copy, horizontal
//! patterns the (0, 0.5) copy, and a natural picture rarely has both at one
//! place. So, per pixel, the copy with the smaller local energy is the one
//! to trust: the chroma is in both, the extra energy is luminance.
//!
//! Each copy also gets an elongated pass band: narrow across the carrier
//! (towards the luminance), wide along it, where the only neighbour is `C1`.
//! Where the ring between the narrow and a wider square band is coherent
//! between the two copies, that ring is chroma (a leak is in one copy only)
//! and the band is opened to the square wide shape (`ring_*` parameters).
//! The block mask of `mask.rs` cannot do any of this: it decides per 64 px
//! block from band/ring energy ratios, which a line pattern inside the band
//! defeats.

use crate::cfa::BayerPhase;
use crate::filter::{convolve_cols_signed, convolve_cols_signed_into, convolve_rows_mod_into, LowpassSpec};
use crate::ingest::Mosaic;
use crate::plane::Plane;
use crate::separate::{luminance, sign_x, Separation, FIXED_MASK_VALUE};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct DuboisParams {
    /// `C1` pass band across the local structure direction, c/px.
    pub c1_narrow: f64,
    /// `C1` pass band along the local structure direction, c/px. Equal to
    /// `c1_narrow` gives the classic square band.
    pub c1_wide: f64,
    /// `C2` pass band across the carrier (towards the luminance), c/px.
    pub c2_narrow: f64,
    /// `C2` pass band along the carrier, c/px.
    pub c2_wide: f64,
    pub transition: f64,
    pub attenuation_db: f64,
    /// Gaussian σ (px) of the local energy estimate that weights the copies.
    pub energy_sigma: f64,
    /// Square pass band (c/px) opened where the band ring of the two C2
    /// copies is coherent; 0 disables the coherence step.
    pub ring_wide: f64,
    /// Exponent on the coherence.
    pub ring_power: f64,
    /// Also open the C1 band with the coherence.
    pub ring_c1: bool,
}

impl Default for DuboisParams {
    fn default() -> Self {
        Self {
            c1_narrow: 0.15,
            c1_wide: 0.30,
            c2_narrow: 0.15,
            c2_wide: 0.30,
            transition: 0.15,
            attenuation_db: 40.0,
            energy_sigma: 3.0,
            ring_wide: 0.25,
            ring_power: 2.0,
            ring_c1: true,
        }
    }
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

struct Kernels {
    k1n: Vec<f64>,
    k1w: Vec<f64>,
    kn: Vec<f64>,
    kw: Vec<f64>,
    kr: Vec<f64>,
    g: Vec<f64>,
}

/// Separable low-pass of `p` in place; `scratch` holds the row pass.
fn lowpass_in_place(p: &mut Plane, k: &[f64], scratch: &mut Plane) {
    convolve_rows_mod_into(p, k, |_, _| 1.0, scratch);
    convolve_cols_signed_into(scratch, k, |_| 1.0, p);
}

/// `p := G_σ * p²` in place.
fn local_energy_in_place(p: &mut Plane, g: &[f64], scratch: &mut Plane) {
    p.data.par_iter_mut().for_each(|v| *v *= *v);
    lowpass_in_place(p, g, scratch);
}

/// `a := a - b` in place (`a` holds the wide copy, `b` the narrow one).
fn ring_in_place(wide: &Plane, narrow: &mut Plane) {
    narrow.data.par_iter_mut().zip(&wide.data).for_each(|(n, w)| *n = w - *n);
}

/// Blend `a` and `b` in place into `a` with the per-pixel weight of `a`.
fn blend_in_place(a: &mut Plane, b: &Plane, wa: &Plane) {
    a.data.par_iter_mut().zip(&b.data).zip(&wa.data).for_each(|((a, b), w)| *a = w * *a + (1.0 - w) * b);
}

/// Open `base` towards `wide` by the per-pixel coherence `kappa`, in place.
fn open_in_place(base: &mut Plane, wide: &Plane, kappa: &Plane) {
    base.data.par_iter_mut().zip(&wide.data).zip(&kappa.data).for_each(|((b, w), k)| *b += k * (w - *b));
}

/// The estimation: returns `(c1, c2)`. Planes are dropped as early as the
/// data flow allows and the low-passes run in place; the peak is ten full
/// planes (mosaic included) with the coherence step, seven without.
fn estimate(m: &Plane, phase: BayerPhase, p: &DuboisParams, k: &Kernels) -> (Plane, Plane) {
    let c = phase.carriers();
    let (w, h) = (m.width, m.height);
    let mut row = Plane::zeros(w, h);

    // Hypothesis H (horizontal structure, spectrum along v): copy a at
    // (0.5, 0) is clean; bands narrow in u, wide in v. Hypothesis V
    // (vertical structure, spectrum along u): copy b at (0, 0.5) is clean;
    // bands wide in u, narrow in v. The square narrow copies are the inner
    // edge of the coherence ring.
    convolve_rows_mod_into(m, &k.kn, |x, _| sign_x(x), &mut row);
    let mut c2 = convolve_cols_signed(&row, &k.kw, |_| c.a);
    let mut ra = (p.ring_wide > 0.0).then(|| convolve_cols_signed(&row, &k.kn, |_| c.a));
    convolve_rows_mod_into(m, &k.kw, |_, _| 1.0, &mut row);
    let mut c2b = convolve_cols_signed(&row, &k.kn, |y| c.b * sign_x(y));

    // Weight of hypothesis H (copy a) is the share of energy in copy b.
    let mut wa = c2.clone();
    local_energy_in_place(&mut wa, &k.g, &mut row);
    {
        let mut eb = c2b.clone();
        local_energy_in_place(&mut eb, &k.g, &mut row);
        wa.data.par_iter_mut().zip(&eb.data).for_each(|(a, b)| {
            let sum = *a + b;
            *a = if sum > 1e-18 { b / sum } else { 0.5 };
        });
    }

    // Coherence of the band ring between the two copies: chroma is in both,
    // luminance leak in one. kappa in [0, 1] says how far to open each band
    // from the elongated to the square wide shape.
    let kappa = ra.take().map(|mut ra| {
        convolve_rows_mod_into(m, &k.kr, |x, _| sign_x(x), &mut row);
        let a_w = convolve_cols_signed(&row, &k.kr, |_| c.a);
        ring_in_place(&a_w, &mut ra);
        convolve_rows_mod_into(m, &k.kn, |_, _| 1.0, &mut row);
        let mut rb = convolve_cols_signed(&row, &k.kn, |y| c.b * sign_x(y));
        convolve_rows_mod_into(m, &k.kr, |_, _| 1.0, &mut row);
        let b_w = convolve_cols_signed(&row, &k.kr, |y| c.b * sign_x(y));
        ring_in_place(&b_w, &mut rb);

        let mut kappa = Plane::from_vec(w, h, ra.data.par_iter().zip(&rb.data).map(|(a, b)| a * b).collect());
        lowpass_in_place(&mut kappa, &k.g, &mut row);
        local_energy_in_place(&mut ra, &k.g, &mut row);
        local_energy_in_place(&mut rb, &k.g, &mut row);
        kappa.data.par_iter_mut().zip(&ra.data).zip(&rb.data).for_each(|((x, a), b)| {
            let d = (a * b).sqrt();
            *x = if d > 1e-18 { (*x / d).clamp(0.0, 1.0).powf(p.ring_power) } else { 0.0 };
        });
        drop((ra, rb));
        open_in_place(&mut c2, &a_w, &kappa);
        drop(a_w);
        open_in_place(&mut c2b, &b_w, &kappa);
        kappa
    });
    blend_in_place(&mut c2, &c2b, &wa);
    drop(c2b);

    convolve_rows_mod_into(m, &k.k1n, |x, _| sign_x(x), &mut row);
    let mut c1 = convolve_cols_signed(&row, &k.k1w, |y| c.eps * sign_x(y));
    if p.c1_wide != p.c1_narrow {
        convolve_rows_mod_into(m, &k.k1w, |x, _| sign_x(x), &mut row);
        let c1v = convolve_cols_signed(&row, &k.k1n, |y| c.eps * sign_x(y));
        blend_in_place(&mut c1, &c1v, &wa);
    }
    drop(wa);
    if let Some(kappa) = &kappa {
        if p.ring_c1 {
            convolve_rows_mod_into(m, &k.kr, |x, _| sign_x(x), &mut row);
            let c1w = convolve_cols_signed(&row, &k.kr, |y| c.eps * sign_x(y));
            open_in_place(&mut c1, &c1w, kappa);
        }
    }
    (c1, c2)
}

pub fn separate_dubois(mosaic: &Mosaic, phase: BayerPhase, p: &DuboisParams) -> Separation {
    let m = &mosaic.plane;
    let spec =
        |cutoff: f64| LowpassSpec { cutoff, transition: p.transition, attenuation_db: p.attenuation_db };
    let k = Kernels {
        k1n: spec(p.c1_narrow).kernel(),
        k1w: spec(p.c1_wide).kernel(),
        kn: spec(p.c2_narrow).kernel(),
        kw: spec(p.c2_wide).kernel(),
        kr: spec(if p.ring_wide > 0.0 { p.ring_wide } else { p.c2_wide }).kernel(),
        g: gaussian_kernel(p.energy_sigma),
    };

    let (c1, c2) = estimate(m, phase, p, &k);
    let w = m.width;
    let lum = luminance(m, phase, &c1, &c2);
    let mask_max = Plane::from_vec(w, m.height, vec![FIXED_MASK_VALUE; w * m.height]);
    Separation { lum, c1, c2, mask_max, rounds: 1, phase, computed: 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfa::Color;
    use crate::ingest::{CameraInfo, IngestReport, NoiseModel};

    fn mosaic_of(w: usize, h: usize, rgb: impl Fn(usize, usize) -> (f64, f64, f64)) -> Mosaic {
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
        Mosaic {
            plane: m,
            phase: Some(phase),
            saturated: vec![0; w * h],
            noise: NoiseModel::default(),
            wb: [1.0; 3],
            orientation: 1,
            camera: CameraInfo {
                make: String::new(),
                model: String::new(),
                clean_make: String::new(),
                clean_model: String::new(),
                bits: 16,
                exposure: Default::default(),
            },
            xyz_to_cam: None,
            report: IngestReport {
                crop: crate::decode::Rect { x: 0, y: 0, w, h },
                saturated_raw: 0,
                saturated_dilated: 0,
                defects: 0,
                defect_hot_rows: vec![],
                min_norm: 0.0,
                max_norm: 1.0,
                wb: [1.0; 3],
                wb_source: "exact".into(),
            },
        }
    }

    #[test]
    fn flat_colour_is_exact() {
        let (r, g, b) = (0.6, 0.3, 0.1);
        let m = mosaic_of(192, 160, |_, _| (r, g, b));
        let s = separate_dubois(&m, BayerPhase::RGGB, &DuboisParams::default());
        let l = (r + 2.0 * g + b) / 4.0;
        for y in 40..120 {
            for x in 40..150 {
                assert!((s.lum.at(x, y) - l).abs() < 1e-9, "{}", s.lum.at(x, y));
            }
        }
    }

    /// A neutral vertical line pattern near Nyquist is luminance; the fixed
    /// estimator subtracts part of it as chroma, the weighted one must keep it.
    #[test]
    fn neutral_vertical_lines_survive() {
        let (w, h) = (256, 128);
        let f = |x: usize| 0.5 + 0.3 * (2.0 * std::f64::consts::PI * 0.42 * x as f64).cos();
        let m = mosaic_of(w, h, |x, _| (f(x), f(x), f(x)));
        let s = separate_dubois(&m, BayerPhase::RGGB, &DuboisParams::default());
        let fixed = crate::separate::separate(
            &m,
            &crate::separate::SeparateParams { mask: crate::separate::MaskMode::Off, ..Default::default() },
        );
        let err = |p: &Plane| {
            let mut e = 0.0;
            for y in 32..h - 32 {
                for x in 32..w - 32 {
                    e += (p.at(x, y) - f(x)).powi(2);
                }
            }
            (e / ((h - 64) * (w - 64)) as f64).sqrt()
        };
        let (e_d, e_f) = (err(&s.lum), err(&fixed.lum));
        assert!(e_d < 0.1 * e_f, "dubois {e_d} fixed {e_f}");
    }
}
