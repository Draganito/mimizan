//! The moiré net: a testing option of the Dubois separation
//! (`DuboisParams::moire_net`, off by default).
//!
//! Near the Nyquist frequency a Bayer mosaic cannot tell fine neutral
//! structure from colour: a grey weave and a coloured field can give the
//! same photosite values to the bit. `dubois.rs` decides between its
//! estimates by fixed rules (energy of the two copies, orientation of the
//! baseband, coherence of the ring), and on a fine fabric or a wound string
//! those rules leave false colour. Here a small network, trained on pictures
//! whose true colour is known, makes that decision instead.
//!
//! What it may do is narrow. Per pixel it reads ten local statistics of the
//! separation and gives two sets of mixing weights, each summing to one:
//! six for `C2`, four for `C1`. What is mixed are estimates the separation
//! already has, every one of them a linear filter of the measured
//! photosites: the Dubois result, its elongated and its square narrow
//! copies, and the mean of the Dubois result over the neighbourhood. The
//! net invents no value; it chooses how much of which measurement to trust.
//! The luminance follows as always, mosaic minus chroma.
//!
//! It does not keep to the fabrics. At the mean of its features 46 % of
//! the `C2` weight and 32 % of the `C1` weight are on the neighbourhood
//! mean: in flat areas it evens out the colour noise, which nobody asked
//! of it, and a fine pattern of pure colour loses contrast.
//!
//! The network: 10 inputs, two hidden layers of 32 (ReLU), 10 outputs,
//! 1 738 weights, single precision. It was trained outside this repository
//! on twelve Kodak pictures (kodim01 to 12, sampled through an RGGB mosaic:
//! sharp, behind a lens, and at half size) and sixty random synthetic
//! scenes, each file weighted by the inverse of the Dubois error on it and
//! with a penalty on every pixel it makes worse than Dubois. It has seen no
//! other camera and no other noise than the default model at one and two
//! times its strength. `docs/PLATE.md` has the figures on the pictures it
//! was not trained on, and where it is worse.

use crate::cfa::{BayerPhase, Carriers};
use crate::dubois::{local_energy_in_place, lowpass_in_place, Kernels};
use crate::filter::{convolve_cols_signed, convolve_rows_mod_into, gaussian_kernel};
use crate::plane::Plane;
use crate::separate::{luminance, sign_x};
use rayon::prelude::*;

mod weights;
use weights::{B1, B2, B3, MEAN, STD, W1, W2, W3};

/// Features a pixel.
const NF: usize = 10;
/// Estimates of `C2` the net mixes, and of `C1`.
const K2: usize = 6;
const K1: usize = 4;
/// Width of the two hidden layers.
const H1: usize = 32;
const H2: usize = 32;

/// σ (px) of the window the local statistics are taken over.
const WINDOW_SIGMA: f64 = 4.0;
/// σ (px) of the neighbourhood mean that is one of the estimates.
const MEAN_SIGMA: f64 = 8.0;

/// Full f64 planes `dubois::estimate` holds at its peak with the net on,
/// the mosaic included: the features are ten single-precision planes (five
/// in f64 terms) under the mix, where six more are alive, and under the
/// last two statistics, where seven are. `pipeline::develop_memory` quotes
/// this figure.
pub const PEAK_PLANES: u64 = 12;

// The features, in the order the net was trained with.
/// Correlation of the two square narrow `C2` copies: chroma is in both,
/// a luminance leak in one.
const F_CORRELATION: usize = 0;
/// Which of the two copies has more energy, −1 to 1.
const F_BALANCE: usize = 1;
/// Their energy, over the squared local level, as a logarithm.
const F_ENERGY: usize = 2;
/// The Dubois weight of copy a.
const F_WEIGHT: usize = 3;
/// Orientation of the baseband (`dubois::orientation`).
const F_ORIENTATION: usize = 4;
/// Coherence of the band ring, as the bands were opened with it.
const F_COHERENCE: usize = 5;
/// Energy of the luminance above the narrow band.
const F_DETAIL: usize = 6;
/// Variance of the baseband, of `C1` and of `C2` in the window.
const F_VAR_BASE: usize = 7;
const F_VAR_C1: usize = 8;
const F_VAR_C2: usize = 9;

