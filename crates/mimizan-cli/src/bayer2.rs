//! Least-squares demosaic on the Bayer period.
//!
//! A 2×2 mosaic is `m = L + C1·pg + C2·pd`. The green-magenta carrier sits
//! at `(½, ½)` and the red-blue carriers at `(½, 0)` and `(0, ½)`. Frequencies
//! that differ by those steps are one set, four bins, and the most probable
//! luminance and both chrominances of the set are one 4×4 solve. The prior
//! is the one the mosaic itself makes most probable, and the lens in front
//! of it is measured from the mosaic radius by radius. A bank of those
//! spectra is weighed per pixel, by how probable the mosaic around the
//! pixel is under each and by how well each tells a photosite from all the
//! others. The second step (a line through two colours) is the one the 6×6
//! solver uses: it reads the photosite, not the period.
//!
//! The transform is the same convention as that solver. The forward transform
//! is unnormalised and the inverse is divided by the number of samples, so a
//! prior fitted here is not a prior fitted on a 6×6 tile.

use mimizan_lab_xtrans::filter::{
    convolve_cols_signed_into, convolve_rows_mod_into, gaussian_kernel, reflect,
};
use mimizan_lab_xtrans::plane::Plane;
use mimizan_lab_xtrans::xtrans::Prior;
use rayon::prelude::*;
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::f64::consts::PI;
use std::sync::Arc;

type C64 = Complex<f64>;

/// The window the separation is done in. Same reach as the 6×6 solver:
/// 960 photosites, 48 of them margin, so the second step sees the same overlap.
const TILE: usize = 960;
const MARGIN: usize = 48;

/// Bins in one set. The period is 2, so a set is 2×2.
const N: usize = 4;

/// A direction holds only above this, in c/px. Below it a flat patch has no direction.
const ORIENTED_FROM: f64 = 0.03;

struct Fft2 {
    w: usize,
    h: usize,
    fw: Arc<dyn Fft<f64>>,
    fh: Arc<dyn Fft<f64>>,
    iw: Arc<dyn Fft<f64>>,
    ih: Arc<dyn Fft<f64>>,
}

impl Fft2 {
    fn new(w: usize, h: usize) -> Self {
        let mut pl = FftPlanner::<f64>::new();
        Self {
            w,
            h,
            fw: pl.plan_fft_forward(w),
            fh: pl.plan_fft_forward(h),
            iw: pl.plan_fft_inverse(w),
            ih: pl.plan_fft_inverse(h),
        }
    }

    /// In place. The inverse is scaled by `1/(w·h)`.
    fn run(&self, buf: &mut [C64], inverse: bool) {
        let (w, h) = (self.w, self.h);
        let (fx, fy) = if inverse { (&self.iw, &self.ih) } else { (&self.fw, &self.fh) };
        let scratch_len = fx.get_inplace_scratch_len().max(fy.get_inplace_scratch_len());
        let mut scratch = vec![C64::new(0.0, 0.0); scratch_len];
        for row in buf.chunks_mut(w) {
            fx.process_with_scratch(row, &mut scratch);
        }
        // Several columns at once: one column alone never fills a transform.
        const COLS: usize = 16;
        let mut cols = vec![C64::new(0.0, 0.0); COLS * h];
        let scale = if inverse { 1.0 / (w * h) as f64 } else { 1.0 };
        for x0 in (0..w).step_by(COLS) {
            let n = COLS.min(w - x0);
            for y in 0..h {
                for (j, v) in buf[y * w + x0..y * w + x0 + n].iter().enumerate() {
                    cols[j * h + y] = *v;
                }
            }
            fy.process_with_scratch(&mut cols[..n * h], &mut scratch);
            for y in 0..h {
                for (j, v) in buf[y * w + x0..y * w + x0 + n].iter_mut().enumerate() {
                    *v = cols[j * h + y] * scale;
                }
            }
        }
    }
}

/// Carriers of `C1` and of `C2` for an RGGB tile whose corner is red:
/// `(k, l, coefficient)` of the 2×2 transform, the non-zero ones.
fn carriers() -> [Vec<(usize, usize, C64)>; 2] {
    let g = 0.5;
    let mut pg = [[0.0; 2]; 2];
    let mut pd = [[0.0; 2]; 2];
    for y in 0..2 {
        for x in 0..2 {
            let green = x != y;
            pg[y][x] = if green { 1.0 - g } else { -g };
            pd[y][x] = match (x, y) {
                (0, 0) => 1.0,
                (1, 1) => -1.0,
                _ => 0.0,
            };
        }
    }
    [dft2(&pg), dft2(&pd)]
}

fn dft2(p: &[[f64; 2]; 2]) -> Vec<(usize, usize, C64)> {
    let mut v = Vec::new();
    for l in 0..2 {
        for k in 0..2 {
            let mut s = C64::new(0.0, 0.0);
            for (y, row) in p.iter().enumerate() {
                for (x, sample) in row.iter().enumerate() {
                    let ph = -PI * (k * x + l * y) as f64;
                    s += C64::from_polar(sample / 4.0, ph);
                }
            }
            if s.norm() > 1e-12 {
                v.push((k, l, s));
            }
        }
    }
    v
}

/// What one baseband bin is the sum of: the observation bins its carriers land on.
fn from_carriers(nz: &[Vec<(usize, usize, C64)>; 2]) -> [Vec<(usize, C64)>; 2] {
    [0, 1].map(|kind| {
        let mut v = Vec::new();
        for b in 0..2 {
            for a in 0..2 {
                v.extend(nz[kind].iter().map(|&(k, l, ak)| (((b + l) % 2) * 2 + (a + k) % 2, ak)));
            }
        }
        v
    })
}

fn bin_freq(i: usize, n: usize) -> f64 {
    if 2 * i <= n {
        i as f64 / n as f64
    } else {
        i as f64 / n as f64 - 1.0
    }
}

/// Width of one step of a measured lens, in c/px.
const LENS_STEP: f64 = 0.01;
/// Steps of a measured lens: out to the corner of the spectrum, 1/√2.
const LENS_STEPS: usize = 72;

/// What the picture's spectrum is taken to be.
///
/// The power laws, the chrominance shares, the noise and the direction are
/// the [`Prior`]'s. The lens need not be its Gaussian: `lens` is what the
/// lens passes of the power at each radius, in steps of [`LENS_STEP`], and
/// it replaces the prior's own. A picture enlarged to twice its size, or
/// one a real lens drew, is flat and then ends; a Gaussian that ends as
/// early has taken the middle frequencies with it.
#[derive(Clone, Debug, PartialEq)]
pub struct Spectrum {
    pub prior: Prior,
    /// What the lens passes of the power, by radius. `None` is the prior's Gaussian.
    pub lens: Option<Arc<Vec<f64>>>,
}

impl Spectrum {
    /// The prior alone, behind its Gaussian lens.
    pub fn plain(prior: Prior) -> Self {
        Self { prior, lens: None }
    }

    /// The same lens in front of another prior.
    pub fn with(&self, prior: Prior) -> Self {
        Self { prior, lens: self.lens.clone() }
    }

    /// What the lens passes of the power at `r` c/px.
    fn pass(&self, r: f64) -> f64 {
        match &self.lens {
            None => (-4.0 * PI * PI * self.prior.lens * self.prior.lens * r * r).exp(),
            Some(t) => {
                // Between the centres of two steps, along a straight line in the logarithm.
                let x = (r / LENS_STEP - 0.5).max(0.0);
                let i = (x.floor() as usize).min(t.len() - 2);
                let f = (x - i as f64).min(1.0);
                (t[i].max(1e-300).ln() * (1.0 - f) + t[i + 1].max(1e-300).ln() * f).exp()
            }
        }
    }

    /// The two power laws at `r²`, luminance and chrominance, without lens or direction.
    fn laws(&self, r2: f64) -> (f64, f64) {
        let p = &self.prior;
        let base = (p.f0 * p.f0 + r2) / (p.f0 * p.f0 + 0.0625);
        (base.powf(-p.alpha / 2.0), base.powf(-p.alpha_c / 2.0))
    }
}

/// Luminance power and the two chrominance powers.
fn power(spec: &Spectrum, fu: f64, fv: f64) -> (f64, [f64; 2]) {
    let prior = &spec.prior;
    let r2 = fu * fu + fv * fv;
    let dir = match prior.oriented {
        Some((theta, k)) if r2 > 0.0 => {
            let c = (fu * theta.cos() + fv * theta.sin()) / r2.sqrt();
            let d = (c * c).powf(k).max(1e-3);
            // The level of a flat area has no direction. A prior that denies
            // the slow colour across its angle moves that level.
            let detail = r2 / (r2 + ORIENTED_FROM * ORIENTED_FROM);
            detail * d + (1.0 - detail)
        }
        _ => 1.0,
    };
    let through = spec.pass(r2.sqrt()) * dir;
    let (l, c) = spec.laws(r2);
    (through * l, [0, 1].map(|k| through * prior.chroma[k] * c))
}

