//! Adaptive block-spectrum mask and the adaptive separation (SPEC sections 5.2–5.4).

use crate::blockfft::{BlockFft, CARRIERS};
use crate::cfa::BayerPhase;
use crate::filter::{lowpass, LowpassSpec};
use crate::ingest::Mosaic;
use crate::plane::Plane;
use crate::separate::{luminance, SeparateParams, Separation};
use rayon::prelude::*;
use tracing::debug;

pub const BLOCK: usize = 64;
pub const STEP: usize = 32;
/// Always-chroma core around each carrier (c/px).
pub const CORE: f64 = 0.05;
/// Outer edge of the ambiguous band = chroma cutoff (c/px).
pub const BAND: f64 = 0.15;
/// Outer edge of the luminance ring (c/px).
pub const RING: f64 = 0.35;

/// Per-block mask grids (after erosion), each value in [0,1]: 1 = carrier
/// content is luminance. Pixels are sampled bilinearly via `GridSampler`.
pub struct MaskField {
    pub grids: [Vec<f64>; 3],
    pub blocks_x: usize,
    pub blocks_y: usize,
}

impl MaskField {
    /// Full-resolution plane of grid `k` (tests, previews).
    pub fn plane(&self, k: usize, w: usize, h: usize) -> Plane {
        interpolate_grid(&self.grids[k], self.blocks_x, self.blocks_y, w, h)
    }

    /// Full-resolution `max(M_1, M_2a, M_2b)`.
    pub fn max_plane(&self, w: usize, h: usize) -> Plane {
        let s = GridSampler::new(self.blocks_x, self.blocks_y, w);
        let mut out = Plane::zeros(w, h);
        out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
            let (r1, r2a, r2b) =
                (s.row(&self.grids[0], y), s.row(&self.grids[1], y), s.row(&self.grids[2], y));
            for (x, o) in row.iter_mut().enumerate() {
                *o = r1.at(x).max(r2a.at(x).max(r2b.at(x)));
            }
        });
        out
    }
}

/// Bilinear sampling of a block grid at pixel positions, with the x tables
/// precomputed once per plane width. Identical arithmetic to `interpolate_grid`.
pub struct GridSampler {
    bx: usize,
    by: usize,
    x0: Vec<usize>,
    x1: Vec<usize>,
    fx: Vec<f64>,
}

/// One grid row ready for `at(x)`.
pub struct GridRow<'a> {
    s: &'a GridSampler,
    top: &'a [f64],
    bottom: &'a [f64],
    fy: f64,
}

impl GridSampler {
    pub fn new(bx: usize, by: usize, w: usize) -> Self {
        let half = (BLOCK / 2) as f64;
        let mut x0 = Vec::with_capacity(w);
        let mut x1 = Vec::with_capacity(w);
        let mut fx = Vec::with_capacity(w);
        for x in 0..w {
            let gx = ((x as f64 - half) / STEP as f64).clamp(0.0, (bx.max(1) - 1) as f64);
            let a = gx.floor() as usize;
            x0.push(a);
            x1.push((a + 1).min(bx.max(1) - 1));
            fx.push(gx - a as f64);
        }
        Self { bx, by, x0, x1, fx }
    }

    pub fn row<'a>(&'a self, grid: &'a [f64], y: usize) -> GridRow<'a> {
        let half = (BLOCK / 2) as f64;
        let gy = ((y as f64 - half) / STEP as f64).clamp(0.0, (self.by.max(1) - 1) as f64);
        let y0 = gy.floor() as usize;
        let y1 = (y0 + 1).min(self.by.max(1) - 1);
        GridRow {
            s: self,
            top: &grid[y0 * self.bx..(y0 + 1) * self.bx],
            bottom: &grid[y1 * self.bx..(y1 + 1) * self.bx],
            fy: gy - y0 as f64,
        }
    }
}

impl GridRow<'_> {
    #[inline]
    pub fn at(&self, x: usize) -> f64 {
        let (x0, x1, fx) = (self.s.x0[x], self.s.x1[x], self.s.fx[x]);
        let (a, b, c, d) = (self.top[x0], self.top[x1], self.bottom[x0], self.bottom[x1]);
        (a * (1.0 - fx) + b * fx) * (1.0 - self.fy) + (c * (1.0 - fx) + d * fx) * self.fy
    }
}

