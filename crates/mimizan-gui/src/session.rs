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
use mimizan_core::icc::GrayTrc;
use mimizan_core::mix::{mix, Weights};
use mimizan_core::paper::{self, PaperTable};
use mimizan_core::pipeline::{Negative, NegativeParams};
use mimizan_core::plane::Plane;
use mimizan_core::plate::{mix_plate, PlateParams, PlateSpace, PlateWeights};
use mimizan_core::print::{
    burn_field, file_sample, layout, loupe_source_rect, orient, rotated, sample_curve, sharpen_window,
    source_window, window_plane, PrintParams,
};
use mimizan_core::recipe::{CropRect, Develop, Recipe};
use mimizan_core::resize::{
    fit_dims, resize, resize_rect, source_coverage, PixelRect, PlaneWindow, ResampleKernel,
};
use mimizan_core::separate::Separation;
use mimizan_core::straighten::{self, Affine, PhasedKernel, STRAIGHTEN_KERNEL};
use mimizan_core::tone::{Tone, ToneMap};
use rayon::prelude::*;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

/// Longest preview edge in pixels (the texture the GUI shows).
pub const PREVIEW_MAX: usize = 1800;

/// What the pipeline develops a frame with: the development the recipe
/// carries (`Develop`: the settings that need a new separation, ingest
/// and separate, not just a re-mix) and the camera file.
///
/// Every photosite stays as measured: the desktop has no defect repair and
/// no reconstruction. Defects are counted and shown in the Negative window;
/// the command's `--repair-defects` is there for a hot pixel from a long
/// exposure.
pub fn negative_params(develop: &Develop, cam: Option<CameraFile>) -> NegativeParams {
    let mut p = NegativeParams { keep_separation: true, ..NegativeParams::default() };
    p.ingest.wb = develop.wb;
    p.separate.mask = develop.mask;
    if let Some(c) = cam {
        p = p.with_camera_file(c);
    }
    p
}

/// Sensor width and height, and the negative a session keeps. With a
/// separation the full-resolution mix is let go: every view mixes from the
/// separation and every export mixes again, and only the size was read. A
/// monochrome negative is the picture, and its plane stays.
pub(crate) fn kept_negative(mut negative: Negative) -> (Negative, usize, usize) {
    let (width, height) = (negative.plane.width, negative.plane.height);
    if negative.separation.is_some() {
        negative.plane = Plane::zeros(0, 0);
    }
    (negative, width, height)
}

/// The upright, downscaled separation the interactive view draws.
pub struct PreviewPlanes {
    pub sep: Separation,
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
    /// The sensor's width and height. A colour session lets the
    /// full-resolution mix go; the size stays here.
    pub width: usize,
    pub height: usize,
    pub camera_file: Option<CameraFile>,
    pub camera_file_path: Option<PathBuf>,
    pub develop: Develop,
    pub preview: PreviewPlanes,
    /// The four CFA channels of the frame as shot; fixed for the session.
    pub raw_hist: RawHistogram,
    /// One line for the log about how this file was opened: a DNG that
    /// was written, or why least squares was not used. The log says it
    /// once; a later open that has nothing to say has none.
    pub note: Option<String>,
}

/// The next session id. Starts at 1 so 0 can mean "none" in a cache.
pub fn next_session_id() -> u64 {
    static NEXT: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);
    NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
}

/// Histogram of the four sensor channels (one per 2x2 tile position) over
/// the active area, normalised by black and white level and binned on a
/// gamma-2.2 axis like the negative histogram. A monochrome sensor is one
/// channel in `bins[0]`, a three-channel file R, G, B in `bins[0..3]`.
/// Nothing but the frame goes in, so it is computed once per file.
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
    /// Three channels a pixel and no tile: R, G, B, the fourth entry empty.
    pub channels: bool,
}