/// Powers at bin `(u, v)`. A bin at half the sampling rate stands for `+½`
/// and `−½` alike, which an oriented prior tells apart, so it gets the mean.
fn bin_power(spec: &Spectrum, u: usize, v: usize, tw: usize, th: usize) -> (f64, [f64; 2]) {
    let (fu, fv) = (bin_freq(u, tw), bin_freq(v, th));
    let p = power(spec, fu, fv);
    if spec.prior.oriented.is_none() || (2 * u != tw && 2 * v != th) {
        return p;
    }
    let q = if 2 * u == tw { power(spec, -fu, fv) } else { power(spec, fu, -fv) };
    (0.5 * (p.0 + q.0), [0.5 * (p.1[0] + q.1[0]), 0.5 * (p.1[1] + q.1[1])])
}

/// The row of sets that holds the conjugates of row `v0`: none for the two
/// rows that hold their own.
fn mirrored(v0: usize, qh: usize) -> Option<usize> {
    (v0 != 0 && 2 * v0 != qh).then_some(qh - v0)
}

fn set_spectrum(spec: &[C64], tw: usize, u0: usize, v0: usize, qw: usize, qh: usize, y: &mut [C64; N]) {
    for b in 0..2 {
        let row = &spec[(v0 + b * qh) * tw..];
        for a in 0..2 {
            y[b * 2 + a] = row[u0 + a * qw];
        }
    }
}

/// Covariance of the four tied bins, and the chrominance powers at each.
fn set_matrix(
    tw: usize,
    th: usize,
    nz: &[Vec<(usize, usize, C64)>; 2],
    spec: &Spectrum,
    u0: usize,
    v0: usize,
    g: &mut [C64; N * N],
) -> [[f64; N]; 2] {
    let (qw, qh) = (tw / 2, th / 2);
    let mut sl = [0.0; N];
    let mut sc = [[0.0; N]; 2];
    for b in 0..2 {
        for a in 0..2 {
            let i = b * 2 + a;
            let (l, c) = bin_power(spec, u0 + a * qw, v0 + b * qh, tw, th);
            sl[i] = l;
            sc[0][i] = c[0];
            sc[1][i] = c[1];
        }
    }
    *g = [C64::new(0.0, 0.0); N * N];
    for i in 0..N {
        g[i * N + i] = C64::new(sl[i] + spec.prior.noise, 0.0);
    }
    for (kind, list) in nz.iter().enumerate() {
        for b in 0..2 {
            for a in 0..2 {
                let s = sc[kind][b * 2 + a];
                for &(k, l, ak) in list {
                    let i = ((b + l) % 2) * 2 + (a + k) % 2;
                    for &(k2, l2, ak2) in list {
                        let j = ((b + l2) % 2) * 2 + (a + k2) % 2;
                        g[i * N + j] += ak * ak2.conj() * s;
                    }
                }
            }
        }
    }
    sc
}

fn cholesky(g: &mut [C64], n: usize) {
    for j in 0..n {
        let mut d = g[j * n + j].re;
        for k in 0..j {
            d -= g[j * n + k].norm_sqr();
        }
        let d = d.max(1e-300).sqrt();
        g[j * n + j] = C64::new(d, 0.0);
        for i in j + 1..n {
            let mut v = g[i * n + j];
            for k in 0..j {
                v -= g[i * n + k] * g[j * n + k].conj();
            }
            g[i * n + j] = v / d;
        }
    }
}

/// `G·x = y` with the Cholesky factor of `G` already in `g`. `y` comes back as `x`.
fn solve(g: &[C64], y: &mut [C64], n: usize) {
    for i in 0..n {
        let mut v = y[i];
        for k in 0..i {
            v -= g[i * n + k] * y[k];
        }
        y[i] = v / g[i * n + i].re;
    }
    for i in (0..n).rev() {
        let mut v = y[i];
        for k in i + 1..n {
            v -= g[k * n + i].conj() * y[k];
        }
        y[i] = v / g[i * n + i].re;
    }
}

/// `|L⁻¹y|²`, the quadratic piece of Whittle's likelihood. `y` is overwritten.
fn quad(g: &[C64], y: &mut [C64], n: usize) -> f64 {
    let mut q = 0.0;
    for i in 0..n {
        let mut v = y[i];
        for k in 0..i {
            v -= g[i * n + k] * y[k];
        }
        y[i] = v / g[i * n + i].re;
        q += y[i].norm_sqr();
    }
    q
}

struct Lattice {
    tw: usize,
    th: usize,
    nz: [Vec<(usize, usize, C64)>; 2],
    from: [Vec<(usize, C64)>; 2],
    fft: Fft2,
}

impl Lattice {
    fn new(tw: usize, th: usize) -> Self {
        let nz = carriers();
        let from = from_carriers(&nz);
        Self { tw, th, nz, from, fft: Fft2::new(tw, th) }
    }
}

struct Factored {
    /// Per set held, the Cholesky factor, row-major, lower triangle filled.
    factors: Vec<C64>,
    /// Per set held, the powers of `C1` and of `C2` at its four bins.
    powers: Vec<[[f64; N]; 2]>,
    /// `log det G` per pixel of the window.
    logdet: f64,
    /// The diagonal of `G⁻¹` in the frame, at each of the four photosites
    /// of the period: `[y % 2][x % 2]`.
    inverse: [[f64; 2]; 2],
}

impl Factored {
    fn build(lat: &Lattice, spec: &Spectrum) -> Self {
        let (tw, th) = (lat.tw, lat.th);
        let (qw, qh) = (tw / 2, th / 2);
        let rows = qh / 2 + 1;
        let mut factors = vec![C64::new(0.0, 0.0); rows * qw * N * N];
        let mut powers = vec![[[0.0; N]; 2]; rows * qw];
        let nz = &lat.nz;
        let (logdet, inverse) = factors
            .par_chunks_mut(qw * N * N)
            .zip(powers.par_chunks_mut(qw))
            .enumerate()
            .map(|(v0, (frow, prow))| {
                let mut logdet = 0.0;
                let mut inverse = [0.0; N];
                for u0 in 0..qw {
                    let g = &mut frow[u0 * N * N..(u0 + 1) * N * N];
                    let mut cell = [C64::new(0.0, 0.0); N * N];
                    prow[u0] = set_matrix(tw, th, nz, spec, u0, v0, &mut cell);
                    cholesky(&mut cell, N);
                    g.copy_from_slice(&cell);
                    logdet += (0..N).map(|i| 2.0 * cell[i * N + i].re.ln()).sum::<f64>();
                    // Column j of G⁻¹, and what it adds to the diagonal at each photosite.
                    for j in 0..N {
                        let mut col = [C64::new(0.0, 0.0); N];
                        col[j] = C64::new(1.0, 0.0);
                        solve(&cell, &mut col, N);
                        for (i, v) in col.iter().enumerate() {
                            let (da, db) = ((i ^ j) & 1, ((i ^ j) >> 1) & 1);
                            for (p, sum) in inverse.iter_mut().enumerate() {
                                let odd = (da * (p & 1) + db * (p >> 1)) % 2 == 1;
                                *sum += if odd { -v.re } else { v.re };
                            }
                        }
                    }
                }
                // The row stands for its mirror as well, unless it is its own.
                let times = if mirrored(v0, qh).is_some() { 2.0 } else { 1.0 };
                (times * logdet, inverse.map(|v| times * v))
            })
            .reduce(
                || (0.0, [0.0; N]),
                |a, b| (a.0 + b.0, [a.1[0] + b.1[0], a.1[1] + b.1[1], a.1[2] + b.1[2], a.1[3] + b.1[3]]),
            );
        let n = (tw * th) as f64;
        let inverse = [[inverse[0] / n, inverse[1] / n], [inverse[2] / n, inverse[3] / n]];
        Self { factors, powers, logdet: logdet / n, inverse }
    }

    /// How far the photosite at `(x, y)` is from what all the others say
    /// of it, from `z = G⁻¹m` there. Leaving one photosite out needs no
    /// second solve: it is `z_i / (G⁻¹)_ii`.
    fn apart(&self, z: f64, x: usize, y: usize) -> f64 {
        z / self.inverse[y % 2][x % 2]
    }
}

fn spectrum(m: &Plane, fft: &Fft2, ox: isize, oy: isize) -> Vec<C64> {
    let (tw, th) = (fft.w, fft.h);
    let mut spec = vec![C64::new(0.0, 0.0); tw * th];
    for y in 0..th {
        let row = m.row(reflect(oy + y as isize, m.height));
        for x in 0..tw {
            spec[y * tw + x] = C64::new(row[reflect(ox + x as isize, m.width)], 0.0);
        }
    }
    fft.run(&mut spec, false);
    spec
}