struct BinSets {
    band: [Vec<usize>; 3],
    ring: [Vec<usize>; 3],
}

fn bin_sets(bf: &BlockFft) -> BinSets {
    let n = bf.n;
    let mut band: [Vec<usize>; 3] = Default::default();
    let mut ring: [Vec<usize>; 3] = Default::default();
    for (k, &(fx, fy)) in CARRIERS.iter().enumerate() {
        for iy in 0..n {
            for ix in 0..n {
                let d = bf.dist_inf(ix, iy, fx, fy);
                if (CORE..BAND).contains(&d) {
                    band[k].push(iy * n + ix);
                } else if (BAND..RING).contains(&d) {
                    ring[k].push(iy * n + ix);
                }
            }
        }
    }
    BinSets { band, ring }
}

/// Block grid dimensions for a plane: blocks start at 0, STEP, ... while they fit.
pub fn grid_dims(w: usize, h: usize) -> (usize, usize) {
    if w < BLOCK || h < BLOCK {
        return (0, 0);
    }
    ((w - BLOCK) / STEP + 1, (h - BLOCK) / STEP + 1)
}

/// Bilinear interpolation of block-centre values to pixels, clamped outside.
pub fn interpolate_grid(grid: &[f64], bx: usize, by: usize, w: usize, h: usize) -> Plane {
    let mut out = Plane::zeros(w, h);
    if bx == 0 || by == 0 {
        return out;
    }
    let half = (BLOCK / 2) as f64;
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let gy = ((y as f64 - half) / STEP as f64).clamp(0.0, (by - 1) as f64);
        let y0 = gy.floor() as usize;
        let y1 = (y0 + 1).min(by - 1);
        let fy = gy - y0 as f64;
        for (x, o) in row.iter_mut().enumerate() {
            let gx = ((x as f64 - half) / STEP as f64).clamp(0.0, (bx - 1) as f64);
            let x0 = gx.floor() as usize;
            let x1 = (x0 + 1).min(bx - 1);
            let fx = gx - x0 as f64;
            let a = grid[y0 * bx + x0];
            let b = grid[y0 * bx + x1];
            let c = grid[y1 * bx + x0];
            let d = grid[y1 * bx + x1];
            *o = (a * (1.0 - fx) + b * fx) * (1.0 - fy) + (c * (1.0 - fx) + d * fx) * fy;
        }
    });
    out
}

/// Fraction of ring energy expected to leak into the band for a 1/f² (edge-like)
/// luminance spectrum: ∫₀.₃₅^0.5 f⁻² df / ∫₀.₁₅^0.35 f⁻² df ≈ 0.225, rounded up
/// to lean slightly towards keeping texture.
pub const LEAK_RATIO: f64 = 0.3;
/// Significance (in standard deviations) of band/ring energy before it counts.
pub const SIGMA: f64 = 3.0;
/// Variance inflation of a sum of N windowed power bins relative to i.i.d.
/// chi² bins. Hann-window bin correlation, conjugate symmetry and the 2×2
/// sub-lattice structure of Bayer noise (each colour's spectrum repeats with
/// period 0.5) all correlate the bins. Measured on pure synthetic noise
/// (`flat_noisy_patches_give_zero_mask`): band std 0.161 vs 0.080 i.i.d.
/// (×8 variance), ring std 0.100 vs 0.035 (×16).
pub const BAND_FLUCT: f64 = 8.0;
pub const RING_FLUCT: f64 = 16.0;

/// Per-block, per-carrier mask value (SPEC 5.2).
///
/// `p_c`, `p_l`: measured band/ring power sums; `n_c`, `n_l`: bin counts;
/// `n_bin`: expected noise power per bin. Returns M in [0,1], the share of the
/// band left in place as luminance:
///
/// 1. no significant ring energy → M = 0 (the fixed estimator);
/// 2. no significant band energy → M = 1 (nothing to remove but noise);
/// 3. otherwise `M = min(1, leak / E_C)²` with `leak = LEAK_RATIO·E_L` the
///    luminance expected to reach the band — squared so ambiguous evidence
///    (colour edges) collapses towards M = 0;
/// 4. capped at `√(n_c·n_bin / (E_C − leak))`: the band energy that the ring
///    cannot explain is chroma, and the residual M leaves of it must stay
///    under the band's noise floor.
pub fn mask_value(p_c: f64, p_l: f64, n_c: f64, n_l: f64, n_bin: f64) -> f64 {
    let e_l = (p_l - n_l * n_bin - SIGMA * n_bin * (RING_FLUCT * n_l).sqrt()).max(0.0);
    if e_l <= 0.0 {
        return 0.0;
    }
    let e_c = (p_c - n_c * n_bin - SIGMA * n_bin * (BAND_FLUCT * n_c).sqrt()).max(0.0);
    if e_c <= 0.0 {
        return 1.0;
    }
    let leak = LEAK_RATIO * e_l;
    let w = (leak / e_c).min(1.0);
    let unexplained = e_c - leak;
    let cap = if unexplained > 0.0 { (n_c * n_bin / unexplained).sqrt() } else { 1.0 };
    (w * w).min(cap).min(1.0)
}

