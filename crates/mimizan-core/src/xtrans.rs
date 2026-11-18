//! EXPERIMENT (branch `xtrans-experiment`, in no release): carrier
//! separation of a Fuji X-Trans mosaic.
//!
//! # The mosaic
//!
//! The 6×6 tile has green on a 3×3 period (five of nine photosites) and
//! red and blue exchanging places on the rest. With `g = 5/9` the mosaic is
//!
//! `m = L + C1·(m_G − g) + C2·(m_R − m_B)`,
//! `L = g·G + (1 − g)·(R + B)/2`, `C1 = G − (R + B)/2`, `C2 = (R − B)/2`.
//!
//! `m_G − g` has eight carriers, `(±1/3, ±1/3)` with amplitude 2/9 and
//! `(±1/3, 0)`, `(0, ±1/3)` with 1/9: four independent copies of `C1` in
//! four directions (Bayer has one, on the diagonal Nyquist). `m_R − m_B`
//! has four, `(1/2, ±1/6)` and `(±1/6, 1/2)` with amplitude 1/3: two
//! copies of `C2` like Bayer's, but 1/6 c/px beside the horizontal and
//! vertical Nyquist instead of on it
//! (`the_tile_has_the_carriers_the_header_names`).
//!
//! # What was tried
//!
//! 1. One low-pass per copy, as the Bayer separation has it
//!    ([`tile_copies`], [`combine`]). No carrier sits at Nyquist, so a
//!    demodulated copy is complex; the chrominance is real, so after
//!    division by the carrier's coefficient the imaginary part (the
//!    quadrature) holds nothing but what leaked into the band, measured
//!    free of the chrominance ([`Weighting::Quadrature`]). A Bayer carrier
//!    is ±1 and has no quadrature. It works as a weight, but the carriers
//!    lie 0.24 c/px apart where Bayer's lie 0.5 apart, and a band narrow
//!    enough to keep the neighbours out loses the colour's detail: about
//!    9 dB behind Bayer on the Kodak set behind a lens.
//! 2. Rings and sectors around every copy, each sector measured by every
//!    copy on both sides of its carrier and averaged with the inverse of
//!    the variances their differences give. Worse than 1; taken out.
//! 3. The least-squares demultiplexer ([`Prior`]): luminance and both
//!    chrominances of the 36 frequencies the tile ties together, solved at
//!    once. It takes the chrominance of one kind out of the copies of the
//!    other, which no filter per copy can. The prior is searched on the
//!    mosaic itself ([`fit_prior`]), and several priors can be weighed
//!    against each other per pixel by how well each explains the mosaic
//!    around it ([`separate_bank`]): more or less colour, and a picture
//!    that varies along one direction only ([`Prior::oriented`]), which
//!    is what a straight edge is and what lets the colour keep its width
//!    across the edge. This is the first step.
//! 4. Choosing between the priors by the roughness of the luminance each
//!    leaves, instead of by the evidence. Worse: a prior with much colour
//!    takes luminance for colour and is rewarded for the smoother rest.
//!    Taken out.
//! 5. The second step ([`Refine`]): where the picture has two colours,
//!    every photosite measures the same scalar through its own gain, and
//!    the colour is as sharp as the mosaic. The first step's colour is
//!    band-limited, and at a hard edge of colour what it misses stands in
//!    the luminance as the tile's pattern, dots along the edge; this
//!    takes them out. Kept only where the luminance comes out smoother,
//!    which here (one candidate against the first step, not many against
//!    each other) is a test that holds.
//!
//! The filters are applied by FFT on tiles: a tool for finding the
//! method, not the form a release would have.

use crate::cfa::Color;
use crate::filter::{gaussian_kernel, lowpass, reflect};
use crate::plane::Plane;
use crate::synth::Rgb;
use rayon::prelude::*;
use rustfft::num_complex::Complex;
use rustfft::{Fft, FftPlanner};
use std::f64::consts::PI;
use std::sync::Arc;

type C64 = Complex<f64>;

/// The tile of every X-Trans sensor rawler reads, row-major, as the X-Pro3
/// has it at its sensor origin.
pub const FUJI: &str = "GGRGGBGGBGGRBRGRBGGGBGGRGGRGGBRBGBRG";

/// A 6×6 colour tile, `[y][x]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct XTile(pub [[Color; 6]; 6]);

impl XTile {
    /// From 36 letters, row-major.
    pub fn from_name(name: &str) -> Option<Self> {
        let b = name.as_bytes();
        if b.len() != 36 {
            return None;
        }
        let mut t = [[Color::G; 6]; 6];
        for (i, c) in b.iter().enumerate() {
            t[i / 6][i % 6] = match c {
                b'R' => Color::R,
                b'G' => Color::G,
                b'B' => Color::B,
                _ => return None,
            };
        }
        Some(Self(t))
    }

    #[inline]
    pub fn color_at(&self, x: isize, y: isize) -> Color {
        self.0[y.rem_euclid(6) as usize][x.rem_euclid(6) as usize]
    }

    /// The tile seen from an origin moved by `(dx, dy)`.
    pub fn shifted(&self, dx: isize, dy: isize) -> Self {
        let mut t = [[Color::G; 6]; 6];
        for (y, row) in t.iter_mut().enumerate() {
            for (x, c) in row.iter_mut().enumerate() {
                *c = self.color_at(x as isize + dx, y as isize + dy);
            }
        }
        Self(t)
    }

    /// Whether mirroring a `w × h` frame at its edges (reflect-101)
    /// continues the tile: the first and last column and row must be
    /// symmetry lines of it.
    pub fn mirrors(&self, w: usize, h: usize) -> bool {
        let (w1, h1) = (w as isize - 1, h as isize - 1);
        (0..6).all(|y| {
            (1..6).all(|d| {
                self.color_at(-d, y) == self.color_at(d, y)
                    && self.color_at(w1 + d, y) == self.color_at(w1 - d, y)
            })
        }) && (0..6).all(|x| {
            (1..6).all(|d| {
                self.color_at(x, -d) == self.color_at(x, d)
                    && self.color_at(x, h1 + d) == self.color_at(x, h1 - d)
            })
        })
    }

    /// The modulation patterns and their Fourier coefficients.
    pub fn pattern(&self) -> Pattern {
        let mut g = 0.0;
        for row in &self.0 {
            for c in row {
                if *c == Color::G {
                    g += 1.0 / 36.0;
                }
            }
        }
        let mut pg = [[0.0; 6]; 6];
        let mut pd = [[0.0; 6]; 6];
        for y in 0..6 {
            for x in 0..6 {
                let c = self.0[y][x];
                pg[y][x] = if c == Color::G { 1.0 - g } else { -g };
                pd[y][x] = match c {
                    Color::R => 1.0,
                    Color::B => -1.0,
                    Color::G => 0.0,
                };
            }
        }
        let dft = |p: &[[f64; 6]; 6]| {
            let mut a = [[C64::new(0.0, 0.0); 6]; 6];
            for (l, row) in a.iter_mut().enumerate() {
                for (k, v) in row.iter_mut().enumerate() {
                    for (y, line) in p.iter().enumerate() {
                        for (x, value) in line.iter().enumerate() {
                            let ph = -2.0 * PI * ((k * x + l * y) % 6) as f64 / 6.0;
                            *v += C64::from_polar(value / 36.0, ph);
                        }
                    }
                }
            }
            a
        };
        Pattern { g, a1: dft(&pg), a2: dft(&pd), pg, pd }
    }
}

/// `m = L + C1·pg + C2·pd`, and the coefficients of `pg`, `pd` on the
/// carriers `(k/6, l/6)`, indexed `[l][k]`.
#[derive(Clone, Debug)]
pub struct Pattern {
    /// Share of green photosites.
    pub g: f64,
    pub pg: [[f64; 6]; 6],
    pub pd: [[f64; 6]; 6],
    pub a1: [[C64; 6]; 6],
    pub a2: [[C64; 6]; 6],
}

/// Half-amplitude cutoffs of one pass band, c/px, along and across the
/// direction named for the copy.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Band {
    pub along: f64,
    pub across: f64,
}

