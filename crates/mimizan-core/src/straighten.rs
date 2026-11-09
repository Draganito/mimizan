//! The straighten: a small turn of the upright negative about its centre,
//! and the largest window of the negative's own proportion inside the
//! turned picture. That window is the *frame*; the crop lives in it, and
//! everything after the crop sees a smaller negative and nothing else.
//!
//! The turn is the one step of the pipeline that interpolates before the
//! size step, so the two are done as one: [`warp`] maps every print pixel
//! back through the crop, the frame and the turn into the upright
//! negative and resamples there once, with the print's own kernel, the
//! support stretched by the reduction as [`crate::resize`] stretches it.
//! At angle zero nothing here runs; the separable resize keeps its bytes.
//!
//! The angle is in degrees, clockwise positive on the screen (y down), and
//! is the turn given to the picture. A horizon that falls to the right is
//! put level by a negative angle.

use crate::plane::Plane;
use crate::recipe::CropRect;
use crate::resize::{PixelRect, PlaneWindow, ResampleKernel};
use rayon::prelude::*;

/// Widest turn a recipe accepts, degrees. Beyond it the picture is turned
/// by 90° instead.
pub const ANGLE_LIMIT: f64 = 45.0;

/// The kernel a turn without a size step resamples with. Measured
/// against a scene rendered at eight times the negative's resolution
/// (the truth for a level sensor) and on a real negative turned there and
/// back: the B-spline the print defaults to keeps half of the fine
/// detail's energy at scale 1 and a fifth less grain; Catmull-Rom has
/// the lowest total error at the usual 0.5–5° (0.0163 against 0.0276 for
/// the B-spline, RMSE on [0,1]), keeps 85–93 % of the detail, and
/// overshoots a hard highlight by under 1 %. Lanczos3 keeps 94–98 % but
/// overshoots by 2–4 %; Mitchell keeps 71–83 %; area aliases. At 1:1 the
/// picture was never resampled before, so the kernel nearest to that is
/// the one the turn takes.
pub const STRAIGHTEN_KERNEL: ResampleKernel = ResampleKernel::CatmullRom;

/// The kernel the one warp through `map` uses for the picture: the
/// recipe's `resample` when the map has a size step, [`STRAIGHTEN_KERNEL`]
/// when it is a turn alone (both scales one). The mask keeps its own.
pub fn kernel_for(map: &Affine, resample: ResampleKernel) -> ResampleKernel {
    let (sx, sy) = map.scales();
    if (sx - 1.0).abs() < 1e-6 && (sy - 1.0).abs() < 1e-6 {
        STRAIGHTEN_KERNEL
    } else {
        resample
    }
}

/// `true` when `angle` asks for a turn at all. Angles below a thousandth
/// of a degree are zero: on a 10 000-pixel side that is less than a tenth
/// of a pixel at the corner.
pub fn turns(angle: f64) -> bool {
    angle.is_finite() && angle.abs() >= 1e-3
}

/// Scale of the frame against the upright negative: the largest rectangle
/// of the negative's proportion that fits inside the negative turned by
/// `angle`, as a fraction of the negative's sides. One at angle zero.
pub fn inscribed_scale(w: usize, h: usize, angle: f64) -> f64 {
    if !turns(angle) || w == 0 || h == 0 {
        return 1.0;
    }
    let (sin, cos) = angle.to_radians().abs().sin_cos();
    let (w, h) = (w as f64, h as f64);
    // Each corner of the inscribed rectangle, turned back, must lie inside
    // the negative: the width bound and the height bound.
    let by_w = w / (w * cos + h * sin);
    let by_h = h / (w * sin + h * cos);
    by_w.min(by_h).clamp(0.0, 1.0)
}

/// The frame: the upright negative's `w×h` turned by `angle` and cut to
/// the largest window of its own proportion, whole pixels, rounded down so
/// every corner stays inside the turned picture. `(w, h)` at angle zero.
pub fn frame_size(w: usize, h: usize, angle: f64) -> (usize, usize) {
    if !turns(angle) {
        return (w, h);
    }
    let s = inscribed_scale(w, h, angle);
    let fw = ((w as f64 * s).floor() as usize).clamp(1, w.max(1));
    let fh = ((h as f64 * s).floor() as usize).clamp(1, h.max(1));
    (fw, fh)
}

