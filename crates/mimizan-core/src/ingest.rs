//! Normalisation, saturation mask, defect repair (SPEC sections 2 and 3).

use crate::cfa::{BayerPhase, Color};
use crate::decode::{RawFrame, Rect, SensorKind};
use crate::plane::Plane;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// `sigma(s) = a*sqrt(max(s,0)) + b`, normalised units.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct NoiseModel {
    pub a: f64,
    pub b: f64,
}

impl Default for NoiseModel {
    fn default() -> Self {
        Self { a: 0.005, b: 2e-4 }
    }
}

impl NoiseModel {
    #[inline]
    pub fn sigma(&self, s: f64) -> f64 {
        self.a * s.max(0.0).sqrt() + self.b
    }
    #[inline]
    pub fn variance(&self, s: f64) -> f64 {
        let v = self.sigma(s);
        v * v
    }
}

/// Channel balance applied before separation (SPEC section 2, "Kanalabgleich").
/// A neutral subject must give empty chrominance carriers, otherwise the raw
/// channel gains modulate the whole luminance spectrum onto the carriers.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub enum WhiteBalance {
    /// As-shot multipliers from the file; gray-world when absent.
    AsShot,
    /// Means of the three CFA channels over unsaturated pixels.
    GrayWorld,
    /// Explicit multipliers R,G,B (normalised to G = 1 internally).
    Manual([f64; 3]),
    /// No balance: raw levels as they are.
    None,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IngestParams {
    pub sat_margin: f64,
    pub sat_dilate: usize,
    pub defect_sigma: f64,
    pub fix_defects: bool,
    pub noise: NoiseModel,
    pub wb: WhiteBalance,
}

impl Default for IngestParams {
    fn default() -> Self {
        Self {
            sat_margin: 0.02,
            sat_dilate: 2,
            defect_sigma: 8.0,
            fix_defects: true,
            noise: NoiseModel::default(),
            wb: WhiteBalance::AsShot,
        }
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct CameraInfo {
    pub make: String,
    pub model: String,
    pub clean_make: String,
    pub clean_model: String,
    pub bits: usize,
    #[serde(default)]
    pub exposure: crate::decode::Exposure,
}

#[derive(Clone, Debug, Serialize)]
pub struct IngestReport {
    pub crop: Rect,
    pub saturated_raw: usize,
    pub saturated_dilated: usize,
    pub defects: usize,
    /// Rows (crop-relative) whose defect count exceeds 50x the mean row count.
    pub defect_hot_rows: Vec<usize>,
    pub min_norm: f64,
    pub max_norm: f64,
    /// Multipliers R,G,B actually applied (G = 1).
    pub wb: [f64; 3],
    pub wb_source: String,
}

/// Cropped, normalised, channel-balanced mosaic plus everything downstream
/// stages need.
#[derive(Clone, Debug)]
pub struct Mosaic {
    pub plane: Plane,
    /// Phase at the crop origin; `None` for monochrome sensors.
    pub phase: Option<BayerPhase>,
    /// 1 where saturated (after dilation), else 0. Same size as `plane`.
    pub saturated: Vec<u8>,
    /// Noise model in raw normalised units (before balance).
    pub noise: NoiseModel,
    /// Multipliers R,G,B applied to `plane` (G = 1).
    pub wb: [f64; 3],
    pub orientation: u16,
    pub camera: CameraInfo,
    pub xyz_to_cam: Option<[[f64; 3]; 3]>,
    pub report: IngestReport,
}

impl Mosaic {
    /// Noise variance of a balanced pixel of colour `c` at balanced level `s`.
    #[inline]
    pub fn balanced_variance(&self, c: Color, s: f64) -> f64 {
        let k = self.wb[c as usize];
        k * k * self.noise.variance(s / k)
    }

    /// Mean noise variance of the balanced mosaic inside a block whose
    /// per-colour mean levels are `levels` (R, G, B, balanced units); the
    /// per-bin noise floor for block spectra. The red and blue multipliers
    /// make their variance dominate, so one pooled level is not good enough.
    pub fn mosaic_variance(&self, levels: [f64; 3]) -> f64 {
        match self.phase {
            Some(_) => {
                0.25 * self.balanced_variance(Color::R, levels[0])
                    + 0.5 * self.balanced_variance(Color::G, levels[1])
                    + 0.25 * self.balanced_variance(Color::B, levels[2])
            }
            None => self.noise.variance(levels[1]),
        }
    }

    /// Per-colour mean levels of the `n`×`n` block at (x0, y0); for a
    /// monochrome mosaic all three entries hold the block mean.
    pub fn block_levels(&self, x0: usize, y0: usize, n: usize) -> [f64; 3] {
        let mut sum = [0.0f64; 3];
        let mut cnt = [0usize; 3];
        match self.phase {
            Some(ph) => {
                for y in y0..y0 + n {
                    let row = &self.plane.row(y)[x0..x0 + n];
                    let c0 = ph.color_at(x0, y) as usize;
                    let c1 = ph.color_at(x0 + 1, y) as usize;
                    let (mut s0, mut s1) = (0.0, 0.0);
                    for pair in row.as_chunks::<2>().0 {
                        s0 += pair[0];
                        s1 += pair[1];
                    }
                    sum[c0] += s0;
                    cnt[c0] += n / 2;
                    sum[c1] += s1;
                    cnt[c1] += n / 2;
                }
                let mut out = [0.0; 3];
                for c in 0..3 {
                    out[c] = if cnt[c] > 0 { sum[c] / cnt[c] as f64 } else { 0.0 };
                }
                out
            }
            None => {
                let mut s = 0.0;
                for y in y0..y0 + n {
                    s += self.plane.row(y)[x0..x0 + n].iter().sum::<f64>();
                }
                [s / (n * n) as f64; 3]
            }
        }
    }
}

/// Multiply each CFA position by its channel multiplier in place.
pub fn apply_balance(plane: &mut Plane, phase: BayerPhase, wb: [f64; 3]) {
    let w = plane.width;
    plane.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let k0 = wb[phase.color_at(0, y) as usize];
        let k1 = wb[phase.color_at(1, y) as usize];
        for (x, v) in row.iter_mut().enumerate() {
            *v *= if x & 1 == 0 { k0 } else { k1 };
        }
    });
}

