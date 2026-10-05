//! What the GUI keeps after a file was developed: the decoded frame (so a
//! re-development skips the decoder), the full-resolution separation (so the
//! weights can be re-mixed without separating again) and a downscaled,
//! upright copy of the separation for the interactive preview.

use mimizan_core::calib::{CameraFile, LookFile};
use mimizan_core::curve::Curve;
use mimizan_core::decode::RawFrame;
use mimizan_core::ingest::WhiteBalance;
use mimizan_core::mix::{mix, Weights};
use mimizan_core::pipeline::{Negative, NegativeParams};
use mimizan_core::plane::Plane;
use mimizan_core::print::{orient, rotate_gray8_cw};
use mimizan_core::resize::{fit_dims, resize};
use mimizan_core::separate::{MaskMode, Separation};
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::Arc;

/// Longest preview edge in pixels (the texture the GUI shows).
pub const PREVIEW_MAX: usize = 1800;

/// Settings that need a new separation (ingest + separate), not just a re-mix.
/// `converter` chooses that separation or a demosaicer. `Mimizan` is the
/// unchanged path.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DevelopParams {
    pub wb: WhiteBalance,
    pub mask: MaskMode,
    pub fix_defects: bool,
    /// Nonlinear reconstruction of the last octave (`reconstruct.rs`): off by
    /// default, because a pixel that passes through it is computed, not measured.
    /// Ignored unless `converter` is [`Converter::Mimizan`].
    pub reconstruct: bool,
    pub converter: Converter,
}

/// How the mosaic becomes three channels. `Mimizan` does not demosaic.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Converter {
    Mimizan,
    Amaze,
    Rcd,
    Dcb,
    Lmmse,
}

impl Converter {
    pub fn label(self) -> &'static str {
        match self {
            Self::Mimizan => "Mimizan",
            Self::Amaze => "AMaZE",
            Self::Rcd => "RCD",
            Self::Dcb => "DCB",
            Self::Lmmse => "LMMSE",
        }
    }

    pub const ALL: [Converter; 5] = [Self::Mimizan, Self::Amaze, Self::Rcd, Self::Dcb, Self::Lmmse];
}

impl Default for DevelopParams {
    fn default() -> Self {
        // Pixel-adaptive Dubois separation is the default (RESULTS.md, Kodak
        // benchmark); the fixed estimator and the block mask stay available
        // under Development.
        Self {
            wb: WhiteBalance::AsShot,
            mask: MaskMode::Dubois,
            fix_defects: true,
            reconstruct: false,
            converter: Converter::Mimizan,
        }
    }
}

impl DevelopParams {
    pub fn negative_params(&self, cam: Option<CameraFile>) -> NegativeParams {
        let mut p = NegativeParams { keep_separation: true, ..NegativeParams::default() };
        p.ingest.wb = self.wb;
        p.ingest.fix_defects = self.fix_defects;
        p.separate.mask = self.mask;
        if self.reconstruct {
            p.separate.reconstruct = Some(Default::default());
        }
        if let Some(c) = cam {
            p = p.with_camera_file(c);
        }
        p
    }
}

/// Upright, downscaled separation plus the sharpening mask at the same size.
pub struct PreviewPlanes {
    pub sep: Separation,
    /// `max(M)` with saturated pixels forced to 1 (the sidecar as a plane).
    pub mask: Plane,
    /// Preview width / upright full width.
    pub scale: f64,
}

pub struct Session {
    pub path: PathBuf,
    pub frame: Arc<RawFrame>,
    pub negative: Negative,
    pub camera_file: Option<CameraFile>,
    pub camera_file_path: Option<PathBuf>,
    pub develop: DevelopParams,
    pub preview: PreviewPlanes,
}

impl Session {
    pub fn separation(&self) -> &Separation {
        self.negative.separation.as_ref().expect("session negatives keep their separation")
    }

    /// Full-resolution sharpening mask (the sidecar bytes as a plane).
    pub fn mask_plane(&self) -> Plane {
        let n = &self.negative;
        Plane::from_vec(
            n.plane.width,
            n.plane.height,
            n.mask8.par_iter().map(|&v| f64::from(v) / 255.0).collect(),
        )
    }

    /// Upright size of the full negative.
    pub fn upright_size(&self) -> (usize, usize) {
        let (w, h) = (self.negative.plane.width, self.negative.plane.height);
        if matches!(self.negative.orientation, 5..=8) {
            (h, w)
        } else {
            (w, h)
        }
    }