/// A map from one continuous pixel coordinate system to another:
/// `(x, y) ↦ (a·x + b·y + tx, c·x + d·y + ty)`. Pixel `i` spans
/// `[i, i+1)` and has its centre at `i + 0.5` on both sides.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Affine {
    pub a: f64,
    pub b: f64,
    pub c: f64,
    pub d: f64,
    pub tx: f64,
    pub ty: f64,
}

impl Affine {
    /// The identity.
    pub const IDENTITY: Affine = Affine { a: 1.0, b: 0.0, c: 0.0, d: 1.0, tx: 0.0, ty: 0.0 };

    /// Where `(x, y)` lands.
    #[inline]
    pub fn apply(&self, x: f64, y: f64) -> (f64, f64) {
        (self.a * x + self.b * y + self.tx, self.c * x + self.d * y + self.ty)
    }

    /// `self` after `inner`: `(self ∘ inner)(p) = self(inner(p))`.
    pub fn after(&self, inner: &Affine) -> Affine {
        Affine {
            a: self.a * inner.a + self.b * inner.c,
            b: self.a * inner.b + self.b * inner.d,
            c: self.c * inner.a + self.d * inner.c,
            d: self.c * inner.b + self.d * inner.d,
            tx: self.a * inner.tx + self.b * inner.ty + self.tx,
            ty: self.c * inner.tx + self.d * inner.ty + self.ty,
        }
    }

    /// Source distance covered by one destination pixel along x and y:
    /// above one when the map shrinks the picture.
    pub fn scales(&self) -> (f64, f64) {
        ((self.a * self.a + self.c * self.c).sqrt(), (self.b * self.b + self.d * self.d).sqrt())
    }

    /// The inverse, when there is one.
    pub fn inverse(&self) -> Option<Affine> {
        let det = self.a * self.d - self.b * self.c;
        if det.abs() < 1e-300 || !det.is_finite() {
            return None;
        }
        let (a, b, c, d) = (self.d / det, -self.b / det, -self.c / det, self.a / det);
        Some(Affine { a, b, c, d, tx: -(a * self.tx + b * self.ty), ty: -(c * self.tx + d * self.ty) })
    }
}

/// The TIFF orientation as a map on continuous coordinates: from the
/// `w×h` sensor plane to the upright picture `orient(_, orientation)`
/// gives, pixel centres to pixel centres. Unknown values are upright.
pub fn orient_map(orientation: u16, w: usize, h: usize) -> Affine {
    let (w, h) = (w as f64, h as f64);
    let m = |a, b, c, d, tx, ty| Affine { a, b, c, d, tx, ty };
    match orientation {
        2 => m(-1.0, 0.0, 0.0, 1.0, w, 0.0),
        3 => m(-1.0, 0.0, 0.0, -1.0, w, h),
        4 => m(1.0, 0.0, 0.0, -1.0, 0.0, h),
        5 => m(0.0, 1.0, 1.0, 0.0, 0.0, 0.0),
        6 => m(0.0, -1.0, 1.0, 0.0, h, 0.0),
        7 => m(0.0, -1.0, -1.0, 0.0, h, w),
        8 => m(0.0, 1.0, -1.0, 0.0, 0.0, w),
        _ => Affine::IDENTITY,
    }
}

/// From the frame to the upright negative: the frame's centre is the
/// negative's centre, and the frame is turned back by `angle`. At angle
/// zero the frame is the negative and this is the identity.
pub fn frame_to_upright(w: usize, h: usize, angle: f64) -> Affine {
    if !turns(angle) {
        return Affine::IDENTITY;
    }
    let (fw, fh) = frame_size(w, h, angle);
    let (sin, cos) = angle.to_radians().sin_cos();
    let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
    let (fx, fy) = (fw as f64 / 2.0, fh as f64 / 2.0);
    // A picture turned clockwise by θ shows at frame point d the source
    // point R(−θ)·d: x' = x cos + y sin, y' = −x sin + y cos (y down).
    Affine { a: cos, b: sin, c: -sin, d: cos, tx: cx - fx * cos - fy * sin, ty: cy + fx * sin - fy * cos }
}

/// From a `dst_w×dst_h` picture to the window `(x, y, w, h)` it is the
/// fit of: the same centre mapping the separable resize uses.
pub fn window_to_frame(window: (usize, usize, usize, usize), dst_w: usize, dst_h: usize) -> Affine {
    let (x, y, w, h) = window;
    Affine {
        a: w as f64 / dst_w.max(1) as f64,
        b: 0.0,
        c: 0.0,
        d: h as f64 / dst_h.max(1) as f64,
        tx: x as f64,
        ty: y as f64,
    }
}