/// A layer's weights turned so that one input's weights lie together: the
/// layer then adds input by input into all its outputs at once, which the
/// compiler can do several outputs at a time. Each output still receives
/// its terms in the order of the inputs.
const fn by_input<const N: usize>(w: &[f32; N], outputs: usize, inputs: usize) -> [f32; N] {
    let mut t = [0.0; N];
    let mut o = 0;
    while o < outputs {
        let mut i = 0;
        while i < inputs {
            t[i * outputs + o] = w[o * inputs + i];
            i += 1;
        }
        o += 1;
    }
    t
}

static W1_BY_INPUT: [f32; H1 * NF] = by_input(&W1, H1, NF);
static W2_BY_INPUT: [f32; H2 * H1] = by_input(&W2, H2, H1);
static W3_BY_INPUT: [f32; (K2 + K1) * H2] = by_input(&W3, K2 + K1, H2);

/// `b + W x`, the weights by input. An input of zero adds nothing and is
/// passed over: after a ReLU about half of them are.
fn layer<const N: usize>(w: &[f32], b: &[f32; N], x: &[f32]) -> [f32; N] {
    let mut out = *b;
    for (x, w) in x.iter().zip(w.as_chunks::<N>().0) {
        if *x != 0.0 {
            for (o, w) in out.iter_mut().zip(w) {
                *o += w * x;
            }
        }
    }
    out
}

/// The two sets of mixing weights for one pixel's features: the first `K2`
/// for the `C2` estimates, the last `K1` for the `C1` estimates, each set a
/// softmax, so positive and summing to one.
fn mixing_weights(f: &[f32; NF]) -> [f32; K2 + K1] {
    let x: [f32; NF] = std::array::from_fn(|i| (f[i] - MEAN[i]) / STD[i]);
    let a = layer(&W1_BY_INPUT, &B1, &x).map(|v| v.max(0.0));
    let b = layer(&W2_BY_INPUT, &B2, &a).map(|v| v.max(0.0));
    let mut z = layer(&W3_BY_INPUT, &B3, &b);
    for set in [0..K2, K2..K2 + K1] {
        let z = &mut z[set];
        let top = z.iter().copied().fold(f32::MIN, f32::max);
        let mut sum = 0.0;
        for v in z.iter_mut() {
            *v = (*v - top).exp();
            sum += *v;
        }
        z.iter_mut().for_each(|v| *v /= sum);
    }
    z
}

/// A plane as the net reads it.
fn single(p: &Plane) -> Vec<f32> {
    p.data.par_iter().map(|v| *v as f32).collect()
}

/// `ln(v / level² + 1e-12)`: an energy relative to the local level, so the
/// net sees the same figure whatever the exposure.
fn log_ratio(v: &Plane, level: &Plane) -> Vec<f32> {
    v.data.par_iter().zip(&level.data).map(|(v, l)| ((v / (l * l + 1e-8)) + 1e-12).ln() as f32).collect()
}

/// Variance of `q` in the window `g`. Two planes of its own.
fn variance(q: &Plane, g: &[f64], scratch: &mut Plane) -> Plane {
    let mut mean = q.clone();
    lowpass_in_place(&mut mean, g, scratch);
    let mut var = q.clone();
    local_energy_in_place(&mut var, g, scratch);
    var.data.par_iter_mut().zip(&mean.data).for_each(|(v, m)| *v = (*v - m * m).max(0.0));
    var
}

/// `out += p · (estimate − base)` per pixel: the share `p` of the weight
/// moves from the base estimate to this one.
fn shift_towards(out: &mut Plane, estimate: &Plane, base: &Plane, p: &[f32]) {
    out.data
        .par_iter_mut()
        .zip(&estimate.data)
        .zip(&base.data)
        .zip(p)
        .for_each(|(((o, e), b), p)| *o += f64::from(*p) * (e - b));
}

/// The net's side of one estimate: the features as they become known, one
/// single-precision plane each, then the mix.
pub(crate) struct MoireNet {
    feat: [Vec<f32>; NF],
}

