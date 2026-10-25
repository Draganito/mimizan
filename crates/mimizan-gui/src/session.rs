//! What the GUI keeps after a file was developed: the decoded frame (so a
//! re-development skips the decoder), the full-resolution separation (so the
//! weights can be re-mixed without separating again) and a downscaled,
//! upright copy of the separation for the interactive preview.

use mimizan_core::calib::CameraFile;
use mimizan_core::cfa::Color;
use mimizan_core::decode::{RawFrame, SensorKind};
use mimizan_core::ingest::WhiteBalance;
use mimizan_core::mix::{mix, Weights};
use mimizan_core::pipeline::{Negative, NegativeParams};
use mimizan_core::plane::Plane;
use mimizan_core::print::{loupe_source_rect, orient, rotated, sharpen_window, window_plane, PrintParams};
use mimizan_core::recipe::{CropRect, Recipe};
use mimizan_core::resize::{fit_dims, resize, PlaneWindow};
use mimizan_core::separate::{MaskMode, Separation};
use mimizan_core::tone::{self, Tone, ToneMap};
use mimizan_core::zone;
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
    /// Nonlinear reconstruction of the last octave (`reconstruct.rs`). Off
    /// everywhere until asked for, as in the daily line since 0.12.0: a
    /// pixel that passes through it is computed, not measured, and with the
    /// baseband tensor in the separation the linear path is within 0.3 dB of
    /// it on real pictures. The switch stays under Development. Ignored
    /// unless `converter` is [`Converter::Mimizan`].
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
            fix_defects: false,
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
    /// The four CFA channels of the frame as shot; fixed for the session.
    pub raw_hist: RawHistogram,
}

/// Histogram of the four sensor channels (one per 2x2 tile position) over
/// the active area, normalised by black and white level and binned on a
/// gamma-2.2 axis like the negative histogram. Nothing but the frame goes
/// in, so it is computed once per file.
#[derive(Clone, Debug, PartialEq)]
pub struct RawHistogram {
    /// Tile positions (0,0), (1,0), (0,1), (1,1), as `RawFrame::black_tile`.
    pub bins: [[u32; 256]; 4],
    pub colors: [Color; 4],
    /// Pixels at or above the white level, per channel.
    pub clipped: [usize; 4],
    pub total: [usize; 4],
}

impl RawHistogram {
    pub fn empty() -> Self {
        Self { bins: [[0; 256]; 4], colors: [Color::G; 4], clipped: [0; 4], total: [0; 4] }
    }

    pub fn of_frame(frame: &RawFrame) -> Self {
        let crop = frame.crop;
        let (x0, y0) = (crop.x, crop.y);
        let (x1, y1) = ((crop.x + crop.w).min(frame.width), (crop.y + crop.h).min(frame.height));
        if x1 <= x0 || y1 <= y0 {
            return Self::empty();
        }
        let lut: Vec<u8> =
            (0..65536).map(|i| ((i as f64 / 65535.0).powf(1.0 / 2.2) * 255.0).round() as u8).collect();
        type RowCounts = ([[u32; 256]; 4], [usize; 4], [usize; 4]);
        let parts: Vec<RowCounts> = (y0..y1)
            .into_par_iter()
            .map(|y| {
                let mut bins = [[0u32; 256]; 4];
                let mut clipped = [0usize; 4];
                let mut total = [0usize; 4];
                let base = y * frame.width;
                for x in x0..x1 {
                    let t = (y & 1) * 2 + (x & 1);
                    let black = frame.black_at(x, y);
                    let white = frame.white_at(x, y);
                    let v = (frame.data.get(base + x) - black) / (white - black).max(1.0);
                    let q = (v.clamp(0.0, 1.0) * 65535.0).round() as usize;
                    bins[t][lut[q] as usize] += 1;
                    clipped[t] += (v >= 1.0) as usize;
                    total[t] += 1;
                }
                (bins, clipped, total)
            })
            .collect();
        let mut out = Self::empty();
        for (bins, clipped, total) in parts {
            for t in 0..4 {
                for (o, v) in out.bins[t].iter_mut().zip(bins[t].iter()) {
                    *o += v;
                }
                out.clipped[t] += clipped[t];
                out.total[t] += total[t];
            }
        }
        // Tile position t = (y & 1) * 2 + (x & 1) in absolute sensor coordinates.
        if let SensorKind::Bayer(phase) = frame.kind {
            for t in 0..4 {
                out.colors[t] = phase.color_at(t & 1, t >> 1);
            }
        }
        out
    }

