//! What the GUI keeps after a file was developed: the decoded frame (so a
//! re-development skips the decoder), the full-resolution separation (so the
//! weights can be re-mixed without separating again) and a downscaled,
//! upright copy of the separation for the interactive preview.
//!
//! A monochrome sensor has no colour filter. The GUI keeps the measured
//! plane and a preview of it. It does not invent chrominance planes.

use mimizan_core::burn::{self, Field, Stroke, Times};
use mimizan_core::calib::CameraFile;
use mimizan_core::cfa::Color;
use mimizan_core::decode::{RawFrame, SensorKind};
use mimizan_core::ingest::WhiteBalance;
use mimizan_core::mix::{mix, Weights};
use mimizan_core::paper::{self, PaperTable};
use mimizan_core::pipeline::{Negative, NegativeParams};
use mimizan_core::plane::Plane;
use mimizan_core::plate::{mix_plate, PlateParams, PlateSpace, PlateWeights};
use mimizan_core::print::{
    burn_field, layout, loupe_source_rect, orient, rotated, sharpen_window, source_window, window_plane,
    PrintParams, MASK_KERNEL,
};
use mimizan_core::recipe::{CropRect, Recipe};
use mimizan_core::resize::{
    fit_dims, resize, resize_rect, source_coverage, PixelRect, PlaneWindow, ResampleKernel,
};
use mimizan_core::separate::{MaskMode, Separation};
use mimizan_core::straighten::{self, Affine, PhasedKernel, STRAIGHTEN_KERNEL};
use mimizan_core::tone::{Tone, ToneMap};
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// Longest preview edge in pixels (the texture the GUI shows).
pub const PREVIEW_MAX: usize = 1800;

/// Settings that need a new separation (ingest + separate), not just a re-mix.
///
/// Every photosite stays as measured: the desktop has no defect repair and
/// no reconstruction. Defects are counted and shown in the Negative window;
/// the command's `--repair-defects` is there for a hot pixel from a long
/// exposure.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DevelopParams {
    pub wb: WhiteBalance,
    pub mask: MaskMode,
}

impl Default for DevelopParams {
    fn default() -> Self {
        // Pixel-adaptive Dubois separation is the default; the fixed estimator
        // and the block mask stay available under Development.
        Self { wb: WhiteBalance::AsShot, mask: MaskMode::Dubois }
    }
}

impl DevelopParams {
    pub fn negative_params(&self, cam: Option<CameraFile>) -> NegativeParams {
        let mut p = NegativeParams { keep_separation: true, ..NegativeParams::default() };
        p.ingest.wb = self.wb;
        p.separate.mask = self.mask;
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

    /// The frame the crop lies in: the file-upright negative straightened
    /// by `angle`, before the user's turns. The upright size at zero.
    pub fn frame_size(&self, angle: f64) -> (usize, usize) {
        let (w, h) = self.upright_size();
        straighten::frame_size(w, h, angle)
    }

    /// Weights the camera file suggests for this image's balance, or native.
    pub fn default_weights(&self) -> Weights {
        self.camera_file.as_ref().map(|c| c.weights_balanced(self.negative.info.wb)).unwrap_or_default()
    }

    /// Balance multipliers of this development (G = 1).
    pub fn wb(&self) -> [f64; 3] {
        self.negative.info.wb
    }

    /// The three taking filters `p` asks for, in front of `base` (the
    /// Wratten set filters the panchromatic mix; the camera set needs the
    /// file's matrix and takes the channels as they are without one).
    pub fn plate_weights(&self, p: &PlateParams, base: Weights) -> mimizan_core::Result<PlateWeights> {
        PlateWeights::resolve(p, base, self.frame.xyz_to_cam.as_ref())
    }

    /// The file carries a colour matrix, so the camera set is colorimetric.
    pub fn has_colour_matrix(&self) -> bool {
        self.frame.xyz_to_cam.is_some()
    }
}

/// The three-colour plate as the view, the print box and the export render
/// it: the taking filters, the projection primaries and the lamps. `None`
/// in a view is the monochrome negative.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PlateView {
    pub weights: PlateWeights,
    pub space: PlateSpace,
    /// Stops added to the red, green and blue positive on top of the placing.
    pub lamps: [f64; 3],
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
    /// The straighten: degrees the upright negative is turned, clockwise
    /// positive. The picture is then the frame (`straighten::frame_size`),
    /// and `crop` lies in it. Zero turns nothing.
    pub angle: f64,
    /// Window on the file-upright frame. `None` is the whole frame.
    /// The preview turns it with `rotation` before cutting.
    pub crop: Option<CropRect>,
    /// The dodges and burns, on the file-upright negative (`Recipe::burns`).
    /// The preview turns them with `rotation` like the crop.
    pub burns: Vec<Stroke>,
    /// A stroke is being painted: the picture follows the hand, the
    /// full-resolution histograms keep their last count and are counted
    /// again when the hand is lifted. Counting them walks the burned
    /// pixels through the paper, tens of milliseconds on a 24-megapixel
    /// file, too much for every frame of a drag.
    pub burn_live: bool,
    /// Three mixes instead of one: the picture is shown in colour. The
    /// histograms stay those of the monochrome mix, the plate's luminance.
    pub plate: Option<PlateView>,
}

/// The parts of a view the full-resolution histograms depend on (the
/// orientation is not one of them: a histogram does not turn). The crop
/// and the straighten are, because they choose which grains are counted;
/// the burns, because they change what those grains print as.
type HistKey = (Weights, Tone, f64, Option<CropRect>, f64, Vec<Stroke>);

impl ViewParams {
    fn hist_key(&self) -> HistKey {
        (self.weights, self.tone.clone(), self.place, self.crop, self.angle, self.burns.clone())
    }
}

/// Where the view's pixels come from: the sensor plane, the upright
/// picture after the file's orientation and the user's turns, and the
/// frame the straighten leaves on it. Every ask of the view is in frame
/// pixels; a frame pixel is an upright pixel when there is no turn, and a
/// resampled one through `map` when there is.
#[derive(Clone, Copy, Debug)]
struct Geometry {
    /// The sensor plane.
    sw: usize,
    sh: usize,
    /// The file's orientation plus the user's turns.
    orientation: u16,
    /// The upright picture.
    uw: usize,
    uh: usize,
    /// The frame.
    fw: usize,
    fh: usize,
    /// Frame → upright, continuous coordinates; `None` without a turn.
    map: Option<Affine>,
}

impl Geometry {
    fn of(s: &Session, v: &ViewParams) -> Self {
        let (sw, sh) = (s.negative.plane.width, s.negative.plane.height);
        let orientation = rotated(s.negative.orientation, v.rotation);
        let (uw, uh) = if matches!(orientation, 5..=8) { (sh, sw) } else { (sw, sh) };
        let (fw, fh) = straighten::frame_size(uw, uh, v.angle);
        let map = straighten::turns(v.angle).then(|| straighten::frame_to_upright(uw, uh, v.angle));
        Self { sw, sh, orientation, uw, uh, fw, fh, map }
    }

    /// The crop, turned into this frame and kept inside it; the whole
    /// frame when there is none. `(x, y, w, h)`.
    fn window(&self, s: &Session, v: &ViewParams) -> (usize, usize, usize, usize) {
        match v.crop {
            Some(c) => {
                let (fw, fh) = s.frame_size(v.angle);
                let t = c.turned(v.rotation, fw as u32, fh as u32).sanitized(self.fw as u32, self.fh as u32);
                (t.x as usize, t.y as usize, t.w as usize, t.h as usize)
            }
            None => (0, 0, self.fw, self.fh),
        }
    }

    /// The upright pixel under frame sample `(ox, oy)`: itself without a
    /// turn, the nearest one through the map with.
    #[inline]
    fn upright_px(&self, ox: usize, oy: usize) -> (usize, usize) {
        match &self.map {
            None => (ox.min(self.uw.saturating_sub(1)), oy.min(self.uh.saturating_sub(1))),
            Some(m) => {
                let (u, v) = m.apply(ox as f64 + 0.5, oy as f64 + 0.5);
                let ux = (u - 0.5).round().clamp(0.0, self.uw.saturating_sub(1) as f64) as usize;
                let uy = (v - 0.5).round().clamp(0.0, self.uh.saturating_sub(1) as f64) as usize;
                (ux, uy)
            }
        }
    }

    /// The sensor pixel under frame sample `(ox, oy)`.
    #[inline]
    fn sensor_px(&self, ox: usize, oy: usize) -> (usize, usize) {
        let (ux, uy) = self.upright_px(ox, oy);
        sensor_xy(self.orientation, self.sw, self.sh, ux, uy)
    }

    /// The view's burns over the frame's pixels: turned with the picture,
    /// read through the straighten onto the upright negative. `None`
    /// without a burn.
    fn field(&self, v: &ViewParams) -> Option<Field> {
        let to_upright = self.map.unwrap_or(Affine::IDENTITY);
        Field::new(&burn::turned(&v.burns, v.rotation), burn::normalize(self.uw, self.uh).after(&to_upright))
    }

    /// Frame pixels `rect` through the turn: the upright footprint the
    /// kernel reads is mapped to the sensor, `upright_of` gives those
    /// pixels upright, and the warp resamples them with `kernel`. `None`
    /// without a turn, or when the source cannot be had.
    fn warped<const N: usize>(
        &self,
        rect: DetailRect,
        kernel: ResampleKernel,
        upright_of: impl Fn(DetailRect) -> Option<[Plane; N]>,
    ) -> Option<[Plane; N]> {
        let map = self.map.as_ref()?;
        let pix = PixelRect { x: rect.x, y: rect.y, w: rect.w, h: rect.h };
        let (x0, y0, x1, y1) = straighten::footprint(map, kernel, pix, self.uw, self.uh);
        if x1 <= x0 || y1 <= y0 {
            return None;
        }
        let up = DetailRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 };
        let sensor = source_rect(self.orientation, self.sw, self.sh, up);
        let planes = upright_of(sensor)?;
        let mut out = Vec::with_capacity(N);
        for plane in &planes {
            if (plane.width, plane.height) != (up.w, up.h) {
                return None;
            }
            let window = PlaneWindow { plane, origin_x: x0, origin_y: y0, full_w: self.uw, full_h: self.uh };
            match straighten::warp(window, map, kernel, pix) {
                Ok(p) => out.push(p),
                Err(e) => {
                    tracing::error!("{e}");
                    return None;
                }
            }
        }
        out.try_into().ok()
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
    /// The monochrome negative, `w * h` bytes.
    pub gray: Vec<u8>,
    /// The plate in colour, interleaved R, G, B, `3 * w * h` bytes.
    pub rgb: Option<Vec<u8>>,
}

/// A rendered ask, kept so the next pan can reuse the overlap.
pub struct DetailTile {
    pub ask: DetailAsk,
    pub gray: Vec<u8>,
    pub rgb: Option<Vec<u8>>,
}