impl MoireNet {
    /// Before the estimate: what the two square narrow copies of `C2` say
    /// of each other. Five planes at its own peak, the mosaic included.
    pub(crate) fn start(m: &Plane, c: Carriers, k: &Kernels, row: &mut Plane) -> Self {
        let (w, h) = (m.width, m.height);
        convolve_rows_mod_into(m, &k.kn, |x, _| sign_x(x), row);
        let mut a = convolve_cols_signed(row, &k.kn, |_| c.a);
        convolve_rows_mod_into(m, &k.kn, |_, _| 1.0, row);
        let mut b = convolve_cols_signed(row, &k.kn, |y| c.b * sign_x(y));
        let mut ab = Plane::from_vec(w, h, a.data.par_iter().zip(&b.data).map(|(a, b)| a * b).collect());
        lowpass_in_place(&mut ab, &k.g, row);
        local_energy_in_place(&mut a, &k.g, row);
        local_energy_in_place(&mut b, &k.g, row);

        let mut feat: [Vec<f32>; NF] = Default::default();
        feat[F_CORRELATION] = ab
            .data
            .par_iter()
            .zip(&a.data)
            .zip(&b.data)
            .map(|((ab, aa), bb)| {
                let d = (aa * bb).sqrt();
                if d > 1e-30 {
                    (ab / d).clamp(-1.0, 1.0) as f32
                } else {
                    0.0
                }
            })
            .collect();
        drop(ab);
        feat[F_BALANCE] = a
            .data
            .par_iter()
            .zip(&b.data)
            .map(|(aa, bb)| if aa + bb > 1e-30 { ((aa - bb) / (aa + bb)) as f32 } else { 0.0 })
            .collect();
        a.data.par_iter_mut().zip(&b.data).for_each(|(aa, bb)| *aa += bb);
        drop(b);

        // The local level: the baseband, averaged over the window.
        let mut level = m.clone();
        lowpass_in_place(&mut level, &k.kn, row);
        lowpass_in_place(&mut level, &gaussian_kernel(WINDOW_SIGMA), row);
        feat[F_ENERGY] = log_ratio(&a, &level);
        Self { feat }
    }

    /// The Dubois weight of copy a, before it is dropped.
    pub(crate) fn weight(&mut self, wa: &Plane) {
        self.feat[F_WEIGHT] = single(wa);
    }

    /// The baseband orientation, before it is dropped; one half without
    /// the tensor.
    pub(crate) fn orientation(&mut self, t: Option<&Plane>, len: usize) {
        self.feat[F_ORIENTATION] = t.map_or_else(|| vec![0.5; len], single);
    }

