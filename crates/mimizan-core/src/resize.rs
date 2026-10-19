//! Separable resampling in f64 (SPEC section 9, "Größe").
//!
//! The kernel is a parameter ([`ResampleKernel`]). The print default is the
//! B-spline; [`resize`] still uses Lanczos-3, which is the bench kernel. When shrinking, the kernel is stretched by the
//! scale factor so it acts as the anti-alias low-pass at the same time (the
//! standard "support scales with the footprint" rule). Weights are normalised
//! per output sample so the DC gain is exactly one everywhere, including at
//! the borders (which are handled by clamping the source index).
//!
//! There is no kernel that is both sharp and free of overshoot. Every
//! interpolating kernel smoother than linear has negative lobes and rings at a
//! hard edge; the only member of the cubic family without them is the
//! B-spline, and it halves the MTF at 0.4 c/px. The step overshoot and the
//! MTF of each kernel were measured on the bench; the
//! choice is recorded in the print's description.

use crate::plane::Plane;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// Resampling kernel of the print stage.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ResampleKernel {
    /// Windowed sinc, support 3: the sharpest (MTF 0.82 at 0.4 c/px), about
    /// 6 % overshoot at a step. The bench's kernel.
    Lanczos3,
    /// Mitchell–Netravali B = C = 1/3: the compromise of that paper, 1.6 %
    /// overshoot, MTF 0.59 at 0.4 c/px.
    Mitchell,
    /// Keys cubic (B = 0, C = 1/2): interpolating, 2.5 % overshoot, MTF 0.72
    /// at 0.4 c/px.
    CatmullRom,
    /// Cubic B-spline (B = 1, C = 0): no negative lobe, no overshoot at any
    /// edge, MTF 0.33 at 0.4 c/px. Not interpolating (it smooths at scale 1).
    /// The print default.
    #[default]
    BSpline,
}

impl ResampleKernel {
    pub const ALL: [ResampleKernel; 4] = [Self::Lanczos3, Self::Mitchell, Self::CatmullRom, Self::BSpline];

    /// Name as it appears in the description JSON and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Lanczos3 => "lanczos3",
            Self::Mitchell => "mitchell",
            Self::CatmullRom => "catmull-rom",
            Self::BSpline => "b-spline",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim().to_ascii_lowercase();
        Self::ALL.iter().copied().find(|k| k.name() == t || k.name().replace('-', "") == t.replace('-', ""))
    }

    /// Gaussian σ of this kernel's ramp when it enlarges, in destination
    /// pixels per unit of enlargement. A step scaled by `s` matches a
    /// Gaussian blur of the nearest-neighbour step at `σ = k · s`, fitted on
    /// the 10–90 % width at `s = 8`. Lanczos and Catmull–Rom are the
    /// narrowest, the B-spline the widest.
    pub fn enlargement_k(self) -> f64 {
        match self {
            Self::Lanczos3 => 0.281,
            Self::Mitchell => 0.330,
            Self::CatmullRom => 0.275,
            Self::BSpline => 0.515,
        }
    }

    /// Half-width of the support in source pixels (at scale 1).
    pub fn support(self) -> f64 {
        match self {
            Self::Lanczos3 => 3.0,
            Self::Mitchell | Self::CatmullRom | Self::BSpline => 2.0,
        }
    }

    /// Kernel value at `x` (source pixels, scale 1). Not normalised.
    #[inline]
    pub fn eval(self, x: f64) -> f64 {
        match self {
            Self::Lanczos3 => lanczos3(x),
            Self::Mitchell => mitchell_netravali(x, 1.0 / 3.0, 1.0 / 3.0),
            Self::CatmullRom => mitchell_netravali(x, 0.0, 0.5),
            Self::BSpline => mitchell_netravali(x, 1.0, 0.0),
        }
    }

    /// Continuous frequency response in output pixel units, DC = 1 (numeric
    /// integration; for reports).
    pub fn mtf(self, f: f64) -> f64 {
        const N: usize = 1201;
        let a = self.support();
        let dx = 2.0 * a / (N - 1) as f64;
        let mut num = 0.0;
        let mut den = 0.0;
        for i in 0..N {
            let x = -a + i as f64 * dx;
            let k = self.eval(x);
            num += k * (2.0 * PI * f * x).cos();
            den += k;
        }
        num / den
    }
}

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