/// 3×3 minimum over the block grid: a block that says "chroma" (M = 0) wins
/// over its neighbours, so interpolation never carries M > 0 into the 32 px
/// next to a colour edge.
pub fn erode_grid(grid: &[f64], bx: usize, by: usize) -> Vec<f64> {
    let mut out = vec![0.0; grid.len()];
    for gy in 0..by {
        for gx in 0..bx {
            let mut m = f64::INFINITY;
            for dy in gy.saturating_sub(1)..=(gy + 1).min(by - 1) {
                for dx in gx.saturating_sub(1)..=(gx + 1).min(bx - 1) {
                    m = m.min(grid[dy * bx + dx]);
                }
            }
            out[gy * bx + gx] = m;
        }
    }
    out
}

/// SPEC 5.2: per block and carrier mask from windowed block spectra; 0 in
/// saturated blocks (> 25 % saturated pixels).
pub fn compute_mask(mosaic: &Mosaic) -> MaskField {
    let m = &mosaic.plane;
    let saturated = &mosaic.saturated;
    let (w, h) = (m.width, m.height);
    let (bx, by) = grid_dims(w, h);
    let bf = BlockFft::new(BLOCK);
    let sets = bin_sets(&bf);
    let nb = bx * by;
    let mut grids: [Vec<f64>; 3] = [vec![0.0; nb], vec![0.0; nb], vec![0.0; nb]];

    let values: Vec<[f64; 3]> = (0..nb)
        .into_par_iter()
        .map_init(Vec::new, |scratch, bi| {
            let gx = bi % bx;
            let gy = bi / bx;
            let x0 = gx * STEP;
            let y0 = gy * STEP;
            let mut sat = 0usize;
            for y in y0..y0 + BLOCK {
                sat += saturated[y * w + x0..y * w + x0 + BLOCK].iter().filter(|&&s| s != 0).count();
            }
            if sat * 4 > BLOCK * BLOCK {
                return [0.0, 0.0, 0.0];
            }
            let (pw, _mean) = bf.power(m, x0, y0, scratch);
            let n_bin = mosaic.mosaic_variance(mosaic.block_levels(x0, y0, BLOCK)) * bf.window_energy;
            let mut out = [0.0; 3];
            for (k, o) in out.iter_mut().enumerate() {
                let p_c: f64 = sets.band[k].iter().map(|&i| pw[i]).sum();
                let p_l: f64 = sets.ring[k].iter().map(|&i| pw[i]).sum();
                *o = mask_value(p_c, p_l, sets.band[k].len() as f64, sets.ring[k].len() as f64, n_bin);
            }
            out
        })
        .collect();
    for (bi, v) in values.iter().enumerate() {
        for k in 0..3 {
            grids[k][bi] = v[k];
        }
    }
    if nb > 0 {
        for g in grids.iter_mut() {
            *g = erode_grid(g, bx, by);
        }
    }
    MaskField { grids, blocks_x: bx, blocks_y: by }
}

/// Alias-free 2x decimation of a plane band-limited below 0.25 c/px.
fn decimate2(p: &Plane) -> Plane {
    let (w, h) = (p.width.div_ceil(2), p.height.div_ceil(2));
    let mut out = Plane::zeros(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = p.row((2 * y).min(p.height - 1));
        for (x, o) in row.iter_mut().enumerate() {
            *o = src[(2 * x).min(p.width - 1)];
        }
    });
    out
}