/// The spectrum of one window, the window fading to nothing at its sides
/// as a raised cosine.
///
/// A window cut out of a picture does not close on itself, and the step
/// from one side to the other is in the spectrum of the cut as a floor
/// that falls only as `1/f²`. Behind a lens the picture falls faster, and
/// where the lens ends the floor is all there is. The fade takes the step
/// away; it mixes each bin only with its neighbours, and the carriers stay
/// where they are.
///
/// The search and the lens are both on this. Measured on the Kodak set
/// under the sRGB curve, against a search on windows that do not fade:
/// 0.34 dB better behind a lens, 0.26 dB better at twice the size and
/// 0.12 dB worse pixel-sharp, where the step is the picture's own.
fn faded_spectrum(m: &Plane, fft: &Fft2, ox: isize, oy: isize) -> Vec<C64> {
    let (tw, th) = (fft.w, fft.h);
    let fade = |n: usize| -> Vec<f64> {
        (0..n).map(|i| 0.5 - 0.5 * (2.0 * PI * (i as f64 + 0.5) / n as f64).cos()).collect()
    };
    let (fx, fy) = (fade(tw), fade(th));
    let mut spec = vec![C64::new(0.0, 0.0); tw * th];
    for y in 0..th {
        let row = m.row(reflect(oy + y as isize, m.height));
        for x in 0..tw {
            spec[y * tw + x] = C64::new(fx[x] * fy[y] * row[reflect(ox + x as isize, m.width)], 0.0);
        }
    }
    fft.run(&mut spec, false);
    spec
}

/// `Ĉ1` and `Ĉ2` at one baseband bin, from `z = G⁻¹y`.
fn chroma_bin(z: &[C64; N], sc: &[[f64; N]; 2], from: &[Vec<(usize, C64)>; 2], bin: usize) -> (C64, C64) {
    let mut c = [C64::new(0.0, 0.0); 2];
    for (kind, list) in from.iter().enumerate() {
        let n = list.len() / N;
        let mut acc = C64::new(0.0, 0.0);
        for &(i, ak) in &list[bin * n..(bin + 1) * n] {
            acc += ak * z[i];
        }
        c[kind] = acc * sc[kind][bin];
    }
    (c[0], c[1])
}

fn pack(c1: C64, c2: C64) -> C64 {
    // `Ĉ1 + i·Ĉ2`. Both pictures are real, so one inverse transform returns both.
    C64::new(c1.re - c2.im, c1.im + c2.re)
}

fn pack_conj(c1: C64, c2: C64) -> C64 {
    // At `−f`: the conjugate of each chrominance, not of the two packed.
    C64::new(c1.re + c2.im, c2.re - c1.im)
}

/// One window. `chroma` holds `Ĉ1 + i·Ĉ2`, `energy` holds `G⁻¹m`, both in the spectrum.
fn demod(lat: &Lattice, fact: &Factored, spec: &[C64], chroma: &mut [C64], energy: &mut [C64]) {
    let (tw, th) = (lat.tw, lat.th);
    let (qw, qh) = (tw / 2, th / 2);
    chroma.fill(C64::new(0.0, 0.0));
    energy.fill(C64::new(0.0, 0.0));
    for v0 in 0..=qh / 2 {
        let mirror = mirrored(v0, qh);
        for u0 in 0..qw {
            let set = v0 * qw + u0;
            let g = &fact.factors[set * N * N..(set + 1) * N * N];
            let sc = &fact.powers[set];
            let mut y = [C64::new(0.0, 0.0); N];
            set_spectrum(spec, tw, u0, v0, qw, qh, &mut y);
            solve(g, &mut y, N);
            for b in 0..2 {
                for a in 0..2 {
                    let bin = b * 2 + a;
                    let (c1, c2) = chroma_bin(&y, sc, &lat.from, bin);
                    let x = u0 + a * qw;
                    let yb = v0 + b * qh;
                    chroma[yb * tw + x] = pack(c1, c2);
                    energy[yb * tw + x] = y[bin];
                    if let Some(mv) = mirror {
                        let x2 = (tw - x) % tw;
                        let y2 = mv + (1 - b) * qh;
                        chroma[y2 * tw + x2] = pack_conj(c1, c2);
                        energy[y2 * tw + x2] = y[bin].conj();
                    }
                }
            }
        }
    }
}

/// How the spectra of a bank are weighed at a pixel.
///
/// Two things are asked of a spectrum there. The first is how probable the
/// mosaic around the pixel is under it, `z·m` and `log det G`, taken over
/// a region `sigma` wide. The second is how well it tells each photosite
/// from all the others, the photosite itself left out, over a region
/// `near` wide. The likelihood is steady and says what kind of picture the
/// region is. The left-out error is taken from far fewer photosites and
/// says on which side of an edge the pixel lies. The score is the two
/// logarithms, the second with the share `left_out`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Weighing {
    pub sigma: f64,
    pub near: f64,
    pub left_out: f64,
    /// How fast a spectrum falls behind the best one.
    pub sharp: f64,
    /// Added to the score of the first spectrum of the bank.
    pub bias: f64,
}

impl Default for Weighing {
    fn default() -> Self {
        Self { sigma: 6.0, near: 1.0, left_out: 0.5, sharp: 20.0, bias: 0.0 }
    }
}

struct Frame {
    sx: usize,
    sy: usize,
    margin: usize,
}

fn frame_of(w: usize, h: usize) -> Frame {
    assert!(w >= 2 && h >= 2, "a Bayer frame has two rows and two columns");
    // The margin and the step are even, so every window starts on red. A
    // side may be odd: what a window holds past the frame is the frame
    // mirrored about a photosite, and that keeps the colour.
    let sx = TILE.min(w);
    let sy = TILE.min(h);
    debug_assert!(TILE.is_multiple_of(2) && MARGIN.is_multiple_of(2));
    Frame { sx, sy, margin: MARGIN }
}

fn origins(w: usize, h: usize, fr: &Frame) -> Vec<(usize, usize)> {
    (0..h).step_by(fr.sy).flat_map(|y| (0..w).step_by(fr.sx).map(move |x| (x, y))).collect()
}

struct Kept {
    c1: Vec<f64>,
    c2: Vec<f64>,
    score: Vec<f64>,
}

#[derive(Clone, Copy)]
struct Pass<'a> {
    m: &'a Plane,
    lat: &'a Lattice,
    fact: &'a Factored,
    fr: &'a Frame,
    head: f64,
    weighing: &'a Weighing,
}

fn window_of(pass: &Pass<'_>, spec: &[C64], x0: usize, y0: usize) -> Kept {
    let Pass { m, lat, fact, fr, head, weighing } = *pass;
    let (ox, oy) = (x0 as isize - fr.margin as isize, y0 as isize - fr.margin as isize);
    let mut chroma = vec![C64::new(0.0, 0.0); lat.tw * lat.th];
    let mut energy = vec![C64::new(0.0, 0.0); lat.tw * lat.th];
    demod(lat, fact, spec, &mut chroma, &mut energy);
    lat.fft.run(&mut chroma, true);
    lat.fft.run(&mut energy, true);
    let (tw, th) = (lat.tw, lat.th);
    let smoothed = |sigma: f64, of: &dyn Fn(f64, f64, usize, usize) -> f64| -> Plane {
        let mut e = Plane::zeros(tw, th);
        for y in 0..th {
            let row = m.row(reflect(oy + y as isize, m.height));
            let out = e.row_mut(y);
            for (x, o) in out.iter_mut().enumerate() {
                *o = of(energy[y * tw + x].re, row[reflect(ox + x as isize, m.width)], x, y);
            }
        }
        let k = gaussian_kernel(sigma);
        let mut rows = Plane::zeros(tw, th);
        let mut smooth = Plane::zeros(tw, th);
        convolve_rows_mod_into(&e, &k, |_, _| 1.0, &mut rows);
        convolve_cols_signed_into(&rows, &k, |_| 1.0, &mut smooth);
        smooth
    };
    let share = weighing.left_out.clamp(0.0, 1.0);
    // `z·m` over the region is the mosaic's own power there, in the prior's measure.
    let likely = (share < 1.0).then(|| smoothed(weighing.sigma, &|z, v, _, _| z * v));
    let apart = (share > 0.0).then(|| smoothed(weighing.near, &|z, _, x, y| fact.apart(z, x, y).powi(2)));
    let (nx, ny) = (fr.sx.min(m.width - x0), fr.sy.min(m.height - y0));
    let mut kept = Kept { c1: vec![0.0; nx * ny], c2: vec![0.0; nx * ny], score: vec![0.0; nx * ny] };
    for y in 0..ny {
        for x in 0..nx {
            let i = y * nx + x;
            let s = (y + fr.margin) * tw + (x + fr.margin);
            kept.c1[i] = chroma[s].re;
            kept.c2[i] = chroma[s].im;
            let mut score = -head;
            if let Some(p) = &likely {
                score += (1.0 - share) * (p.data[s].max(1e-30).ln() + fact.logdet);
            }
            if let Some(p) = &apart {
                score += share * p.data[s].max(1e-30).ln();
            }
            kept.score[i] = score;
        }
    }
    kept
}