/// `rgb`, when present, has three bytes for every one of `gray`.
fn rgb_fits(gray: &[u8], rgb: Option<&Vec<u8>>) -> bool {
    rgb.is_none_or(|c| c.len() == gray.len().saturating_mul(3))
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
/// Otherwise the pixels are the 8-bit face of the file: the linear picture
/// of the ask ([`render_detail_linear`]) placed, toned and burned
/// ([`tone_tile`]). The ask is clamped to the image.
pub fn render_detail(s: &Session, v: &ViewParams, ask: DetailAsk, table: &ToneTable) -> Option<DetailImage> {
    if v.show_mask {
        return render_mask_detail(s, v, ask);
    }
    Some(tone_tile(&render_detail_linear(s, v, ask)?, v, None, table))
}

/// The linear picture of a tile: the mixed negative over the ask's grid,
/// before the placing, the tone and the burns, which are a lookup per
/// pixel. The worker keeps it while the geometry and the mix stand, so a
/// slider or a stroke pays the lookup alone.
pub struct LinearTile {
    /// The ask as clipped to the picture.
    pub ask: DetailAsk,
    /// The monochrome mix, `ask.w × ask.h`.
    pub gray: Plane,
    /// The three plates, by output channel, when the view is in colour.
    pub rgb: Option<[Plane; 3]>,
    /// How the burns reach this tile's pixels.
    burns: BurnMap,
}

/// Where a tile's pixels lie for the strokes.
enum BurnMap {
    /// A tile of the file as viewed: tile pixel (continuous, pixel `i`
    /// centred at `i + ½`) to the normalised file-upright negative, the
    /// strokes turned with the picture.
    View { to_norm: Affine, rotation: u8 },
    /// A tile of the print: the recipe's strokes through the print's own
    /// map, at this tile's offset.
    Print { full_w: usize, full_h: usize, image_w: usize, image_h: usize, x: usize, y: usize },
}

impl LinearTile {
    /// The strokes over this tile's pixels; `None` without a stroke.
    fn field(&self, v: &ViewParams, recipe: Option<&Recipe>) -> Option<Field> {
        match &self.burns {
            BurnMap::View { to_norm, rotation } => Field::new(&burn::turned(&v.burns, *rotation), *to_norm),
            BurnMap::Print { full_w, full_h, image_w, image_h, x, y } => {
                burn_field(recipe?, *full_w, *full_h, *image_w, *image_h).map(|f| f.shifted(*x, *y))
            }
        }
    }
}

/// The 8-bit face of a linear tile: each pixel placed, then through the
/// table, or through the paper at its own times where a stroke reaches.
/// `recipe` is the print's, for a print tile's strokes.
pub fn tone_tile(t: &LinearTile, v: &ViewParams, recipe: Option<&Recipe>, table: &ToneTable) -> DetailImage {
    let field = t.field(v, recipe);
    let gray = display_bytes_with(&t.gray, v.place, table, field.as_ref());
    let rgb = match (&t.rgb, v.plate) {
        (Some(planes), Some(plate)) => {
            Some(display_rgb_with(planes, v.place, plate.lamps, table, field.as_ref()))
        }
        _ => None,
    };
    DetailImage { ask: t.ask, gray, rgb }
}

/// The ask on the frame: the committed crop shifts it, the picture clips
/// it. `(x, y, w, h, origin)`, `origin` the crop's corner.
fn frame_ask(
    g: &Geometry,
    s: &Session,
    v: &ViewParams,
    ask: DetailAsk,
) -> Option<(DetailRect, (usize, usize))> {
    let step = ask.step();
    let (cx, cy, _, _) = g.window(s, v);
    let (x, y) = (ask.x.saturating_add(cx), ask.y.saturating_add(cy));
    if x >= g.fw || y >= g.fh || ask.w == 0 || ask.h == 0 {
        return None;
    }
    let w = ask.w.min((g.fw - x).div_ceil(step));
    let h = ask.h.min((g.fh - y).div_ceil(step));
    if w == 0 || h == 0 {
        return None;
    }
    Some((DetailRect { x, y, w, h }, (cx, cy)))
}

/// The linear picture of `ask` of the file as viewed: the mix, resampled
/// through the turn at 1:1 or sampled on the screen's grid, and the
/// plates with it when the view is in colour. Not the mask.
pub fn render_detail_linear(s: &Session, v: &ViewParams, ask: DetailAsk) -> Option<LinearTile> {
    let step = ask.step();
    let g = Geometry::of(s, v);
    let (rect, origin) = frame_ask(&g, s, v, ask)?;
    let DetailRect { x, y, w, h } = rect;
    let gray = if s.is_mono() {
        measured_linear(s, &g, rect, step)?
    } else {
        mixed_linear(s.separation()?, &v.weights, &g, rect, step)?
    };
    debug_assert_eq!((gray.width, gray.height), (w, h));
    let rgb = match v.plate.filter(|_| !s.is_mono()) {
        Some(plate) => {
            let sep = s.separation()?;
            Some(plate_linear(sep, &plate.weights, &g, rect, step)?)
        }
        None => None,
    };
    // Tile sample `(i, j)` is frame pixel `(x + i·step, y + j·step)`, read
    // at its centre; the frame goes through the turn onto the upright
    // negative, and that is normalised.
    let to_upright = g.map.unwrap_or(Affine::IDENTITY);
    let to_frame = Affine {
        a: step as f64,
        b: 0.0,
        c: 0.0,
        d: step as f64,
        tx: x as f64 + 0.5 - 0.5 * step as f64,
        ty: y as f64 + 0.5 - 0.5 * step as f64,
    };
    let to_norm = burn::normalize(g.uw, g.uh).after(&to_upright).after(&to_frame);
    Some(LinearTile {
        ask: DetailAsk { x: x.saturating_sub(origin.0), y: y.saturating_sub(origin.1), w, h, step },
        gray,
        rgb,
        burns: BurnMap::View { to_norm, rotation: v.rotation },
    })
}

/// The mix over `rect` of the frame: at 1:1 the footprint mixed upright
/// and warped through the turn (or cut, without one); on a coarser grid
/// the nearest sensor pixel of each sample, mixed.
fn mixed_linear(
    sep: &Separation,
    weights: &Weights,
    g: &Geometry,
    rect: DetailRect,
    step: usize,
) -> Option<Plane> {
    if step == 1 {
        if g.map.is_some() {
            let [plane] = g.warped(rect, STRAIGHTEN_KERNEL, |sensor| {
                Some([mixed_upright(sep, weights, sensor, g.orientation)?])
            })?;
            return Some(plane);
        }
        let src = source_rect(g.orientation, g.sw, g.sh, rect);
        return mixed_upright(sep, weights, src, g.orientation);
    }
    Some(sampled(g, rect, step, |sx, sy| mix_at(sep, weights, sx, sy)))
}

/// The three plates over `rect`, as [`mixed_linear`].
fn plate_linear(
    sep: &Separation,
    w: &PlateWeights,
    g: &Geometry,
    rect: DetailRect,
    step: usize,
) -> Option<[Plane; 3]> {
    if step == 1 {
        if g.map.is_some() {
            return g.warped(rect, STRAIGHTEN_KERNEL, |sensor| plate_upright(sep, w, sensor, g.orientation));
        }
        let sensor = source_rect(g.orientation, g.sw, g.sh, rect);
        return plate_upright(sep, w, sensor, g.orientation);
    }
    let weights = w.weights();
    Some(weights.map(|wc| sampled(g, rect, step, |sx, sy| mix_at(sep, &wc, sx, sy))))
}

/// The measured plane of a monochrome negative over `rect`, as
/// [`mixed_linear`].
fn measured_linear(s: &Session, g: &Geometry, rect: DetailRect, step: usize) -> Option<Plane> {
    let n = &s.negative;
    if step == 1 {
        if g.map.is_some() {
            let [plane] = g.warped(rect, STRAIGHTEN_KERNEL, |sensor| {
                Some([orient(&crop(&n.plane, sensor)?, g.orientation)])
            })?;
            return Some(plane);
        }
        let src = source_rect(g.orientation, g.sw, g.sh, rect);
        return Some(orient(&crop(&n.plane, src)?, g.orientation));
    }
    Some(sampled(g, rect, step, |sx, sy| n.plane.at(sx, sy)))
}

/// One sample every `step` frame pixels over `rect` (whose `w × h` are
/// samples): sample `(i, j)` is the frame pixel `(x + i·step, y + j·step)`,
/// read from the nearest sensor pixel by `at`.
fn sampled(g: &Geometry, rect: DetailRect, step: usize, at: impl Fn(usize, usize) -> f64 + Sync) -> Plane {
    let DetailRect { x, y, w, h } = rect;
    let mut out = Plane::zeros(w, h);
    out.data.par_chunks_mut(w).enumerate().for_each(|(j, row)| {
        let oy = y + j * step;
        for (i, o) in row.iter_mut().enumerate() {
            let (sx, sy) = g.sensor_px(x + i * step, oy);
            *o = at(sx, sy);
        }
    });
    out
}

/// The sidecar mask over `ask`, as bytes: resampled through the turn at
/// 1:1 with the mask's kernel, else cut or sampled. One byte per sensor
/// sample, so a monochrome and a Bayer negative read alike.
fn render_mask_detail(s: &Session, v: &ViewParams, ask: DetailAsk) -> Option<DetailImage> {
    if !mask_covers(s) {
        return None;
    }
    let step = ask.step();
    let g = Geometry::of(s, v);
    let (rect, origin) = frame_ask(&g, s, v, ask)?;
    let DetailRect { x, y, w, h } = rect;
    let n = &s.negative;
    let to_u8 = |m: f64| (m.clamp(0.0, 1.0) * 255.0).round() as u8;
    let gray = if step == 1 {
        let plane = if g.map.is_some() {
            let [plane] = g.warped(rect, MASK_KERNEL, |sensor| {
                Some([orient(&mask_window(n, sensor)?, g.orientation)])
            })?;
            plane
        } else {
            let src = source_rect(g.orientation, g.sw, g.sh, rect);
            orient(&mask_window(n, src)?, g.orientation)
        };
        plane.data.par_iter().map(|&m| to_u8(m)).collect()
    } else {
        let mut out = vec![0u8; w * h];
        out.par_chunks_mut(w).enumerate().for_each(|(j, row)| {
            let oy = y + j * step;
            for (i, o) in row.iter_mut().enumerate() {
                let (sx, sy) = g.sensor_px(x + i * step, oy);
                *o = n.mask8[sy * n.plane.width + sx];
            }
        });
        out
    };
    Some(DetailImage {
        ask: DetailAsk { x: x.saturating_sub(origin.0), y: y.saturating_sub(origin.1), w, h, step },
        gray,
        rgb: None,
    })
}

/// One row of frame pixels under the field: the normalisation done once
/// for the row, the field read only over `span`, the constants `left`
/// and `right` on either side of it (unit without a field). `new` spans
/// every pixel a stroke reaches, with the whole row under a card;
/// `varying` only those whose times change, which is a card's band.
struct FieldRow<'a> {
    row: Option<burn::Row<'a>>,
    span: std::ops::Range<usize>,
    left: Times,
    right: Times,
}

impl<'a> FieldRow<'a> {
    /// Row `y`, read for the pixels `from..to`; outside the span the
    /// times are unit.
    fn new(field: Option<&'a Field>, y: usize, from: usize, to: usize) -> Self {
        match field {
            Some(f) => {
                let row = f.row(y);
                let span = row.span(from, to);
                Self { row: Some(row), span, left: Times::UNIT, right: Times::UNIT }
            }
            None => Self::none(),
        }
    }

    /// Row `y` for the pixels `from..to`, the field read where its times
    /// vary, the sides' constants elsewhere.
    fn varying(field: Option<&'a Field>, y: usize, from: usize, to: usize) -> Self {
        match field {
            Some(f) => {
                let row = f.row(y);
                let v = row.varying(from, to);
                Self { row: Some(row), span: v.range, left: v.left, right: v.right }
            }
            None => Self::none(),
        }
    }

    fn none() -> Self {
        Self { row: None, span: 0..0, left: Times::UNIT, right: Times::UNIT }
    }

