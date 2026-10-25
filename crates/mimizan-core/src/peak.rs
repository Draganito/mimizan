//! Peak's high-pass, measured on the native-size render of the picture.
//!
//! The same search as the `peak` program: short steps across the picture,
//! the soft ones dropped, the sharpest tenth averaged. Radius and amount are
//! the smallest that bring that edge close to `TARGET_MTF50` without the
//! contrast at a quarter cycle per pixel passing `M25_CAP`. A picture with
//! no such edge keeps the fallback radius and an amount of zero.
//!
//! `out = x + amount · (x − blur(σ))`. The blur is the same Gaussian the
//! print's unsharp mask uses ([`crate::filter::gaussian_kernel`]), so a
//! measured amount applied with [`crate::usm::unsharp`] is the mask this
//! search scored. The radius is in pixels of the negative; the print scales
//! it with the picture ([`crate::print::peak_print_radius`]).

use crate::filter::{gaussian_kernel, reflect};
use std::f64::consts::PI;

/// Blur radius, in pixels of the negative, when the search finds no edge:
/// the recipe's default, so a measured recipe and an untouched one agree.
const SIGMA_PX: f64 = crate::recipe::PEAK_RADIUS_DEFAULT;
/// Where a measured edge should land, in cycles per pixel.
const TARGET_MTF50: f64 = 0.35;
/// Contrast at a quarter cycle per pixel. The rim appears above 0.80.
/// The search stops a tenth earlier.
const M25_CAP: f64 = 0.70;
/// Highest lift the search will try, as contrast at half a cycle per pixel.
const FALLBACK_GAIN: f64 = 2.20;

const STRIDE: usize = 4;
const MARGIN: usize = 16;
const MAG_MIN: f32 = 0.02;
const CELL: usize = 96;
const PER_CELL: usize = 3;
const MAX_EDGES: usize = 2_000;
const MIN_EDGES: usize = 8;
const PROFILE: usize = 13;
const SIGMAS: [f64; 5] = [0.7, 1.0, 1.4, 2.0, 2.8];

struct Highpass {
    sigma: f64,
    /// Coefficient in `x + amount · (x − blur)`.
    amount: f64,
}

/// Radius and amount for one picture. `found` is false when no clean edge
/// was measured; the picture is then left as it was.
pub struct Tuned {
    /// Gaussian σ, pixels of the negative.
    pub radius: f64,
    /// Coefficient in `x + amount · (x − blur)`.
    pub amount: f64,
    /// A clean edge was measured.
    pub found: bool,
}

/// Measure one picture. `picture` is the native-size print, row-major,
/// without the paper border. Values are the code values of the sheet, 0 to
/// 1. A buffer that does not match `width × height` is not measured.
pub fn tune(picture: &[f32], width: usize, height: usize) -> Tuned {
    let fits = width.checked_mul(height).is_some_and(|n| n == picture.len());
    let Some(esf) = fits.then(|| sharp_esf(picture, width, height)).flatten() else {
        let mask = for_gain(1.0);
        return Tuned { radius: mask.sigma, amount: mask.amount, found: false };
    };
    let (sigma, gain, _, _) = solve(&esf);
    let mask = for_sigma_gain(sigma, gain);
    Tuned { radius: mask.sigma, amount: mask.amount, found: true }
}

fn for_gain(nyquist_gain: f64) -> Highpass {
    for_sigma_gain(SIGMA_PX, nyquist_gain)
}

fn for_sigma_gain(sigma: f64, nyquist_gain: f64) -> Highpass {
    let pass = 1.0 - blurred(0.5, sigma);
    let amount = (nyquist_gain - 1.0) / pass.max(1e-6);
    Highpass { sigma, amount }
}

/// Response of the Gaussian at `freq` cycles per pixel.
fn blurred(freq: f64, sigma: f64) -> f64 {
    let k = gaussian_kernel(sigma);
    let r = k.len() / 2;
    k.iter().enumerate().map(|(i, w)| w * (2.0 * PI * freq * (i as f64 - r as f64)).cos()).sum()
}