    /// Weights the camera file suggests for this image's balance, or native.
    pub fn default_weights(&self) -> Weights {
        self.camera_file.as_ref().map(|c| c.weights_balanced(self.negative.info.wb)).unwrap_or_default()
    }

    /// Balance multipliers of this development (G = 1).
    pub fn wb(&self) -> [f64; 3] {
        self.negative.info.wb
    }
}

/// Build the preview planes from a negative that kept its separation.
pub fn preview_planes(neg: &Negative) -> PreviewPlanes {
    let sep = neg.separation.as_ref().expect("keep_separation");
    let o = neg.orientation;
    let lum = orient(&sep.lum, o);
    let (w, h) = (lum.width, lum.height);
    let (pw, ph) = fit_dims(w, h, PREVIEW_MAX, PREVIEW_MAX);
    let (pw, ph) = (pw.min(w), ph.min(h));
    let small = |p: &Plane| {
        let up = orient(p, o);
        if (up.width, up.height) == (pw, ph) {
            up
        } else {
            resize(&up, pw, ph)
        }
    };
    let mask_full = Plane::from_vec(
        neg.plane.width,
        neg.plane.height,
        neg.mask8.par_iter().map(|&v| f64::from(v) / 255.0).collect(),
    );
    let mask = small(&mask_full);
    drop(mask_full);
    let sep_small = Separation {
        lum: if (w, h) == (pw, ph) { lum } else { resize(&lum, pw, ph) },
        c1: small(&sep.c1),
        c2: small(&sep.c2),
        mask_max: small(&sep.mask_max),
        rounds: sep.rounds,
        phase: sep.phase,
        computed: sep.computed,
    };
    PreviewPlanes { sep: sep_small, mask, scale: pw as f64 / w as f64 }
}

/// Everything the preview depends on besides the session. The preview is the
/// negative: mix and curve, never any sharpening.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewParams {
    pub weights: Weights,
    pub look: LookFile,
    pub show_mask: bool,
    /// Extra clockwise quarter turns on top of the file's orientation.
    pub rotation: u8,
}

/// A window of the full-resolution negative, in upright (and rotated)
/// pixel coordinates, that the view wants pixel for pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetailRect {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
}

/// 1:1 crop of the negative for the zoomed view.
pub struct DetailImage {
    pub rect: DetailRect,
    pub gray: Vec<u8>,
}

/// Map a rectangle of the upright image (after `orientation`) back to the
/// sensor frame of a `w`×`h` source, using the same table as `orient`.
fn source_rect(orientation: u16, w: usize, h: usize, r: DetailRect) -> DetailRect {
    let map = |ox: usize, oy: usize| -> (usize, usize) {
        match orientation {
            2 => (w - 1 - ox, oy),
            3 => (w - 1 - ox, h - 1 - oy),
            4 => (ox, h - 1 - oy),
            5 => (oy, ox),
            6 => (oy, h - 1 - ox),
            7 => (w - 1 - oy, h - 1 - ox),
            8 => (w - 1 - oy, ox),
            _ => (ox, oy),
        }
    };
    let corners =
        [map(r.x, r.y), map(r.x + r.w - 1, r.y), map(r.x, r.y + r.h - 1), map(r.x + r.w - 1, r.y + r.h - 1)];
    let x0 = corners.iter().map(|c| c.0).min().unwrap_or(0);
    let x1 = corners.iter().map(|c| c.0).max().unwrap_or(0);
    let y0 = corners.iter().map(|c| c.1).min().unwrap_or(0);
    let y1 = corners.iter().map(|c| c.1).max().unwrap_or(0);
    DetailRect { x: x0, y: y0, w: x1 - x0 + 1, h: y1 - y0 + 1 }
}

fn crop(p: &Plane, r: DetailRect) -> Plane {
    let mut out = Plane::zeros(r.w, r.h);
    out.data.par_chunks_mut(r.w).enumerate().for_each(|(y, row)| {
        row.copy_from_slice(&p.row(r.y + y)[r.x..r.x + r.w]);
    });
    out
}