/// How the copies of one chrominance are put together.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Weighting {
    /// By the carrier's power alone (least squares for white noise).
    Fixed,
    /// Inverse of the leak variance measured in the quadrature.
    Quadrature,
    /// Inverse of the copy's own local energy (Dubois' rule, which holds
    /// the chrominance too).
    Energy,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct XParams {
    /// `C1` copies at `(±1/3, ±1/3)`: along the diagonal through the
    /// origin, across it.
    pub c1_diag: Band,
    /// `C1` copies at `(1/3, 0)`, `(0, 1/3)`: along their axis, across
    /// it. `None` leaves them out.
    pub c1_axis: Option<Band>,
    /// `C2` copies: along the Nyquist axis they lie beside, across it.
    pub c2: Band,
    /// Width of every band edge (raised cosine), c/px.
    pub transition: f64,
    /// Gaussian σ (px) of the local leak estimate.
    pub sigma: f64,
    pub weight: Weighting,
    /// Noise floor of the weights, in units of the mosaic.
    pub floor: f64,
    /// Exponent on the inverse variance.
    pub power: f64,
    /// Core size and margin of the FFT tiles, px.
    pub tile: usize,
    pub margin: usize,
    /// `Some`: the least-squares demultiplexer ([`Prior`]) instead of one
    /// band per copy.
    pub lmmse: Option<Prior>,
}

/// The spectra the least-squares demultiplexer assumes, as power against
/// frequency `f` (c/px): luminance `((f0² + f²)/(f0² + 1/16))^(−α/2)`,
/// each chrominance the same law with its own exponent and share, white
/// noise beside them, all behind a Gaussian lens.
///
/// The mosaic's spectrum at one frequency is the luminance there plus
/// every chrominance copy that falls there. The 36 frequencies that differ
/// by multiples of 1/6 are tied to one another by the tile and to nothing
/// else; with these spectra as the prior, the most probable luminance and
/// chrominance of each such set is one 36×36 solve. It is the best any
/// fixed set of filters can do for pictures with these spectra: it takes
/// every copy and both side bands of each with the weight their
/// neighbourhood allows, and it takes the chrominance of one kind out of
/// the copies of the other. It works on any 6×6 tile, a Bayer one too.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Prior {
    /// Exponent of the luminance power law.
    pub alpha: f64,
    /// Exponent of the chrominance power law.
    pub alpha_c: f64,
    /// Knee of both laws, c/px.
    pub f0: f64,
    /// Power of `C1` and of `C2` against the luminance's at one frequency.
    pub chroma: [f64; 2],
    /// White noise against the luminance's power at 0.25 c/px.
    pub noise: f64,
    /// Gaussian σ (px) of the lens in front of the sensor; 0 = none.
    pub lens: f64,
    /// A luminance (and chrominance) that varies along one direction
    /// only: the angle of that direction (radians, 0 = along x) and the
    /// exponent of `cos²` that holds the spectrum to it. The slow part of
    /// the picture, below 0.03 c/px, is left as it is without a direction.
    pub oriented: Option<(f64, f64)>,
}

impl Default for Prior {
    fn default() -> Self {
        Self {
            alpha: 3.0,
            alpha_c: 3.0,
            f0: 0.01,
            chroma: [0.05, 0.05],
            noise: 1e-4,
            lens: 0.0,
            oriented: None,
        }
    }
}

/// Frequency (c/px) from which an oriented prior holds its direction.
const ORIENTED_FROM: f64 = 0.03;

impl Prior {
    /// Luminance power and the common factor of the chrominance powers.
    fn power(&self, fu: f64, fv: f64) -> (f64, f64) {
        let r2 = fu * fu + fv * fv;
        let base = (self.f0 * self.f0 + r2) / (self.f0 * self.f0 + 0.0625);
        let lens = (-4.0 * PI * PI * self.lens * self.lens * r2).exp();
        let dir = match self.oriented {
            Some((theta, k)) if r2 > 0.0 => {
                let c = (fu * theta.cos() + fv * theta.sin()) / r2.sqrt();
                let d = (c * c).powf(k).max(1e-3);
                // The direction holds for the detail only. Below
                // `ORIENTED_FROM` the spectrum is the plain one: the level
                // of a flat area has no direction, and a prior that denies
                // the slow colour across its angle moves that level (red
                // +4.8 %, blue -3.9 % in a noisy dark patch at ISO 3200).
                let detail = r2 / (r2 + ORIENTED_FROM * ORIENTED_FROM);
                detail * d + (1.0 - detail)
            }
            _ => 1.0,
        };
        (lens * dir * base.powf(-self.alpha / 2.0), lens * dir * base.powf(-self.alpha_c / 2.0))
    }
}

impl Default for XParams {
    fn default() -> Self {
        Self {
            c1_diag: Band { along: 0.20, across: 0.09 },
            c1_axis: None,
            c2: Band { along: 0.20, across: 0.08 },
            transition: 0.08,
            sigma: 3.0,
            weight: Weighting::Quadrature,
            floor: 1e-3,
            power: 1.0,
            tile: 960,
            margin: 48,
            lmmse: None,
        }
    }
}

/// One demodulated copy on a tile: the chrominance estimate in `re`, the
/// leak alone in `im`.
#[derive(Clone, Debug)]
pub struct Copy {
    pub name: &'static str,
    /// 1 for `C1`, 2 for `C2`.
    pub kind: u8,
    /// Magnitude of the carrier's coefficient.
    pub amp: f64,
    pub re: Plane,
    pub im: Plane,
}

struct Spec {
    name: &'static str,
    kind: u8,
    k: usize,
    l: usize,
    /// Unit vector of the band's `along` direction.
    e: (f64, f64),
    band: Band,
}

fn specs(p: &XParams) -> Vec<Spec> {
    let s = std::f64::consts::FRAC_1_SQRT_2;
    let mut v = vec![
        Spec { name: "C1 (1/3, 1/3)", kind: 1, k: 2, l: 2, e: (s, s), band: p.c1_diag },
        Spec { name: "C1 (1/3,-1/3)", kind: 1, k: 2, l: 4, e: (s, -s), band: p.c1_diag },
        Spec { name: "C2 (1/2, 1/6)", kind: 2, k: 3, l: 1, e: (1.0, 0.0), band: p.c2 },
        Spec { name: "C2 (1/6, 1/2)", kind: 2, k: 1, l: 3, e: (0.0, 1.0), band: p.c2 },
    ];
    if let Some(b) = p.c1_axis {
        v.push(Spec { name: "C1 (1/3, 0)", kind: 1, k: 2, l: 0, e: (1.0, 0.0), band: b });
        v.push(Spec { name: "C1 (0, 1/3)", kind: 1, k: 0, l: 2, e: (0.0, 1.0), band: b });
    }
    v
}

/// Raised-cosine low-pass profile: 1 below `c − t/2`, 0 above `c + t/2`.
fn profile(f: f64, c: f64, t: f64) -> f64 {
    let a = f.abs();
    let (lo, hi) = (c - t / 2.0, c + t / 2.0);
    if a <= lo {
        1.0
    } else if a >= hi {
        0.0
    } else {
        0.5 * (1.0 + (PI * (a - lo) / t).cos())
    }
}

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

    /// In place; the inverse is scaled by `1/(w·h)`.
    fn run(&self, buf: &mut [C64], inverse: bool) {
        let (w, h) = (self.w, self.h);
        let (fx, fy) = if inverse { (&self.iw, &self.ih) } else { (&self.fw, &self.fh) };
        fx.process(buf);
        let mut t = vec![C64::new(0.0, 0.0); w * h];
        for y in 0..h {
            for x in 0..w {
                t[x * h + y] = buf[y * w + x];
            }
        }
        fy.process(&mut t);
        let s = if inverse { 1.0 / (w * h) as f64 } else { 1.0 };
        for y in 0..h {
            for x in 0..w {
                buf[y * w + x] = t[x * h + y] * s;
            }
        }
    }
}

/// The copies of the `tw × th` window of `m` at `(ox, oy)`; pixels outside
/// the frame are mirrored. `tw`, `th` are multiples of 6.
fn tile_copies(
    m: &Plane,
    tile: &XTile,
    p: &XParams,
    ox: isize,
    oy: isize,
    tw: usize,
    th: usize,
) -> Vec<Copy> {
    assert!(tw.is_multiple_of(6) && th.is_multiple_of(6));
    let pat = tile.shifted(ox, oy).pattern();
    let fft = Fft2::new(tw, th);
    let mut spec = vec![C64::new(0.0, 0.0); tw * th];
    for y in 0..th {
        let row = m.row(reflect(oy + y as isize, m.height));
        for x in 0..tw {
            spec[y * tw + x] = C64::new(row[reflect(ox + x as isize, m.width)], 0.0);
        }
    }
    fft.run(&mut spec, false);
    let freq =
        |i: usize, n: usize| if i < n.div_ceil(2) { i as f64 / n as f64 } else { i as f64 / n as f64 - 1.0 };
    specs(p)
        .into_iter()
        .map(|s| {
            let a = if s.kind == 1 { pat.a1[s.l][s.k] } else { pat.a2[s.l][s.k] };
            let (sx, sy) = (s.k * tw / 6, s.l * th / 6);
            let inv = a.inv();
            let mut z = vec![C64::new(0.0, 0.0); tw * th];
            for v in 0..th {
                let fv = freq(v, th);
                let src = ((v + sy) % th) * tw;
                for u in 0..tw {
                    let fu = freq(u, tw);
                    let along = fu * s.e.0 + fv * s.e.1;
                    let across = -fu * s.e.1 + fv * s.e.0;
                    let hgain = profile(along, s.band.along, p.transition)
                        * profile(across, s.band.across, p.transition);
                    if hgain > 0.0 {
                        z[v * tw + u] = spec[src + (u + sx) % tw] * inv * hgain;
                    }
                }
            }
            fft.run(&mut z, true);
            Copy {
                name: s.name,
                kind: s.kind,
                amp: a.norm(),
                re: Plane::from_vec(tw, th, z.iter().map(|c| c.re).collect()),
                im: Plane::from_vec(tw, th, z.iter().map(|c| c.im).collect()),
            }
        })
        .collect()
}

