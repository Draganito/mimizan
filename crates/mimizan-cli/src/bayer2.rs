//! Least-squares demosaic on the Bayer period.
//!
//! A 2×2 mosaic is `m = L + C1·pg + C2·pd`. The green-magenta carrier sits
//! at `(½, ½)` and the red-blue carriers at `(½, 0)` and `(0, ½)`. Frequencies
//! that differ by those steps are one set, four bins, and the most probable
//! luminance and both chrominances of the set are one 4×4 solve. The prior
//! is the one the mosaic itself makes most probable, a bank of those priors
//! is weighed per pixel, and the second step (a line through two colours)
//! is the one the 6×6 solver uses: it reads the photosite, not the period.
//!
//! The transform is the same convention as that solver. The forward transform
//! is unnormalised and the inverse is divided by the number of samples, so a
//! prior fitted here is not a prior fitted on a 6×6 tile.

use mimizan_lab_xtrans::filter::{
    convolve_cols_signed_into, convolve_rows_mod_into, gaussian_kernel, reflect,
};
use mimizan_lab_xtrans::plane::Plane;
use mimizan_lab_xtrans::xtrans::{Prior, Selection};
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

fn power(prior: &Prior, fu: f64, fv: f64) -> (f64, f64) {
    let r2 = fu * fu + fv * fv;
    let base = (prior.f0 * prior.f0 + r2) / (prior.f0 * prior.f0 + 0.0625);
    let lens = (-4.0 * PI * PI * prior.lens * prior.lens * r2).exp();
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
    (lens * dir * base.powf(-prior.alpha / 2.0), lens * dir * base.powf(-prior.alpha_c / 2.0))
}

/// Powers at bin `(u, v)`. A bin at half the sampling rate stands for `+½`
/// and `−½` alike, which an oriented prior tells apart, so it gets the mean.
fn bin_power(prior: &Prior, u: usize, v: usize, tw: usize, th: usize) -> (f64, f64) {
    let (fu, fv) = (bin_freq(u, tw), bin_freq(v, th));
    let p = power(prior, fu, fv);
    if prior.oriented.is_none() || (2 * u != tw && 2 * v != th) {
        return p;
    }
    let q = if 2 * u == tw { power(prior, -fu, fv) } else { power(prior, fu, -fv) };
    (0.5 * (p.0 + q.0), 0.5 * (p.1 + q.1))
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
    prior: &Prior,
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
            let (l, c) = bin_power(prior, u0 + a * qw, v0 + b * qh, tw, th);
            sl[i] = l;
            sc[0][i] = c * prior.chroma[0];
            sc[1][i] = c * prior.chroma[1];
        }
    }
    *g = [C64::new(0.0, 0.0); N * N];
    for i in 0..N {
        g[i * N + i] = C64::new(sl[i] + prior.noise, 0.0);
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
}