/// Render `rect` of the negative at full resolution: crop the separation in
/// the sensor frame, mix, curve (or the mask), then orient the small crop.
/// The rectangle is clamped to the image; the returned `rect` is the one
/// actually rendered.
pub fn render_detail(s: &Session, v: &ViewParams, rect: DetailRect) -> Option<DetailImage> {
    let sep = s.separation();
    let (sw, sh) = (sep.lum.width, sep.lum.height);
    let orientation = mimizan_core::print::rotated(s.negative.orientation, v.rotation);
    let (uw, uh) = if matches!(orientation, 5..=8) { (sh, sw) } else { (sw, sh) };
    if rect.x >= uw || rect.y >= uh {
        return None;
    }
    let rect = DetailRect {
        x: rect.x,
        y: rect.y,
        w: rect.w.min(uw - rect.x).max(1),
        h: rect.h.min(uh - rect.y).max(1),
    };
    let src = source_rect(orientation, sw, sh, rect);
    let to_u8 = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    let plane = if v.show_mask {
        let n = &s.negative;
        let mut m = Plane::zeros(src.w, src.h);
        m.data.par_chunks_mut(src.w).enumerate().for_each(|(y, row)| {
            let base = (src.y + y) * n.plane.width + src.x;
            for (x, o) in row.iter_mut().enumerate() {
                *o = f64::from(n.mask8[base + x]) / 255.0;
            }
        });
        m
    } else {
        let small = Separation {
            lum: crop(&sep.lum, src),
            c1: crop(&sep.c1, src),
            c2: crop(&sep.c2, src),
            mask_max: Plane::zeros(1, 1),
            rounds: sep.rounds,
            phase: sep.phase,
            computed: sep.computed,
        };
        let mixed = mix(&small, &v.weights);
        Curve::from_look(&v.look).apply(&mixed)
    };
    let upright = orient(&plane, orientation);
    debug_assert_eq!((upright.width, upright.height), (rect.w, rect.h));
    let gray: Vec<u8> = upright.data.par_iter().map(|&x| to_u8(x)).collect();
    Some(DetailImage { rect: DetailRect { w: upright.width, h: upright.height, ..rect }, gray })
}

/// 256-bin histogram of an 8-bit view plus the clipped fractions.
#[derive(Clone, Debug, PartialEq)]
pub struct Histogram {
    pub bins: [u32; 256],
    pub total: usize,
    /// Mean of the values (0..1).
    pub mean: f64,
    /// Pixels that are 0 / full scale in the 16-bit output (true clipping,
    /// not the width of the end bins).
    pub low: usize,
    pub high: usize,
}

/// 16-bit quantisation of a linear value in [0,1], as the TIFF writer does it.
#[inline]
fn q16(x: f64) -> usize {
    (x.clamp(0.0, 1.0) * 65535.0).round() as usize
}

/// Partial histogram counts of one row (or their sum).
struct Counts {
    neg: [u32; 256],
    view: [u32; 256],
    neg_low: usize,
    neg_high: usize,
    view_low: usize,
    view_high: usize,
}

impl Default for Counts {
    fn default() -> Self {
        Self { neg: [0; 256], view: [0; 256], neg_low: 0, neg_high: 0, view_low: 0, view_high: 0 }
    }
}

impl Counts {
    fn add(&mut self, o: &Self) {
        for (a, b) in self.neg.iter_mut().zip(o.neg.iter()) {
            *a += b;
        }
        for (a, b) in self.view.iter_mut().zip(o.view.iter()) {
            *a += b;
        }
        self.neg_low += o.neg_low;
        self.neg_high += o.neg_high;
        self.view_low += o.view_low;
        self.view_high += o.view_high;
    }
}

impl Histogram {
    pub fn empty() -> Self {
        Self { bins: [0; 256], total: 0, mean: 0.0, low: 0, high: 0 }
    }

    fn from_bins(bins: [u32; 256], total: usize, low: usize, high: usize) -> Self {
        let sum: u64 = bins.iter().enumerate().map(|(i, &n)| i as u64 * n as u64).sum();
        Self {
            bins,
            total,
            mean: if total > 0 { sum as f64 / (255.0 * total as f64) } else { 0.0 },
            low,
            high,
        }
    }