/// The copies of one chrominance put together per pixel.
pub fn combine(copies: &[Copy], kind: u8, p: &XParams) -> Plane {
    let set: Vec<&Copy> = copies.iter().filter(|c| c.kind == kind).collect();
    let (w, h) = (set[0].re.width, set[0].re.height);
    let g = gaussian_kernel(p.sigma);
    let weights: Vec<Plane> = set
        .iter()
        .map(|c| {
            let floor = (p.floor / c.amp).powi(2);
            match p.weight {
                Weighting::Fixed => Plane::from_vec(w, h, vec![c.amp * c.amp; w * h]),
                Weighting::Quadrature | Weighting::Energy => {
                    let src = if p.weight == Weighting::Quadrature { &c.im } else { &c.re };
                    let sq = Plane::from_vec(w, h, src.data.iter().map(|v| v * v).collect());
                    let mut e = lowpass(&sq, &g);
                    e.data.iter_mut().for_each(|v| *v = (v.max(0.0) + floor).powf(-p.power));
                    e
                }
            }
        })
        .collect();
    let mut out = Plane::zeros(w, h);
    out.data.par_iter_mut().enumerate().for_each(|(i, o)| {
        let (mut num, mut den) = (0.0, 0.0);
        for (c, wt) in set.iter().zip(&weights) {
            num += wt.data[i] * c.re.data[i];
            den += wt.data[i];
        }
        *o = num / den;
    });
    out
}

/// Solve `G·y = b` for a Hermitian positive definite `G` (36×36, row-major)
/// by Cholesky; `g` and `b` are overwritten, `b` with `y`.
fn solve_hermitian(g: &mut [C64], b: &mut [C64], n: usize) {
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
    for i in 0..n {
        let mut v = b[i];
        for k in 0..i {
            v -= g[i * n + k] * b[k];
        }
        b[i] = v / g[i * n + i].re;
    }
    for i in (0..n).rev() {
        let mut v = b[i];
        for k in i + 1..n {
            v -= g[k * n + i].conj() * b[k];
        }
        b[i] = v / g[i * n + i].re;
    }
}

/// The spectrum of the `tw × th` window of `m` at `(ox, oy)`, mirrored
/// outside the frame.
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

/// The carriers of `C1` and of `C2`: `(k, l, coefficient)`.
type CarrierLists = [Vec<(usize, usize, C64)>; 2];

fn carrier_lists(pat: &Pattern) -> CarrierLists {
    let nonzero = |a: &[[C64; 6]; 6]| -> Vec<(usize, usize, C64)> {
        let mut v = Vec::new();
        for (l, row) in a.iter().enumerate() {
            for (k, c) in row.iter().enumerate() {
                if c.norm() > 1e-12 {
                    v.push((k, l, *c));
                }
            }
        }
        v
    };
    [nonzero(&pat.a1), nonzero(&pat.a2)]
}

/// Signed frequency of bin `i` of `n`, c/px.
fn bin_freq(i: usize, n: usize) -> f64 {
    if 2 * i <= n {
        i as f64 / n as f64
    } else {
        i as f64 / n as f64 - 1.0
    }
}

/// The 36 tied bins of the set at `(u0, v0)`: the mosaic's spectrum there
/// in `y`, its covariance under `prior` in `g`, the chrominance powers in
/// the return.
#[allow(clippy::too_many_arguments)]
fn set_system(
    spec: &[C64],
    tw: usize,
    th: usize,
    nz: &CarrierLists,
    prior: &Prior,
    u0: usize,
    v0: usize,
    g: &mut [C64],
    y: &mut [C64; 36],
) -> [[f64; 36]; 2] {
    let (qw, qh) = (tw / 6, th / 6);
    let mut sl = [0.0; 36];
    let mut sc = [[0.0; 36]; 2];
    for b in 0..6 {
        for a in 0..6 {
            let i = b * 6 + a;
            let (u, v) = (u0 + a * qw, v0 + b * qh);
            y[i] = spec[v * tw + u];
            let (l, c) = prior.power(bin_freq(u, tw), bin_freq(v, th));
            sl[i] = l;
            sc[0][i] = c * prior.chroma[0];
            sc[1][i] = c * prior.chroma[1];
        }
    }
    g.iter_mut().for_each(|v| *v = C64::new(0.0, 0.0));
    for i in 0..36 {
        g[i * 36 + i] = C64::new(sl[i] + prior.noise, 0.0);
    }
    for (kind, list) in nz.iter().enumerate() {
        for b in 0..6 {
            for a in 0..6 {
                let s = sc[kind][b * 6 + a];
                for &(k, l, ak) in list {
                    let i = ((b + l) % 6) * 6 + (a + k) % 6;
                    for &(k2, l2, ak2) in list {
                        let j = ((b + l2) % 6) * 6 + (a + k2) % 6;
                        g[i * 36 + j] += ak * ak2.conj() * s;
                    }
                }
            }
        }
    }
    sc
}

/// What one prior gives on a tile.
struct TileEstimate {
    c1: Plane,
    c2: Plane,
    /// `m·(G⁻¹m)` per pixel: how improbable the mosaic is there under
    /// the prior, before the prior's level is set.
    energy: Plane,
    /// `log det G` per pixel of the tile.
    logdet: f64,
}

/// The least-squares demultiplexer on one tile.
fn tile_lmmse(
    m: &Plane,
    tile: &XTile,
    prior: &Prior,
    ox: isize,
    oy: isize,
    tw: usize,
    th: usize,
) -> TileEstimate {
    let pat = tile.shifted(ox, oy).pattern();
    let fft = Fft2::new(tw, th);
    let n = tw * th;
    let spec = spectrum(m, &fft, ox, oy);
    let nz = carrier_lists(&pat);
    let (qw, qh) = (tw / 6, th / 6);
    // Per row of sets: the two chrominance spectra and G⁻¹m at the set's
    // 36 bins, and the row's log det.
    let rows: Vec<(Vec<[C64; 3]>, f64)> = (0..qh)
        .into_par_iter()
        .map(|v0| {
            let mut out = vec![[C64::new(0.0, 0.0); 3]; qw * 36];
            let mut g = vec![C64::new(0.0, 0.0); 36 * 36];
            let mut logdet = 0.0;
            for u0 in 0..qw {
                let mut y = [C64::new(0.0, 0.0); 36];
                let sc = set_system(&spec, tw, th, &nz, prior, u0, v0, &mut g, &mut y);
                solve_hermitian(&mut g, &mut y, 36);
                logdet += (0..36).map(|i| 2.0 * g[i * 36 + i].re.ln()).sum::<f64>();
                for (kind, list) in nz.iter().enumerate() {
                    for b in 0..6 {
                        for a in 0..6 {
                            let mut c = C64::new(0.0, 0.0);
                            for &(k, l, ak) in list {
                                c += ak.conj() * y[((b + l) % 6) * 6 + (a + k) % 6];
                            }
                            out[u0 * 36 + b * 6 + a][kind] = c * sc[kind][b * 6 + a];
                        }
                    }
                }
                for (i, v) in y.iter().enumerate() {
                    out[u0 * 36 + i][2] = *v;
                }
            }
            (out, logdet)
        })
        .collect();
    let mut planes = Vec::new();
    for kind in 0..3 {
        let mut z = vec![C64::new(0.0, 0.0); n];
        for (v0, (row, _)) in rows.iter().enumerate() {
            for u0 in 0..qw {
                for b in 0..6 {
                    for a in 0..6 {
                        z[(v0 + b * qh) * tw + u0 + a * qw] = row[u0 * 36 + b * 6 + a][kind];
                    }
                }
            }
        }
        fft.run(&mut z, true);
        planes.push(Plane::from_vec(tw, th, z.iter().map(|c| c.re).collect()));
    }
    let mut energy = planes.pop().expect("three planes");
    for y in 0..th {
        let row = m.row(reflect(oy + y as isize, m.height));
        for (x, v) in energy.row_mut(y).iter_mut().enumerate() {
            *v *= row[reflect(ox + x as isize, m.width)];
        }
    }
    let c2 = planes.pop().expect("three planes");
    let c1 = planes.pop().expect("three planes");
    TileEstimate { c1, c2, energy, logdet: rows.iter().map(|r| r.1).sum::<f64>() / n as f64 }
}