    /// The remaining features, the weights, and the mix: `(c1, c2)` of the
    /// Dubois estimate in, the net's out. `kappa` is the coherence the
    /// bands were opened with, `None` without the coherence step.
    pub(crate) fn mix(
        mut self,
        m: &Plane,
        phase: BayerPhase,
        k: &Kernels,
        kappa: Option<Plane>,
        (c1, c2): (Plane, Plane),
        row: &mut Plane,
    ) -> (Plane, Plane) {
        let c = phase.carriers();
        let (w, h) = (m.width, m.height);
        let t = std::time::Instant::now();
        self.feat[F_COHERENCE] = kappa.as_ref().map_or_else(|| vec![0.0; w * h], single);
        drop(kappa);

        {
            let g = gaussian_kernel(WINDOW_SIGMA);
            let mut base = m.clone();
            lowpass_in_place(&mut base, &k.kn, row);
            let mut level = base.clone();
            lowpass_in_place(&mut level, &g, row);
            {
                let mut detail = luminance(m, phase, &c1, &c2);
                detail.data.par_iter_mut().zip(&base.data).for_each(|(l, b)| *l -= b);
                local_energy_in_place(&mut detail, &g, row);
                self.feat[F_DETAIL] = log_ratio(&detail, &level);
            }
            // The variance of the baseband; its mean is the level.
            local_energy_in_place(&mut base, &g, row);
            base.data.par_iter_mut().zip(&level.data).for_each(|(v, m)| *v = (*v - m * m).max(0.0));
            self.feat[F_VAR_BASE] = log_ratio(&base, &level);
            drop(base);
            self.feat[F_VAR_C1] = log_ratio(&variance(&c1, &g, row), &level);
            self.feat[F_VAR_C2] = log_ratio(&variance(&c2, &g, row), &level);
        }

        let features_ms = t.elapsed().as_millis();
        let t = std::time::Instant::now();

        // The features become the weights, in place, row by row.
        {
            let mut planes: Vec<_> = self.feat.iter_mut().map(|f| f.chunks_mut(w)).collect();
            let rows: Vec<[&mut [f32]; NF]> =
                (0..h).map(|_| std::array::from_fn(|i| planes[i].next().expect("a row a plane"))).collect();
            rows.into_par_iter().for_each(|mut r| {
                for x in 0..w {
                    let p = mixing_weights(&std::array::from_fn(|i| r[i][x]));
                    for (plane, p) in r.iter_mut().zip(p) {
                        plane[x] = p;
                    }
                }
            });
        }
        // The first weight of each set is the Dubois estimate's own. It is
        // not read: the mix starts from that estimate and moves the other
        // weights' shares away from it, so the weights sum to one exactly
        // and a field where all estimates agree stays as it is to the bit.
        self.feat[0] = Vec::new();
        self.feat[K2] = Vec::new();
        let weights_ms = t.elapsed().as_millis();
        let t = std::time::Instant::now();
        let mut share = |j: usize| std::mem::take(&mut self.feat[j]);

        let mut out2 = c2.clone();
        {
            // Copy a, elongated and square narrow, from one row pass.
            convolve_rows_mod_into(m, &k.kn, |x, _| sign_x(x), row);
            shift_towards(&mut out2, &convolve_cols_signed(row, &k.kw, |_| c.a), &c2, &share(1));
            shift_towards(&mut out2, &convolve_cols_signed(row, &k.kn, |_| c.a), &c2, &share(3));
            // Copy b, elongated and square narrow.
            convolve_rows_mod_into(m, &k.kw, |_, _| 1.0, row);
            shift_towards(&mut out2, &convolve_cols_signed(row, &k.kn, |y| c.b * sign_x(y)), &c2, &share(2));
            convolve_rows_mod_into(m, &k.kn, |_, _| 1.0, row);
            shift_towards(&mut out2, &convolve_cols_signed(row, &k.kn, |y| c.b * sign_x(y)), &c2, &share(4));
            // The mean of the Dubois estimate over the neighbourhood.
            let mut mean = c2.clone();
            lowpass_in_place(&mut mean, &gaussian_kernel(MEAN_SIGMA), row);
            shift_towards(&mut out2, &mean, &c2, &share(5));
        }
        drop(c2);

        let mut out1 = c1.clone();
        {
            // The two elongated bands of C1, before Dubois blends them.
            convolve_rows_mod_into(m, &k.k1n, |x, _| sign_x(x), row);
            let along = convolve_cols_signed(row, &k.k1w, |y| c.eps * sign_x(y));
            shift_towards(&mut out1, &along, &c1, &share(K2 + 1));
            drop(along);
            convolve_rows_mod_into(m, &k.k1w, |x, _| sign_x(x), row);
            let across = convolve_cols_signed(row, &k.k1n, |y| c.eps * sign_x(y));
            shift_towards(&mut out1, &across, &c1, &share(K2 + 2));
            drop(across);
            let mut mean = c1.clone();
            lowpass_in_place(&mut mean, &gaussian_kernel(MEAN_SIGMA), row);
            shift_towards(&mut out1, &mean, &c1, &share(K2 + 3));
        }
        tracing::debug!(
            "moiré net: features {features_ms} ms, weights {weights_ms} ms, mix {} ms",
            t.elapsed().as_millis()
        );
        (out1, out2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each set of weights is a softmax: positive, summing to one, for
    /// features in the trained range and far outside it.
    #[test]
    fn the_weights_of_each_set_sum_to_one() {
        let mut cases = vec![MEAN, [0.0; NF], [1.0; NF]];
        for scale in [-40.0f32, 40.0] {
            cases.push(std::array::from_fn(|i| MEAN[i] + scale * STD[i]));
        }
        for f in cases {
            let p = mixing_weights(&f);
            assert!(p.iter().all(|v| v.is_finite() && *v >= 0.0), "{p:?}");
            let (s2, s1): (f32, f32) = (p[..K2].iter().sum(), p[K2..].iter().sum());
            assert!((s2 - 1.0).abs() < 1e-5 && (s1 - 1.0).abs() < 1e-5, "{s2} {s1}");
        }
    }

    /// What the module says of the net's leaning: at the mean of its
    /// features, close to half of the `C2` weight and a third of the `C1`
    /// weight are on the neighbourhood mean.
    #[test]
    fn the_mean_pixel_leans_on_the_neighbourhood() {
        let p = mixing_weights(&MEAN);
        assert!((0.45..0.47).contains(&p[K2 - 1]), "{p:?}");
        assert!((0.31..0.33).contains(&p[K2 + K1 - 1]), "{p:?}");
    }
}
