//! Separable Lanczos-3 resampling in f64 (SPEC section 9, "Größe").
//!
//! When shrinking, the kernel is stretched by the scale factor so it acts as
//! the anti-alias low-pass at the same time (the standard "support scales with
//! the footprint" rule). Weights are normalised per output sample so the DC
//! gain is exactly one everywhere, including at the borders (which are
//! handled by clamping the source index).

use crate::plane::Plane;
use rayon::prelude::*;
use std::f64::consts::PI;

const A: f64 = 3.0;

#[inline]
fn lanczos3(x: f64) -> f64 {
    let x = x.abs();
    if x < 1e-12 {
        1.0
    } else if x >= A {
        0.0
    } else {
        let px = PI * x;
        A * px.sin() * (px / A).sin() / (px * px)
    }
}

/// Per-output-sample taps: (first source index, weights).
struct Taps {
    start: Vec<usize>,
    weights: Vec<Vec<f64>>,
}

fn build_taps(src_len: usize, dst_len: usize) -> Taps {
    let scale = src_len as f64 / dst_len as f64; // > 1 when shrinking
    let stretch = scale.max(1.0);
    let support = A * stretch;
    let mut start = Vec::with_capacity(dst_len);
    let mut weights = Vec::with_capacity(dst_len);
    for i in 0..dst_len {
        // Centre of destination pixel i mapped into source coordinates.
        let centre = (i as f64 + 0.5) * scale - 0.5;
        let lo = (centre - support).floor() as isize;
        let hi = (centre + support).ceil() as isize;
        let mut w = Vec::with_capacity((hi - lo + 1) as usize);
        let mut sum = 0.0;
        for j in lo..=hi {
            let v = lanczos3((j as f64 - centre) / stretch);
            w.push(v);
            sum += v;
        }
        for v in &mut w {
            *v /= sum;
        }
        // Clamp indices at the borders by folding the weights onto the edge sample.
        let first = lo.max(0) as usize;
        let last = (hi.min(src_len as isize - 1)) as usize;
        let mut folded = vec![0.0; last - first + 1];
        for (k, j) in (lo..=hi).enumerate() {
            let jc = j.clamp(0, src_len as isize - 1) as usize;
            folded[jc - first] += w[k];
        }
        start.push(first);
        weights.push(folded);
    }
    Taps { start, weights }
}

fn resize_rows(src: &Plane, dst_w: usize) -> Plane {
    let taps = build_taps(src.width, dst_w);
    let mut out = Plane::zeros(dst_w, src.height);
    out.data.par_chunks_mut(dst_w).enumerate().for_each(|(y, row)| {
        let s = src.row(y);
        for (x, o) in row.iter_mut().enumerate() {
            let st = taps.start[x];
            let w = &taps.weights[x];
            let mut acc = 0.0;
            for (k, wk) in w.iter().enumerate() {
                acc += wk * s[st + k];
            }
            *o = acc;
        }
    });
    out
}

fn resize_cols(src: &Plane, dst_h: usize) -> Plane {
    let taps = build_taps(src.height, dst_h);
    let w = src.width;
    let mut out = Plane::zeros(w, dst_h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let st = taps.start[y];
        let wts = &taps.weights[y];
        row.iter_mut().for_each(|o| *o = 0.0);
        for (k, wk) in wts.iter().enumerate() {
            let s = src.row(st + k);
            for x in 0..w {
                row[x] += wk * s[x];
            }
        }
    });
    out
}

/// Resample to exactly `dst_w × dst_h`. Rows first when shrinking in x more
/// than in y (fewer operations), otherwise columns first.
pub fn resize(src: &Plane, dst_w: usize, dst_h: usize) -> Plane {
    assert!(dst_w > 0 && dst_h > 0, "target size must be positive");
    if dst_w == src.width && dst_h == src.height {
        return src.clone();
    }
    let cost_rows_first = src.height * dst_w + dst_w * dst_h;
    let cost_cols_first = src.width * dst_h + dst_w * dst_h;
    if cost_rows_first <= cost_cols_first {
        let t = if dst_w != src.width { resize_rows(src, dst_w) } else { src.clone() };
        if dst_h != t.height {
            resize_cols(&t, dst_h)
        } else {
            t
        }
    } else {
        let t = if dst_h != src.height { resize_cols(src, dst_h) } else { src.clone() };
        if dst_w != t.width {
            resize_rows(&t, dst_w)
        } else {
            t
        }
    }
}

/// Pixel size that fits `(w, h)` into `(box_w, box_h)` keeping the aspect
/// ratio; the box is rotated to match the image orientation.
pub fn fit_dims(w: usize, h: usize, box_w: usize, box_h: usize) -> (usize, usize) {
    let (bw, bh) = if (w > h) != (box_w > box_h) { (box_h, box_w) } else { (box_w, box_h) };
    let s = (bw as f64 / w as f64).min(bh as f64 / h as f64);
    let dw = ((w as f64 * s).round() as usize).clamp(1, bw.max(1));
    let dh = ((h as f64 * s).round() as usize).clamp(1, bh.max(1));
    (dw, dh)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flat_stays_flat_everywhere() {
        let src = Plane::from_vec(100, 60, vec![0.37; 6000]);
        for (w, h) in [(50, 30), (33, 17), (200, 120), (7, 90)] {
            let out = resize(&src, w, h);
            for v in &out.data {
                assert!((v - 0.37).abs() < 1e-12, "{v}");
            }
        }
    }

    #[test]
    fn identity_and_fit() {
        let src = Plane::from_vec(10, 5, (0..50).map(|i| i as f64 / 50.0).collect());
        let same = resize(&src, 10, 5);
        assert_eq!(same.data, src.data);
        // The paper box is rotated to the image orientation.
        assert_eq!(fit_dims(6000, 4000, 3000, 4500), (4500, 3000));
        assert_eq!(fit_dims(4000, 6000, 3000, 4500), (3000, 4500));
        assert_eq!(fit_dims(6000, 4000, 4500, 3000), (4500, 3000));
        // 3:2 image on 4:5 paper: the long side limits.
        assert_eq!(fit_dims(6000, 4000, 2400, 3000), (3000, 2000));
    }

    #[test]
    fn downscale_low_frequency_ramp_is_preserved() {
        let (w, h) = (256, 64);
        let mut src = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                src.set(x, y, 0.1 + 0.8 * x as f64 / (w - 1) as f64);
            }
        }
        let out = resize(&src, 64, 16);
        for y in 0..16 {
            for x in 4..60 {
                let expected = 0.1 + 0.8 * ((x as f64 + 0.5) * 4.0 - 0.5) / (w - 1) as f64;
                assert!((out.at(x, y) - expected).abs() < 2e-3, "{} vs {expected}", out.at(x, y));
            }
        }
    }

    #[test]
    fn downscale_kills_nyquist_checkerboard() {
        let (w, h) = (128, 128);
        let mut src = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                src.set(x, y, if (x + y) % 2 == 0 { 0.9 } else { 0.1 });
            }
        }
        let out = resize(&src, 32, 32);
        for y in 4..28 {
            for x in 4..28 {
                assert!((out.at(x, y) - 0.5).abs() < 1e-3, "{}", out.at(x, y));
            }
        }
    }
}