/// The print's map: from a `dst_w×dst_h` picture, through the crop window
/// on the frame and the frame's turn, into the upright `w×h` negative.
pub fn print_map(
    w: usize,
    h: usize,
    angle: f64,
    crop: Option<CropRect>,
    dst_w: usize,
    dst_h: usize,
) -> Affine {
    let (fw, fh) = frame_size(w, h, angle);
    let window = crate::print::source_window(fw, fh, crop);
    frame_to_upright(w, h, angle).after(&window_to_frame(window, dst_w, dst_h))
}

/// Taps along one axis for source centre `c` (index units), the kernel
/// stretched by `stretch`: first source index, then the folded weights.
/// Indices are clamped to `0..len` and their weights folded onto the edge
/// sample, as the separable resize does. A kernel that touches nothing
/// (the box exactly between two pixels) takes the nearest one.
fn axis_taps(c: f64, len: usize, kernel: ResampleKernel, stretch: f64, w: &mut Vec<f64>) -> usize {
    w.clear();
    let support = kernel.support() * stretch;
    let lo = (c - support).floor() as isize;
    let hi = (c + support).ceil() as isize;
    let last = len as isize - 1;
    let first = lo.clamp(0, last);
    let end = hi.clamp(0, last);
    w.resize((end - first + 1) as usize, 0.0);
    let mut sum = 0.0;
    for j in lo..=hi {
        let v = kernel.eval((j as f64 - c) / stretch);
        if v != 0.0 {
            let jc = j.clamp(0, last);
            w[(jc - first) as usize] += v;
            sum += v;
        }
    }
    if sum.abs() < 1e-12 {
        w.clear();
        w.push(1.0);
        return c.round().clamp(0.0, last as f64) as usize;
    }
    for v in w.iter_mut() {
        *v /= sum;
    }
    first as usize
}

/// Source pixels `warp` reads for destination `rect` through `map` with
/// `kernel`: `(x0, y0, x1, y1)`, half-open, clamped to the `src_w×src_h`
/// source. The map is affine, so the extremes lie at the corners.
pub fn footprint(
    map: &Affine,
    kernel: ResampleKernel,
    rect: PixelRect,
    src_w: usize,
    src_h: usize,
) -> (usize, usize, usize, usize) {
    if rect.w == 0 || rect.h == 0 || src_w == 0 || src_h == 0 {
        return (0, 0, 0, 0);
    }
    let (sx, sy) = map.scales();
    let (stretch_x, stretch_y) = (sx.max(1.0), sy.max(1.0));
    let (support_x, support_y) = (kernel.support() * stretch_x, kernel.support() * stretch_y);
    let corners = [
        (rect.x as f64 + 0.5, rect.y as f64 + 0.5),
        ((rect.x + rect.w) as f64 - 0.5, rect.y as f64 + 0.5),
        (rect.x as f64 + 0.5, (rect.y + rect.h) as f64 - 0.5),
        ((rect.x + rect.w) as f64 - 0.5, (rect.y + rect.h) as f64 - 0.5),
    ];
    let (mut u0, mut u1, mut v0, mut v1) = (f64::MAX, f64::MIN, f64::MAX, f64::MIN);
    for (x, y) in corners {
        let (u, v) = map.apply(x, y);
        u0 = u0.min(u);
        u1 = u1.max(u);
        v0 = v0.min(v);
        v1 = v1.max(v);
    }
    // Index centres are the continuous coordinates less a half.
    let clamp = |lo: f64, hi: f64, len: usize| {
        let last = len as f64 - 1.0;
        let a = (lo - 0.5 - 1.0).floor().clamp(0.0, last) as usize;
        let b = (hi - 0.5 + 1.0).ceil().clamp(0.0, last) as usize;
        (a, b + 1)
    };
    let (x0, x1) = clamp(u0 - support_x, u1 + support_x, src_w);
    let (y0, y1) = clamp(v0 - support_y, v1 + support_y, src_h);
    (x0, y0, x1, y1)
}