/// The two chrominances under several spectra at once, weighed per pixel
/// as [`Weighing`] says. One spectrum is that spectrum alone.
///
/// `C1` is `G − (R + B)/2` and `C2` is `(R − B)/2`, the same two the second
/// step reads. The frame's corner is red.
pub fn separate_bank(m: &Plane, priors: &[Spectrum], sel: &Weighing, done: &dyn Fn(f64)) -> (Plane, Plane) {
    assert!(!priors.is_empty(), "there is no prior to separate under");
    let (w, h) = (m.width, m.height);
    let fr = frame_of(w, h);
    let at = origins(w, h, &fr);
    let (tw, th) = ((fr.sx + 2 * fr.margin).div_ceil(2) * 2, (fr.sy + 2 * fr.margin).div_ceil(2) * 2);
    let lat = Lattice::new(tw, th);
    struct Running {
        nx: usize,
        ny: usize,
        a1: Vec<f64>,
        a2: Vec<f64>,
        total: Vec<f64>,
        best: Vec<f64>,
    }
    let mut running: Vec<Running> = at
        .iter()
        .map(|&(x0, y0)| {
            let (nx, ny) = (fr.sx.min(w - x0), fr.sy.min(h - y0));
            let zeros = || vec![0.0; nx * ny];
            Running { nx, ny, a1: zeros(), a2: zeros(), total: zeros(), best: vec![f64::INFINITY; nx * ny] }
        })
        .collect();
    // The spectrum depends on the window, not on the prior, so it is taken once.
    let specs: Vec<Vec<C64>> = at
        .par_iter()
        .map(|&(x0, y0)| {
            spectrum(m, &lat.fft, x0 as isize - fr.margin as isize, y0 as isize - fr.margin as isize)
        })
        .collect();
    for (k, prior) in priors.iter().enumerate() {
        let fact = Factored::build(&lat, prior);
        let head = if k == 0 { sel.bias } else { 0.0 };
        let pass = Pass { m, lat: &lat, fact: &fact, fr: &fr, head, weighing: sel };
        let kept: Vec<Kept> =
            specs.par_iter().zip(&at).map(|(spec, &(x0, y0))| window_of(&pass, spec, x0, y0)).collect();
        for (run, got) in running.iter_mut().zip(&kept) {
            for i in 0..run.nx * run.ny {
                let s = got.score[i];
                if s < run.best[i] {
                    let down = if run.best[i].is_finite() {
                        (-0.5 * sel.sharp * (run.best[i] - s)).exp()
                    } else {
                        0.0
                    };
                    run.a1[i] *= down;
                    run.a2[i] *= down;
                    run.total[i] *= down;
                    run.best[i] = s;
                }
                // Below one part in 10¹⁸ of the best the sums do not move.
                let behind = 0.5 * sel.sharp * (s - run.best[i]);
                if behind > 42.0 {
                    continue;
                }
                let wt = (-behind).exp();
                run.a1[i] += wt * got.c1[i];
                run.a2[i] += wt * got.c2[i];
                run.total[i] += wt;
            }
        }
        done((k + 1) as f64 / priors.len() as f64);
    }
    let mut c1 = Plane::zeros(w, h);
    let mut c2 = Plane::zeros(w, h);
    for (run, &(x0, y0)) in running.iter().zip(&at) {
        for y in 0..run.ny {
            let at = y * run.nx..(y + 1) * run.nx;
            let total = &run.total[at.clone()];
            for (plane, sums) in [(&mut c1, &run.a1), (&mut c2, &run.a2)] {
                let out = &mut plane.row_mut(y0 + y)[x0..x0 + run.nx];
                for ((o, v), t) in out.iter_mut().zip(&sums[at.clone()]).zip(total) {
                    *o = v / t;
                }
            }
        }
    }
    (c1, c2)
}

/// How probable the windows are under `prior` (Whittle's likelihood, the
/// overall level left free): larger is better. The set at the origin holds
/// the mean, which is not part of the power law, and it is left out: its
/// Nyquist bins are luminance as much as colour, and counting them pulls
/// the colour up. Every `stride`-th set in each direction is counted.
fn evidence(lat: &Lattice, specs: &[Vec<C64>], prior: &Spectrum, stride: usize) -> f64 {
    let (tw, th) = (lat.tw, lat.th);
    let (qw, qh) = (tw / 2, th / 2);
    let both = qw.is_multiple_of(stride) && qh.is_multiple_of(stride);
    let rows: Vec<(usize, f64)> = (0..qh)
        .step_by(stride)
        .filter_map(|v0| match mirrored(v0, qh) {
            Some(v) if both => (v0 < v).then_some((v0, 2.0)),
            _ => Some((v0, 1.0)),
        })
        .collect();
    let (q, d, n) = rows
        .par_iter()
        .map(|&(v0, times)| {
            let (mut q, mut d, mut n) = (0.0, 0.0, 0.0);
            for u0 in (0..qw).step_by(stride) {
                if u0 == 0 && v0 == 0 {
                    continue;
                }
                let mut g = [C64::new(0.0, 0.0); N * N];
                set_matrix(tw, th, &lat.nz, prior, u0, v0, &mut g);
                cholesky(&mut g, N);
                let logdet: f64 = (0..N).map(|i| 2.0 * g[i * N + i].re.ln()).sum();
                for spec in specs {
                    let mut y = [C64::new(0.0, 0.0); N];
                    set_spectrum(spec, tw, u0, v0, qw, qh, &mut y);
                    q += quad(&g, &mut y, N);
                }
                d += logdet * specs.len() as f64;
                n += (N * specs.len()) as f64;
            }
            (times * q, times * d, times * n)
        })
        .reduce(|| (0.0, 0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2));
    if n <= 0.0 {
        return f64::NEG_INFINITY;
    }
    -(n * (q / n).ln() + d) / n
}

/// The prior under which the mosaic itself is most probable.
///
/// Nelder–Mead over the free ones of the two exponents, the two chrominance
/// shares, the noise and the lens, from the start and from three more starts
/// with more noise, a wider lens and both. Up to nine windows are looked at,
/// every `stride`-th set of each.
pub fn fit_prior(m: &Plane, start: Prior, stride: usize, free: [bool; 6]) -> Prior {
    assert!(stride >= 1, "the stride skips nothing");
    let (w, h) = (m.width, m.height);
    let fr = frame_of(w, h);
    let (tw, th) = ((fr.sx + 2 * fr.margin).div_ceil(2) * 2, (fr.sy + 2 * fr.margin).div_ceil(2) * 2);
    let at = fit_windows(w, h, &fr);
    let lat = Lattice::new(tw, th);
    let specs: Vec<Vec<C64>> = at.par_iter().map(|&(ox, oy)| faded_spectrum(m, &lat.fft, ox, oy)).collect();
    let x0 = [
        start.alpha,
        start.alpha_c,
        start.chroma[0].ln(),
        start.chroma[1].ln(),
        start.noise.ln(),
        start.lens,
    ];
    let dims: Vec<usize> = (0..6).filter(|i| free[*i]).collect();
    let nd = dims.len();
    if nd == 0 {
        return start;
    }
    let to_prior = |z: &[f64]| {
        let mut x = x0;
        for (v, i) in z.iter().zip(&dims) {
            x[*i] = *v;
        }
        Prior {
            alpha: x[0].clamp(1.0, 8.0),
            alpha_c: x[1].clamp(1.0, 8.0),
            chroma: [x[2].clamp(-14.0, 5.0).exp(), x[3].clamp(-14.0, 5.0).exp()],
            noise: x[4].clamp(-25.0, 0.0).exp(),
            lens: x[5].abs().min(4.0),
            ..start
        }
    };
    let cost = |z: &[f64]| -evidence(&lat, &specs, &Spectrum::plain(to_prior(z)), stride);
    let steps = [0.6, 0.6, 1.0, 1.0, 2.0, 0.35];
    let z_start: Vec<f64> = dims.iter().map(|i| x0[*i]).collect();
    // Little noise behind no lens explains a noisy mosaic nearly as well as
    // the noise does. The search is run from several starts.
    let mut ends: Vec<(Vec<f64>, f64)> = Vec::new();
    for (more_noise, more_lens) in [(0.0, 0.0), (6.0, 0.0), (0.0, 0.5), (6.0, 0.5)] {
        let mut z0 = z_start.clone();
        for (v, i) in z0.iter_mut().zip(&dims) {
            match *i {
                4 => *v += more_noise,
                5 => *v += more_lens,
                _ => {}
            }
        }
        let mut simplex: Vec<(Vec<f64>, f64)> = vec![(z0.clone(), cost(&z0))];
        for (k, i) in dims.iter().enumerate() {
            let mut z = z0.clone();
            z[k] += steps[*i];
            simplex.push((z.clone(), cost(&z)));
        }
        for _ in 0..220 {
            simplex.sort_by(|a, b| a.1.total_cmp(&b.1));
            if simplex[nd].1 - simplex[0].1 < 2e-6 {
                break;
            }
            let mut centre = vec![0.0; nd];
            for (z, _) in &simplex[..nd] {
                for (c, v) in centre.iter_mut().zip(z) {
                    *c += v / nd as f64;
                }
            }
            let worst = simplex[nd].clone();
            let along = |t: f64| -> (Vec<f64>, f64) {
                let z: Vec<f64> = centre.iter().zip(&worst.0).map(|(c, v)| c + t * (v - c)).collect();
                (z.clone(), cost(&z))
            };
            let reflected = along(-1.0);
            if reflected.1 < simplex[0].1 {
                let expanded = along(-2.0);
                simplex[nd] = if expanded.1 < reflected.1 { expanded } else { reflected };
            } else if nd > 1 && reflected.1 < simplex[nd - 1].1 {
                simplex[nd] = reflected;
            } else {
                let contracted = along(if reflected.1 < worst.1 { -0.5 } else { 0.5 });
                if contracted.1 < worst.1.min(reflected.1) {
                    simplex[nd] = contracted;
                } else {
                    let best = simplex[0].0.clone();
                    for v in simplex.iter_mut().skip(1) {
                        for (x, b) in v.0.iter_mut().zip(&best) {
                            *x = b + 0.5 * (*x - b);
                        }
                        v.1 = cost(&v.0);
                    }
                }
            }
        }
        simplex.sort_by(|a, b| a.1.total_cmp(&b.1));
        ends.push(simplex.swap_remove(0));
    }
    ends.sort_by(|a, b| a.1.total_cmp(&b.1));
    to_prior(&ends[0].0)
}