/// One window of the frame for [`fit_prior`]: its spectrum and carriers.
struct Sample {
    spec: Vec<C64>,
    tw: usize,
    th: usize,
    nz: CarrierLists,
}

/// How probable the samples are under `prior` (Whittle's likelihood of
/// the mosaic's spectrum, the overall level left free): larger is better.
/// Every `stride`-th set in each direction is counted.
fn evidence(samples: &[Sample], prior: &Prior, stride: usize) -> f64 {
    let mut total = (0.0, 0.0, 0.0);
    for s in samples {
        let (qw, qh) = (s.tw / 6, s.th / 6);
        let part = (0..qh)
            .into_par_iter()
            .step_by(stride)
            .map(|v0| {
                let mut g = vec![C64::new(0.0, 0.0); 36 * 36];
                let (mut q, mut d, mut n) = (0.0, 0.0, 0.0);
                for u0 in (0..qw).step_by(stride) {
                    if u0 == 0 && v0 == 0 {
                        continue;
                    }
                    let mut y = [C64::new(0.0, 0.0); 36];
                    set_system(&s.spec, s.tw, s.th, &s.nz, prior, u0, v0, &mut g, &mut y);
                    // Cholesky in place; then |L⁻¹y|² and log det.
                    let nn = 36;
                    for j in 0..nn {
                        let mut dd = g[j * nn + j].re;
                        for k in 0..j {
                            dd -= g[j * nn + k].norm_sqr();
                        }
                        let dd = dd.max(1e-300).sqrt();
                        g[j * nn + j] = C64::new(dd, 0.0);
                        for i in j + 1..nn {
                            let mut v = g[i * nn + j];
                            for k in 0..j {
                                v -= g[i * nn + k] * g[j * nn + k].conj();
                            }
                            g[i * nn + j] = v / dd;
                        }
                        d += 2.0 * dd.ln();
                    }
                    for i in 0..nn {
                        let mut v = y[i];
                        for k in 0..i {
                            v -= g[i * nn + k] * y[k];
                        }
                        y[i] = v / g[i * nn + i].re;
                        q += y[i].norm_sqr();
                    }
                    n += nn as f64;
                }
                (q, d, n)
            })
            .reduce(|| (0.0, 0.0, 0.0), |a, b| (a.0 + b.0, a.1 + b.1, a.2 + b.2));
        total = (total.0 + part.0, total.1 + part.1, total.2 + part.2);
    }
    let (q, d, n) = total;
    -(n * (q / n).ln() + d) / n
}

/// The prior under which the mosaic itself is most probable, searched
/// from `start` (Nelder–Mead over the two exponents, the two chrominance
/// shares, the noise and the lens). Up to nine windows of the frame are
/// looked at, every `stride`-th set of each.
pub fn fit_prior(
    m: &Plane,
    tile: &XTile,
    p: &XParams,
    start: Prior,
    stride: usize,
    free: [bool; 6],
) -> Prior {
    let (w, h) = (m.width, m.height);
    let (sx, sy) = (p.tile.min(up6(w)), p.tile.min(up6(h)));
    let (tw, th) = (up6(sx + 2 * p.margin), up6(sy + 2 * p.margin));
    let mg = p.margin as isize;
    let pick = |n: usize, s: usize| -> Vec<usize> {
        let all: Vec<usize> = (0..n).step_by(s).collect();
        match all.len() {
            0..=3 => all,
            k => vec![all[0], all[k / 2], all[k - 1]],
        }
    };
    let fft = Fft2::new(tw, th);
    let samples: Vec<Sample> = pick(h, sy)
        .into_iter()
        .flat_map(|y0| pick(w, sx).into_iter().map(move |x0| (x0, y0)))
        .map(|(x0, y0)| {
            let (ox, oy) = (x0 as isize - mg, y0 as isize - mg);
            Sample {
                spec: spectrum(m, &fft, ox, oy),
                tw,
                th,
                nz: carrier_lists(&tile.shifted(ox, oy).pattern()),
            }
        })
        .collect();
    let x0 = [
        start.alpha,
        start.alpha_c,
        start.chroma[0].ln(),
        start.chroma[1].ln(),
        start.noise.ln(),
        start.lens,
    ];
    // The search runs over the free ones only; `z` holds them in order.
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
    let cost = |z: &[f64]| -evidence(&samples, &to_prior(z), stride);
    let steps = [0.6, 0.6, 1.0, 1.0, 2.0, 0.35];
    let z0: Vec<f64> = dims.iter().map(|i| x0[*i]).collect();
    let mut simplex: Vec<(Vec<f64>, f64)> = vec![(z0.clone(), cost(&z0))];
    for (k, i) in dims.iter().enumerate() {
        let mut z = z0.clone();
        z[k] += steps[*i];
        let c = cost(&z);
        simplex.push((z, c));
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
            let c = cost(&z);
            (z, c)
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
    to_prior(&simplex[0].0)
}

/// Luminance and the two chrominances of an X-Trans mosaic.
#[derive(Clone, Debug)]
pub struct XSeparation {
    /// `g·G + (1 − g)·(R + B)/2`, full resolution.
    pub lum: Plane,
    /// `G − (R + B)/2`, band-limited.
    pub c1: Plane,
    /// `(R − B)/2`, band-limited.
    pub c2: Plane,
    /// Share of green photosites (5/9).
    pub g: f64,
}

impl XSeparation {
    /// The three channels: the luminance plus the band-limited colour.
    pub fn rgb(&self) -> Rgb {
        let n = self.lum.data.len();
        let (w, h) = (self.lum.width, self.lum.height);
        let ch = |k1: f64, k2: f64| {
            Plane::from_vec(
                w,
                h,
                (0..n)
                    .into_par_iter()
                    .map(|i| self.lum.data[i] + k1 * self.c1.data[i] + k2 * self.c2.data[i])
                    .collect(),
            )
        };
        Rgb { r: ch(-self.g, 1.0), g: ch(1.0 - self.g, 0.0), b: ch(-self.g, -1.0) }
    }

    /// `w_R·R + w_G·G + w_B·B` of those channels (weights summing to one).
    pub fn mix(&self, w: [f64; 3]) -> Plane {
        let k1 = -self.g * (w[0] + w[2]) + (1.0 - self.g) * w[1];
        let k2 = w[0] - w[2];
        let n = self.lum.data.len();
        Plane::from_vec(
            self.lum.width,
            self.lum.height,
            (0..n)
                .into_par_iter()
                .map(|i| self.lum.data[i] + k1 * self.c1.data[i] + k2 * self.c2.data[i])
                .collect(),
        )
    }
}

fn up6(n: usize) -> usize {
    n.div_ceil(6) * 6
}

/// The copies over the whole frame as one tile (for small pictures and
/// for looking at each copy).
pub fn copies_whole(m: &Plane, tile: &XTile, p: &XParams) -> Vec<Copy> {
    let mg = p.margin as isize;
    let (tw, th) = (up6(m.width + 2 * p.margin), up6(m.height + 2 * p.margin));
    let crop = |pl: &Plane| {
        let mut o = Plane::zeros(m.width, m.height);
        for y in 0..m.height {
            o.row_mut(y).copy_from_slice(&pl.row(y + p.margin)[p.margin..p.margin + m.width]);
        }
        o
    };
    tile_copies(m, tile, p, -mg, -mg, tw, th)
        .into_iter()
        .map(|c| Copy { re: crop(&c.re), im: crop(&c.im), ..c })
        .collect()
}

/// `L̂ = m − Ĉ1·pg − Ĉ2·pd`.
pub fn luminance(m: &Plane, tile: &XTile, c1: &Plane, c2: &Plane) -> Plane {
    let pat = tile.pattern();
    let w = m.width;
    let mut lum = Plane::zeros(w, m.height);
    lum.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (mr, r1, r2) = (m.row(y), c1.row(y), c2.row(y));
        let (pg, pd) = (&pat.pg[y % 6], &pat.pd[y % 6]);
        for x in 0..w {
            row[x] = mr[x] - r1[x] * pg[x % 6] - r2[x] * pd[x % 6];
        }
    });
    lum
}

/// The separation. `tile` is the colour tile at the frame's origin. The
/// frame's edges are mirrored; where they are not symmetry lines of the
/// tile ([`XTile::mirrors`]) the outer `margin` pixels are not to be
/// trusted.
pub fn separate_xtrans(m: &Plane, tile: &XTile, p: &XParams) -> XSeparation {
    let (c1, c2) = chroma(m, tile, p);
    from_chroma(m, tile, c1, c2)
}