    /// Histograms of the full-resolution negative in one pass: the linear mix
    /// on a gamma-2.2 axis, and the mix after `curve` (what the TIFF gets).
    /// Values are quantised to 16 bits first, so the clipping counts are the
    /// TIFF's own 0 and 65535 pixels. Nothing is downscaled: a resampled copy
    /// rings at edges (false black and white) and averages isolated clipped
    /// pixels away, so its histogram ends are wrong by an order of magnitude.
    pub fn of_separation(sep: &Separation, w: &Weights, curve: &Curve) -> (Self, Self) {
        // 16-bit input -> 8-bit bin, for both encodings.
        let lut_neg: Vec<u8> =
            (0..65536).map(|i| ((i as f64 / 65535.0).powf(1.0 / 2.2) * 255.0).round() as u8).collect();
        let lut_view: Vec<u8> =
            (0..65536).map(|i| (curve.eval(i as f64 / 65535.0) * 255.0).round() as u8).collect();
        let view_clips: Vec<bool> =
            (0..65536).map(|i| matches!(q16(curve.eval(i as f64 / 65535.0)), 0 | 65535)).collect();
        let k1 = w.g - w.r - w.b;
        let k2 = 2.0 * (w.r - w.b);
        let width = sep.lum.width;
        // Row-ordered partial counts, merged deterministically.
        let parts: Vec<Counts> = (0..sep.lum.height)
            .into_par_iter()
            .map(|y| {
                let (l, c1, c2) = (sep.lum.row(y), sep.c1.row(y), sep.c2.row(y));
                let mut c = Counts::default();
                for x in 0..width {
                    let m = l[x] + k1 * c1[x] + k2 * c2[x];
                    let q = q16(m);
                    c.neg[lut_neg[q] as usize] += 1;
                    c.view[lut_view[q] as usize] += 1;
                    c.neg_low += (q == 0) as usize;
                    c.neg_high += (q == 65535) as usize;
                    if view_clips[q] {
                        if lut_view[q] == 0 {
                            c.view_low += 1;
                        } else {
                            c.view_high += 1;
                        }
                    }
                }
                c
            })
            .collect();
        let mut all = Counts::default();
        for c in parts {
            all.add(&c);
        }
        let total = width * sep.lum.height;
        (
            Self::from_bins(all.neg, total, all.neg_low, all.neg_high),
            Self::from_bins(all.view, total, all.view_low, all.view_high),
        )
    }

    pub fn of_bytes(g: &[u8]) -> Self {
        // Row-ordered partial histograms, merged deterministically.
        let parts: Vec<([u32; 256], u64)> = g
            .par_chunks(1 << 16)
            .map(|c| {
                let mut b = [0u32; 256];
                let mut s = 0u64;
                for &v in c {
                    b[v as usize] += 1;
                    s += v as u64;
                }
                (b, s)
            })
            .collect();
        let mut bins = [0u32; 256];
        let mut sum = 0u64;
        for (b, s) in parts {
            for (o, v) in bins.iter_mut().zip(b.iter()) {
                *o += v;
            }
            sum += s;
        }
        let total = g.len();
        Self {
            bins,
            total,
            mean: if total > 0 { sum as f64 / (255.0 * total as f64) } else { 0.0 },
            low: bins[0] as usize,
            high: bins[255] as usize,
        }
    }

    /// Fraction of pixels clipped to black.
    pub fn clipped_low(&self) -> f64 {
        self.frac(self.low)
    }

    /// Fraction of pixels clipped to white.
    pub fn clipped_high(&self) -> f64 {
        self.frac(self.high)
    }

    fn frac(&self, n: usize) -> f64 {
        if self.total == 0 {
            0.0
        } else {
            n as f64 / self.total as f64
        }
    }
}

/// What the preview worker hands back: the 8-bit view, plus two histograms of
/// the full-resolution negative: after the curve (the TIFF) and the linear mix
/// before it (binned on a gamma-2.2 axis so the shadows are readable).
pub struct PreviewImage {
    pub width: usize,
    pub height: usize,
    /// Quarter turns this image was rendered with (`ViewParams::rotation`).
    pub rotation: u8,
    pub gray: Vec<u8>,
    pub hist_view: Histogram,
    pub hist_negative: Histogram,
}

/// Full-resolution histograms kept between renders: they depend on the
/// weights and the look only, not on rotation or the mask view.
pub struct HistCache {
    key: Option<(*const Session, Weights, LookFile)>,
    hists: (Histogram, Histogram),
}

impl Default for HistCache {
    fn default() -> Self {
        Self { key: None, hists: (Histogram::empty(), Histogram::empty()) }
    }
}

impl HistCache {
    fn get(&mut self, s: &Session, v: &ViewParams, curve: &Curve) -> (Histogram, Histogram) {
        let key = (s as *const Session, v.weights, v.look.clone());
        if self.key.as_ref() != Some(&key) {
            self.hists = Histogram::of_separation(s.separation(), &v.weights, curve);
            self.key = Some(key);
        }
        self.hists.clone()
    }
}