#[derive(Clone, Copy)]
struct Gate {
    step: f32,
    flat: f32,
}

/// Strict first, then the gates a real photograph still passes.
const GATES: [Gate; 3] =
    [Gate { step: 0.12, flat: 0.04 }, Gate { step: 0.08, flat: 0.08 }, Gate { step: 0.05, flat: 0.14 }];

fn solve(esf: &[f64]) -> (f64, f64, f64, f64) {
    let (bare, _) = measure(esf);
    let mut best: Option<(f64, f64, f64, f64)> = None;
    for &sigma in &SIGMAS {
        let mut g = 1.0;
        while g <= FALLBACK_GAIN + 1e-9 {
            let mask = for_sigma_gain(sigma, g);
            let out = lift_esf(esf, mask.sigma, mask.amount);
            let (m50, m25) = measure(&out);
            if m25 <= M25_CAP && m50 <= TARGET_MTF50 + 0.01 {
                let replace = match best {
                    None => true,
                    Some((bm, bs, bg, _)) => {
                        m50 > bm + 0.015
                            || ((m50 - bm).abs() <= 0.015
                                && (sigma < bs - 1e-9 || (sigma - bs).abs() < 1e-9 && g < bg))
                    }
                };
                if replace {
                    best = Some((m50, sigma, g, m25));
                }
            }
            if m25 > M25_CAP || m50 > TARGET_MTF50 + 0.01 {
                break;
            }
            g = ((g + 0.05) * 20.0).round() / 20.0;
        }
    }
    match best {
        Some((after, sigma, gain, _)) => (sigma, gain, bare, after),
        None => (SIGMA_PX, 1.0, bare, bare),
    }
}

fn lift_esf(esf: &[f64], sigma: f64, amount: f64) -> Vec<f64> {
    let blurred = blur_line(esf, sigma);
    esf.iter().zip(blurred).map(|(v, b)| v + amount * (v - b)).collect()
}

/// Gaussian blur of one line, reflect-101 borders.
fn blur_line(src: &[f64], sigma: f64) -> Vec<f64> {
    let k = gaussian_kernel(sigma);
    let r = k.len() / 2;
    let n = src.len();
    let mut out = vec![0.0; n];
    for (x, dest) in out.iter_mut().enumerate() {
        let mut acc = 0.0;
        for (i, w) in k.iter().enumerate() {
            acc += w * src[reflect(x as isize + i as isize - r as isize, n)];
        }
        *dest = acc;
    }
    out
}

/// MTF50 and the contrast at 0.25 cycles per pixel.
fn measure(esf: &[f64]) -> (f64, f64) {
    if esf.len() < 8 {
        return (0.0, 0.0);
    }
    let lsf: Vec<f64> = esf.windows(2).map(|w| w[1] - w[0]).collect();
    let n = 64.max(lsf.len());
    let mut spec = vec![0.0; n / 2 + 1];
    for (k, bin) in spec.iter_mut().enumerate() {
        let mut re = 0.0;
        let mut im = 0.0;
        for (i, v) in lsf.iter().enumerate() {
            let a = 2.0 * PI * k as f64 * i as f64 / n as f64;
            re += v * a.cos();
            im -= v * a.sin();
        }
        *bin = (re * re + im * im).sqrt();
    }
    let scale = spec[0] + 1e-15;
    for v in &mut spec {
        *v /= scale;
    }
    let freq = |k: usize| k as f64 / n as f64;
    let mut m50 = 0.5;
    for k in 1..spec.len() {
        let f = freq(k);
        if f > 0.5 {
            break;
        }
        if spec[k] <= 0.5 {
            let f0 = freq(k - 1);
            let t = (0.5 - spec[k - 1]) / (spec[k] - spec[k - 1] + 1e-15);
            m50 = f0 + t * (f - f0);
            break;
        }
    }
    (m50, at(&spec, n, 0.25))
}

