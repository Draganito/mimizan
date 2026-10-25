//! Separable resampling in f64.
//!
//! The kernel is a parameter ([`ResampleKernel`]). The print default is the
//! B-spline; [`resize`] still uses Lanczos-3, which is the bench kernel.
//! When shrinking, a convolution kernel is stretched by the scale factor so
//! it acts as the anti-alias low-pass at the same time (the standard
//! "support scales with the footprint" rule). Weights are normalised per
//! output sample so the DC gain is exactly one everywhere, including at the
//! borders (which are handled by clamping the source index).
//!
//! [`ResampleKernel::Area`] is not that convolution. Each output pixel is the
//! mean of the source rectangle it covers, weighted by overlap. The same
//! centre mapping tiles the source exactly, so the weights are a partition
//! of the measured pixels.
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
    /// Box area. Each output pixel is the mean of the source area its
    /// rectangle covers: overlap lengths, renormalised, so every weight is
    /// positive and the weights sum to one. Shrinking averages the measured
    /// pixels. Enlarging holds each pixel, and mixes two neighbours only
    /// where an output pixel crosses a source edge. Nothing is invented
    /// outside the samples. The hold is not a blur, so [`Self::enlargement_k`]
    /// is zero and the sharpener does not undo it.
    Area,
}

impl ResampleKernel {
    /// Every kernel, in menu order.
    pub const ALL: [ResampleKernel; 5] =
        [Self::Lanczos3, Self::Mitchell, Self::CatmullRom, Self::BSpline, Self::Area];

    /// Name as it appears in the description JSON and on the command line.
    pub fn name(self) -> &'static str {
        match self {
            Self::Lanczos3 => "lanczos3",
            Self::Mitchell => "mitchell",
            Self::CatmullRom => "catmull-rom",
            Self::BSpline => "b-spline",
            Self::Area => "area",
        }
    }

    /// Kernel from its name, case-insensitive, hyphens optional; `box` is
    /// an alias of `area`.
    pub fn parse(s: &str) -> Option<Self> {
        let t = s.trim().to_ascii_lowercase();
        if t == "box" {
            return Some(Self::Area);
        }
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
            // A hold, not a ramp. There is no enlargement blur to fit.
            Self::Area => 0.0,
        }
    }

    /// Half-width of the support in source pixels (at scale 1).
    pub fn support(self) -> f64 {
        match self {
            Self::Lanczos3 => 3.0,
            Self::Mitchell | Self::CatmullRom | Self::BSpline => 2.0,
            // Unit box at scale 1: the pixel itself. Taps do not use this.
            Self::Area => 0.5,
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
            Self::Area => box_area(x),
        }
    }

    /// Continuous frequency response in output pixel units, DC = 1 (numeric
    /// integration). The figures in the variant docs come from this.
    #[cfg(test)]
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