/// Gray-world multipliers: `mean_G / mean_c` over unsaturated pixels.
pub fn gray_world(plane: &Plane, phase: BayerPhase, saturated: &[u8]) -> [f64; 3] {
    let w = plane.width;
    // Per-row partial sums in parallel, combined in row order: the result does
    // not depend on how rayon splits the work.
    let rows: Vec<([f64; 3], [usize; 3])> = plane
        .data
        .par_chunks(w)
        .zip(saturated.par_chunks(w))
        .enumerate()
        .map(|(y, (row, srow))| {
            let mut s = [0.0f64; 3];
            let mut n = [0usize; 3];
            for x in 0..w {
                if srow[x] == 0 {
                    let c = phase.color_at(x, y) as usize;
                    s[c] += row[x];
                    n[c] += 1;
                }
            }
            (s, n)
        })
        .collect();
    let (mut sum, mut cnt) = ([0.0f64; 3], [0usize; 3]);
    for (s, n) in rows {
        for c in 0..3 {
            sum[c] += s[c];
            cnt[c] += n[c];
        }
    }
    let mean = |c: usize| if cnt[c] > 0 { sum[c] / cnt[c] as f64 } else { 0.0 };
    let (r, g, b) = (mean(0), mean(1), mean(2));
    if r <= 0.0 || g <= 0.0 || b <= 0.0 {
        return [1.0; 3];
    }
    [g / r, 1.0, g / b]
}

fn normalise_wb(wb: [f64; 3]) -> [f64; 3] {
    if wb[1] <= 0.0 || !wb.iter().all(|v| v.is_finite() && *v > 0.0) {
        return [1.0; 3];
    }
    [wb[0] / wb[1], 1.0, wb[2] / wb[1]]
}

/// Shift an odd crop origin inward by one pixel so the phase is unchanged.
pub fn even_crop(c: Rect) -> Rect {
    let mut r = c;
    if r.x % 2 == 1 {
        r.x += 1;
        r.w = r.w.saturating_sub(1);
    }
    if r.y % 2 == 1 {
        r.y += 1;
        r.h = r.h.saturating_sub(1);
    }
    r
}