fn at(spec: &[f64], n: usize, f: f64) -> f64 {
    let freq = |k: usize| k as f64 / n as f64;
    let mut k = 1;
    while k + 1 < spec.len() && freq(k) < f {
        k += 1;
    }
    k = k.clamp(1, spec.len() - 1);
    let f0 = freq(k - 1);
    let f1 = freq(k);
    let t = ((f - f0) / (f1 - f0 + 1e-15)).clamp(0.0, 1.0);
    spec[k - 1] + t * (spec[k] - spec[k - 1])
}

fn sharp_esf(picture: &[f32], width: usize, height: usize) -> Option<Vec<f64>> {
    if width < 48 || height < 48 {
        return None;
    }
    let (gx, gy, mag) = gradients(picture, width, height);
    let ridges = spread_ridges(&mag, &gx, &gy, width, height);
    if ridges.is_empty() {
        return None;
    }
    for gate in GATES {
        let mut kept: Vec<(f64, Vec<f64>)> = Vec::new();
        for &(x, y) in &ridges {
            if kept.len() == MAX_EDGES {
                break;
            }
            let i = y * width + x;
            if let Some((rms, profile)) = profile(picture, width, height, x, y, gx[i], gy[i], gate) {
                kept.push((rms, profile));
            }
        }
        if kept.len() >= MIN_EDGES {
            return Some(pool_sharp(&mut kept));
        }
    }
    None
}

fn pool_sharp(kept: &mut [(f64, Vec<f64>)]) -> Vec<f64> {
    kept.sort_by(|a, b| a.0.total_cmp(&b.0));
    let tenth = (kept.len() as f64 * 0.10).ceil().max(MIN_EDGES as f64) as usize;
    let n = tenth.min(kept.len());
    pool(&kept[..n])
}

fn pool(kept: &[(f64, Vec<f64>)]) -> Vec<f64> {
    let mut esf = vec![0.0; PROFILE];
    for (_, profile) in kept {
        for (slot, v) in esf.iter_mut().zip(profile) {
            *slot += v;
        }
    }
    let n = kept.len().max(1) as f64;
    for v in &mut esf {
        *v /= n;
    }
    esf
}

/// A few of the strongest ridges in every block, so one bright lash cannot
/// crowd out the rest of the picture.
fn spread_ridges(mag: &[f32], gx: &[f32], gy: &[f32], width: usize, height: usize) -> Vec<(usize, usize)> {
    let cw = width.div_ceil(CELL);
    let ch = height.div_ceil(CELL);
    let mut cells: Vec<Vec<(f32, usize, usize)>> = vec![Vec::new(); cw * ch];
    let mut y = MARGIN;
    while y + MARGIN < height {
        let mut x = MARGIN;
        while x + MARGIN < width {
            let i = y * width + x;
            let m = mag[i];
            if m >= MAG_MIN && is_ridge(mag, width, height, x, y, gx[i], gy[i]) {
                let cell = &mut cells[(y / CELL) * cw + (x / CELL)];
                if cell.len() < PER_CELL {
                    cell.push((m, x, y));
                } else if let Some(low) = cell.iter_mut().min_by(|a, b| a.0.total_cmp(&b.0)) {
                    if m > low.0 {
                        *low = (m, x, y);
                    }
                }
            }
            x += STRIDE;
        }
        y += STRIDE;
    }
    let mut ridges = Vec::new();
    for cell in cells {
        ridges.extend(cell.into_iter().map(|(_, x, y)| (x, y)));
    }
    ridges
}

fn gradients(picture: &[f32], width: usize, height: usize) -> (Vec<f32>, Vec<f32>, Vec<f32>) {
    let n = width * height;
    let mut gx = vec![0.0f32; n];
    let mut gy = vec![0.0f32; n];
    let mut mag = vec![0.0f32; n];
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let i = y * width + x;
            let dx = 0.5 * (picture[i + 1] - picture[i - 1]);
            let dy = 0.5 * (picture[i + width] - picture[i - width]);
            gx[i] = dx;
            gy[i] = dy;
            mag[i] = dx.hypot(dy);
        }
    }
    (gx, gy, mag)
}

