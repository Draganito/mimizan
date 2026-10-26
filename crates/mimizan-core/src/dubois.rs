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
//!
//! The energy ratio alone fails where the chroma itself is strong: both
//! copies carry it, the ratio tends to one half, and the contaminated copy
//! gets half the weight exactly at a coloured edge, where the leak is
//! largest. The baseband of the mosaic (square low-pass below the carriers,
//! chroma-free) has the orientation of the luminance independent of the
//! chroma, so a structure tensor of it is a second factor of the weight
//! (`tensor_*` parameters), and where it is confidently oriented the clean
//! copy's ring is opened whatever the other ring says (`ring_tensor`).
//!
//! The block mask of `mask.rs` cannot do any of this: it decides per 64 px
//! block from band/ring energy ratios, which a line pattern inside the band
//! defeats.

use crate::cfa::BayerPhase;
use crate::filter::{
    convolve_cols_signed, convolve_cols_signed_into, convolve_rows_mod_into, gaussian_kernel, LowpassSpec,
};
use crate::ingest::Mosaic;
use crate::plane::Plane;
use crate::separate::{luminance, sign_x, Separation, FIXED_MASK_VALUE};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Bands and weights of the pixel-adaptive separation.
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
    /// Transition width of every band, c/px.
    pub transition: f64,
    /// Stop-band attenuation of every band, dB.
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
    /// Exponent β of the baseband orientation in the hypothesis weight:
    /// `w_H ∝ e_b · t^β`, `w_V ∝ e_a · (1 − t)^β` with
    /// `t = J_yy / (J_xx + J_yy)` the share of vertical second differences
    /// in the structure tensor of the baseband. 0 leaves Dubois' weight
    /// (the 0.11 behaviour).
    #[serde(default = "DuboisParams::default_tensor_power")]
    pub tensor_power: f64,
    /// Square band (c/px) of the baseband the tensor is taken from.
    #[serde(default = "DuboisParams::default_tensor_band")]
    pub tensor_band: f64,
    /// Gaussian σ (px) of the tensor window.
    #[serde(default = "DuboisParams::default_tensor_sigma")]
    pub tensor_sigma: f64,
    /// Where the baseband is oriented, the clean copy's ring is chroma
    /// whatever the other copy's ring says: its gain is raised by
    /// `min(1, γ |2t − 1|)` times that copy's weight. 0 = off.
    #[serde(default = "DuboisParams::default_ring_tensor")]
    pub ring_tensor: f64,
}

impl DuboisParams {
    fn default_tensor_power() -> f64 {
        0.5
    }
    fn default_tensor_band() -> f64 {
        0.30
    }
    fn default_tensor_sigma() -> f64 {
        2.0
    }
    fn default_ring_tensor() -> f64 {
        0.5
    }

    /// The 0.11 estimator: Dubois' energy weight and the coherence ring,
    /// without the baseband tensor.
    pub fn without_tensor() -> Self {
        Self { tensor_power: 0.0, ring_tensor: 0.0, ..Self::default() }
    }
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
            tensor_power: Self::default_tensor_power(),
            tensor_band: Self::default_tensor_band(),
            tensor_sigma: Self::default_tensor_sigma(),
            ring_tensor: Self::default_ring_tensor(),
        }
    }
}