/// The separation that goes with a chrominance estimated elsewhere.
pub fn from_chroma(m: &Plane, tile: &XTile, c1: Plane, c2: Plane) -> XSeparation {
    let lum = luminance(m, tile, &c1, &c2);
    XSeparation { lum, c1, c2, g: tile.pattern().g }
}

/// The two chrominances alone, tile by tile.
pub fn chroma(m: &Plane, tile: &XTile, p: &XParams) -> (Plane, Plane) {
    let (w, h) = (m.width, m.height);
    let (sx, sy) = (p.tile.min(up6(w)), p.tile.min(up6(h)));
    let (tw, th) = (up6(sx + 2 * p.margin), up6(sy + 2 * p.margin));
    let mg = p.margin;
    let origins: Vec<(usize, usize)> =
        (0..h).step_by(sy).flat_map(|y| (0..w).step_by(sx).map(move |x| (x, y))).collect();
    let parts: Vec<(usize, usize, Plane, Plane)> = origins
        .par_iter()
        .map(|&(x0, y0)| {
            let (ox, oy) = (x0 as isize - mg as isize, y0 as isize - mg as isize);
            if let Some(prior) = &p.lmmse {
                let t = tile_lmmse(m, tile, prior, ox, oy, tw, th);
                return (x0, y0, t.c1, t.c2);
            }
            let copies = tile_copies(m, tile, p, ox, oy, tw, th);
            (x0, y0, combine(&copies, 1, p), combine(&copies, 2, p))
        })
        .collect();
    let mut c1 = Plane::zeros(w, h);
    let mut c2 = Plane::zeros(w, h);
    for (x0, y0, t1, t2) in &parts {
        let (nx, ny) = (sx.min(w - x0), sy.min(h - y0));
        for y in 0..ny {
            c1.row_mut(y0 + y)[*x0..x0 + nx].copy_from_slice(&t1.row(y + mg)[mg..mg + nx]);
            c2.row_mut(y0 + y)[*x0..x0 + nx].copy_from_slice(&t2.row(y + mg)[mg..mg + nx]);
        }
    }
    (c1, c2)
}

/// What one prior makes of the whole frame, and how well it explains it.
#[derive(Clone, Debug)]
pub struct Hypothesis {
    pub c1: Plane,
    pub c2: Plane,
    /// `m·(G⁻¹m)` per pixel.
    pub energy: Plane,
    /// `log det G` per pixel.
    pub logdet: f64,
}

/// The least-squares estimate under `p.lmmse` with what [`select`] needs.
pub fn hypothesis(m: &Plane, tile: &XTile, p: &XParams) -> Hypothesis {
    let prior = p.lmmse.expect("hypothesis: no prior");
    let (w, h) = (m.width, m.height);
    let (sx, sy) = (p.tile.min(up6(w)), p.tile.min(up6(h)));
    let (tw, th) = (up6(sx + 2 * p.margin), up6(sy + 2 * p.margin));
    let mg = p.margin;
    let origins: Vec<(usize, usize)> =
        (0..h).step_by(sy).flat_map(|y| (0..w).step_by(sx).map(move |x| (x, y))).collect();
    let parts: Vec<(usize, usize, TileEstimate)> = origins
        .par_iter()
        .map(|&(x0, y0)| {
            (
                x0,
                y0,
                tile_lmmse(m, tile, &prior, x0 as isize - mg as isize, y0 as isize - mg as isize, tw, th),
            )
        })
        .collect();
    let mut out = Hypothesis {
        c1: Plane::zeros(w, h),
        c2: Plane::zeros(w, h),
        energy: Plane::zeros(w, h),
        logdet: 0.0,
    };
    for (x0, y0, t) in &parts {
        let (nx, ny) = (sx.min(w - x0), sy.min(h - y0));
        for y in 0..ny {
            out.c1.row_mut(y0 + y)[*x0..x0 + nx].copy_from_slice(&t.c1.row(y + mg)[mg..mg + nx]);
            out.c2.row_mut(y0 + y)[*x0..x0 + nx].copy_from_slice(&t.c2.row(y + mg)[mg..mg + nx]);
            out.energy.row_mut(y0 + y)[*x0..x0 + nx].copy_from_slice(&t.energy.row(y + mg)[mg..mg + nx]);
        }
        out.logdet = t.logdet;
    }
    out
}

/// How [`select`] weights the hypotheses.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Selection {
    /// Gaussian σ (px) of the window the evidence is gathered in.
    pub sigma: f64,
    /// How many pixels' worth of evidence the weight is given.
    pub sharp: f64,
    /// Head start of the first hypothesis, in the units of the score.
    pub bias: f64,
}

impl Default for Selection {
    fn default() -> Self {
        Self { sigma: 6.0, sharp: 20.0, bias: 0.0 }
    }
}

/// `(c1, c2, energy, logdet)` of each hypothesis on one window, weighted
/// per pixel by how probable the mosaic around the pixel is under each:
/// the local mean of `m·G⁻¹m` is the level the prior would need there,
/// and `log level + log det G` is what that costs.
fn select_planes(parts: &[(&Plane, &Plane, &Plane, f64)], sel: &Selection) -> (Plane, Plane) {
    let (w, h) = (parts[0].0.width, parts[0].0.height);
    let g = gaussian_kernel(sel.sigma);
    let scores: Vec<Plane> = parts
        .iter()
        .enumerate()
        .map(|(i, part)| {
            let mut e = lowpass(part.2, &g);
            let head = if i == 0 { sel.bias } else { 0.0 };
            e.data.par_iter_mut().for_each(|v| *v = v.max(1e-30).ln() + part.3 - head);
            e
        })
        .collect();
    let mut c1 = Plane::zeros(w, h);
    let mut c2 = Plane::zeros(w, h);
    c1.data.par_iter_mut().zip(c2.data.par_iter_mut()).enumerate().for_each(|(i, (o1, o2))| {
        let best = scores.iter().map(|s| s.data[i]).fold(f64::INFINITY, f64::min);
        let (mut a, mut b, mut t) = (0.0, 0.0, 0.0);
        for (part, s) in parts.iter().zip(&scores) {
            let wt = (-0.5 * sel.sharp * (s.data[i] - best)).exp();
            a += wt * part.0.data[i];
            b += wt * part.1.data[i];
            t += wt;
        }
        *o1 = a / t;
        *o2 = b / t;
    });
    (c1, c2)
}

/// [`select_planes`] on whole-frame hypotheses.
pub fn select(hyps: &[Hypothesis], sel: &Selection) -> (Plane, Plane) {
    let parts: Vec<(&Plane, &Plane, &Plane, f64)> =
        hyps.iter().map(|h| (&h.c1, &h.c2, &h.energy, h.logdet)).collect();
    select_planes(&parts, sel)
}

/// The separation under several priors at once, chosen between per pixel
/// by the local evidence, tile by tile (one tile's hypotheses in memory
/// at a time). One prior alone is [`separate_xtrans`] with it.
pub fn separate_bank(m: &Plane, tile: &XTile, p: &XParams, priors: &[Prior], sel: &Selection) -> XSeparation {
    let (w, h) = (m.width, m.height);
    let (sx, sy) = (p.tile.min(up6(w)), p.tile.min(up6(h)));
    let (tw, th) = (up6(sx + 2 * p.margin), up6(sy + 2 * p.margin));
    let mg = p.margin;
    let origins: Vec<(usize, usize)> =
        (0..h).step_by(sy).flat_map(|y| (0..w).step_by(sx).map(move |x| (x, y))).collect();
    let g = gaussian_kernel(sel.sigma);
    let mut c1 = Plane::zeros(w, h);
    let mut c2 = Plane::zeros(w, h);
    // Memory: one tile's estimate under one prior at a time, beside the
    // tile's running sums, and no more than four tiles at once (each
    // estimate is spread over the threads within itself).
    for group in origins.chunks(4) {
        let parts: Vec<(usize, usize, Plane, Plane)> = group
            .par_iter()
            .map(|&(x0, y0)| {
                let (ox, oy) = (x0 as isize - mg as isize, y0 as isize - mg as isize);
                if priors.len() == 1 {
                    let t = tile_lmmse(m, tile, &priors[0], ox, oy, tw, th);
                    return (x0, y0, t.c1, t.c2);
                }
                // The weights `exp(−½·sharp·(score − best))` as the priors
                // come: the sums are held against the best score so far
                // and scaled down when a better one arrives.
                let n = tw * th;
                let (mut a1, mut a2) = (vec![0.0; n], vec![0.0; n]);
                let (mut total, mut best) = (vec![0.0; n], vec![f64::INFINITY; n]);
                for (k, q) in priors.iter().enumerate() {
                    let t = tile_lmmse(m, tile, q, ox, oy, tw, th);
                    let mut score = lowpass(&t.energy, &g);
                    let head = if k == 0 { sel.bias } else { 0.0 };
                    score.data.iter_mut().for_each(|v| *v = v.max(1e-30).ln() + t.logdet - head);
                    for i in 0..n {
                        let s = score.data[i];
                        if s < best[i] {
                            let down = if best[i].is_finite() {
                                (-0.5 * sel.sharp * (best[i] - s)).exp()
                            } else {
                                0.0
                            };
                            a1[i] *= down;
                            a2[i] *= down;
                            total[i] *= down;
                            best[i] = s;
                        }
                        let wt = (-0.5 * sel.sharp * (s - best[i])).exp();
                        a1[i] += wt * t.c1.data[i];
                        a2[i] += wt * t.c2.data[i];
                        total[i] += wt;
                    }
                }
                let out =
                    |a: Vec<f64>| Plane::from_vec(tw, th, a.iter().zip(&total).map(|(v, t)| v / t).collect());
                (x0, y0, out(a1), out(a2))
            })
            .collect();
        for (x0, y0, t1, t2) in &parts {
            let (nx, ny) = (sx.min(w - x0), sy.min(h - y0));
            for y in 0..ny {
                c1.row_mut(y0 + y)[*x0..x0 + nx].copy_from_slice(&t1.row(y + mg)[mg..mg + nx]);
                c2.row_mut(y0 + y)[*x0..x0 + nx].copy_from_slice(&t2.row(y + mg)[mg..mg + nx]);
            }
        }
    }
    from_chroma(m, tile, c1, c2)
}

