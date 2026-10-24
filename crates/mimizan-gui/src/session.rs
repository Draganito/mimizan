//! What the GUI keeps after a file was developed: the decoded frame (so a
//! re-development skips the decoder), the full-resolution separation (so the
//! weights can be re-mixed without separating again) and a downscaled,
//! upright copy of the separation for the interactive preview.
//!
//! A monochrome sensor has no colour filter. The GUI keeps the measured
//! plane and a preview of it. It does not invent chrominance planes.

use mimizan_core::calib::CameraFile;
use mimizan_core::cfa::Color;
use mimizan_core::decode::{RawFrame, SensorKind};
use mimizan_core::ingest::WhiteBalance;
use mimizan_core::mix::{mix, Weights};
use mimizan_core::pipeline::{Negative, NegativeParams};
use mimizan_core::plane::Plane;
use mimizan_core::print::{
    layout, loupe_source_rect, orient, rotated, sharpen_window, source_window, window_plane, PrintParams,
};
use mimizan_core::recipe::{CropRect, Recipe};
use mimizan_core::resize::{fit_dims, resize, resize_rect, source_coverage, PixelRect, PlaneWindow};
use mimizan_core::separate::{MaskMode, Separation};
use mimizan_core::tone::{Tone, ToneMap};
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// Longest preview edge in pixels (the texture the GUI shows).
pub const PREVIEW_MAX: usize = 1800;

/// Settings that need a new separation (ingest + separate), not just a re-mix.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DevelopParams {
    pub wb: WhiteBalance,
    pub mask: MaskMode,
    /// Defect repair. The desktop starts with it off (every photosite stays
    /// as measured, the Negative window has the switch); the command keeps
    /// it on until `--no-defects`, because a batch has nobody looking.
    pub fix_defects: bool,
    /// Nonlinear reconstruction of the last octave (`reconstruct.rs`). Off
    /// everywhere until asked for: a pixel that passes through it is
    /// computed, not measured. Since the baseband tensor in the separation
    /// the linear path is within 0.3 dB of it on real pictures and above it
    /// at the camera's own band limit; it still pays on pixel-sharp
    /// synthetic material, so the switch stays.
    pub reconstruct: bool,
}