/// Bilinear 2x upsampling back to (w, h).
fn upsample2(p: &Plane, w: usize, h: usize) -> Plane {
    let mut out = Plane::zeros(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = y as f64 / 2.0;
        let y0 = (sy.floor() as usize).min(p.height - 1);
        let y1 = (y0 + 1).min(p.height - 1);
        let fy = sy - y0 as f64;
        let (r0, r1) = (p.row(y0), p.row(y1));
        for (x, o) in row.iter_mut().enumerate() {
            let sx = x as f64 / 2.0;
            let x0 = (sx.floor() as usize).min(p.width - 1);
            let x1 = (x0 + 1).min(p.width - 1);
            let fx = sx - x0 as f64;
            *o = (r0[x0] * (1.0 - fx) + r0[x1] * fx) * (1.0 - fy) + (r1[x0] * (1.0 - fx) + r1[x1] * fx) * fy;
        }
    });
    out
}

/// Core (always-chroma) part of a wide low-pass chroma estimate at half
/// resolution: the input is band-limited below 0.175 c/px, so 2x decimation
/// is alias-free; then the narrow low-pass. Upsample with `upsample2` or
/// sample on the fly with `Half::at`.
pub fn chroma_core_half(c_wide: &Plane) -> Plane {
    let half = decimate2(c_wide);
    let k = LowpassSpec { cutoff: 2.0 * CORE, transition: 0.10, attenuation_db: 60.0 }.kernel();
    lowpass(&half, &k)
}

/// Full-resolution core (tests).
pub fn chroma_core(c_wide: &Plane) -> Plane {
    upsample2(&chroma_core_half(c_wide), c_wide.width, c_wide.height)
}

/// Bilinear 2x upsampling of one half-resolution row pair, same arithmetic as
/// `upsample2`.
struct HalfRow<'a> {
    r0: &'a [f64],
    r1: &'a [f64],
    fy: f64,
    hw: usize,
}

impl HalfRow<'_> {
    fn new(p: &Plane, y: usize) -> HalfRow<'_> {
        let sy = y as f64 / 2.0;
        let y0 = (sy.floor() as usize).min(p.height - 1);
        let y1 = (y0 + 1).min(p.height - 1);
        HalfRow { r0: p.row(y0), r1: p.row(y1), fy: sy - y0 as f64, hw: p.width }
    }

    /// `x/2` has the fraction 0 or exactly 0.5, so the integer form is the
    /// same arithmetic as `upsample2` without the float floor.
    #[inline]
    fn at(&self, x: usize) -> f64 {
        let x0 = (x >> 1).min(self.hw - 1);
        let x1 = (x0 + 1).min(self.hw - 1);
        let fx = if x & 1 == 1 && x0 == x >> 1 { 0.5 } else { (x as f64 / 2.0) - x0 as f64 };
        (self.r0[x0] * (1.0 - fx) + self.r0[x1] * fx) * (1.0 - self.fy)
            + (self.r1[x0] * (1.0 - fx) + self.r1[x1] * fx) * self.fy
    }
}

/// Which per-pixel gain multiplies the band part of a chroma estimate.
#[derive(Clone, Copy)]
enum Gain {
    /// `g1 = 1 − M1`
    C1,
    /// `g2 = max(1 − M2a, 1 − M2b)`
    C2,
}

/// In place: `wide ← core + g·(wide − core)` with the core sampled from its
/// half-resolution plane and `g` from the mask grids.
fn blend_in_place(wide: &mut Plane, core_half: &Plane, field: &MaskField, gain: Gain) {
    let w = wide.width;
    let s = GridSampler::new(field.blocks_x, field.blocks_y, w);
    wide.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let core = HalfRow::new(core_half, y);
        let (ga, gb) = match gain {
            Gain::C1 => (s.row(&field.grids[0], y), None),
            Gain::C2 => (s.row(&field.grids[1], y), Some(s.row(&field.grids[2], y))),
        };
        for (x, o) in row.iter_mut().enumerate() {
            let g = match &gb {
                None => 1.0 - ga.at(x),
                Some(gb) => (1.0 - ga.at(x)).max(1.0 - gb.at(x)),
            };
            // `g·wide + (1−g)·core`: exact at g = 1 (fixed estimator) and g = 0.
            *o = g * *o + (1.0 - g) * core.at(x);
        }
    });
}