/// The spectrum under which the mosaic itself is most probable.
///
/// The prior first, by [`fit_prior`]: two power laws behind a Gaussian
/// lens. Then, with everything else held, what a Gaussian cannot say: the
/// lens radius by radius, and with it the noise. See [`measure_spectrum`].
pub fn fit_spectrum(m: &Plane, start: Prior, stride: usize, rounds: usize) -> Spectrum {
    measure_spectrum(m, fit_prior(m, start, stride, [true; 6]), rounds)
}

/// The lens by radius and the noise, around a prior already found.
///
/// Both are variances, and expectation-maximisation moves each to the mean
/// of what the mosaic gives it: every round the luminance and the two
/// chrominances most probable at a bin say how much power they have there
/// against what the spectrum allowed them, and the lens at that radius
/// becomes the mean of the three. The mosaic is never less probable after
/// a round than before.
///
/// The lens does not rise with frequency. Where the luminance near the
/// sensor's limit and a slow chrominance fall on the same bins, the mosaic
/// cannot say which it is, and a lens that may rise takes the chrominance's
/// carrier for luminance.
///
/// `rounds` is 100 where the pictures are made. Measured on seven Kodak pictures, 300
/// and 1000 rounds make the mosaic more probable still and the picture
/// worse, by up to 0.2 and 0.4 dB: the noise grows at the cost of the
/// lens's last part.
pub fn measure_spectrum(m: &Plane, prior: Prior, rounds: usize) -> Spectrum {
    if rounds == 0 {
        return Spectrum::plain(prior);
    }
    let (w, h) = (m.width, m.height);
    let fr = frame_of(w, h);
    let (tw, th) = ((fr.sx + 2 * fr.margin).div_ceil(2) * 2, (fr.sy + 2 * fr.margin).div_ceil(2) * 2);
    let at = fit_windows(w, h, &fr);
    let lat = Lattice::new(tw, th);
    let specs: Vec<Vec<C64>> = at.par_iter().map(|&(ox, oy)| faded_spectrum(m, &lat.fft, ox, oy)).collect();
    let (qw, qh) = (tw / 2, th / 2);

    // The search leaves the level free. Here it is the mosaic's own, between 0.05 and 0.2 c/px.
    let gaussian = Spectrum::plain(Prior { oriented: None, ..prior });
    let (mut have, mut want) = (0.0, 0.0);
    for v in 0..th {
        for u in 0..tw {
            let (fu, fv) = (bin_freq(u, tw), bin_freq(v, th));
            if (0.05..0.2).contains(&fu.hypot(fv)) {
                have += specs.iter().map(|s| s[v * tw + u].norm_sqr()).sum::<f64>() / specs.len() as f64;
                want += power(&gaussian, fu, fv).0;
            }
        }
    }
    let level = have / want;
    // A lens that passes everything, to start from: a round takes away
    // what is not there much faster than it adds what is.
    let mut lens = vec![level; LENS_STEPS];
    let mut noise = level * prior.noise;
    let step_of = |r: f64| ((r / LENS_STEP) as usize).min(LENS_STEPS - 1);

    #[derive(Clone)]
    struct Sums {
        lens: Vec<f64>,
        count: Vec<f64>,
        noise: f64,
        bins: f64,
    }
    let zero = || Sums { lens: vec![0.0; LENS_STEPS], count: vec![0.0; LENS_STEPS], noise: 0.0, bins: 0.0 };
    for _ in 0..rounds {
        let spec =
            Spectrum { prior: Prior { noise, oriented: None, ..prior }, lens: Some(Arc::new(lens.clone())) };
        let sums = (0..=qh / 2)
            .into_par_iter()
            .map(|v0| {
                let mut a = zero();
                let times = if mirrored(v0, qh).is_some() { 2.0 } else { 1.0 };
                for u0 in 0..qw {
                    if u0 == 0 && v0 == 0 {
                        continue;
                    }
                    let mut g = [C64::new(0.0, 0.0); N * N];
                    set_matrix(tw, th, &lat.nz, &spec, u0, v0, &mut g);
                    // Per bin of the set: its step of the lens, and the power
                    // of the luminance and of the two chrominances there.
                    let mut step = [0usize; N];
                    let mut parts = [[0.0; 3]; N];
                    for b in 0..2 {
                        for a2 in 0..2 {
                            let i = b * 2 + a2;
                            let (u, v) = (u0 + a2 * qw, v0 + b * qh);
                            let (l, c) = bin_power(&spec, u, v, tw, th);
                            step[i] = step_of(bin_freq(u, tw).hypot(bin_freq(v, th)));
                            parts[i] = [l, c[0], c[1]];
                        }
                    }
                    cholesky(&mut g, N);
                    let mut inv = [[C64::new(0.0, 0.0); N]; N];
                    for j in 0..N {
                        let mut col = [C64::new(0.0, 0.0); N];
                        col[j] = C64::new(1.0, 0.0);
                        solve(&g, &mut col, N);
                        for i in 0..N {
                            inv[i][j] = col[i];
                        }
                    }
                    for one in &specs {
                        let mut z = [C64::new(0.0, 0.0); N];
                        set_spectrum(one, tw, u0, v0, qw, qh, &mut z);
                        solve(&g, &mut z, N);
                        for i in 0..N {
                            // A variance `s` the mosaic sees through `a` becomes
                            // `s·(1 + s·(|aᴴz|² − aᴴG⁻¹a))`. Noise and
                            // luminance are seen at their own bin.
                            let more = z[i].norm_sqr() - inv[i][i].re;
                            a.noise += times * (1.0 + noise * more);
                            let mut ratios = 1.0 + parts[i][0] * more;
                            for (kind, list) in lat.from.iter().enumerate() {
                                let n = list.len() / N;
                                let mine = &list[i * n..(i + 1) * n];
                                let mut seen = C64::new(0.0, 0.0);
                                let mut known = 0.0;
                                for &(p, ap) in mine {
                                    seen += ap.conj() * z[p];
                                    for &(q, aq) in mine {
                                        known += (ap.conj() * inv[p][q] * aq).re;
                                    }
                                }
                                ratios += 1.0 + parts[i][1 + kind] * (seen.norm_sqr() - known);
                            }
                            a.lens[step[i]] += times * ratios;
                            a.count[step[i]] += times * 3.0;
                        }
                        a.bins += times * N as f64;
                    }
                }
                a
            })
            .reduce(zero, |mut a, b| {
                for i in 0..LENS_STEPS {
                    a.lens[i] += b.lens[i];
                    a.count[i] += b.count[i];
                }
                a.noise += b.noise;
                a.bins += b.bins;
                a
            });
        for b in 0..LENS_STEPS {
            if sums.count[b] > 0.0 {
                lens[b] *= (sums.lens[b] / sums.count[b]).max(1e-6);
            } else if b > 0 {
                lens[b] = lens[b - 1];
            }
        }
        falling(&mut lens, &sums.count);
        noise *= (sums.noise / sums.bins).max(1e-6);
    }
    // Back to the prior's own measure: the lens passes everything at rest.
    let top = lens[0];
    for v in &mut lens {
        *v /= top;
    }
    Spectrum { prior: Prior { noise: noise / top, ..prior }, lens: Some(Arc::new(lens)) }
}