/// Unit box, half-width 1/2. The scale-1 shape of [`ResampleKernel::Area`];
/// the resize itself uses overlap lengths, not samples of this function.
#[inline]
fn box_area(x: f64) -> f64 {
    if x.abs() < 0.5 {
        1.0
    } else {
        0.0
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
    if kernel == ResampleKernel::Area {
        return area_taps(src_len, dst_len, i0, i1);
    }
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

/// Overlap of destination pixels `i0..i1` with the source pixels.
///
/// Destination pixel `i` covers `[i·scale − ½, (i+1)·scale − ½]` in source
/// coordinates, and source pixel `j` covers `[j−½, j+½]`. That mapping tiles
/// `[−½, N−½]`, the whole sensor. A weight is the overlap length divided by
/// the sum, so the sample is a convex combination of the pixels it touches.
fn area_taps(src_len: usize, dst_len: usize, i0: usize, i1: usize) -> Taps {
    let scale = src_len as f64 / dst_len as f64;
    let src_lo = -0.5;
    let src_hi = src_len as f64 - 0.5;
    let n = i1.saturating_sub(i0);
    let mut start = Vec::with_capacity(n);
    let mut weights = Vec::with_capacity(n);
    for i in i0..i1 {
        let left = (i as f64 * scale - 0.5).max(src_lo);
        let right = ((i as f64 + 1.0) * scale - 0.5).min(src_hi);
        let j_first = (left + 0.5).floor().clamp(0.0, src_len.saturating_sub(1) as f64) as usize;
        let j_stop = (right + 0.5).ceil().clamp(0.0, src_len as f64) as usize;
        let mut w = Vec::new();
        let mut sum = 0.0;
        let mut first = j_first;
        let mut seen = false;
        for j in j_first..j_stop {
            let overlap = right.min(j as f64 + 0.5) - left.max(j as f64 - 0.5);
            if overlap > 0.0 {
                if !seen {
                    first = j;
                    seen = true;
                }
                w.push(overlap);
                sum += overlap;
            } else if seen {
                break;
            }
        }
        if sum == 0.0 {
            let c = ((i as f64 + 0.5) * scale - 0.5).round().clamp(0.0, src_len.saturating_sub(1) as f64)
                as usize;
            start.push(c);
            weights.push(vec![1.0]);
            continue;
        }
        for v in &mut w {
            *v /= sum;
        }
        start.push(first);
        weights.push(w);
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
        for (k, wk) in wts.iter().enumerate() {
            let s = src.row(st + k);
            for (o, &v) in row.iter_mut().zip(s) {
                *o += wk * v;
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

/// A rectangle of a larger plane. `plane` holds the pixels from
/// `(origin_x, origin_y)` of an image that is `full_w × full_h`. Resampling
/// a window reads through this, with the same weights as the full image.
#[derive(Clone, Copy)]
pub struct PlaneWindow<'a> {
    /// The pixels held.
    pub plane: &'a Plane,
    /// Full-image coordinate of `plane`'s top-left pixel.
    pub origin_x: usize,
    pub origin_y: usize,
    /// Size of the full image the window is cut from.
    pub full_w: usize,
    pub full_h: usize,
}

impl<'a> PlaneWindow<'a> {
    /// The whole plane, origin at 0.
    pub fn full(plane: &'a Plane) -> Self {
        Self { plane, origin_x: 0, origin_y: 0, full_w: plane.width, full_h: plane.height }
    }

    /// Full-image pixel `(x, y)`; must lie inside the window.
    #[inline]
    fn at(self, x: usize, y: usize) -> f64 {
        self.plane.at(x - self.origin_x, y - self.origin_y)
    }

    /// Does the window hold full-image columns `[x0, x1)` and rows `[y0, y1)`?
    fn covers(self, x0: usize, y0: usize, x1: usize, y1: usize) -> bool {
        x0 >= self.origin_x
            && y0 >= self.origin_y
            && x1 <= self.origin_x.saturating_add(self.plane.width)
            && y1 <= self.origin_y.saturating_add(self.plane.height)
    }
}

/// A rectangle in pixel indices.
#[derive(Clone, Copy, Debug)]
pub struct PixelRect {
    /// Left edge.
    pub x: usize,
    /// Top edge.
    pub y: usize,
    /// Width.
    pub w: usize,
    /// Height.
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

/// Source span `[a, b)` that destination samples `i0..i1` read. The taps of
/// one axis move monotonically with the sample, so the first sample's first
/// tap and the last sample's last tap bound every sample between.
fn axis_span(src_len: usize, dst_len: usize, kernel: ResampleKernel, i0: usize, i1: usize) -> (usize, usize) {
    if i0 >= i1 {
        return (i0, i0);
    }
    if src_len == dst_len {
        return (i0, i1);
    }
    let first = taps_range(src_len, dst_len, kernel, i0, i0 + 1);
    let last = taps_range(src_len, dst_len, kernel, i1 - 1, i1);
    (first.start[0], last.start[0] + last.weights[0].len())
}

/// Destination pixels `[dx, dx+dw) × [dy, dy+dh)` of [`resize_with`] on the
/// full image. `src` must cover [`source_coverage`] of that rectangle; a
/// window that does not, an empty or out-of-range rectangle, or an empty
/// destination is an error, never a panic.
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
    if dst_w == 0 || dst_h == 0 || src.full_w == 0 || src.full_h == 0 {
        return Err(crate::Error::Invalid("resize of or to an empty plane".into()));
    }
    let inside =
        dx.checked_add(dw).is_some_and(|r| r <= dst_w) && dy.checked_add(dh).is_some_and(|b| b <= dst_h);
    if !inside {
        return Err(crate::Error::Invalid(format!(
            "resize window {dx},{dy} {dw}×{dh} lies outside the {dst_w}×{dst_h} destination"
        )));
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
    let (sx0, sy0, sx1, sy1) = source_coverage(src.full_w, src.full_h, dst_w, dst_h, kernel, rect);
    if !src.covers(sx0, sy0, sx1, sy1) {
        return Err(crate::Error::Invalid(format!(
            "resize window needs source {sx0},{sy0}..{sx1},{sy1} but the plane holds {},{}..{},{}",
            src.origin_x,
            src.origin_y,
            src.origin_x + src.plane.width,
            src.origin_y + src.plane.height
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
    let rows_first = src.full_h * dst_w <= src.full_w * dst_h;
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
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
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
    });
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
    out.data.par_chunks_mut(w).enumerate().for_each(|(i, row)| {
        let st = taps.start[i];
        let weights = &taps.weights[i];
        for (k, wk) in weights.iter().enumerate() {
            let sy = st + k;
            for (x, o) in row.iter_mut().enumerate() {
                *o += wk * src.at(x0 + x, sy);
            }
        }
    });
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
    out.data.par_chunks_mut(w).enumerate().for_each(|(i, row)| {
        let st = taps.start[i];
        let weights = &taps.weights[i];
        for (k, wk) in weights.iter().enumerate() {
            let s = mid.row(st + k - mid_y0);
            for (o, &v) in row.iter_mut().zip(s) {
                *o += wk * v;
            }
        }
    });
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
    let w = x1 - x0;
    let mut out = Plane::zeros(w, mid.height);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let s = mid.row(y);
        for (x, o) in row.iter_mut().enumerate() {
            let st = taps.start[x];
            let weights = &taps.weights[x];
            let mut acc = 0.0;
            for (k, wk) in weights.iter().enumerate() {
                acc += wk * s[st + k - mid_x0];
            }
            *o = acc;
        }
    });
    out
}

/// Resample to exactly `dst_w × dst_h` with `kernel`. When both axes
/// change, the pass whose intermediate is smaller runs first. An unchanged
/// size returns a copy, whatever the kernel (the B-spline would otherwise
/// smooth at scale 1). An empty source or destination gives an empty plane
/// of the destination size.
pub fn resize_with(src: &Plane, dst_w: usize, dst_h: usize, kernel: ResampleKernel) -> Plane {
    if dst_w == 0 || dst_h == 0 || src.width == 0 || src.height == 0 {
        return Plane::zeros(dst_w, dst_h);
    }
    match (dst_w != src.width, dst_h != src.height) {
        (false, false) => src.clone(),
        (true, false) => resize_rows(src, dst_w, kernel),
        (false, true) => resize_cols(src, dst_h, kernel),
        (true, true) => {
            // Intermediate samples: rows first leaves `src.height × dst_w`,
            // columns first `src.width × dst_h`.
            if src.height * dst_w <= src.width * dst_h {
                resize_cols(&resize_rows(src, dst_w, kernel), dst_h, kernel)
            } else {
                resize_rows(&resize_cols(src, dst_h, kernel), dst_w, kernel)
            }
        }
    }
}

/// Largest overshoot above 1 and undershoot below 0 when a unit step is
/// resampled by `scale` with `kernel`: what the kernel does at a hard edge.
/// The figures in the variant docs come from this.
#[cfg(test)]
pub fn step_overshoot(kernel: ResampleKernel, scale: f64) -> (f64, f64) {
    let n = 64;
    let mut src = Plane::zeros(n, 1);
    for x in n / 2..n {
        src.set(x, 0, 1.0);
    }
    let out = resize_with(&src, ((n as f64 * scale).round() as usize).max(1), 1, kernel);
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

    /// Bad geometry is an error the loupe thread can report, not a panic.
    #[test]
    fn bad_window_geometry_is_an_error() {
        let src = Plane::zeros(8, 8);
        let k = ResampleKernel::Lanczos3;
        // Rectangle beyond the destination.
        let off = PixelRect { x: 20, y: 0, w: 2, h: 2 };
        assert!(resize_rect(PlaneWindow::full(&src), 5, 5, k, off).is_err());
        // Empty destination.
        let rect = PixelRect { x: 0, y: 0, w: 1, h: 1 };
        assert!(resize_rect(PlaneWindow::full(&src), 0, 5, k, rect).is_err());
        // A window that does not hold the taps the rectangle needs.
        let part = Plane::zeros(2, 2);
        let window = PlaneWindow { plane: &part, origin_x: 6, origin_y: 6, full_w: 8, full_h: 8 };
        let rect = PixelRect { x: 0, y: 0, w: 2, h: 2 };
        assert!(resize_rect(window, 16, 16, k, rect).is_err());
        // The same rectangle from a window that does hold them.
        let window = PlaneWindow { plane: &src, origin_x: 0, origin_y: 0, full_w: 8, full_h: 8 };
        assert!(resize_rect(window, 16, 16, k, rect).is_ok());
        // Empty planes do not panic either.
        assert_eq!(resize_with(&Plane::zeros(0, 5), 3, 3, k).data.len(), 9);
        assert_eq!(resize_with(&src, 0, 3, k).data.len(), 0);
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
        assert_eq!(ResampleKernel::parse("box"), Some(ResampleKernel::Area));
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
            for kernel in [ResampleKernel::BSpline, ResampleKernel::Area] {
                let (hi, lo) = step_overshoot(kernel, s);
                assert!(hi < 1e-12 && lo > -1e-12, "{kernel:?} at {s}: {hi} {lo}");
            }
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
            if kernel == ResampleKernel::Area {
                assert_eq!(k, 0.0);
                assert_eq!(enlargement_sigma(32, 8, 256, 64, kernel), 0.0);
                continue;
            }
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

    #[test]
    fn area_integer_scales_are_the_pixels() {
        let src = Plane::from_vec(4, 1, vec![1.0, 2.0, 3.0, 4.0]);
        let down = resize_with(&src, 2, 1, ResampleKernel::Area);
        assert_eq!(down.data, vec![1.5, 3.5]);
        let up = resize_with(&src, 8, 1, ResampleKernel::Area);
        assert_eq!(up.data, vec![1.0, 1.0, 2.0, 2.0, 3.0, 3.0, 4.0, 4.0]);
        // 3 → 2: pixel 0 covers 1 + ½, pixel 1 covers ½ + 1.
        let src3 = Plane::from_vec(3, 1, vec![10.0, 20.0, 30.0]);
        let mid = resize_with(&src3, 2, 1, ResampleKernel::Area);
        assert!((mid.at(0, 0) - (10.0 * 2.0 + 20.0) / 3.0).abs() < 1e-12);
        assert!((mid.at(1, 0) - (20.0 + 30.0 * 2.0) / 3.0).abs() < 1e-12);
        // 2 → 3: the middle output pixel straddles the source edge.
        let src2 = Plane::from_vec(2, 1, vec![10.0, 40.0]);
        let up3 = resize_with(&src2, 3, 1, ResampleKernel::Area);
        assert!((up3.at(0, 0) - 10.0).abs() < 1e-12);
        assert!((up3.at(1, 0) - 25.0).abs() < 1e-12);
        assert!((up3.at(2, 0) - 40.0).abs() < 1e-12);
    }

    #[test]
    fn area_matches_the_overlap_and_keeps_the_mean() {
        let (w, h) = (5usize, 4usize);
        let mut src = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                src.set(x, y, (x * 3 + y * 5) as f64);
            }
        }
        let (dw, dh) = (3usize, 7usize);
        let got = resize_with(&src, dw, dh, ResampleKernel::Area);
        let sx = w as f64 / dw as f64;
        let sy = h as f64 / dh as f64;
        for y in 0..dh {
            let top = (y as f64 * sy - 0.5).max(-0.5);
            let bot = ((y as f64 + 1.0) * sy - 0.5).min(h as f64 - 0.5);
            for x in 0..dw {
                let left = (x as f64 * sx - 0.5).max(-0.5);
                let right = ((x as f64 + 1.0) * sx - 0.5).min(w as f64 - 0.5);
                let mut acc = 0.0;
                let mut sum = 0.0;
                for iy in 0..h {
                    let oy = bot.min(iy as f64 + 0.5) - top.max(iy as f64 - 0.5);
                    if oy <= 0.0 {
                        continue;
                    }
                    for ix in 0..w {
                        let ox = right.min(ix as f64 + 0.5) - left.max(ix as f64 - 0.5);
                        if ox > 0.0 {
                            let a = ox * oy;
                            acc += a * src.at(ix, iy);
                            sum += a;
                        }
                    }
                }
                assert!((got.at(x, y) - acc / sum).abs() < 1e-12, "at {x},{y}");
            }
        }
        let src_mean = src.data.iter().sum::<f64>() / src.data.len() as f64;
        let dst_mean = got.data.iter().sum::<f64>() / got.data.len() as f64;
        assert!((src_mean - dst_mean).abs() < 1e-9, "{src_mean} vs {dst_mean}");
        let block = Plane::from_vec(2, 2, vec![1.0, 3.0, 5.0, 7.0]);
        let one = resize_with(&block, 1, 1, ResampleKernel::Area);
        assert!((one.at(0, 0) - 4.0).abs() < 1e-12);
    }
}