/// The low-passes of one separation, built once.
struct Kernels {
    /// C1 narrow and wide bands.
    k1n: Vec<f64>,
    k1w: Vec<f64>,
    /// C2 narrow and wide bands.
    kn: Vec<f64>,
    kw: Vec<f64>,
    /// Outer edge of the coherence ring; `None` without the coherence step.
    kr: Option<Vec<f64>>,
    /// Gaussian of the local energy.
    g: Vec<f64>,
    /// Baseband low-pass and Gaussian window of the structure tensor;
    /// `None` when neither tensor term is used.
    tensor: Option<(Vec<f64>, Vec<f64>)>,
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

/// `narrow := wide − narrow` in place: the ring between the two bands.
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

/// Orientation of the baseband: `t = J_yy / (J_xx + J_yy)` per pixel, the
/// share of vertical second differences in the Gaussian-windowed structure
/// tensor of the square low-passed mosaic. 0.5 where the baseband is flat
/// or isotropic, 1 for horizontal structure (hypothesis H), 0 for vertical.
/// Four planes at its own peak (mosaic, scratch, J_xx, J_yy); returns one.
fn orientation(m: &Plane, kb: &[f64], g: &[f64], scratch: &mut Plane) -> Plane {
    let (w, h) = (m.width, m.height);
    let mut base = m.clone();
    lowpass_in_place(&mut base, kb, scratch);
    let mut jxx = Plane::zeros(w, h);
    let mut jyy = Plane::zeros(w, h);
    jxx.data.par_chunks_mut(w).zip(jyy.data.par_chunks_mut(w)).enumerate().for_each(|(y, (rx, ry))| {
        let (ym, yp) = (y.saturating_sub(1), (y + 1).min(h - 1));
        let (rm, r0, rp) = (base.row(ym), base.row(y), base.row(yp));
        for x in 0..w {
            let (xm, xp) = (x.saturating_sub(1), (x + 1).min(w - 1));
            let gx = r0[xp] - 2.0 * r0[x] + r0[xm];
            let gy = rp[x] - 2.0 * r0[x] + rm[x];
            rx[x] = gx * gx;
            ry[x] = gy * gy;
        }
    });
    drop(base);
    lowpass_in_place(&mut jxx, g, scratch);
    lowpass_in_place(&mut jyy, g, scratch);
    jyy.data.par_iter_mut().zip(&jxx.data).for_each(|(yy, xx)| {
        let t = *yy + xx;
        *yy = if t > 1e-18 { *yy / t } else { 0.5 };
    });
    jyy
}

/// The estimation: returns `(c1, c2)`. Planes are dropped as early as the
/// data flow allows and the low-passes run in place; the peak is nine full
/// planes (mosaic included) with the coherence step and the tensor, eight
/// without the tensor, seven without either. `pipeline::develop_memory`
/// quotes this figure.
fn estimate(m: &Plane, phase: BayerPhase, p: &DuboisParams, k: &Kernels) -> (Plane, Plane) {
    let c = phase.carriers();
    let (w, h) = (m.width, m.height);
    let mut row = Plane::zeros(w, h);

    // Orientation of the chroma-free baseband, before the copies: its own
    // peak lies below the later one.
    let orient = k.tensor.as_ref().map(|(kb, g)| orientation(m, kb, g, &mut row));

    // Hypothesis H (horizontal structure, spectrum along v): copy a at
    // (0.5, 0) is clean; bands narrow in u, wide in v. Hypothesis V
    // (vertical structure, spectrum along u): copy b at (0, 0.5) is clean;
    // bands wide in u, narrow in v. The square narrow copies are the inner
    // edge of the coherence ring.
    convolve_rows_mod_into(m, &k.kn, |x, _| sign_x(x), &mut row);
    let mut c2 = convolve_cols_signed(&row, &k.kw, |_| c.a);
    let ra = k.kr.as_ref().map(|kr| (kr, convolve_cols_signed(&row, &k.kn, |_| c.a)));
    convolve_rows_mod_into(m, &k.kw, |_, _| 1.0, &mut row);
    let mut c2b = convolve_cols_signed(&row, &k.kn, |y| c.b * sign_x(y));

    // Weight of hypothesis H (copy a) is the share of energy in copy b,
    // times the share of horizontal structure in the baseband.
    let mut wa = c2.clone();
    local_energy_in_place(&mut wa, &k.g, &mut row);
    {
        let mut eb = c2b.clone();
        local_energy_in_place(&mut eb, &k.g, &mut row);
        match (&orient, p.tensor_power > 0.0) {
            (Some(t), true) => {
                let beta = p.tensor_power;
                wa.data.par_iter_mut().zip(&eb.data).zip(&t.data).for_each(|((a, b), t)| {
                    let (a_, b_) = (*a * (1.0 - t).powf(beta), b * t.powf(beta));
                    let sum = a_ + b_;
                    *a = if sum > 1e-18 { b_ / sum } else { 0.5 };
                });
            }
            _ => wa.data.par_iter_mut().zip(&eb.data).for_each(|(a, b)| {
                let sum = *a + b;
                *a = if sum > 1e-18 { b / sum } else { 0.5 };
            }),
        }
    }

    // Coherence of the band ring between the two copies: chroma is in both,
    // luminance leak in one. kappa in [0, 1] says how far to open each band
    // from the elongated to the square wide shape. With the tensor, the
    // copy the orientation declares clean is opened further, by the
    // anisotropy scaled with that copy's weight; kappa then becomes the
    // weighted mean of the two gains, for C1.
    //
    // The wide copies `a_w`, `b_w` are each made twice: once for the ring,
    // dropped at once, and again when the bands are opened. Holding them
    // across the coherence step would put two more full planes under the
    // peak; the second convolution costs a few per cent of the time and
    // gives the same bits.
    let kappa = ra.map(|(kr, mut ra)| {
        let wide_a = |row: &mut Plane| {
            convolve_rows_mod_into(m, kr, |x, _| sign_x(x), row);
            convolve_cols_signed(row, kr, |_| c.a)
        };
        let wide_b = |row: &mut Plane| {
            convolve_rows_mod_into(m, kr, |_, _| 1.0, row);
            convolve_cols_signed(row, kr, |y| c.b * sign_x(y))
        };
        ring_in_place(&wide_a(&mut row), &mut ra);
        convolve_rows_mod_into(m, &k.kn, |_, _| 1.0, &mut row);
        let mut rb = convolve_cols_signed(&row, &k.kn, |y| c.b * sign_x(y));
        ring_in_place(&wide_b(&mut row), &mut rb);

        let mut kappa = Plane::from_vec(w, h, ra.data.par_iter().zip(&rb.data).map(|(a, b)| a * b).collect());
        lowpass_in_place(&mut kappa, &k.g, &mut row);
        local_energy_in_place(&mut ra, &k.g, &mut row);
        local_energy_in_place(&mut rb, &k.g, &mut row);
        kappa.data.par_iter_mut().zip(&ra.data).zip(&rb.data).for_each(|((x, a), b)| {
            let d = (a * b).sqrt();
            *x = if d > 1e-18 { (*x / d).clamp(0.0, 1.0).powf(p.ring_power) } else { 0.0 };
        });
        drop((ra, rb));
        let a_w = wide_a(&mut row);
        match (&orient, p.ring_tensor > 0.0) {
            (Some(t), true) => {
                let gamma = p.ring_tensor;
                // Copy a is clean under H: scale by w_H. Copy b under V: by 1 − w_H.
                c2.data.par_iter_mut().zip(&a_w.data).zip(&kappa.data).zip(&t.data).zip(&wa.data).for_each(
                    |((((b, w), k), t), wa)| {
                        let c = (gamma * (2.0 * t - 1.0).abs()).min(1.0);
                        *b += (k + (1.0 - k) * c * wa) * (w - *b);
                    },
                );
                drop(a_w);
                let b_w = wide_b(&mut row);
                c2b.data.par_iter_mut().zip(&b_w.data).zip(&kappa.data).zip(&t.data).zip(&wa.data).for_each(
                    |((((b, w), k), t), wa)| {
                        let c = (gamma * (2.0 * t - 1.0).abs()).min(1.0);
                        *b += (k + (1.0 - k) * c * (1.0 - wa)) * (w - *b);
                    },
                );
                // kappa := w_H g_a + (1 − w_H) g_b, the gain C1 is opened with.
                kappa.data.par_iter_mut().zip(&t.data).zip(&wa.data).for_each(|((k, t), wa)| {
                    let c = (gamma * (2.0 * t - 1.0).abs()).min(1.0);
                    let (ga, gb) = (*k + (1.0 - *k) * c * wa, *k + (1.0 - *k) * c * (1.0 - wa));
                    *k = wa * ga + (1.0 - wa) * gb;
                });
            }
            _ => {
                open_in_place(&mut c2, &a_w, &kappa);
                drop(a_w);
                open_in_place(&mut c2b, &wide_b(&mut row), &kappa);
            }
        }
        kappa
    });
    drop(orient);
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
    if let (Some(kappa), Some(kr)) = (&kappa, &k.kr) {
        if p.ring_c1 {
            convolve_rows_mod_into(m, kr, |x, _| sign_x(x), &mut row);
            let c1w = convolve_cols_signed(&row, kr, |y| c.eps * sign_x(y));
            open_in_place(&mut c1, &c1w, kappa);
        }
    }
    (c1, c2)
}

/// The pixel-adaptive separation: two hypotheses about the local structure,
/// weighted per pixel by the energy in the two C2 copies.
pub fn separate_dubois(mosaic: &Mosaic, phase: BayerPhase, p: &DuboisParams) -> Separation {
    let m = &mosaic.plane;
    let spec =
        |cutoff: f64| LowpassSpec { cutoff, transition: p.transition, attenuation_db: p.attenuation_db };
    let k = Kernels {
        k1n: spec(p.c1_narrow).kernel(),
        k1w: spec(p.c1_wide).kernel(),
        kn: spec(p.c2_narrow).kernel(),
        kw: spec(p.c2_wide).kernel(),
        kr: (p.ring_wide > 0.0).then(|| spec(p.ring_wide).kernel()),
        g: gaussian_kernel(p.energy_sigma),
        tensor: (p.tensor_power > 0.0 || (p.ring_tensor > 0.0 && p.ring_wide > 0.0))
            .then(|| (spec(p.tensor_band).kernel(), gaussian_kernel(p.tensor_sigma))),
    };

    let (c1, c2) = estimate(m, phase, p, &k);
    let w = m.width;
    let lum = luminance(m, phase, &c1, &c2);
    let mask_max = Plane::from_vec(w, m.height, vec![FIXED_MASK_VALUE; w * m.height]);
    Separation { lum, c1, c2, mask_max, rounds: 1, phase }
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
                defects_repaired: false,
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
        for p in [DuboisParams::default(), DuboisParams::without_tensor()] {
            let s = separate_dubois(&m, BayerPhase::RGGB, &p);
            let l = (r + 2.0 * g + b) / 4.0;
            for y in 40..120 {
                for x in 40..150 {
                    assert!((s.lum.at(x, y) - l).abs() < 1e-9, "{}", s.lum.at(x, y));
                }
            }
        }
    }