    #[inline]
    fn at(&self, x: usize) -> Times {
        match &self.row {
            Some(row) => {
                if x < self.span.start {
                    self.left
                } else if x >= self.span.end {
                    self.right
                } else {
                    row.at(x)
                }
            }
            None => Times::UNIT,
        }
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

/// The upright window the print and the loupe enlarge. Bayer mixes the
/// separation. A monochrome sensor enlarges the measured plane.
fn measured_window(s: &Session, weights: &Weights, src: DetailRect, orientation: u16) -> Option<Plane> {
    if s.is_mono() {
        Some(orient(&crop(&s.negative.plane, src)?, orientation))
    } else {
        mixed_upright(s.separation()?, weights, src, orientation)
    }
}

fn crop_separation(sep: &Separation, src: DetailRect) -> Option<Separation> {
    Some(Separation {
        lum: crop(&sep.lum, src)?,
        c1: crop(&sep.c1, src)?,
        c2: crop(&sep.c2, src)?,
        mask_max: Plane::zeros(1, 1),
        rounds: sep.rounds,
        phase: sep.phase,
    })
}

fn mixed_upright(sep: &Separation, weights: &Weights, src: DetailRect, orientation: u16) -> Option<Plane> {
    Some(orient(&mix(&crop_separation(sep, src)?, weights), orientation))
}

/// The three plates of a window, upright, by output channel R, G, B.
fn plate_upright(
    sep: &Separation,
    w: &PlateWeights,
    src: DetailRect,
    orientation: u16,
) -> Option<[Plane; 3]> {
    let small = crop_separation(sep, src)?;
    Some(mix_plate(&small, w).map(|p| orient(&p, orientation)))
}

/// 8-bit face of a plate print: channel `c` placed by `place + lamps[c]`,
/// then the same table as the grey, interleaved R, G, B. The planes must
/// agree in size. With a field, the burns give the same times to the three
/// positives, as one card over the paper is for the three passes of a plate.
fn display_rgb_with(
    planes: &[Plane; 3],
    place: f64,
    lamps: [f64; 3],
    table: &ToneTable,
    field: Option<&Field>,
) -> Vec<u8> {
    let n = planes.iter().map(|p| p.data.len()).min().unwrap_or(0);
    debug_assert!(planes.iter().all(|p| p.data.len() == n));
    let gains = lamps.map(|l| 2f64.powf(place + l));
    let mut out = vec![0u8; n * 3];
    if let Some(field) = field {
        let width = planes[0].width.max(1);
        out.par_chunks_mut(width * 3).enumerate().for_each(|(y, row)| {
            let mut timed = Timed::new(table);
            let line = FieldRow::varying(Some(field), y, 0, width);
            for (x, px) in row.as_chunks_mut::<3>().0.iter_mut().enumerate() {
                let idx = y * width + x;
                if idx >= n {
                    break;
                }
                let t = line.at(x);
                if line.span.contains(&x) && !t.is_unit() {
                    let solved = timed.solve(t);
                    for c in 0..3 {
                        px[c] = timed.show_solved(planes[c].data[idx] * gains[c], t, solved);
                    }
                } else {
                    for c in 0..3 {
                        px[c] = timed.show(planes[c].data[idx] * gains[c], t);
                    }
                }
            }
        });
        return out;
    }
    const ROW: usize = 4096;
    out.par_chunks_mut(ROW * 3).enumerate().for_each(|(k, chunk)| {
        let base = k * ROW;
        for (i, px) in chunk.as_chunks_mut::<3>().0.iter_mut().enumerate() {
            let idx = base + i;
            for c in 0..3 {
                px[c] = table.show[q16(planes[c].data[idx] * gains[c])];
            }
        }
    });
    out
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
        return render_detail(s, v, DetailAsk::pixels(rect), table);
    }
    Some(tone_tile(&render_print_linear(s, v, recipe, rect)?, v, Some(recipe), table))
}

/// The linear picture of `rect` of the fitted print: the mix of the
/// footprint, enlarged with the recipe's kernel through the print's map,
/// and the plates with it.
pub fn render_print_linear(
    s: &Session,
    v: &ViewParams,
    recipe: &Recipe,
    rect: DetailRect,
) -> Option<LinearTile> {
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
    let pix = PixelRect { x: rect.x, y: rect.y, w: rect.w, h: rect.h };
    // Through a turn the tile is warped from its upright footprint, as
    // the export is; without one the crop window is resized, as before.
    let turned = straighten::turns(recipe.angle);
    let map = straighten::print_map(full_w, full_h, recipe.angle, recipe.crop, lay.image_w, lay.image_h);
    let (ox, oy, sw, sh) =
        if turned { (0, 0, full_w, full_h) } else { source_window(full_w, full_h, recipe.crop) };
    let kernel = if turned { straighten::kernel_for(&map, recipe.resample) } else { recipe.resample };
    let (x0, y0, x1, y1) = if turned {
        straighten::footprint(&map, kernel, pix, full_w, full_h)
    } else {
        source_coverage(sw, sh, lay.image_w, lay.image_h, recipe.resample, pix)
    };
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let upright = DetailRect { x: x0 + ox, y: y0 + oy, w: x1 - x0, h: y1 - y0 };
    let sensor = source_rect(orientation, sensor_w, sensor_h, upright);
    let mixed = measured_window(s, &v.weights, sensor, orientation)?;
    debug_assert_eq!((mixed.width, mixed.height), (upright.w, upright.h));
    let enlarge = |plane: &Plane| {
        let window = PlaneWindow { plane, origin_x: x0, origin_y: y0, full_w: sw, full_h: sh };
        let out = if turned {
            straighten::warp(window, &map, kernel, pix)
        } else {
            resize_rect(window, lay.image_w, lay.image_h, recipe.resample, pix)
        };
        match out {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::error!("{e}");
                None
            }
        }
    };
    let gray = enlarge(&mixed)?;
    let rgb = match v.plate.filter(|_| !s.is_mono()) {
        Some(plate) => {
            let planes = plate_upright(s.separation()?, &plate.weights, sensor, orientation)?;
            Some([enlarge(&planes[0])?, enlarge(&planes[1])?, enlarge(&planes[2])?])
        }
        None => None,
    };
    Some(LinearTile {
        ask: DetailAsk::pixels(rect),
        gray,
        rgb,
        burns: BurnMap::Print {
            full_w,
            full_h,
            image_w: lay.image_w,
            image_h: lay.image_h,
            x: rect.x,
            y: rect.y,
        },
    })
}

/// A render that clipped the ask, or failed, still has to come back as a
/// tile whose buffer is exactly `ask.w * ask.h`. The clipped ask is the one
/// that gets kept: the screen asked for a corner the picture does not have.
fn painted(ask: DetailAsk, rendered: Option<DetailImage>) -> DetailTile {
    if let Some(d) = rendered {
        if d.ask.step() == ask.step()
            && d.gray.len() == d.ask.w.saturating_mul(d.ask.h)
            && rgb_fits(&d.gray, d.rgb.as_ref())
        {
            return DetailTile { ask: d.ask, gray: d.gray, rgb: d.rgb };
        }
    }
    DetailTile { ask, gray: vec![0; ask.w.saturating_mul(ask.h)], rgb: None }
}

/// Copy `h` rows of `w` samples, `ch` bytes each, from `(sx, sy)` of a
/// `src_w`-wide buffer to `(dx, dy)` of a `dst_w`-wide one.
#[allow(clippy::too_many_arguments)]
fn copy_rows(
    dst: &mut [u8],
    dst_w: usize,
    (dx, dy): (usize, usize),
    src: &[u8],
    src_w: usize,
    (sx, sy): (usize, usize),
    (w, h): (usize, usize),
    ch: usize,
) {
    for j in 0..h {
        let d = ((dy + j) * dst_w + dx) * ch;
        let s = ((sy + j) * src_w + sx) * ch;
        dst[d..d + w * ch].copy_from_slice(&src[s..s + w * ch]);
    }
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
        && old.gray.len() == old.ask.w.saturating_mul(old.ask.h)
        && rgb_fits(&old.gray, old.rgb.as_ref());
    let (Some(shift_i), Some(shift_j)) = (
        aligned.then(|| grid_shift(ask.x, old.ask.x, step)).flatten(),
        aligned.then(|| grid_shift(ask.y, old.ask.y, step)).flatten(),
    ) else {
        return painted(ask, render(ask));
    };
    let n = ask.w.saturating_mul(ask.h);
    let mut gray = vec![0u8; n];
    // The colour comes along when the old tile had it; a gap rendered
    // without it (the view changed under us) repaints the whole ask.
    let mut rgb = old.rgb.as_ref().map(|_| vec![0u8; n * 3]);
    let ni0 = (-shift_i).max(0) as usize;
    let ni1 = (old.ask.w as isize - shift_i).clamp(0, ask.w as isize) as usize;
    let nj0 = (-shift_j).max(0) as usize;
    let nj1 = (old.ask.h as isize - shift_j).clamp(0, ask.h as isize) as usize;
    if ni0 >= ni1 || nj0 >= nj1 {
        return painted(ask, render(ask));
    }
    let from = ((ni0 as isize + shift_i) as usize, (nj0 as isize + shift_j) as usize);
    let span = (ni1 - ni0, nj1 - nj0);
    copy_rows(&mut gray, ask.w, (ni0, nj0), &old.gray, old.ask.w, from, span, 1);
    if let (Some(dst), Some(src)) = (rgb.as_mut(), old.rgb.as_ref()) {
        copy_rows(dst, ask.w, (ni0, nj0), src, old.ask.w, from, span, 3);
    }
    if ni0 == 0 && ni1 == ask.w && nj0 == 0 && nj1 == ask.h {
        return DetailTile { ask, gray, rgb };
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
        if got.ask.w != gw
            || got.ask.h != gh
            || got.gray.len() != gw * gh
            || got.rgb.is_some() != rgb.is_some()
            || !rgb_fits(&got.gray, got.rgb.as_ref())
        {
            return painted(ask, render(ask));
        }
        copy_rows(&mut gray, ask.w, (gx, gy), &got.gray, gw, (0, 0), (gw, gh), 1);
        if let (Some(dst), Some(src)) = (rgb.as_mut(), got.rgb.as_ref()) {
            copy_rows(dst, ask.w, (gx, gy), src, gw, (0, 0), (gw, gh), 3);
        }
    }
    DetailTile { ask, gray, rgb }
}

fn grid_shift(new_origin: usize, old_origin: usize, step: usize) -> Option<isize> {
    let d = new_origin as isize - old_origin as isize;
    (step > 0 && d % step as isize == 0).then_some(d / step as isize)
}

pub struct SharpenLoupe {
    pub gray: Vec<u8>,
    /// The plate's box in colour, interleaved R, G, B.
    pub rgb: Option<Vec<u8>>,
    pub width: usize,
    pub height: usize,
}

impl SharpenLoupe {
    fn empty() -> Self {
        Self { gray: Vec::new(), rgb: None, width: 0, height: 0 }
    }
}

/// One box of the print at 1:1: the same resize and paper the TIFF gets,
/// on a tile around `(nx, ny)`. `view` is the box side in print pixels.
/// With a `plate` the box is the three positives, each with its lamp.
#[allow(clippy::too_many_arguments)]
pub fn render_sharpen_loupe(
    s: &Session,
    weights: &Weights,
    plate: Option<&PlateView>,
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
    let plate = plate.filter(|_| !s.is_mono());
    let (linear, plates, mask) = if ex > sx && ey > sy {
        let upright_rect = DetailRect { x: sx, y: sy, w: ex - sx, h: ey - sy };
        let sensor = source_rect(orientation, sensor_w, sensor_h, upright_rect);
        let Some(mixed) = measured_window(s, weights, sensor, orientation) else {
            return SharpenLoupe::empty();
        };
        let plates = match plate {
            Some(p) => {
                match s.separation().and_then(|sep| plate_upright(sep, &p.weights, sensor, orientation)) {
                    Some(planes) => Some(planes),
                    None => return SharpenLoupe::empty(),
                }
            }
            None => None,
        };
        let mask =
            want_mask.then(|| mask_window(&s.negative, sensor)).flatten().map(|m| orient(&m, orientation));
        (mixed, plates, mask)
    } else {
        (Plane::zeros(1, 1), None, None)
    };
    let (origin_x, origin_y) = if ex > sx && ey > sy { (sx, sy) } else { (0, 0) };
    let mask_window = mask.as_ref().map(|m| PlaneWindow { plane: m, origin_x, origin_y, full_w, full_h });
    let through = |plane: &Plane, p: &PrintParams| {
        let window = PlaneWindow { plane, origin_x, origin_y, full_w, full_h };
        match sharpen_window(window, mask_window, p, nx, ny, view) {
            Ok(p) => Some(p),
            Err(e) => {
                tracing::error!("{e}");
                None
            }
        }
    };
    let Some(plane) = through(&linear, &params) else { return SharpenLoupe::empty() };
    let to_u8 = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let gray = plane.data.par_iter().map(|&v| to_u8(v)).collect();
    let rgb = match (plate, plates) {
        (Some(p), Some(planes)) => {
            // Each positive through the same pass, plus its lamp, as the export does.
            let mut out = Vec::with_capacity(3);
            for (positive, lamp) in planes.iter().zip(p.lamps) {
                let mut q = params.clone();
                q.recipe.place += lamp;
                let Some(pos) = through(positive, &q) else { return SharpenLoupe::empty() };
                // Against the grey box, not the source window the positive came from.
                if (pos.width, pos.height) != (plane.width, plane.height) {
                    return SharpenLoupe::empty();
                }
                out.push(pos);
            }
            let n = plane.data.len();
            let mut rgb = vec![0u8; n * 3];
            rgb.par_chunks_mut(3).enumerate().for_each(|(i, px)| {
                for c in 0..3 {
                    px[c] = to_u8(out[c].data[i]);
                }
            });
            Some(rgb)
        }
        _ => None,
    };
    SharpenLoupe { gray, rgb, width: plane.width, height: plane.height }
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

/// 16-bit quantisation of a linear value in [0,1], as the TIFF writer does
/// it. Half rounds up by the add and the truncating cast, which is the
/// nearest integer of a non-negative number without the library's
/// `round` (a call, where the cast is one instruction).
#[inline]
fn q16(x: f64) -> usize {
    (x.clamp(0.0, 1.0) * 65535.0 + 0.5) as usize
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
    /// The tone itself, for the pixels a stroke gives their own times: the
    /// table is one curve, and a stroke under one light is another.
    tone: ToneMap,
    /// The paper tabulated for those pixels; a look is quick as it is.
    timed: Option<PaperTable>,
}

/// A reader of the table for pixels under strokes: the table where it
/// holds, the paper solved at the pixel's own times where it does not.
/// The times repeat from pixel to pixel (a stroke's interior is one
/// time), so what they need, a scene factor or two logarithms, is kept
/// from the last pixel.
struct Timed<'a> {
    table: &'a ToneTable,
    last: Option<(Times, Solved)>,
}