impl RawHistogram {
    fn empty() -> Self {
        Self {
            bins: [[0; 256]; 4],
            colors: [Color::G; 4],
            clipped: [0; 4],
            total: [0; 4],
            mono: false,
            channels: false,
        }
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
                let mut count = |t: usize, raw: f64, black: f64, white: f64| {
                    let v = (raw - black) / (white - black).max(1.0);
                    let q = (v.clamp(0.0, 1.0) * 65535.0).round() as usize;
                    bins[t][lut[q] as usize] += 1;
                    clipped[t] += (v >= 1.0) as usize;
                    total[t] += 1;
                };
                for x in x0..x1 {
                    if frame.kind == SensorKind::Rgb {
                        let raw = frame.rgb_at(x, y);
                        for (c, &v) in raw.iter().enumerate() {
                            let (black, white) = frame.channel_levels(c);
                            count(c, v, black, white);
                        }
                        continue;
                    }
                    let t = if mono { 0 } else { (y & 1) * 2 + (x & 1) };
                    count(t, frame.data.get(base + x), frame.black_at(x, y), frame.white_at(x, y));
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
                for (t, color) in out.colors.iter_mut().enumerate() {
                    *color = phase.color_at(t & 1, t >> 1);
                }
            }
            SensorKind::Mono => out.mono = true,
            SensorKind::Rgb => {
                out.colors = [Color::R, Color::G, Color::B, Color::G];
                out.channels = true;
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
    /// A monochrome sensor: one measured plane, no separation.
    pub fn is_mono(&self) -> bool {
        self.negative.separation.is_none()
    }

    /// The sensor's width and height.
    pub fn size(&self) -> (usize, usize) {
        (self.width, self.height)
    }

    /// A file that came with its three channels (a linear DNG): there was
    /// no mosaic to separate. In every other respect a colour file.
    pub fn is_channels(&self) -> bool {
        self.frame.kind == SensorKind::Rgb
    }

    /// None on a monochrome negative: `is_mono` is this, asked the other
    /// way round.
    pub fn separation(&self) -> Option<&Separation> {
        self.negative.separation.as_ref()
    }

    /// Full-resolution sharpening mask (the sidecar bytes as a plane).
    pub fn mask_plane(&self) -> mimizan_core::Result<Plane> {
        let (w, h) = self.size();
        mask_plane_of(&self.negative, w, h)
    }

    /// Upright size of the full negative.
    pub fn upright_size(&self) -> (usize, usize) {
        let (w, h) = self.size();
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

/// What the screen shows of a print value: the sample the print's file
/// holds for it, by the core's own rule (`print::sample_curve`,
/// `print::file_sample`), so the picture in the window is the picture in
/// the file. A plate in sRGB is written on the sRGB curve, each value as
/// the sRGB encoding of the light it stands for, and is shown so: its
/// deep shadows stand lower than the print's values, the rest all but
/// where it was. The grey sheet and a plate in the Adobe space hold the
/// print's own values, and so does the screen. A grey sheet written as
/// RGB is the grey print in another file and is shown as the grey print.
///
/// Only what shows a tone goes through here: the picture, the 1:1 window,
/// the print box, the sheet's rings, the paper strip. What counts instead
/// — the histograms, the clip marks — stays on the print's values.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Screen(GrayTrc);

impl Screen {
    /// The print's own values: the grey sheet on paper.
    #[cfg(test)]
    pub const PRINT: Self = Self(GrayTrc::Gamma(2.2));

    /// The screen of a print through `tone`: a plate when one is shown,
    /// the grey sheet otherwise.
    pub fn of(tone: &Tone, plate: Option<&PlateView>) -> Self {
        Self(sample_curve(plate.map(|p| p.space), tone.encoding()))
    }

    /// The screen value of a print value, both in [0, 1].
    #[inline]
    pub fn value(self, v: f64) -> f64 {
        file_sample(v, self.0)
    }

    /// The screen's byte of a print value.
    #[inline]
    pub fn byte(self, v: f64) -> u8 {
        (self.value(v) * 255.0).round() as u8
    }

    /// The print's values as they are: nothing is encoded again.
    #[inline]
    pub fn is_print(self) -> bool {
        !matches!(self.0, GrayTrc::Srgb)
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
    let sep_small = Separation {
        lum: sh.small(&sep.lum),
        c1: sh.small(&sep.c1),
        c2: sh.small(&sep.c2),
        mask_max: Plane::zeros(1, 1),
        rounds: sep.rounds,
        phase: sep.phase,
    };
    Ok(PreviewPlanes { sep: sep_small, scale: sh.scale() })
}

/// Preview of a measured plane. Chrominance stays empty at preview size so
/// the existing mix draws the measurement and nothing else.
fn preview_measured(neg: &Negative) -> mimizan_core::Result<PreviewPlanes> {
    let sh = PreviewShrink::new(neg);
    let lum = sh.small(&neg.plane);
    let zero = Plane::zeros(sh.pw, sh.ph);
    let sep = Separation {
        lum,
        c1: zero.clone(),
        c2: zero,
        mask_max: Plane::zeros(1, 1),
        rounds: 0,
        phase: mimizan_core::cfa::BayerPhase::RGGB,
    };
    Ok(PreviewPlanes { sep, scale: sh.scale() })
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
    /// What the screen shows this view's print values as: the plate's
    /// file when the picture is shown in colour, the grey sheet's when
    /// it is not (a monochrome sensor has no plate).
    pub fn screen(&self, s: &Session) -> Screen {
        Screen::of(&self.tone, self.plate.as_ref().filter(|_| !s.is_mono()))
    }

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
        let (sw, sh) = s.size();
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
    /// pixels upright, and the warp resamples them with the straighten's
    /// kernel. `None` without a turn, or when the source cannot be had.
    fn warped<const N: usize>(
        &self,
        rect: DetailRect,
        upright_of: impl Fn(DetailRect) -> Option<[Plane; N]>,
    ) -> Option<[Plane; N]> {
        let kernel = STRAIGHTEN_KERNEL;
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

/// The step at which a window of `span_w` × `span_h` file pixels, with
/// its margin, is a texture the graphics card takes: `step`, or the first
/// coarser grid with no side over `max_side` samples. A texture larger
/// than that limit (8192 with the Windows build's renderer, as little as
/// 4096 on an old card) is not refused, it ends the program.
pub fn detail_step(step: usize, span_w: usize, span_h: usize, max_side: usize) -> usize {
    // The margin on both sides, and the two samples the grid's rounding
    // may add.
    let room = max_side.saturating_sub(2 * DETAIL_MARGIN + 2).max(1);
    step.max(1).max(span_w.div_ceil(room)).max(span_h.div_ceil(room))
}

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

    /// The same grid with no side over `max_side` samples. What is over
    /// comes off both ends alike, so the middle of the window stays: the
    /// last net under [`detail_step`], for a window that must keep its
    /// step.
    pub fn within(self, max_side: usize) -> Self {
        let (max_side, step) = (max_side.max(1), self.step());
        let trim = |at: usize, n: usize| {
            if n > max_side {
                (at + (n - max_side) / 2 * step, max_side)
            } else {
                (at, n)
            }
        };
        let (x, w) = trim(self.x, self.w);
        let (y, h) = trim(self.y, self.h);
        Self { x, y, w, h, step: self.step }
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

/// `ask` of the negative as the worker shows it, its two steps in one, for
/// the tests' expectations: the linear picture of the ask
/// ([`render_detail_linear`]) placed, toned and burned ([`tone_tile`]).
#[cfg(test)]
fn render_detail(s: &Session, v: &ViewParams, ask: DetailAsk, table: &ToneTable) -> Option<DetailImage> {
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
/// plates with it when the view is in colour.
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
            let [plane] =
                g.warped(rect, |sensor| Some([mixed_upright(sep, weights, sensor, g.orientation)?]))?;
            return Some(plane);
        }
        let src = source_rect(g.orientation, g.sw, g.sh, rect);
        return mixed_upright(sep, weights, src, g.orientation);
    }
    Some(sampled(g, rect, step, |sx, sy| mix_at(sep, weights, sx, sy)))
}

/// The three plates over `rect`, as [`mixed_linear`]: on a coarser grid
/// the plates of the nearest sensor pixel of each sample.
fn plate_linear(
    sep: &Separation,
    w: &PlateWeights,
    g: &Geometry,
    rect: DetailRect,
    step: usize,
) -> Option<[Plane; 3]> {
    if step == 1 {
        if g.map.is_some() {
            return g.warped(rect, |sensor| plate_upright(sep, w, sensor, g.orientation));
        }
        let sensor = source_rect(g.orientation, g.sw, g.sh, rect);
        return plate_upright(sep, w, sensor, g.orientation);
    }
    // The separation read on the grid, then the core's plate: its three
    // mixes and the settling of a plate the weights drive below no light,
    // as at 1:1, in the fit copy and in the file.
    let small = Separation {
        lum: sampled(g, rect, step, |sx, sy| sep.lum.at(sx, sy)),
        c1: sampled(g, rect, step, |sx, sy| sep.c1.at(sx, sy)),
        c2: sampled(g, rect, step, |sx, sy| sep.c2.at(sx, sy)),
        mask_max: Plane::zeros(1, 1),
        rounds: sep.rounds,
        phase: sep.phase,
    };
    Some(mix_plate(&small, w))
}

/// The measured plane of a monochrome negative over `rect`, as
/// [`mixed_linear`].
fn measured_linear(s: &Session, g: &Geometry, rect: DetailRect, step: usize) -> Option<Plane> {
    let n = &s.negative;
    if step == 1 {
        if g.map.is_some() {
            let [plane] = g.warped(rect, |sensor| Some([orient(&crop(&n.plane, sensor)?, g.orientation)]))?;
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
                px[c] = table.show_of(planes[c].data[idx] * gains[c]);
            }
        }
    });
    out
}

/// The three channels of a plate print counted on the print's values:
/// the pixels of [`display_rgb_with`], each in the bin the histogram
/// gives its print value, whatever the screen shows it as.
fn plate_histograms(
    planes: &[Plane; 3],
    place: f64,
    lamps: [f64; 3],
    table: &ToneTable,
    field: Option<&Field>,
) -> [[u32; 256]; 3] {
    let n = planes.iter().map(|p| p.data.len()).min().unwrap_or(0);
    let gains = lamps.map(|l| 2f64.powf(place + l));
    let width = planes[0].width.max(1);
    (0..n.div_ceil(width))
        .into_par_iter()
        .map(|y| {
            let mut bins = [[0u32; 256]; 3];
            let mut timed = Timed::new(table);
            let line = FieldRow::varying(field, y, 0, width);
            for idx in y * width..((y + 1) * width).min(n) {
                let t = line.at(idx - y * width);
                for c in 0..3 {
                    bins[c][usize::from(timed.view(planes[c].data[idx] * gains[c], t).0)] += 1;
                }
            }
            bins
        })
        .reduce(
            || [[0u32; 256]; 3],
            |mut a, b| {
                for (x, y) in a.iter_mut().flatten().zip(b.iter().flatten()) {
                    *x += y;
                }
                a
            },
        )
}

/// The visible window of a fitted print as the worker shows it, its two
/// steps in one, for the tests' expectations: the recipe's kernel on the
/// negative ([`render_print_linear`]), then the same table as the file's
/// 16-bit codes ([`tone_tile`]).
#[cfg(test)]
fn render_print_detail(
    s: &Session,
    v: &ViewParams,
    recipe: &Recipe,
    rect: DetailRect,
    table: &ToneTable,
) -> Option<DetailImage> {
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
    let (sensor_w, sensor_h) = s.size();
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
    let (sensor_w, sensor_h) = s.size();
    let (full_w, full_h) =
        if matches!(orientation, 5..=8) { (sensor_h, sensor_w) } else { (sensor_w, sensor_h) };
    let (sx, sy, ex, ey) = loupe_source_rect(full_w, full_h, &params, nx, ny, view);
    // USM is the only step that reads the mask, and only then is the crop
    // wide enough for the mask's own resample (`MASK_KERNEL`).
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
        let mask = want_mask
            .then(|| mask_window(&s.negative, sensor_w, sensor_h, sensor))
            .flatten()
            .map(|m| orient(&m, orientation));
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
    // The box shows the file's samples, as the picture beside it does.
    let screen = Screen::of(&recipe.curve, plate);
    let to_u8 = |v: f64| screen.byte(v);
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
/// not match the sensor.
fn mask_plane_of(n: &Negative, width: usize, height: usize) -> mimizan_core::Result<Plane> {
    Plane::try_from_vec(width, height, n.mask8.par_iter().map(|&v| f64::from(v) / 255.0).collect())
}

/// A window of the sidecar (sensor frame) as a plane in [0, 1]. `None`
/// when the sidecar does not fit the sensor or the window lies outside.
fn mask_window(n: &Negative, width: usize, height: usize, r: DetailRect) -> Option<Plane> {
    let samples = width.checked_mul(height)?;
    if n.mask8.len() != samples || r.w == 0 || r.h == 0 {
        return None;
    }
    if r.x.saturating_add(r.w) > width || r.y.saturating_add(r.h) > height {
        return None;
    }
    let mut out = Plane::zeros(r.w, r.h);
    out.data.par_chunks_mut(r.w).enumerate().for_each(|(y, row)| {
        let base = (r.y + y) * width + r.x;
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

/// Steps of the tone table per unit of placed linear value above 1.
const ABOVE_STEPS: usize = 8192;
/// Where the tone table ends: three stops and a sixth above 1. A value
/// past it is evaluated, not looked up.
const TABLE_TOP: f64 = 9.0;
/// Slots of the tone table: the 16-bit codes up to 1, then the steps above.
const TABLE_SLOTS: usize = 65536 + (TABLE_TOP as usize - 1) * ABOVE_STEPS;

/// The table's slot for a placed linear value: `q16` up to 1, as before
/// the negative kept what stands above it, then steps of 1/8192 up to
/// [`TABLE_TOP`] (the paper is at its toe there: a step is a hundredth of
/// an 8-bit level at most). `None` past the table's end.
#[inline]
fn slot(x: f64) -> Option<usize> {
    if x > 1.0 {
        (x <= TABLE_TOP).then(|| 65535 + ((x - 1.0) * ABOVE_STEPS as f64 + 0.5) as usize)
    } else {
        Some(q16(x))
    }
}

/// The placed linear value a slot of the table stands for.
#[inline]
fn slot_value(i: usize) -> f64 {
    if i <= 65535 {
        i as f64 / 65535.0
    } else {
        1.0 + (i - 65535) as f64 / ABOVE_STEPS as f64
    }
}

/// One table of the paper: index [`slot`] of the placed linear value, then
/// the print.
///
/// The screen and the histogram share it. Building it is one evaluation
/// per slot; a slider then looks up, it does not solve the paper again per
/// pixel. `view` is the histogram bin `(eval * 255).round()`. `show` is the
/// 8-bit face of the 16-bit code the TIFF stores: `q16(eval)`, and for a
/// file on the sRGB curve (`screen`) `q16` of the sample the file holds for
/// it. The bin and the clip flag stay the print value's on every screen.
/// Quantising the linear value first is the histogram's contract; the
/// file evaluates the float and then quantises, so a code can differ by
/// the last bit.
/// Above 1 the table goes on as the negative does: a highlight the sensor
/// read above the level at which green is full prints lighter than the
/// clip, and the screen shows it so.
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
    /// What `show` is of a print value: the file's sample.
    screen: Screen,
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
    /// The table at the value scaled by this: `eval_timed(l, t) == eval(l·k)`,
    /// above 1 as below it.
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
            return t.faces_of(lp);
        }
        match self.solved(times) {
            Solved::Factor(k) => t.faces_of(lp * k),
            Solved::Logged { soft, hard } => t.faces(t.timed.as_ref().unwrap().eval_logged(lp, soft, hard)),
            Solved::Exact => t.faces(t.tone.eval_timed(lp, times)),
        }
    }

    /// `show` for a pixel whose times are its own: solved afresh, the
    /// last pixel's solution neither read nor kept. In a stroke's
    /// penumbra, where every pixel differs, that is the shorter path:
    /// the pixels do not wait on one another.
    #[inline]
    fn show_fresh(&self, lp: f64, times: Times) -> u8 {
        if times.is_unit() {
            return self.table.show_of(lp);
        }
        self.show_solved(lp, times, self.solve(times))
    }

    /// The shown face of `lp` under `times`, already solved.
    #[inline]
    fn show_solved(&self, lp: f64, times: Times, solved: Solved) -> u8 {
        let t = self.table;
        match solved {
            Solved::Factor(k) => t.show_of(lp * k),
            Solved::Logged { soft, hard } => t.faces(t.timed.as_ref().unwrap().eval_logged(lp, soft, hard)).1,
            Solved::Exact => t.faces(t.tone.eval_timed(lp, times)).1,
        }
    }

    /// The shown face alone: one table for the pixels the table holds.
    #[inline]
    fn show(&mut self, lp: f64, times: Times) -> u8 {
        let t = self.table;
        if times.is_unit() {
            return t.show_of(lp);
        }
        if let Solved::Factor(k) = self.solved(times) {
            return t.show_of(lp * k);
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
    /// The three faces of one print value on `screen`: the histogram
    /// bin, the 8-bit face of the 16-bit code the file holds for it, and
    /// whether the print's own code is an end.
    #[inline]
    fn faces_on(t: f64, screen: Screen) -> (u8, u8, u8) {
        let code = q16(t);
        let view = (t.clamp(0.0, 1.0) * 255.0).round() as u8;
        let held = if screen.is_print() { code } else { q16(screen.value(t)) };
        let show = ((held as f64 / 65535.0) * 255.0).round() as u8;
        let clip = if code == 0 {
            1
        } else if code == 65535 {
            2
        } else {
            0
        };
        (view, show, clip)
    }

    /// The three faces of one print value on this table's screen.
    #[inline]
    fn faces(&self, t: f64) -> (u8, u8, u8) {
        Self::faces_on(t, self.screen)
    }

    /// The shown face of a placed linear value: the table, or past its
    /// end the tone itself.
    #[inline]
    fn show_of(&self, x: f64) -> u8 {
        match slot(x) {
            Some(i) => self.show[i],
            None => self.faces(self.tone.eval(x)).1,
        }
    }

    /// `(view, show, clip)` of a placed linear value.
    #[inline]
    fn faces_of(&self, x: f64) -> (u8, u8, u8) {
        match slot(x) {
            Some(i) => (self.view[i], self.show[i], self.clip[i]),
            None => self.faces(self.tone.eval(x)),
        }
    }

    /// The table of the grey sheet: the screen shows the print's values.
    #[cfg(test)]
    fn build(tone: &ToneMap) -> Self {
        Self::build_on(tone, Screen::PRINT)
    }

    /// The table of `tone` for a picture shown on `screen`.
    fn build_on(tone: &ToneMap, screen: Screen) -> Self {
        let built: Vec<(u8, u8, u8)> = (0..TABLE_SLOTS)
            .into_par_iter()
            .map(|i| Self::faces_on(tone.eval(slot_value(i)), screen))
            .collect();
        let mut view = Vec::with_capacity(built.len());
        let mut show = Vec::with_capacity(built.len());
        let mut clip = Vec::with_capacity(built.len());
        for (v, s, c) in built {
            view.push(v);
            show.push(s);
            clip.push(c);
        }
        Self { view, show, clip, tone: tone.clone(), timed: tone.table(), screen }
    }
}

/// The paper for `tone` on `screen`, kept while the exposures and the
/// file's curve stay put. A pan reuses it.
#[derive(Default)]
pub struct ToneHold {
    held: Option<(Tone, Screen, ToneTable)>,
}

impl ToneHold {
    pub fn get(&mut self, tone: &Tone, screen: Screen) -> &ToneTable {
        let fresh = !matches!(&self.held, Some((t, on, _)) if t == tone && *on == screen);
        if fresh {
            self.held = Some((tone.clone(), screen, ToneTable::build_on(&tone.compile(), screen)));
        }
        &self.held.as_ref().unwrap().2
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

/// The 16-bit codes of the full-resolution mix as the negative's file
/// holds them, sensor order, and the file's range: `q16(mix / range)`, the
/// range the highest value of the mix and 1 when nothing stands above 1
/// (`pipeline::scene_range`), so a code times the range over 65535 is the
/// scene value. Rebuilt when the weights change, then every exposure is a
/// lookup.
fn mix_codes(sep: &Separation, w: &Weights) -> (Vec<u16>, f64) {
    let width = sep.lum.width;
    if w.is_native() {
        return codes_of(width, sep.lum.height, |y| {
            let l = sep.lum.row(y);
            move |x| l[x]
        });
    }
    let k1 = w.g - w.r - w.b;
    let k2 = 2.0 * (w.r - w.b);
    codes_of(width, sep.lum.height, |y| {
        let (l, c1, c2) = (sep.lum.row(y), sep.c1.row(y), sep.c2.row(y));
        move |x| l[x] + k1 * c1[x] + k2 * c2[x]
    })
}

/// [`mix_codes`] of a plane as it stands: a monochrome negative.
fn plane_codes(plane: &Plane) -> (Vec<u16>, f64) {
    codes_of(plane.width, plane.height, |y| {
        let row = plane.row(y);
        move |x| row[x]
    })
}

/// The codes of `row(y)(x)` and their range. One pass when nothing stands
/// above 1, which is every file but the ones with burnt highlights; a
/// second one, by the range, when something does.
fn codes_of<R: Fn(usize) -> f64>(
    width: usize,
    height: usize,
    row: impl Fn(usize) -> R + Sync,
) -> (Vec<u16>, f64) {
    let mut out = vec![0u16; width * height];
    if width == 0 {
        return (out, 1.0);
    }
    let top = out
        .par_chunks_mut(width)
        .enumerate()
        .map(|(y, codes)| {
            let at = row(y);
            let mut top = 1.0f64;
            for (x, o) in codes.iter_mut().enumerate() {
                let v = at(x);
                if v.is_finite() && v > top {
                    top = v;
                }
                *o = q16(v) as u16;
            }
            top
        })
        .reduce(|| 1.0, f64::max);
    if top > 1.0 {
        out.par_chunks_mut(width).enumerate().for_each(|(y, codes)| {
            let at = row(y);
            for (x, o) in codes.iter_mut().enumerate() {
                *o = q16(at(x) / top) as u16;
            }
        });
    }
    (out, top)
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
        return plane.data.par_iter().map(|&v| table.show_of(placed(v))).collect();
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
    /// quantised to 16 bits first, as the negative's file holds them (over
    /// `range`, the highest value of the mix, 1 when nothing stands above
    /// 1), so the clipping counts are the TIFF's own 0 and 65535 pixels.
    /// Nothing is downscaled: a resampled copy rings at
    /// edges (false black and white) and averages isolated clipped pixels
    /// away, so its histogram ends are wrong by an order of magnitude.
    #[cfg(test)]
    pub fn of_separation(
        sep: &Separation,
        w: &Weights,
        range: f64,
        table: &ToneTable,
        place: f64,
        window: Option<HistWindow>,
        field: Option<&Field>,
    ) -> (Self, Self) {
        let k1 = w.g - w.r - w.b;
        let k2 = 2.0 * (w.r - w.b);
        Self::count(sep.lum.width, sep.lum.height, range, window, table, place, field, |y| {
            let (l, c1, c2) = (sep.lum.row(y), sep.c1.row(y), sep.c2.row(y));
            move |x| l[x] + k1 * c1[x] + k2 * c2[x]
        })
    }

    /// The same counts as [`of_separation`] on a plane that has no colour.
    /// A monochrome negative is that plane. Weights do not enter.
    #[cfg(test)]
    pub fn of_plane(
        plane: &Plane,
        range: f64,
        table: &ToneTable,
        place: f64,
        window: Option<HistWindow>,
        field: Option<&Field>,
    ) -> (Self, Self) {
        Self::count(plane.width, plane.height, range, window, table, place, field, |y| {
            let row = plane.row(y);
            move |x| row[x]
        })
    }

    /// The same counts as [`of_separation`] from the cached codes of the
    /// mix and their range ([`mix_codes`]). `place == 0` is exact: the view
    /// is read at the code. A placing dequantises the code before scaling,
    /// which can move a bin at the boundary. The tests' reference for the
    /// tally.
    #[cfg(test)]
    fn of_codes(
        codes: &[u16],
        range: f64,
        width: usize,
        table: &ToneTable,
        place: f64,
        window: Option<HistWindow>,
        field: Option<&Field>,
    ) -> (Self, Self) {
        let height = codes.len().checked_div(width).unwrap_or(0);
        Self::count(width, height, range, window, table, place, field, |y| {
            let row = &codes[y * width..(y + 1) * width];
            move |x| row[x] as f64 / 65535.0 * range
        })
    }

    /// The same histograms from a tally of the window's codes: the tone and
    /// the placing rebin it, one step per code, the view of a placed code
    /// (or of a code under a `range` above 1) being read at the dequantised
    /// code (which can differ from the float walk by a bin at a boundary).
    /// The pixels a `field` of strokes
    /// reaches are taken out of the tally and counted at their own times,
    /// from `codes`; the negative's bins do not see the strokes. Every
    /// such pixel is counted with its own code. Over a large burned area
    /// the times are read on a lattice of `k` pixels, one sample for its
    /// cell of `k²`, so the field is asked at most `BURN_WALK_BUDGET`
    /// times: up to that many pixels the count is exact, beyond it a
    /// penumbra is `k − 1` pixels coarser.
    #[allow(clippy::too_many_arguments)]
    fn of_tally(
        tally: &Tally,
        codes: &[u16],
        range: f64,
        width: usize,
        window: Option<HistWindow>,
        table: &ToneTable,
        place: f64,
        field: Option<&Field>,
    ) -> (Self, Self) {
        let lut_neg = neg_lut();
        // A code as the placed scene value: the code itself when the file
        // has no range and nothing is placed.
        let plain = place == 0.0 && range == 1.0;
        let scale = if plain { 1.0 } else { range * 2f64.powf(place) };
        let placed = |q: u16| if plain { f64::from(q) / 65535.0 } else { f64::from(q) / 65535.0 * scale };
        let mut unit = tally.bins.clone();
        let mut burned = Counts::default();
        if let Some(field) = field {
            let height = codes.len().checked_div(width).unwrap_or(0);
            let (x0, y0, x1, y1) = hist_bounds(width, height, window);
            let quad = window.filter(|w| w.quad.is_some());
            let area: usize =
                (y0..y1).into_par_iter().map(|y| FieldRow::new(Some(field), y, x0, x1).span.len()).sum();
            let k = (area.div_ceil(BURN_WALK_BUDGET) as f64).sqrt().ceil().max(1.0) as usize;
            // The lattice rows in a few contiguous blocks, one array of
            // codes each, as the tally is counted.
            let lattice: Vec<usize> = (y0..y1).step_by(k).collect();
            let blocks = rayon::current_num_threads().clamp(1, 64).min(lattice.len().max(1));
            let parts: Vec<(Vec<u32>, Counts)> = lattice
                .par_chunks(lattice.len().div_ceil(blocks).max(1))
                .map(|rows| {
                    let mut taken = vec![0u32; 65536];
                    let mut counts = Counts::default();
                    let mut timed = Timed::new(table);
                    let mut count = |counts: &mut Counts, q: u16, times: Times, n: u32| {
                        let (bin, clip) = timed.view(placed(q), times);
                        counts.view[bin as usize] += n;
                        match clip {
                            1 => counts.view_low += n as usize,
                            2 => counts.view_high += n as usize,
                            _ => {}
                        }
                    };
                    // The times that repeat from sample to sample, a card's
                    // far side or a stroke's interior, are most of a large
                    // burn: their pixels are counted by code, and the paper
                    // is asked once a code at the end.
                    let mut even: Option<(Times, Vec<u32>)> = None;
                    let mut last = Times::UNIT;
                    for &y in rows {
                        let line = FieldRow::new(Some(field), y, x0, x1);
                        let span = line.span.clone();
                        for x in span.clone().step_by(k) {
                            let times = line.at(x);
                            if times.is_unit() {
                                continue;
                            }
                            if even.is_none() && times == last {
                                even = Some((times, vec![0u32; 65536]));
                            }
                            last = times;
                            let mut by_code = even.as_mut().filter(|(t, _)| *t == times).map(|(_, n)| n);
                            // The cell of this sample: its times, and every
                            // pixel's own code.
                            for cy in y..(y + k).min(y1) {
                                let row = &codes[cy * width..(cy + 1) * width];
                                for (cx, &q) in row.iter().enumerate().take((x + k).min(span.end)).skip(x) {
                                    if quad.is_some_and(|w| !w.holds(cx, cy)) {
                                        continue;
                                    }
                                    taken[q as usize] += 1;
                                    match by_code.as_mut() {
                                        Some(n) => n[q as usize] += 1,
                                        None => count(&mut counts, q, times, 1),
                                    }
                                }
                            }
                        }
                    }
                    if let Some((times, by_code)) = even {
                        for (q, n) in by_code.into_iter().enumerate().filter(|(_, n)| *n > 0) {
                            count(&mut counts, q as u16, times, n);
                        }
                    }
                    (taken, counts)
                })
                .collect();
            for (taken, counts) in parts {
                for (left, n) in unit.iter_mut().zip(taken) {
                    debug_assert!(n <= *left, "the tally is of another window than the walk");
                    *left = left.saturating_sub(n);
                }
                for (sum, n) in burned.view.iter_mut().zip(counts.view) {
                    *sum += n;
                }
                burned.view_low += counts.view_low;
                burned.view_high += counts.view_high;
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
                let (bin, _, clip) =
                    if plain { (table.view[q], 0, table.clip[q]) } else { table.faces_of(placed(q as u16)) };
                all.view[bin as usize] += n;
                match clip {
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
    /// `x`; the negative code is the file's, `q16(m / range)`, and the view
    /// is read at `m · 2^place` on the float (what `tone::place` does to
    /// the pixels), or at the negative code itself when `place == 0`. A
    /// `field` of burns over the sensor's pixels gives a pixel its own
    /// times; the negative's bins do not see it. Row-ordered partial counts
    /// are merged deterministically.
    #[cfg(test)]
    #[allow(clippy::too_many_arguments)]
    fn count<R: Fn(usize) -> f64>(
        width: usize,
        height: usize,
        range: f64,
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
                    let q = if range == 1.0 { q16(m) } else { q16(m / range) };
                    c.neg[lut_neg[q] as usize] += 1;
                    c.neg_low += (q == 0) as usize;
                    c.neg_high += (q == 65535) as usize;
                    let times = line.at(x);
                    let (bin, clip) = if times.is_unit() {
                        if place != 0.0 {
                            let (bin, _, clip) = table.faces_of(m * global);
                            (bin, clip)
                        } else if range == 1.0 {
                            (table.view[q], table.clip[q])
                        } else {
                            let (bin, _, clip) = table.faces_of(q as f64 / 65535.0 * range);
                            (bin, clip)
                        }
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
    /// The three channels of the plate, counted on the preview's pixels
    /// by their print values: outlines over the print histogram, which
    /// stays the mix's.
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
    /// What the codes are over: the range of the negative's file, 1 when
    /// nothing in the mix stands above 1.
    range: f64,
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
            range: 1.0,
            width: 0,
            tally: None,
            hist_key: None,
            hists: (Histogram::empty(), Histogram::empty()),
        }
    }
}

/// Burned pixels whose times a histogram reads one by one at most; over
/// a larger burned area they are read on a lattice (`Histogram::of_tally`).
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
    let (sw, sh) = s.size();
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
    let (sw, sh) = s.size();
    let to_upright = straighten::orient_map(s.negative.orientation, sw, sh);
    Field::new(&v.burns, burn::normalize(uw, uh).after(&to_upright))
}

impl HistCache {
    fn ensure(&mut self, s: &Session, w: &Weights) {
        let ptr = s.id;
        // A monochrome sensor has no mix: the measured plane, whatever the
        // weights say.
        let Some(sep) = s.separation() else {
            if self.session == Some(ptr) && self.width == s.width && !self.codes.is_empty() {
                return;
            }
            self.width = s.width;
            (self.codes, self.range) = plane_codes(&s.negative.plane);
            self.session = Some(ptr);
            self.weights = None;
            self.tally = None;
            self.hist_key = None;
            return;
        };
        if self.session == Some(ptr) && self.weights.as_ref() == Some(w) {
            return;
        }
        self.width = sep.lum.width;
        (self.codes, self.range) = mix_codes(sep, w);
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
                Histogram::of_tally(
                    tally,
                    &self.codes,
                    self.range,
                    self.width,
                    window,
                    table,
                    v.place,
                    field.as_ref(),
                )
            };
            self.hist_key = Some(key);
        }
        self.hists.clone()
    }
}

/// The phase table of the straighten's kernel for the small preview's
/// turn, built once; `None` when the kernel has no table and is evaluated.
fn phased() -> Option<&'static PhasedKernel> {
    static STRAIGHTEN: OnceLock<Option<PhasedKernel>> = OnceLock::new();
    STRAIGHTEN.get_or_init(|| PhasedKernel::new(STRAIGHTEN_KERNEL).ok()).as_ref()
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
    fn show(s: &Session, v: &ViewParams, plane: &Plane) -> (Plane, (usize, usize), CropRect) {
        let turned = orient(plane, rotated(1, v.rotation));
        let turned = match phased() {
            Some(k) => straighten::straighten_phased(&turned, v.angle, k),
            None => straighten::straighten(&turned, v.angle, STRAIGHTEN_KERNEL),
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
            let (lum, small, window) = Self::show(s, v, &pp.sep.lum);
            // A monochrome negative has no colour to show; its chrominance
            // stays the empty planes the mix ignores.
            let (c1, c2) = if s.is_mono() {
                (Plane::zeros(lum.width, lum.height), Plane::zeros(lum.width, lum.height))
            } else {
                (Self::show(s, v, &pp.sep.c1).0, Self::show(s, v, &pp.sep.c2).0)
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
}

/// 8-bit grey preview of the whole negative: the downscaled mix, then the
/// tone table, in the picture's final orientation. The
/// stages that stand are taken from `stages`.
pub fn render_preview(
    s: &Session,
    v: &ViewParams,
    cache: &mut HistCache,
    stages: &mut PreviewStages,
    table: &ToneTable,
) -> PreviewImage {
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
    let (rgb, hist_rgb) = match v.plate.filter(|_| !s.is_mono()) {
        Some(plate) => {
            let planes = stages.plate(s, v, &plate.weights);
            let rgb = display_rgb_with(planes, v.place, plate.lamps, table, field.as_ref());
            // The outlines stand on the print's values, as the bars under
            // them do. Where the screen shows the print's values the
            // bytes are those; where it shows the file's samples the
            // channels are counted through the table's other face.
            let hist = if table.screen.is_print() {
                rgb_histograms(&rgb)
            } else {
                plate_histograms(planes, v.place, plate.lamps, table, field.as_ref())
            };
            (Some(rgb), Some(hist))
        }
        None => (None, None),
    };
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

    /// A window between half size and 1:1 on a wide screen is up to twice
    /// the screen a side. Whatever the screen and the picture, the tile
    /// asked for is a texture the card takes.
    #[test]
    fn the_sharp_window_is_never_wider_than_the_largest_texture() {
        for max_side in [2048usize, 4096, 8192, 16384] {
            for (uw, uh) in [(6048usize, 4032usize), (9504, 6336), (11648, 8736), (16000, 400)] {
                for visible in [1000usize, 3840, 5120, 7680, 10240, uw] {
                    let (x1, y1) = (visible.min(uw), (visible * 2 / 3).clamp(1, uh));
                    let step = detail_step(1, x1, y1, max_side);
                    let ask = DetailAsk::covering(0, 0, x1, y1, step, uw, uh).unwrap();
                    let grown = ask.expand(DETAIL_MARGIN, uw, uh).within(max_side);
                    assert!(grown.w <= max_side && grown.h <= max_side, "{max_side} {uw}x{uh} {visible}");
                    assert!(grown.covers(&ask), "the margin goes first: {max_side} {uw}x{uh} {visible}");
                    // No coarser than the limit asks for.
                    if x1 + 2 * DETAIL_MARGIN + 2 <= max_side && y1 + 2 * DETAIL_MARGIN + 2 <= max_side {
                        assert_eq!(step, 1);
                    }
                }
            }
        }
        // A window that must keep its step keeps its middle.
        let wide = DetailAsk { x: 100, y: 0, w: 5000, h: 300, step: 1 }.within(4096);
        assert_eq!((wide.x, wide.w, wide.y, wide.h), (100 + 452, 4096, 0, 300));
        let coarse = DetailAsk { x: 0, y: 0, w: 10, h: 9, step: 4 }.within(4);
        assert_eq!((coarse.x, coarse.w, coarse.y, coarse.h, coarse.step), (12, 4, 8, 4, 4));
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

    /// The plate on a coarser grid is the plate at 1:1, every `step`-th
    /// pixel of it: also where the weights drive a plate below no light
    /// and the core brings it back, the other two giving up the same
    /// share. Unsettled, such a pixel had another colour below 1:1 than at
    /// 1:1 and in the file.
    #[test]
    fn a_plate_below_one_to_one_is_the_plate_at_one_to_one() {
        let (w, h) = (12usize, 8usize);
        let mut lum = Plane::zeros(w, h);
        let mut c1 = Plane::zeros(w, h);
        let mut c2 = Plane::zeros(w, h);
        for (i, ((l, a), b)) in
            lum.data.iter_mut().zip(c1.data.iter_mut()).zip(c2.data.iter_mut()).enumerate()
        {
            *l = 0.05 + 0.01 * (i % 7) as f64;
            *a = 0.04 * ((i % 5) as f64 - 2.0);
            *b = 0.05 * ((i % 3) as f64 - 1.0);
        }
        let sep = Separation {
            lum,
            c1,
            c2,
            mask_max: Plane::zeros(1, 1),
            rounds: 1,
            phase: mimizan_core::cfa::BayerPhase::RGGB,
        };
        // A set with negative terms, as a camera matrix has.
        let weights = PlateWeights {
            rows: [[1.8, -0.6, -0.2], [-0.3, 1.6, -0.3], [0.0, -0.7, 1.7]],
            ..PlateWeights::channels()
        };
        let unsettled = weights.weights().map(|wc| mix(&sep, &wc));
        let whole = mix_plate(&sep, &weights);
        assert!(
            (0..3).any(|c| unsettled[c].data != whole[c].data),
            "the patch has a plate below no light, or the test sees nothing"
        );
        for orientation in [1u16, 3, 6] {
            let (uw, uh) = if matches!(orientation, 5..=8) { (h, w) } else { (w, h) };
            let g = Geometry { sw: w, sh: h, orientation, uw, uh, fw: uw, fh: uh, map: None };
            let full = DetailRect { x: 0, y: 0, w: uw, h: uh };
            let fine = plate_linear(&sep, &weights, &g, full, 1).unwrap();
            let step = 2;
            let grid = DetailRect { x: 0, y: 0, w: uw / step, h: uh / step };
            let coarse = plate_linear(&sep, &weights, &g, grid, step).unwrap();
            for c in 0..3 {
                for j in 0..grid.h {
                    for i in 0..grid.w {
                        assert_eq!(
                            coarse[c].at(i, j),
                            fine[c].at(i * step, j * step),
                            "plate {c} at {i},{j}, orientation {orientation}"
                        );
                    }
                }
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
        let p = negative_params(&Develop::default(), None);
        assert!(!p.ingest.repair_defects);
    }
}

#[cfg(test)]
mod histogram_tests {
    use super::*;
    use mimizan_core::calib::LookFile;
    use mimizan_core::cfa::BayerPhase;

    /// The one-pass full-resolution histogram must agree with the plain
    /// per-pixel computation, including the 16-bit clipping counts. The
    /// sweep runs above 1: the codes are the file's, over its range, and
    /// the paper is read at the scene value the code stands for.
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
        let mixed = mix(&sep, &wts);
        let range = mimizan_core::pipeline::scene_range(&mixed);
        assert!(range > 1.15, "the sweep stands above 1: {range}");
        for curve in [Tone::Look(look).compile(), Tone::paper().compile()] {
            let (neg, view) =
                Histogram::of_separation(&sep, &wts, range, &ToneTable::build(&curve), 0.0, None, None);
            let mut en = [0u32; 256];
            let mut ev = [0u32; 256];
            let (mut nl, mut nh, mut vl, mut vh) = (0, 0, 0, 0);
            for &m in &mixed.data {
                let q = q16(m / range);
                nl += (q == 0) as usize;
                nh += (q == 65535) as usize;
                let lin = q as f64 / 65535.0;
                en[(lin.powf(1.0 / 2.2) * 255.0).round() as usize] += 1;
                // The scene value of the file's code, at the table's step.
                let v = curve.eval(slot_value(slot(lin * range).unwrap()));
                ev[(v * 255.0).round() as usize] += 1;
                vl += (q16(v) == 0) as usize;
                vh += (q16(v) == 65535) as usize;
            }
            assert_eq!(neg.bins, en);
            assert_eq!(view.bins, ev);
            assert_eq!((neg.low, neg.high), (nl, nh));
            assert_eq!((view.low, view.high), (vl, vh));
            // Black still clips; the top of the file is its highest pixel.
            assert!(nl > 1 && nh == 1, "{nl} at black, {nh} at the top");
            assert_eq!(neg.total, w * h);
        }
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
    /// The placing on the paper lifts the top of the sweep above 1, where
    /// the paper goes on printing lighter and the table follows it.
    #[test]
    fn of_separation_matches_the_rendered_picture() {
        let sep = sweep(40, 24);
        let wts = Weights::default();
        for (tone, place) in
            [(Tone::Look(LookFile::neutral()).compile(), -0.4), (Tone::paper().compile(), 0.2)]
        {
            let (_, view) =
                Histogram::of_separation(&sep, &wts, 1.0, &ToneTable::build(&tone), place, None, None);
            let mut pic = mix(&sep, &wts);
            mimizan_core::tone::place(&mut pic, place);
            let mut ev = [0u32; 256];
            for &l in &pic.data {
                let lin = slot_value(slot(l).unwrap());
                ev[(tone.eval(lin) * 255.0).round() as usize] += 1;
            }
            assert_eq!(view.bins, ev);
            let above = pic.data.iter().filter(|&&l| l > 1.0).count();
            assert_eq!(above > 0, place > 0.0, "{above} values above 1 at place {place}");
        }
    }

    /// Place 0 counts the cached codes. The bins are the float walk's bins.
    #[test]
    fn of_codes_matches_of_separation_without_placing() {
        let sep = sweep(40, 24);
        let wts = Weights { r: 0.2, g: 0.5, b: 0.3 };
        let tone = Tone::paper().compile();
        let table = ToneTable::build(&tone);
        let (n1, v1) = Histogram::of_separation(&sep, &wts, 1.0, &table, 0.0, None, None);
        let (codes, range) = mix_codes(&sep, &wts);
        assert_eq!(range, 1.0);
        let (n2, v2) = Histogram::of_codes(&codes, range, sep.lum.width, &table, 0.0, None, None);
        assert_eq!(n1, n2);
        assert_eq!(v1, v2);
    }

    /// The byte on screen is the 8-bit face of the 16-bit print code, of
    /// a value above 1 as of one below it.
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
            let code = q16(tone.eval(slot_value(slot(v).unwrap())));
            let expect = ((code as f64 / 65535.0) * 255.0).round() as u8;
            assert_eq!(*byte, expect);
        }
        assert!(got[15] > got[14], "1.05 prints lighter than 0.98: {} and {}", got[15], got[14]);
    }

    /// The table above 1. Up to 1 a slot is the 16-bit code it always was;
    /// above, the table follows the paper to within a level of the screen,
    /// never falling, past its end by the paper itself. A highlight above
    /// the clip burnt in by `l^γₙ` shows where the clip shows, and a look,
    /// which has no negative, stays flat above 1.
    #[test]
    fn the_table_goes_on_above_one() {
        use mimizan_core::paper::NEGATIVE_GAMMA;
        for i in 0..=4000 {
            let x = -0.1 + 1.1 * i as f64 / 4000.0;
            assert_eq!(slot(x), Some(q16(x)), "{x}");
        }
        assert_eq!((slot(1.0), slot(f64::NAN)), (Some(65535), Some(0)));
        assert_eq!(slot(TABLE_TOP), Some(TABLE_SLOTS - 1));
        assert_eq!(slot(TABLE_TOP + 1e-9), None);
        for i in [0, 1, 40_000, 65_535, 65_536, 70_000, TABLE_SLOTS - 1] {
            assert_eq!(slot(slot_value(i)), Some(i));
        }
        let tone = Tone::Paper(mimizan_core::paper::SplitGrade::new(0.6, -0.4)).compile();
        let table = ToneTable::build(&tone);
        assert_eq!(table.show.len(), TABLE_SLOTS);
        let face = |t: f64| ToneTable::faces_on(t, Screen::PRINT).1;
        let mut last = 0u8;
        for i in 0..=2400 {
            let x = 0.9 + 11.1 * i as f64 / 2400.0;
            let shown = table.show_of(x);
            let d = (i32::from(shown) - i32::from(face(tone.eval(x)))).abs();
            assert!(d <= 1, "at {x} the table is {d} levels from the paper");
            assert!(shown >= last, "the table falls at {x}");
            last = shown;
            if x > TABLE_TOP {
                assert_eq!(table.faces_of(x), ToneTable::faces_on(tone.eval(x), Screen::PRINT));
            }
        }
        assert!(table.show_of(1.34) > table.show_of(1.0) + 5);
        // The burn that brings a highlight back to the clip's tone.
        let mut timed = Timed::new(&table);
        for l in [1.1f64, 1.34, 2.0, 3.5] {
            let t = l.powf(NEGATIVE_GAMMA);
            let both = Times { soft: t, hard: t, all: t };
            let at_clip = i32::from(table.show_of(1.0));
            assert!((i32::from(timed.show(l, both)) - at_clip).abs() <= 1, "{l}");
            assert!((i32::from(timed.show_fresh(l, both)) - at_clip).abs() <= 1, "{l}");
            assert!((i32::from(timed.faces(l, both).1) - at_clip).abs() <= 1, "{l}");
            // One light alone is the paper solved at the pixel.
            let soft = Times { soft: t, hard: 1.0, all: t };
            let d = i32::from(timed.show(l, soft)) - i32::from(face(tone.eval_timed(l, soft)));
            assert!(d.abs() <= 1, "{l} under the soft light alone");
            assert!(timed.show(l, soft) < table.show_of(l), "{l}: more soft light is darker");
        }
        let look = ToneTable::build(&Tone::Look(LookFile::neutral()).compile());
        assert_eq!(look.show_of(1.6), look.show_of(1.0));
        assert_eq!(look.faces_of(40.0), look.faces_of(1.0));
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
        let (n1, v1) = Histogram::of_separation(&sep, &w, 1.0, &table, 0.4, None, None);
        let (n2, v2) = Histogram::of_plane(&lum, 1.0, &table, 0.4, None, None);
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
        let (negative, width, height) = kept_negative(neg);
        let session = Session {
            id: next_session_id(),
            path: "mono.dng".into(),
            frame: Arc::new(frame),
            negative,
            width,
            height,
            camera_file: None,
            camera_file_path: None,
            develop: Develop::default(),
            preview,
            raw_hist,
            note: None,
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

    /// A linear DNG opens as a colour file without a mosaic: the window
    /// has a separation to mix, the weights of one channel show that
    /// channel, and the raw histogram counts three channels a pixel.
    #[test]
    fn a_three_channel_file_is_a_colour_file() {
        use mimizan_core::decode::{RawData, Rect, SensorKind};
        use mimizan_core::pipeline::{self, NegativeParams};
        use std::sync::Arc;
        let (w, h) = (8usize, 6usize);
        let px = |i: usize| -> [u16; 3] {
            if i == 9 {
                [200, 1000, 300]
            } else {
                [100 + 20 * (i % w) as u16, 400, 700 - 50 * (i / w) as u16]
            }
        };
        let raw: Vec<u16> = (0..w * h).flat_map(px).collect();
        let frame = RawFrame {
            make: "Fujifilm".into(),
            model: "X-Pro3".into(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: w,
            height: h,
            bits: 16,
            kind: SensorKind::Rgb,
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
        assert!(neg.separation.is_some());
        assert_eq!(neg.info.phase, None);
        let preview = preview_planes(&neg).unwrap();
        let raw_hist = RawHistogram::of_frame(&frame);
        let (negative, width, height) = kept_negative(neg);
        let session = Session {
            id: next_session_id(),
            path: "fuji.dng".into(),
            frame: Arc::new(frame),
            negative,
            width,
            height,
            camera_file: None,
            camera_file_path: None,
            develop: Develop::default(),
            preview,
            raw_hist,
            note: None,
        };
        assert!(session.is_channels() && !session.is_mono());
        let hist = &session.raw_hist;
        assert!(hist.channels && !hist.mono);
        assert_eq!(hist.colors[..3], [Color::R, Color::G, Color::B]);
        assert_eq!(hist.total, [w * h, w * h, w * h, 0]);
        assert_eq!(hist.clipped, [0, 1, 0, 0], "one green at white");

        let tone = Tone::paper().compile();
        let table = ToneTable::build(&tone);
        let red = Weights { r: 1.0, g: 0.0, b: 0.0 };
        let view = ViewParams {
            weights: red,
            tone: Tone::paper(),
            place: 0.0,
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
        let mut channel = Plane::from_vec(w, h, (0..w * h).map(|i| px(i)[0] as f64 / 1000.0).collect());
        // The burnt-out green is held to the ceiling in the colour and
        // not in the luminance: half of what it stands above (1 − 0.98)
        // shows in red.
        channel.data[9] += 0.01;
        assert_eq!(detail.gray, display_bytes(&channel, 0.0, &table));
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
        let (negative, width, height) = kept_negative(neg);
        let session = Session {
            id: next_session_id(),
            path: "mono.dng".into(),
            frame: Arc::new(frame),
            negative,
            width,
            height,
            camera_file: None,
            camera_file_path: None,
            develop: Develop::default(),
            preview,
            raw_hist,
            note: None,
        };
        let tone = Tone::paper().compile();
        let table = ToneTable::build(&tone);
        let plain = ViewParams {
            weights: Weights::default(),
            tone: Tone::paper(),
            place: 0.0,
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
        let (codes, range) = mix_codes(&sep, &wts);
        assert_eq!(range, 1.0);
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
            let (n1, v1) = Histogram::of_separation(&sep, &wts, 1.0, &table, 0.0, window, field);
            let (n2, v2) = Histogram::of_tally(&tally, &codes, 1.0, w, window, &table, 0.0, field);
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
            let (n1, v1) = Histogram::of_separation(&sep, &wts, 1.0, &table, place, None, Some(&field));
            let (n2, v2) = Histogram::of_tally(&tally, &codes, 1.0, w, None, &table, place, Some(&field));
            assert_eq!(n1, n2);
            assert_eq!(v1.total, v2.total);
            let moved: u32 = v1.bins.iter().zip(&v2.bins).map(|(a, b)| a.abs_diff(*b)).sum();
            assert!(moved <= 2 * 4, "{moved} pixels moved at place {place}");
        }
    }

    /// A burn over more pixels than the budget reads the stroke's times
    /// on a lattice, and still counts every pixel with its own code: where
    /// the times are even, the histogram is the one the pixel-by-pixel
    /// walk gives. Codes that seldom repeat are the case that went wrong
    /// when a sample stood for its neighbours' codes as well: about one
    /// burned pixel in eight stayed at its unburned tone.
    #[test]
    fn a_sampled_burn_counts_every_pixel_at_its_own_code() {
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
        let hand = |feather: f64, r: f64| Stroke::Hand {
            tip: Tip { rx: r, ry: r, angle: 0.0, feather },
            stops: 1.0,
            light: Light::Both,
            points: vec![[0.0, 0.0]],
        };
        let tally = Tally::of(&codes, w, None);
        let (plain_neg, plain) = Histogram::of_tally(&tally, &codes, 1.0, w, None, &table, 0.0, None);
        let sum = |h: &Histogram| h.bins.iter().map(|&n| n as usize).sum::<usize>();
        assert_eq!((sum(&plain), plain.total), (w * h, w * h));

        // One time over every pixel: the lattice loses nothing.
        let stroke = hand(0.0, 0.9);
        let field = Field::new(std::slice::from_ref(&stroke), burn::normalize(w, h)).unwrap();
        let (neg, view) = Histogram::of_tally(&tally, &codes, 1.0, w, None, &table, 0.0, Some(&field));
        let (walk_neg, walk) = Histogram::of_codes(&codes, 1.0, w, &table, 0.0, None, Some(&field));
        assert_eq!(neg, plain_neg);
        assert_eq!(neg, walk_neg);
        assert_ne!(view.bins, plain.bins);
        assert_eq!(view, walk);

        // A soft tip inside the picture: only the penumbra and the outline
        // are a lattice step coarse, a small share of the burned pixels.
        let stroke = hand(0.1, 0.3);
        let field = Field::new(std::slice::from_ref(&stroke), burn::normalize(w, h)).unwrap();
        let (_, view) = Histogram::of_tally(&tally, &codes, 1.0, w, None, &table, 0.0, Some(&field));
        let (_, walk) = Histogram::of_codes(&codes, 1.0, w, &table, 0.0, None, Some(&field));
        assert_eq!((sum(&view), view.total), (w * h, w * h));
        let moved = |a: &Histogram, b: &Histogram| {
            a.bins.iter().zip(&b.bins).map(|(&x, &y)| x.abs_diff(y) as usize).sum::<usize>() / 2
        };
        let burned = moved(&walk, &plain);
        let off = moved(&view, &walk);
        assert!(burned > 50_000, "the stroke moves {burned} pixels");
        assert!(off * 100 < burned, "{off} of {burned} burned pixels are in another bin than the walk's");
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
        let (negative, width, height) = kept_negative(neg);
        let session = Session {
            id: next_session_id(),
            path: "bayer.dng".into(),
            frame: Arc::new(frame),
            negative,
            width,
            height,
            camera_file: None,
            camera_file_path: None,
            develop: Develop::default(),
            preview,
            raw_hist,
            note: None,
        };
        let base = ViewParams {
            weights: Weights::default(),
            tone: Tone::paper(),
            place: 0.0,
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
        let (negative, width, height) = kept_negative(neg);
        let session = Session {
            id: next_session_id(),
            path: "grey.nef".into(),
            frame: Arc::new(frame),
            negative,
            width,
            height,
            camera_file: None,
            camera_file_path: None,
            develop: Develop::default(),
            preview,
            raw_hist,
            note: None,
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

    /// Contract 2 under the brush: the fitted print's window on the screen
    /// is the picture the export writes. A Bayer negative, a weight, a
    /// placing, the paper off its base, a straighten, a window, an
    /// enlargement, a soft hand stroke and a card, upright and through the
    /// 90° turns; every pixel of the picture is compared with the 8-bit
    /// face of the file's 16-bit code. The file is made from the recipe
    /// as the edit holds it, the turns a number the print carries out;
    /// the screen from the view's turn and the recipe on the turned
    /// picture, as the stage asks for it.
    /// The screen reads the paper at the 16-bit code of the placed value,
    /// the file at the value itself, so a pixel on the edge between two
    /// screen levels may show the neighbouring one: one level, and rarely.
    #[test]
    fn the_print_on_screen_with_a_brush_is_the_files_picture() {
        use mimizan_core::burn::{Light, Tip};
        use mimizan_core::cfa::BayerPhase;
        use mimizan_core::decode::{RawData, Rect};
        use mimizan_core::pipeline;
        use mimizan_core::print::{render_planes, PrintSize, PrintSource};
        let (w, h) = (96usize, 64usize);
        let raw: Vec<u16> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                (150.0 + 550.0 * (x as f64 / w as f64) + 90.0 * ((y / 5) % 3) as f64) as u16
            })
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
        let (negative, width, height) = kept_negative(neg);
        let session = Session {
            id: next_session_id(),
            path: "bayer.dng".into(),
            frame: Arc::new(frame),
            negative,
            width,
            height,
            camera_file: None,
            camera_file_path: None,
            develop: Develop::default(),
            preview,
            raw_hist,
            note: None,
        };
        let burns = vec![
            Stroke::Hand {
                tip: Tip { rx: 0.12, ry: 0.06, angle: 30.0, feather: 0.05 },
                stops: 1.25,
                light: Light::Both,
                points: vec![[-0.3, -0.1], [0.0, 0.05], [0.3, -0.05]],
            },
            Stroke::Card {
                x: 0.1,
                y: 0.0,
                angle: 20.0,
                travel: 0.1,
                feather: 0.05,
                stops: -0.75,
                light: Light::Soft,
            },
        ];
        let weights = Weights { r: 0.5, g: 0.3, b: 0.2 };
        let framings = [(0.0, None), (2.0, Some(CropRect { x: 6, y: 4, w: 72, h: 48 }))];
        for (quarter_turns, (angle, crop)) in
            [0u8, 1, 2, 3].into_iter().flat_map(|q| framings.into_iter().map(move |f| (q, f)))
        {
            let recipe = Recipe {
                quarter_turns,
                angle,
                crop,
                size: Some(PrintSize::Physical { width_in: 0.6, height_in: 0.4 }),
                dpi: 300.0,
                place: 0.3,
                curve: Tone::Paper(mimizan_core::paper::SplitGrade::new(0.4, -0.3)),
                burns: burns.clone(),
                ..Recipe::on_paper()
            };
            recipe.validate().unwrap();
            let view = ViewParams {
                weights,
                tone: recipe.curve.clone(),
                place: recipe.place,
                rotation: quarter_turns,
                angle,
                crop,
                burns: burns.clone(),
                burn_live: false,
                plate: None,
            };
            // The file, as the export worker makes it.
            let mixed = mix(session.separation().unwrap(), &weights);
            let src = PrintSource {
                plane: &mixed,
                mask: None,
                orientation: session.negative.orientation,
                name: "t".into(),
                description: None,
            };
            let file = render_planes(&src, &PrintParams { recipe: recipe.clone(), ..PrintParams::default() });
            assert_eq!(file.info.recipe, recipe);
            // The screen: the whole picture as one window of the recipe
            // on the turned picture.
            let (turned, on_turned) = recipe.with_turns_applied(session.negative.orientation, w, h);
            assert_eq!(turned, rotated(session.negative.orientation, quarter_turns));
            let print = PrintParams { recipe: on_turned.clone(), ..PrintParams::default() };
            let lay = layout(w, h, turned, &print);
            assert!(lay.enlargement > 1.0);
            assert_eq!((file.plane.width, file.plane.height), (lay.out_w, lay.out_h));
            let table = ToneTable::build(&view.tone.compile());
            let rect = DetailRect { x: 0, y: 0, w: lay.image_w, h: lay.image_h };
            let recipe = on_turned;
            let shown = render_print_detail(&session, &view, &recipe, rect, &table).unwrap();
            assert_eq!((shown.ask.w, shown.ask.h), (lay.image_w, lay.image_h));
            let mut off_by_one = 0usize;
            for y in 0..lay.image_h {
                for x in 0..lay.image_w {
                    let code = q16(file.plane.at(lay.picture_x + x, lay.picture_y + y));
                    let face = ((code as f64 / 65535.0) * 255.0).round() as i32;
                    let d = (i32::from(shown.gray[y * lay.image_w + x]) - face).abs();
                    assert!(d <= 1, "{quarter_turns} turns, angle {angle} at {x},{y}: the screen is {d} levels from the file");
                    off_by_one += (d == 1) as usize;
                }
            }
            let pixels = lay.image_w * lay.image_h;
            assert!(
                off_by_one * 1000 <= pixels,
                "{quarter_turns} turns, angle {angle}: {off_by_one} of {pixels} a level off"
            );
            // The strokes are in that picture: without them it is another one.
            let bare = ViewParams { burns: Vec::new(), ..view.clone() };
            let bare_recipe = Recipe { burns: Vec::new(), ..recipe.clone() };
            let unburned = render_print_detail(&session, &bare, &bare_recipe, rect, &table).unwrap();
            let touched = shown.gray.iter().zip(&unburned.gray).filter(|(a, b)| a != b).count();
            assert!(
                touched * 4 > pixels,
                "{quarter_turns} turns, angle {angle}: the strokes reach {touched} of {pixels} pixels"
            );
        }
    }

    /// What stands above 1 is on the screen as it is in the files. A Bayer
    /// negative with a burnt-out square, balanced so that the square reads
    /// well above the level at which green is full. The window's codes are
    /// the samples of the negative's TIFF, over the same range; the fitted
    /// print on the screen is the export's picture, at the base print and
    /// with the square burnt in by a stop; the square is lighter than the
    /// clip's tone and comes down with the burn; and the print histogram
    /// counts the tones of that picture, none of them piled at the clip.
    /// Before, the window held the negative in 16 bits up to 1: the square
    /// was one flat tone on the screen whatever the file printed.
    #[test]
    fn a_highlight_above_one_on_screen_is_the_files_picture() {
        use mimizan_core::burn::{Light, Tip};
        use mimizan_core::cfa::Color;
        use mimizan_core::decode::{RawData, Rect};
        use mimizan_core::icc::GrayTrc;
        use mimizan_core::ingest::WhiteBalance;
        use mimizan_core::pipeline;
        use mimizan_core::print::{render_planes, PrintSize, PrintSource};
        use mimizan_core::tiffin::{described_range, read_gray, read_negative};
        use mimizan_core::tiffout::{write_gray16_range, TiffMeta};
        let (w, h) = (96usize, 64usize);
        let phase = BayerPhase::RGGB;
        let burnt = |x: usize, y: usize| (52..84).contains(&x) && (16..48).contains(&y);
        // A neutral ramp under the balance (2, 1, 1.5), and the square at
        // the sensor's white in every colour.
        let raw: Vec<u16> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if burnt(x, y) {
                    return 1000;
                }
                let grey = 120.0 + 480.0 * (x as f64 / w as f64);
                let part = match phase.color_at(x, y) {
                    Color::R => 0.5,
                    Color::G => 1.0,
                    Color::B => 2.0 / 3.0,
                };
                (grey * part) as u16
            })
            .collect();
        let frame = RawFrame {
            make: "Test".into(),
            model: "Bayer".into(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: w,
            height: h,
            bits: 14,
            kind: SensorKind::Bayer(phase),
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
        let mut params = NegativeParams { keep_separation: true, ..NegativeParams::default() };
        params.ingest.wb = WhiteBalance::Manual([2.0, 1.0, 1.5]);
        let neg = pipeline::negative_from_frame(&frame, &params, 0).unwrap();
        let preview = preview_planes(&neg).unwrap();
        let raw_hist = RawHistogram::of_frame(&frame);
        let (negative, width, height) = kept_negative(neg);
        let session = Session {
            id: next_session_id(),
            path: "burnt.dng".into(),
            frame: Arc::new(frame),
            negative,
            width,
            height,
            camera_file: None,
            camera_file_path: None,
            develop: Develop::default(),
            preview,
            raw_hist,
            note: None,
        };
        let weights = Weights::default();
        let mixed = mix(session.separation().unwrap(), &weights);
        let range = pipeline::scene_range(&mixed);
        assert!((mixed.at(68, 32) - 1.375).abs() < 1e-9, "the square reads {}", mixed.at(68, 32));
        assert!(range >= 1.375, "{range}");
        assert_eq!(range, session.negative.info.range);

        // The window's codes are the negative's file.
        let dir = std::env::temp_dir().join(format!("mimizan-gui-range-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("negative.tif");
        let description = serde_json::to_string(&session.negative.info).unwrap();
        let meta = TiffMeta { description: &description, trc: GrayTrc::Linear, orientation: 1, dpi: None };
        write_gray16_range(&path, &mixed, range, &meta).unwrap();
        let file = read_gray(&path).unwrap();
        assert_eq!(described_range(file.description.as_deref()).unwrap(), range);
        let samples: Vec<u16> = file.plane.data.iter().map(|&v| (v * 65535.0).round() as u16).collect();
        let mut cache = HistCache::default();
        assert_eq!(cache.codes_for(&session, &weights), &samples[..]);
        assert_eq!(cache.range, range);
        let scene = read_negative(&path).unwrap().plane;
        std::fs::remove_dir_all(&dir).ok();

        let tone = Tone::paper();
        let compiled = tone.compile();
        let table = ToneTable::build(&compiled);
        let face = |t: f64| ((q16(t) as f64 / 65535.0) * 255.0).round() as i32;
        let clip = face(compiled.eval(1.0));
        let burn = Stroke::Hand {
            tip: Tip { rx: 0.25, ry: 0.35, angle: 0.0, feather: 0.05 },
            stops: 1.0,
            light: Light::Both,
            points: vec![[0.2, 0.0]],
        };
        let mut lightest = Vec::new();
        for burns in [Vec::new(), vec![burn]] {
            let recipe = Recipe {
                size: Some(PrintSize::Physical { width_in: 0.6, height_in: 0.4 }),
                dpi: 300.0,
                curve: tone.clone(),
                burns: burns.clone(),
                ..Recipe::on_paper()
            };
            recipe.validate().unwrap();
            let view = ViewParams {
                weights,
                tone: tone.clone(),
                place: 0.0,
                rotation: 0,
                angle: 0.0,
                crop: None,
                burns: burns.clone(),
                burn_live: false,
                plate: None,
            };
            let print = PrintParams { recipe: recipe.clone(), ..PrintParams::default() };
            let lay = layout(w, h, 1, &print);
            let pixels = lay.image_w * lay.image_h;
            let rect = DetailRect { x: 0, y: 0, w: lay.image_w, h: lay.image_h };
            let shown = render_print_detail(&session, &view, &recipe, rect, &table).unwrap();
            // The export from the window's plane, and the print of the
            // negative's file: one picture, and the screen shows it.
            for (name, plane) in [("the window", &mixed), ("the file", &scene)] {
                let src =
                    PrintSource { plane, mask: None, orientation: 1, name: "t".into(), description: None };
                let out = render_planes(&src, &print);
                let mut off_by_one = 0usize;
                for y in 0..lay.image_h {
                    for x in 0..lay.image_w {
                        let d = i32::from(shown.gray[y * lay.image_w + x])
                            - face(out.plane.at(lay.picture_x + x, lay.picture_y + y));
                        assert!(d.abs() <= 1, "{name}, at {x},{y}: the screen is {d} levels off");
                        off_by_one += (d != 0) as usize;
                    }
                }
                assert!(off_by_one * 100 <= pixels, "{name}: {off_by_one} of {pixels} a level off");
            }
            let top = shown.gray.iter().copied().max().unwrap();
            lightest.push(i32::from(top));

            // The print histogram is that picture's, counted on the sensor.
            let (neg_hist, print_hist) = cache.histograms(&session, &view, &table);
            assert_eq!((neg_hist.total, print_hist.total), (w * h, w * h));
            assert_eq!(neg_hist.high, samples.iter().filter(|&&q| q == 65535).count());
            assert!(neg_hist.high >= 1 && neg_hist.high < 8, "{} pixels at the file's top", neg_hist.high);
            let field = hist_field(&session, &view);
            let mut expect = [0u32; 256];
            for y in 0..h {
                let line = FieldRow::new(field.as_ref(), y, 0, w);
                for x in 0..w {
                    let v = compiled.eval_timed(mixed.at(x, y), line.at(x));
                    expect[(v.clamp(0.0, 1.0) * 255.0).round() as usize] += 1;
                }
            }
            let moved: u32 = print_hist.bins.iter().zip(&expect).map(|(a, b)| a.abs_diff(*b)).sum();
            assert!(moved as usize * 100 <= 2 * w * h, "{moved} of {} pixels in another bin", w * h);
            assert_eq!(print_hist.high, 0);
        }
        // The square is lighter than the clip's tone at the base print, and
        // a stop of burning brings it down below where it was.
        let square = (face(compiled.eval(1.375)), clip);
        assert!(square.0 > square.1 + 8, "1.375 prints as {} and the clip as {}", square.0, square.1);
        assert!(lightest[0] >= square.0, "the lightest tone on screen is {}", lightest[0]);
        assert!(lightest[1] < lightest[0] - 20, "burnt in: {} from {}", lightest[1], lightest[0]);

        // The plate's positives go through the same table, each by its lamp.
        let planes = [mixed.clone(), mixed.clone(), mixed.clone()];
        let rgb = display_rgb_with(&planes, 0.0, [0.0, 0.25, -0.25], &table, None);
        for (c, lamp) in [0.0, 0.25, -0.25].into_iter().enumerate() {
            let grey = display_bytes(&mixed, lamp, &table);
            assert!(rgb.iter().skip(c).step_by(3).eq(grey.iter()), "channel {c}");
        }
        let at = (32 * w + 68) * 3;
        assert!(rgb[at + 1] > rgb[at] && rgb[at] > rgb[at + 2], "{:?}", &rgb[at..at + 3]);
    }

    /// A Bayer session of a coloured ramp, dark at the left: the three
    /// plates differ, and the left third lies in the print's deep shadows.
    fn colour_session() -> Session {
        use mimizan_core::cfa::BayerPhase;
        use mimizan_core::decode::{RawData, Rect};
        use mimizan_core::pipeline;
        let (w, h) = (96usize, 64usize);
        let raw: Vec<u16> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                // RGGB: red, the two greens, blue, each with its own share.
                let share = [[0.9, 0.6], [0.6, 0.35]][y % 2][x % 2];
                let level = 30.0 + 900.0 * (x as f64 / w as f64).powi(2) + 40.0 * ((y / 6) % 3) as f64;
                (level * share) as u16
            })
            .collect();
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
        let (negative, width, height) = kept_negative(neg);
        Session {
            id: next_session_id(),
            path: "colour.nef".into(),
            frame: Arc::new(frame),
            negative,
            width,
            height,
            camera_file: None,
            camera_file_path: None,
            develop: Develop::default(),
            preview,
            raw_hist,
            note: None,
        }
    }

    #[test]
    fn a_colour_session_lets_the_full_resolution_mix_go() {
        let session = colour_session();
        assert!(session.negative.separation.is_some());
        assert_eq!(session.negative.plane.width, 0);
        assert_eq!(session.negative.plane.height, 0);
        assert!(session.negative.plane.data.is_empty());
        assert!(session.width > 0 && session.height > 0);
    }

    /// The screen of a print is the sample its file holds. The grey sheet
    /// and a plate in the Adobe space hold the print's values, and the
    /// table is what it was; a plate in sRGB holds the sRGB encoding of
    /// the light `v^2.2`, and the table shows that, by the table, past its
    /// end and under a stroke's own times, while the histogram's bin and
    /// the clip flag stay the print value's.
    #[test]
    fn the_screen_shows_the_files_sample_and_the_histogram_counts_the_print() {
        use mimizan_core::calib::LookEncoding;
        use mimizan_core::kodak::srgb_encode;
        use mimizan_core::paper::SplitGrade;
        let srgb = PlateView { weights: PlateWeights::channels(), space: PlateSpace::Srgb, lamps: [0.0; 3] };
        let adobe = PlateView { space: PlateSpace::AdobeRgb, ..srgb };
        let paper = Tone::Paper(SplitGrade::new(0.6, -0.4));
        // Which screen a print has.
        assert_eq!(Screen::of(&paper, None), Screen::PRINT);
        assert!(Screen::PRINT.is_print());
        assert!(Screen::of(&paper, Some(&adobe)).is_print());
        let screen = Screen::of(&paper, Some(&srgb));
        assert!(!screen.is_print());
        // A look without an encoding is linear in every file.
        let linear = Tone::Look(LookFile { encoding: LookEncoding::None, ..LookFile::neutral() });
        assert!(Screen::of(&linear, Some(&srgb)).is_print());
        // A look on the gamma is written as the paper is.
        assert_eq!(Screen::of(&Tone::Look(LookFile::neutral()), Some(&srgb)), screen);

        // The value: the print's own, or the sRGB encoding of its light.
        let in_file = |v: f64| srgb_encode(v.clamp(0.0, 1.0).powf(2.2));
        for i in 0..=1000 {
            let v = i as f64 / 1000.0;
            assert_eq!(Screen::PRINT.value(v), v);
            assert_eq!(Screen::PRINT.byte(v), (v * 255.0).round() as u8);
            assert_eq!(Screen::of(&paper, Some(&adobe)).value(v), v);
            assert!((screen.value(v) - in_file(v)).abs() < 1e-12, "{v}");
        }
        // Black and white are the ends on either screen; the deep shadows
        // stand lower, the upper half all but where it was.
        assert_eq!((screen.byte(0.0), screen.byte(1.0)), (0, 255));
        assert_eq!(
            (screen.byte(13.0 / 255.0), screen.byte(25.0 / 255.0), screen.byte(51.0 / 255.0)),
            (5, 18, 48)
        );
        for b in 102..=255u8 {
            let d = i32::from(screen.byte(f64::from(b) / 255.0)) - i32::from(b);
            assert!((0..=2).contains(&d), "{b} is shown {d} away");
        }
        assert_eq!((screen.value(-0.5), screen.byte(7.0)), (0.0, 255));

        // The table of the plate in sRGB against the print's.
        let map = paper.compile();
        let print = ToneTable::build(&map);
        let file = ToneTable::build_on(&map, screen);
        assert_eq!(file.view, print.view, "the histogram's bins are the print's");
        assert_eq!(file.clip, print.clip, "the clip marks are the print's");
        assert_eq!(ToneTable::build_on(&map, Screen::of(&paper, Some(&adobe))).show, print.show);
        let face = |t: f64| ((q16(in_file(t)) as f64 / 65535.0) * 255.0).round() as i32;
        let (mut last, mut lower) = (0u8, 0usize);
        for i in 0..=6000 {
            // Up to the clip closely, then on above it and past the table's end.
            let x = if i <= 4000 { i as f64 / 4000.0 } else { 1.0 + 11.0 * (i - 4000) as f64 / 2000.0 };
            let shown = file.show_of(x);
            let d = (i32::from(shown) - face(map.eval(x))).abs();
            assert!(d <= 1, "at {x} the screen is {d} levels from the file");
            assert!(shown >= last, "the screen falls at {x}");
            last = shown;
            let was = print.show_of(x);
            if (8..=60).contains(&was) {
                assert!(shown < was, "at {x} the file's shadow is not under the print's value");
                lower += 1;
            }
            assert_eq!(file.faces_of(x).0, print.faces_of(x).0, "bin at {x}");
            assert_eq!(file.faces_of(x).2, print.faces_of(x).2, "clip at {x}");
        }
        assert!(lower > 100, "{lower} shadow values looked at");

        // Under a stroke: an even time is the table at another value, one
        // light alone is the paper solved at the pixel.
        let even = Times { soft: 2.0, hard: 2.0, all: 2.0 };
        let soft = Times { soft: 2.5, hard: 1.0, all: 2.5 };
        let (mut on_file, mut on_print) = (Timed::new(&file), Timed::new(&print));
        for times in [even, soft] {
            for l in [0.004, 0.01, 0.03, 0.09, 0.18, 0.5, 1.0, 1.4] {
                let want = face(map.eval_timed(l, times));
                assert!((i32::from(on_file.show(l, times)) - want).abs() <= 1, "{l} {times:?}");
                assert!((i32::from(on_file.show_fresh(l, times)) - want).abs() <= 1, "{l} {times:?}");
                let solved = on_file.solve(times);
                assert!(
                    (i32::from(on_file.show_solved(l, times, solved)) - want).abs() <= 1,
                    "{l} {times:?}"
                );
                assert!((i32::from(on_file.faces(l, times).1) - want).abs() <= 1, "{l} {times:?}");
                assert_eq!(on_file.view(l, times), on_print.view(l, times), "{l} {times:?}");
            }
        }
        // A look under uneven times is evaluated exactly, and encoded.
        let look = Tone::Look(LookFile::neutral());
        let look_map = look.compile();
        let look_file = ToneTable::build_on(&look_map, Screen::of(&look, Some(&srgb)));
        let third = Times { soft: 2.0, hard: 1.0, all: 3.0 };
        let mut timed = Timed::new(&look_file);
        for l in [0.01, 0.05, 0.3] {
            let want = face(look_map.eval_timed(l, third));
            assert!((i32::from(timed.show(l, third)) - want).abs() <= 1, "{l}");
        }

        // The worker keeps one table: another screen is another table.
        let mut hold = ToneHold::default();
        assert_eq!(hold.get(&paper, Screen::PRINT).show, print.show);
        assert_eq!(hold.get(&paper, screen).show, file.show);
        assert_ne!(file.show, print.show);
        assert_eq!(hold.get(&paper, Screen::PRINT).show, print.show);
    }

    /// The same plate shown in sRGB and in the Adobe space, with the same
    /// three mixes: the whole picture, the 1:1 window, a coarser grid of
    /// it, the print's window and the print box are, byte for byte, the
    /// print's values in the one and the file's samples of those values
    /// in the other; the histograms are the same in both. With lamps, a
    /// placing, the paper off its base, and under strokes.
    #[test]
    fn a_plate_in_srgb_is_shown_as_its_file_and_counted_as_its_print() {
        use mimizan_core::burn::{Light, Tip};
        use mimizan_core::paper::SplitGrade;
        use mimizan_core::print::PrintSize;
        let session = colour_session();
        let strokes = vec![
            Stroke::Hand {
                tip: Tip { rx: 0.14, ry: 0.07, angle: 20.0, feather: 0.05 },
                stops: 1.0,
                light: Light::Both,
                points: vec![[-0.35, -0.1], [-0.1, 0.05], [0.2, -0.05]],
            },
            Stroke::Card {
                x: -0.2,
                y: 0.0,
                angle: 15.0,
                travel: 0.1,
                feather: 0.05,
                stops: -0.5,
                light: Light::Soft,
            },
        ];
        let tone = Tone::Paper(SplitGrade::new(0.3, -0.2));
        let plate = |space| PlateView { weights: PlateWeights::channels(), space, lamps: [0.25, 0.0, -0.5] };
        let on_file = Screen::of(&tone, Some(&plate(PlateSpace::Srgb)));
        // The file's sample of a byte of the print, to the level.
        let mapped = |b: u8| i32::from(on_file.byte(f64::from(b) / 255.0));
        let mut seen_lower = 0usize;
        let mut same_picture = |what: &str, print: &[u8], file: &[u8]| {
            assert_eq!(print.len(), file.len(), "{what}");
            assert!(!print.is_empty(), "{what}");
            for (i, (&p, &f)) in print.iter().zip(file).enumerate() {
                let d = (i32::from(f) - mapped(p)).abs();
                assert!(
                    d <= 1,
                    "{what} byte {i}: the print's {p} is shown as {f}, {d} from the file's sample"
                );
                seen_lower += (f < p) as usize;
            }
        };
        for burns in [Vec::new(), strokes] {
            let view = |space| ViewParams {
                weights: Weights { r: 0.4, g: 0.4, b: 0.2 },
                tone: tone.clone(),
                place: 0.2,
                rotation: 0,
                angle: 0.0,
                crop: None,
                burns: burns.clone(),
                burn_live: false,
                plate: Some(plate(space)),
            };
            let (adobe, srgb) = (view(PlateSpace::AdobeRgb), view(PlateSpace::Srgb));
            assert!(adobe.screen(&session).is_print());
            assert_eq!(srgb.screen(&session), on_file);
            let print_table = ToneTable::build_on(&tone.compile(), adobe.screen(&session));
            let file_table = ToneTable::build_on(&tone.compile(), srgb.screen(&session));

            // The whole picture and its histograms.
            let fresh = || (HistCache::default(), PreviewStages::default());
            let (mut c, mut st) = fresh();
            let a = render_preview(&session, &adobe, &mut c, &mut st, &print_table);
            let (mut c, mut st) = fresh();
            let b = render_preview(&session, &srgb, &mut c, &mut st, &file_table);
            same_picture("preview", a.rgb.as_ref().unwrap(), b.rgb.as_ref().unwrap());
            same_picture("preview grey", &a.gray, &b.gray);
            assert_eq!(a.hist_rgb, b.hist_rgb, "the channels are counted on the print's values");
            assert_eq!(a.hist_rgb.unwrap(), rgb_histograms(a.rgb.as_ref().unwrap()));
            assert_eq!(a.hist_view, b.hist_view);
            assert_eq!(a.hist_negative, b.hist_negative);
            // The three channels differ, or the test says nothing of colour.
            let px = a.rgb.as_ref().unwrap().as_chunks::<3>().0;
            assert!(px.iter().filter(|p| p[0] != p[1] && p[1] != p[2]).count() * 2 > px.len());

            // The 1:1 window and a coarser grid of it.
            for ask in [
                DetailAsk::pixels(DetailRect { x: 2, y: 3, w: 60, h: 40 }),
                DetailAsk { x: 0, y: 0, w: 40, h: 28, step: 2 },
            ] {
                let a = render_detail(&session, &adobe, ask, &print_table).unwrap();
                let b = render_detail(&session, &srgb, ask, &file_table).unwrap();
                same_picture("window", a.rgb.as_ref().unwrap(), b.rgb.as_ref().unwrap());
                same_picture("window grey", &a.gray, &b.gray);
            }

            // The print's window and the print box.
            let recipe = Recipe {
                size: Some(PrintSize::Physical { width_in: 0.5, height_in: 0.4 }),
                dpi: 300.0,
                place: 0.2,
                curve: tone.clone(),
                burns: burns.clone(),
                ..Recipe::on_paper()
            };
            recipe.validate().unwrap();
            let rect = DetailRect { x: 0, y: 0, w: 120, h: 80 };
            let a = render_print_detail(&session, &adobe, &recipe, rect, &print_table).unwrap();
            let b = render_print_detail(&session, &srgb, &recipe, rect, &file_table).unwrap();
            assert!(a.ask.w > 60, "the print is enlarged: {:?}", a.ask);
            same_picture("print window", a.rgb.as_ref().unwrap(), b.rgb.as_ref().unwrap());
            let o = session.negative.orientation;
            let w = Weights { r: 0.4, g: 0.4, b: 0.2 };
            let a = render_sharpen_loupe(&session, &w, adobe.plate.as_ref(), &recipe, o, 0.2, 0.5, 48);
            let b = render_sharpen_loupe(&session, &w, srgb.plate.as_ref(), &recipe, o, 0.2, 0.5, 48);
            assert_eq!((a.width, a.height), (b.width, b.height));
            same_picture("print box", a.rgb.as_ref().unwrap(), b.rgb.as_ref().unwrap());
            same_picture("print box grey", &a.gray, &b.gray);
            // Without the plate the box is the grey print, as it was.
            let grey = render_sharpen_loupe(&session, &w, None, &recipe, o, 0.2, 0.5, 48);
            assert_eq!(grey.gray, a.gray);
        }
        assert!(seen_lower > 1000, "only {seen_lower} bytes stood lower in the file");
    }

    /// The print's window on the screen is the file the export writes, in
    /// the plate too: a plate in sRGB and one in the Adobe space, with
    /// lamps, a placing, the paper off its base and strokes, upright and
    /// through a turn with a straighten and a crop. The file is written
    /// (`print::write_as`), in 16 bits and in 8, and read back; every
    /// pixel of the picture on the screen is compared with the 8-bit face
    /// of the file's sample, in the three channels.
    #[test]
    fn the_plates_window_on_the_screen_is_the_written_file() {
        use mimizan_core::burn::{Light, Tip};
        use mimizan_core::paper::SplitGrade;
        use mimizan_core::plate::PlateInfo;
        use mimizan_core::print::{render_plate, write_as, FileOptions, PrintSize, PrintSource};
        use mimizan_core::tiffin::read_rgb16;
        let session = colour_session();
        let (w, h) = session.size();
        let sep = session.separation().unwrap();
        let dir = std::env::temp_dir().join(format!("mimizan-gui-screen-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let burns = vec![
            Stroke::Hand {
                tip: Tip { rx: 0.14, ry: 0.07, angle: 20.0, feather: 0.05 },
                stops: 1.0,
                light: Light::Both,
                points: vec![[-0.35, -0.1], [-0.1, 0.05], [0.2, -0.05]],
            },
            Stroke::Card {
                x: -0.2,
                y: 0.0,
                angle: 15.0,
                travel: 0.1,
                feather: 0.05,
                stops: -0.5,
                light: Light::Soft,
            },
        ];
        let weights = Weights { r: 0.4, g: 0.4, b: 0.2 };
        let framings = [(0u8, 0.0, None), (1u8, 2.0, Some(CropRect { x: 4, y: 4, w: 80, h: 52 }))];
        let mut shadows = 0usize;
        for space in [PlateSpace::Srgb, PlateSpace::AdobeRgb] {
            for (quarter_turns, angle, crop) in framings {
                let plate = PlateView { weights: PlateWeights::channels(), space, lamps: [0.25, 0.0, -0.5] };
                let recipe = Recipe {
                    quarter_turns,
                    angle,
                    crop,
                    size: Some(PrintSize::Physical { width_in: 0.6, height_in: 0.4 }),
                    dpi: 300.0,
                    place: 0.2,
                    curve: Tone::Paper(SplitGrade::new(0.3, -0.2)),
                    burns: burns.clone(),
                    ..Recipe::on_paper()
                };
                recipe.validate().unwrap();
                let view = ViewParams {
                    weights,
                    tone: recipe.curve.clone(),
                    place: recipe.place,
                    rotation: quarter_turns,
                    angle,
                    crop,
                    burns: burns.clone(),
                    burn_live: false,
                    plate: Some(plate),
                };
                // The file, as the export worker makes and writes it.
                let planes = mix_plate(sep, &plate.weights);
                let params =
                    PrintParams { recipe: recipe.clone(), lamps: plate.lamps, ..PrintParams::default() };
                let pi = PlateInfo::new(&plate.weights, plate.space, sep.lum.height);
                let [r, g, b] = &planes;
                let sources = [r, g, b].map(|plane| PrintSource {
                    plane,
                    mask: None,
                    orientation: session.negative.orientation,
                    name: "t".into(),
                    description: None,
                });
                let out = render_plate(sources, &pi, &params);
                // The screen: the whole picture as one window of the recipe
                // on the turned picture.
                let (turned, on_turned) = recipe.with_turns_applied(session.negative.orientation, w, h);
                let lay = layout(
                    w,
                    h,
                    turned,
                    &PrintParams { recipe: on_turned.clone(), ..PrintParams::default() },
                );
                assert!(lay.enlargement > 1.0);
                assert_eq!((out.plane.width, out.plane.height), (lay.out_w, lay.out_h));
                let table = ToneTable::build_on(&view.tone.compile(), view.screen(&session));
                let rect = DetailRect { x: 0, y: 0, w: lay.image_w, h: lay.image_h };
                let shown = render_print_detail(&session, &view, &on_turned, rect, &table).unwrap();
                assert_eq!((shown.ask.w, shown.ask.h), (lay.image_w, lay.image_h));
                let rgb = shown.rgb.as_ref().unwrap();
                for eight_bit in [false, true] {
                    let path = dir.join(format!("{space:?}-{quarter_turns}-{eight_bit}.tif"));
                    write_as(&out, &path, &params, FileOptions { eight_bit, rgb: false }).unwrap();
                    let file = read_rgb16(&path).unwrap();
                    let mut off_by_one = 0usize;
                    for y in 0..lay.image_h {
                        for x in 0..lay.image_w {
                            for (c, plane) in file.iter().enumerate() {
                                let face =
                                    (plane.at(lay.picture_x + x, lay.picture_y + y) * 255.0).round() as i32;
                                let on_screen = i32::from(rgb[(y * lay.image_w + x) * 3 + c]);
                                let d = (on_screen - face).abs();
                                assert!(
                                    d <= 1,
                                    "{space:?}, {quarter_turns} turns, 8 bit {eight_bit}, channel {c} at {x},{y}: \
                                     the screen is {d} levels from the file"
                                );
                                off_by_one += (d == 1) as usize;
                                shadows += (space == PlateSpace::Srgb && (4..40).contains(&face)) as usize;
                            }
                        }
                    }
                    // A level off where a rounding falls the other way: the
                    // table reads the placed value at a 16-bit slot, the
                    // file evaluates it. This picture stands deep in the
                    // shadows, where the paper is steepest, so the share is
                    // above the grey sheet's (measured: 0.14 to 0.18 %, in
                    // the Adobe space, whose screen is the print's values,
                    // as in sRGB).
                    let samples = 3 * lay.image_w * lay.image_h;
                    assert!(
                        off_by_one * 250 <= samples,
                        "{space:?}, {quarter_turns} turns, 8 bit {eight_bit}: {off_by_one} of {samples} a level off"
                    );
                }
            }
        }
        // The comparison reached the deep shadows, where the two curves part.
        assert!(shadows > 2000, "only {shadows} shadow samples compared");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