    /// A sharp vertical colour edge under flat luminance: the baseband is
    /// flat (`t = 0.5` everywhere, no orientation), so the tensor terms
    /// must leave the estimate as Dubois' weight alone gives it.
    #[test]
    fn flat_luminance_ignores_the_tensor() {
        let m = mosaic_of(192, 160, |x, _| if x < 96 { (0.6, 0.3, 0.1) } else { (0.1, 0.3, 0.6) });
        let with = separate_dubois(&m, BayerPhase::RGGB, &DuboisParams::default());
        let without = separate_dubois(&m, BayerPhase::RGGB, &DuboisParams::without_tensor());
        let mut worst = 0.0f64;
        for y in 20..140 {
            for x in 20..172 {
                worst = worst.max((with.lum.at(x, y) - without.lum.at(x, y)).abs());
            }
        }
        // The edge itself is a baseband step in R and B (the mosaic is not
        // flat there), so a small difference near x = 96 is allowed.
        assert!(worst < 2e-3, "{worst}");
    }

    /// A coloured horizontal line pattern near the carrier: the leak sits in
    /// copy b, the chroma is strong in both copies, and Dubois' energy ratio
    /// cannot tell the clean copy. The baseband orientation can; the tensor
    /// estimate must be closer to the truth than the 0.11 one.
    #[test]
    fn oriented_colour_edges_gain_from_the_tensor() {
        let (w, h) = (256, 192);
        let f = |y: usize| 0.5 + 0.25 * (2.0 * std::f64::consts::PI * 0.38 * y as f64).cos();
        let m = mosaic_of(w, h, |_, y| (0.8 * f(y), 0.5 * f(y), 0.2 * f(y)));
        let truth = |y: usize| (0.8 + 1.0 + 0.2) / 4.0 * f(y);
        let err = |p: &DuboisParams| {
            let s = separate_dubois(&m, BayerPhase::RGGB, p);
            let mut e = 0.0;
            for y in 32..h - 32 {
                for x in 32..w - 32 {
                    e += (s.lum.at(x, y) - truth(y)).powi(2);
                }
            }
            (e / ((h - 64) * (w - 64)) as f64).sqrt()
        };
        let (e_t, e_0) = (err(&DuboisParams::default()), err(&DuboisParams::without_tensor()));
        assert!(e_t < 0.7 * e_0, "tensor {e_t} without {e_0}");
    }