/// The second step: colour that follows one line.
///
/// The first step holds the colour to a narrow band, and where the colour
/// itself has a hard edge the rest of it stays in the luminance as the
/// tile's pattern. Around such an edge the picture has two colours, and
/// every pixel lies on the line between them: `X_k = μ_k + d_k·t`, one
/// scalar `t` for the three channels. The line (`μ`, `d`) is the same in
/// the picture and in any blur of it, so the first step's estimate,
/// blurred until the pattern is gone, gives it. On the line every
/// photosite measures the same `t`, each through its own `d_k`: the
/// mosaic there is a grey picture with three gains, and the colour has the
/// mosaic's own resolution.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Refine {
    /// Gaussian σ (px) of the blur that takes the pattern out of the first
    /// step's channels.
    pub pre: f64,
    /// Gaussian σ (px) of the window the line is fitted in.
    pub window: f64,
    /// Weight of the first step's own `t` (its channels laid on the line)
    /// beside the photosite's, whose weight is `d_k²` of a unit `d`: what
    /// decides where the photosite says nothing, its `d_k` being zero.
    /// (Taking `t` from the photosites around instead blurs it where they
    /// are few, red against black, and was worse.)
    pub hold: f64,
    /// Standard deviation along the line below which there is no line to
    /// speak of, in units of the mosaic.
    pub floor: f64,
    /// Exponent on how much of the variance lies on the line.
    pub power: f64,
    /// How often the step is repeated on its own result.
    pub rounds: usize,
    /// The test on the result: a luminance knows nothing of the tile, so
    /// the step is kept where it leaves the luminance smoother than it
    /// found it and dropped where it roughens it. The weight is
    /// `1/(1 + (after/before)^check)` of the local roughness; 0 = no test.
    pub check: f64,
    /// Gaussian σ (px) of the window the roughness is taken in.
    pub check_window: f64,
}

/// What the luminance has between neighbours, squared, in a window of
/// Gaussian σ `sigma`: the tile's pattern shows here before anything else.
fn roughness(lum: &Plane, sigma: f64) -> Plane {
    let (w, h) = (lum.width, lum.height);
    let mut e = Plane::zeros(w, h);
    e.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (up, mid, down) =
            (lum.row(reflect(y as isize - 1, h)), lum.row(y), lum.row(reflect(y as isize + 1, h)));
        for x in 0..w {
            let (l, r) = (reflect(x as isize - 1, w), reflect(x as isize + 1, w));
            let v = mid[x] - (mid[l] + mid[r] + up[x] + down[x]) / 4.0;
            row[x] = v * v;
        }
    });
    lowpass(&e, &gaussian_kernel(sigma))
}

impl Default for Refine {
    fn default() -> Self {
        Self {
            pre: 1.0,
            window: 2.0,
            hold: 0.15,
            floor: 0.001,
            power: 0.5,
            rounds: 1,
            check: 4.0,
            check_window: 1.0,
        }
    }
}

/// [`Refine`] on a separation of `m`.
pub fn refine(m: &Plane, tile: &XTile, sep: &XSeparation, r: &Refine) -> XSeparation {
    let (w, h) = (m.width, m.height);
    let n = w * h;
    let g = sep.g;
    let mut out = sep.clone();
    for _ in 0..r.rounds {
        let rgb = out.rgb();
        let pre = |p: &Plane| lowpass(p, &gaussian_kernel(r.pre));
        let b = [pre(&rgb.r), pre(&rgb.g), pre(&rgb.b)];
        let win = gaussian_kernel(r.window);
        let mean: Vec<Plane> = b.iter().map(|p| lowpass(p, &win)).collect();
        // Second moments in the window: rr, gg, bb, rg, rb, gb.
        let pairs = [(0, 0), (1, 1), (2, 2), (0, 1), (0, 2), (1, 2)];
        let second: Vec<Plane> = pairs
            .par_iter()
            .map(|&(i, j)| {
                let prod =
                    Plane::from_vec(w, h, b[i].data.iter().zip(&b[j].data).map(|(x, y)| x * y).collect());
                lowpass(&prod, &win)
            })
            .collect();
        let (mut c1, mut c2) = (Plane::zeros(w, h), Plane::zeros(w, h));
        c1.data.par_chunks_mut(w).zip(c2.data.par_chunks_mut(w)).enumerate().for_each(|(y, (row1, row2))| {
            for x in 0..w {
                let i = y * w + x;
                let mu = [mean[0].data[i], mean[1].data[i], mean[2].data[i]];
                let cov = |k: usize, a: usize, b: usize| second[k].data[i] - mu[a] * mu[b];
                let s = [
                    [cov(0, 0, 0), cov(3, 0, 1), cov(4, 0, 2)],
                    [cov(3, 0, 1), cov(1, 1, 1), cov(5, 1, 2)],
                    [cov(4, 0, 2), cov(5, 1, 2), cov(2, 2, 2)],
                ];
                let trace = s[0][0] + s[1][1] + s[2][2];
                let (old1, old2) = (out.c1.data[i], out.c2.data[i]);
                if trace <= 1e-18 {
                    row1[x] = old1;
                    row2[x] = old2;
                    continue;
                }
                // The line's direction: the largest eigenvector, by power iteration.
                let start = (0..3).max_by(|&a, &b| s[a][a].total_cmp(&s[b][b])).expect("three");
                let mut d = s[start];
                let mut lambda = 0.0;
                for _ in 0..12 {
                    let norm = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-300);
                    d = [d[0] / norm, d[1] / norm, d[2] / norm];
                    let next = [
                        s[0][0] * d[0] + s[0][1] * d[1] + s[0][2] * d[2],
                        s[1][0] * d[0] + s[1][1] * d[1] + s[1][2] * d[2],
                        s[2][0] * d[0] + s[2][1] * d[1] + s[2][2] * d[2],
                    ];
                    lambda = next[0] * d[0] + next[1] * d[1] + next[2] * d[2];
                    d = next;
                }
                let norm = (d[0] * d[0] + d[1] * d[1] + d[2] * d[2]).sqrt().max(1e-300);
                let d = [d[0] / norm, d[1] / norm, d[2] / norm];
                // How much of the picture lies on the line, how much of its
                // colour does, and whether the colour on the line is more
                // than noise.
                let on_line = ((3.0 * lambda / trace - 1.0) / 2.0).clamp(0.0, 1.0);
                let dc = [d[1] - (d[0] + d[2]) / 2.0, (d[0] - d[2]) / 2.0];
                let line_colour = lambda * (dc[0] * dc[0] + dc[1] * dc[1]);
                let colour = s[1][1] + (s[0][0] + s[2][2] + 2.0 * s[0][2]) / 4.0 - (s[0][1] + s[1][2])
                    + (s[0][0] + s[2][2] - 2.0 * s[0][2]) / 4.0;
                let colour_on_line = (line_colour / colour.max(1e-30)).clamp(0.0, 1.0);
                let trust = (on_line * colour_on_line).powf(r.power) * line_colour
                    / (line_colour + r.floor * r.floor);
                // t at this pixel: its photosite, and the first step where
                // the photosite does not see the line.
                let first = [rgb.r.data[i], rgb.g.data[i], rgb.b.data[i]];
                let t_first: f64 = (0..3).map(|k| d[k] * (first[k] - mu[k])).sum();
                let c = tile.color_at(x as isize, y as isize) as usize;
                let num = r.hold * t_first + d[c] * (m.data[i] - mu[c]);
                let den = r.hold + d[c] * d[c] + 1e-9;
                let t = num / den;
                let x_new = [mu[0] + d[0] * t, mu[1] + d[1] * t, mu[2] + d[2] * t];
                let new1 = x_new[1] - (x_new[0] + x_new[2]) / 2.0;
                let new2 = (x_new[0] - x_new[2]) / 2.0;
                row1[x] = old1 + trust * (new1 - old1);
                row2[x] = old2 + trust * (new2 - old2);
            }
        });
        debug_assert_eq!(c1.data.len(), n);
        let mut next = from_chroma(m, tile, c1, c2);
        if r.check > 0.0 {
            let (before, after) = (roughness(&out.lum, r.check_window), roughness(&next.lum, r.check_window));
            let keep: Vec<f64> = before
                .data
                .par_iter()
                .zip(after.data.par_iter())
                .map(|(b, a)| 1.0 / (1.0 + (a / b.max(1e-30)).powf(r.check)))
                .collect();
            let blend = |new: &Plane, old: &Plane| {
                Plane::from_vec(
                    w,
                    h,
                    (0..n).map(|i| old.data[i] + keep[i] * (new.data[i] - old.data[i])).collect(),
                )
            };
            next = from_chroma(m, tile, blend(&next.c1, &out.c1), blend(&next.c2, &out.c2));
        }
        out = next;
        out.g = g;
    }
    out
}

