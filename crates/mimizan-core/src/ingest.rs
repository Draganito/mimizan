//! Normalisation, saturation mask, defect repair, channel balance: the raw
//! frame becomes a mosaic the separation can read.

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
    /// Noise standard deviation at level `s`.
    #[inline]
    pub fn sigma(&self, s: f64) -> f64 {
        self.a * s.max(0.0).sqrt() + self.b
    }
    /// Noise variance at level `s`.
    #[inline]
    pub fn variance(&self, s: f64) -> f64 {
        let v = self.sigma(s);
        v * v
    }
}

/// Channel balance applied before separation.
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

/// Settings of the ingest stage.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct IngestParams {
    /// A raw sample at or above `white · (1 − sat_margin)` counts as saturated.
    pub sat_margin: f64,
    /// Radius, in pixels, the saturation mask grows by.
    pub sat_dilate: usize,
    /// A sample this many sigma outside its same-colour neighbours is a defect.
    pub defect_sigma: f64,
    /// Run the defect repair at all.
    pub fix_defects: bool,
    /// Noise model in normalised raw units, before balance.
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

/// What the file says about the camera, carried into the negative's description.
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

/// Counts and settings the ingest stage reports into the description.
#[derive(Clone, Debug, Serialize)]
pub struct IngestReport {
    pub crop: Rect,
    /// Photosites at or above the saturation level.
    pub saturated_raw: usize,
    /// Photosites inside the dilated saturation mask.
    pub saturated_dilated: usize,
    /// Photosites the defect repair replaced.
    pub defects: usize,
    /// Rows (crop-relative) with more than eight defects and more than 50
    /// times the mean row count.
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
    /// monochrome mosaic all three entries hold the block mean. `n` must be
    /// even and the block must lie inside the plane.
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

/// Crop, normalise to `[0, 1]` between black and white, mask saturation,
/// repair defects, and balance the channels. The crop must lie inside the
/// frame and every white level must be above its black level; a frame that
/// does not is an error, not a panic.
pub fn ingest(frame: &RawFrame, p: &IngestParams) -> crate::Result<Mosaic> {
    // A Bayer crop with an odd origin would move the phase. A monochrome
    // sensor has no phase, so every pixel of the file's crop stays.
    let crop = match frame.kind {
        SensorKind::Bayer(_) => even_crop(frame.crop),
        SensorKind::Mono => frame.crop,
    };
    let (w, h) = (crop.w, crop.h);
    let fits = w > 0
        && h > 0
        && crop.x.checked_add(w).is_some_and(|x1| x1 <= frame.width)
        && crop.y.checked_add(h).is_some_and(|y1| y1 <= frame.height)
        && frame.data.len() == frame.width * frame.height;
    if !fits {
        return Err(crate::Error::Invalid(format!(
            "crop {}x{} at {},{} does not fit a {}x{} frame",
            w, h, crop.x, crop.y, frame.width, frame.height
        )));
    }
    let levels_ok = frame.black_tile.iter().all(|b| b.is_finite())
        && frame.white_rgb.iter().all(|wl| wl.is_finite())
        && frame.black_tile.iter().all(|b| frame.white_rgb.iter().all(|wl| wl > b));
    if !levels_ok {
        return Err(crate::Error::Invalid(format!(
            "white level {:?} is not above black level {:?}",
            frame.white_rgb, frame.black_tile
        )));
    }
    let phase = match frame.kind {
        SensorKind::Bayer(ph) => Some(ph.shifted(crop.x, crop.y)),
        SensorKind::Mono => None,
    };

    // Normalise, flag raw saturation, and check for a non-finite sample in
    // one pass. A NaN would fail the sort in the defect median.
    let mut norm = vec![0.0f64; w * h];
    let mut sat_raw = vec![0u8; w * h];
    let (sat_raw_count, finite) = norm
        .par_chunks_mut(w)
        .zip(sat_raw.par_chunks_mut(w))
        .enumerate()
        .map(|(yy, (row, srow))| {
            let y = crop.y + yy;
            let mut count = 0usize;
            let mut finite = true;
            for (xx, (v, s)) in row.iter_mut().zip(srow.iter_mut()).enumerate() {
                let x = crop.x + xx;
                let raw = frame.data.get(y * frame.width + x);
                let black = frame.black_at(x, y);
                let white = frame.white_at(x, y);
                *v = (raw - black) / (white - black);
                finite &= v.is_finite();
                if raw >= white * (1.0 - p.sat_margin) {
                    *s = 1;
                    count += 1;
                }
            }
            (count, finite)
        })
        .reduce(|| (0, true), |a, b| (a.0 + b.0, a.1 && b.1));
    if !finite {
        return Err(crate::Error::Invalid("a sensor sample is not a number".into()));
    }

    let (saturated, sat_dilated_count) = dilate_counted(&sat_raw, w, h, p.sat_dilate);
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

    Ok(Mosaic {
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
    })
}

/// Chebyshev dilation of a 0/1 mask by `r` pixels (separable max filter).
/// `w` and `h` must be positive and `mask` must hold `w · h` entries.
#[cfg(test)]
fn dilate(mask: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    dilate_counted(mask, w, h, r).0
}

/// [`dilate`] and the number of set pixels afterwards, counted in the same
/// pass.
fn dilate_counted(mask: &[u8], w: usize, h: usize, r: usize) -> (Vec<u8>, usize) {
    if r == 0 {
        return (mask.to_vec(), mask.iter().filter(|&&s| s != 0).count());
    }
    let mut tmp = vec![0u8; w * h];
    tmp.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let src = &mask[y * w..(y + 1) * w];
        for (x, o) in row.iter_mut().enumerate() {
            let lo = x.saturating_sub(r);
            let hi = (x + r).min(w - 1);
            *o = u8::from(src[lo..=hi].iter().any(|&v| v != 0));
        }
    });
    let mut out = vec![0u8; w * h];
    let count = out
        .par_chunks_mut(w)
        .enumerate()
        .map(|(y, row)| {
            let lo = y.saturating_sub(r);
            let hi = (y + r).min(h - 1);
            let mut count = 0usize;
            for (x, o) in row.iter_mut().enumerate() {
                let set = (lo..=hi).any(|yy| tmp[yy * w + x] != 0);
                *o = u8::from(set);
                count += usize::from(set);
            }
            count
        })
        .sum();
    (out, count)
}