/// Destination pixels `rect` of the picture `map` is written for, read
/// from `src` through `map` and resampled with `kernel`. `src` must hold
/// [`footprint`] of `rect`; a window that does not is an error, never a
/// panic. The weights of one pixel are the kernel's along x times the
/// kernel's along y, each normalised to one, each stretched by what one
/// destination pixel covers on that axis when the map shrinks.
pub fn warp(
    src: PlaneWindow<'_>,
    map: &Affine,
    kernel: ResampleKernel,
    rect: PixelRect,
) -> crate::Result<Plane> {
    if rect.w == 0 || rect.h == 0 {
        return Err(crate::Error::Invalid("warp window is empty".into()));
    }
    if src.full_w == 0 || src.full_h == 0 || src.plane.width == 0 || src.plane.height == 0 {
        return Err(crate::Error::Invalid("warp of an empty plane".into()));
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
    let (x0, y0, x1, y1) = footprint(map, kernel, rect, src.full_w, src.full_h);
    let held_x1 = src.origin_x.saturating_add(src.plane.width);
    let held_y1 = src.origin_y.saturating_add(src.plane.height);
    if x0 < src.origin_x || y0 < src.origin_y || x1 > held_x1 || y1 > held_y1 {
        return Err(crate::Error::Invalid(format!(
            "warp window needs source {x0},{y0}..{x1},{y1} but the plane holds {},{}..{held_x1},{held_y1}",
            src.origin_x, src.origin_y
        )));
    }
    let (sx, sy) = map.scales();
    let (stretch_x, stretch_y) = (sx.max(1.0), sy.max(1.0));
    let (full_w, full_h) = (src.full_w, src.full_h);
    let (ox, oy) = (src.origin_x, src.origin_y);
    let plane = src.plane;
    let mut out = Plane::zeros(rect.w, rect.h);
    out.data.par_chunks_mut(rect.w).enumerate().for_each(|(row_i, row)| {
        let mut wx = Vec::with_capacity(8);
        let mut wy = Vec::with_capacity(8);
        let y = (rect.y + row_i) as f64 + 0.5;
        for (col_i, o) in row.iter_mut().enumerate() {
            let x = (rect.x + col_i) as f64 + 0.5;
            let (u, v) = map.apply(x, y);
            let jx = axis_taps(u - 0.5, full_w, kernel, stretch_x, &mut wx);
            let jy = axis_taps(v - 0.5, full_h, kernel, stretch_y, &mut wy);
            let mut acc = 0.0;
            for (ky, wyk) in wy.iter().enumerate() {
                let s = plane.row(jy + ky - oy);
                let mut line = 0.0;
                for (kx, wxk) in wx.iter().enumerate() {
                    line += wxk * s[jx + kx - ox];
                }
                acc += wyk * line;
            }
            *o = acc;
        }
    });
    Ok(out)
}

/// The whole `dst_w×dst_h` picture of `src` through `map`.
pub fn warp_plane(src: &Plane, map: &Affine, dst_w: usize, dst_h: usize, kernel: ResampleKernel) -> Plane {
    if dst_w == 0 || dst_h == 0 || src.width == 0 || src.height == 0 {
        return Plane::zeros(dst_w, dst_h);
    }
    warp(PlaneWindow::full(src), map, kernel, PixelRect { x: 0, y: 0, w: dst_w, h: dst_h })
        .unwrap_or_else(|_| Plane::zeros(dst_w, dst_h))
}

/// The upright negative `w×h` straightened: the frame, pixel for pixel,
/// no size step. What the linear exports write before the crop.
pub fn straighten(src: &Plane, angle: f64, kernel: ResampleKernel) -> Plane {
    if !turns(angle) {
        return src.clone();
    }
    let (fw, fh) = frame_size(src.width, src.height, angle);
    warp_plane(src, &frame_to_upright(src.width, src.height, angle), fw, fh, kernel)
}

/// The turn that puts the line from `(x0, y0)` to `(x1, y1)` (screen
/// coordinates, y down) level, or upright when it is nearer to vertical:
/// degrees, clockwise positive, as `angle` takes it. `None` for a line
/// too short to have a direction.
pub fn angle_of_line(x0: f64, y0: f64, x1: f64, y1: f64) -> Option<f64> {
    let (dx, dy) = (x1 - x0, y1 - y0);
    if dx.hypot(dy) < 1e-9 || !dx.is_finite() || !dy.is_finite() {
        return None;
    }
    // The line's own tilt, clockwise from horizontal.
    let tilt = dy.atan2(dx).to_degrees();
    // Reduce to (−45, 45]: the nearest of horizontal and vertical.
    let mut t = tilt.rem_euclid(90.0);
    if t > 45.0 {
        t -= 90.0;
    }
    // Turning the picture by −t puts the line on that axis.
    Some(-t)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print::orient;

    fn ramp(w: usize, h: usize) -> Plane {
        let mut p = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                p.set(x, y, (x * 7 + y * 13) as f64 / (w * 7 + h * 13) as f64);
            }
        }
        p
    }

    #[test]
    fn frame_follows_the_formula() {
        assert_eq!(frame_size(6000, 4000, 0.0), (6000, 4000));
        assert_eq!(frame_size(6000, 4000, 0.0001), (6000, 4000));
        let (w, h) = frame_size(6000, 4000, 2.0);
        assert_eq!((w, h), (5704, 3803));
        // The proportion is kept to the pixel the rounding allows.
        assert!((w as f64 / h as f64 - 1.5).abs() < 1e-3);
        // Square at 45°: 1/√2 of the side.
        let (w, h) = frame_size(1000, 1000, 45.0);
        assert_eq!((w, h), (707, 707));
        // The sign does not matter to the frame.
        assert_eq!(frame_size(6000, 4000, -2.0), frame_size(6000, 4000, 2.0));
        // Portrait and landscape agree.
        let (a, b) = frame_size(4000, 6000, 3.0);
        assert_eq!((b, a), frame_size(6000, 4000, 3.0));
    }

    #[test]
    fn frame_corners_lie_inside_the_negative() {
        for &(w, h) in &[(6000usize, 4000usize), (4000, 6000), (997, 991), (10, 3)] {
            for angle in [-45.0, -17.3, -2.0, 0.5, 7.0, 30.0, 45.0] {
                let (fw, fh) = frame_size(w, h, angle);
                let m = frame_to_upright(w, h, angle);
                for (x, y) in [(0.0, 0.0), (fw as f64, 0.0), (0.0, fh as f64), (fw as f64, fh as f64)] {
                    let (u, v) = m.apply(x, y);
                    assert!(u >= -1e-6 && u <= w as f64 + 1e-6, "{w}x{h} {angle}°: u {u}");
                    assert!(v >= -1e-6 && v <= h as f64 + 1e-6, "{w}x{h} {angle}°: v {v}");
                }
            }
        }
    }

    #[test]
    fn ninety_degrees_is_the_orientation_turn() {
        // The map at 90° clockwise, written for an h×w picture, is
        // `orient(_, 6)`: the warp with any interpolating kernel reads
        // exactly one pixel and copies it.
        let src = ramp(13, 7);
        let (w, h) = (src.width, src.height);
        let (sin, cos) = (90.0f64).to_radians().sin_cos();
        let m = Affine {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            tx: w as f64 / 2.0 - (h as f64 / 2.0) * cos - (w as f64 / 2.0) * sin,
            ty: h as f64 / 2.0 + (h as f64 / 2.0) * sin - (w as f64 / 2.0) * cos,
        };
        for kernel in [ResampleKernel::Lanczos3, ResampleKernel::CatmullRom, ResampleKernel::Area] {
            let turned = warp_plane(&src, &m, h, w, kernel);
            let reference = orient(&src, 6);
            assert_eq!((turned.width, turned.height), (reference.width, reference.height));
            for (a, b) in turned.data.iter().zip(&reference.data) {
                assert!((a - b).abs() < 1e-9, "{kernel:?}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn identity_map_copies_with_an_interpolating_kernel() {
        let src = ramp(31, 17);
        let out = warp_plane(&src, &Affine::IDENTITY, 31, 17, ResampleKernel::CatmullRom);
        for (a, b) in out.data.iter().zip(&src.data) {
            assert!((a - b).abs() < 1e-12);
        }
    }

    #[test]
    fn flat_stays_flat_through_any_turn() {
        let src = Plane::from_vec(120, 80, vec![0.37; 9600]);
        for kernel in ResampleKernel::ALL {
            for angle in [-30.0, -3.0, 1.5, 44.0] {
                let out = straighten(&src, angle, kernel);
                let (fw, fh) = frame_size(120, 80, angle);
                assert_eq!((out.width, out.height), (fw, fh));
                for v in &out.data {
                    assert!((v - 0.37).abs() < 1e-9, "{kernel:?} {angle}: {v}");
                }
            }
        }
    }

    #[test]
    fn a_turn_and_its_reverse_meet_in_the_middle() {
        // Turning by θ, then the frame by −θ, lands on the original picture
        // in the middle, where no edge was folded; a smooth picture
        // survives two B-spline passes to a few percent.
        let w = 200;
        let h = 140;
        let mut src = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                let fx = x as f64 / w as f64 * 6.0;
                let fy = y as f64 / h as f64 * 6.0;
                src.set(x, y, 0.5 + 0.25 * fx.sin() + 0.2 * fy.cos());
            }
        }
        let once = straighten(&src, 5.0, ResampleKernel::CatmullRom);
        let back = straighten(&once, -5.0, ResampleKernel::CatmullRom);
        // The result is centred on the original; compare the middle.
        let (bw, bh) = (back.width, back.height);
        let (ox, oy) = ((w - bw) / 2, (h - bh) / 2);
        let mut worst: f64 = 0.0;
        for y in bh / 4..3 * bh / 4 {
            for x in bw / 4..3 * bw / 4 {
                worst = worst.max((back.at(x, y) - src.at(x + ox, y + oy)).abs());
            }
        }
        assert!(worst < 0.02, "{worst}");
    }

    #[test]
    fn the_print_map_at_angle_zero_is_the_separable_resize() {
        // Through the same centre mapping, the warp of the whole frame to
        // a size gives the separable resize: both normalise the same taps,
        // both stretch the kernel by the reduction, both fold the edge.
        let src = ramp(97, 61);
        let (dw, dh) = (45, 30);
        let m = print_map(97, 61, 0.0, None, dw, dh);
        let warped = warp_plane(&src, &m, dw, dh, ResampleKernel::BSpline);
        let resized = crate::resize::resize_with(&src, dw, dh, ResampleKernel::BSpline);
        for (a, b) in warped.data.iter().zip(&resized.data) {
            assert!((a - b).abs() < 1e-9, "{a} vs {b}");
        }
        // With a crop the two agree inside; at the window's edge the
        // separable resize folds onto the edge pixel of the cut window,
        // the warp reads the negative's real neighbours beyond it.
        let crop = CropRect { x: 10, y: 5, w: 60, h: 40 };
        let m = print_map(97, 61, 0.0, Some(crop), dw, dh);
        let warped = warp_plane(&src, &m, dw, dh, ResampleKernel::BSpline);
        let window = crate::print::window_plane(&src, crop);
        let resized = crate::resize::resize_with(&window, dw, dh, ResampleKernel::BSpline);
        for y in 4..dh - 4 {
            for x in 4..dw - 4 {
                let (a, b) = (warped.at(x, y), resized.at(x, y));
                assert!((a - b).abs() < 1e-9, "{x},{y}: {a} vs {b}");
            }
        }
    }

    #[test]
    fn footprint_covers_what_the_warp_reads() {
        let src = ramp(300, 200);
        let m = print_map(300, 200, 7.0, Some(CropRect { x: 20, y: 10, w: 200, h: 150 }), 160, 120);
        for kernel in ResampleKernel::ALL {
            let rect = PixelRect { x: 30, y: 40, w: 50, h: 20 };
            let (x0, y0, x1, y1) = footprint(&m, kernel, rect, 300, 200);
            let cut = crate::print::window_plane(
                &src,
                CropRect { x: x0 as u32, y: y0 as u32, w: (x1 - x0) as u32, h: (y1 - y0) as u32 },
            );
            let window = PlaneWindow { plane: &cut, origin_x: x0, origin_y: y0, full_w: 300, full_h: 200 };
            let part = warp(window, &m, kernel, rect).expect("the footprint covers the taps");
            let whole = warp_plane(&src, &m, 160, 120, kernel);
            for y in 0..rect.h {
                for x in 0..rect.w {
                    let a = part.at(x, y);
                    let b = whole.at(rect.x + x, rect.y + y);
                    assert!((a - b).abs() < 1e-12, "{kernel:?} at {x},{y}: {a} vs {b}");
                }
            }
        }
    }

    #[test]
    fn a_short_window_is_refused() {
        let src = ramp(100, 100);
        let m = frame_to_upright(100, 100, 10.0);
        let cut = crate::print::window_plane(&src, CropRect { x: 40, y: 40, w: 10, h: 10 });
        let window = PlaneWindow { plane: &cut, origin_x: 40, origin_y: 40, full_w: 100, full_h: 100 };
        assert!(warp(window, &m, ResampleKernel::BSpline, PixelRect { x: 0, y: 0, w: 50, h: 50 }).is_err());
    }

    /// A turn alone takes the straighten's kernel whatever the recipe
    /// says; a turn with a size step takes the recipe's.
    #[test]
    fn the_turn_alone_has_its_own_kernel() {
        let (w, h) = (4032usize, 6048usize);
        let (fw, fh) = frame_size(w, h, -2.5);
        let one_to_one = print_map(w, h, -2.5, None, fw, fh);
        assert_eq!(kernel_for(&one_to_one, ResampleKernel::BSpline), STRAIGHTEN_KERNEL);
        assert_eq!(kernel_for(&one_to_one, ResampleKernel::Lanczos3), STRAIGHTEN_KERNEL);
        let crop = crate::recipe::CropRect { x: 100, y: 200, w: 3000, h: 4000 };
        let cut = print_map(w, h, -2.5, Some(crop), 3000, 4000);
        assert_eq!(kernel_for(&cut, ResampleKernel::BSpline), STRAIGHTEN_KERNEL);
        let reduced = print_map(w, h, -2.5, Some(crop), 1500, 2000);
        assert_eq!(kernel_for(&reduced, ResampleKernel::BSpline), ResampleKernel::BSpline);
        let enlarged = print_map(w, h, -2.5, None, fw * 2, fh * 2);
        assert_eq!(kernel_for(&enlarged, ResampleKernel::Lanczos3), ResampleKernel::Lanczos3);
    }

    #[test]
    fn line_angles() {
        // A horizon falling to the right by 2°: the picture turns back by 2°.
        let a = angle_of_line(0.0, 0.0, 100.0, 100.0 * (2.0f64).to_radians().tan()).unwrap();
        assert!((a + 2.0).abs() < 1e-9, "{a}");
        // Drawn the other way round, the same.
        let b = angle_of_line(100.0, 100.0 * (2.0f64).to_radians().tan(), 0.0, 0.0).unwrap();
        assert!((b + 2.0).abs() < 1e-9, "{b}");
        // A pole leaning right at the top is turned clockwise by 3°: the
        // picture turns back, anticlockwise.
        let c = angle_of_line(0.0, 100.0, 100.0 * (3.0f64).to_radians().tan(), 0.0).unwrap();
        assert!((c + 3.0).abs() < 1e-9, "{c}");
        // Level and plumb lines ask for nothing.
        assert!(angle_of_line(0.0, 5.0, 50.0, 5.0).unwrap().abs() < 1e-12);
        assert!(angle_of_line(5.0, 0.0, 5.0, 50.0).unwrap().abs() < 1e-12);
        assert!(angle_of_line(1.0, 1.0, 1.0, 1.0).is_none());
    }

    #[test]
    fn orient_map_follows_orient() {
        let (w, h) = (13usize, 9usize);
        let mut src = Plane::zeros(w, h);
        for (i, v) in src.data.iter_mut().enumerate() {
            *v = i as f64;
        }
        for o in 1..=8u16 {
            let up = orient(&src, o);
            let m = orient_map(o, w, h);
            for sy in 0..h {
                for sx in 0..w {
                    let (ux, uy) = m.apply(sx as f64 + 0.5, sy as f64 + 0.5);
                    let (ux, uy) = ((ux - 0.5).round() as usize, (uy - 0.5).round() as usize);
                    assert_eq!(up.at(ux, uy), src.at(sx, sy), "orientation {o} at {sx},{sy}");
                }
            }
        }
    }

    #[test]
    fn affine_composition_and_inverse() {
        let m = frame_to_upright(640, 480, 12.5);
        let inv = m.inverse().unwrap();
        let (x, y) = (123.4, 56.7);
        let (u, v) = m.apply(x, y);
        let (bx, by) = inv.apply(u, v);
        assert!((bx - x).abs() < 1e-9 && (by - y).abs() < 1e-9);
        let both = m.after(&inv);
        let (px, py) = both.apply(x, y);
        assert!((px - x).abs() < 1e-9 && (py - y).abs() < 1e-9);
    }
}