/// Sample a picture through the tile: one channel per pixel.
pub fn mosaic_of(rgb: &Rgb, tile: &XTile) -> Plane {
    let (w, h) = (rgb.width(), rgb.height());
    let mut m = Plane::zeros(w, h);
    m.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            *v = match tile.color_at(x as isize, y as isize) {
                Color::R => rgb.r.at(x, y),
                Color::G => rgb.g.at(x, y),
                Color::B => rgb.b.at(x, y),
            };
        }
    });
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fuji() -> XTile {
        XTile::from_name(FUJI).unwrap()
    }

    /// The carriers the module's header names, with their amplitudes, and
    /// no others; the model `L + C1·pg + C2·pd` gives back every photosite.
    #[test]
    fn the_tile_has_the_carriers_the_header_names() {
        let t = fuji();
        let p = t.pattern();
        assert!((p.g - 5.0 / 9.0).abs() < 1e-15);
        for l in 0..6 {
            for k in 0..6 {
                let (a1, a2) = (p.a1[l][k].norm(), p.a2[l][k].norm());
                let third = |i: usize| i == 2 || i == 4;
                let want1 = match (third(k), third(l), k, l) {
                    (true, true, ..) => 2.0 / 9.0,
                    (true, false, _, 0) | (false, true, 0, _) => 1.0 / 9.0,
                    _ => 0.0,
                };
                let sixth = |i: usize| i == 1 || i == 5;
                let want2 = if (k == 3 && sixth(l)) || (l == 3 && sixth(k)) { 1.0 / 3.0 } else { 0.0 };
                assert!((a1 - want1).abs() < 1e-12, "a1 at ({k},{l}): {a1}");
                assert!((a2 - want2).abs() < 1e-12, "a2 at ({k},{l}): {a2}");
            }
        }
        let (r, g, b) = (0.7, 0.4, 0.2);
        let (l, c1, c2) = (p.g * g + (1.0 - p.g) * (r + b) / 2.0, g - (r + b) / 2.0, (r - b) / 2.0);
        for y in 0..6 {
            for x in 0..6 {
                let want = match t.0[y][x] {
                    Color::R => r,
                    Color::G => g,
                    Color::B => b,
                };
                assert!((l + c1 * p.pg[y][x] + c2 * p.pd[y][x] - want).abs() < 1e-15);
            }
        }
    }

    /// A flat colour separates exactly, whatever the weighting, and its
    /// quadrature is empty.
    #[test]
    fn a_flat_colour_is_exact() {
        let t = fuji().shifted(2, 2);
        let (w, h) = (154, 121);
        assert!(t.mirrors(w, h));
        let (r, g, b) = (0.6, 0.3, 0.1);
        let mut rgb = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                rgb.set(x, y, r, g, b);
            }
        }
        let m = mosaic_of(&rgb, &t);
        for weight in [Weighting::Fixed, Weighting::Quadrature, Weighting::Energy] {
            let p =
                XParams { weight, c1_axis: Some(Band { along: 0.08, across: 0.12 }), ..Default::default() };
            let s = separate_xtrans(&m, &t, &p);
            let out = s.rgb();
            for y in 0..h {
                for x in 0..w {
                    assert!((out.r.at(x, y) - r).abs() < 1e-12, "{weight:?} {}", out.r.at(x, y));
                    assert!((out.g.at(x, y) - g).abs() < 1e-12, "{weight:?}");
                    assert!((out.b.at(x, y) - b).abs() < 1e-12, "{weight:?}");
                }
            }
            for c in copies_whole(&m, &t, &p) {
                assert!(c.im.data.iter().all(|v| v.abs() < 1e-12), "{}", c.name);
            }
        }
    }

    /// Tiles give what one tile over the whole frame gives, up to what the
    /// filters' tails carry across the margin.
    #[test]
    fn tiles_agree_with_the_whole_frame() {
        let t = fuji().shifted(2, 2);
        let (w, h) = (304, 211);
        let mut rgb = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f64 * 0.05, y as f64 * 0.04);
                rgb.set(x, y, 0.5 + 0.3 * fx.sin(), 0.5 + 0.3 * (fx + fy).cos(), 0.4 + 0.2 * fy.sin());
            }
        }
        let m = mosaic_of(&rgb, &t);
        let whole = separate_xtrans(&m, &t, &XParams { tile: 6000, ..Default::default() });
        let tiled = separate_xtrans(&m, &t, &XParams { tile: 96, ..Default::default() });
        let worst =
            whole.lum.data.iter().zip(&tiled.lum.data).map(|(a, b)| (a - b).abs()).fold(0.0f64, f64::max);
        assert!(worst < 2e-3, "{worst}");
    }

    /// The least-squares demultiplexer returns a smooth, strongly coloured
    /// picture to a part in a thousand, on the X-Trans tile and on a Bayer
    /// one, and what it gives adds up to the mosaic exactly. The frame's
    /// edge is its weak place: the mirrored continuation has a kink there,
    /// and the outermost pixels are off by up to 1.2 % (measured; the
    /// bounds below are those figures with room, not a requirement).
    #[test]
    fn least_squares_returns_a_smooth_picture() {
        let (w, h) = (304, 211);
        let mut rgb = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f64 * 0.02, y as f64 * 0.015);
                rgb.set(x, y, 0.5 + 0.3 * fx.sin(), 0.5 + 0.3 * (fx + fy).cos(), 0.4 + 0.2 * fy.sin());
            }
        }
        let bayer = XTile::from_name("RGRGRGGBGBGBRGRGRGGBGBGBRGRGRGGBGBGB").unwrap();
        for t in [fuji().shifted(2, 2), bayer] {
            assert!(t.mirrors(w, h));
            let m = mosaic_of(&rgb, &t);
            let p = XParams { lmmse: Some(Prior::default()), ..Default::default() };
            let out = separate_xtrans(&m, &t, &p).rgb();
            let (mut edge, mut inner) = (0.0f64, 0.0f64);
            for y in 0..h {
                for x in 0..w {
                    let e = (out.r.at(x, y) - rgb.r.at(x, y))
                        .abs()
                        .max((out.g.at(x, y) - rgb.g.at(x, y)).abs())
                        .max((out.b.at(x, y) - rgb.b.at(x, y)).abs());
                    if x >= 8 && y >= 8 && x < w - 8 && y < h - 8 {
                        inner = inner.max(e);
                    } else {
                        edge = edge.max(e);
                    }
                    // The photosite's own channel is the photosite.
                    let own = match t.color_at(x as isize, y as isize) {
                        Color::R => out.r.at(x, y),
                        Color::G => out.g.at(x, y),
                        Color::B => out.b.at(x, y),
                    };
                    assert!((own - m.at(x, y)).abs() < 1e-12);
                }
            }
            assert!(inner < 1e-3, "{inner}");
            assert!(edge < 2e-2, "{edge}");
        }
    }

    /// The search finds the lens the picture was taken through and the
    /// share of its colour, from the mosaic alone.
    #[test]
    fn the_prior_is_found_on_the_mosaic() {
        let (w, h) = (304, 211);
        let mut rng = crate::synth::Rng::new(7);
        let mut rgb = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let l = 0.5 + 0.1 * rng.gaussian();
                rgb.set(x, y, l + 0.02 * rng.gaussian(), l, l + 0.02 * rng.gaussian());
            }
        }
        let lens = 1.0;
        let blur = |p: &Plane| crate::usm::gaussian_blur(p, lens);
        let rgb = Rgb { r: blur(&rgb.r), g: blur(&rgb.g), b: blur(&rgb.b) };
        let t = fuji().shifted(2, 2);
        let m = mosaic_of(&rgb, &t);
        let found =
            fit_prior(&m, &t, &XParams::default(), Prior { lens: 0.3, ..Prior::default() }, 1, [true; 6]);
        assert!((found.lens - lens).abs() < 0.15, "{found:?}");
        assert!(found.chroma[0] < 0.2 && found.chroma[1] < 0.2, "{found:?}");
    }

    /// Several priors weighed per pixel, differing only in the colour's
    /// share: a picture without colour comes out without colour, but for
    /// the frame's edge.
    #[test]
    fn a_grey_picture_stays_grey_under_a_bank() {
        let (w, h) = (154, 121);
        let mut rgb = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = 0.5 + 0.2 * (x as f64 * 0.11).sin() * (y as f64 * 0.07).cos();
                rgb.set(x, y, v, v, v);
            }
        }
        let t = fuji().shifted(2, 2);
        let m = mosaic_of(&rgb, &t);
        let base = Prior::default();
        let priors: Vec<Prior> = [1.0, 0.25, 4.0]
            .iter()
            .map(|f| Prior { chroma: [base.chroma[0] * f, base.chroma[1] * f], ..base })
            .collect();
        let s = separate_bank(&m, &t, &XParams::default(), &priors, &Selection::default());
        let (mut edge, mut inner) = (0.0f64, 0.0f64);
        for y in 0..h {
            for x in 0..w {
                let e = s.c1.at(x, y).abs().max(s.c2.at(x, y).abs());
                if x >= 8 && y >= 8 && x < w - 8 && y < h - 8 {
                    inner = inner.max(e);
                } else {
                    edge = edge.max(e);
                }
            }
        }
        // Measured 2e-5 inside and 3e-3 in the frame's corner.
        assert!(inner < 1e-4, "{inner}");
        assert!(edge < 5e-3, "{edge}");
    }

    /// Two colours with hard edges between them (a disc and stripes, the
    /// edges one pixel wide): `(error before, error after)` of the second
    /// step, RMS over the three channels away from the frame's edge.
    fn two_colours(a: [f64; 3], b: [f64; 3]) -> (f64, f64) {
        let (w, h) = (154, 121);
        let mut rgb = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let (fx, fy) = (x as f64, y as f64);
                let disc = 30.0 - (fx - 50.0).hypot(fy - 60.0);
                let stripes = 3.0 - ((0.8 * fx + 0.6 * fy).rem_euclid(14.0) - 7.0).abs();
                let inside = if fx < 95.0 { disc } else { stripes };
                let t = (inside + 0.5).clamp(0.0, 1.0);
                rgb.set(x, y, a[0] + t * (b[0] - a[0]), a[1] + t * (b[1] - a[1]), a[2] + t * (b[2] - a[2]));
            }
        }
        let t = fuji().shifted(2, 2);
        let m = mosaic_of(&rgb, &t);
        let p = XParams { lmmse: Some(Prior { lens: 0.3, ..Prior::default() }), ..Default::default() };
        let first = separate_xtrans(&m, &t, &p);
        let second = refine(&m, &t, &first, &Refine::default());
        let rms = |s: &XSeparation| {
            let out = s.rgb();
            let (mut sum, mut n) = (0.0, 0.0);
            for y in 8..h - 8 {
                for x in 8..w - 8 {
                    for (o, i) in [(&out.r, &rgb.r), (&out.g, &rgb.g), (&out.b, &rgb.b)] {
                        sum += (o.at(x, y) - i.at(x, y)).powi(2);
                        n += 1.0;
                    }
                }
            }
            (sum / n).sqrt()
        };
        (rms(&first), rms(&second))
    }

    /// Where every photosite sees the line between the two colours (blue
    /// against yellow), the second step takes most of the first's error
    /// away; where only the red ones do (red against black) it has
    /// nothing to add and does no harm.
    #[test]
    fn the_second_step_follows_the_line_between_two_colours() {
        let (before, after) = two_colours([0.05, 0.1, 0.6], [0.85, 0.7, 0.1]);
        eprintln!("blue/yellow {before:.5} {after:.5}");
        assert!(after < 0.6 * before, "{before} {after}");
        let (before, after) = two_colours([0.02, 0.02, 0.02], [0.7, 0.03, 0.03]);
        eprintln!("black/red {before:.5} {after:.5}");
        assert!(after < 1.05 * before, "{before} {after}");
    }

    /// A dark flat field between brighter ones, under noise, keeps its
    /// level under a bank with oriented priors: the mean of each channel
    /// in its middle is the mean of that colour's photosites there.
    #[test]
    fn a_noisy_flat_field_keeps_its_level_under_oriented_priors() {
        let (w, h) = (216, 180);
        let field = [0.0318, 0.0329, 0.0337];
        let around = [[0.42, 0.30, 0.12], [0.10, 0.22, 0.40], [0.30, 0.34, 0.10], [0.36, 0.12, 0.20]];
        let inside = |x: usize, y: usize| (60..150).contains(&x) && (48..132).contains(&y);
        let mut rgb = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let c = if inside(x, y) {
                    field
                } else {
                    around[usize::from(x >= 105) + 2 * usize::from(y >= 90)]
                };
                rgb.set(x, y, c[0], c[1], c[2]);
            }
        }
        let t = fuji().shifted(2, 2);
        let mut m = mosaic_of(&rgb, &t);
        let mut state = 0x2545_f491_4f6c_dd1du64;
        for v in m.data.iter_mut() {
            // Sum of four uniform numbers: near enough to a Gaussian.
            let mut n = 0.0;
            for _ in 0..4 {
                state = state.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
                n += (state >> 11) as f64 / (1u64 << 53) as f64 - 0.5;
            }
            *v += 0.006 * n / (4.0f64 / 12.0).sqrt();
        }
        let base = Prior {
            alpha: 3.21,
            alpha_c: 1.94,
            chroma: [0.1037, 0.0737],
            noise: 5.3e-2,
            lens: 0.52,
            ..Prior::default()
        };
        let mut priors = vec![base];
        for i in 0..8 {
            priors.push(Prior {
                chroma: [base.chroma[0] * 4.0, base.chroma[1] * 4.0],
                oriented: Some((PI * i as f64 / 8.0, 8.0)),
                ..base
            });
        }
        let s = separate_bank(&m, &t, &XParams::default(), &priors, &Selection::default());
        let out = s.rgb();
        let (mut sites, mut count, mut got) = ([0.0f64; 3], [0.0f64; 3], [0.0f64; 3]);
        for y in 66..114 {
            for x in 78..132 {
                let c = t.color_at(x as isize, y as isize) as usize;
                sites[c] += m.data[y * w + x];
                count[c] += 1.0;
                for (k, plane) in [&out.r, &out.g, &out.b].into_iter().enumerate() {
                    got[k] += plane.at(x, y);
                }
            }
        }
        let total = count.iter().sum::<f64>();
        for k in 0..3 {
            let (want, have) = (sites[k] / count[k], got[k] / total);
            // Measured within 0.3 %. With the direction held down to the
            // slowest frequencies: red -59 %, green +18 %, blue +13 %.
            assert!((have / want - 1.0).abs() < 0.01, "channel {k}: {have} against {want}");
        }
    }

    /// The second step leaves a picture without colour alone.
    #[test]
    fn the_second_step_leaves_grey_alone() {
        let (w, h) = (154, 121);
        let mut rgb = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let v = 0.5 + 0.2 * (x as f64 * 0.31).sin() * (y as f64 * 0.17).cos();
                rgb.set(x, y, v, v, v);
            }
        }
        let t = fuji().shifted(2, 2);
        let m = mosaic_of(&rgb, &t);
        let p = XParams { lmmse: Some(Prior::default()), ..Default::default() };
        let first = separate_xtrans(&m, &t, &p);
        let second = refine(&m, &t, &first, &Refine::default());
        let moved =
            first.lum.data.iter().zip(&second.lum.data).fold(0.0f64, |a, (x, y)| a.max((x - y).abs()));
        assert!(moved < 1e-3, "{moved}");
    }
}