/// What a pixel's times come to, once.
#[derive(Clone, Copy)]
enum Solved {
    /// The table at the value scaled by this: `eval_timed(l, t) == eval(l·k)`.
    Factor(f64),
    /// The paper solved from the tables, the times as logarithms.
    Logged { soft: f64, hard: f64 },
    /// The tone itself, exactly (a look under uneven times).
    Exact,
}

impl<'a> Timed<'a> {
    fn new(table: &'a ToneTable) -> Self {
        Self { table, last: None }
    }

    fn solve(&self, times: Times) -> Solved {
        let t = self.table;
        match &t.timed {
            Some(_) if times.is_even() && times.soft > 0.0 => Solved::Factor(paper::scene_factor(times.soft)),
            Some(_) => {
                Solved::Logged { soft: paper::log2_time(times.soft), hard: paper::log2_time(times.hard) }
            }
            None => match t.tone.scene_factor(times) {
                Some(k) => Solved::Factor(k),
                None => Solved::Exact,
            },
        }
    }

    /// What the times come to, from the last pixel when they are its.
    #[inline]
    fn solved(&mut self, times: Times) -> Solved {
        match self.last {
            Some((held, s)) if held == times => s,
            _ => {
                let s = self.solve(times);
                self.last = Some((times, s));
                s
            }
        }
    }

    /// `(view, show, clip)` of a placed linear value under `times`.
    #[inline]
    fn faces(&mut self, lp: f64, times: Times) -> (u8, u8, u8) {
        let t = self.table;
        if times.is_unit() {
            let q = q16(lp);
            return (t.view[q], t.show[q], t.clip[q]);
        }
        match self.solved(times) {
            Solved::Factor(k) => {
                let s = lp * k;
                if lp <= 1.0 && s <= 1.0 {
                    let q = q16(s);
                    return (t.view[q], t.show[q], t.clip[q]);
                }
                ToneTable::faces(t.tone.eval_timed(lp, times))
            }
            Solved::Logged { soft, hard } => {
                ToneTable::faces(t.timed.as_ref().unwrap().eval_logged(lp, soft, hard))
            }
            Solved::Exact => ToneTable::faces(t.tone.eval_timed(lp, times)),
        }
    }

    /// `show` for a pixel whose times are its own: solved afresh, the
    /// last pixel's solution neither read nor kept. In a stroke's
    /// penumbra, where every pixel differs, that is the shorter path:
    /// the pixels do not wait on one another.
    #[inline]
    fn show_fresh(&self, lp: f64, times: Times) -> u8 {
        if times.is_unit() {
            return self.table.show[q16(lp)];
        }
        self.show_solved(lp, times, self.solve(times))
    }

    /// The shown face of `lp` under `times`, already solved.
    #[inline]
    fn show_solved(&self, lp: f64, times: Times, solved: Solved) -> u8 {
        let t = self.table;
        match solved {
            Solved::Factor(k) => {
                let s = lp * k;
                if lp <= 1.0 && s <= 1.0 {
                    return t.show[q16(s)];
                }
                ToneTable::faces(t.tone.eval_timed(lp, times)).1
            }
            Solved::Logged { soft, hard } => {
                ToneTable::faces(t.timed.as_ref().unwrap().eval_logged(lp, soft, hard)).1
            }
            Solved::Exact => ToneTable::faces(t.tone.eval_timed(lp, times)).1,
        }
    }

    /// The shown face alone: one table for the pixels the table holds.
    #[inline]
    fn show(&mut self, lp: f64, times: Times) -> u8 {
        let t = self.table;
        if times.is_unit() {
            return t.show[q16(lp)];
        }
        if let Solved::Factor(k) = self.solved(times) {
            let s = lp * k;
            if lp <= 1.0 && s <= 1.0 {
                return t.show[q16(s)];
            }
        }
        self.faces(lp, times).1
    }

    /// Histogram bin and clip flag.
    #[inline]
    fn view(&mut self, lp: f64, times: Times) -> (u8, u8) {
        let (view, _, clip) = self.faces(lp, times);
        (view, clip)
    }
}

impl ToneTable {
    /// The three faces of one print value: the histogram bin, the 8-bit
    /// face of the 16-bit code, and whether that code is an end.
    #[inline]
    fn faces(t: f64) -> (u8, u8, u8) {
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
    }

    fn build(tone: &ToneMap) -> Self {
        let built: Vec<(u8, u8, u8)> =
            (0..65536u32).into_par_iter().map(|i| Self::faces(tone.eval(f64::from(i) / 65535.0))).collect();
        let mut view = Vec::with_capacity(built.len());
        let mut show = Vec::with_capacity(built.len());
        let mut clip = Vec::with_capacity(built.len());
        for (v, s, c) in built {
            view.push(v);
            show.push(s);
            clip.push(c);
        }
        Self { view, show, clip, tone: tone.clone(), timed: tone.table() }
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

/// 8-bit face of the print without burns, for the tests' expectations.
#[cfg(test)]
fn display_bytes(plane: &Plane, place: f64, table: &ToneTable) -> Vec<u8> {
    display_bytes_with(plane, place, table, None)
}

/// 8-bit face of the print. `place == 0` quantises `v`; a placing quantises
/// `v · 2^place`, which is what the histogram counts. `field` is the burns
/// over this plane's own pixels; without one every pixel has unit times.
fn display_bytes_with(plane: &Plane, place: f64, table: &ToneTable, field: Option<&Field>) -> Vec<u8> {
    let global = 2f64.powf(place);
    let placed = |v: f64| if place == 0.0 { v } else { v * global };
    let Some(field) = field else {
        return plane.data.par_iter().map(|&v| table.show[q16(placed(v))]).collect();
    };
    let width = plane.width.max(1);
    let mut out = vec![0u8; plane.data.len()];
    out.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let src = plane.row(y);
        let mut timed = Timed::new(table);
        let line = FieldRow::varying(Some(field), y, 0, width);
        let (a, b) = (line.span.start.min(width), line.span.end.min(width));
        for (x, o) in row[..a].iter_mut().enumerate() {
            *o = timed.show(placed(src[x]), line.left);
        }
        for (x, o) in row[a..b].iter_mut().enumerate() {
            let x = a + x;
            *o = timed.show_fresh(placed(src[x]), line.at(x));
        }
        for (x, o) in row[b..].iter_mut().enumerate() {
            *o = timed.show(placed(src[b + x]), line.right);
        }
    });
    out
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

/// Partial histogram counts of one row (or their sum).
struct Counts {
    neg: [u32; 256],
    view: [u32; 256],
    neg_low: usize,
    neg_high: usize,
    view_low: usize,
    view_high: usize,
    /// Pixels counted: the rectangle, less what a turned window leaves out.
    /// The walk counts them; the tally brings its own total.
    #[cfg(test)]
    total: usize,
}

impl Default for Counts {
    fn default() -> Self {
        Self {
            neg: [0; 256],
            view: [0; 256],
            neg_low: 0,
            neg_high: 0,
            view_low: 0,
            view_high: 0,
            #[cfg(test)]
            total: 0,
        }
    }
}

#[cfg(test)]
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
        self.total += o.total;
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
    #[cfg(test)]
    pub fn of_separation(
        sep: &Separation,
        w: &Weights,
        table: &ToneTable,
        place: f64,
        window: Option<HistWindow>,
        field: Option<&Field>,
    ) -> (Self, Self) {
        let k1 = w.g - w.r - w.b;
        let k2 = 2.0 * (w.r - w.b);
        Self::count(sep.lum.width, sep.lum.height, window, table, place, field, |y| {
            let (l, c1, c2) = (sep.lum.row(y), sep.c1.row(y), sep.c2.row(y));
            move |x| l[x] + k1 * c1[x] + k2 * c2[x]
        })
    }

    /// The same counts as [`of_separation`] on a plane that has no colour.
    /// A monochrome negative is that plane. Weights do not enter.
    #[cfg(test)]
    pub fn of_plane(
        plane: &Plane,
        table: &ToneTable,
        place: f64,
        window: Option<HistWindow>,
        field: Option<&Field>,
    ) -> (Self, Self) {
        Self::count(plane.width, plane.height, window, table, place, field, |y| {
            let row = plane.row(y);
            move |x| row[x]
        })
    }

    /// The same counts as [`of_separation`] from cached `q16(mix)` codes.
    /// `place == 0` is exact: the view index is the code. A placing
    /// dequantises the code before scaling, which can move a bin at the
    /// boundary. The tests' reference for the tally.
    #[cfg(test)]
    fn of_codes(
        codes: &[u16],
        width: usize,
        table: &ToneTable,
        place: f64,
        window: Option<HistWindow>,
        field: Option<&Field>,
    ) -> (Self, Self) {
        let height = codes.len().checked_div(width).unwrap_or(0);
        Self::count(width, height, window, table, place, field, |y| {
            let row = &codes[y * width..(y + 1) * width];
            move |x| row[x] as f64 / 65535.0
        })
    }