/// In place: Dubois combination of the two C2 copies into `c2a`:
/// `(ρa·Ĉ2a + ρb·Ĉ2b)/(ρa+ρb)`, `ρ = 1 − M`; the plain mean when both
/// copies are fully masked (ρa = ρb = 0, nothing to prefer).
fn combine_c2_in_place(c2a: &mut Plane, c2b: &Plane, field: &MaskField) {
    let w = c2a.width;
    let s = GridSampler::new(field.blocks_x, field.blocks_y, w);
    c2a.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let (ma, mb) = (s.row(&field.grids[1], y), s.row(&field.grids[2], y));
        let b = c2b.row(y);
        for (x, o) in row.iter_mut().enumerate() {
            let (ra, rb) = (1.0 - ma.at(x), 1.0 - mb.at(x));
            let sum = ra + rb;
            *o = if sum > 1e-9 { (ra * *o + rb * b[x]) / sum } else { 0.5 * (*o + b[x]) };
        }
    });
}

/// Relative RMS change between two planes; per-row sums combined in row order
/// (deterministic across thread counts).
fn rel_change(a: &Plane, b: &Plane) -> f64 {
    let w = a.width;
    let rows: Vec<(f64, f64)> = a
        .data
        .par_chunks(w)
        .zip(b.data.par_chunks(w))
        .map(|(ra, rb)| {
            ra.iter().zip(rb).fold((0.0, 0.0), |(n, d), (&x, &y)| (n + (x - y) * (x - y), d + y * y))
        })
        .collect();
    let (num, den) = rows.iter().fold((0.0, 0.0), |(n, d), &(a, b)| (n + a, d + b));
    (num / den.max(1e-300)).sqrt()
}

/// Subtract the other carriers' current estimates (gains already folded into
/// Ĉ), then re-demodulate carrier `k`.
fn refine(m: &Plane, phase: BayerPhase, k: usize, c1: &Plane, c2: &Plane, kernel: &[f64]) -> Plane {
    let c = phase.carriers();
    let w = m.width;
    let mut z = Plane::zeros(w, m.height);
    z.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = crate::separate::sign_x(y);
        let (mr, c1r, c2r) = (m.row(y), c1.row(y), c2.row(y));
        for x in 0..w {
            let sx = crate::separate::sign_x(x);
            let t1 = c.eps * sx * sy * c1r[x];
            let t2a = c.a * sx * c2r[x];
            let t2b = c.b * sy * c2r[x];
            let (residual, carrier) = match k {
                0 => (mr[x] - t2a - t2b, c.eps * sx * sy),
                1 => (mr[x] - t1 - t2b, c.a * sx),
                _ => (mr[x] - t1 - t2a, c.b * sy),
            };
            row[x] = carrier * residual;
        }
    });
    lowpass(&z, kernel)
}

/// Apply the mask to the three wide estimates in place and combine C2.
/// Returns `(c1, c2)`; `c2b` is consumed.
fn apply_mask(field: &MaskField, mut c1: Plane, mut c2a: Plane, c2b: Plane) -> (Plane, Plane) {
    let mut c2b = c2b;
    let core = chroma_core_half(&c1);
    blend_in_place(&mut c1, &core, field, Gain::C1);
    let core = chroma_core_half(&c2a);
    blend_in_place(&mut c2a, &core, field, Gain::C2);
    let core = chroma_core_half(&c2b);
    blend_in_place(&mut c2b, &core, field, Gain::C2);
    combine_c2_in_place(&mut c2a, &c2b, field);
    (c1, c2a)
}