/// The nearest sequence that does not rise, in the logarithm, each value
/// weighed by `weight`: pool adjacent violators.
fn falling(values: &mut [f64], weight: &[f64]) {
    let mut blocks: Vec<(f64, f64, usize)> = Vec::new();
    for (v, w) in values.iter().zip(weight) {
        let w = w.max(1.0);
        blocks.push((v.max(1e-300).ln() * w, w, 1));
        while blocks.len() > 1 {
            let k = blocks.len();
            if blocks[k - 2].0 / blocks[k - 2].1 >= blocks[k - 1].0 / blocks[k - 1].1 {
                break;
            }
            let (sum, w, n) = blocks.pop().expect("two blocks");
            let last = blocks.last_mut().expect("one block");
            last.0 += sum;
            last.1 += w;
            last.2 += n;
        }
    }
    let mut at = 0;
    for (sum, w, n) in blocks {
        for v in &mut values[at..at + n] {
            *v = (sum / w).exp();
        }
        at += n;
    }
}

/// Where the search looks: up to nine windows, the corners, the edges and the middle.
fn fit_windows(w: usize, h: usize, fr: &Frame) -> Vec<(isize, isize)> {
    let pick = |n: usize, s: usize| -> Vec<usize> {
        let all: Vec<usize> = (0..n).step_by(s).collect();
        match all.len() {
            0..=3 => all,
            k => vec![all[0], all[k / 2], all[k - 1]],
        }
    };
    let mg = fr.margin as isize;
    pick(h, fr.sy)
        .into_iter()
        .flat_map(|y0| pick(w, fr.sx).into_iter().map(move |x0| (x0 as isize - mg, y0 as isize - mg)))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use mimizan_lab_xtrans::xtrans::{self, XTile};

    fn tile() -> XTile {
        XTile::from_name("RGRGRGGBGBGBRGRGRGGBGBGBRGRGRGGBGBGB").expect("Bayer as six")
    }

    #[test]
    fn the_carriers_are_the_bayer_ones() {
        let [c1, c2] = carriers();
        assert_eq!(c1.len(), 1, "green-magenta has one carrier");
        assert_eq!(c1[0].0, 1);
        assert_eq!(c1[0].1, 1);
        assert!((c1[0].2.re + 0.5).abs() < 1e-12 && c1[0].2.im.abs() < 1e-12, "C1 at (1/2, 1/2)");
        assert_eq!(c2.len(), 2, "red-blue has the two axis carriers");
        for (k, l, a) in &c2 {
            assert!(a.im.abs() < 1e-12 && (a.re - 0.5).abs() < 1e-12, "C2 coefficient");
            assert_eq!(*k + *l, 1, "one of k, l is the Nyquist bin");
        }
    }

    #[test]
    fn a_known_hermitian_system_solves() {
        // [[4, 1+i], [1−i, 6]] · [1+i, 2] = [6+6i, 14]
        let mut g = [C64::new(4.0, 0.0), C64::new(1.0, 1.0), C64::new(1.0, -1.0), C64::new(6.0, 0.0)];
        cholesky(&mut g, 2);
        let mut y = [C64::new(6.0, 6.0), C64::new(14.0, 0.0)];
        solve(&g, &mut y, 2);
        assert!((y[0].re - 1.0).abs() < 1e-9 && (y[0].im - 1.0).abs() < 1e-9);
        assert!((y[1].re - 2.0).abs() < 1e-9 && y[1].im.abs() < 1e-9);
    }

    /// A mosaic with no order in it, the same on every run.
    fn scattered(w: usize, h: usize, seed: u64) -> Plane {
        let mut s = seed;
        let data = (0..w * h)
            .map(|_| {
                s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
                (s >> 11) as f64 / (1u64 << 53) as f64
            })
            .collect();
        Plane::from_vec(w, h, data)
    }

    /// `a·x = b` for a symmetric positive `a`, by elimination with pivoting.
    fn solve_dense(mut a: Vec<f64>, mut b: Vec<f64>, n: usize) -> Vec<f64> {
        for col in 0..n {
            let pivot =
                (col..n).max_by(|&i, &j| a[i * n + col].abs().total_cmp(&a[j * n + col].abs())).unwrap();
            for k in 0..n {
                a.swap(col * n + k, pivot * n + k);
            }
            b.swap(col, pivot);
            for row in col + 1..n {
                let f = a[row * n + col] / a[col * n + col];
                for k in col..n {
                    a[row * n + k] -= f * a[col * n + k];
                }
                b[row] -= f * b[col];
            }
        }
        for row in (0..n).rev() {
            for k in row + 1..n {
                b[row] -= a[row * n + k] * b[k];
            }
            b[row] /= a[row * n + row];
        }
        b
    }

    /// Which way a photosite leans in each chrominance.
    fn patterns(w: usize) -> (impl Fn(usize) -> f64, impl Fn(usize) -> f64) {
        let pg = move |i: usize| if (i % w) % 2 != (i / w) % 2 { 0.5 } else { -0.5 };
        let pd = move |i: usize| match ((i % w) % 2, (i / w) % 2) {
            (0, 0) => 1.0,
            (1, 1) => -1.0,
            _ => 0.0,
        };
        (pg, pd)
    }

    /// The covariance of every pair of photosites of a periodic frame, summed
    /// from the prior's spectrum, and the chrominance's own for every lag.
    /// Nothing here knows of sets, carriers or mirrored rows.
    fn dense_covariance(w: usize, h: usize, prior: &Spectrum) -> (Vec<f64>, Vec<[f64; 2]>) {
        let n = w * h;
        let mut lag_l = vec![0.0; n];
        let mut lag_c = vec![[0.0; 2]; n];
        for dy in 0..h {
            for dx in 0..w {
                let (mut l, mut c) = (0.0, [0.0; 2]);
                for v in 0..h {
                    for u in 0..w {
                        let (pl, pc) = bin_power(prior, u, v, w, h);
                        let turn =
                            (2.0 * PI * ((u * dx) as f64 / w as f64 + (v * dy) as f64 / h as f64)).cos();
                        l += pl * turn;
                        c[0] += pc[0] * turn;
                        c[1] += pc[1] * turn;
                    }
                }
                lag_l[dy * w + dx] = l / n as f64;
                lag_c[dy * w + dx] = [c[0] / n as f64, c[1] / n as f64];
            }
        }
        let (pg, pd) = patterns(w);
        let mut cov = vec![0.0; n * n];
        for i in 0..n {
            for j in 0..n {
                let d = lag_of(i, j, w, h);
                cov[i * n + j] = lag_l[d]
                    + pg(i) * pg(j) * lag_c[d][0]
                    + pd(i) * pd(j) * lag_c[d][1]
                    + if i == j { prior.prior.noise } else { 0.0 };
            }
        }
        (cov, lag_c)
    }

    fn lag_of(i: usize, j: usize, w: usize, h: usize) -> usize {
        ((j / w + h - i / w) % h) * w + (j % w + w - i % w) % w
    }

    /// The most probable chrominances of a periodic frame: one system of
    /// `w·h` unknowns, photosite against photosite.
    fn dense_chroma(m: &Plane, prior: &Spectrum) -> (Vec<f64>, Vec<f64>) {
        let (w, h) = (m.width, m.height);
        let n = w * h;
        let (cov, lag_c) = dense_covariance(w, h, prior);
        let (pg, pd) = patterns(w);
        let z = solve_dense(cov, m.data.clone(), n);
        let mut c1 = vec![0.0; n];
        let mut c2 = vec![0.0; n];
        for i in 0..n {
            for (j, zj) in z.iter().enumerate() {
                let d = lag_of(i, j, w, h);
                c1[i] += lag_c[d][0] * pg(j) * zj;
                c2[i] += lag_c[d][1] * pd(j) * zj;
            }
        }
        (c1, c2)
    }

    /// `ln det` of a symmetric positive matrix, by its real Cholesky factor.
    fn dense_logdet(mut a: Vec<f64>, n: usize) -> f64 {
        let mut sum = 0.0;
        for j in 0..n {
            let mut d = a[j * n + j];
            for k in 0..j {
                d -= a[j * n + k] * a[j * n + k];
            }
            assert!(d > 0.0, "the covariance is not positive");
            let d = d.sqrt();
            a[j * n + j] = d;
            sum += 2.0 * d.ln();
            for i in j + 1..n {
                let mut v = a[i * n + j];
                for k in 0..j {
                    v -= a[i * n + k] * a[j * n + k];
                }
                a[i * n + j] = v / d;
            }
        }
        sum
    }

    /// How probable a periodic frame is, the level left free and the set at
    /// the origin left out, from the covariance of every pair of photosites.
    fn dense_evidence(m: &Plane, prior: &Spectrum) -> f64 {
        let (w, h) = (m.width, m.height);
        let n = w * h;
        let (cov, _) = dense_covariance(w, h, prior);
        // The four pictures of the set at the origin: flat, and the three checkers.
        let modes: Vec<Vec<f64>> = [(0, 0), (1, 0), (0, 1), (1, 1)]
            .iter()
            .map(|&(a, b)| {
                (0..n)
                    .map(
                        |i| if (a * (i % w) + b * (i / w)) % 2 == 0 { 1.0 } else { -1.0 } / (n as f64).sqrt(),
                    )
                    .collect()
            })
            .collect();
        let mut rest = m.data.clone();
        for mode in &modes {
            let along: f64 = mode.iter().zip(&m.data).map(|(a, b)| a * b).sum();
            for (r, a) in rest.iter_mut().zip(mode) {
                *r -= along * a;
            }
        }
        let z = solve_dense(cov.clone(), rest.clone(), n);
        let q: f64 = rest.iter().zip(&z).map(|(a, b)| a * b).sum();
        let mut block = vec![0.0; 16];
        for (a, ma) in modes.iter().enumerate() {
            for (b, mb) in modes.iter().enumerate() {
                let mut s = 0.0;
                for i in 0..n {
                    for j in 0..n {
                        s += ma[i] * cov[i * n + j] * mb[j];
                    }
                }
                block[a * 4 + b] = s;
            }
        }
        let d = dense_logdet(cov, n) - dense_logdet(block, 4);
        let count = (n - 4) as f64;
        // The transform is not normalised: a spectrum's square is `w·h` times the frame's.
        -(count * (n as f64 * q / count).ln() + d) / count
    }

    /// Unit normal numbers with no order in them, the same on every run.
    fn normal(n: usize, seed: u64) -> Vec<f64> {
        let flat = scattered(2 * n, 1, seed).data;
        (0..n)
            .map(|i| (-2.0 * (1.0 - flat[2 * i]).ln()).sqrt() * (2.0 * PI * flat[2 * i + 1]).cos())
            .collect()
    }

    /// A periodic mosaic drawn from a spectrum: a luminance, two
    /// chrominances and noise, each with the power the spectrum gives it.
    fn drawn(w: usize, h: usize, spec: &Spectrum, seed: u64) -> Plane {
        let fft = Fft2::new(w, h);
        let n = w * h;
        let field = |kind: usize| -> Vec<f64> {
            let mut f: Vec<C64> =
                normal(n, seed + kind as u64).into_iter().map(|v| C64::new(v, 0.0)).collect();
            fft.run(&mut f, false);
            for v in 0..h {
                for u in 0..w {
                    let (l, c) = bin_power(spec, u, v, w, h);
                    f[v * w + u] *= [l, c[0], c[1]][kind].sqrt();
                }
            }
            fft.run(&mut f, true);
            f.iter().map(|c| c.re).collect()
        };
        let (l, c1, c2) = (field(0), field(1), field(2));
        let noise = normal(n, seed + 3);
        let (pg, pd) = patterns(w);
        let data = (0..n)
            .map(|i| l[i] + pg(i) * c1[i] + pd(i) * c2[i] + spec.prior.noise.sqrt() * noise[i])
            .collect();
        Plane::from_vec(w, h, data)
    }

    /// How far each photosite of the windows is from what all the others say
    /// of it under `spec`: the mean square, inside the margin.
    fn left_out(lat: &Lattice, specs: &[Vec<C64>], spec: &Spectrum, margin: usize) -> f64 {
        let fact = Factored::build(lat, spec);
        let (tw, th) = (lat.tw, lat.th);
        let (qw, qh) = (tw / 2, th / 2);
        let sums: (f64, f64) = specs
            .par_iter()
            .map(|one| {
                let mut z = vec![C64::new(0.0, 0.0); tw * th];
                for v0 in 0..=qh / 2 {
                    let mirror = mirrored(v0, qh);
                    for u0 in 0..qw {
                        let set = v0 * qw + u0;
                        let g = &fact.factors[set * N * N..(set + 1) * N * N];
                        let mut y = [C64::new(0.0, 0.0); N];
                        set_spectrum(one, tw, u0, v0, qw, qh, &mut y);
                        solve(g, &mut y, N);
                        for b in 0..2 {
                            for a in 0..2 {
                                let (x, yb) = (u0 + a * qw, v0 + b * qh);
                                z[yb * tw + x] = y[b * 2 + a];
                                if let Some(mv) = mirror {
                                    z[(mv + (1 - b) * qh) * tw + (tw - x) % tw] = y[b * 2 + a].conj();
                                }
                            }
                        }
                    }
                }
                lat.fft.run(&mut z, true);
                let (mut sum, mut n) = (0.0, 0.0);
                for y in margin..th - margin {
                    for x in margin..tw - margin {
                        sum += fact.apart(z[y * tw + x].re, x, y).powi(2);
                        n += 1.0;
                    }
                }
                (sum, n)
            })
            .reduce(|| (0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1));
        sums.0 / sums.1
    }

    /// The prior, one of its six numbers moved.
    fn moved(prior: Prior, which: usize, by: f64) -> Prior {
        match which {
            0 => Prior { alpha: prior.alpha * by, ..prior },
            1 => Prior { alpha_c: prior.alpha_c * by, ..prior },
            2 => Prior { chroma: [prior.chroma[0] * by, prior.chroma[1]], ..prior },
            3 => Prior { chroma: [prior.chroma[0], prior.chroma[1] * by], ..prior },
            4 => Prior { noise: prior.noise * by, ..prior },
            _ => Prior { lens: prior.lens * by, ..prior },
        }
    }

    #[test]
    fn a_frame_is_most_probable_under_the_spectrum_it_was_drawn_from() {
        let prior = Prior {
            alpha: 2.4,
            alpha_c: 3.2,
            lens: 0.5,
            noise: 2e-3,
            chroma: [0.05, 0.02],
            ..Prior::default()
        };
        let (w, h) = (192usize, 160usize);
        let m = drawn(w, h, &Spectrum::plain(prior), 71);
        let lat = Lattice::new(w, h);
        let specs = [spectrum(&m, &lat.fft, 0, 0)];
        let own = evidence(&lat, &specs, &Spectrum::plain(prior), 1);
        for which in 0..6 {
            for by in [1.0 / 1.3, 1.3] {
                let other = evidence(&lat, &specs, &Spectrum::plain(moved(prior, which, by)), 1);
                assert!(
                    other < own,
                    "number {which} times {by} makes the frame more probable: {other} over {own}"
                );
            }
        }
    }

    #[test]
    fn a_photosite_is_told_best_under_the_spectrum_the_frame_was_drawn_from() {
        let prior = Prior {
            alpha: 2.4,
            alpha_c: 3.2,
            lens: 0.5,
            noise: 0.03,
            chroma: [0.05, 0.02],
            ..Prior::default()
        };
        let (w, h) = (192usize, 160usize);
        let m = drawn(w, h, &Spectrum::plain(prior), 73);
        let lat = Lattice::new(w, h);
        let specs = [spectrum(&m, &lat.fft, 0, 0)];
        let own = left_out(&lat, &specs, &Spectrum::plain(prior), 0);
        for which in 0..6 {
            for by in [0.5, 2.0] {
                let other = left_out(&lat, &specs, &Spectrum::plain(moved(prior, which, by)), 0);
                assert!(
                    other > own,
                    "number {which} times {by} tells the photosites better: {other} under {own}"
                );
            }
        }
    }

    #[test]
    fn the_lens_a_frame_was_drawn_through_is_found() {
        // Flat to 0.2 c/px and gone by 0.4: nothing a Gaussian is like.
        let prior = Prior {
            alpha: 2.4,
            alpha_c: 3.2,
            lens: 0.5,
            noise: 1e-4,
            chroma: [0.05, 0.02],
            ..Prior::default()
        };
        let through = |r: f64| 1.0 / (1.0 + (r / 0.22).powi(6));
        let lens: Vec<f64> = (0..LENS_STEPS).map(|b| through((b as f64 + 0.5) * LENS_STEP)).collect();
        let truth = Spectrum { prior, lens: Some(Arc::new(lens)) };
        let m = drawn(384, 320, &truth, 91);
        let found = measure_spectrum(&m, prior, 100);
        let table = found.lens.as_ref().expect("a lens by radius");
        let at = |r: f64| table[(r / LENS_STEP) as usize] / table[5];
        for r in [0.105, 0.155, 0.205, 0.255, 0.305, 0.355, 0.405, 0.455] {
            let (got, want) = (at(r), through(r) / through(0.055));
            assert!(
                got < 1.7 * want && want < 1.7 * got,
                "at {r} c/px the lens passes {got}, drawn with {want}"
            );
        }
        // The Gaussian the search starts from is far from it where the lens ends.
        let gaussian = Spectrum::plain(prior);
        assert!(gaussian.pass(0.405) / gaussian.pass(0.055) > 3.0 * through(0.405) / through(0.055));
    }

    #[test]
    fn a_photosite_left_out_is_where_the_others_put_it() {
        let plain = Prior { lens: 0.4, noise: 3e-3, chroma: [0.08, 0.03], ..Prior::default() };
        let cases = [
            (12usize, 8usize, Spectrum::plain(plain)),
            (8, 12, Spectrum::plain(Prior { oriented: Some((0.3, 8.0)), ..plain })),
            (10, 6, measured(plain)),
        ];
        for (k, (w, h, prior)) in cases.into_iter().enumerate() {
            let m = scattered(w, h, 29 + k as u64);
            let n = w * h;
            let (cov, _) = dense_covariance(w, h, &prior);
            let lat = Lattice::new(w, h);
            let fact = Factored::build(&lat, &prior);
            let spec = spectrum(&m, &lat.fft, 0, 0);
            let mut chroma = vec![C64::new(0.0, 0.0); n];
            let mut z = vec![C64::new(0.0, 0.0); n];
            demod(&lat, &fact, &spec, &mut chroma, &mut z);
            lat.fft.run(&mut z, true);
            for i in 0..n {
                // Every photosite but this one, and what they say of this one.
                let others: Vec<usize> = (0..n).filter(|j| *j != i).collect();
                let mut rest = vec![0.0; (n - 1) * (n - 1)];
                for (a, &ja) in others.iter().enumerate() {
                    for (b, &jb) in others.iter().enumerate() {
                        rest[a * (n - 1) + b] = cov[ja * n + jb];
                    }
                }
                let seen: Vec<f64> = others.iter().map(|&j| m.data[j]).collect();
                let x = solve_dense(rest, seen, n - 1);
                let said: f64 = others.iter().zip(&x).map(|(&j, v)| cov[i * n + j] * v).sum();
                let want = m.data[i] - said;
                let got = fact.apart(z[i].re, i % w, i / w);
                assert!((got - want).abs() < 1e-8, "case {k}: photosite {i} is {got} apart, not {want}");
            }
        }
    }

    fn spectral_chroma(m: &Plane, prior: &Spectrum) -> (Vec<f64>, Vec<f64>) {
        let (w, h) = (m.width, m.height);
        let lat = Lattice::new(w, h);
        let fact = Factored::build(&lat, prior);
        let spec = spectrum(m, &lat.fft, 0, 0);
        let mut chroma = vec![C64::new(0.0, 0.0); w * h];
        let mut energy = vec![C64::new(0.0, 0.0); w * h];
        demod(&lat, &fact, &spec, &mut chroma, &mut energy);
        lat.fft.run(&mut chroma, true);
        (chroma.iter().map(|c| c.re).collect(), chroma.iter().map(|c| c.im).collect())
    }

    /// A lens that is no Gaussian: flat, and then it ends.
    fn measured(prior: Prior) -> Spectrum {
        let lens = (0..LENS_STEPS).map(|b| 1.0 / (1.0 + (b as f64 / 22.0).powi(6))).collect();
        Spectrum { prior, lens: Some(Arc::new(lens)) }
    }

    #[test]
    fn the_sets_solve_what_the_whole_frame_solves() {
        let plain = Prior { lens: 0.4, noise: 3e-3, chroma: [0.08, 0.03], ..Prior::default() };
        let of = Spectrum::plain;
        let cases = [
            (12usize, 8usize, of(plain)),
            (10, 6, of(plain)),
            (8, 12, of(Prior { alpha: 1.6, alpha_c: 3.5, ..plain })),
            (12, 8, of(Prior { oriented: Some((0.3, 8.0)), ..plain })),
            (10, 6, of(Prior { oriented: Some((PI * 3.0 / 8.0, 32.0)), ..plain })),
            (8, 12, of(Prior { oriented: Some((PI / 2.0, 8.0)), ..plain })),
            (12, 8, measured(plain)),
            (10, 6, measured(Prior { oriented: Some((1.1, 8.0)), ..plain })),
        ];
        for (k, (w, h, prior)) in cases.into_iter().enumerate() {
            let m = scattered(w, h, 17 + k as u64);
            let (d1, d2) = dense_chroma(&m, &prior);
            let (s1, s2) = spectral_chroma(&m, &prior);
            let mut worst = 0.0f64;
            let mut size = 0.0f64;
            for i in 0..w * h {
                worst = worst.max((d1[i] - s1[i]).abs()).max((d2[i] - s2[i]).abs());
                size = size.max(d1[i].abs()).max(d2[i].abs());
            }
            assert!(size > 1e-3, "case {k}: the frame has no colour to compare");
            assert!(
                worst < 1e-9 * size.max(1.0),
                "case {k}: {w}×{h} differs from the dense solve by {worst}"
            );
        }
    }

    #[test]
    fn the_evidence_is_the_likelihood_of_the_whole_frame() {
        let plain = Prior { lens: 0.4, noise: 3e-3, chroma: [0.08, 0.03], ..Prior::default() };
        let of = Spectrum::plain;
        let cases = [
            (12usize, 8usize, of(plain)),
            (10, 6, of(Prior { alpha: 1.6, alpha_c: 3.5, noise: 0.05, ..plain })),
            (8, 12, of(Prior { oriented: Some((0.3, 8.0)), ..plain })),
            (12, 8, measured(plain)),
        ];
        for (k, (w, h, prior)) in cases.into_iter().enumerate() {
            let m = scattered(w, h, 5 + k as u64);
            let lat = Lattice::new(w, h);
            let spec = spectrum(&m, &lat.fft, 0, 0);
            let got = evidence(&lat, &[spec], &prior, 1);
            let want = dense_evidence(&m, &prior);
            assert!((got - want).abs() < 1e-9 * want.abs().max(1.0), "case {k}: {got} is not {want}");
        }
    }

    fn flat(w: usize, h: usize, rgb: [f64; 3]) -> Plane {
        let mut m = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                m[y * w + x] = match (x % 2, y % 2) {
                    (0, 0) => rgb[0],
                    (1, 1) => rgb[2],
                    _ => rgb[1],
                };
            }
        }
        Plane::from_vec(w, h, m)
    }

    fn apart(m: &Plane, prior: &Prior) -> (Plane, Plane, mimizan_lab_xtrans::plane::Rgb) {
        let (c1, c2) = separate_bank(m, &[Spectrum::plain(*prior)], &Weighing::default(), &|_| {});
        let rgb = xtrans::from_chroma(m, &tile(), c1.clone(), c2.clone()).rgb();
        (c1, c2, rgb)
    }

    #[test]
    fn a_flat_colour_comes_back() {
        let rgb = [0.20, 0.60, 0.30];
        let m = flat(64, 48, rgb);
        let prior = Prior { lens: 0.3, ..Prior::default() };
        let (c1, c2, got) = apart(&m, &prior);
        let want1 = rgb[1] - (rgb[0] + rgb[2]) / 2.0;
        let want2 = (rgb[0] - rgb[2]) / 2.0;
        let mut worst = 0.0f64;
        for i in 0..m.data.len() {
            worst = worst.max((c1.data[i] - want1).abs()).max((c2.data[i] - want2).abs());
            worst = worst.max((got.r.data[i] - rgb[0]).abs());
            worst = worst.max((got.g.data[i] - rgb[1]).abs());
            worst = worst.max((got.b.data[i] - rgb[2]).abs());
        }
        assert!(worst < 1e-3, "a flat colour moved by {worst}");
    }

    #[test]
    fn a_green_magenta_checker_is_chrominance() {
        // L = 0.5, C1 = 0.4, C2 = 0. The mosaic is the diagonal carrier and nothing else.
        let m = flat(64, 48, [0.30, 0.70, 0.30]);
        let prior = Prior { lens: 0.3, ..Prior::default() };
        let (_c1, c2, got) = apart(&m, &prior);
        let mut worst_c2 = 0.0f64;
        let mut worst = 0.0f64;
        for i in 0..m.data.len() {
            worst_c2 = worst_c2.max(c2.data[i].abs());
            worst = worst.max((got.r.data[i] - 0.30).abs());
            worst = worst.max((got.g.data[i] - 0.70).abs());
            worst = worst.max((got.b.data[i] - 0.30).abs());
        }
        assert!(worst_c2 < 1e-3, "red-blue appeared in a green-magenta field: {worst_c2}");
        assert!(worst < 1e-3, "the checker was not put back: {worst}");
    }

    #[test]
    fn a_slow_colour_wave_comes_back_inside_the_frame() {
        let (w, h) = (96usize, 64usize);
        let mut data = vec![0.0; w * h];
        let at = |x: usize, y: usize| {
            let r = 0.45 + 0.15 * (2.0 * PI * x as f64 / 32.0).sin();
            let g = 0.40 + 0.08 * (2.0 * PI * y as f64 / 40.0).sin();
            let b = 0.35;
            (r, g, b)
        };
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = at(x, y);
                data[y * w + x] = match (x % 2, y % 2) {
                    (0, 0) => r,
                    (1, 1) => b,
                    _ => g,
                };
            }
        }
        let m = Plane::from_vec(w, h, data);
        let prior = Prior { lens: 0.3, ..Prior::default() };
        let (_c1, _c2, got) = apart(&m, &prior);
        let mut worst = 0.0f64;
        for y in 12..h - 12 {
            for x in 12..w - 12 {
                let (r, g, b) = at(x, y);
                let i = y * w + x;
                worst = worst.max((got.r.data[i] - r).abs());
                worst = worst.max((got.g.data[i] - g).abs());
                worst = worst.max((got.b.data[i] - b).abs());
            }
        }
        assert!(worst < 0.02, "a slow wave moved by {worst}");
    }
}