impl Default for DevelopParams {
    fn default() -> Self {
        // Pixel-adaptive Dubois separation is the default; the fixed estimator
        // and the block mask stay available under Development.
        Self { wb: WhiteBalance::AsShot, mask: MaskMode::Dubois, fix_defects: false, reconstruct: false }
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

/// One developed file, shared read-only (`Arc`) between the UI and the
/// workers. Owns the decoded frame (a re-development skips the decoder),
/// the full-resolution negative with its separation (a new mix skips the
/// separation) and the upright preview planes the interactive view draws.
pub struct Session {
    /// Unique per development; the caches key on it, never on the address.
    pub id: u64,
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

/// The next session id. Starts at 1 so 0 can mean "none" in a cache.
pub fn next_session_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Histogram of the four sensor channels (one per 2x2 tile position) over
/// the active area, normalised by black and white level and binned on a
/// gamma-2.2 axis like the negative histogram. A monochrome sensor is one
/// channel in `bins[0]`. Nothing but the frame goes in, so it is computed
/// once per file.
#[derive(Clone, Debug, PartialEq)]
pub struct RawHistogram {
    /// Tile positions (0,0), (1,0), (0,1), (1,1), as `RawFrame::black_tile`.
    pub bins: [[u32; 256]; 4],
    pub colors: [Color; 4],
    /// Pixels at or above the white level, per channel.
    pub clipped: [usize; 4],
    pub total: [usize; 4],
    /// One measured channel, no colour filter.
    pub mono: bool,
}

impl RawHistogram {
    fn empty() -> Self {
        Self { bins: [[0; 256]; 4], colors: [Color::G; 4], clipped: [0; 4], total: [0; 4], mono: false }
    }

    /// Count the active area of the frame.
    pub fn of_frame(frame: &RawFrame) -> Self {
        let crop = frame.crop;
        let (x0, y0) = (crop.x, crop.y);
        let (x1, y1) = ((crop.x + crop.w).min(frame.width), (crop.y + crop.h).min(frame.height));
        if x1 <= x0 || y1 <= y0 {
            return Self::empty();
        }
        let lut = neg_lut();
        type RowCounts = ([[u32; 256]; 4], [usize; 4], [usize; 4]);
        let parts: Vec<RowCounts> = (y0..y1)
            .into_par_iter()
            .map(|y| {
                let mut bins = [[0u32; 256]; 4];
                let mut clipped = [0usize; 4];
                let mut total = [0usize; 4];
                let mono = matches!(frame.kind, SensorKind::Mono);
                let base = y * frame.width;
                for x in x0..x1 {
                    let t = if mono { 0 } else { (y & 1) * 2 + (x & 1) };
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
        match frame.kind {
            SensorKind::Bayer(phase) => {
                for t in 0..4 {
                    out.colors[t] = phase.color_at(t & 1, t >> 1);
                }
            }
            SensorKind::Mono => out.mono = true,
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
    /// A monochrome sensor: one measured plane, no separation.
    pub fn is_mono(&self) -> bool {
        self.negative.separation.is_none()
    }

    /// None on a monochrome negative, and none when a colour file was stored
    /// without its separation. Callers turn that into a failed preview.
    pub fn separation(&self) -> Option<&Separation> {
        self.negative.separation.as_ref()
    }

    /// Full-resolution sharpening mask (the sidecar bytes as a plane).
    pub fn mask_plane(&self) -> mimizan_core::Result<Plane> {
        mask_plane_of(&self.negative)
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

/// Shrinks full-resolution planes to the preview size and turns them
/// upright. The resize runs on the sensor frame (to the swapped size when
/// the orientation transposes) and the small result is oriented, so no
/// full-resolution copy is ever made.
struct PreviewShrink {
    orientation: u16,
    /// Upright preview size.
    pw: usize,
    ph: usize,
    /// Upright full size.
    w: usize,
}

impl PreviewShrink {
    fn new(neg: &Negative) -> Self {
        let (fw, fh) = (neg.plane.width, neg.plane.height);
        let swap = matches!(neg.orientation, 5..=8);
        let (w, h) = if swap { (fh, fw) } else { (fw, fh) };
        let (pw, ph) = fit_dims(w, h, PREVIEW_MAX, PREVIEW_MAX);
        Self { orientation: neg.orientation, pw: pw.min(w), ph: ph.min(h), w }
    }

    fn small(&self, p: &Plane) -> Plane {
        let swap = matches!(self.orientation, 5..=8);
        let (tw, th) = if swap { (self.ph, self.pw) } else { (self.pw, self.ph) };
        let shrunk = if (p.width, p.height) == (tw, th) { p.clone() } else { resize(p, tw, th) };
        orient(&shrunk, self.orientation)
    }

    /// The sidecar as a plane at preview size.
    fn mask(&self, neg: &Negative) -> mimizan_core::Result<Plane> {
        Ok(self.small(&mask_plane_of(neg)?))
    }

    fn scale(&self) -> f64 {
        self.pw as f64 / self.w as f64
    }
}

/// Build the preview planes. A Bayer negative keeps its separation. A
/// monochrome negative previews the measured plane and stores no colour.
/// `mask_max` is not previewed: the mix ignores it and nothing draws it.
pub fn preview_planes(neg: &Negative) -> mimizan_core::Result<PreviewPlanes> {
    let Some(sep) = neg.separation.as_ref() else {
        return preview_measured(neg);
    };
    let sh = PreviewShrink::new(neg);
    let mask = sh.mask(neg)?;
    let sep_small = Separation {
        lum: sh.small(&sep.lum),
        c1: sh.small(&sep.c1),
        c2: sh.small(&sep.c2),
        mask_max: Plane::zeros(1, 1),
        rounds: sep.rounds,
        phase: sep.phase,
        computed: sep.computed,
    };
    Ok(PreviewPlanes { sep: sep_small, mask, scale: sh.scale() })
}

/// Preview of a measured plane. Chrominance stays empty at preview size so
/// the existing mix draws the measurement and nothing else.
fn preview_measured(neg: &Negative) -> mimizan_core::Result<PreviewPlanes> {
    let sh = PreviewShrink::new(neg);
    let lum = sh.small(&neg.plane);
    let mask = sh.mask(neg)?;
    let zero = Plane::zeros(sh.pw, sh.ph);
    let sep = Separation {
        lum,
        c1: zero.clone(),
        c2: zero,
        mask_max: Plane::zeros(1, 1),
        rounds: 0,
        phase: mimizan_core::cfa::BayerPhase::RGGB,
        computed: 0.0,
    };
    Ok(PreviewPlanes { sep, mask, scale: sh.scale() })
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

/// Samples kept ready around the visible window, in samples of the ask's
/// grid (one screen pixel below 1:1, one file pixel at 1:1), so a short
/// pan is already on screen.
pub const DETAIL_MARGIN: usize = 64;

/// The zoomed window the view asks for.
///
/// `step` is the file pixels from one sample to the next. `1` is every
/// pixel: that is the 1:1 view, and it is what the TIFF stores. A larger
/// step is the screen's own grid, one nearest file pixel per screen pixel,
/// while the picture is still smaller than 1:1.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DetailAsk {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    pub step: usize,
}

impl DetailAsk {
    /// Every pixel of `rect`: the 1:1 ask.
    pub fn pixels(rect: DetailRect) -> Self {
        Self { x: rect.x, y: rect.y, w: rect.w, h: rect.h, step: 1 }
    }

    /// The grid step, never 0.
    pub fn step(&self) -> usize {
        self.step.max(1)
    }

    /// `self` already holds every sample of `inner`, on the same grid.
    pub fn covers(&self, inner: &Self) -> bool {
        let step = self.step();
        inner.step() == step
            && inner.x >= self.x
            && inner.y >= self.y
            && inner.x + inner.w.saturating_mul(step) <= self.x + self.w.saturating_mul(step)
            && inner.y + inner.h.saturating_mul(step) <= self.y + self.h.saturating_mul(step)
    }

    /// The grid of `step` that covers the file window `[x0, x1) × [y0, y1)`,
    /// clamped to a `lim_w` × `lim_h` picture. `step` of 1 is that window.
    pub fn covering(
        x0: usize,
        y0: usize,
        x1: usize,
        y1: usize,
        step: usize,
        lim_w: usize,
        lim_h: usize,
    ) -> Option<Self> {
        let step = step.max(1);
        if x1 <= x0 || y1 <= y0 || x0 >= lim_w || y0 >= lim_h {
            return None;
        }
        let x1 = x1.min(lim_w);
        let y1 = y1.min(lim_h);
        let x = x0 - x0 % step;
        let y = y0 - y0 % step;
        let w = (x1 - x).div_ceil(step).min((lim_w - x).div_ceil(step));
        let h = (y1 - y).div_ceil(step).min((lim_h - y).div_ceil(step));
        (w > 0 && h > 0).then_some(Self { x, y, w, h, step })
    }

    /// The same grid, grown by `margin` samples on every side, still inside
    /// the picture and still covering `self`.
    pub fn expand(&self, margin: usize, lim_w: usize, lim_h: usize) -> Self {
        let step = self.step();
        let x = self.x.saturating_sub(margin.saturating_mul(step));
        let y = self.y.saturating_sub(margin.saturating_mul(step));
        let x = x - x % step;
        let y = y - y % step;
        let x1 = self
            .x
            .saturating_add(self.w.saturating_mul(step))
            .saturating_add(margin.saturating_mul(step))
            .min(lim_w);
        let y1 = self
            .y
            .saturating_add(self.h.saturating_mul(step))
            .saturating_add(margin.saturating_mul(step))
            .min(lim_h);
        let w = if x < x1 { (x1 - x).div_ceil(step) } else { self.w };
        let h = if y < y1 { (y1 - y).div_ceil(step) } else { self.h };
        let w = if x < lim_w { w.min((lim_w - x).div_ceil(step)).max(1) } else { self.w.max(1) };
        let h = if y < lim_h { h.min((lim_h - y).div_ceil(step)).max(1) } else { self.h.max(1) };
        Self { x, y, w, h, step }
    }
}

/// 1:1 crop of the negative for the zoomed view, or a coarser grid of it.
pub struct DetailImage {
    pub ask: DetailAsk,
    pub gray: Vec<u8>,
}

/// A rendered ask, kept so the next pan can reuse the overlap.
pub struct DetailTile {
    pub ask: DetailAsk,
    pub gray: Vec<u8>,
}

/// Map a rectangle of the upright image (after `orientation`) back to the
/// sensor frame of a `w`×`h` source, using the same table as `orient`.
/// An empty rectangle maps to an empty one at its origin.
pub(crate) fn source_rect(orientation: u16, w: usize, h: usize, r: DetailRect) -> DetailRect {
    if r.w == 0 || r.h == 0 || w == 0 || h == 0 {
        return DetailRect { x: 0, y: 0, w: 0, h: 0 };
    }
    // Clamped to the upright size, so a rectangle that overhangs the
    // picture maps to its edge instead of underflowing the table.
    let (uw, uh) = if matches!(orientation, 5..=8) { (h, w) } else { (w, h) };
    let map = |ox: usize, oy: usize| sensor_xy(orientation, w, h, ox.min(uw - 1), oy.min(uh - 1));
    let corners =
        [map(r.x, r.y), map(r.x + r.w - 1, r.y), map(r.x, r.y + r.h - 1), map(r.x + r.w - 1, r.y + r.h - 1)];
    let x0 = corners.iter().map(|c| c.0).min().unwrap_or(0);
    let x1 = corners.iter().map(|c| c.0).max().unwrap_or(0);
    let y0 = corners.iter().map(|c| c.1).min().unwrap_or(0);
    let y1 = corners.iter().map(|c| c.1).max().unwrap_or(0);
    DetailRect { x: x0, y: y0, w: x1 - x0 + 1, h: y1 - y0 + 1 }
}

fn crop(p: &Plane, r: DetailRect) -> Option<Plane> {
    let n = p.width.checked_mul(p.height)?;
    if p.data.len() != n || r.w == 0 || r.h == 0 {
        return None;
    }
    if r.x.saturating_add(r.w) > p.width || r.y.saturating_add(r.h) > p.height {
        return None;
    }
    let mut out = Plane::zeros(r.w, r.h);
    out.data.par_chunks_mut(r.w).enumerate().for_each(|(y, row)| {
        row.copy_from_slice(&p.row(r.y + y)[r.x..r.x + r.w]);
    });
    Some(out)
}

/// The mask bytes are one per sensor sample. A short buffer used to be read
/// past its end when the mask was on screen.
fn mask_covers(s: &Session) -> bool {
    let n = &s.negative;
    n.plane.width.checked_mul(n.plane.height).is_some_and(|k| n.mask8.len() == k)
}

/// Render `ask` of the negative.
///
/// `step` of 1 is every file pixel. A larger step reads only that grid, the
/// nearest file pixel of each screen pixel. The mask is the sidecar.
/// Otherwise the pixels are the 8-bit face of the file: with `codes` and no
/// placing, a lookup in the cached mix; else the samples are mixed and
/// looked up in `table`. The ask is clamped to the image.
pub fn render_detail(
    s: &Session,
    v: &ViewParams,
    ask: DetailAsk,
    table: &ToneTable,
    codes: Option<&[u16]>,
) -> Option<DetailImage> {
    if v.show_mask && !mask_covers(s) {
        return None;
    }
    if s.is_mono() {
        return render_measured(s, v, ask, table, codes);
    }
    let step = ask.step();
    let sep = s.separation()?;
    let (sw, sh) = (sep.lum.width, sep.lum.height);
    let orientation = rotated(s.negative.orientation, v.rotation);
    let (uw, uh) = if matches!(orientation, 5..=8) { (sh, sw) } else { (sw, sh) };
    // A committed crop: the ask is in the window. Shift it onto the full upright.
    let (x, y, origin) = match v.crop {
        Some(c) => {
            let (fw, fh) = s.upright_size();
            let t = c.turned(v.rotation, fw as u32, fh as u32).sanitized(uw as u32, uh as u32);
            (
                ask.x.saturating_add(t.x as usize),
                ask.y.saturating_add(t.y as usize),
                (t.x as usize, t.y as usize),
            )
        }
        None => (ask.x, ask.y, (0, 0)),
    };
    if x >= uw || y >= uh || ask.w == 0 || ask.h == 0 {
        return None;
    }
    let w = ask.w.min((uw - x).div_ceil(step));
    let h = ask.h.min((uh - y).div_ceil(step));
    if w == 0 || h == 0 {
        return None;
    }
    let rect = DetailRect { x, y, w, h };
    let src = DetailSource { s, v, sep, sw, sh, orientation, table, codes };
    let gray = if step == 1 { src.dense(rect)? } else { src.grid(x, y, w, h, step) };
    debug_assert_eq!(gray.len(), w * h);
    Some(DetailImage {
        ask: DetailAsk { x: x.saturating_sub(origin.0), y: y.saturating_sub(origin.1), w, h, step },
        gray,
    })
}

/// What one sample of the negative needs. Shared by the dense 1:1 path and
/// the coarser screen grid.
#[derive(Clone, Copy)]
struct DetailSource<'a> {
    s: &'a Session,
    v: &'a ViewParams,
    sep: &'a Separation,
    sw: usize,
    sh: usize,
    orientation: u16,
    table: &'a ToneTable,
    codes: Option<&'a [u16]>,
}

impl DetailSource<'_> {
    /// Every file pixel of `rect`; the 1:1 path.
    fn dense(&self, rect: DetailRect) -> Option<Vec<u8>> {
        let Self { s, v, sep, sw, sh, orientation, table, codes } = *self;
        if v.show_mask {
            let src = source_rect(orientation, sw, sh, rect);
            let upright = orient(&mask_window(&s.negative, src)?, orientation);
            debug_assert_eq!((upright.width, upright.height), (rect.w, rect.h));
            Some(upright.data.par_iter().map(|&x| (x.clamp(0.0, 1.0) * 255.0).round() as u8).collect())
        } else if v.place == 0.0 {
            if let Some(codes) = codes.filter(|c| c.len() == sw * sh) {
                Some(display_codes(codes, sw, sh, orientation, rect, table))
            } else {
                let src = source_rect(orientation, sw, sh, rect);
                Some(display_bytes(&mixed_upright(sep, &v.weights, src, orientation)?, 0.0, table))
            }
        } else {
            let src = source_rect(orientation, sw, sh, rect);
            Some(display_bytes(&mixed_upright(sep, &v.weights, src, orientation)?, v.place, table))
        }
    }

    /// One sample every `step` file pixels. Sample `(i, j)` is the file pixel
    /// `(x + i·step, y + j·step)`, the nearest file pixel of one screen pixel.
    fn grid(&self, x: usize, y: usize, w: usize, h: usize, step: usize) -> Vec<u8> {
        let Self { s, v, sep, sw, sh, orientation, table, codes } = *self;
        let mut out = vec![0u8; w * h];
        let codes = codes.filter(|c| c.len() == sw * sh);
        let n = &s.negative;
        out.par_chunks_mut(w).enumerate().for_each(|(j, row)| {
            let oy = y + j * step;
            for (i, o) in row.iter_mut().enumerate() {
                let ox = x + i * step;
                let (sx, sy) = sensor_xy(orientation, sw, sh, ox, oy);
                *o = if v.show_mask {
                    n.mask8[sy * n.plane.width + sx]
                } else if v.place == 0.0 {
                    if let Some(codes) = codes {
                        table.show[codes[sy * sw + sx] as usize]
                    } else {
                        show_sample(mix_at(sep, &v.weights, sx, sy), 0.0, table)
                    }
                } else {
                    show_sample(mix_at(sep, &v.weights, sx, sy), v.place, table)
                };
            }
        });
        out
    }
}

fn mix_at(sep: &Separation, w: &Weights, x: usize, y: usize) -> f64 {
    let l = sep.lum.at(x, y);
    if w.is_native() {
        return l;
    }
    let k1 = w.g - w.r - w.b;
    let k2 = 2.0 * (w.r - w.b);
    l + k1 * sep.c1.at(x, y) + k2 * sep.c2.at(x, y)
}

fn show_sample(v: f64, place: f64, table: &ToneTable) -> u8 {
    let q = if place == 0.0 { q16(v) } else { q16(v * 2f64.powf(place)) };
    table.show[q]
}

/// 1:1 and the screen grid of a monochrome negative. Samples the measured
/// plane. The clamp is the same one `render_detail` uses for Bayer.
fn render_measured(
    s: &Session,
    v: &ViewParams,
    ask: DetailAsk,
    table: &ToneTable,
    codes: Option<&[u16]>,
) -> Option<DetailImage> {
    if v.show_mask && !mask_covers(s) {
        return None;
    }
    let step = ask.step();
    let (sw, sh) = (s.negative.plane.width, s.negative.plane.height);
    let orientation = rotated(s.negative.orientation, v.rotation);
    let (uw, uh) = if matches!(orientation, 5..=8) { (sh, sw) } else { (sw, sh) };
    let (x, y, origin) = match v.crop {
        Some(c) => {
            let (fw, fh) = s.upright_size();
            let t = c.turned(v.rotation, fw as u32, fh as u32).sanitized(uw as u32, uh as u32);
            (
                ask.x.saturating_add(t.x as usize),
                ask.y.saturating_add(t.y as usize),
                (t.x as usize, t.y as usize),
            )
        }
        None => (ask.x, ask.y, (0, 0)),
    };
    if x >= uw || y >= uh || ask.w == 0 || ask.h == 0 {
        return None;
    }
    let w = ask.w.min((uw - x).div_ceil(step));
    let h = ask.h.min((uh - y).div_ceil(step));
    if w == 0 || h == 0 {
        return None;
    }
    let gray = measured_gray(s, v, table, codes, MeasuredSpan { sw, sh, orientation, x, y, w, h, step })?;
    debug_assert_eq!(gray.len(), w * h);
    Some(DetailImage {
        ask: DetailAsk { x: x.saturating_sub(origin.0), y: y.saturating_sub(origin.1), w, h, step },
        gray,
    })
}

struct MeasuredSpan {
    sw: usize,
    sh: usize,
    orientation: u16,
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    step: usize,
}

fn measured_gray(
    s: &Session,
    v: &ViewParams,
    table: &ToneTable,
    codes: Option<&[u16]>,
    span: MeasuredSpan,
) -> Option<Vec<u8>> {
    let MeasuredSpan { sw, sh, orientation, x, y, w, h, step } = span;
    let n = &s.negative;
    let rect = DetailRect { x, y, w, h };
    if step == 1 {
        if v.show_mask {
            let src = source_rect(orientation, sw, sh, rect);
            let upright = orient(&mask_window(n, src)?, orientation);
            return Some(
                upright.data.par_iter().map(|&p| (p.clamp(0.0, 1.0) * 255.0).round() as u8).collect(),
            );
        }
        let src = source_rect(orientation, sw, sh, rect);
        if v.place == 0.0 {
            if let Some(codes) = codes.filter(|c| c.len() == sw * sh) {
                return Some(display_codes(codes, sw, sh, orientation, rect, table));
            }
        }
        return Some(display_bytes(&orient(&crop(&n.plane, src)?, orientation), v.place, table));
    }
    let codes = codes.filter(|c| c.len() == sw * sh);
    let mut out = vec![0u8; w * h];
    out.par_chunks_mut(w).enumerate().for_each(|(j, row)| {
        let oy = y + j * step;
        for (i, o) in row.iter_mut().enumerate() {
            let ox = x + i * step;
            let (sx, sy) = sensor_xy(orientation, sw, sh, ox, oy);
            *o = if v.show_mask {
                n.mask8[sy * n.plane.width + sx]
            } else if v.place == 0.0 {
                if let Some(codes) = codes {
                    table.show[codes[sy * sw + sx] as usize]
                } else {
                    show_sample(n.plane.at(sx, sy), 0.0, table)
                }
            } else {
                show_sample(n.plane.at(sx, sy), v.place, table)
            };
        }
    });
    Some(out)
}

/// The upright window the print and the loupe enlarge. Bayer mixes the
/// separation. A monochrome sensor enlarges the measured plane.
fn measured_window(s: &Session, weights: &Weights, src: DetailRect, orientation: u16) -> Option<Plane> {
    if s.is_mono() {
        Some(orient(&crop(&s.negative.plane, src)?, orientation))
    } else {
        mixed_upright(s.separation()?, weights, src, orientation)
    }
}

fn mixed_upright(sep: &Separation, weights: &Weights, src: DetailRect, orientation: u16) -> Option<Plane> {
    let small = Separation {
        lum: crop(&sep.lum, src)?,
        c1: crop(&sep.c1, src)?,
        c2: crop(&sep.c2, src)?,
        mask_max: Plane::zeros(1, 1),
        rounds: sep.rounds,
        phase: sep.phase,
        computed: sep.computed,
    };
    Some(orient(&mix(&small, weights), orientation))
}

/// The visible window of a fitted print: the recipe's kernel on the negative,
/// then the same table as the file's 16-bit codes. Borders stay outside the
/// picture. Desktop sharpening is off, so the tile is not sharpened.
pub fn render_print_detail(
    s: &Session,
    v: &ViewParams,
    recipe: &Recipe,
    rect: DetailRect,
    table: &ToneTable,
) -> Option<DetailImage> {
    if v.show_mask {
        return render_detail(s, v, DetailAsk::pixels(rect), table, None);
    }
    let (sensor_w, sensor_h) = (s.negative.plane.width, s.negative.plane.height);
    let orientation = rotated(s.negative.orientation, v.rotation);
    let (full_w, full_h) =
        if matches!(orientation, 5..=8) { (sensor_h, sensor_w) } else { (sensor_w, sensor_h) };
    let params = PrintParams { recipe: recipe.clone(), ..PrintParams::default() };
    let lay = layout(full_w, full_h, 1, &params);
    if lay.image_w == 0 || lay.image_h == 0 || rect.x >= lay.image_w || rect.y >= lay.image_h {
        return None;
    }
    let rect = DetailRect {
        x: rect.x,
        y: rect.y,
        w: rect.w.min(lay.image_w - rect.x).max(1),
        h: rect.h.min(lay.image_h - rect.y).max(1),
    };
    let (ox, oy, sw, sh) = source_window(full_w, full_h, recipe.crop);
    let pix = PixelRect { x: rect.x, y: rect.y, w: rect.w, h: rect.h };
    let (x0, y0, x1, y1) = source_coverage(sw, sh, lay.image_w, lay.image_h, recipe.resample, pix);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let upright = DetailRect { x: x0 + ox, y: y0 + oy, w: x1 - x0, h: y1 - y0 };
    let sensor = source_rect(orientation, sensor_w, sensor_h, upright);
    let mixed = measured_window(s, &v.weights, sensor, orientation)?;
    debug_assert_eq!((mixed.width, mixed.height), (upright.w, upright.h));
    let window = PlaneWindow { plane: &mixed, origin_x: x0, origin_y: y0, full_w: sw, full_h: sh };
    let picture = match resize_rect(window, lay.image_w, lay.image_h, recipe.resample, pix) {
        Ok(p) => p,
        Err(e) => {
            tracing::error!("{e}");
            return None;
        }
    };
    let gray = display_bytes(&picture, v.place, table);
    Some(DetailImage { ask: DetailAsk::pixels(rect), gray })
}

/// A render that clipped the ask, or failed, still has to come back as a
/// tile whose buffer is exactly `ask.w * ask.h`. The clipped ask is the one
/// that gets kept: the screen asked for a corner the picture does not have.
fn painted(ask: DetailAsk, rendered: Option<DetailImage>) -> DetailTile {
    if let Some(d) = rendered {
        if d.ask.step() == ask.step() && d.gray.len() == d.ask.w.saturating_mul(d.ask.h) {
            return DetailTile { ask: d.ask, gray: d.gray };
        }
    }
    DetailTile { ask, gray: vec![0; ask.w.saturating_mul(ask.h)] }
}

/// `have` supplies every sample that already sits on this grid. The rest is
/// rendered: a pan is the newly uncovered strip, a jump or a new step is the
/// whole ask. `render` may clip the ask to the picture. The tile then carries
/// that clipped ask, and its buffer is that ask's `w * h`.
pub fn cover_detail(
    have: Option<&DetailTile>,
    ask: DetailAsk,
    mut render: impl FnMut(DetailAsk) -> Option<DetailImage>,
) -> DetailTile {
    let Some(old) = have else {
        return painted(ask, render(ask));
    };
    let step = ask.step();
    let aligned = ask.x.is_multiple_of(step)
        && ask.y.is_multiple_of(step)
        && old.ask.step() == step
        && old.ask.x.is_multiple_of(step)
        && old.ask.y.is_multiple_of(step)
        && old.gray.len() == old.ask.w.saturating_mul(old.ask.h);
    let (Some(shift_i), Some(shift_j)) = (
        aligned.then(|| grid_shift(ask.x, old.ask.x, step)).flatten(),
        aligned.then(|| grid_shift(ask.y, old.ask.y, step)).flatten(),
    ) else {
        return painted(ask, render(ask));
    };
    let mut gray = vec![0u8; ask.w.saturating_mul(ask.h)];
    let ni0 = (-shift_i).max(0) as usize;
    let ni1 = (old.ask.w as isize - shift_i).clamp(0, ask.w as isize) as usize;
    let nj0 = (-shift_j).max(0) as usize;
    let nj1 = (old.ask.h as isize - shift_j).clamp(0, ask.h as isize) as usize;
    if ni0 >= ni1 || nj0 >= nj1 {
        return painted(ask, render(ask));
    }
    for nj in nj0..nj1 {
        let oj = (nj as isize + shift_j) as usize;
        let src = oj * old.ask.w + (ni0 as isize + shift_i) as usize;
        let dst = nj * ask.w + ni0;
        let n = ni1 - ni0;
        gray[dst..dst + n].copy_from_slice(&old.gray[src..src + n]);
    }
    if ni0 == 0 && ni1 == ask.w && nj0 == 0 && nj1 == ask.h {
        return DetailTile { ask, gray };
    }
    let mut gaps = Vec::with_capacity(4);
    if ni0 > 0 {
        gaps.push((0, 0, ni0, ask.h));
    }
    if ni1 < ask.w {
        gaps.push((ni1, 0, ask.w - ni1, ask.h));
    }
    if nj0 > 0 && ni1 > ni0 {
        gaps.push((ni0, 0, ni1 - ni0, nj0));
    }
    if nj1 < ask.h && ni1 > ni0 {
        gaps.push((ni0, nj1, ni1 - ni0, ask.h - nj1));
    }
    for (gx, gy, gw, gh) in gaps {
        let sub = DetailAsk { x: ask.x + gx * step, y: ask.y + gy * step, w: gw, h: gh, step };
        let Some(got) = render(sub) else {
            return painted(ask, render(ask));
        };
        // A clip changes the rectangle. The strip no longer fits the gap,
        // so the whole ask is rendered again and keeps the clipped size.
        if got.ask.w != gw || got.ask.h != gh || got.gray.len() != gw * gh {
            return painted(ask, render(ask));
        }
        for j in 0..gh {
            let dst = (gy + j) * ask.w + gx;
            let src = j * gw;
            gray[dst..dst + gw].copy_from_slice(&got.gray[src..src + gw]);
        }
    }
    DetailTile { ask, gray }
}

fn grid_shift(new_origin: usize, old_origin: usize, step: usize) -> Option<isize> {
    let d = new_origin as isize - old_origin as isize;
    (step > 0 && d % step as isize == 0).then_some(d / step as isize)
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
    let (sensor_w, sensor_h) = (s.negative.plane.width, s.negative.plane.height);
    let (full_w, full_h) =
        if matches!(orientation, 5..=8) { (sensor_h, sensor_w) } else { (sensor_w, sensor_h) };
    let (sx, sy, ex, ey) = loupe_source_rect(full_w, full_h, &params, nx, ny, view);
    // USM is the only step that reads the mask, and only then is the crop
    // wide enough for its Lanczos-3 resample.
    let want_mask = recipe.sharpen.usm_amount() > 0.0;
    let (linear, mask) = if ex > sx && ey > sy {
        let upright_rect = DetailRect { x: sx, y: sy, w: ex - sx, h: ey - sy };
        let sensor = source_rect(orientation, sensor_w, sensor_h, upright_rect);
        let Some(mixed) = measured_window(s, weights, sensor, orientation) else {
            return SharpenLoupe { gray: Vec::new(), width: 0, height: 0 };
        };
        let mask =
            want_mask.then(|| mask_window(&s.negative, sensor)).flatten().map(|m| orient(&m, orientation));
        (mixed, mask)
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
    let gray = plane.data.par_iter().map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    SharpenLoupe { gray, width: plane.width, height: plane.height }
}

/// The whole sidecar as a plane in [0, 1]; an error when its length does
/// not match the negative.
fn mask_plane_of(n: &Negative) -> mimizan_core::Result<Plane> {
    Plane::try_from_vec(
        n.plane.width,
        n.plane.height,
        n.mask8.par_iter().map(|&v| f64::from(v) / 255.0).collect(),
    )
}

/// A window of the sidecar (sensor frame) as a plane in [0, 1]. `None`
/// when the sidecar does not fit the negative or the window lies outside.
fn mask_window(n: &Negative, r: DetailRect) -> Option<Plane> {
    let samples = n.plane.width.checked_mul(n.plane.height)?;
    if n.mask8.len() != samples || r.w == 0 || r.h == 0 {
        return None;
    }
    if r.x.saturating_add(r.w) > n.plane.width || r.y.saturating_add(r.h) > n.plane.height {
        return None;
    }
    let mut out = Plane::zeros(r.w, r.h);
    out.data.par_chunks_mut(r.w).enumerate().for_each(|(y, row)| {
        let base = (r.y + y) * n.plane.width + r.x;
        for (x, o) in row.iter_mut().enumerate() {
            *o = f64::from(n.mask8[base + x]) / 255.0;
        }
    });
    Some(out)
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

/// One table of the paper: index `q16(placed linear)`, then the print.
///
/// The screen and the histogram share it. Building it is 65536 evaluations;
/// a slider then looks up, it does not solve the paper again per pixel.
/// `view` is the histogram bin `(eval * 255).round()`. `show` is the 8-bit
/// face of the 16-bit code the TIFF stores, `q16(eval)`. Quantising the
/// linear value first is the histogram's contract; the file evaluates the
/// float and then quantises, so a code can differ by the last bit.
pub(crate) struct ToneTable {
    view: Vec<u8>,
    show: Vec<u8>,
    /// 1 = code 0 (paper black), 2 = code 65535 (paper white).
    clip: Vec<u8>,
}

impl ToneTable {
    fn build(tone: &ToneMap) -> Self {
        let built: Vec<(u8, u8, u8)> = (0..65536u32)
            .into_par_iter()
            .map(|i| {
                let t = tone.eval(f64::from(i) / 65535.0);
                let code = q16(t);
                let view = (t.clamp(0.0, 1.0) * 255.0).round() as u8;
                let show = ((code as f64 / 65535.0) * 255.0).round() as u8;
                let clip = if code == 0 {
                    1
                } else if code == 65535 {
                    2
                } else {
                    0
                };
                (view, show, clip)
            })
            .collect();
        let mut view = Vec::with_capacity(built.len());
        let mut show = Vec::with_capacity(built.len());
        let mut clip = Vec::with_capacity(built.len());
        for (v, s, c) in built {
            view.push(v);
            show.push(s);
            clip.push(c);
        }
        Self { view, show, clip }
    }
}

/// The paper for `tone`, kept while the exposures stay put. A pan reuses it.
#[derive(Default)]
pub struct ToneHold {
    held: Option<(Tone, ToneTable)>,
}

impl ToneHold {
    pub fn get(&mut self, tone: &Tone) -> &ToneTable {
        let fresh = !matches!(&self.held, Some((t, _)) if t == tone);
        if fresh {
            self.held = Some((tone.clone(), ToneTable::build(&tone.compile())));
        }
        &self.held.as_ref().unwrap().1
    }
}

/// Gamma-2.2 bins of a 16-bit linear code. The negative histogram's axis;
/// it does not depend on the paper, so it is built once.
fn neg_lut() -> &'static [u8; 65536] {
    static LUT: OnceLock<[u8; 65536]> = OnceLock::new();
    LUT.get_or_init(|| {
        let mut lut = [0u8; 65536];
        for (i, bin) in lut.iter_mut().enumerate() {
            *bin = ((i as f64 / 65535.0).powf(1.0 / 2.2) * 255.0).round() as u8;
        }
        lut
    })
}

/// `q16` of the full-resolution mix, sensor order. Rebuilt when the weights
/// change, then every exposure is a lookup.
fn mix_codes(sep: &Separation, w: &Weights) -> Vec<u16> {
    let width = sep.lum.width;
    let mut out = vec![0u16; width * sep.lum.height];
    if w.is_native() {
        out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
            for (o, &v) in row.iter_mut().zip(sep.lum.row(y)) {
                *o = q16(v) as u16;
            }
        });
        return out;
    }
    let k1 = w.g - w.r - w.b;
    let k2 = 2.0 * (w.r - w.b);
    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let (l, c1, c2) = (sep.lum.row(y), sep.c1.row(y), sep.c2.row(y));
        for x in 0..width {
            row[x] = q16(l[x] + k1 * c1[x] + k2 * c2[x]) as u16;
        }
    });
    out
}

/// 8-bit face of the print. `place == 0` quantises `v`; a placing quantises
/// `v · 2^place`, which is what the histogram counts.
fn display_bytes(plane: &Plane, place: f64, table: &ToneTable) -> Vec<u8> {
    let global = 2f64.powf(place);
    plane
        .data
        .par_iter()
        .map(|&v| {
            let q = if place == 0.0 { q16(v) } else { q16(v * global) };
            table.show[q]
        })
        .collect()
}

/// Sensor pixel under upright `(ox, oy)`. The same map `orient` uses.
fn sensor_xy(orientation: u16, w: usize, h: usize, ox: usize, oy: usize) -> (usize, usize) {
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
}

/// The visible window from cached mix codes, place 0. One lookup per pixel.
fn display_codes(
    codes: &[u16],
    sw: usize,
    sh: usize,
    orientation: u16,
    rect: DetailRect,
    table: &ToneTable,
) -> Vec<u8> {
    debug_assert_eq!(codes.len(), sw.saturating_mul(sh));
    let mut out = vec![0u8; rect.w * rect.h];
    out.par_chunks_mut(rect.w).enumerate().for_each(|(y, row)| {
        let oy = rect.y + y;
        for (x, o) in row.iter_mut().enumerate() {
            let (sx, sy) = sensor_xy(orientation, sw, sh, rect.x + x, oy);
            debug_assert!(sx < sw && sy < sh);
            *o = table.show[codes[sy * sw + sx] as usize];
        }
    });
    out
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
    /// `place` stops and the tone `table` (what the TIFF gets). Values are
    /// quantised to 16 bits first, so the clipping counts are the TIFF's own
    /// 0 and 65535 pixels. Nothing is downscaled: a resampled copy rings at
    /// edges (false black and white) and averages isolated clipped pixels
    /// away, so its histogram ends are wrong by an order of magnitude.
    pub fn of_separation(
        sep: &Separation,
        w: &Weights,
        table: &ToneTable,
        place: f64,
        window: Option<(usize, usize, usize, usize)>,
    ) -> (Self, Self) {
        let k1 = w.g - w.r - w.b;
        let k2 = 2.0 * (w.r - w.b);
        Self::count(sep.lum.width, sep.lum.height, window, table, place, |y| {
            let (l, c1, c2) = (sep.lum.row(y), sep.c1.row(y), sep.c2.row(y));
            move |x| l[x] + k1 * c1[x] + k2 * c2[x]
        })
    }

    /// The same counts as [`of_separation`] on a plane that has no colour.
    /// A monochrome negative is that plane. Weights do not enter.
    pub fn of_plane(
        plane: &Plane,
        table: &ToneTable,
        place: f64,
        window: Option<(usize, usize, usize, usize)>,
    ) -> (Self, Self) {
        Self::count(plane.width, plane.height, window, table, place, |y| {
            let row = plane.row(y);
            move |x| row[x]
        })
    }

    /// The same counts as [`of_separation`] from cached `q16(mix)` codes.
    /// `place == 0` is exact: the view index is the code. A placing
    /// dequantises the code before scaling, which can move a bin at the
    /// boundary, so the worker keeps the float walk when `place != 0`.
    fn of_codes(
        codes: &[u16],
        width: usize,
        table: &ToneTable,
        place: f64,
        window: Option<(usize, usize, usize, usize)>,
    ) -> (Self, Self) {
        let height = codes.len().checked_div(width).unwrap_or(0);
        Self::count(width, height, window, table, place, |y| {
            let row = &codes[y * width..(y + 1) * width];
            move |x| row[x] as f64 / 65535.0
        })
    }

    /// The shared walk. `row(y)` gives a sampler of the linear value `m` at
    /// `x`; the negative code is `q16(m)` and the view code `q16(m · 2^place)`
    /// on the float (what `tone::place` does to the pixels), or the negative
    /// code itself when `place == 0`. Row-ordered partial counts are merged
    /// deterministically.
    fn count<R: Fn(usize) -> f64>(
        width: usize,
        height: usize,
        window: Option<(usize, usize, usize, usize)>,
        table: &ToneTable,
        place: f64,
        row: impl Fn(usize) -> R + Sync,
    ) -> (Self, Self) {
        let (x0, y0, x1, y1) = match window {
            Some((x, y, w, h)) => {
                let x0 = x.min(width);
                let y0 = y.min(height);
                (x0, y0, x.saturating_add(w).min(width).max(x0), y.saturating_add(h).min(height).max(y0))
            }
            None => (0, 0, width, height),
        };
        let lut_neg = neg_lut();
        let global = 2f64.powf(place);
        let parts: Vec<Counts> = (y0..y1)
            .into_par_iter()
            .map(|y| {
                let code_at = row(y);
                let mut c = Counts::default();
                for x in x0..x1 {
                    let m = code_at(x);
                    let q = q16(m);
                    c.neg[lut_neg[q] as usize] += 1;
                    c.neg_low += (q == 0) as usize;
                    c.neg_high += (q == 65535) as usize;
                    let qv = if place == 0.0 { q } else { q16(m * global) };
                    let bin = table.view[qv];
                    c.view[bin as usize] += 1;
                    match table.clip[qv] {
                        1 => c.view_low += 1,
                        2 => c.view_high += 1,
                        _ => {}
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
    pub hist_view: Histogram,
    pub hist_negative: Histogram,
}

/// Full-resolution histograms, and the mix codes they are counted from.
/// The codes depend on the session and the weights. The bins also depend
/// on the tone, the placing and the crop. A drag of the exposures rebins
/// the codes; it does not mix the negative again.
pub struct HistCache {
    session: Option<u64>,
    weights: Option<Weights>,
    codes: Vec<u16>,
    width: usize,
    hist_key: Option<HistKey>,
    hists: (Histogram, Histogram),
}

impl Default for HistCache {
    fn default() -> Self {
        Self {
            session: None,
            weights: None,
            codes: Vec::new(),
            width: 0,
            hist_key: None,
            hists: (Histogram::empty(), Histogram::empty()),
        }
    }
}

fn hist_window(s: &Session, v: &ViewParams) -> Option<(usize, usize, usize, usize)> {
    v.crop.map(|c| {
        let (fw, fh) = s.upright_size();
        let c = c.sanitized(fw as u32, fh as u32);
        let (sw, sh) = (s.negative.plane.width, s.negative.plane.height);
        let r = source_rect(
            s.negative.orientation,
            sw,
            sh,
            DetailRect { x: c.x as usize, y: c.y as usize, w: c.w as usize, h: c.h as usize },
        );
        (r.x, r.y, r.w, r.h)
    })
}

impl HistCache {
    fn ensure(&mut self, s: &Session, w: &Weights) {
        let ptr = s.id;
        if s.is_mono() {
            if self.session == Some(ptr) && self.width == s.negative.plane.width && !self.codes.is_empty() {
                return;
            }
            self.width = s.negative.plane.width;
            self.codes = s.negative.plane.data.par_iter().map(|&v| q16(v) as u16).collect();
            self.session = Some(ptr);
            self.weights = None;
            self.hist_key = None;
            return;
        }
        if self.session == Some(ptr) && self.weights.as_ref() == Some(w) {
            return;
        }
        let Some(sep) = s.separation() else {
            self.codes.clear();
            self.width = 0;
            self.session = Some(ptr);
            self.weights = Some(*w);
            self.hist_key = None;
            return;
        };
        self.width = sep.lum.width;
        self.codes = mix_codes(sep, w);
        self.session = Some(ptr);
        self.weights = Some(*w);
        self.hist_key = None;
    }

    /// Cached mix codes for this session and these weights. Sensor order.
    pub fn codes_for(&mut self, s: &Session, w: &Weights) -> &[u16] {
        self.ensure(s, w);
        &self.codes
    }

    fn histograms(&mut self, s: &Session, v: &ViewParams, table: &ToneTable) -> (Histogram, Histogram) {
        let ptr = s.id;
        let key = v.hist_key();
        if self.session != Some(ptr) || self.hist_key.as_ref() != Some(&key) {
            let window = hist_window(s, v);
            // Place 0 reads the cached codes. A placing keeps the float
            // walk: `q16(m · 2^place)` is not `q16` of the dequantised code,
            // and the histogram tests lock the float.
            let hists = if v.place == 0.0 {
                self.ensure(s, &v.weights);
                Histogram::of_codes(&self.codes, self.width, table, 0.0, window)
            } else if s.is_mono() {
                self.session = Some(ptr);
                Histogram::of_plane(&s.negative.plane, table, v.place, window)
            } else if let Some(sep) = s.separation() {
                self.session = Some(ptr);
                Histogram::of_separation(sep, &v.weights, table, v.place, window)
            } else {
                self.session = Some(ptr);
                (Histogram::empty(), Histogram::empty())
            };
            self.hists = hists;
            self.hist_key = Some(key);
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

/// 8-bit grey preview of the whole negative: the downscaled mix, then the
/// tone table (or the mask), in the picture's final orientation.
pub fn render_preview(s: &Session, v: &ViewParams, cache: &mut HistCache, table: &ToneTable) -> PreviewImage {
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
            hist_view,
            hist_negative: Histogram::empty(),
        };
    }
    // Histograms from the full-resolution separation, not the preview copy.
    let (hist_negative, hist_view) = cache.histograms(s, v, table);
    let mut mixed = orient(&mix(&pp.sep, &v.weights), rotated(1, v.rotation));
    if let Some(c) = v.crop {
        let (fw, fh) = s.upright_size();
        let turned = c.turned(v.rotation, fw as u32, fh as u32);
        let scaled = scale_crop(turned, s.preview.scale, mixed.width, mixed.height);
        mixed = window_plane(&mixed, scaled);
    }
    let gray = display_bytes(&mixed, v.place, table);
    PreviewImage {
        width: mixed.width,
        height: mixed.height,
        rotation: v.rotation,
        gray,
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
            let got = orient(&crop(&full, src).unwrap(), o);
            assert_eq!((got.width, got.height), (r.w, r.h), "orientation {o}");
            assert_eq!(got.data, expected, "orientation {o}");
        }
    }

    /// A window of the cached codes is the oriented mix, including a turn.
    #[test]
    fn code_window_matches_the_oriented_mix() {
        let (w, h) = (13usize, 9usize);
        let mut lum = Plane::zeros(w, h);
        let mut c1 = Plane::zeros(w, h);
        for (i, (l, a)) in lum.data.iter_mut().zip(c1.data.iter_mut()).enumerate() {
            *l = i as f64 / (w * h - 1) as f64;
            *a = 0.01 * ((i % 5) as f64 - 2.0);
        }
        let sep = Separation {
            lum,
            c1,
            c2: Plane::zeros(w, h),
            mask_max: Plane::zeros(1, 1),
            rounds: 1,
            phase: mimizan_core::cfa::BayerPhase::RGGB,
            computed: 0.0,
        };
        let wts = Weights { r: 0.2, g: 0.6, b: 0.2 };
        let o = 6u16;
        let upright = orient(&mix(&sep, &wts), o);
        let rect = DetailRect { x: 1, y: 2, w: 4, h: 3 };
        let table = ToneTable::build(&Tone::paper().compile());
        let expected = display_bytes(&crop(&upright, rect).unwrap(), 0.0, &table);
        let codes = mix_codes(&sep, &wts);
        let got = display_codes(&codes, w, h, o, rect, &table);
        assert_eq!(got, expected);
    }

    #[test]
    fn one_to_one_covers_the_window_exactly() {
        let ask = DetailAsk::covering(2, 5, 9, 12, 1, 40, 30).unwrap();
        assert_eq!((ask.x, ask.y, ask.w, ask.h, ask.step), (2, 5, 7, 7, 1));
    }

    /// Below 1:1 the grid starts on a sample boundary and reaches the far edge.
    #[test]
    fn a_coarse_grid_covers_the_visible_window() {
        let ask = DetailAsk::covering(10, 3, 20, 15, 4, 100, 80).unwrap();
        assert_eq!(ask.step, 4);
        assert_eq!((ask.x, ask.y), (8, 0));
        assert!(ask.x + ask.w * ask.step >= 20);
        assert!(ask.y + ask.h * ask.step >= 15);
        assert!(ask.x + (ask.w - 1) * ask.step < 100);
        let grown = ask.expand(2, 100, 80);
        assert!(grown.covers(&ask));
        assert!(grown.x + grown.w * grown.step <= 100);
        assert!(grown.y + grown.h * grown.step <= 80);
    }

    #[test]
    fn mix_at_matches_the_mixed_plane() {
        let (w, h) = (6usize, 4usize);
        let mut lum = Plane::zeros(w, h);
        let mut c1 = Plane::zeros(w, h);
        let mut c2 = Plane::zeros(w, h);
        for (i, ((l, a), b)) in
            lum.data.iter_mut().zip(c1.data.iter_mut()).zip(c2.data.iter_mut()).enumerate()
        {
            *l = 0.2 + 0.05 * i as f64;
            *a = 0.01 * (i as f64 - 3.0);
            *b = -0.02 * ((i % 3) as f64);
        }
        let sep = Separation {
            lum,
            c1,
            c2,
            mask_max: Plane::zeros(1, 1),
            rounds: 1,
            phase: mimizan_core::cfa::BayerPhase::RGGB,
            computed: 0.0,
        };
        let wts = Weights { r: 0.2, g: 0.5, b: 0.3 };
        let mixed = mix(&sep, &wts);
        for y in 0..h {
            for x in 0..w {
                let got = mix_at(&sep, &wts, x, y);
                assert!((got - mixed.at(x, y)).abs() < 1e-12, "at {x},{y}");
            }
        }
    }

    /// A diagonal pan keeps the shared samples and asks only for the new strips.
    #[test]
    fn a_pan_fills_only_the_new_strip() {
        fn paint(ask: DetailAsk) -> Option<DetailImage> {
            let gray = (0..ask.h)
                .flat_map(|j| {
                    (0..ask.w).map(move |i| ((ask.x / ask.step + i) * 3 + (ask.y / ask.step + j)) as u8)
                })
                .collect();
            Some(DetailImage { ask, gray })
        }
        let first = DetailAsk { x: 0, y: 0, w: 6, h: 4, step: 2 };
        let have = cover_detail(None, first, paint);
        assert_eq!(have.gray, paint(first).unwrap().gray);
        let next = DetailAsk { x: 4, y: 2, w: 6, h: 4, step: 2 };
        let mut strips = 0usize;
        let got = cover_detail(Some(&have), next, |ask| {
            strips += 1;
            assert!(ask.w * ask.h < next.w * next.h, "a pan must not rebuild the window");
            paint(ask)
        });
        assert_eq!(got.gray, paint(next).unwrap().gray);
        assert!((1..=4).contains(&strips), "strips {strips}");
        let mut again = 0usize;
        let same = cover_detail(Some(&got), next, |_| {
            again += 1;
            None
        });
        assert_eq!(again, 0);
        assert_eq!(same.gray, paint(next).unwrap().gray);
    }

    /// The 1:1 window can hang off the picture. The renderer clips it. The
    /// tile must keep that clipped size, or the screen panics on the upload.
    #[test]
    fn a_window_past_the_picture_keeps_the_pixels_that_exist() {
        fn clip(ask: DetailAsk) -> Option<DetailImage> {
            let (lim_w, lim_h) = (40usize, 30usize);
            if ask.x >= lim_w || ask.y >= lim_h || ask.w == 0 || ask.h == 0 {
                return None;
            }
            let ask = DetailAsk { w: ask.w.min(lim_w - ask.x), h: ask.h.min(lim_h - ask.y), ..ask };
            let gray = vec![7u8; ask.w * ask.h];
            Some(DetailImage { ask, gray })
        }
        let ask = DetailAsk { x: 30, y: 20, w: 20, h: 20, step: 1 };
        let tile = cover_detail(None, ask, clip);
        assert_eq!((tile.ask.x, tile.ask.y, tile.ask.w, tile.ask.h), (30, 20, 10, 10));
        assert_eq!(tile.gray.len(), tile.ask.w * tile.ask.h);
        // A later pan that still hangs off the corner must not pair the old
        // size with the clipped buffer.
        let further = DetailAsk { x: 32, y: 22, w: 20, h: 20, step: 1 };
        let next = cover_detail(Some(&tile), further, clip);
        assert_eq!(next.gray.len(), next.ask.w * next.ask.h);
        assert!(next.ask.x + next.ask.w <= 40);
        assert!(next.ask.y + next.ask.h <= 30);
    }

    /// The desktop program starts with every pixel measured: the last
    /// octave is reconstructed only when the switch is set.
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
        let (neg, view) = Histogram::of_separation(&sep, &wts, &ToneTable::build(&curve), 0.0, None);

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
            let (_, view) = Histogram::of_separation(&sep, &wts, &ToneTable::build(&tone), place, None);
            let mut pic = mix(&sep, &wts);
            mimizan_core::tone::place(&mut pic, place);
            let mut ev = [0u32; 256];
            for &l in &pic.data {
                let lin = q16(l) as f64 / 65535.0;
                ev[(tone.eval(lin) * 255.0).round() as usize] += 1;
            }
            assert_eq!(view.bins, ev);
        }
    }

    /// Place 0 counts the cached codes. The bins are the float walk's bins.
    #[test]
    fn of_codes_matches_of_separation_without_placing() {
        let sep = sweep(40, 24);
        let wts = Weights { r: 0.2, g: 0.5, b: 0.3 };
        let tone = Tone::paper().compile();
        let table = ToneTable::build(&tone);
        let (n1, v1) = Histogram::of_separation(&sep, &wts, &table, 0.0, None);
        let codes = mix_codes(&sep, &wts);
        let (n2, v2) = Histogram::of_codes(&codes, sep.lum.width, &table, 0.0, None);
        assert_eq!(n1, n2);
        assert_eq!(v1, v2);
    }

    /// The byte on screen is the 8-bit face of the 16-bit print code.
    #[test]
    fn display_byte_is_the_files_code() {
        let n = 16usize;
        let mut plane = Plane::zeros(8, 2);
        for (i, v) in plane.data.iter_mut().enumerate() {
            *v = -0.05 + 1.1 * i as f64 / (n - 1) as f64;
        }
        let tone = Tone::paper().compile();
        let table = ToneTable::build(&tone);
        let got = display_bytes(&plane, 0.0, &table);
        for (byte, &v) in got.iter().zip(&plane.data) {
            let code = q16(tone.eval(q16(v) as f64 / 65535.0));
            let expect = ((code as f64 / 65535.0) * 255.0).round() as u8;
            assert_eq!(*byte, expect);
        }
    }

    /// A plane with no colour counts the same as a mix of that plane.
    #[test]
    fn of_plane_matches_a_mix_with_no_colour() {
        use mimizan_core::cfa::BayerPhase;
        let mut lum = Plane::zeros(5, 4);
        for (i, v) in lum.data.iter_mut().enumerate() {
            *v = i as f64 / 19.0;
        }
        let sep = Separation {
            lum: lum.clone(),
            c1: Plane::zeros(5, 4),
            c2: Plane::zeros(5, 4),
            mask_max: Plane::zeros(5, 4),
            rounds: 0,
            phase: BayerPhase::RGGB,
            computed: 0.0,
        };
        let w = Weights { r: 0.2, g: 0.5, b: 0.3 };
        let tone = Tone::paper().compile();
        let table = ToneTable::build(&tone);
        let (n1, v1) = Histogram::of_separation(&sep, &w, &table, 0.4, None);
        let (n2, v2) = Histogram::of_plane(&lum, &table, 0.4, None);
        assert_eq!(n1, n2);
        assert_eq!(v1, v2);
    }

    /// A monochrome file opens onto its measured plane. Weights do not
    /// rewrite it, and the window does not ask for a separation.
    #[test]
    fn a_monochrome_negative_is_the_measured_plane() {
        use mimizan_core::decode::{RawData, Rect, SensorKind};
        use mimizan_core::pipeline::{self, NegativeParams};
        use std::sync::Arc;
        let (w, h) = (8usize, 6usize);
        let raw: Vec<u16> = (0..w * h).map(|i| (i * 40) as u16).collect();
        let frame = RawFrame {
            make: "Leica".into(),
            model: "M11 Monochrom".into(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: w,
            height: h,
            bits: 14,
            kind: SensorKind::Mono,
            black_tile: [0.0; 4],
            white_rgb: [1000.0; 3],
            crop: Rect { x: 0, y: 0, w, h },
            orientation: 1,
            xyz_to_cam: None,
            wb_as_shot: None,
            exposure: Default::default(),
            data: RawData::Integer(raw),
        };
        let params = NegativeParams { keep_separation: true, ..NegativeParams::default() };
        let neg = pipeline::negative_from_frame(&frame, &params, 0).unwrap();
        assert!(neg.separation.is_none());
        assert!(!neg.info.reconstruct);
        let preview = preview_planes(&neg).unwrap();
        assert_eq!(preview.sep.lum.data, neg.plane.data);
        assert!(preview.sep.c1.data.iter().all(|&v| v == 0.0));
        assert!(preview.sep.c2.data.iter().all(|&v| v == 0.0));
        let raw_hist = RawHistogram::of_frame(&frame);
        let session = Session {
            id: next_session_id(),
            path: "mono.dng".into(),
            frame: Arc::new(frame),
            negative: neg,
            camera_file: None,
            camera_file_path: None,
            develop: DevelopParams::default(),
            preview,
            raw_hist,
        };
        assert!(session.is_mono());
        assert!(session.raw_hist.mono);
        assert_eq!(session.raw_hist.total[0], w * h);
        assert_eq!(session.raw_hist.total[1], 0);
        let mut cache = HistCache::default();
        let native = Weights::default();
        let shifted = Weights { r: 0.1, g: 0.2, b: 0.7 };
        let a = cache.codes_for(&session, &native).to_vec();
        let b = cache.codes_for(&session, &shifted).to_vec();
        assert_eq!(a, b);
        assert_eq!(a.len(), w * h);
        assert_eq!(a[0], q16(session.negative.plane.at(0, 0)) as u16);
        let tone = Tone::paper().compile();
        let table = ToneTable::build(&tone);
        let view = ViewParams {
            weights: shifted,
            tone: Tone::paper(),
            place: 0.0,
            show_mask: false,
            rotation: 0,
            crop: None,
        };
        let detail = render_detail(
            &session,
            &view,
            DetailAsk::pixels(DetailRect { x: 0, y: 0, w, h }),
            &table,
            Some(&a),
        )
        .unwrap();
        assert_eq!(detail.gray, display_bytes(&session.negative.plane, 0.0, &table));
    }
}