pub fn ingest(frame: &RawFrame, p: &IngestParams) -> Mosaic {
    let crop = even_crop(frame.crop);
    let (w, h) = (crop.w, crop.h);
    let phase = match frame.kind {
        SensorKind::Bayer(ph) => Some(ph.shifted(crop.x, crop.y)),
        SensorKind::Mono => None,
    };

    // Normalise and flag raw saturation in one pass.
    let mut norm = vec![0.0f64; w * h];
    let mut sat_raw = vec![0u8; w * h];
    let sat_raw_count: usize = norm
        .par_chunks_mut(w)
        .zip(sat_raw.par_chunks_mut(w))
        .enumerate()
        .map(|(yy, (row, srow))| {
            let y = crop.y + yy;
            let mut count = 0usize;
            for (xx, (v, s)) in row.iter_mut().zip(srow.iter_mut()).enumerate() {
                let x = crop.x + xx;
                let raw = frame.data.get(y * frame.width + x);
                let black = frame.black_at(x, y);
                let white = frame.white_at(x, y);
                *v = (raw - black) / (white - black);
                if raw >= white * (1.0 - p.sat_margin) {
                    *s = 1;
                    count += 1;
                }
            }
            count
        })
        .sum();

    let saturated = dilate(&sat_raw, w, h, p.sat_dilate);
    let sat_dilated_count = saturated.iter().filter(|&&s| s != 0).count();

    let step = if phase.is_some() { 2usize } else { 1usize };
    let (plane_data, defects, hot_rows) =
        if p.fix_defects { repair_defects(&norm, &saturated, w, h, step, p) } else { (norm, 0, Vec::new()) };

    let (min_norm, max_norm) = plane_data
        .par_iter()
        .fold(|| (f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), &v| (lo.min(v), hi.max(v)))
        .reduce(|| (f64::INFINITY, f64::NEG_INFINITY), |a, b| (a.0.min(b.0), a.1.max(b.1)));

    let mut plane = Plane::from_vec(w, h, plane_data);
    let (wb, wb_source) = match (phase, p.wb) {
        (None, _) | (_, WhiteBalance::None) => ([1.0; 3], "none".to_string()),
        (Some(ph), WhiteBalance::AsShot) => match frame.wb_as_shot {
            Some(c) => (normalise_wb(c), "as-shot".to_string()),
            None => (gray_world(&plane, ph, &saturated), "gray-world (no as-shot)".to_string()),
        },
        (Some(ph), WhiteBalance::GrayWorld) => (gray_world(&plane, ph, &saturated), "gray-world".to_string()),
        (_, WhiteBalance::Manual(c)) => (normalise_wb(c), "manual".to_string()),
    };
    if let Some(ph) = phase {
        if wb != [1.0; 3] {
            apply_balance(&mut plane, ph, wb);
        }
    }

    Mosaic {
        plane,
        phase,
        saturated,
        noise: p.noise,
        wb,
        orientation: frame.orientation,
        camera: CameraInfo {
            make: frame.make.clone(),
            model: frame.model.clone(),
            clean_make: frame.clean_make.clone(),
            clean_model: frame.clean_model.clone(),
            bits: frame.bits,
            exposure: frame.exposure.clone(),
        },
        xyz_to_cam: frame.xyz_to_cam,
        report: IngestReport {
            crop,
            saturated_raw: sat_raw_count,
            saturated_dilated: sat_dilated_count,
            defects,
            defect_hot_rows: hot_rows,
            min_norm,
            max_norm,
            wb,
            wb_source,
        },
    }
}

/// Chebyshev dilation of a 0/1 mask by `r` pixels (separable max filter).
pub fn dilate(mask: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    if r == 0 {
        return mask.to_vec();
    }
    let mut tmp = vec![0u8; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &mask[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let lo = x.saturating_sub(r);
            let hi = (x + r).min(w - 1);
            *o = if src[lo..=hi].iter().any(|&v| v != 0) { 1 } else { 0 };
        }
    });
    let mut out = vec![0u8; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let lo = y.saturating_sub(r);
        let hi = (y + r).min(h - 1);
        for x in 0..w {
            row[x] = if (lo..=hi).any(|yy| tmp[yy * w + x] != 0) { 1 } else { 0 };
        }
    });
    out
}