fn is_ridge(mag: &[f32], width: usize, height: usize, x: usize, y: usize, dx: f32, dy: f32) -> bool {
    let m = mag[y * width + x];
    let norm = m.max(1e-8);
    let sx = (dx / norm).round() as isize;
    let sy = (dy / norm).round() as isize;
    if sx == 0 && sy == 0 {
        return false;
    }
    let ahead = shifted(width, height, x, y, sx, sy);
    let behind = shifted(width, height, x, y, -sx, -sy);
    let (Some(a), Some(b)) = (ahead, behind) else {
        return false;
    };
    m >= mag[a] && m >= mag[b]
}

fn shifted(width: usize, height: usize, x: usize, y: usize, dx: isize, dy: isize) -> Option<usize> {
    let xx = x as isize + dx;
    let yy = y as isize + dy;
    if xx < 0 || yy < 0 || xx >= width as isize || yy >= height as isize {
        return None;
    }
    Some(yy as usize * width + xx as usize)
}

#[allow(clippy::too_many_arguments)]
fn profile(
    picture: &[f32],
    width: usize,
    height: usize,
    x: usize,
    y: usize,
    dx: f32,
    dy: f32,
    gate: Gate,
) -> Option<(f64, Vec<f64>)> {
    let norm = dx.hypot(dy).max(1e-8);
    let ux = dx / norm;
    let uy = dy / norm;
    let mut wide = [0.0f32; 29];
    for (i, slot) in wide.iter_mut().enumerate() {
        let t = i as f32 - 14.0;
        let sx = x as f32 + t * ux;
        let sy = y as f32 + t * uy;
        *slot = bilinear(picture, width, height, sx, sy)?;
    }
    let left: f32 = wide[..4].iter().sum::<f32>() / 4.0;
    let right: f32 = wide[25..].iter().sum::<f32>() / 4.0;
    let step = right - left;
    if step.abs() < gate.step {
        return None;
    }
    let sign = step.signum();
    let mut ad = [0.0f64; 28];
    let mut sum = 0.0;
    for i in 0..28 {
        let d = f64::from((wide[i + 1] - wide[i]) * sign).max(0.0);
        ad[i] = d;
        sum += d;
    }
    if sum < 1e-6 {
        return None;
    }
    let mut cen = 0.0;
    for (i, d) in ad.iter().enumerate() {
        cen += i as f64 * d;
    }
    cen /= sum;
    let mut core = 0.0;
    for (i, d) in ad.iter().enumerate() {
        if (i as f64 - cen).abs() <= 2.0 {
            core += d;
        }
    }
    if core < 0.50 * sum {
        return None;
    }
    let mut vs = [0.0f64; PROFILE];
    for (i, slot) in vs.iter_mut().enumerate() {
        let pos = 8.0 + i as f64 + (cen - 13.5);
        if !(0.0..=28.0).contains(&pos) {
            return None;
        }
        let i0 = pos.floor() as usize;
        let f = pos - i0 as f64;
        let a = f64::from(wide[i0]);
        let b = f64::from(wide[(i0 + 1).min(28)]);
        *slot = a + f * (b - a);
    }
    let left = vs[..3].iter().sum::<f64>() / 3.0;
    let right = vs[PROFILE - 3..].iter().sum::<f64>() / 3.0;
    let step = right - left;
    if step.abs() < f64::from(gate.step) {
        return None;
    }
    let flat = f64::from(gate.flat);
    if deviation(&vs[..3]) > flat || deviation(&vs[PROFILE - 3..]) > flat {
        return None;
    }
    let mut back = 0;
    let mut diffs = 0;
    for w in vs.windows(2) {
        diffs += 1;
        if (w[1] - w[0]) * step < -0.02 {
            back += 1;
        }
    }
    if back as f64 > 0.2 * diffs as f64 {
        return None;
    }
    let mut ad2 = [0.0f64; PROFILE - 1];
    let mut sum2 = 0.0;
    for i in 0..PROFILE - 1 {
        let d = ((vs[i + 1] - vs[i]) * step.signum()).max(0.0);
        ad2[i] = d;
        sum2 += d;
    }
    if sum2 < 1e-8 {
        return None;
    }
    let mut c2 = 0.0;
    for (i, d) in ad2.iter().enumerate() {
        c2 += i as f64 * d;
    }
    c2 /= sum2;
    let mut moment = 0.0;
    for (i, d) in ad2.iter().enumerate() {
        let t = i as f64 - c2;
        moment += t * t * d;
    }
    let rms = (moment / sum2).sqrt();
    let mut normed = Vec::with_capacity(PROFILE);
    for v in vs {
        normed.push((v - left) / step);
    }
    Some((rms, normed))
}