fn median_of(v: &mut [f64]) -> f64 {
    // Ingest refuses a NaN before repair. A later caller still must not abort.
    v.sort_by(f64::total_cmp);
    let n = v.len();
    if n % 2 == 1 {
        v[n / 2]
    } else {
        0.5 * (v[n / 2 - 1] + v[n / 2])
    }
}

/// Same-phase neighbour median test: a sample far outside the range of its
/// eight same-colour neighbours is replaced by their median.
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

    fn frame(w: usize, h: usize, kind: SensorKind, white: f64, crop: Rect, raw: Vec<u16>) -> RawFrame {
        RawFrame {
            make: String::new(),
            model: String::new(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: w,
            height: h,
            bits: 12,
            kind,
            black_tile: [0.0; 4],
            white_rgb: [white; 3],
            crop,
            orientation: 1,
            xyz_to_cam: None,
            wb_as_shot: None,
            exposure: Default::default(),
            data: crate::decode::RawData::Integer(raw),
        }
    }

    /// White at or below black, a crop outside the frame, or a non-finite
    /// sample are errors, not panics or an infinite negative.
    #[test]
    fn a_bad_frame_is_an_error() {
        let (w, h) = (4usize, 4usize);
        let full = Rect { x: 0, y: 0, w, h };
        let err = ingest(&frame(w, h, SensorKind::Mono, 0.0, full, vec![0; w * h]), &IngestParams::default())
            .unwrap_err();
        assert!(err.to_string().contains("white level"), "{err}");
        let outside = Rect { x: 2, y: 0, w: 4, h };
        let err =
            ingest(&frame(w, h, SensorKind::Mono, 1000.0, outside, vec![0; w * h]), &IngestParams::default())
                .unwrap_err();
        assert!(err.to_string().contains("does not fit"), "{err}");
        let short = frame(w, h, SensorKind::Mono, 1000.0, full, vec![0; w * h - 1]);
        assert!(ingest(&short, &IngestParams::default()).is_err());
        // A one-pixel Bayer crop at an odd origin shifts to an empty crop.
        let sliver = Rect { x: 1, y: 0, w: 1, h };
        let err = ingest(
            &frame(w, h, SensorKind::Bayer(BayerPhase::RGGB), 1000.0, sliver, vec![0; w * h]),
            &IngestParams::default(),
        )
        .unwrap_err();
        assert!(err.to_string().contains("does not fit"), "{err}");
        let mut f = frame(w, h, SensorKind::Mono, 1000.0, full, Vec::new());
        let mut data = vec![0.0f32; w * h];
        data[5] = f32::NAN;
        f.data = crate::decode::RawData::Float(data);
        let err = ingest(&f, &IngestParams::default()).unwrap_err();
        assert!(err.to_string().contains("not a number"), "{err}");
    }

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

    /// A monochrome crop keeps an odd origin. Bayer still shifts it.
    #[test]
    fn mono_keeps_an_odd_crop_origin() {
        let (w, h) = (8usize, 8usize);
        let mut raw = vec![0u16; w * h];
        raw[w + 1] = 500;
        let frame = |kind| frame(w, h, kind, 1000.0, Rect { x: 1, y: 1, w: 5, h: 5 }, raw.clone());
        let p = IngestParams { fix_defects: false, ..IngestParams::default() };
        let mono = ingest(&frame(SensorKind::Mono), &p).unwrap();
        assert_eq!((mono.plane.width, mono.plane.height), (5, 5));
        assert!((mono.plane.at(0, 0) - 0.5).abs() < 1e-12);
        let bayer = ingest(&frame(SensorKind::Bayer(BayerPhase::RGGB)), &p).unwrap();
        assert_eq!((bayer.plane.width, bayer.plane.height), (4, 4));
        assert!(bayer.plane.at(0, 0).abs() < 1e-12);
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