/// 8-bit grey preview of the whole negative: mix and curve (or the mask).
pub fn render_preview(s: &Session, v: &ViewParams, cache: &mut HistCache) -> PreviewImage {
    let pp = &s.preview;
    let (w, h) = (pp.sep.lum.width, pp.sep.lum.height);
    let to_u8 = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    if v.show_mask {
        let gray: Vec<u8> = pp.mask.data.par_iter().map(|&m| to_u8(m)).collect();
        let hist_view = Histogram::of_bytes(&gray);
        let (width, height, gray) = rotate_gray8_cw(w, h, &gray, v.rotation);
        return PreviewImage {
            width,
            height,
            rotation: v.rotation,
            gray,
            hist_view,
            hist_negative: Histogram::empty(),
        };
    }
    let curve = Curve::from_look(&v.look);
    // Histograms from the full-resolution separation, not the preview copy.
    let (hist_negative, hist_view) = cache.get(s, v, &curve);
    let mixed = mix(&pp.sep, &v.weights);
    let plane = curve.apply(&mixed);
    let gray: Vec<u8> = plane.data.par_iter().map(|&x| to_u8(x)).collect();
    let (width, height, gray) = rotate_gray8_cw(w, h, &gray, v.rotation);
    PreviewImage { width, height, rotation: v.rotation, gray, hist_view, hist_negative }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Orienting a sensor-frame crop must equal cropping the oriented image,
    /// for every TIFF orientation and a rectangle off-centre in both axes.
    #[test]
    fn source_rect_inverts_orient() {
        let (w, h) = (13usize, 9usize);
        let mut full = Plane::zeros(w, h);
        for (i, v) in full.data.iter_mut().enumerate() {
            *v = i as f64;
        }
        for o in 1..=8u16 {
            let upright = orient(&full, o);
            let r = DetailRect { x: 2, y: 1, w: 5, h: 3 };
            let expected: Vec<f64> =
                (0..r.h).flat_map(|y| upright.row(r.y + y)[r.x..r.x + r.w].to_vec()).collect();
            let src = source_rect(o, w, h, r);
            let got = orient(&crop(&full, src), o);
            assert_eq!((got.width, got.height), (r.w, r.h), "orientation {o}");
            assert_eq!(got.data, expected, "orientation {o}");
        }
    }
}

#[cfg(test)]
mod histogram_tests {
    use super::*;
    use mimizan_core::calib::LookFile;
    use mimizan_core::cfa::BayerPhase;

    /// The one-pass full-resolution histogram must agree with the plain
    /// per-pixel computation, including the 16-bit clipping counts.
    #[test]
    fn of_separation_matches_direct_computation() {
        let (w, h) = (64usize, 3usize);
        let mut lum = Plane::zeros(w, h);
        let mut c1 = Plane::zeros(w, h);
        let mut c2 = Plane::zeros(w, h);
        for (i, ((l, a), b)) in
            lum.data.iter_mut().zip(c1.data.iter_mut()).zip(c2.data.iter_mut()).enumerate()
        {
            // Sweep from below black to above white, with chroma offsets.
            *l = -0.1 + 1.3 * i as f64 / (w * h - 1) as f64;
            *a = 0.02 * ((i % 7) as f64 - 3.0);
            *b = 0.01 * ((i % 5) as f64 - 2.0);
        }
        let sep = Separation {
            lum,
            c1,
            c2,
            mask_max: Plane::zeros(1, 1),
            rounds: 1,
            phase: BayerPhase::RGGB,
            computed: 0.0,
        };
        let wts = Weights { r: 0.3, g: 0.5, b: 0.2 };
        let look = LookFile::load(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../look/reference.json"
        )))
        .unwrap();
        let curve = Curve::from_look(&look);
        let (neg, view) = Histogram::of_separation(&sep, &wts, &curve);

        let mixed = mix(&sep, &wts);
        let mut en = [0u32; 256];
        let mut ev = [0u32; 256];
        let (mut nl, mut nh, mut vl, mut vh) = (0, 0, 0, 0);
        for &m in &mixed.data {
            let q = q16(m);
            nl += (q == 0) as usize;
            nh += (q == 65535) as usize;
            let lin = q as f64 / 65535.0;
            en[(lin.powf(1.0 / 2.2) * 255.0).round() as usize] += 1;
            let v = curve.eval(lin);
            ev[(v * 255.0).round() as usize] += 1;
            vl += (q16(v) == 0) as usize;
            vh += (q16(v) == 65535) as usize;
        }
        assert_eq!(neg.bins, en);
        assert_eq!(view.bins, ev);
        assert_eq!((neg.low, neg.high), (nl, nh));
        assert_eq!((view.low, view.high), (vl, vh));
        assert!(nl > 0 && nh > 0, "sweep must clip at both ends");
        assert_eq!(neg.total, w * h);
    }
}