    /// The same histograms from a tally of the window's codes: the tone and
    /// the placing rebin it, one step per code, the view code of a placed
    /// one being `q16` of the dequantised code (which can differ from the
    /// float walk by a bin at a boundary). The pixels a `field` of strokes
    /// reaches are taken out of the tally and counted at their own times,
    /// from `codes`; the negative's bins do not see the strokes. A large
    /// burned area is sampled on a lattice of `k` pixels, each sample
    /// standing for `k²`, so the walk stays within `BURN_WALK_BUDGET`
    /// pixels; up to that many it is exact. A sample stands for no more
    /// pixels than the tally still has of its code, so every pixel is
    /// counted once and the bins add up to the total.
    fn of_tally(
        tally: &Tally,
        codes: &[u16],
        width: usize,
        window: Option<HistWindow>,
        table: &ToneTable,
        place: f64,
        field: Option<&Field>,
    ) -> (Self, Self) {
        let lut_neg = neg_lut();
        let global = 2f64.powf(place);
        let view_code = |q: usize| if place == 0.0 { q } else { q16(q as f64 / 65535.0 * global) };
        let mut unit = tally.bins.clone();
        let mut burned = Counts::default();
        if let Some(field) = field {
            let height = codes.len().checked_div(width).unwrap_or(0);
            let (x0, y0, x1, y1) = hist_bounds(width, height, window);
            let quad = window.filter(|w| w.quad.is_some());
            let area: usize =
                (y0..y1).into_par_iter().map(|y| FieldRow::new(Some(field), y, x0, x1).span.len()).sum();
            let k = (area.div_ceil(BURN_WALK_BUDGET) as f64).sqrt().ceil().max(1.0) as usize;
            let each = (k * k) as u32;
            // Per row: the codes of the pixels with their own times, each
            // with its view bin and whether the paper clips it.
            let parts: Vec<Vec<(u16, u8, u8)>> = (y0..y1)
                .into_par_iter()
                .step_by(k)
                .filter_map(|y| {
                    let line = FieldRow::new(Some(field), y, x0, x1);
                    let span = line.span.clone();
                    if span.is_empty() {
                        return None;
                    }
                    let row = &codes[y * width..(y + 1) * width];
                    let mut timed = Timed::new(table);
                    let mut taken = Vec::new();
                    for x in span.step_by(k) {
                        if quad.is_some_and(|q| !q.holds(x, y)) {
                            continue;
                        }
                        let times = line.at(x);
                        if times.is_unit() {
                            continue;
                        }
                        let q = row[x];
                        let m = q as f64 / 65535.0;
                        let (bin, clip) = timed.view(if place == 0.0 { m } else { m * global }, times);
                        taken.push((q, bin, clip));
                    }
                    (!taken.is_empty()).then_some(taken)
                })
                .collect();
            for (q, bin, clip) in parts.into_iter().flatten() {
                // What is counted under the stroke is what leaves the
                // tally: `each`, or what the tally still has of the code.
                let n = unit[q as usize].min(each);
                unit[q as usize] -= n;
                burned.view[bin as usize] += n;
                match clip {
                    1 => burned.view_low += n as usize,
                    2 => burned.view_high += n as usize,
                    _ => {}
                }
            }
        }
        let mut all = burned;
        for (q, &n) in tally.bins.iter().enumerate() {
            if n > 0 {
                all.neg[lut_neg[q] as usize] += n;
            }
        }
        all.neg_low = tally.bins[0] as usize;
        all.neg_high = tally.bins[65535] as usize;
        for (q, &n) in unit.iter().enumerate() {
            if n > 0 {
                let qv = view_code(q);
                all.view[table.view[qv] as usize] += n;
                match table.clip[qv] {
                    1 => all.view_low += n as usize,
                    2 => all.view_high += n as usize,
                    _ => {}
                }
            }
        }
        let total = tally.total;
        (
            Self::from_bins(all.neg, total, all.neg_low, all.neg_high),
            Self::from_bins(all.view, total, all.view_low, all.view_high),
        )
    }

    /// The shared walk. `row(y)` gives a sampler of the linear value `m` at
    /// `x`; the negative code is `q16(m)` and the view code `q16(m · 2^place)`
    /// on the float (what `tone::place` does to the pixels), or the negative
    /// code itself when `place == 0`. A `field` of burns over the sensor's
    /// pixels gives a pixel its own times; the negative's bins do not see
    /// it. Row-ordered partial counts are merged deterministically.
    #[cfg(test)]
    fn count<R: Fn(usize) -> f64>(
        width: usize,
        height: usize,
        window: Option<HistWindow>,
        table: &ToneTable,
        place: f64,
        field: Option<&Field>,
        row: impl Fn(usize) -> R + Sync,
    ) -> (Self, Self) {
        let (x0, y0, x1, y1) = hist_bounds(width, height, window);
        let quad = window.filter(|w| w.quad.is_some());
        let lut_neg = neg_lut();
        let global = 2f64.powf(place);
        let parts: Vec<Counts> = (y0..y1)
            .into_par_iter()
            .map(|y| {
                let code_at = row(y);
                let mut c = Counts::default();
                let mut timed = Timed::new(table);
                let line = FieldRow::new(field, y, x0, x1);
                for x in x0..x1 {
                    if quad.is_some_and(|q| !q.holds(x, y)) {
                        continue;
                    }
                    c.total += 1;
                    let m = code_at(x);
                    let q = q16(m);
                    c.neg[lut_neg[q] as usize] += 1;
                    c.neg_low += (q == 0) as usize;
                    c.neg_high += (q == 65535) as usize;
                    let times = line.at(x);
                    let (bin, clip) = if times.is_unit() {
                        let qv = if place == 0.0 { q } else { q16(m * global) };
                        (table.view[qv], table.clip[qv])
                    } else {
                        timed.view(if place == 0.0 { m } else { m * global }, times)
                    };
                    c.view[bin as usize] += 1;
                    match clip {
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
        let total = all.total;
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
    /// The plate in colour, interleaved R, G, B, when the view asked for one.
    pub rgb: Option<Vec<u8>>,
    pub hist_view: Histogram,
    pub hist_negative: Histogram,
    /// The three channels of the plate as shown, counted on the preview's
    /// bytes: outlines over the print histogram, which stays the mix's.
    pub hist_rgb: Option<[[u32; 256]; 3]>,
}

/// Per-channel histograms of interleaved R G B bytes.
pub fn rgb_histograms(rgb: &[u8]) -> [[u32; 256]; 3] {
    let mut out = [[0u32; 256]; 3];
    for px in rgb.as_chunks::<3>().0 {
        for (c, &v) in px.iter().enumerate() {
            out[c][v as usize] += 1;
        }
    }
    out
}

/// Full-resolution histograms, and the mix codes they are counted from.
/// The codes depend on the session and the weights; the tally of them,
/// one count per 16-bit code, on the crop and the straighten as well. The
/// bins then depend on the tone, the placing and the strokes: the tone
/// and the placing are one map per code, so they rebin the tally, 65536
/// steps for 24 million pixels; only the pixels a stroke reaches are
/// walked, at their own times.
pub struct HistCache {
    session: Option<u64>,
    weights: Option<Weights>,
    codes: Vec<u16>,
    width: usize,
    /// The window's codes counted, by crop and angle (the weights through
    /// the codes), and the pixels in it.
    tally: Option<((Option<CropRect>, f64), Tally)>,
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
            tally: None,
            hist_key: None,
            hists: (Histogram::empty(), Histogram::empty()),
        }
    }
}

/// Burned pixels a histogram walks one by one at most; a larger burned
/// area is sampled on a lattice (`Histogram::of_tally`).
const BURN_WALK_BUDGET: usize = 1 << 20;

/// One count per 16-bit code over the pixels a histogram counts.
#[derive(Clone)]
struct Tally {
    bins: Vec<u32>,
    total: usize,
}

impl Tally {
    /// The codes of the window counted: the rows in a few contiguous
    /// blocks, one bin array each, merged at the end. (A bin array per
    /// rayon split would be hundreds of 256 KB arrays to add up.)
    fn of(codes: &[u16], width: usize, window: Option<HistWindow>) -> Self {
        let height = codes.len().checked_div(width).unwrap_or(0);
        let (x0, y0, x1, y1) = hist_bounds(width, height, window);
        let quad = window.filter(|w| w.quad.is_some());
        let rows = y1 - y0;
        let blocks = rayon::current_num_threads().clamp(1, 64).min(rows.max(1));
        let per_block = rows.div_ceil(blocks).max(1);
        let parts: Vec<Vec<u32>> = (0..blocks)
            .into_par_iter()
            .map(|b| {
                let mut bins = vec![0u32; 65536];
                let ya = y0 + b * per_block;
                let yb = (ya + per_block).min(y1);
                for y in ya..yb {
                    let row = &codes[y * width..(y + 1) * width];
                    for x in x0..x1 {
                        if quad.is_some_and(|q| !q.holds(x, y)) {
                            continue;
                        }
                        bins[row[x] as usize] += 1;
                    }
                }
                bins
            })
            .collect();
        let mut bins = vec![0u32; 65536];
        for part in parts {
            for (o, v) in bins.iter_mut().zip(part) {
                *o += v;
            }
        }
        let total = bins.iter().map(|&n| n as usize).sum();
        Self { bins, total }
    }
}

/// The rectangle of rows and columns a window counts, clamped to the plane.
fn hist_bounds(width: usize, height: usize, window: Option<HistWindow>) -> (usize, usize, usize, usize) {
    match window {
        Some(HistWindow { x, y, w, h, .. }) => {
            let x0 = x.min(width);
            let y0 = y.min(height);
            (x0, y0, x.saturating_add(w).min(width).max(x0), y.saturating_add(h).min(height).max(y0))
        }
        None => (0, 0, width, height),
    }
}

/// The sensor pixels a histogram counts: a rectangle, and with a turn the
/// tilted window inside it, tested pixel by pixel through `quad`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HistWindow {
    pub x: usize,
    pub y: usize,
    pub w: usize,
    pub h: usize,
    /// Sensor → frame, and the window on the frame `(x0, y0, x1, y1)`,
    /// half-open, continuous. `None` counts the whole rectangle.
    pub quad: Option<(Affine, [f64; 4])>,
}

impl HistWindow {
    /// Is sensor pixel `(x, y)` counted?
    #[inline]
    fn holds(&self, x: usize, y: usize) -> bool {
        match &self.quad {
            None => true,
            Some((m, [x0, y0, x1, y1])) => {
                let (fx, fy) = m.apply(x as f64 + 0.5, y as f64 + 0.5);
                fx >= *x0 && fx < *x1 && fy >= *y0 && fy < *y1
            }
        }
    }
}

/// What the histograms count: the crop, in sensor pixels. Without a crop
/// and without a turn, everything. A turned frame without a crop is the
/// frame itself: the corners the turn leaves out are not in the picture.
fn hist_window(s: &Session, v: &ViewParams) -> Option<HistWindow> {
    let (uw, uh) = s.upright_size();
    let (fw, fh) = s.frame_size(v.angle);
    let (sw, sh) = (s.negative.plane.width, s.negative.plane.height);
    let o = s.negative.orientation;
    let c = match v.crop {
        Some(c) => c.sanitized(fw as u32, fh as u32),
        None if straighten::turns(v.angle) => CropRect { x: 0, y: 0, w: fw as u32, h: fh as u32 },
        None => return None,
    };
    let frame_rect = DetailRect { x: c.x as usize, y: c.y as usize, w: c.w as usize, h: c.h as usize };
    if !straighten::turns(v.angle) {
        let r = source_rect(o, sw, sh, frame_rect);
        return Some(HistWindow { x: r.x, y: r.y, w: r.w, h: r.h, quad: None });
    }
    // The window's footprint upright (its corners through the turn), then
    // the sensor rectangle around that; inside it, every pixel is asked.
    let to_upright = straighten::frame_to_upright(uw, uh, v.angle);
    let pix = PixelRect { x: frame_rect.x, y: frame_rect.y, w: frame_rect.w, h: frame_rect.h };
    let (x0, y0, x1, y1) = straighten::footprint(&to_upright, ResampleKernel::Area, pix, uw, uh);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let r = source_rect(o, sw, sh, DetailRect { x: x0, y: y0, w: x1 - x0, h: y1 - y0 });
    let to_frame = to_upright.inverse()?.after(&straighten::orient_map(o, sw, sh));
    let quad = [c.x as f64, c.y as f64, (c.x + c.w) as f64, (c.y + c.h) as f64];
    Some(HistWindow { x: r.x, y: r.y, w: r.w, h: r.h, quad: Some((to_frame, quad)) })
}

/// The view's burns over the sensor's pixels, for the histogram: the
/// sensor to the file-upright negative, then normalised. The burns are
/// stored file-upright, so no turn enters.
fn hist_field(s: &Session, v: &ViewParams) -> Option<Field> {
    let (uw, uh) = s.upright_size();
    let (sw, sh) = (s.negative.plane.width, s.negative.plane.height);
    let to_upright = straighten::orient_map(s.negative.orientation, sw, sh);
    Field::new(&v.burns, burn::normalize(uw, uh).after(&to_upright))
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
            self.tally = None;
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
            self.tally = None;
            self.hist_key = None;
            return;
        };
        self.width = sep.lum.width;
        self.codes = mix_codes(sep, w);
        self.session = Some(ptr);
        self.weights = Some(*w);
        self.tally = None;
        self.hist_key = None;
    }