    /// A neutral vertical line pattern near Nyquist is luminance; the fixed
    /// estimator subtracts part of it as chroma, the weighted one must keep it.
    #[test]
    fn neutral_vertical_lines_survive() {
        let (w, h) = (256, 128);
        let f = |x: usize| 0.5 + 0.3 * (2.0 * std::f64::consts::PI * 0.42 * x as f64).cos();
        let m = mosaic_of(w, h, |x, _| (f(x), f(x), f(x)));
        let fixed = crate::separate::separate(
            &m,
            &crate::separate::SeparateParams { mask: crate::separate::MaskMode::Off, ..Default::default() },
        )
        .expect("synthetic mosaic has a phase");
        let err = |p: &Plane| {
            let mut e = 0.0;
            for y in 32..h - 32 {
                for x in 32..w - 32 {
                    e += (p.at(x, y) - f(x)).powi(2);
                }
            }
            (e / ((h - 64) * (w - 64)) as f64).sqrt()
        };
        let e_f = err(&fixed.lum);
        for p in [DuboisParams::default(), DuboisParams::without_tensor()] {
            let s = separate_dubois(&m, BayerPhase::RGGB, &p);
            let e_d = err(&s.lum);
            assert!(e_d < 0.1 * e_f, "dubois {e_d} fixed {e_f}");
        }
    }
}