fn median_of(v: &mut [f64]) -> f64 {
    v.sort_by(|a, b| a.partial_cmp(b).expect("no NaN in mosaic"));
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Same-phase neighbour median test (SPEC section 2, "Defekte").
fn repair_defects(
    src: &[f64],
    saturated: &[u8],
    w: usize,
    h: usize,
    step: usize,
    p: &IngestParams,
) -> (Vec<f64>, usize, Vec<usize>) {
    let mut out = vec![0.0f64; w * h];
    let row_counts: Vec<usize> = out
        .par_chunks_mut(w)
        .enumerate()
        .map(|(y, row)| {
            let mut count = 0usize;
            let mut buf = [0.0f64; 8];
            for x in 0..w {
                let v = src[y * w + x];
                if saturated[y * w + x] != 0 {
                    row[x] = v;
                    continue;
                }
                let mut n = 0usize;
                for dy in [-(step as isize), 0, step as isize] {
                    for dx in [-(step as isize), 0, step as isize] {
                        if dx == 0 && dy == 0 {
                            continue;
                        }
                        let nx = x as isize + dx;
                        let ny = y as isize + dy;
                        if nx < 0 || ny < 0 || nx >= w as isize || ny >= h as isize {
                            continue;
                        }
                        let ni = ny as usize * w + nx as usize;
                        if saturated[ni] != 0 {
                            continue;
                        }
                        buf[n] = src[ni];
                        n += 1;
                    }
                }
                if n < 3 {
                    row[x] = v;
                    continue;
                }
                let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
                for &b in &buf[..n] {
                    lo = lo.min(b);
                    hi = hi.max(b);
                }
                // Outside the neighbour range by more than k sigma: an edge pixel
                // lies inside the range, an isolated outlier does not.
                // The neighbour spread (hi - lo) is a model-free noise scale, so the
                // rule stays valid when the file's ISO exceeds the noise defaults.
                let excursion = (v - hi).max(lo - v);
                if excursion <= 0.0 {
                    // Inside the range: can never exceed a non-negative threshold,
                    // and the median (a sort per pixel) is not needed.
                    row[x] = v;
                    continue;
                }
                let med = median_of(&mut buf[..n]);
                if excursion > (p.defect_sigma * p.noise.sigma(med)).max(hi - lo) {
                    row[x] = med;
                    count += 1;
                } else {
                    row[x] = v;
                }
            }
            count
        })
        .collect();
    let total: usize = row_counts.iter().sum();
    let mean = total as f64 / h.max(1) as f64;
    let hot: Vec<usize> = if total > 0 {
        row_counts
            .iter()
            .enumerate()
            .filter(|(_, &c)| c as f64 > 50.0 * mean && c > 8)
            .map(|(y, _)| y)
            .collect()
    } else {
        Vec::new()
    };
    (out, total, hot)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dilate_grows_by_radius() {
        let (w, h) = (7, 7);
        let mut m = vec![0u8; w * h];
        m[3 * w + 3] = 1;
        let d = dilate(&m, w, h, 2);
        assert_eq!(d.iter().filter(|&&v| v == 1).count(), 25);
        assert_eq!(d[0], 0);
        assert_eq!(d[w + 1], 1);
    }

    #[test]
    fn even_crop_keeps_phase() {
        let c = even_crop(Rect { x: 7, y: 4, w: 100, h: 50 });
        assert_eq!((c.x, c.y, c.w, c.h), (8, 4, 99, 50));
    }

    #[test]
    fn hot_pixel_is_repaired() {
        let (w, h) = (9, 9);
        let mut v = vec![0.2f64; w * h];
        v[4 * w + 4] = 0.9;
        let sat = vec![0u8; w * h];
        let p = IngestParams::default();
        let (out, n, _) = repair_defects(&v, &sat, w, h, 2, &p);
        assert_eq!(n, 1);
        assert!((out[4 * w + 4] - 0.2).abs() < 1e-12);
    }
}