    /// Cached mix codes for this session and these weights. Sensor order.
    #[cfg(test)]
    pub fn codes_for(&mut self, s: &Session, w: &Weights) -> &[u16] {
        self.ensure(s, w);
        &self.codes
    }

    fn histograms(&mut self, s: &Session, v: &ViewParams, table: &ToneTable) -> (Histogram, Histogram) {
        let ptr = s.id;
        let key = v.hist_key();
        // Under a dragged card the last count stands; the release counts.
        if v.burn_live && self.session == Some(ptr) && self.hist_key.is_some() {
            return self.hists.clone();
        }
        if self.session != Some(ptr) || self.hist_key.as_ref() != Some(&key) {
            self.ensure(s, &v.weights);
            let window = hist_window(s, v);
            let field = hist_field(s, v);
            let tally_key = (v.crop, v.angle);
            if self.tally.as_ref().is_none_or(|(k, _)| *k != tally_key) {
                self.tally = Some((tally_key, Tally::of(&self.codes, self.width, window)));
            }
            let (_, tally) = self.tally.as_ref().unwrap();
            self.hists = if self.codes.is_empty() {
                (Histogram::empty(), Histogram::empty())
            } else {
                Histogram::of_tally(tally, &self.codes, self.width, window, table, v.place, field.as_ref())
            };
            self.hist_key = Some(key);
        }
        self.hists.clone()
    }
}

/// The phase table of `kernel` for the small preview's turn, built once;
/// `None` for a kernel other than the straighten's four-tap one, which is
/// evaluated (the mask's box).
fn phased(kernel: ResampleKernel) -> Option<&'static PhasedKernel> {
    static STRAIGHTEN: OnceLock<Option<PhasedKernel>> = OnceLock::new();
    (kernel == STRAIGHTEN_KERNEL)
        .then(|| STRAIGHTEN.get_or_init(|| PhasedKernel::new(kernel).ok()).as_ref())
        .flatten()
}

/// Scale a full-resolution upright window onto the preview plane.
fn scale_crop(c: CropRect, scale: f64, dst_w: usize, dst_h: usize) -> CropRect {
    let x = (f64::from(c.x) * scale).round().max(0.0) as u32;
    let y = (f64::from(c.y) * scale).round().max(0.0) as u32;
    let w = (f64::from(c.w) * scale).round().max(1.0) as u32;
    let h = (f64::from(c.h) * scale).round().max(1.0) as u32;
    CropRect { x, y, w, h }.sanitized(dst_w as u32, dst_h as u32)
}

/// What the preview worker keeps of one picture between two views, so a
/// view that changes only what comes later pays only that: the small
/// planes turned, straightened and cut (the geometry), the mix of them
/// (the weights), and then the placing, the tone and the strokes, which
/// are a lookup per pixel.
#[derive(Default)]
pub struct PreviewStages {
    shown: Option<(ShownKey, Shown)>,
    mixed: Option<((ShownKey, Weights), Plane)>,
    plate: Option<((ShownKey, PlateWeights), [Plane; 3])>,
    mask: Option<(ShownKey, Plane)>,
}

/// What the geometry of the preview depends on.
type ShownKey = (u64, u8, f64, Option<CropRect>);

/// The small planes as shown: turned, straightened, cut to the crop.
struct Shown {
    sep: Separation,
    /// The straightened small frame before the window, and the window cut
    /// on it, for the map from a preview pixel back to the frame.
    small: (usize, usize),
    window: CropRect,
}

impl PreviewStages {
    fn key(s: &Session, v: &ViewParams) -> ShownKey {
        (s.id, v.rotation, v.angle, v.crop)
    }

    /// Turn, straighten, then cut the window, on a small plane. The turn
    /// reads the kernel's weights from a table of phases (a quarter of the
    /// exact warp's time; the difference is below a 16-bit step), the 1:1
    /// tiles and the exports evaluate it.
    fn show(
        s: &Session,
        v: &ViewParams,
        plane: &Plane,
        kernel: ResampleKernel,
    ) -> (Plane, (usize, usize), CropRect) {
        let turned = orient(plane, rotated(1, v.rotation));
        let turned = match phased(kernel) {
            Some(k) => straighten::straighten_phased(&turned, v.angle, k),
            None => straighten::straighten(&turned, v.angle, kernel),
        };
        let small = (turned.width, turned.height);
        match v.crop {
            Some(c) => {
                let (file_fw, file_fh) = s.frame_size(v.angle);
                let window = c.turned(v.rotation, file_fw as u32, file_fh as u32);
                let scaled = scale_crop(window, s.preview.scale, turned.width, turned.height);
                (window_plane(&turned, scaled), small, scaled)
            }
            None => {
                let whole = CropRect { x: 0, y: 0, w: small.0 as u32, h: small.1 as u32 };
                (turned, small, whole)
            }
        }
    }

    fn shown(&mut self, s: &Session, v: &ViewParams) -> &Shown {
        let key = Self::key(s, v);
        if self.shown.as_ref().is_none_or(|(k, _)| *k != key) {
            let pp = &s.preview;
            let (lum, small, window) = Self::show(s, v, &pp.sep.lum, STRAIGHTEN_KERNEL);
            // A monochrome negative has no colour to show; its chrominance
            // stays the empty planes the mix ignores.
            let (c1, c2) = if s.is_mono() {
                (Plane::zeros(lum.width, lum.height), Plane::zeros(lum.width, lum.height))
            } else {
                (
                    Self::show(s, v, &pp.sep.c1, STRAIGHTEN_KERNEL).0,
                    Self::show(s, v, &pp.sep.c2, STRAIGHTEN_KERNEL).0,
                )
            };
            let sep = Separation {
                lum,
                c1,
                c2,
                mask_max: Plane::zeros(1, 1),
                rounds: pp.sep.rounds,
                phase: pp.sep.phase,
            };
            self.shown = Some((key, Shown { sep, small, window }));
            self.mixed = None;
            self.plate = None;
        }
        &self.shown.as_ref().unwrap().1
    }

    fn mixed(&mut self, s: &Session, v: &ViewParams) -> &Plane {
        let key = (Self::key(s, v), v.weights);
        if self.mixed.as_ref().is_none_or(|(k, _)| *k != key) {
            let shown = self.shown(s, v);
            let plane = mix(&shown.sep, &v.weights);
            self.mixed = Some((key, plane));
        }
        &self.mixed.as_ref().unwrap().1
    }

    fn plate(&mut self, s: &Session, v: &ViewParams, w: &PlateWeights) -> &[Plane; 3] {
        let key = (Self::key(s, v), *w);
        if self.plate.as_ref().is_none_or(|(k, _)| *k != key) {
            let shown = self.shown(s, v);
            let planes = mix_plate(&shown.sep, w);
            self.plate = Some((key, planes));
        }
        &self.plate.as_ref().unwrap().1
    }

    fn mask(&mut self, s: &Session, v: &ViewParams) -> &Plane {
        let key = Self::key(s, v);
        if self.mask.as_ref().is_none_or(|(k, _)| *k != key) {
            let (plane, _, _) = Self::show(s, v, &s.preview.mask, MASK_KERNEL);
            self.mask = Some((key, plane));
        }
        &self.mask.as_ref().unwrap().1
    }
}