/// Mitchell–Netravali cubic family (B, C); support 2.
#[inline]
fn mitchell_netravali(x: f64, b: f64, c: f64) -> f64 {
    let x = x.abs();
    if x < 1.0 {
        ((12.0 - 9.0 * b - 6.0 * c) * x * x * x + (-18.0 + 12.0 * b + 6.0 * c) * x * x + (6.0 - 2.0 * b))
            / 6.0
    } else if x < 2.0 {
        ((-b - 6.0 * c) * x * x * x
            + (6.0 * b + 30.0 * c) * x * x
            + (-12.0 * b - 48.0 * c) * x
            + (8.0 * b + 24.0 * c))
            / 6.0
    } else {
        0.0
    }
}

/// Per-output-sample taps: (first source index, weights).
struct Taps {
    start: Vec<usize>,
    weights: Vec<Vec<f64>>,
}

fn build_taps(src_len: usize, dst_len: usize, kernel: ResampleKernel) -> Taps {
    taps_range(src_len, dst_len, kernel, 0, dst_len)
}

/// Taps for destination samples `i0..i1` only. The weights are those of a
/// resize of the full length, so a window matches the same pixels of
/// [`resize_with`].
fn taps_range(src_len: usize, dst_len: usize, kernel: ResampleKernel, i0: usize, i1: usize) -> Taps {
    let scale = src_len as f64 / dst_len as f64; // > 1 when shrinking
    let stretch = scale.max(1.0);
    let support = kernel.support() * stretch;
    let n = i1.saturating_sub(i0);
    let mut start = Vec::with_capacity(n);
    let mut weights = Vec::with_capacity(n);
    for i in i0..i1 {
        // Centre of destination pixel i mapped into source coordinates.
        let centre = (i as f64 + 0.5) * scale - 0.5;
        let lo = (centre - support).floor() as isize;
        let hi = (centre + support).ceil() as isize;
        let mut w = Vec::with_capacity((hi - lo + 1) as usize);
        let mut sum = 0.0;
        for j in lo..=hi {
            let v = kernel.eval((j as f64 - centre) / stretch);
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

fn resize_rows(src: &Plane, dst_w: usize, kernel: ResampleKernel) -> Plane {
    let taps = build_taps(src.width, dst_w, kernel);
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

fn resize_cols(src: &Plane, dst_h: usize, kernel: ResampleKernel) -> Plane {
    let taps = build_taps(src.height, dst_h, kernel);
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

/// Resample to exactly `dst_w × dst_h` with Lanczos-3 (the default kernel;
/// the bench and the preview use this one).
pub fn resize(src: &Plane, dst_w: usize, dst_h: usize) -> Plane {
    resize_with(src, dst_w, dst_h, ResampleKernel::Lanczos3)
}

/// A rectangle of a larger plane. `plane` holds pixels `[origin_x, origin_y)`
/// of an image that is `full_w × full_h`. Resampling a window reads through
/// this, with the same weights as the full image.
#[derive(Clone, Copy)]
pub struct PlaneWindow<'a> {
    pub plane: &'a Plane,
    pub origin_x: usize,
    pub origin_y: usize,
    pub full_w: usize,
    pub full_h: usize,
}

impl<'a> PlaneWindow<'a> {
    /// The whole plane, origin at 0.
    pub fn full(plane: &'a Plane) -> Self {
        Self { plane, origin_x: 0, origin_y: 0, full_w: plane.width, full_h: plane.height }
    }

    #[inline]
    fn at(self, x: usize, y: usize) -> f64 {
        self.plane.at(x - self.origin_x, y - self.origin_y)
    }
}

/// A rectangle in pixel indices.
#[derive(Clone, Copy, Debug)]
pub struct PixelRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// Source pixels a resize of the full image reads for `rect`. An axis that
/// is not resampled (`src == dst`) reads the destination index itself. The
/// span is what a crop must cover for [`resize_rect`] to match [`resize_with`].
pub fn source_coverage(
    full_w: usize,
    full_h: usize,
    dst_w: usize,
    dst_h: usize,
    kernel: ResampleKernel,
    rect: PixelRect,
) -> (usize, usize, usize, usize) {
    let (x0, x1) = axis_span(full_w, dst_w, kernel, rect.x, rect.x + rect.w);
    let (y0, y1) = axis_span(full_h, dst_h, kernel, rect.y, rect.y + rect.h);
    (x0, y0, x1, y1)
}

fn axis_span(src_len: usize, dst_len: usize, kernel: ResampleKernel, i0: usize, i1: usize) -> (usize, usize) {
    if i0 >= i1 {
        return (i0, i0);
    }
    if src_len == dst_len {
        return (i0, i1);
    }
    let taps = taps_range(src_len, dst_len, kernel, i0, i1);
    let mut a = usize::MAX;
    let mut b = 0usize;
    for (s, w) in taps.start.iter().zip(&taps.weights) {
        a = a.min(*s);
        b = b.max(s + w.len());
    }
    (a, b)
}

/// Destination pixels `[dx, dx+dw) × [dy, dy+dh)` of [`resize_with`] on the
/// full image. `src` must cover [`source_coverage`] of that rectangle.
pub fn resize_rect(
    src: PlaneWindow<'_>,
    dst_w: usize,
    dst_h: usize,
    kernel: ResampleKernel,
    rect: PixelRect,
) -> crate::Result<Plane> {
    let (dx, dy, dw, dh) = (rect.x, rect.y, rect.w, rect.h);
    if dw == 0 || dh == 0 {
        return Err(crate::Error::Invalid("resize window is empty".into()));
    }
    let samples = src.plane.width.checked_mul(src.plane.height).ok_or_else(|| {
        crate::Error::Invalid(format!("plane {}×{} does not fit", src.plane.width, src.plane.height))
    })?;
    if src.plane.data.len() != samples {
        return Err(crate::Error::Invalid(format!(
            "plane is {}×{} but has {} samples",
            src.plane.width,
            src.plane.height,
            src.plane.data.len()
        )));
    }
    if dst_w == src.full_w && dst_h == src.full_h {
        let mut out = Plane::zeros(dw, dh);
        for y in 0..dh {
            for x in 0..dw {
                out.set(x, y, src.at(dx + x, dy + y));
            }
        }
        return Ok(out);
    }
    let rows_first = src.full_h * dst_w + dst_w * dst_h <= src.full_w * dst_h + dst_w * dst_h;
    Ok(if rows_first {
        let (sy0, sy1) = axis_span(src.full_h, dst_h, kernel, dy, dy + dh);
        let mid = resize_rows_window(src, dst_w, kernel, dx, dx + dw, sy0, sy1);
        if dst_h == src.full_h {
            mid
        } else {
            resize_cols_partial(&mid, sy0, src.full_h, dst_h, kernel, dy, dy + dh)
        }
    } else {
        let (sx0, sx1) = axis_span(src.full_w, dst_w, kernel, dx, dx + dw);
        let mid = resize_cols_window(src, dst_h, kernel, sx0, sx1, dy, dy + dh);
        if dst_w == src.full_w {
            mid
        } else {
            resize_rows_partial(&mid, sx0, src.full_w, dst_w, kernel, dx, dx + dw)
        }
    })
}

fn resize_rows_window(
    src: PlaneWindow<'_>,
    dst_w: usize,
    kernel: ResampleKernel,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
) -> Plane {
    let w = x1 - x0;
    let h = y1 - y0;
    let mut out = Plane::zeros(w, h);
    if dst_w == src.full_w {
        for y in 0..h {
            for x in 0..w {
                out.set(x, y, src.at(x0 + x, y0 + y));
            }
        }
        return out;
    }
    let taps = taps_range(src.full_w, dst_w, kernel, x0, x1);
    for y in 0..h {
        let row = out.row_mut(y);
        let sy = y0 + y;
        for (x, o) in row.iter_mut().enumerate() {
            let st = taps.start[x];
            let weights = &taps.weights[x];
            let mut acc = 0.0;
            for (k, wk) in weights.iter().enumerate() {
                acc += wk * src.at(st + k, sy);
            }
            *o = acc;
        }
    }
    out
}

fn resize_cols_window(
    src: PlaneWindow<'_>,
    dst_h: usize,
    kernel: ResampleKernel,
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
) -> Plane {
    let w = x1 - x0;
    let h = y1 - y0;
    let mut out = Plane::zeros(w, h);
    if dst_h == src.full_h {
        for y in 0..h {
            for x in 0..w {
                out.set(x, y, src.at(x0 + x, y0 + y));
            }
        }
        return out;
    }
    let taps = taps_range(src.full_h, dst_h, kernel, y0, y1);
    for (i, row) in out.data.chunks_mut(w).enumerate() {
        let st = taps.start[i];
        let weights = &taps.weights[i];
        for (k, wk) in weights.iter().enumerate() {
            let sy = st + k;
            for (x, o) in row.iter_mut().enumerate() {
                *o += wk * src.at(x0 + x, sy);
            }
        }
    }
    out
}

/// Vertical resize of a plane whose rows are absolute source rows `mid_y0..`.
fn resize_cols_partial(
    mid: &Plane,
    mid_y0: usize,
    src_h: usize,
    dst_h: usize,
    kernel: ResampleKernel,
    y0: usize,
    y1: usize,
) -> Plane {
    let taps = taps_range(src_h, dst_h, kernel, y0, y1);
    let w = mid.width;
    let mut out = Plane::zeros(w, y1 - y0);
    for (i, row) in out.data.chunks_mut(w).enumerate() {
        let st = taps.start[i];
        let weights = &taps.weights[i];
        for (k, wk) in weights.iter().enumerate() {
            let s = mid.row(st + k - mid_y0);
            for x in 0..w {
                row[x] += wk * s[x];
            }
        }
    }
    out
}

/// Horizontal resize of a plane whose columns are absolute source columns `mid_x0..`.
fn resize_rows_partial(
    mid: &Plane,
    mid_x0: usize,
    src_w: usize,
    dst_w: usize,
    kernel: ResampleKernel,
    x0: usize,
    x1: usize,
) -> Plane {
    let taps = taps_range(src_w, dst_w, kernel, x0, x1);
    let mut out = Plane::zeros(x1 - x0, mid.height);
    for y in 0..mid.height {
        let s = mid.row(y);
        let row = out.row_mut(y);
        for (x, o) in row.iter_mut().enumerate() {
            let st = taps.start[x];
            let weights = &taps.weights[x];
            let mut acc = 0.0;
            for (k, wk) in weights.iter().enumerate() {
                acc += wk * s[st + k - mid_x0];
            }
            *o = acc;
        }
    }
    out
}

/// Resample to exactly `dst_w × dst_h` with `kernel`. Rows first when
/// shrinking in x more than in y (fewer operations), otherwise columns first.
/// An unchanged size returns a copy, whatever the kernel (the B-spline would
/// otherwise smooth at scale 1).
pub fn resize_with(src: &Plane, dst_w: usize, dst_h: usize, kernel: ResampleKernel) -> Plane {
    assert!(dst_w > 0 && dst_h > 0, "target size must be positive");
    if dst_w == src.width && dst_h == src.height {
        return src.clone();
    }
    let cost_rows_first = src.height * dst_w + dst_w * dst_h;
    let cost_cols_first = src.width * dst_h + dst_w * dst_h;
    if cost_rows_first <= cost_cols_first {
        let t = if dst_w != src.width { resize_rows(src, dst_w, kernel) } else { src.clone() };
        if dst_h != t.height {
            resize_cols(&t, dst_h, kernel)
        } else {
            t
        }
    } else {
        let t = if dst_h != src.height { resize_cols(src, dst_h, kernel) } else { src.clone() };
        if dst_w != t.width {
            resize_rows(&t, dst_w, kernel)
        } else {
            t
        }
    }
}

/// Largest overshoot above 1 and undershoot below 0 when a unit step is
/// resampled by `scale` with `kernel` (a measurement for the reports: what
/// the kernel does at a hard edge).
pub fn step_overshoot(kernel: ResampleKernel, scale: f64) -> (f64, f64) {
    let n = 64;
    let mut src = Plane::zeros(n, 1);
    for x in n / 2..n {
        src.set(x, 0, 1.0);
    }
    let out = resize_with(&src, (n as f64 * scale).round() as usize, 1, kernel);
    let hi = out.data.iter().cloned().fold(0.0, f64::max) - 1.0;
    let lo = out.data.iter().cloned().fold(0.0, f64::min);
    (hi.max(0.0), lo.min(0.0))
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

/// Long-side scale `destination / source`. Greater than 1 when the picture
/// is enlarged.
pub fn enlargement_scale(src_w: usize, src_h: usize, dst_w: usize, dst_h: usize) -> f64 {
    let src = if src_w >= src_h { src_w } else { src_h };
    let dst = if src_w >= src_h { dst_w } else { dst_h };
    if src == 0 {
        1.0
    } else {
        dst as f64 / src as f64
    }
}

/// Blur the resampler adds when it enlarges, in destination pixels.
/// Zero at native size and when shrinking: there the picture is the sensor,
/// or the kernel is the anti-alias low-pass.
pub fn enlargement_sigma(
    src_w: usize,
    src_h: usize,
    dst_w: usize,
    dst_h: usize,
    kernel: ResampleKernel,
) -> f64 {
    let s = enlargement_scale(src_w, src_h, dst_w, dst_h);
    if s <= 1.0 {
        0.0
    } else {
        kernel.enlargement_k() * s
    }
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
    fn a_window_matches_the_full_resize() {
        let (w, h) = (41, 27);
        let mut src = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                src.set(x, y, ((x * 17 + y * 9) % 64) as f64 / 63.0);
            }
        }
        for kernel in ResampleKernel::ALL {
            for (dw, dh) in [(19, 13), (60, 40), (41, 14), (10, 27)] {
                let full = resize_with(&src, dw, dh, kernel);
                for (dx, dy, ww, hh) in [(0, 0, 5, 4), (dw - 6, dh - 5, 6, 5), (dw / 3, dh / 4, 7, 6)] {
                    let rect = PixelRect { x: dx, y: dy, w: ww, h: hh };
                    let got = resize_rect(PlaneWindow::full(&src), dw, dh, kernel, rect).unwrap();
                    for y in 0..hh {
                        for x in 0..ww {
                            assert_eq!(
                                got.at(x, y),
                                full.at(dx + x, dy + y),
                                "{kernel:?} {dw}x{dh} at {dx},{dy}"
                            );
                        }
                    }
                    let (sx, sy, ex, ey) = source_coverage(w, h, dw, dh, kernel, rect);
                    let mut crop = Plane::zeros(ex - sx, ey - sy);
                    for y in 0..crop.height {
                        for x in 0..crop.width {
                            crop.set(x, y, src.at(sx + x, sy + y));
                        }
                    }
                    let win = PlaneWindow { plane: &crop, origin_x: sx, origin_y: sy, full_w: w, full_h: h };
                    let from_crop = resize_rect(win, dw, dh, kernel, rect).unwrap();
                    assert_eq!(from_crop.data, got.data);
                }
            }
        }
    }

    #[test]
    fn an_empty_window_is_an_error() {
        let src = Plane::zeros(4, 4);
        let rect = PixelRect { x: 0, y: 0, w: 0, h: 2 };
        let err = resize_rect(PlaneWindow::full(&src), 4, 4, ResampleKernel::BSpline, rect).unwrap_err();
        assert!(err.to_string().contains("empty"), "{err}");
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
    fn every_kernel_keeps_flat_flat_and_dc_one() {
        let src = Plane::from_vec(100, 60, vec![0.37; 6000]);
        for k in ResampleKernel::ALL {
            for (w, h) in [(50, 30), (33, 17), (200, 120), (7, 90)] {
                let out = resize_with(&src, w, h, k);
                for v in &out.data {
                    assert!((v - 0.37).abs() < 1e-12, "{k:?} {v}");
                }
            }
            assert!((k.mtf(0.0) - 1.0).abs() < 1e-9, "{k:?}");
            assert_eq!(ResampleKernel::parse(k.name()), Some(k));
        }
        assert_eq!(ResampleKernel::parse("Catmull-Rom"), Some(ResampleKernel::CatmullRom));
        assert_eq!(ResampleKernel::parse("bspline"), Some(ResampleKernel::BSpline));
        assert_eq!(ResampleKernel::parse("nearest"), None);
    }

    /// The claim in the docs: the B-spline never overshoots, the others do,
    /// and the sharper the kernel the more.
    #[test]
    fn overshoot_orders_the_kernels() {
        let over = |k| {
            let (hi, lo) = step_overshoot(k, 1.37);
            hi.max(-lo)
        };
        // Zero up to the rounding of the normalised taps.
        for s in [1.37, 0.61, 2.0] {
            let (hi, lo) = step_overshoot(ResampleKernel::BSpline, s);
            assert!(hi < 1e-12 && lo > -1e-12, "b-spline at {s}: {hi} {lo}");
        }
        let (m, c, l) = (
            over(ResampleKernel::Mitchell),
            over(ResampleKernel::CatmullRom),
            over(ResampleKernel::Lanczos3),
        );
        assert!(m > 0.0 && m < c && c < l, "mitchell {m} catmull {c} lanczos {l}");
        assert!(l > 0.05 && l < 0.15, "{l}");
        // Sharpness follows the same order (MTF at 0.4 c/px).
        let mtf = |k: ResampleKernel| k.mtf(0.4);
        assert!(mtf(ResampleKernel::BSpline) < mtf(ResampleKernel::Mitchell));
        assert!(mtf(ResampleKernel::Mitchell) < mtf(ResampleKernel::CatmullRom));
        assert!(mtf(ResampleKernel::CatmullRom) < mtf(ResampleKernel::Lanczos3));
    }

    /// Interpolating kernels pass through the samples when the grid lines up
    /// (2× magnification puts every other output pixel on a source sample).
    #[test]
    fn interpolating_kernels_hit_the_samples_at_integer_scale() {
        let src = Plane::from_vec(16, 1, (0..16).map(|i| ((i * 7919) % 101) as f64 / 101.0).collect());
        for k in [ResampleKernel::Lanczos3, ResampleKernel::CatmullRom] {
            // Output pixel 2i+0.5 maps to source (2i+0.5)/2-0.5 = i-0.25: not on
            // a sample. Use 3×: output 3i+1 -> (3i+1.5)/3-0.5 = i exactly.
            let out = resize_with(&src, 48, 1, k);
            for i in 2..14 {
                assert!((out.at(3 * i + 1, 0) - src.at(i, 0)).abs() < 1e-12, "{k:?} at {i}");
            }
        }
        // The B-spline smooths even there.
        let out = resize_with(&src, 48, 1, ResampleKernel::BSpline);
        assert!((2..14).any(|i| (out.at(3 * i + 1, 0) - src.at(i, 0)).abs() > 1e-3));
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

    /// The stored `enlargement_k` is the 10–90 % fit. It has to stay put
    /// across scales: the kernel is not stretched when enlarging, so σ grows
    /// with `s`.
    #[test]
    fn enlargement_k_matches_the_fitted_ramp() {
        for kernel in ResampleKernel::ALL {
            let k = kernel.enlargement_k();
            assert!((fit_k(kernel, 8.0) - k).abs() < 0.01, "{kernel:?} at 8×");
            assert!((fit_k(kernel, 4.0) - k).abs() < 0.025, "{kernel:?} at 4×");
            assert_eq!(enlargement_sigma(32, 8, 32, 8, kernel), 0.0);
            assert_eq!(enlargement_sigma(32, 8, 16, 4, kernel), 0.0);
            let s = enlargement_sigma(32, 8, 100, 25, kernel);
            assert!((s - k * 100.0 / 32.0).abs() < 1e-12, "{s}");
        }
    }

    fn fit_k(kernel: ResampleKernel, s: f64) -> f64 {
        let src_w = 32usize;
        let dst_w = (src_w as f64 * s).round() as usize;
        let mut src = Plane::zeros(src_w, 1);
        for x in src_w / 2..src_w {
            src.set(x, 0, 1.0);
        }
        let resized = resize_with(&src, dst_w, 1, kernel);
        let mut nn = Plane::zeros(dst_w, 1);
        for x in 0..dst_w {
            let sx = ((x as f64 + 0.5) * src_w as f64 / dst_w as f64).floor() as usize;
            nn.set(x, 0, src.at(sx.min(src_w - 1), 0));
        }
        let target = rise_width(&resized.data);
        let mut lo = 0.05;
        let mut hi = s * 3.0;
        for _ in 0..48 {
            let mid = 0.5 * (lo + hi);
            let blurred = crate::usm::gaussian_blur(&nn, mid);
            if rise_width(&blurred.data) < target {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        (0.5 * (lo + hi)) / s
    }

    /// Width from the 10 % level to the 90 % level, measured from the 50 %
    /// crossing so a ringing lobe away from the edge does not count.
    fn rise_width(data: &[f64]) -> f64 {
        let mut cross = None;
        for i in 0..data.len().saturating_sub(1) {
            let (a, b) = (data[i], data[i + 1]);
            if (a - 0.5) * (b - 0.5) <= 0.0 && b != a {
                let t = (0.5 - a) / (b - a);
                cross = Some(i as f64 + t);
                break;
            }
        }
        let edge = cross.expect("step crosses 0.5");
        let level = |want: f64, dir: f64| -> f64 {
            let mut x = edge;
            loop {
                let i = x.floor() as isize;
                if i < 0 || i + 1 >= data.len() as isize {
                    break;
                }
                let (a, b) = (data[i as usize], data[(i + 1) as usize]);
                let v = a + (b - a) * (x - i as f64);
                if (dir < 0.0 && v <= want) || (dir > 0.0 && v >= want) {
                    // Step back one sample and interpolate.
                    let prev = x - dir;
                    let j = prev.floor() as isize;
                    if j < 0 || j + 1 >= data.len() as isize {
                        return x;
                    }
                    let (c, d) = (data[j as usize], data[(j + 1) as usize]);
                    let vp = c + (d - c) * (prev - j as f64);
                    let den = v - vp;
                    if den.abs() < 1e-12 {
                        return x;
                    }
                    return prev + dir * (want - vp) / den;
                }
                x += dir;
                if x < 1.0 || x > data.len() as f64 - 2.0 {
                    break;
                }
            }
            x
        };
        level(0.9, 1.0) - level(0.1, -1.0)
    }
}