fn deviation(values: &[f64]) -> f64 {
    let mean = values.iter().sum::<f64>() / values.len() as f64;
    let var = values.iter().map(|v| (v - mean).powi(2)).sum::<f64>() / values.len() as f64;
    var.sqrt()
}

fn bilinear(picture: &[f32], width: usize, height: usize, x: f32, y: f32) -> Option<f32> {
    if x < 0.0 || y < 0.0 || x > (width - 1) as f32 || y > (height - 1) as f32 {
        return None;
    }
    let x0 = x.floor() as usize;
    let y0 = y.floor() as usize;
    let x1 = (x0 + 1).min(width - 1);
    let y1 = (y0 + 1).min(height - 1);
    let fx = x - x0 as f32;
    let fy = y - y0 as f32;
    let v00 = picture[y0 * width + x0];
    let v10 = picture[y0 * width + x1];
    let v01 = picture[y1 * width + x0];
    let v11 = picture[y1 * width + x1];
    Some(v00 * (1.0 - fx) * (1.0 - fy) + v10 * fx * (1.0 - fy) + v01 * (1.0 - fx) * fy + v11 * fx * fy)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sheet(width: usize, height: usize, sigma: f64) -> Vec<f32> {
        let mut raw = vec![0.0f32; width * height];
        // One pixel below the middle lands the steepest row on the search grid.
        let mid = height as f64 / 2.0 + 1.0;
        let radius = 12;
        for y in 0..height {
            let mut v = 0.0;
            let mut norm = 0.0;
            for t in -radius..=radius {
                let w = (-0.5 * (t as f64).powi(2) / (sigma * sigma)).exp();
                let step = if (y as f64 + t as f64) >= mid { 0.78 } else { 0.06 };
                v += w * step;
                norm += w;
            }
            for x in 0..width {
                raw[y * width + x] = (v / norm) as f32;
            }
        }
        raw
    }

    #[test]
    fn a_flat_sheet_keeps_the_fallback() {
        let raw = vec![0.4f32; 80 * 64];
        let tuned = tune(&raw, 80, 64);
        assert!(!tuned.found);
        assert!(tuned.amount.abs() < 1e-6);
        assert!((tuned.radius - SIGMA_PX).abs() < 1e-9);
    }

    #[test]
    fn a_soft_edge_is_lifted() {
        let raw = sheet(240, 120, 1.3);
        let tuned = tune(&raw, 240, 120);
        assert!(tuned.found);
        assert!(tuned.amount > 0.05, "amount {}", tuned.amount);
        assert!(tuned.radius >= SIGMA_PX);
    }

    #[test]
    fn an_edge_already_there_is_left_quiet() {
        let mut esf = vec![0.0; PROFILE];
        for (i, v) in esf.iter_mut().enumerate() {
            *v = if i < PROFILE / 2 { 0.0 } else { 1.0 };
        }
        let (sigma, gain, before, after) = solve(&esf);
        assert!(before > TARGET_MTF50, "before {before}");
        assert!((gain - 1.0).abs() < 1e-9, "gain {gain}");
        assert!((sigma - SIGMA_PX).abs() < 1e-9, "sigma {sigma}");
        assert!((after - before).abs() < 1e-9, "after {after}");
    }
}