    /// Fraction of a channel's pixels at the white level.
    pub fn clipped_frac(&self, t: usize) -> f64 {
        if self.total[t] == 0 {
            0.0
        } else {
            self.clipped[t] as f64 / self.total[t] as f64
        }
    }
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
/// negative: mix, placing and tone in the recipe's order, never any
/// sharpening or resampling.
#[derive(Clone, Debug, PartialEq)]
pub struct ViewParams {
    pub weights: Weights,
    pub tone: Tone,
    /// Placing in stops before the tone (`Recipe::place`).
    pub place: f64,
    pub show_mask: bool,
    /// Extra clockwise quarter turns on top of the file's orientation.
    pub rotation: u8,
    /// Window on the file-upright negative. `None` is the whole frame.
    /// The preview turns it with `rotation` before cutting.
    pub crop: Option<CropRect>,
    /// Scene zone (0 … X) whose pixels the fit preview marks, if any.
    pub hover_zone: Option<u8>,
}

/// The parts of a view the full-resolution histograms depend on (the
/// orientation is not one of them: a histogram does not turn). The crop
/// is, because it chooses which grains are counted.
type HistKey = (Weights, Tone, f64, Option<CropRect>);

impl ViewParams {
    fn hist_key(&self) -> HistKey {
        (self.weights, self.tone.clone(), self.place, self.crop)
    }
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
pub(crate) fn source_rect(orientation: u16, w: usize, h: usize, r: DetailRect) -> DetailRect {
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

/// Mix, orient, place and tone one sensor-space rectangle of a separation.
/// The rectangle is clamped to the separation. This is the picture before
/// the size step.
pub(crate) fn develop_window(
    sep: &Separation,
    rect: DetailRect,
    orientation: u16,
    weights: &Weights,
    place: f64,
    tone: &Tone,
) -> Option<SharpenLoupe> {
    let (sw, sh) = (sep.lum.width, sep.lum.height);
    if rect.w == 0 || rect.h == 0 || rect.x >= sw || rect.y >= sh {
        return None;
    }
    let rect = DetailRect {
        x: rect.x,
        y: rect.y,
        w: rect.w.min(sw - rect.x).max(1),
        h: rect.h.min(sh - rect.y).max(1),
    };
    let small = Separation {
        lum: crop(&sep.lum, rect),
        c1: crop(&sep.c1, rect),
        c2: crop(&sep.c2, rect),
        mask_max: Plane::zeros(1, 1),
        rounds: sep.rounds,
        phase: sep.phase,
        computed: sep.computed,
    };
    let mut mixed = orient(&mix(&small, weights), orientation);
    tone::render(&mut mixed, place, &tone.compile());
    let gray = mixed.data.iter().map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    Some(SharpenLoupe { gray, width: mixed.width, height: mixed.height })
}

/// Render `rect` of the negative at full resolution: crop the separation in
/// the sensor frame, mix, orient the small crop, then place and tone (or
/// the mask).
/// The rectangle is clamped to the image; the returned `rect` is the one
/// actually rendered.
pub fn render_detail(s: &Session, v: &ViewParams, rect: DetailRect) -> Option<DetailImage> {
    let sep = s.separation();
    let (sw, sh) = (sep.lum.width, sep.lum.height);
    let orientation = rotated(s.negative.orientation, v.rotation);
    let (uw, uh) = if matches!(orientation, 5..=8) { (sh, sw) } else { (sw, sh) };
    // A committed crop: `rect` is in the window. Shift it onto the full upright.
    let (rect, origin) = match v.crop {
        Some(c) => {
            let (fw, fh) = s.upright_size();
            let t = c.turned(v.rotation, fw as u32, fh as u32).sanitized(uw as u32, uh as u32);
            (
                DetailRect {
                    x: rect.x.saturating_add(t.x as usize),
                    y: rect.y.saturating_add(t.y as usize),
                    w: rect.w,
                    h: rect.h,
                },
                (t.x as usize, t.y as usize),
            )
        }
        None => (rect, (0, 0)),
    };
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
    let upright = if v.show_mask {
        let n = &s.negative;
        let mut m = Plane::zeros(src.w, src.h);
        m.data.par_chunks_mut(src.w).enumerate().for_each(|(y, row)| {
            let base = (src.y + y) * n.plane.width + src.x;
            for (x, o) in row.iter_mut().enumerate() {
                *o = f64::from(n.mask8[base + x]) / 255.0;
            }
        });
        orient(&m, orientation)
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
        let mut mixed = orient(&mix(&small, &v.weights), orientation);
        tone::render(&mut mixed, v.place, &v.tone.compile());
        mixed
    };
    debug_assert_eq!((upright.width, upright.height), (rect.w, rect.h));
    let gray: Vec<u8> = upright.data.par_iter().map(|&x| to_u8(x)).collect();
    Some(DetailImage {
        rect: DetailRect {
            x: rect.x.saturating_sub(origin.0),
            y: rect.y.saturating_sub(origin.1),
            w: upright.width,
            h: upright.height,
        },
        gray,
    })
}

pub struct SharpenLoupe {
    pub gray: Vec<u8>,
    pub width: usize,
    pub height: usize,
}

/// One box of the print at 1:1: the same resize and paper the TIFF gets,
/// on a tile around `(nx, ny)`. `view` is the box side in print pixels.
pub fn render_sharpen_loupe(
    s: &Session,
    weights: &Weights,
    recipe: &Recipe,
    orientation: u16,
    nx: f64,
    ny: f64,
    view: usize,
) -> SharpenLoupe {
    let view = view.clamp(1, 4096);
    let params = PrintParams { recipe: recipe.clone(), ..PrintParams::default() };
    let sep = s.separation();
    let (sensor_w, sensor_h) = (sep.lum.width, sep.lum.height);
    let (full_w, full_h) =
        if matches!(orientation, 5..=8) { (sensor_h, sensor_w) } else { (sensor_w, sensor_h) };
    let (sx, sy, ex, ey) = loupe_source_rect(full_w, full_h, &params, nx, ny, view);
    // USM is the only step that reads the mask, and only then is the crop
    // wide enough for its Lanczos-3 resample.
    let want_mask = recipe.sharpen.usm_amount() > 0.0;
    let (linear, mask) = if ex > sx && ey > sy {
        let upright_rect = DetailRect { x: sx, y: sy, w: ex - sx, h: ey - sy };
        let sensor = source_rect(orientation, sensor_w, sensor_h, upright_rect);
        let mixed = mix(
            &Separation {
                lum: crop(&sep.lum, sensor),
                c1: crop(&sep.c1, sensor),
                c2: crop(&sep.c2, sensor),
                mask_max: Plane::zeros(1, 1),
                rounds: sep.rounds,
                phase: sep.phase,
                computed: sep.computed,
            },
            weights,
        );
        let mask = want_mask.then(|| orient(&crop_mask(s, sensor), orientation));
        (orient(&mixed, orientation), mask)
    } else {
        (Plane::zeros(1, 1), None)
    };
    let window = if ex > sx && ey > sy {
        PlaneWindow { plane: &linear, origin_x: sx, origin_y: sy, full_w, full_h }
    } else {
        PlaneWindow { plane: &linear, origin_x: 0, origin_y: 0, full_w, full_h }
    };
    let mask_window = mask.as_ref().map(|m| PlaneWindow {
        plane: m,
        origin_x: window.origin_x,
        origin_y: window.origin_y,
        full_w,
        full_h,
    });
    let plane = match sharpen_window(window, mask_window, &params, nx, ny, view) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("{e}");
            return SharpenLoupe { gray: Vec::new(), width: 0, height: 0 };
        }
    };
    let gray = plane.data.iter().map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    SharpenLoupe { gray, width: plane.width, height: plane.height }
}

fn crop_mask(s: &Session, r: DetailRect) -> Plane {
    let n = &s.negative;
    let mut out = Plane::zeros(r.w, r.h);
    for y in 0..r.h {
        let base = (r.y + y) * n.plane.width + r.x;
        let row = out.row_mut(y);
        for (x, o) in row.iter_mut().enumerate() {
            *o = f64::from(n.mask8[base + x]) / 255.0;
        }
    }
    out
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
    /// on a gamma-2.2 axis (the scan, before any placing), and the mix after
    /// `place` stops and `tone` (what the TIFF gets). Values are quantised
    /// to 16 bits first, so the clipping counts are the TIFF's own 0 and
    /// 65535 pixels. Nothing is downscaled: a resampled copy rings at edges
    /// (false black and white) and averages isolated clipped pixels away, so
    /// its histogram ends are wrong by an order of magnitude.
    pub fn of_separation(
        sep: &Separation,
        w: &Weights,
        tone: &ToneMap,
        place: f64,
        window: Option<(usize, usize, usize, usize)>,
    ) -> (Self, Self) {
        // 16-bit input -> 8-bit bin, for both encodings.
        let lut_neg: Vec<u8> =
            (0..65536).map(|i| ((i as f64 / 65535.0).powf(1.0 / 2.2) * 255.0).round() as u8).collect();
        let lut_view: Vec<u8> =
            (0..65536).map(|i| (tone.eval(i as f64 / 65535.0) * 255.0).round() as u8).collect();
        let view_clips: Vec<bool> =
            (0..65536).map(|i| matches!(q16(tone.eval(i as f64 / 65535.0)), 0 | 65535)).collect();
        let k1 = w.g - w.r - w.b;
        let k2 = 2.0 * (w.r - w.b);
        let width = sep.lum.width;
        let height = sep.lum.height;
        let (x0, y0, x1, y1) = match window {
            Some((x, y, w, h)) => {
                let x0 = x.min(width);
                let y0 = y.min(height);
                (x0, y0, (x + w).min(width).max(x0), (y + h).min(height).max(y0))
            }
            None => (0, 0, width, height),
        };
        let global = 2f64.powf(place);
        // Row-ordered partial counts, merged deterministically.
        let parts: Vec<Counts> = (y0..y1)
            .into_par_iter()
            .map(|y| {
                let (l, c1, c2) = (sep.lum.row(y), sep.c1.row(y), sep.c2.row(y));
                let mut c = Counts::default();
                for x in x0..x1 {
                    let m = l[x] + k1 * c1[x] + k2 * c2[x];
                    let q = q16(m);
                    c.neg[lut_neg[q] as usize] += 1;
                    c.neg_low += (q == 0) as usize;
                    c.neg_high += (q == 65535) as usize;
                    let qv = q16(m * global);
                    let (bin, clips) = (lut_view[qv], view_clips[qv]);
                    c.view[bin as usize] += 1;
                    if clips {
                        if bin == 0 {
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
        let total = (x1 - x0) * (y1 - y0);
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
    /// 255 where the curve's input lies in `ViewParams::hover_zone`, same
    /// layout as `gray`; `None` when no zone is hovered.
    pub zone_mask: Option<Vec<u8>>,
    pub hist_view: Histogram,
    pub hist_negative: Histogram,
}

/// Full-resolution histograms kept between renders: they depend on the
/// weights, the tone and the placing, not on the mask view or the
/// hovered zone.
pub struct HistCache {
    key: Option<(*const Session, HistKey)>,
    hists: (Histogram, Histogram),
}

impl Default for HistCache {
    fn default() -> Self {
        Self { key: None, hists: (Histogram::empty(), Histogram::empty()) }
    }
}

impl HistCache {
    fn get(&mut self, s: &Session, v: &ViewParams, tone: &ToneMap) -> (Histogram, Histogram) {
        let key = (s as *const Session, v.hist_key());
        if self.key.as_ref() != Some(&key) {
            let window = v.crop.map(|c| {
                let (fw, fh) = s.upright_size();
                let c = c.sanitized(fw as u32, fh as u32);
                let (sw, sh) = (s.separation().lum.width, s.separation().lum.height);
                let r = source_rect(
                    s.negative.orientation,
                    sw,
                    sh,
                    DetailRect { x: c.x as usize, y: c.y as usize, w: c.w as usize, h: c.h as usize },
                );
                (r.x, r.y, r.w, r.h)
            });
            self.hists = Histogram::of_separation(s.separation(), &v.weights, tone, v.place, window);
            self.key = Some(key);
        }
        self.hists.clone()
    }
}

/// Scale a full-resolution upright window onto the preview plane.
fn scale_crop(c: CropRect, scale: f64, dst_w: usize, dst_h: usize) -> CropRect {
    let x = (f64::from(c.x) * scale).round().max(0.0) as u32;
    let y = (f64::from(c.y) * scale).round().max(0.0) as u32;
    let w = (f64::from(c.w) * scale).round().max(1.0) as u32;
    let h = (f64::from(c.h) * scale).round().max(1.0) as u32;
    CropRect { x, y, w, h }.sanitized(dst_w as u32, dst_h as u32)
}

/// 8-bit grey preview of the whole negative: mix, place and tone (or the
/// mask), in the picture's final orientation.
pub fn render_preview(s: &Session, v: &ViewParams, cache: &mut HistCache) -> PreviewImage {
    let pp = &s.preview;
    let to_u8 = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    if v.show_mask {
        let mut plane = orient(&pp.mask, rotated(1, v.rotation));
        if let Some(c) = v.crop {
            let (fw, fh) = s.upright_size();
            let turned = c.turned(v.rotation, fw as u32, fh as u32);
            plane = window_plane(&plane, scale_crop(turned, s.preview.scale, plane.width, plane.height));
        }
        let gray: Vec<u8> = plane.data.par_iter().map(|&m| to_u8(m)).collect();
        let hist_view = Histogram::of_bytes(&gray);
        return PreviewImage {
            width: plane.width,
            height: plane.height,
            rotation: v.rotation,
            gray,
            zone_mask: None,
            hist_view,
            hist_negative: Histogram::empty(),
        };
    }
    let tone_map = v.tone.compile();
    // Histograms from the full-resolution separation, not the preview copy.
    let (hist_negative, hist_view) = cache.get(s, v, &tone_map);
    let mut mixed = orient(&mix(&pp.sep, &v.weights), rotated(1, v.rotation));
    if let Some(c) = v.crop {
        let (fw, fh) = s.upright_size();
        let turned = c.turned(v.rotation, fw as u32, fh as u32);
        let scaled = scale_crop(turned, s.preview.scale, mixed.width, mixed.height);
        mixed = window_plane(&mixed, scaled);
    }
    let zone_mask = v.hover_zone.map(|k| {
        // The scene zone after placing.
        let (lo, hi) = (k as f64 - 5.5, k as f64 - 4.5);
        mixed
            .data
            .par_iter()
            .map(|&l| {
                // Stops around the grey card (paper, equation 3); the core
                // keeps this reading for its tests only.
                let st = (l * 2f64.powf(v.place)).max(1e-6).log2() - zone::GRAY_CARD.log2();
                // The outer zones take everything beyond them.
                let inside = (st >= lo || k == 0) && (st < hi || k as usize == zone::ZONES - 1);
                if inside {
                    255
                } else {
                    0
                }
            })
            .collect()
    });
    tone::render(&mut mixed, v.place, &tone_map);
    let plane = mixed;
    let gray: Vec<u8> = plane.data.par_iter().map(|&x| to_u8(x)).collect();
    PreviewImage {
        width: plane.width,
        height: plane.height,
        rotation: v.rotation,
        gray,
        zone_mask,
        hist_view,
        hist_negative,
    }
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

    /// The desktop starts without the reconstruction, like the daily line
    /// and the library; the last octave is reconstructed only when the
    /// switch is set.
    #[test]
    fn reconstruct_starts_off() {
        let p = DevelopParams::default();
        assert!(!p.reconstruct);
        assert!(p.negative_params(None).separate.reconstruct.is_none());
        let on = DevelopParams { reconstruct: true, ..p };
        assert!(on.negative_params(None).separate.reconstruct.is_some());
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
        let curve = Tone::Look(look).compile();
        let (neg, view) = Histogram::of_separation(&sep, &wts, &curve, 0.0, None);

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

    fn sweep(w: usize, h: usize) -> Separation {
        let mut lum = Plane::zeros(w, h);
        for (i, l) in lum.data.iter_mut().enumerate() {
            *l = 0.9 * i as f64 / (w * h - 1) as f64;
        }
        Separation {
            lum,
            c1: Plane::zeros(w, h),
            c2: Plane::zeros(w, h),
            mask_max: Plane::zeros(1, 1),
            rounds: 1,
            phase: BayerPhase::RGGB,
            computed: 0.0,
        }
    }

    /// The view histogram must equal the histogram of the picture the
    /// export renders: place, then the look or the paper, 16-bit first.
    #[test]
    fn of_separation_matches_the_rendered_picture() {
        let sep = sweep(40, 24);
        let wts = Weights::default();
        for (tone, place) in
            [(Tone::Look(LookFile::neutral()).compile(), -0.4), (Tone::paper().compile(), 0.2)]
        {
            let (_, view) = Histogram::of_separation(&sep, &wts, &tone, place, None);
            let mut pic = mix(&sep, &wts);
            tone::place(&mut pic, place);
            let mut ev = [0u32; 256];
            for &l in &pic.data {
                let lin = q16(l) as f64 / 65535.0;
                ev[(tone.eval(lin) * 255.0).round() as usize] += 1;
            }
            assert_eq!(view.bins, ev);
        }
    }
}