/// 8-bit grey preview of the whole negative: the downscaled mix, then the
/// tone table (or the mask), in the picture's final orientation. The
/// stages that stand are taken from `stages`.
pub fn render_preview(
    s: &Session,
    v: &ViewParams,
    cache: &mut HistCache,
    stages: &mut PreviewStages,
    table: &ToneTable,
) -> PreviewImage {
    let to_u8 = |x: f64| (x.clamp(0.0, 1.0) * 255.0).round() as u8;
    if v.show_mask {
        let plane = stages.mask(s, v);
        let gray: Vec<u8> = plane.data.par_iter().map(|&m| to_u8(m)).collect();
        let hist_view = Histogram::of_bytes(&gray);
        return PreviewImage {
            width: plane.width,
            height: plane.height,
            rotation: v.rotation,
            gray,
            rgb: None,
            hist_view,
            hist_negative: Histogram::empty(),
            hist_rgb: None,
        };
    }
    // Histograms from the full-resolution separation, not the preview copy.
    let (hist_negative, hist_view) = cache.histograms(s, v, table);
    let g = Geometry::of(s, v);
    let (fw, fh) = (g.fw, g.fh);
    let (small, window) = {
        let shown = stages.shown(s, v);
        (shown.small, shown.window)
    };
    // The burns over the preview's pixels: a preview pixel to the small
    // frame, the small frame to the full one by its sides, then through
    // the straighten onto the upright negative.
    let field = g.field(v).map(|f| {
        let (sx, sy) = (fw as f64 / small.0.max(1) as f64, fh as f64 / small.1.max(1) as f64);
        f.through(&Affine {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            tx: f64::from(window.x) * sx,
            ty: f64::from(window.y) * sy,
        })
    });
    let mixed = stages.mixed(s, v);
    let (width, height) = (mixed.width, mixed.height);
    let gray = display_bytes_with(mixed, v.place, table, field.as_ref());
    let rgb = match v.plate.filter(|_| !s.is_mono()) {
        Some(plate) => {
            let planes = stages.plate(s, v, &plate.weights);
            Some(display_rgb_with(planes, v.place, plate.lamps, table, field.as_ref()))
        }
        None => None,
    };
    let hist_rgb = rgb.as_deref().map(rgb_histograms);
    PreviewImage { width, height, rotation: v.rotation, gray, rgb, hist_view, hist_negative, hist_rgb }
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
            Some(DetailImage { ask, gray, rgb: None })
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
            Some(DetailImage { ask, gray, rgb: None })
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

    /// The desktop program leaves every photosite as measured: no defect
    /// repair, whatever the core's default says.
    #[test]
    fn the_desktop_never_repairs_defects() {
        let p = DevelopParams::default().negative_params(None);
        assert!(!p.ingest.repair_defects);
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
        let sep =
            Separation { lum, c1, c2, mask_max: Plane::zeros(1, 1), rounds: 1, phase: BayerPhase::RGGB };
        let wts = Weights { r: 0.3, g: 0.5, b: 0.2 };
        let look = LookFile::load(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../look/reference.json"
        )))
        .unwrap();
        let curve = Tone::Look(look).compile();
        let (neg, view) = Histogram::of_separation(&sep, &wts, &ToneTable::build(&curve), 0.0, None, None);

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
            let (_, view) = Histogram::of_separation(&sep, &wts, &ToneTable::build(&tone), place, None, None);
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
        let (n1, v1) = Histogram::of_separation(&sep, &wts, &table, 0.0, None, None);
        let codes = mix_codes(&sep, &wts);
        let (n2, v2) = Histogram::of_codes(&codes, sep.lum.width, &table, 0.0, None, None);
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
        };
        let w = Weights { r: 0.2, g: 0.5, b: 0.3 };
        let tone = Tone::paper().compile();
        let table = ToneTable::build(&tone);
        let (n1, v1) = Histogram::of_separation(&sep, &w, &table, 0.4, None, None);
        let (n2, v2) = Histogram::of_plane(&lum, &table, 0.4, None, None);
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
            illuminant: None,
            wb_as_shot: None,
            exposure: Default::default(),
            data: RawData::Integer(raw),
        };
        let params = NegativeParams { keep_separation: true, ..NegativeParams::default() };
        let neg = pipeline::negative_from_frame(&frame, &params, 0).unwrap();
        assert!(neg.separation.is_none());
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
        let mut stages = PreviewStages::default();
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
            angle: 0.0,
            crop: None,
            burns: Vec::new(),
            burn_live: false,
            plate: None,
        };
        let detail =
            render_detail(&session, &view, DetailAsk::pixels(DetailRect { x: 0, y: 0, w, h }), &table)
                .unwrap();
        assert_eq!(detail.gray, display_bytes(&session.negative.plane, 0.0, &table));

        // Straightened: the 1:1 tile is the frame, and its pixels are the
        // upright negative warped with the recipe's kernel, as the export
        // writes them. The fit copy has the frame's size and proportion.
        let angle = 4.0;
        let turned = ViewParams { angle, ..view.clone() };
        let (fw, fh) = straighten::frame_size(w, h, angle);
        assert!(fw < w && fh < h);
        let detail = render_detail(
            &session,
            &turned,
            DetailAsk::pixels(DetailRect { x: 0, y: 0, w: fw, h: fh }),
            &table,
        )
        .unwrap();
        let expected = straighten::straighten(&session.negative.plane, angle, STRAIGHTEN_KERNEL);
        assert_eq!((expected.width, expected.height), (fw, fh));
        assert_eq!(detail.gray, display_bytes(&expected, 0.0, &table));
        let preview = render_preview(&session, &turned, &mut cache, &mut stages, &table);
        assert_eq!((preview.width, preview.height), straighten::frame_size(w, h, angle));
        // The histogram counts only the frame's pixels.
        assert!(preview.hist_view.total <= fw * fh);
        assert!(preview.hist_view.total > 0);
        // And the whole frame turned 90° is the frame of the turned negative.
        let quarter = ViewParams { rotation: 1, ..turned.clone() };
        let d = render_detail(
            &session,
            &quarter,
            DetailAsk::pixels(DetailRect { x: 0, y: 0, w: fh, h: fw }),
            &table,
        )
        .unwrap();
        assert_eq!(d.gray, display_bytes(&mimizan_core::print::orient(&expected, 6), 0.0, &table));
    }

    /// A burn in the view: the 1:1 tile gives the ellipse's pixels their
    /// times and leaves the rest as they were; a quarter turn of the picture
    /// turns the burn with it; the preview and its print histogram follow,
    /// the negative's histogram does not.
    #[test]
    fn a_burn_is_shown_turned_and_counted() {
        use mimizan_core::burn::{Light, Stroke, Times, Tip};
        use mimizan_core::decode::{RawData, Rect, SensorKind};
        use mimizan_core::pipeline::{self, NegativeParams};
        use std::sync::Arc;
        let (w, h) = (48usize, 32usize);
        // A mid-grey ramp, so that two stops more light shows on every pixel.
        let raw: Vec<u16> =
            (0..w * h).map(|i| (300.0 + 300.0 * ((i % w) as f64 / w as f64)) as u16).collect();
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
            illuminant: None,
            wb_as_shot: None,
            exposure: Default::default(),
            data: RawData::Integer(raw),
        };
        let params = NegativeParams { keep_separation: true, ..NegativeParams::default() };
        let neg = pipeline::negative_from_frame(&frame, &params, 0).unwrap();
        let preview = preview_planes(&neg).unwrap();
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
        let tone = Tone::paper().compile();
        let table = ToneTable::build(&tone);
        let plain = ViewParams {
            weights: Weights::default(),
            tone: Tone::paper(),
            place: 0.0,
            show_mask: false,
            rotation: 0,
            angle: 0.0,
            crop: None,
            burns: Vec::new(),
            burn_live: false,
            plate: None,
        };
        // An ellipse a quarter of the long side right of the centre, a tenth
        // of it in radius, two stops, a hard edge: its centre is pixel
        // (36, 16), its area about 72 pixels.
        let burn = Stroke::Hand {
            tip: Tip { rx: 0.1, ry: 0.1, angle: 0.0, feather: 0.0 },
            stops: 2.0,
            light: Light::Both,
            points: vec![[0.25, 0.0]],
        };
        let burned = ViewParams { burns: vec![burn.clone()], ..plain.clone() };
        let whole = DetailRect { x: 0, y: 0, w, h };
        let tile = |v: &ViewParams, rect: DetailRect| {
            render_detail(&session, v, DetailAsk::pixels(rect), &table).unwrap().gray
        };
        let (before, after) = (tile(&plain, whole), tile(&burned, whole));
        assert_eq!(before, display_bytes(&session.negative.plane, 0.0, &table));
        let four = Times { soft: 4.0, hard: 4.0, all: 4.0 };
        let centre = session.negative.plane.at(36, 16);
        assert_eq!(after[16 * w + 36], Timed::new(&table).show(centre, four));
        assert_ne!(after[16 * w + 36], before[16 * w + 36]);
        assert_eq!(after[2 * w + 2], before[2 * w + 2]);
        let changed = before.iter().zip(&after).filter(|(a, b)| a != b).count();
        assert!((60..=85).contains(&changed), "{changed} pixels changed");

        // The picture turned a quarter: the burn turns with it, so the tile
        // is the unturned tile turned.
        let quarter = ViewParams { rotation: 1, ..burned.clone() };
        let turned = tile(&quarter, DetailRect { x: 0, y: 0, w: h, h: w });
        let as_plane = Plane::from_vec(w, h, after.iter().map(|&b| f64::from(b)).collect());
        let expected: Vec<u8> =
            mimizan_core::print::orient(&as_plane, 6).data.iter().map(|&v| v as u8).collect();
        assert_eq!(turned, expected);

        // The preview and the print histogram see the burn; the negative's
        // histogram does not.
        let mut cache = HistCache::default();
        let mut stages = PreviewStages::default();
        let p0 = render_preview(&session, &plain, &mut cache, &mut stages, &table);
        let p1 = render_preview(&session, &burned, &mut cache, &mut stages, &table);
        assert_eq!((p1.width, p1.height), (p0.width, p0.height));
        assert_ne!(p1.gray, p0.gray);
        assert_ne!(p1.hist_view, p0.hist_view);
        assert_eq!(p1.hist_negative, p0.hist_negative);
        assert_eq!(p1.hist_view.total, p0.hist_view.total);
        // With the preview at full size, its bytes are the tile's.
        if (p1.width, p1.height) == (w, h) {
            assert_eq!(p1.gray, after);
        }
        // While the card is dragged the picture follows and the count
        // stands; the release counts.
        let moved = Stroke::Hand {
            tip: Tip { rx: 0.1, ry: 0.1, angle: 0.0, feather: 0.0 },
            stops: 2.0,
            light: Light::Both,
            points: vec![[0.0, 0.0]],
        };
        let live = ViewParams { burns: vec![moved], burn_live: true, ..plain.clone() };
        let p2 = render_preview(&session, &live, &mut cache, &mut stages, &table);
        assert_ne!(p2.gray, p1.gray);
        assert_eq!(p2.hist_view, p1.hist_view);
        let settled = ViewParams { burn_live: false, ..live };
        let p3 = render_preview(&session, &settled, &mut cache, &mut stages, &table);
        assert_eq!(p3.gray, p2.gray);
        assert_ne!(p3.hist_view, p1.hist_view);
    }

    /// The tally rebinned is the walk: to the pixel at place 0, with a
    /// window and a turn; under a stroke, or with a placing, the paper is
    /// read at the dequantised code rather than the float, which can move
    /// a boundary pixel by a bin, no more.
    #[test]
    fn the_tally_rebins_as_the_walk_counts() {
        use mimizan_core::burn::{Light, Stroke, Tip};
        let sep = sweep(64, 40);
        let wts = Weights { r: 0.3, g: 0.5, b: 0.2 };
        let table = ToneTable::build(&Tone::paper().compile());
        let codes = mix_codes(&sep, &wts);
        let (w, h) = (sep.lum.width, sep.lum.height);
        let to_frame = Affine { a: 0.98, b: 0.2, c: -0.2, d: 0.98, tx: 3.0, ty: -2.0 };
        let window =
            Some(HistWindow { x: 4, y: 3, w: 50, h: 30, quad: Some((to_frame, [5.0, 2.0, 50.0, 31.0])) });
        let stroke = Stroke::Hand {
            tip: Tip { rx: 0.12, ry: 0.08, angle: 20.0, feather: 0.03 },
            stops: 1.5,
            light: Light::Soft,
            points: vec![[-0.1, 0.0], [0.1, 0.05]],
        };
        let field = Field::new(std::slice::from_ref(&stroke), burn::normalize(w, h)).unwrap();
        for (window, field) in [(None, None), (window, None), (window, Some(&field)), (None, Some(&field))] {
            let tally = Tally::of(&codes, w, window);
            let (n1, v1) = Histogram::of_separation(&sep, &wts, &table, 0.0, window, field);
            let (n2, v2) = Histogram::of_tally(&tally, &codes, w, window, &table, 0.0, field);
            assert_eq!(n1, n2);
            assert!(v1.total > 0);
            if field.is_none() {
                assert_eq!(v1, v2);
            } else {
                assert_eq!((v1.total, v1.low, v1.high), (v2.total, v2.low, v2.high));
                let moved: u32 = v1.bins.iter().zip(&v2.bins).map(|(a, b)| a.abs_diff(*b)).sum();
                assert!(moved <= 2 * 2, "{moved} pixels moved under the stroke");
            }
        }
        for place in [-0.7, 0.4] {
            let tally = Tally::of(&codes, w, None);
            let (n1, v1) = Histogram::of_separation(&sep, &wts, &table, place, None, Some(&field));
            let (n2, v2) = Histogram::of_tally(&tally, &codes, w, None, &table, place, Some(&field));
            assert_eq!(n1, n2);
            assert_eq!(v1.total, v2.total);
            let moved: u32 = v1.bins.iter().zip(&v2.bins).map(|(a, b)| a.abs_diff(*b)).sum();
            assert!(moved <= 2 * 4, "{moved} pixels moved at place {place}");
        }
    }

    /// A burned area too large to walk is sampled, and every pixel is still
    /// counted once: the bins add up to the pixels, the clipped ones are
    /// among them. Codes that seldom repeat are the case where a sample
    /// stands for more pixels than the picture has of its code.
    #[test]
    fn a_sampled_burn_counts_every_pixel_once() {
        use mimizan_core::burn::{Light, Stroke, Tip};
        let (w, h) = (1600usize, 1400usize);
        assert!(w * h > 2 * BURN_WALK_BUDGET);
        let codes: Vec<u16> = (0..(w * h) as u64)
            .map(|i| {
                let mut z = i.wrapping_add(0x9E37_79B9_7F4A_7C15);
                z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
                z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
                ((z ^ (z >> 31)) >> 48) as u16
            })
            .collect();
        let table = ToneTable::build(&Tone::paper().compile());
        let stroke = Stroke::Hand {
            tip: Tip { rx: 0.9, ry: 0.9, angle: 0.0, feather: 0.0 },
            stops: 1.0,
            light: Light::Both,
            points: vec![[0.0, 0.0]],
        };
        let field = Field::new(std::slice::from_ref(&stroke), burn::normalize(w, h)).unwrap();
        let tally = Tally::of(&codes, w, None);
        let (plain_neg, plain) = Histogram::of_tally(&tally, &codes, w, None, &table, 0.0, None);
        let (neg, view) = Histogram::of_tally(&tally, &codes, w, None, &table, 0.0, Some(&field));
        let sum = |h: &Histogram| h.bins.iter().map(|&n| n as usize).sum::<usize>();
        assert_eq!(neg, plain_neg);
        assert_eq!((sum(&plain), plain.total), (w * h, w * h));
        assert_ne!(view.bins, plain.bins);
        assert_eq!((sum(&view), view.total), (w * h, w * h));
        assert!(view.low + view.high <= view.total);
    }

    /// The staged preview is the fresh one: a tone, a weight, a turn and a
    /// crop in turn, each read back from the stages that stand.
    #[test]
    fn the_stages_show_what_a_fresh_render_shows() {
        use mimizan_core::cfa::BayerPhase;
        use mimizan_core::decode::{RawData, Rect, SensorKind};
        use mimizan_core::pipeline::{self, NegativeParams};
        use std::sync::Arc;
        let (w, h) = (48usize, 32usize);
        let raw: Vec<u16> = (0..w * h)
            .map(|i| (200.0 + 500.0 * ((i % w) as f64 / w as f64) + 60.0 * ((i / w) % 3) as f64) as u16)
            .collect();
        let frame = RawFrame {
            make: "Test".into(),
            model: "Bayer".into(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: w,
            height: h,
            bits: 14,
            kind: SensorKind::Bayer(BayerPhase::RGGB),
            black_tile: [0.0; 4],
            white_rgb: [1000.0; 3],
            crop: Rect { x: 0, y: 0, w, h },
            orientation: 1,
            xyz_to_cam: None,
            illuminant: None,
            wb_as_shot: None,
            exposure: Default::default(),
            data: RawData::Integer(raw),
        };
        let params = NegativeParams { keep_separation: true, ..NegativeParams::default() };
        let neg = pipeline::negative_from_frame(&frame, &params, 0).unwrap();
        let preview = preview_planes(&neg).unwrap();
        let raw_hist = RawHistogram::of_frame(&frame);
        let session = Session {
            id: next_session_id(),
            path: "bayer.dng".into(),
            frame: Arc::new(frame),
            negative: neg,
            camera_file: None,
            camera_file_path: None,
            develop: DevelopParams::default(),
            preview,
            raw_hist,
        };
        let base = ViewParams {
            weights: Weights::default(),
            tone: Tone::paper(),
            place: 0.0,
            show_mask: false,
            rotation: 0,
            angle: 0.0,
            crop: None,
            burns: Vec::new(),
            burn_live: false,
            plate: None,
        };
        let plate =
            PlateView { weights: PlateWeights::channels(), space: PlateSpace::Srgb, lamps: [0.0, 0.3, 0.0] };
        let views = [
            base.clone(),
            ViewParams { tone: Tone::Paper(mimizan_core::paper::SplitGrade::new(0.5, -0.3)), ..base.clone() },
            ViewParams { place: -0.6, ..base.clone() },
            ViewParams { weights: Weights { r: 0.5, g: 0.3, b: 0.2 }, ..base.clone() },
            ViewParams { plate: Some(plate), ..base.clone() },
            ViewParams { angle: 3.0, plate: Some(plate), ..base.clone() },
            ViewParams { rotation: 1, angle: 3.0, ..base.clone() },
            ViewParams { crop: Some(CropRect { x: 4, y: 2, w: 30, h: 20 }), ..base.clone() },
            ViewParams { show_mask: true, angle: 3.0, ..base.clone() },
            base.clone(),
        ];
        let mut stages = PreviewStages::default();
        let mut cache = HistCache::default();
        for v in &views {
            let table = ToneTable::build(&v.tone.compile());
            let staged = render_preview(&session, v, &mut cache, &mut stages, &table);
            let fresh =
                render_preview(&session, v, &mut HistCache::default(), &mut PreviewStages::default(), &table);
            assert_eq!((staged.width, staged.height), (fresh.width, fresh.height), "{v:?}");
            assert_eq!(staged.gray, fresh.gray, "{v:?}");
            assert_eq!(staged.rgb, fresh.rgb, "{v:?}");
            assert_eq!(staged.hist_view, fresh.hist_view, "{v:?}");
            assert_eq!(staged.hist_negative, fresh.hist_negative, "{v:?}");
        }
        // The linear tile re-toned is the fresh tile, for the file and the print.
        use mimizan_core::burn::{Light, Stroke, Tip};
        let stroke = Stroke::Hand {
            tip: Tip { rx: 0.1, ry: 0.1, angle: 0.0, feather: 0.02 },
            stops: 1.0,
            light: Light::Both,
            points: vec![[0.1, 0.0], [0.2, 0.1]],
        };
        let geometry = ViewParams { angle: 2.0, plate: Some(plate), ..base.clone() };
        let lin = render_detail_linear(&session, &geometry, DetailAsk { x: 3, y: 2, w: 30, h: 20, step: 1 })
            .unwrap();
        let grid = render_detail_linear(&session, &geometry, DetailAsk { x: 0, y: 0, w: 20, h: 14, step: 2 })
            .unwrap();
        let recipe = Recipe {
            angle: 2.0,
            size: Some(mimizan_core::print::PrintSize::Physical { width_in: 0.4, height_in: 0.25 }),
            dpi: 300.0,
            ..Recipe::default()
        };
        let print =
            render_print_linear(&session, &geometry, &recipe, DetailRect { x: 2, y: 2, w: 40, h: 30 })
                .unwrap();
        for later in [
            ViewParams { place: 0.3, ..geometry.clone() },
            ViewParams {
                tone: Tone::Paper(mimizan_core::paper::SplitGrade::new(-0.4, 0.6)),
                ..geometry.clone()
            },
            ViewParams { burns: vec![stroke.clone()], ..geometry.clone() },
            ViewParams { plate: Some(PlateView { lamps: [0.2, 0.0, -0.2], ..plate }), ..geometry.clone() },
        ] {
            let table = ToneTable::build(&later.tone.compile());
            let a = tone_tile(&lin, &later, None, &table);
            let b = render_detail(&session, &later, lin.ask, &table).unwrap();
            assert_eq!((a.ask, a.gray, a.rgb), (b.ask, b.gray, b.rgb), "{later:?}");
            let a = tone_tile(&grid, &later, None, &table);
            let b = render_detail(&session, &later, grid.ask, &table).unwrap();
            assert_eq!((a.ask, a.gray, a.rgb), (b.ask, b.gray, b.rgb), "grid {later:?}");
            let later_recipe = Recipe {
                place: later.place,
                curve: later.tone.clone(),
                burns: later.burns.clone(),
                ..recipe.clone()
            };
            let a = tone_tile(&print, &later, Some(&later_recipe), &table);
            let r = DetailRect { x: print.ask.x, y: print.ask.y, w: print.ask.w, h: print.ask.h };
            let b = render_print_detail(&session, &later, &later_recipe, r, &table).unwrap();
            assert_eq!((a.ask, a.gray, a.rgb), (b.ask, b.gray, b.rgb), "print {later:?}");
        }
    }

    /// A plate on a grey Bayer mosaic: the preview, the 1:1 window, the
    /// screen grid, the fitted-print window and the print box all carry
    /// colour bytes, three per sample, and every one of them is grey, and
    /// equal to the monochrome byte. A red lamp lifts red alone. A pan of
    /// the tile keeps the colour.
    #[test]
    fn a_plate_on_grey_shows_grey_in_colour() {
        use mimizan_core::cfa::BayerPhase;
        use mimizan_core::decode::{RawData, Rect, SensorKind};
        use mimizan_core::pipeline::{self, NegativeParams};
        use std::sync::Arc;
        let (w, h) = (48usize, 32usize);
        // A soft ramp: the separation leaves a grey mosaic grey.
        let raw: Vec<u16> =
            (0..w * h).map(|i| (200.0 + 600.0 * ((i % w) as f64 / w as f64)) as u16).collect();
        let frame = RawFrame {
            make: "Test".into(),
            model: "Bayer".into(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: w,
            height: h,
            bits: 10,
            kind: SensorKind::Bayer(BayerPhase::RGGB),
            black_tile: [0.0; 4],
            white_rgb: [1000.0; 3],
            crop: Rect { x: 0, y: 0, w, h },
            orientation: 1,
            xyz_to_cam: None,
            illuminant: None,
            wb_as_shot: Some([1.0, 1.0, 1.0]),
            exposure: Default::default(),
            data: RawData::Integer(raw),
        };
        let params = NegativeParams { keep_separation: true, ..NegativeParams::default() };
        let neg = pipeline::negative_from_frame(&frame, &params, 0).unwrap();
        let preview = preview_planes(&neg).unwrap();
        let raw_hist = RawHistogram::of_frame(&frame);
        let session = Session {
            id: next_session_id(),
            path: "grey.nef".into(),
            frame: Arc::new(frame),
            negative: neg,
            camera_file: None,
            camera_file_path: None,
            develop: DevelopParams::default(),
            preview,
            raw_hist,
        };
        assert!(!session.is_mono());
        let plate_params = PlateParams::default();
        // No matrix: the camera set falls back to the channels.
        let weights = session.plate_weights(&plate_params, Weights::default()).unwrap();
        assert_eq!(weights, PlateWeights::channels());
        let plate = PlateView { weights, space: PlateSpace::AdobeRgb, lamps: [0.0; 3] };
        let tone = Tone::paper().compile();
        let table = ToneTable::build(&tone);
        let view = ViewParams {
            weights: Weights::default(),
            tone: Tone::paper(),
            place: 0.0,
            show_mask: false,
            rotation: 1,
            angle: 0.0,
            crop: None,
            burns: Vec::new(),
            burn_live: false,
            plate: Some(plate),
        };
        let grey_in_colour = |gray: &[u8], rgb: &[u8]| {
            assert_eq!(rgb.len(), gray.len() * 3);
            for (g, px) in gray.iter().zip(rgb.as_chunks::<3>().0) {
                assert_eq!(*px, [*g, *g, *g], "a grey mosaic prints grey");
            }
        };

        let mut cache = HistCache::default();
        let mut stages = PreviewStages::default();
        let pv = render_preview(&session, &view, &mut cache, &mut stages, &table);
        grey_in_colour(&pv.gray, pv.rgb.as_ref().unwrap());
        assert_eq!(pv.gray.len(), pv.width * pv.height);

        let ask = DetailAsk::pixels(DetailRect { x: 3, y: 2, w: 20, h: 12 });
        let d = render_detail(&session, &view, ask, &table).unwrap();
        grey_in_colour(&d.gray, d.rgb.as_ref().unwrap());
        let grid = DetailAsk { x: 0, y: 0, w: 10, h: 8, step: 2 };
        let d = render_detail(&session, &view, grid, &table).unwrap();
        grey_in_colour(&d.gray, d.rgb.as_ref().unwrap());

        let mut recipe = Recipe::on_paper();
        recipe.size = Some(mimizan_core::print::PrintSize::Physical { width_in: 1.0, height_in: 1.0 });
        recipe.dpi = 72.0;
        let rect = DetailRect { x: 0, y: 0, w: 24, h: 24 };
        let d = render_print_detail(&session, &view, &recipe, rect, &table).unwrap();
        grey_in_colour(&d.gray, d.rgb.as_ref().unwrap());

        let o = rotated(1, view.rotation);
        let lp = render_sharpen_loupe(&session, &view.weights, Some(&plate), &recipe, o, 0.5, 0.5, 16);
        assert!(lp.width > 0);
        grey_in_colour(&lp.gray, lp.rgb.as_ref().unwrap());
        // Peak in the box runs on every positive alike: grey stays grey.
        let mut peaked = recipe.clone();
        peaked.peak.on = true;
        peaked.peak.radius = 1.0;
        peaked.peak.amount = 0.8;
        let lp2 = render_sharpen_loupe(&session, &view.weights, Some(&plate), &peaked, o, 0.5, 0.5, 16);
        assert_eq!((lp2.width, lp2.height), (lp.width, lp.height));
        grey_in_colour(&lp2.gray, lp2.rgb.as_ref().unwrap());

        // A red lamp: red brighter, green and blue as before.
        let lit = PlateView { lamps: [0.5, 0.0, 0.0], ..plate };
        let lit_view = ViewParams { plate: Some(lit), ..view.clone() };
        let d0 = render_detail(&session, &view, ask, &table).unwrap();
        let d1 = render_detail(&session, &lit_view, ask, &table).unwrap();
        let (a, b) = (d0.rgb.unwrap(), d1.rgb.unwrap());
        let mut lifted = 0;
        for (p, q) in a.as_chunks::<3>().0.iter().zip(b.as_chunks::<3>().0) {
            assert!(q[0] >= p[0]);
            lifted += (q[0] > p[0]) as usize;
            assert_eq!(q[1], p[1]);
            assert_eq!(q[2], p[2]);
        }
        assert!(lifted > 0);

        // A pan keeps the colour and matches a fresh render.
        let paint = |a: DetailAsk| render_detail(&session, &view, a, &table);
        let first = DetailAsk { x: 4, y: 4, w: 16, h: 12, step: 1 };
        let have = cover_detail(None, first, paint);
        assert!(have.rgb.is_some());
        let next = DetailAsk { x: 8, y: 6, w: 16, h: 12, step: 1 };
        let got = cover_detail(Some(&have), next, paint);
        let fresh = paint(next).unwrap();
        assert_eq!(got.gray, fresh.gray);
        assert_eq!(got.rgb, fresh.rgb);
    }
}