pub fn separate_adaptive(
    mosaic: &Mosaic,
    phase: BayerPhase,
    kernel: &[f64],
    c1_wide: Plane,
    c2a_wide: Plane,
    c2b_wide: Plane,
    p: &SeparateParams,
) -> Separation {
    let m = &mosaic.plane;
    let t = std::time::Instant::now();
    let field = compute_mask(mosaic);
    debug!("mask grid {}x{} in {} ms", field.blocks_x, field.blocks_y, t.elapsed().as_millis());

    let t = std::time::Instant::now();
    let (mut c1, mut c2) = apply_mask(&field, c1_wide, c2a_wide, c2b_wide);
    debug!("chroma core + blend + combine {} ms", t.elapsed().as_millis());
    let t = std::time::Instant::now();
    let mut lum = luminance(m, phase, &c1, &c2);
    let mask_max = field.max_plane(m.width, m.height);
    debug!("luminance + mask plane {} ms", t.elapsed().as_millis());
    let mut rounds = 1usize;

    while rounds < p.rounds_max {
        let n1 = refine(m, phase, 0, &c1, &c2, kernel);
        let n2a = refine(m, phase, 1, &c1, &c2, kernel);
        let n2b = refine(m, phase, 2, &c1, &c2, kernel);
        (c1, c2) = apply_mask(&field, n1, n2a, n2b);
        let next = luminance(m, phase, &c1, &c2);
        let change = rel_change(&next, &lum);
        lum = next;
        rounds += 1;
        debug!("round {rounds}: relative change {change:.2e}");
        if change < 1e-4 {
            break;
        }
    }

    Separation { lum, c1, c2, mask_max, rounds, phase, computed: 0.0 }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grid_interpolation_is_exact_at_block_centres() {
        let (w, h) = (256, 128);
        let (bx, by) = grid_dims(w, h);
        assert_eq!((bx, by), (7, 3));
        let grid: Vec<f64> = (0..bx * by).map(|i| i as f64).collect();
        let p = interpolate_grid(&grid, bx, by, w, h);
        for gy in 0..by {
            for gx in 0..bx {
                let v = p.at(gx * STEP + BLOCK / 2, gy * STEP + BLOCK / 2);
                assert!((v - (gy * bx + gx) as f64).abs() < 1e-9);
            }
        }
        assert!((p.at(0, 0) - 0.0).abs() < 1e-9);
    }

    #[test]
    fn flat_noisy_patches_give_zero_mask() {
        use crate::synth::{mosaic, Scene, SynthParams};
        let s = mosaic(Scene::Patches, 1024, 1024, &SynthParams::default());
        let field = compute_mask(&s.mosaic);
        // Blocks fully inside a patch (170x256 px) see nothing but the chroma
        // tone (in the core) and noise: M must be 0 after erosion.
        let mut interior_hits = 0usize;
        for gy in 0..field.blocks_y {
            for gx in 0..field.blocks_x {
                let (x0, y0) = (gx * STEP, gy * STEP);
                let inside_x = x0 / 170 == (x0 + BLOCK - 1) / 170;
                let inside_y = y0 / 256 == (y0 + BLOCK - 1) / 256;
                if inside_x && inside_y {
                    let bi = gy * field.blocks_x + gx;
                    let m = field.grids[0][bi].max(field.grids[1][bi]).max(field.grids[2][bi]);
                    if m > 0.0 {
                        interior_hits += 1;
                    }
                }
            }
        }
        assert_eq!(interior_hits, 0, "interior blocks flagged");
    }

    /// The constants BAND_FLUCT / RING_FLUCT are measured here: band and ring
    /// power sums over pure Bayer noise fluctuate far more than i.i.d. chi²
    /// bins would, and the mask thresholds must use the real spread.
    #[test]
    fn noise_sum_fluctuation_matches_constants() {
        use crate::synth::{mosaic, Scene, SynthParams};
        let noisy = mosaic(Scene::Patches, 1024, 1024, &SynthParams::default());
        let clean = mosaic(Scene::Patches, 1024, 1024, &SynthParams { noise: None, ..Default::default() });
        let mut noise_only = noisy.mosaic.plane.clone();
        noise_only.data.iter_mut().zip(&clean.mosaic.plane.data).for_each(|(a, &b)| *a -= b);
        let bf = BlockFft::new(BLOCK);
        let sets = bin_sets(&bf);
        let (bx, by) = grid_dims(1024, 1024);
        let mut scratch = Vec::new();
        let (mut band_r, mut ring_r) = (Vec::new(), Vec::new());
        for gy in 0..by {
            for gx in 0..bx {
                let (x0, y0) = (gx * STEP, gy * STEP);
                let (pw, _) = bf.power(&noise_only, x0, y0, &mut scratch);
                let n_bin =
                    noisy.mosaic.mosaic_variance(noisy.mosaic.block_levels(x0, y0, BLOCK)) * bf.window_energy;
                for k in 0..3 {
                    band_r.push(
                        sets.band[k].iter().map(|&i| pw[i]).sum::<f64>()
                            / (sets.band[k].len() as f64 * n_bin),
                    );
                    ring_r.push(
                        sets.ring[k].iter().map(|&i| pw[i]).sum::<f64>()
                            / (sets.ring[k].len() as f64 * n_bin),
                    );
                }
            }
        }
        let stats = |v: &[f64]| {
            let m = v.iter().sum::<f64>() / v.len() as f64;
            let s = (v.iter().map(|x| (x - m) * (x - m)).sum::<f64>() / v.len() as f64).sqrt();
            (m, s)
        };
        let (bm, bs) = stats(&band_r);
        let (rm, rs) = stats(&ring_r);
        // The noise model reproduces the mean floor.
        assert!((bm - 1.0).abs() < 0.03, "band mean {bm}");
        assert!((rm - 1.0).abs() < 0.03, "ring mean {rm}");
        // Spread within the constants (std = sqrt(FLUCT / N)), not far below.
        let band_pred = (BAND_FLUCT / sets.band[0].len() as f64).sqrt();
        let ring_pred = (RING_FLUCT / sets.ring[0].len() as f64).sqrt();
        assert!(bs < band_pred * 1.1 && bs > band_pred * 0.6, "band std {bs} vs {band_pred}");
        assert!(rs < ring_pred * 1.1 && rs > ring_pred * 0.6, "ring std {rs} vs {ring_pred}");
    }

    #[test]
    fn patches_carrier_residual_stays_below_noise() {
        use crate::metrics::carrier_energy;
        use crate::separate::{separate, SeparateParams};
        use crate::synth::{mosaic, Scene, SynthParams};
        let s = mosaic(Scene::Patches, 1024, 1024, &SynthParams::default());
        let sep = separate(&s.mosaic, &SeparateParams::default());
        let (pw, ph) = (1024 / 6, 1024 / 4);
        for row in 0..4 {
            for col in 0..6 {
                let rc = crate::decode::Rect { x: col * pw + 8, y: row * ph + 8, w: pw - 16, h: ph - 16 };
                let rep = carrier_energy(&sep.lum, &s.mosaic.noise, Some(rc), 0.15);
                for r in rep.ratio {
                    assert!(r < 1.5, "patch r{row} c{col}: carrier ratio {r}");
                }
            }
        }
    }

    #[test]
    fn mask_value_cases() {
        let (n_c, n_l, n_bin) = (300.0, 1200.0, 1.0);
        // Pure noise: no evidence.
        assert_eq!(mask_value(n_c, n_l, n_c, n_l, n_bin), 0.0);
        // Strong ring, empty band: luminance texture, keep.
        assert_eq!(mask_value(n_c, n_l + 1e5, n_c, n_l, n_bin), 1.0);
        // Colour edge: band carries far more than the ring could leak.
        let m = mask_value(n_c + 1e5, n_l + 2e4, n_c, n_l, n_bin);
        assert!(m < 0.05, "{m}");
        // Luminance edge with a chroma step the ring cannot explain: the
        // residual of the unexplained part is capped at the band noise floor.
        let (e_l, e_c) = (1e5, 6e4);
        let m = mask_value(n_c + e_c, n_l + e_l, n_c, n_l, n_bin);
        let unexplained = e_c - LEAK_RATIO * e_l;
        assert!(unexplained > 0.0);
        assert!(m * m * unexplained <= n_c * n_bin * 1.01, "{m}");
        // Band fully explained by the ring (texture): no cap, keep.
        assert_eq!(mask_value(n_c + 2e4, n_l + 1e6, n_c, n_l, n_bin), 1.0);
        // Weak ring below the inflated 3 sigma: ignored.
        assert_eq!(mask_value(n_c, n_l + 2.0 * (RING_FLUCT * n_l).sqrt(), n_c, n_l, n_bin), 0.0);
    }

    #[test]
    fn chroma_core_keeps_dc_removes_band() {
        let (w, h) = (256, 256);
        let mut p = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                // DC 0.3 plus a 0.12 c/px ripple (inside the wide band, outside the core)
                p.set(x, y, 0.3 + 0.1 * (2.0 * std::f64::consts::PI * 0.12 * x as f64).sin());
            }
        }
        let c = chroma_core(&p);
        for y in 64..h - 64 {
            for x in 64..w - 64 {
                assert!((c.at(x, y) - 0.3).abs() < 2e-3, "{}", c.at(x, y));
            }
        }
    }
}