impl Factored {
    fn build(lat: &Lattice, prior: &Prior) -> Self {
        let (tw, th) = (lat.tw, lat.th);
        let (qw, qh) = (tw / 2, th / 2);
        let rows = qh / 2 + 1;
        let mut factors = vec![C64::new(0.0, 0.0); rows * qw * N * N];
        let mut powers = vec![[[0.0; N]; 2]; rows * qw];
        let nz = &lat.nz;
        let logdet: f64 = factors
            .par_chunks_mut(qw * N * N)
            .zip(powers.par_chunks_mut(qw))
            .enumerate()
            .map(|(v0, (frow, prow))| {
                let mut logdet = 0.0;
                for u0 in 0..qw {
                    let g = &mut frow[u0 * N * N..(u0 + 1) * N * N];
                    let mut cell = [C64::new(0.0, 0.0); N * N];
                    prow[u0] = set_matrix(tw, th, nz, prior, u0, v0, &mut cell);
                    cholesky(&mut cell, N);
                    g.copy_from_slice(&cell);
                    logdet += (0..N).map(|i| 2.0 * cell[i * N + i].re.ln()).sum::<f64>();
                }
                // The row stands for its mirror as well, unless it is its own.
                if mirrored(v0, qh).is_some() {
                    2.0 * logdet
                } else {
                    logdet
                }
            })
            .sum();
        Self { factors, powers, logdet: logdet / (tw * th) as f64 }
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

struct Frame {
    sx: usize,
    sy: usize,
    margin: usize,
}

fn frame_of(w: usize, h: usize) -> Frame {
    assert!(w >= 2 && h >= 2 && w.is_multiple_of(2) && h.is_multiple_of(2), "a Bayer frame has even sides");
    // The margin and the step are even, so every window still starts on red.
    let sx = TILE.min(w);
    let sy = TILE.min(h);
    debug_assert!(sx.is_multiple_of(2) && sy.is_multiple_of(2) && MARGIN.is_multiple_of(2));
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
    sigma: f64,
}

fn window_of(pass: &Pass<'_>, spec: &[C64], x0: usize, y0: usize) -> Kept {
    let Pass { m, lat, fact, fr, head, sigma } = *pass;
    let (ox, oy) = (x0 as isize - fr.margin as isize, y0 as isize - fr.margin as isize);
    let mut chroma = vec![C64::new(0.0, 0.0); lat.tw * lat.th];
    let mut energy = vec![C64::new(0.0, 0.0); lat.tw * lat.th];
    demod(lat, fact, spec, &mut chroma, &mut energy);
    lat.fft.run(&mut chroma, true);
    lat.fft.run(&mut energy, true);
    let (tw, th) = (lat.tw, lat.th);
    let mut e = Plane::zeros(tw, th);
    for y in 0..th {
        let row = m.row(reflect(oy + y as isize, m.height));
        let out = e.row_mut(y);
        for x in 0..tw {
            out[x] = energy[y * tw + x].re * row[reflect(ox + x as isize, m.width)];
        }
    }
    let k = gaussian_kernel(sigma);
    let mut rows = Plane::zeros(tw, th);
    let mut smooth = Plane::zeros(tw, th);
    convolve_rows_mod_into(&e, &k, |_, _| 1.0, &mut rows);
    convolve_cols_signed_into(&rows, &k, |_| 1.0, &mut smooth);
    let (nx, ny) = (fr.sx.min(m.width - x0), fr.sy.min(m.height - y0));
    let mut kept = Kept { c1: vec![0.0; nx * ny], c2: vec![0.0; nx * ny], score: vec![0.0; nx * ny] };
    for y in 0..ny {
        for x in 0..nx {
            let i = y * nx + x;
            let s = (y + fr.margin) * tw + (x + fr.margin);
            kept.c1[i] = chroma[s].re;
            kept.c2[i] = chroma[s].im;
            kept.score[i] = smooth.data[s].max(1e-30).ln() + fact.logdet - head;
        }
    }
    kept
}

/// The two chrominances under several priors at once, chosen between per pixel
/// by how probable the mosaic around the pixel is. One prior is that prior alone.
///
/// `C1` is `G − (R + B)/2` and `C2` is `(R − B)/2`, the same two the second
/// step reads. The frame's corner is red.
pub fn separate_bank(m: &Plane, priors: &[Prior], sel: &Selection, done: &dyn Fn(f64)) -> (Plane, Plane) {
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
        let pass = Pass { m, lat: &lat, fact: &fact, fr: &fr, head, sigma: sel.sigma };
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
fn evidence(lat: &Lattice, specs: &[Vec<C64>], prior: &Prior, stride: usize) -> f64 {
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
    let pick = |n: usize, s: usize| -> Vec<usize> {
        let all: Vec<usize> = (0..n).step_by(s).collect();
        match all.len() {
            0..=3 => all,
            k => vec![all[0], all[k / 2], all[k - 1]],
        }
    };
    let mg = fr.margin as isize;
    let at: Vec<(isize, isize)> = pick(h, fr.sy)
        .into_iter()
        .flat_map(|y0| pick(w, fr.sx).into_iter().map(move |x0| (x0 as isize - mg, y0 as isize - mg)))
        .collect();
    let lat = Lattice::new(tw, th);
    let specs: Vec<Vec<C64>> = at.par_iter().map(|&(ox, oy)| spectrum(m, &lat.fft, ox, oy)).collect();
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
    let cost = |z: &[f64]| -evidence(&lat, &specs, &to_prior(z), stride);
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
        let (c1, c2) = separate_bank(m, &[*prior], &Selection::default(), &|_| {});
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
