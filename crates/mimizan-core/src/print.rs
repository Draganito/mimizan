//! Negative -> print file: the one pass.
//! Upright -> crop window -> size -> place -> curve -> black edge ->
//! white border -> [Richardson–Lucy ->] USM -> peak on the picture ->
//! one quantisation ->
//! file. The mix already happened when the negative was written. Everything
//! the pass does is in the [`Recipe`]; [`PrintParams`] adds what is not a
//! print decision (viewing model constant, sidecar use).

use crate::deconv::{richardson_lucy, DECONV_ITERATIONS};
use crate::error::{Error, Result};
use crate::icc::GrayTrc;
use crate::plane::Plane;
use crate::recipe::{CropRect, Recipe, Sharpen};
use crate::resize::{
    enlargement_scale, enlargement_sigma, fit_dims, resize_rect, resize_with, PixelRect, PlaneWindow,
    ResampleKernel,
};
use crate::tiffin::{read_gray, read_mask8};
use crate::tiffout::{write_gray16, TiffMeta};
use crate::usm::{kernel_radius, radius_px, unsharp, USM_K};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{info, warn};

/// Output size: a physical print (a paper box the picture is fitted into).
/// An enum with one variant so the recipe JSON (`{"physical":{...}}`)
/// stays what it was.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrintSize {
    Physical { width_in: f64, height_in: f64 },
}

impl PrintSize {
    /// `"30x40cm"`, `"12x16in"`, `"8.5x11in"`.
    pub fn parse(s: &str) -> Result<Self> {
        let t = s.trim().to_ascii_lowercase();
        let (num, unit) = if let Some(n) = t.strip_suffix("cm") {
            (n, 2.54)
        } else if let Some(n) = t.strip_suffix("mm") {
            (n, 25.4)
        } else if let Some(n) = t.strip_suffix("in") {
            (n, 1.0)
        } else {
            return Err(Error::Invalid(format!("size '{s}' needs a unit: cm, mm or in")));
        };
        let parts: Vec<&str> = num.split('x').collect();
        if parts.len() != 2 {
            return Err(Error::Invalid(format!("size '{s}' must be WxH<unit>")));
        }
        let p =
            |v: &str| v.trim().parse::<f64>().map_err(|_| Error::Invalid(format!("size '{s}': bad number")));
        let (w, h) = (p(parts[0])? / unit, p(parts[1])? / unit);
        let size = Self::Physical { width_in: w, height_in: h };
        size.validate()?;
        Ok(size)
    }

    /// Both sides finite, positive and at most [`SIZE_MAX_IN`] inches.
    pub fn validate(&self) -> Result<()> {
        let Self::Physical { width_in, height_in } = *self;
        let ok = |v: f64| v.is_finite() && v > 0.0 && v <= SIZE_MAX_IN;
        if !(ok(width_in) && ok(height_in)) {
            return Err(Error::Invalid(format!(
                "size {width_in}x{height_in} in must be positive and at most {SIZE_MAX_IN} in a side"
            )));
        }
        Ok(())
    }

    /// Bounding box in pixels.
    pub fn pixels(&self, dpi: f64) -> (usize, usize) {
        let Self::Physical { width_in, height_in } = *self;
        ((width_in * dpi).round().max(1.0) as usize, (height_in * dpi).round().max(1.0) as usize)
    }
}

/// Longest side a print size may have, inches (10 m).
pub const SIZE_MAX_IN: f64 = 400.0;

/// What the print pass needs besides the recipe: the viewing model's
/// constant and distance (recorded in the description, not applied to a
/// pixel) and whether the mask sidecar may restrict the USM.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintParams {
    /// Every print decision.
    pub recipe: Recipe,
    /// Arc-minute constant of the viewing model (`usm.rs`). Only
    /// `usm_radius_px` in the description comes from it.
    pub usm_k: f64,
    /// Viewing distance for that radius, mm. `None` = print diagonal.
    pub viewing_distance_mm: Option<f64>,
    /// Use the `.mask.tif` sidecar to restrict USM.
    pub use_mask: bool,
    /// The three lamps of the projector, for a plate: stops of exposure
    /// added to the red, green and blue positive on top of the recipe's
    /// placing. The only colour control there is; zero for a negative.
    pub lamps: [f64; 3],
}

impl Default for PrintParams {
    fn default() -> Self {
        Self {
            recipe: Recipe::default(),
            usm_k: USM_K,
            viewing_distance_mm: None,
            use_mask: true,
            lamps: [0.0; 3],
        }
    }
}

/// Lamps stay within this many stops of the placing.
pub const LAMP_LIMIT: f64 = 2.0;

impl PrintParams {
    /// USM consults the mask. Deconvolution does not: it runs on the whole
    /// picture, and USM may follow it.
    pub fn sharpens(&self) -> bool {
        self.recipe.sharpen.uses_mask()
    }
}

/// The print's `ImageDescription`: what was printed, from what, and how.
#[derive(Clone, Debug, Serialize)]
pub struct PrintInfo {
    /// Crate version that wrote the file.
    pub mimizan: &'static str,
    /// Always `"print"`.
    pub stage: &'static str,
    /// The negative's file name.
    pub source: String,
    /// The negative's own description, when it parsed as JSON.
    pub source_description: Option<serde_json::Value>,
    /// Name of the tone (paper or look).
    pub look: String,
    /// `"gamma22"` or `"none"`: the TRC of the embedded ICC profile.
    pub look_encoding: String,
    /// Output file size, pixels.
    pub width: usize,
    pub height: usize,
    pub dpi: f64,
    /// TIFF orientation of the negative that was applied; the file is upright.
    pub orientation_applied: u16,
    /// The recipe this pass ran (Zonensystem paper, section 11): place,
    /// curve, kernel, size, border, sharpening mode.
    pub recipe: Recipe,
    /// White border in pixels on each side, when there is one.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub border_px: usize,
    /// Black edge in pixels on each side, inside the white border.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub black_px: usize,
    /// USM amount of the sharpening mode, 0 when none.
    pub usm_amount: f64,
    /// One arc minute at the viewing distance, in print pixels. Recorded
    /// only: sharpening uses [`Self::resample_sigma_px`].
    pub usm_radius_px: f64,
    /// Gaussian σ of the enlargement, in print pixels. Zero at native size,
    /// when shrinking, and for the Area kernel, which adds no blur of its
    /// own. USM and Richardson–Lucy use this.
    pub resample_sigma_px: f64,
    /// A mask sidecar restricted the USM.
    pub usm_masked: bool,
    /// Richardson–Lucy iterations when that path ran; omitted when it did not.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub deconvolution_iterations: u32,
    /// Wall time of the pass, milliseconds.
    pub ms: u128,
    /// Present when the source was a three-colour plate: the filter set it
    /// was taken with, the primaries it is projected through, the lamps.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plate: Option<PlatePrintInfo>,
}

/// The plate part of a print's description.
#[derive(Clone, Debug, Serialize)]
pub struct PlatePrintInfo {
    pub filters: crate::plate::PlateFilters,
    pub space: crate::plate::PlateSpace,
    /// Stops added to the red, green and blue positive.
    pub lamps: [f64; 3],
}

/// The green and blue positives of a plate print; the red one is
/// [`PrintOutput::plane`].
pub struct PlateSheet {
    pub green: Plane,
    pub blue: Plane,
    pub space: crate::plate::PlateSpace,
}

impl PlateSheet {
    /// The three positives as `[red, green, blue]`.
    pub fn rgb<'a>(&'a self, red: &'a Plane) -> [&'a Plane; 3] {
        [red, &self.green, &self.blue]
    }
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}

/// Apply a TIFF orientation (1..=8) to the pixels so the result is upright.
/// Upright (1) and unknown values copy the plane.
pub fn orient(p: &Plane, orientation: u16) -> Plane {
    let (w, h) = (p.width, p.height);
    if !(2..=8).contains(&orientation) || w == 0 || h == 0 {
        return p.clone();
    }
    let swap = matches!(orientation, 5..=8);
    let (ow, oh) = if swap { (h, w) } else { (w, h) };
    let mut out = Plane::zeros(ow, oh);
    out.data.par_chunks_mut(ow).enumerate().for_each(|(oy, row)| {
        for (ox, o) in row.iter_mut().enumerate() {
            // Map output (ox, oy) back to source (sx, sy).
            let (sx, sy) = match orientation {
                2 => (w - 1 - ox, oy),
                3 => (w - 1 - ox, h - 1 - oy),
                4 => (ox, h - 1 - oy),
                5 => (oy, ox),
                6 => (oy, h - 1 - ox),
                7 => (w - 1 - oy, h - 1 - ox),
                8 => (w - 1 - oy, ox),
                _ => (ox, oy),
            };
            *o = p.at(sx, sy);
        }
    });
    out
}

/// Where source pixel (`sx`, `sy`) of a `w` x `h` plane lands after
/// `orient(_, orientation)`: the inverse of the lookup inside `orient`,
/// kept to test it.
#[cfg(test)]
fn upright_pixel(orientation: u16, w: usize, h: usize, sx: usize, sy: usize) -> (usize, usize) {
    match orientation {
        2 => (w - 1 - sx, sy),
        3 => (w - 1 - sx, h - 1 - sy),
        4 => (sx, h - 1 - sy),
        5 => (sy, sx),
        6 => (h - 1 - sy, sx),
        7 => (h - 1 - sy, w - 1 - sx),
        8 => (sy, w - 1 - sx),
        _ => (sx, sy),
    }
}

/// The orientation that equals `orientation` followed by a 90° clockwise
/// turn, i.e. `orient(orient(p, o), 6) == orient(p, rotate_cw(o))`.
/// Unknown (0) is treated as upright.
pub fn rotate_cw(orientation: u16) -> u16 {
    match orientation {
        1 | 0 => 6,
        6 => 3,
        3 => 8,
        8 => 1,
        2 => 7,
        7 => 4,
        4 => 5,
        5 => 2,
        o => o,
    }
}

/// `orientation` followed by `quarter_turns` clockwise 90° turns.
pub fn rotated(orientation: u16, quarter_turns: u8) -> u16 {
    (0..quarter_turns % 4).fold(orientation, |o, _| rotate_cw(o))
}

/// Rotate an 8-bit image 90° clockwise `quarter_turns` times; returns the
/// new (width, height, pixels). Kept to test `rotate_cw` against `orient`.
#[cfg(test)]
fn rotate_gray8_cw(w: usize, h: usize, g: &[u8], quarter_turns: u8) -> (usize, usize, Vec<u8>) {
    assert_eq!(g.len(), w * h);
    let k = quarter_turns % 4;
    if k == 0 {
        return (w, h, g.to_vec());
    }
    let (ow, oh) = if k % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; ow * oh];
    for (oy, row) in out.chunks_mut(ow.max(1)).enumerate() {
        for (ox, o) in row.iter_mut().enumerate() {
            let (sx, sy) = match k {
                1 => (oy, h - 1 - ox),
                2 => (w - 1 - ox, h - 1 - oy),
                _ => (w - 1 - oy, ox),
            };
            *o = g[sy * w + sx];
        }
    }
    (ow, oh, out)
}

/// `<negative stem>.mask.tif` next to the negative.
pub fn sidecar_path(neg: &Path) -> PathBuf {
    let stem = neg.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    neg.with_file_name(format!("{stem}.mask.tif"))
}

/// The rendered sheet and its description.
pub struct PrintOutput {
    /// Print values in [0, 1], not yet quantised. For a plate, the red
    /// positive; the other two are in `plate`.
    pub plane: Plane,
    pub info: PrintInfo,
    /// The green and blue positives when the source was a plate.
    pub plate: Option<PlateSheet>,
}

/// Full print rendering in memory.
pub fn render(neg_path: &Path, p: &PrintParams) -> Result<PrintOutput> {
    render_within(neg_path, p, None)
}

/// `render`, refused before any print plane is allocated when
/// `memory_estimate` exceeds `budget` bytes. The caller knows the machine;
/// this crate does not.
pub fn render_within(neg_path: &Path, p: &PrintParams, budget: Option<u64>) -> Result<PrintOutput> {
    let t = Instant::now();
    let neg = read_gray(neg_path)?;
    let src_desc = neg.description.as_deref().and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok());
    let plate_info = crate::plate::PlateInfo::from_description(src_desc.as_ref());
    // A plate is three frames stacked; the print works on one frame's size.
    let frames = match &plate_info {
        Some(pi) => Some(crate::plate::split(&neg.plane, pi)?),
        None => None,
    };
    let (fw, fh) = match &frames {
        Some(f) => (f[0].width, f[0].height),
        None => (neg.plane.width, neg.plane.height),
    };
    info!(
        "{} {}x{} {} bit, orientation {}",
        if plate_info.is_some() { "plate, each frame" } else { "negative" },
        fw,
        fh,
        neg.bits,
        neg.orientation
    );
    let need = memory_estimate(fw, fh, neg.orientation, p) * if frames.is_some() { 3 } else { 1 };
    info!("print holds about {} at its widest", memory_text(need));
    if let Some(limit) = budget {
        if need > limit {
            return Err(Error::Invalid(format!(
                "the print would hold about {} at once and the machine has {}; a smaller size or dpi",
                memory_text(need),
                memory_text(limit)
            )));
        }
    }

    let mut mask: Option<Plane> = None;
    if p.use_mask && p.sharpens() {
        let sp = sidecar_path(neg_path);
        match read_mask8(&sp) {
            Ok((mw, mh, data)) if mw == fw && mh == fh => {
                let values: Vec<f64> = data.par_iter().map(|&v| f64::from(v) / 255.0).collect();
                match Plane::try_from_vec(mw, mh, values) {
                    Ok(m) => mask = Some(m),
                    Err(e) => warn!("mask sidecar {}: {e}; USM everywhere", sp.display()),
                }
            }
            Ok(_) => warn!("mask sidecar {} has the wrong size; USM everywhere", sp.display()),
            Err(e) => warn!("no mask sidecar {} ({e}); USM everywhere", sp.display()),
        }
    }

    let name = neg_path.display().to_string();
    let orientation = neg.orientation;
    let mut out = match (frames, plate_info) {
        (Some(frames), Some(pi)) => {
            // The stacked plane is not needed any more.
            drop(neg);
            let [r, g, b] = &frames;
            let sources = [r, g, b].map(|plane| PrintSource {
                plane,
                mask: mask.as_ref(),
                orientation,
                name: name.clone(),
                description: src_desc.clone(),
            });
            render_plate(sources, &pi, p)
        }
        _ => {
            let source = PrintSource {
                plane: &neg.plane,
                mask: mask.as_ref(),
                orientation: neg.orientation,
                name,
                description: src_desc,
            };
            render_planes(&source, p)
        }
    };
    out.info.ms = t.elapsed().as_millis();
    Ok(out)
}

/// The three positives of a plate: each frame through the same pass, the
/// same layout, the same paper and the same two exposures, plus its lamp.
/// `sources[i]` are red, green, blue and must agree in size and
/// orientation; `orientation` is read from the first.
pub fn render_plate(
    sources: [PrintSource<'_>; 3],
    pi: &crate::plate::PlateInfo,
    p: &PrintParams,
) -> PrintOutput {
    let t = Instant::now();
    let lamp = |c: usize| {
        let mut q = p.clone();
        q.recipe.place += p.lamps[c];
        q
    };
    let [red, green, blue] = sources;
    let mut r = render_planes(&red, &lamp(0));
    let g = render_planes(&green, &lamp(1));
    let b = render_planes(&blue, &lamp(2));
    if p.lamps.iter().any(|&l| l != 0.0) {
        info!("lamps R {:+.2} G {:+.2} B {:+.2} stops", p.lamps[0], p.lamps[1], p.lamps[2]);
    }
    r.info.plate = Some(PlatePrintInfo { filters: pi.filters, space: pi.space, lamps: p.lamps });
    r.info.ms = t.elapsed().as_millis();
    r.plate = Some(PlateSheet { green: g.plane, blue: b.plane, space: pi.space });
    r
}

/// A negative already in memory (GUI session, or the file just read).
pub struct PrintSource<'a> {
    /// Linear luminance as stored in the negative, sensor orientation.
    pub plane: &'a Plane,
    /// Mask in [0,1], same size as `plane`; `None` = USM everywhere.
    pub mask: Option<&'a Plane>,
    /// TIFF orientation of `plane`, 1..=8.
    pub orientation: u16,
    /// File name for the description.
    pub name: String,
    /// The negative's own description, when it parsed as JSON.
    pub description: Option<serde_json::Value>,
}

/// Where the picture lands: its pixel size, the two rings around it, the sheet
/// (picture plus black plus white), and the viewing model's numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// Picture size after the fit, pixels.
    pub image_w: usize,
    pub image_h: usize,
    /// White border on each side, pixels.
    pub border_px: usize,
    /// Black edge on each side, inside the white, pixels.
    pub black_px: usize,
    /// Output file size: picture plus twice the black edge plus twice the white.
    pub out_w: usize,
    pub out_h: usize,
    /// Viewing distance, mm (given, or the sheet diagonal).
    pub viewing_mm: f64,
    /// Long-side scale, destination / source. Above 1 when the picture is enlarged.
    pub enlargement: f64,
    /// Blur the resampler added, print pixels. Zero unless `enlargement > 1`
    /// with a kernel that blurs (the Area kernel holds pixels and adds none).
    pub resample_sigma_px: f64,
    /// One arc minute at `viewing_mm`, print pixels. Recorded, not applied.
    pub usm_radius_px: f64,
}

/// Paper box, in pixels, whose fitted picture is exactly `picture_w` pixels
/// wide. The height is that same scale, `round(img_h · picture_w / img_w)`,
/// and `border` pixels of paper sit outside the picture on every side.
///
/// The inner height is `ceil`, not the rounded height: `fit_dims` takes the
/// tighter side, and a ceil keeps the width the tighter one, so the rounded
/// height still fits inside the box.
pub fn box_for_picture_width(img_w: usize, img_h: usize, picture_w: usize, border: usize) -> (usize, usize) {
    let picture_w = picture_w.max(1);
    let inner_h = (picture_w as f64 * img_h as f64 / img_w.max(1) as f64).ceil().max(1.0) as usize;
    (picture_w.saturating_add(2 * border), inner_h.saturating_add(2 * border))
}

/// Paper box, in pixels, whose fitted picture is exactly `picture_h` pixels
/// tall. Mirror of [`box_for_picture_width`].
pub fn box_for_picture_height(img_w: usize, img_h: usize, picture_h: usize, border: usize) -> (usize, usize) {
    let picture_h = picture_h.max(1);
    let inner_w = (picture_h as f64 * img_w as f64 / img_h.max(1) as f64).ceil().max(1.0) as usize;
    (inner_w.saturating_add(2 * border), picture_h.saturating_add(2 * border))
}

/// The crop window inside an upright image, or the whole image.
pub fn source_window(full_w: usize, full_h: usize, crop: Option<CropRect>) -> (usize, usize, usize, usize) {
    match crop {
        Some(c) => {
            let c = c.sanitized(full_w as u32, full_h as u32);
            (c.x as usize, c.y as usize, c.w as usize, c.h as usize)
        }
        None => (0, 0, full_w, full_h),
    }
}

/// Shift a full-image window into crop coordinates. The plane itself is
/// unchanged. The window's origin must not lie before the crop.
fn window_frame<'a>(src: PlaneWindow<'a>, ox: usize, oy: usize, sw: usize, sh: usize) -> PlaneWindow<'a> {
    debug_assert!(src.origin_x >= ox && src.origin_y >= oy, "window starts before the crop");
    PlaneWindow {
        plane: src.plane,
        origin_x: src.origin_x.saturating_sub(ox),
        origin_y: src.origin_y.saturating_sub(oy),
        full_w: sw,
        full_h: sh,
    }
}

/// Copy the window out of `p`. `c` is already inside the plane.
pub fn window_plane(p: &Plane, c: CropRect) -> Plane {
    let c = c.sanitized(p.width as u32, p.height as u32);
    let (x, y, w, h) = (c.x as usize, c.y as usize, c.w as usize, c.h as usize);
    let mut out = Plane::zeros(w, h);
    for row in 0..h {
        out.row_mut(row).copy_from_slice(&p.row(y + row)[x..x + w]);
    }
    out
}

/// Layout for a negative of `w×h` (before orientation). Both rings are taken
/// from the paper box before the fit, so picture plus black plus white never
/// exceeds the size asked for, unless the frame alone is wider than the
/// sheet, which leaves one pixel of picture. With no size, the rings are
/// added outside the crop window and the picture pixels stay as they are.
pub fn layout(w: usize, h: usize, orientation: u16, p: &PrintParams) -> Layout {
    let r = &p.recipe;
    let (w, h) = if matches!(orientation, 5..=8) { (h, w) } else { (w, h) };
    // The fit and the enlargement are measured from the crop window.
    let (_, _, w, h) = source_window(w, h, r.crop);
    let white = r.border_px();
    let black = r.black_px();
    let frame = white.saturating_add(black);
    let frame2 = frame.saturating_mul(2);
    let (image_w, image_h) = match r.size {
        Some(sz) => {
            let (bw, bh) = sz.pixels(r.dpi);
            // A frame wider than the paper leaves one pixel of picture.
            fit_dims(w, h, bw.saturating_sub(frame2).max(1), bh.saturating_sub(frame2).max(1))
        }
        None => (w, h),
    };
    let (out_w, out_h) = (image_w.saturating_add(frame2), image_h.saturating_add(frame2));
    let viewing_mm = p.viewing_distance_mm.unwrap_or_else(|| {
        let diag_in = ((out_w as f64 / r.dpi).powi(2) + (out_h as f64 / r.dpi).powi(2)).sqrt();
        diag_in * 25.4
    });
    let enlargement = enlargement_scale(w, h, image_w, image_h);
    let enlargement_px = enlargement_sigma(w, h, image_w, image_h, r.resample);
    Layout {
        image_w,
        image_h,
        border_px: white,
        black_px: black,
        out_w,
        out_h,
        viewing_mm,
        enlargement,
        resample_sigma_px: enlargement_px,
        usm_radius_px: radius_px(p.usm_k, viewing_mm, r.dpi),
    }
}

/// Bytes `render_planes` holds at its widest point for a negative of `w×h`
/// (before orientation): the live f64 planes of the stage that keeps the
/// most, plus the 16-bit copy the writer makes. The source plane the
/// caller already owns is not counted. A guide for refusing an export the
/// machine cannot hold, good to about a fifth; the allocator and the order
/// in which planes drop are what it does not see.
pub fn memory_estimate(w: usize, h: usize, orientation: u16, p: &PrintParams) -> u64 {
    let r = &p.recipe;
    let lay = layout(w, h, orientation, p);
    let (fw, fh) = if matches!(orientation, 5..=8) { (h, w) } else { (w, h) };
    let (_, _, cw, ch) = source_window(fw, fh, r.crop);
    let full = fw as f64 * fh as f64;
    let n = cw as f64 * ch as f64;
    let picture = lay.image_w as f64 * lay.image_h as f64;
    let sheet = lay.out_w as f64 * lay.out_h as f64;
    // The mask walks beside the picture through orient, crop, resize and
    // the frame, doubling those stages.
    let m = if p.sharpens() { 2.0 } else { 1.0 };
    // Orientation copy, then the crop window beside it.
    let orient = m * (full + n);
    // Separable resize: source, the smaller intermediate, destination.
    let mid = (ch as f64 * lay.image_w as f64).min(cw as f64 * lay.image_h as f64);
    let resize =
        if (lay.image_w, lay.image_h) == (cw, ch) { m * (n + picture) } else { m * (n + mid + picture) };
    // The picture and the sheet it is copied into; no rings, no copy.
    let frame = if lay.border_px + lay.black_px > 0 { m * picture + sheet } else { 0.0 };
    // Picture cut out, blur scratch, blur, result; Richardson–Lucy keeps
    // estimate, blurred, ratio and correction as well.
    let peak = if r.peak.applies() { sheet + 4.0 * picture } else { 0.0 };
    let sharpen = if p.sharpens() && lay.resample_sigma_px > 0.0 {
        let planes = if r.sharpen.deconv_passes() > 0 { 7.0 } else { 5.0 };
        sheet + planes * picture
    } else {
        0.0
    };
    let widest = [orient, resize, frame, peak, sharpen].into_iter().fold(0.0, f64::max);
    let bytes = 8.0 * widest + 2.0 * sheet;
    bytes.min(u64::MAX as f64) as u64
}

/// Bytes as a short figure: `640 MB`, `27.3 GB`.
pub fn memory_text(bytes: u64) -> String {
    let gb = bytes as f64 / 1e9;
    if gb >= 1.0 {
        format!("{gb:.1} GB")
    } else {
        format!("{:.0} MB", bytes as f64 / 1e6)
    }
}

/// How far, in print pixels, a sharpened sample can see.
///
/// One Richardson–Lucy iteration widens the dependence by two Gaussian
/// radii (the estimate is blurred, then the ratio is blurred again). USM
/// adds one more radius. The loupe keeps this much real image around the
/// box, so the centre matches the full print.
pub fn sharpen_halo(sigma: f64, passes: usize, usm: bool) -> usize {
    if sigma <= 0.0 || (passes == 0 && !usm) {
        return 0;
    }
    let kr = kernel_radius(sigma);
    let mut halo = 0;
    if passes > 0 {
        halo += 2 * passes * kr;
    }
    if usm {
        halo += kr;
    }
    halo
}

/// Where the loupe sits on the sheet, and which picture pixels have to be
/// resampled so the sharpening around that box sees real neighbours.
struct LoupeGeom {
    /// Box origin on the sheet (picture plus black edge plus white border).
    view_x: usize,
    view_y: usize,
    view_w: usize,
    view_h: usize,
    /// Sheet rectangle the sharpening kernel reads, clamped to the sheet.
    tile_x: usize,
    tile_y: usize,
    tile_w: usize,
    tile_h: usize,
    /// Picture pixels inside the tile, in picture coordinates. Empty when
    /// the tile is only border.
    pic_x: usize,
    pic_y: usize,
    pic_w: usize,
    pic_h: usize,
    border: usize,
    image_w: usize,
    image_h: usize,
}

/// Radius in print pixels. The stored radius is in negative pixels, and the
/// print multiplies it by the long-side scale `destination / source`, above
/// 1 and below. At native size the factor is 1. The amount is never scaled.
pub fn peak_print_radius(radius: f64, scale: f64) -> f64 {
    radius * scale
}

fn peak_halo_px(recipe: &Recipe, enlargement: f64) -> usize {
    if recipe.peak.applies() {
        kernel_radius(peak_print_radius(recipe.peak.radius, enlargement))
    } else {
        0
    }
}

fn loupe_geom(lay: &Layout, sharpen: Sharpen, nx: f64, ny: f64, view: usize, peak_halo: usize) -> LoupeGeom {
    let (image_w, image_h) = (lay.image_w, lay.image_h);
    // Picture inset: white on the outside, black between that and the picture.
    let border = lay.border_px + lay.black_px;
    let (sheet_w, sheet_h) = (lay.out_w, lay.out_h);
    let vw = view.min(sheet_w).max(1);
    let vh = view.min(sheet_h).max(1);
    let pick_x = (nx.clamp(0.0, 1.0) * image_w.saturating_sub(1) as f64).round() as isize + border as isize;
    let pick_y = (ny.clamp(0.0, 1.0) * image_h.saturating_sub(1) as f64).round() as isize + border as isize;
    let mut left = pick_x - vw as isize / 2;
    let mut top = pick_y - vh as isize / 2;
    left = left.clamp(0, sheet_w as isize - vw as isize);
    top = top.clamp(0, sheet_h as isize - vh as isize);
    let passes = sharpen.deconv_passes() as usize;
    let usm = sharpen.usm_amount() > 0.0;
    let from_sharpen = if lay.resample_sigma_px > 0.0 && (passes > 0 || usm) {
        sharpen_halo(lay.resample_sigma_px, passes, usm)
    } else {
        0
    };
    let halo = from_sharpen.saturating_add(peak_halo) as isize;
    let tile_x = (left - halo).max(0) as usize;
    let tile_y = (top - halo).max(0) as usize;
    let tile_r = (left + vw as isize + halo).min(sheet_w as isize) as usize;
    let tile_b = (top + vh as isize + halo).min(sheet_h as isize) as usize;
    let pic_x0 = border;
    let pic_y0 = border;
    let pic_x1 = border + image_w;
    let pic_y1 = border + image_h;
    let ix0 = tile_x.max(pic_x0);
    let iy0 = tile_y.max(pic_y0);
    let ix1 = tile_r.min(pic_x1);
    let iy1 = tile_b.min(pic_y1);
    let (pic_x, pic_y, pic_w, pic_h) = if ix0 < ix1 && iy0 < iy1 {
        (ix0 - border, iy0 - border, ix1 - ix0, iy1 - iy0)
    } else {
        (0, 0, 0, 0)
    };
    LoupeGeom {
        view_x: left as usize,
        view_y: top as usize,
        view_w: vw,
        view_h: vh,
        tile_x,
        tile_y,
        tile_w: tile_r - tile_x,
        tile_h: tile_b - tile_y,
        pic_x,
        pic_y,
        pic_w,
        pic_h,
        border,
        image_w,
        image_h,
    }
}

/// Upright source pixels the loupe resamples. When USM is on, the mask is
/// resampled with [`MASK_KERNEL`] and that span is united with the picture
/// kernel's, so one crop feeds both. `(0, 0, 0, 0)` when the box lies
/// entirely on the border.
pub fn loupe_source_rect(
    full_w: usize,
    full_h: usize,
    p: &PrintParams,
    nx: f64,
    ny: f64,
    view: usize,
) -> (usize, usize, usize, usize) {
    let (ox, oy, sw, sh) = source_window(full_w, full_h, p.recipe.crop);
    let lay = layout(full_w, full_h, 1, p);
    let g = loupe_geom(&lay, p.recipe.sharpen, nx, ny, view, peak_halo_px(&p.recipe, lay.enlargement));
    if g.pic_w == 0 || g.pic_h == 0 {
        return (0, 0, 0, 0);
    }
    let span = |kernel| {
        let rect = PixelRect { x: g.pic_x, y: g.pic_y, w: g.pic_w, h: g.pic_h };
        crate::resize::source_coverage(sw, sh, g.image_w, g.image_h, kernel, rect)
    };
    let (x0, y0, x1, y1) = span(p.recipe.resample);
    let (x0, y0, x1, y1) = if p.recipe.sharpen.usm_amount() > 0.0 {
        let (mx0, my0, mx1, my1) = span(MASK_KERNEL);
        (x0.min(mx0), y0.min(my0), x1.max(mx1), y1.max(my1))
    } else {
        (x0, y0, x1, y1)
    };
    (x0 + ox, y0 + oy, x1 + ox, y1 + oy)
}

/// Kernel the mask sidecar is resampled with. Its weights are positive and
/// sum to one, so a saturated flag (exactly 1.0) stays exactly 1.0 when the
/// picture is enlarged, and nothing overshoots into or out of the flag when
/// it is reduced. A sharper kernel would ring the flags and sharpen pixels
/// the sidecar had excluded.
pub const MASK_KERNEL: ResampleKernel = ResampleKernel::Area;

/// The print pixels of one box: resize, place, tone, then the same
/// Richardson–Lucy and USM as [`render_planes`], on a tile padded by
/// [`sharpen_halo`]. `linear` and `mask` are the upright negative (orientation
/// already applied) and must cover [`loupe_source_rect`].
pub fn sharpen_window(
    linear: PlaneWindow<'_>,
    mask: Option<PlaneWindow<'_>>,
    p: &PrintParams,
    nx: f64,
    ny: f64,
    view: usize,
) -> crate::Result<Plane> {
    let (ox, oy, sw, sh) = source_window(linear.full_w, linear.full_h, p.recipe.crop);
    let lay = layout(linear.full_w, linear.full_h, 1, p);
    let g = loupe_geom(&lay, p.recipe.sharpen, nx, ny, view, peak_halo_px(&p.recipe, lay.enlargement));
    let tone = p.recipe.curve.compile();
    let white = tone.white();
    let mut tile = Plane::from_vec(g.tile_w, g.tile_h, vec![white; g.tile_w * g.tile_h]);
    if lay.black_px > 0 {
        let x0 = lay.border_px;
        let y0 = lay.border_px;
        paint_sheet_rect(
            &mut tile,
            g.tile_x,
            g.tile_y,
            (
                x0,
                y0,
                x0 + lay.black_px + lay.image_w + lay.black_px,
                y0 + lay.black_px + lay.image_h + lay.black_px,
            ),
            tone.eval(0.0),
        );
    }
    let mut mask_tile = mask.map(|_| Plane::zeros(g.tile_w, g.tile_h));
    // The plane window is in full-image coordinates. The resize maps the
    // crop, so its coordinate system is the window.
    let linear = window_frame(linear, ox, oy, sw, sh);
    let mask = mask.map(|m| window_frame(m, ox, oy, sw, sh));
    if g.pic_w > 0 && g.pic_h > 0 {
        let pic = PixelRect { x: g.pic_x, y: g.pic_y, w: g.pic_w, h: g.pic_h };
        let mut picture = resize_rect(linear, g.image_w, g.image_h, p.recipe.resample, pic)?;
        crate::tone::render(&mut picture, p.recipe.place, &tone);
        let ox = g.pic_x + g.border - g.tile_x;
        let oy = g.pic_y + g.border - g.tile_y;
        for y in 0..g.pic_h {
            tile.row_mut(oy + y)[ox..ox + g.pic_w].copy_from_slice(picture.row(y));
        }
        // Only resample the mask when a USM will read it: the crop from
        // `loupe_source_rect` includes the mask kernel's taps only then, and
        // resampling it anyway could read off the window.
        if p.recipe.sharpen.usm_amount() > 0.0 {
            if let (Some(m), Some(dst)) = (mask, mask_tile.as_mut()) {
                let resampled = resize_rect(m, g.image_w, g.image_h, MASK_KERNEL, pic)?;
                for y in 0..g.pic_h {
                    dst.row_mut(oy + y)[ox..ox + g.pic_w].copy_from_slice(resampled.row(y));
                }
            }
        }
        // Sharpening and peak on the picture pixels of the tile only, the
        // same rectangle `render_planes` sharpens, so the box matches the
        // full print and the paper around the picture stays untouched.
        let rect = (ox, oy, g.pic_w, g.pic_h);
        apply_sharpen(&mut tile, mask_tile.as_ref(), &p.recipe, &lay, rect);
        apply_peak_picture(
            &mut tile,
            &p.recipe,
            rect,
            peak_print_radius(p.recipe.peak.radius, lay.enlargement),
        );
    }
    let mut out = Plane::zeros(g.view_w, g.view_h);
    let ox = g.view_x - g.tile_x;
    let oy = g.view_y - g.tile_y;
    for y in 0..g.view_h {
        out.row_mut(y).copy_from_slice(&tile.row(oy + y)[ox..ox + g.view_w]);
    }
    Ok(out)
}

/// Paint `value` into the part of `tile` that overlaps the sheet rectangle
/// `(x0, y0, x1, y1)`, half-open. `tile_x` and `tile_y` are the tile's origin.
fn paint_sheet_rect(
    tile: &mut Plane,
    tile_x: usize,
    tile_y: usize,
    rect: (usize, usize, usize, usize),
    value: f64,
) {
    let (x0, y0, x1, y1) = rect;
    let left = x0.max(tile_x);
    let top = y0.max(tile_y);
    let right = x1.min(tile_x + tile.width);
    let bottom = y1.min(tile_y + tile.height);
    if left >= right || top >= bottom {
        return;
    }
    for y in top..bottom {
        tile.row_mut(y - tile_y)[left - tile_x..right - tile_x].fill(value);
    }
}

/// Surround `p` with `b` pixels of `value` on every side.
pub fn add_border(p: &Plane, b: usize, value: f64) -> Plane {
    frame_sheet(p, 0, b, value, value)
}

/// The sheet: `p` inside `black` pixels of `black_value`, inside `white`
/// pixels of `white_value`, in one allocation.
fn frame_sheet(p: &Plane, black: usize, white: usize, black_value: f64, white_value: f64) -> Plane {
    let frame = black + white;
    if frame == 0 {
        return p.clone();
    }
    let (w, h) = (p.width + 2 * frame, p.height + 2 * frame);
    let mut out = Plane::from_vec(w, h, vec![white_value; w * h]);
    out.data.par_chunks_mut(w).enumerate().skip(white).take(p.height + 2 * black).for_each(|(y, row)| {
        let inner = &mut row[white..w - white];
        if y < frame || y >= frame + p.height {
            inner.fill(black_value);
        } else {
            inner[..black].fill(black_value);
            inner[black..black + p.width].copy_from_slice(p.row(y - frame));
            inner[black + p.width..].fill(black_value);
        }
    });
    out
}

/// `(x0, y0, width, height)` of the picture inside a sheet or tile.
type PictureRect = (usize, usize, usize, usize);

/// Run `f` on the picture rectangle of `plane` and write the result back.
/// The paper and the black edge stay as they were, and the filters reflect
/// inside the picture, not into the border. A rectangle that does not fit
/// the plane is left alone. `mask` is cut to the same rectangle.
fn on_picture(
    plane: &mut Plane,
    mask: Option<&Plane>,
    rect: PictureRect,
    f: impl FnOnce(&Plane, Option<&Plane>) -> Plane,
) {
    let (x0, y0, width, height) = rect;
    if width == 0 || height == 0 {
        return;
    }
    if x0.saturating_add(width) > plane.width || y0.saturating_add(height) > plane.height {
        return;
    }
    let cut = |src: &Plane| {
        let mut out = Plane::zeros(width, height);
        for y in 0..height {
            out.row_mut(y).copy_from_slice(&src.row(y0 + y)[x0..x0 + width]);
        }
        out
    };
    let picture = cut(plane);
    let mask = mask.filter(|m| (m.width, m.height) == (plane.width, plane.height)).map(cut);
    let out = f(&picture, mask.as_ref());
    if (out.width, out.height) != (width, height) {
        return;
    }
    for y in 0..height {
        plane.row_mut(y0 + y)[x0..x0 + width].copy_from_slice(out.row(y));
    }
}

/// Peak on the picture rectangle only.
fn apply_peak_picture(plane: &mut Plane, recipe: &Recipe, rect: PictureRect, print_radius: f64) {
    if !recipe.peak.applies() {
        return;
    }
    on_picture(plane, None, rect, |picture, _| unsharp(picture, print_radius, recipe.peak.amount, None));
}

/// Richardson–Lucy and USM at the enlargement σ, on the picture rectangle
/// only. Returns the iterations run. A σ of zero leaves the plane as it is.
fn apply_sharpen(
    plane: &mut Plane,
    mask: Option<&Plane>,
    recipe: &Recipe,
    lay: &Layout,
    rect: PictureRect,
) -> usize {
    let detail = lay.resample_sigma_px;
    let passes = recipe.sharpen.deconv_passes() as usize;
    let amount = recipe.sharpen.usm_amount();
    if detail <= 0.0 || (passes == 0 && amount <= 0.0) {
        return 0;
    }
    let deconv_n = if passes > 0 { passes.min(DECONV_ITERATIONS) } else { 0 };
    on_picture(plane, mask, rect, |picture, mask| {
        let mut out = if deconv_n > 0 { richardson_lucy(picture, detail, deconv_n) } else { picture.clone() };
        if amount > 0.0 {
            out = unsharp(&out, detail, amount, mask);
        }
        out
    });
    deconv_n
}

/// The one pass on in-memory planes. `info.ms` covers this call only.
pub fn render_planes(src: &PrintSource<'_>, p: &PrintParams) -> PrintOutput {
    let t = Instant::now();
    let r = &p.recipe;
    let mut mask: Option<Plane> = None;
    // Upright first, so the fit uses the displayed aspect ratio.
    let mut plane = orient(src.plane, src.orientation);
    if p.sharpens() {
        if let Some(m) = src.mask {
            mask = Some(orient(m, src.orientation));
        }
    }
    if let Some(c) = r.crop {
        let c = c.sanitized(plane.width as u32, plane.height as u32);
        plane = window_plane(&plane, c);
        if let Some(m) = mask.take() {
            mask = Some(window_plane(&m, c));
        }
    }

    let lay = layout(src.plane.width, src.plane.height, src.orientation, p);
    let (tw, th) = (lay.image_w, lay.image_h);
    if (tw, th) != (plane.width, plane.height) {
        info!("resize {}x{} -> {}x{} ({})", plane.width, plane.height, tw, th, r.resample.name());
        plane = resize_with(&plane, tw, th, r.resample);
        if let Some(m) = mask.take() {
            mask = Some(resize_with(&m, tw, th, MASK_KERNEL));
        }
    }

    // Placing and the tone in one pass: stops on the linear luminance,
    // then paper or look.
    if r.place != 0.0 {
        info!("place {:+.2} stops", r.place);
    }
    let tone = r.curve.compile();
    info!("tone {}", r.curve.name());
    crate::tone::render(&mut plane, r.place, &tone);

    let px_mm = |px: usize| px as f64 * 25.4 / r.dpi;
    if lay.black_px > 0 {
        info!(
            "black edge {} px ({:.2} mm) at clear film {:.4}",
            lay.black_px,
            px_mm(lay.black_px),
            tone.eval(0.0)
        );
    }
    if lay.border_px > 0 {
        info!(
            "border {} px ({:.2} mm) in paper white {:.4}",
            lay.border_px,
            px_mm(lay.border_px),
            tone.white()
        );
    }
    let frame = lay.black_px + lay.border_px;
    if frame > 0 {
        // Both rings in one allocation. The mask gets 0 there: nothing to
        // sharpen on paper.
        plane = frame_sheet(&plane, lay.black_px, lay.border_px, tone.eval(0.0), tone.white());
        if let Some(m) = mask.take() {
            mask = Some(add_border(&m, frame, 0.0));
        }
    }

    let picture = (frame, frame, lay.image_w, lay.image_h);
    let deconv_n = apply_sharpen(&mut plane, mask.as_ref(), r, &lay, picture);
    let print_radius = peak_print_radius(r.peak.radius, lay.enlargement);
    apply_peak_picture(&mut plane, r, picture, print_radius);
    if r.peak.applies() {
        info!(
            "peak radius {:.2} px, amount {:.2}, scale {:.2}, border left as it was",
            print_radius, r.peak.amount, lay.enlargement
        );
    }
    if deconv_n > 0 {
        info!(
            "richardson-lucy {deconv_n} iterations, enlargement sigma {:.2} px, no mask, picture only",
            lay.resample_sigma_px
        );
    }
    if r.sharpen.usm_amount() > 0.0 && lay.resample_sigma_px > 0.0 {
        info!(
            "usm amount {:.2}, enlargement sigma {:.2} px ({:.2}×), masked: {}",
            r.sharpen.usm_amount(),
            lay.resample_sigma_px,
            lay.enlargement,
            mask.is_some()
        );
    }
    let info = PrintInfo {
        mimizan: env!("CARGO_PKG_VERSION"),
        stage: "print",
        source: src.name.clone(),
        source_description: src.description.clone(),
        look: r.curve.name(),
        look_encoding: format!("{:?}", r.curve.encoding()).to_ascii_lowercase(),
        width: lay.out_w,
        height: lay.out_h,
        dpi: r.dpi,
        orientation_applied: src.orientation,
        recipe: r.clone(),
        border_px: lay.border_px,
        black_px: lay.black_px,
        usm_amount: r.sharpen.usm_amount(),
        usm_radius_px: lay.usm_radius_px,
        resample_sigma_px: lay.resample_sigma_px,
        usm_masked: mask.is_some(),
        deconvolution_iterations: deconv_n as u32,
        ms: t.elapsed().as_millis(),
        plate: None,
    };
    debug_assert_eq!((plane.width, plane.height), (lay.out_w, lay.out_h));
    PrintOutput { plane, info, plate: None }
}

/// Write the print: a 16-bit TIFF (grey ICC; RGB ICC for a plate) unless
/// `path` ends in `.jpg`/`.jpeg`, in which case an 8-bit JPEG is written
/// (the mobile app's album; the desktop writes TIFF).
pub fn write(out: &PrintOutput, path: &Path, p: &PrintParams) -> Result<()> {
    let desc = serde_json::to_string(&out.info)?;
    let trc = match p.recipe.curve.encoding() {
        crate::calib::LookEncoding::Gamma22 => GrayTrc::Gamma(2.2),
        crate::calib::LookEncoding::None => GrayTrc::Linear,
    };
    let meta = TiffMeta { description: &desc, trc, orientation: 1, dpi: Some(p.recipe.dpi) };
    match (&out.plate, is_jpeg_path(path)) {
        (Some(sheet), true) => write_rgb_jpeg(sheet.rgb(&out.plane), sheet.space, path, trc)?,
        (Some(sheet), false) => crate::tiffout::write_rgb16(path, sheet.rgb(&out.plane), sheet.space, &meta)?,
        (None, true) => write_jpeg(&out.plane, path, trc)?,
        (None, false) => write_gray16(path, &out.plane, &meta)?,
    }
    Ok(())
}

/// JPEG of a plate print: 8-bit RGB, quality 100, the RGB ICC embedded.
pub fn write_rgb_jpeg(
    rgb: [&Plane; 3],
    space: crate::plate::PlateSpace,
    path: &Path,
    trc: GrayTrc,
) -> Result<()> {
    use image::ImageEncoder;
    let [r, g, b] = rgb;
    if (g.width, g.height) != (r.width, r.height) || (b.width, b.height) != (r.width, r.height) {
        return Err(Error::Invalid("the three print planes differ in size".into()));
    }
    let q = |v: f64| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
    let mut buf = vec![0u8; 3 * r.data.len()];
    buf.par_chunks_mut(3 * r.width).enumerate().for_each(|(y, row)| {
        let (rr, gr, br) = (r.row(y), g.row(y), b.row(y));
        for x in 0..r.width {
            row[3 * x] = q(rr[x]);
            row[3 * x + 1] = q(gr[x]);
            row[3 * x + 2] = q(br[x]);
        }
    });
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(file, 100);
    enc.set_icc_profile(crate::icc::rgb_profile(space, trc))
        .map_err(|e| Error::Invalid(format!("jpeg icc: {e}")))?;
    enc.write_image(&buf, r.width as u32, r.height as u32, image::ExtendedColorType::Rgb8)?;
    Ok(())
}

/// `.jpg` or `.jpeg`, any case.
pub fn is_jpeg_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("jpg" | "jpeg")
    )
}

/// JPEG of an already encoded plane: full print resolution, 8-bit grey (one
/// channel, so no chroma subsampling), quality 100, the same grey ICC as
/// the TIFF embedded.
pub fn write_jpeg(plane: &Plane, path: &Path, trc: GrayTrc) -> Result<()> {
    use image::ImageEncoder;
    let buf: Vec<u8> = plane.data.par_iter().map(|&v| (v.clamp(0.0, 1.0) * 255.0).round() as u8).collect();
    let file = std::io::BufWriter::new(std::fs::File::create(path)?);
    let mut enc = image::codecs::jpeg::JpegEncoder::new_with_quality(file, 100);
    enc.set_icc_profile(crate::icc::gray_profile(trc))
        .map_err(|e| Error::Invalid(format!("jpeg icc: {e}")))?;
    enc.write_image(&buf, plane.width as u32, plane.height as u32, image::ExtendedColorType::L8)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn size_parsing() {
        let s = PrintSize::parse("30x40cm").unwrap();
        let PrintSize::Physical { width_in, .. } = s;
        assert!((width_in - 11.811).abs() < 1e-3);
        assert_eq!(s.pixels(300.0), (3543, 4724));
        assert_eq!(PrintSize::parse("8x10in").unwrap().pixels(300.0), (2400, 3000));
        assert!(PrintSize::parse("30x40").is_err());
        assert!(PrintSize::parse("0x40cm").is_err());
        assert!(PrintSize::parse("infx10in").is_err());
        assert!(PrintSize::parse("nanx10in").is_err());
        assert!(PrintSize::parse("1000x10in").is_err());
        assert!(PrintSize::parse("2048px").is_err());
        assert_eq!(PrintSize::parse("100x100mm").unwrap().pixels(254.0), (1000, 1000));
    }

    #[test]
    fn memory_estimate_follows_the_sheet() {
        // 3 x 2 m at 300 dpi from a 24 MP negative, peak on: the sheet is
        // 837 MP, and the widest stage holds it beside four picture planes.
        let mut recipe = Recipe::on_paper();
        recipe.size = Some(PrintSize::parse("300x200cm").unwrap());
        recipe.dpi = 300.0;
        recipe.peak.on = true;
        recipe.peak.amount = 1.15;
        let p = PrintParams { recipe, ..Default::default() };
        let lay = layout(6000, 4000, 1, &p);
        let sheet = (lay.out_w * lay.out_h) as f64;
        let need = memory_estimate(6000, 4000, 1, &p) as f64;
        assert!(need >= 8.0 * 5.0 * sheet * 0.9, "{need} vs sheet {sheet}");
        assert!(need <= 8.0 * 5.0 * sheet * 1.2 + 2.0 * sheet, "{need} vs sheet {sheet}");
        assert!(need > 25e9 && need < 40e9, "{}", memory_text(need as u64));
        // Native size without peak: a few copies of the negative itself.
        let native = PrintParams { recipe: Recipe::on_paper(), ..Default::default() };
        let small = memory_estimate(6000, 4000, 1, &native);
        assert!(small < 24_000_000 * 8 * 4, "{small}");
        assert_eq!(memory_text(640_000_000), "640 MB");
        assert_eq!(memory_text(27_300_000_000), "27.3 GB");
    }

    #[test]
    fn native_crop_is_the_window() {
        let full = Plane::from_vec(3, 2, vec![0.1, 0.2, 0.3, 0.4, 0.5, 0.6]);
        let cut = Plane::from_vec(2, 2, vec![0.2, 0.3, 0.5, 0.6]);
        let recipe =
            Recipe { crop: Some(crate::recipe::CropRect { x: 1, y: 0, w: 2, h: 2 }), ..Recipe::on_paper() };
        let via = render_planes(
            &PrintSource { plane: &full, mask: None, orientation: 1, name: "full".into(), description: None },
            &PrintParams { recipe: recipe.clone(), ..Default::default() },
        );
        let direct = render_planes(
            &PrintSource { plane: &cut, mask: None, orientation: 1, name: "cut".into(), description: None },
            &PrintParams { recipe: Recipe { crop: None, ..recipe }, ..Default::default() },
        );
        assert_eq!((via.plane.width, via.plane.height), (2, 2));
        assert_eq!(via.plane.data, direct.plane.data);
    }

    #[test]
    fn deconvolution_runs_everywhere_the_mask_would_block_usm() {
        let (w, h) = (64, 32);
        let mut plane = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                plane.set(x, y, if x < 32 { 0.2 } else { 0.8 });
            }
        }
        let mask = Plane::from_vec(w, h, vec![0.0; w * h]);
        let src = PrintSource {
            plane: &plane,
            mask: Some(&mask),
            orientation: 1,
            name: "step".into(),
            description: None,
        };
        // Native size has no enlargement blur, so the detail pass is a copy.
        // This print is enlarged, and the mask is black everywhere: deconvolution
        // still runs, USM does not.
        let enlarge = |sharpen| PrintParams {
            recipe: Recipe {
                sharpen,
                dpi: 100.0,
                size: Some(PrintSize::Physical { width_in: 2.0, height_in: 1.0 }),
                ..Recipe::default()
            },
            ..Default::default()
        };
        let base = render_planes(&src, &enlarge(Sharpen::Off));
        let deconv =
            enlarge(Sharpen::Deconv { passes: crate::deconv::DECONV_ITERATIONS as u32, usm_amount: 0.0 });
        let out = render_planes(&src, &deconv);
        assert!(out.info.resample_sigma_px > 0.0);
        assert_eq!(out.info.deconvolution_iterations, crate::deconv::DECONV_ITERATIONS as u32);
        assert!(!out.info.usm_masked);
        let contrast = |p: &Plane| {
            let row = p.row(p.height / 2);
            row.windows(2).map(|w| w[1] - w[0]).fold(0.0_f64, f64::max)
        };
        assert!(
            contrast(&out.plane) > contrast(&base.plane),
            "deconv {:.4} base {:.4}",
            contrast(&out.plane),
            contrast(&base.plane)
        );
        let masked = render_planes(&src, &enlarge(Sharpen::Usm { amount: 1.0 }));
        assert_eq!(masked.plane.data, base.plane.data);
    }

    fn sized(size: PrintSize) -> PrintParams {
        PrintParams { recipe: Recipe { size: Some(size), ..Recipe::default() }, ..Default::default() }
    }

    #[test]
    fn layout_fits_the_upright_picture_into_the_box() {
        // 10 in square box at the default 300 dpi: 3000 px; 80 in: 24000 px.
        let p = sized(PrintSize::Physical { width_in: 10.0, height_in: 10.0 });
        assert_eq!(layout(4000, 3000, 1, &p).image_w, 3000);
        assert_eq!(layout(4000, 3000, 6, &p).image_h, 3000);
        let big = sized(PrintSize::Physical { width_in: 80.0, height_in: 80.0 });
        assert_eq!(layout(4000, 3000, 1, &big).image_w, 24000);
        assert_eq!(layout(4000, 3000, 1, &PrintParams::default()).image_w, 4000);
        assert!(is_jpeg_path(Path::new("a/b.JPG")) && !is_jpeg_path(Path::new("a/b.tif")));
    }

    /// The GUI's centimetre fields describe the picture. The box they write
    /// is that picture plus the border, and `layout` must give the picture
    /// back: the side the user typed, exactly, and the other side from the
    /// one scale `fit_dims` uses.
    #[test]
    fn box_for_picture_round_trips_through_layout() {
        let dpi = 300.0;
        let cases = [(5504, 8256), (4032, 3024), (100, 99), (5, 4), (3, 5), (1, 1), (8256, 5504)];
        let pictures = [2, 3, 10, 50, 1772, 2400];
        let borders = [0, 1, 3, 15, 177];
        for (img_w, img_h) in cases {
            for picture in pictures {
                for border in borders {
                    check_picture_box(img_w, img_h, picture, border, dpi, true);
                    check_picture_box(img_w, img_h, picture, border, dpi, false);
                }
            }
        }
    }

    fn check_picture_box(
        img_w: usize,
        img_h: usize,
        picture: usize,
        border: usize,
        dpi: f64,
        drive_width: bool,
    ) {
        let (bw, bh) = if drive_width {
            box_for_picture_width(img_w, img_h, picture, border)
        } else {
            box_for_picture_height(img_w, img_h, picture, border)
        };
        let p = PrintParams {
            recipe: Recipe {
                size: Some(PrintSize::Physical { width_in: bw as f64 / dpi, height_in: bh as f64 / dpi }),
                dpi,
                border_mm: border as f64 / dpi * 25.4,
                ..Recipe::default()
            },
            ..Default::default()
        };
        assert_eq!(p.recipe.border_px(), border, "border px must survive the mm round trip");
        let lay = layout(img_w, img_h, 1, &p);
        let (got, other, along) =
            if drive_width { (lay.image_w, lay.image_h, img_h) } else { (lay.image_h, lay.image_w, img_w) };
        assert_eq!(
            got,
            picture,
            "{img_w}x{img_h} drive {} = {picture} border {border}",
            if drive_width { "width" } else { "height" }
        );
        let expect = (along as f64 * picture as f64 / (if drive_width { img_w } else { img_h }) as f64)
            .round() as usize;
        assert_eq!(other, expect.max(1), "the other side is the one scale");
        assert_eq!((lay.out_w, lay.out_h), (lay.image_w + 2 * border, lay.image_h + 2 * border));
    }

    /// The border is taken from the paper, filled with the curve's white,
    /// and the picture sits centred inside it.
    #[test]
    fn border_is_paper_white_inside_the_paper() {
        let (w, h) = (60, 40);
        let plane = Plane::from_vec(w, h, vec![0.5; w * h]);
        let src =
            PrintSource { plane: &plane, mask: None, orientation: 1, name: "flat".into(), description: None };
        // 1 inch of border at 100 dpi on a 6x4 inch sheet: the 3:2 picture
        // fits the 400x200 inner box as 300x200, the file is 500x400.
        let p = PrintParams {
            recipe: Recipe {
                size: Some(PrintSize::Physical { width_in: 6.0, height_in: 4.0 }),
                dpi: 100.0,
                border_mm: 25.4,
                ..Recipe::default()
            },
            ..Default::default()
        };
        let lay = layout(w, h, 1, &p);
        assert_eq!(
            (lay.image_w, lay.image_h, lay.border_px, lay.out_w, lay.out_h),
            (300, 200, 100, 500, 400)
        );
        let out = render_planes(&src, &p);
        assert_eq!((out.plane.width, out.plane.height), (500, 400));
        assert_eq!(out.info.border_px, 100);
        assert_eq!((out.info.width, out.info.height), (500, 400));
        let white = p.recipe.curve.compile().white();
        assert_eq!(white, 1.0);
        assert_eq!(out.plane.at(0, 0), 1.0);
        assert_eq!(out.plane.at(99, 399), 1.0);
        assert_eq!(out.plane.at(499, 0), 1.0);
        let grey = 0.5f64.powf(1.0 / 2.2);
        assert!((out.plane.at(100, 100) - grey).abs() < 1e-12);
        assert!((out.plane.at(399, 299) - grey).abs() < 1e-12);
        assert_eq!(out.plane.at(400, 299), 1.0);
        // A look with a damped paper white fills the border with that value;
        // on paper the border is unexposed paper, 1 whatever the exposures.
        let mut look = crate::calib::LookFile::neutral();
        look.points.last_mut().unwrap()[1] = 0.95;
        let p2 = PrintParams {
            recipe: Recipe { curve: crate::tone::Tone::Look(look), ..p.recipe.clone() },
            ..Default::default()
        };
        let out2 = render_planes(&src, &p2);
        assert!((out2.plane.at(0, 0) - 0.95).abs() < 1e-12);
        let p3 = PrintParams {
            recipe: Recipe {
                curve: crate::tone::Tone::Paper(crate::paper::SplitGrade::new(1.0, 1.0)),
                ..p.recipe.clone()
            },
            ..Default::default()
        };
        let out3 = render_planes(&src, &p3);
        assert_eq!(out3.plane.at(0, 0), 1.0);
        assert!(out3.plane.at(100, 100) < grey);
    }

    /// Both rings are added around the crop window. A pixel inside the crop
    /// is the same as a print of that window with no border. A pixel outside
    /// the crop is not on the sheet. Black sits against the picture; white
    /// is the outer edge. A black wider than the white is as wide as asked.
    #[test]
    fn black_edge_wraps_the_crop_and_leaves_its_pixels() {
        let (w, h) = (8, 6);
        let mut data = vec![0.5; w * h];
        data[0] = 0.9;
        data[2 * w + 3] = 0.25;
        let plane = Plane::from_vec(w, h, data);
        let recipe = Recipe {
            dpi: 254.0,
            border_mm: 2.0,
            black_mm: 1.0,
            crop: Some(crate::recipe::CropRect { x: 2, y: 1, w: 4, h: 3 }),
            ..Recipe::default()
        };
        assert_eq!((recipe.border_px(), recipe.black_px(), recipe.frame_px()), (20, 10, 30));
        let wider = Recipe { black_mm: 9.0, ..recipe.clone() };
        assert_eq!(wider.black_px(), 90);
        let src =
            PrintSource { plane: &plane, mask: None, orientation: 1, name: "crop".into(), description: None };
        let p = PrintParams { recipe, ..Default::default() };
        let lay = layout(w, h, 1, &p);
        assert_eq!((lay.image_w, lay.image_h, lay.black_px, lay.border_px), (4, 3, 10, 20));
        assert_eq!((lay.out_w, lay.out_h), (64, 63));
        let out = render_planes(&src, &p);
        assert_eq!((out.plane.width, out.plane.height), (64, 63));
        assert_eq!((out.info.border_px, out.info.black_px), (20, 10));
        assert_eq!(out.plane.at(0, 0), 1.0);
        assert_eq!(out.plane.at(19, 31), 1.0);
        assert_eq!(out.plane.at(20, 31), 0.0);
        assert_eq!(out.plane.at(29, 31), 0.0);
        let bare = Recipe { border_mm: 0.0, black_mm: 0.0, ..p.recipe.clone() };
        let bare_out = render_planes(&src, &PrintParams { recipe: bare, ..Default::default() });
        assert_eq!((bare_out.plane.width, bare_out.plane.height), (4, 3));
        assert!((out.plane.at(30, 30) - bare_out.plane.at(0, 0)).abs() < 1e-12);
        assert!((out.plane.at(31, 31) - bare_out.plane.at(1, 1)).abs() < 1e-12);
        assert!((out.plane.at(33, 32) - bare_out.plane.at(3, 2)).abs() < 1e-12);
        let outside = 0.9f64.powf(1.0 / 2.2);
        assert!(out.plane.data.iter().all(|v| (*v - outside).abs() > 1e-6));
    }

    /// The black edge stands on its own: with no white border the sheet is
    /// the picture inside black, and the outermost pixel is black.
    #[test]
    fn black_edge_alone_is_the_whole_frame() {
        let (w, h) = (8, 6);
        let plane = Plane::from_vec(w, h, vec![0.5; w * h]);
        let recipe = Recipe { dpi: 254.0, border_mm: 0.0, black_mm: 1.0, ..Recipe::default() };
        assert_eq!((recipe.border_px(), recipe.black_px(), recipe.frame_px()), (0, 10, 10));
        let src = PrintSource {
            plane: &plane,
            mask: None,
            orientation: 1,
            name: "black".into(),
            description: None,
        };
        let p = PrintParams { recipe, ..Default::default() };
        let lay = layout(w, h, 1, &p);
        assert_eq!((lay.out_w, lay.out_h, lay.black_px, lay.border_px), (28, 26, 10, 0));
        let out = render_planes(&src, &p);
        assert_eq!((out.plane.width, out.plane.height), (28, 26));
        assert_eq!(out.plane.at(0, 0), 0.0);
        assert_eq!(out.plane.at(9, 9), 0.0);
        assert_eq!(out.plane.at(27, 25), 0.0);
        let bare = Recipe { black_mm: 0.0, ..p.recipe.clone() };
        let bare_out = render_planes(&src, &PrintParams { recipe: bare, ..Default::default() });
        assert!((out.plane.at(10, 10) - bare_out.plane.at(0, 0)).abs() < 1e-12);
        assert!(out.plane.data.iter().all(|v| *v < 1.0));
    }

    /// The centimetre picture stays the size that was asked. Black and white
    /// grow the sheet around it.
    #[test]
    fn black_and_white_are_added_around_the_picture() {
        let dpi = 300.0;
        let (img_w, img_h) = (5504, 8256);
        let picture = 2400usize;
        let white = 15usize;
        let black = 4usize;
        let frame = white + black;
        let (bw, bh) = box_for_picture_width(img_w, img_h, picture, frame);
        let p = PrintParams {
            recipe: Recipe {
                size: Some(PrintSize::Physical { width_in: bw as f64 / dpi, height_in: bh as f64 / dpi }),
                dpi,
                border_mm: white as f64 / dpi * 25.4,
                black_mm: black as f64 / dpi * 25.4,
                ..Recipe::default()
            },
            ..Default::default()
        };
        assert_eq!(p.recipe.border_px(), white);
        assert_eq!(p.recipe.black_px(), black);
        let lay = layout(img_w, img_h, 1, &p);
        assert_eq!(lay.image_w, picture);
        assert_eq!((lay.out_w, lay.out_h), (lay.image_w + 2 * frame, lay.image_h + 2 * frame));
    }

    /// Paper, table 4: place = 0 is the run that never had
    /// the step; the same recipe twice is the same picture; paper at its
    /// base exposure is a different picture from the neutral look.
    #[test]
    fn untouched_recipe_is_bit_identical_to_neutral() {
        let (w, h) = (48, 32);
        let plane = Plane::from_vec(w, h, (0..w * h).map(|i| (i % 97) as f64 / 96.0).collect());
        let src =
            PrintSource { plane: &plane, mask: None, orientation: 6, name: "ramp".into(), description: None };
        let neutral = render_planes(&src, &PrintParams::default());
        let placed_zero =
            PrintParams { recipe: Recipe { place: 0.0, ..Recipe::default() }, ..Default::default() };
        assert_eq!(render_planes(&src, &placed_zero).plane.data, neutral.plane.data);
        let paper = PrintParams { recipe: Recipe::on_paper(), ..Default::default() };
        let a = render_planes(&src, &paper);
        let b = render_planes(&src, &paper);
        assert_eq!(a.plane.data, b.plane.data);
        assert_ne!(a.plane.data, neutral.plane.data);
        // A placed picture is a different one, and the description says so.
        let placed = PrintParams { recipe: Recipe { place: 0.5, ..Recipe::default() }, ..Default::default() };
        let out = render_planes(&src, &placed);
        assert_ne!(out.plane.data, neutral.plane.data);
        let json = serde_json::to_string(&out.info).unwrap();
        assert!(json.contains(r#""recipe":{"#) && json.contains(r#""place":0.5"#), "{json}");
    }

    #[test]
    fn orientation_round_trips() {
        let p = Plane::from_vec(3, 2, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        assert_eq!(orient(&p, 1).data, p.data);
        let r = orient(&p, 6); // rotate 90° CW: 3x2 -> 2x3
        assert_eq!((r.width, r.height), (2, 3));
        assert_eq!(r.data, vec![4.0, 1.0, 5.0, 2.0, 6.0, 3.0]);
        let l = orient(&p, 8); // rotate 90° CCW
        assert_eq!(l.data, vec![3.0, 6.0, 2.0, 5.0, 1.0, 4.0]);
        let u = orient(&p, 3);
        assert_eq!(u.data, vec![6.0, 5.0, 4.0, 3.0, 2.0, 1.0]);
        // 6 then 8 is the identity.
        assert_eq!(orient(&r, 8).data, p.data);
    }

    #[test]
    fn rotate_cw_composes_with_every_orientation() {
        let p = Plane::from_vec(3, 2, vec![1.0, 2.0, 3.0, 4.0, 5.0, 6.0]);
        for o in 1..=8u16 {
            let twice = orient(&orient(&p, o), 6);
            let once = orient(&p, rotate_cw(o));
            assert_eq!((twice.width, twice.height), (once.width, once.height), "orientation {o}");
            assert_eq!(twice.data, once.data, "orientation {o}");
        }
        assert_eq!(rotated(1, 4), 1);
        assert_eq!(rotated(1, 2), 3);
        // The 8-bit rotation matches `orient` with orientation 6 / 3 / 8.
        let g: Vec<u8> = p.data.iter().map(|&v| v as u8).collect();
        for (k, o) in [(1u8, 6u16), (2, 3), (3, 8)] {
            let (rw, rh, rg) = rotate_gray8_cw(3, 2, &g, k);
            let r = orient(&p, o);
            assert_eq!((rw, rh), (r.width, r.height));
            assert_eq!(rg, r.data.iter().map(|&v| v as u8).collect::<Vec<_>>());
        }
    }

    /// `upright_pixel` is the inverse of the lookup `orient` performs.
    #[test]
    fn upright_pixel_inverts_orient() {
        let (w, h) = (4, 3);
        let p = Plane::from_vec(w, h, (0..12).map(|i| i as f64).collect());
        for o in 1..=8u16 {
            let up = orient(&p, o);
            for sy in 0..h {
                for sx in 0..w {
                    let (ux, uy) = upright_pixel(o, w, h, sx, sy);
                    assert_eq!(up.at(ux, uy), p.at(sx, sy), "orientation {o} at {sx},{sy}");
                }
            }
        }
    }

    /// The loupe is the same pixels as the full print, including a resize,
    /// a border (paper white under the kernel) and both sharpening steps.
    #[test]
    fn sharpen_window_matches_the_full_print() {
        let (w, h) = (36, 24);
        let mut plane = Plane::zeros(w, h);
        let mut mask = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                plane.set(x, y, 0.15 + 0.7 * x as f64 / w as f64 + if y > 12 { 0.1 } else { 0.0 });
                mask.set(x, y, if x > 4 && x < 30 { 0.9 } else { 0.2 });
            }
        }
        let mut recipe = Recipe::on_paper();
        recipe.dpi = 100.0;
        recipe.border_mm = 1.0;
        recipe.black_mm = 0.5;
        recipe.size = Some(PrintSize::Physical { width_in: 0.22, height_in: 0.22 });
        recipe.resample = ResampleKernel::Lanczos3;
        recipe.sharpen = Sharpen::Deconv { passes: 2, usm_amount: 0.6 };
        recipe.place = 0.25;
        let params = PrintParams { recipe, ..PrintParams::default() };
        let full = render_planes(
            &PrintSource {
                plane: &plane,
                mask: Some(&mask),
                orientation: 1,
                name: "t".into(),
                description: None,
            },
            &params,
        );
        let lay = layout(w, h, 1, &params);
        for (nx, ny) in [(0.5, 0.5), (0.0, 0.0), (1.0, 1.0), (0.1, 0.85)] {
            let (sx, sy, ex, ey) = loupe_source_rect(w, h, &params, nx, ny, 11);
            assert!(ex > sx && ey > sy);
            let mut lc = Plane::zeros(ex - sx, ey - sy);
            let mut mc = Plane::zeros(ex - sx, ey - sy);
            for y in 0..lc.height {
                for x in 0..lc.width {
                    lc.set(x, y, plane.at(sx + x, sy + y));
                    mc.set(x, y, mask.at(sx + x, sy + y));
                }
            }
            let got = sharpen_window(
                PlaneWindow { plane: &lc, origin_x: sx, origin_y: sy, full_w: w, full_h: h },
                Some(PlaneWindow { plane: &mc, origin_x: sx, origin_y: sy, full_w: w, full_h: h }),
                &params,
                nx,
                ny,
                11,
            )
            .expect("loupe window");
            let g = loupe_geom(
                &lay,
                params.recipe.sharpen,
                nx,
                ny,
                11,
                peak_halo_px(&params.recipe, lay.enlargement),
            );
            for y in 0..got.height {
                for x in 0..got.width {
                    let expect = full.plane.at(g.view_x + x, g.view_y + y);
                    let v = got.at(x, y);
                    assert!((v - expect).abs() < 1e-9, "at {nx},{ny} +{x},{y}: {v} vs {expect}");
                }
            }
        }
    }

    /// Size, dpi and a kernel narrower than Lanczos-3 must not read outside
    /// the crop. The mask resample is Lanczos-3 and only runs for USM; doing
    /// it for every kernel walks off the window and kills the loupe thread.
    #[test]
    fn loupe_follows_size_dpi_and_kernel() {
        let (w, h) = (48, 32);
        let mut plane = Plane::zeros(w, h);
        let mut mask = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                plane.set(x, y, ((x * 3 + y * 5) % 17) as f64 / 16.0);
                mask.set(x, y, if x > 2 { 0.9 } else { 0.1 });
            }
        }
        let points = [(0.5, 0.5), (0.0, 0.0), (1.0, 1.0), (0.08, 0.92)];
        let sharpens = [
            Sharpen::Off,
            Sharpen::Usm { amount: 0.5 },
            Sharpen::Deconv { passes: 2, usm_amount: 0.0 },
            Sharpen::Deconv { passes: 1, usm_amount: 0.25 },
        ];
        for kernel in ResampleKernel::ALL {
            for (dpi, width_in) in [(100.0, 0.3), (300.0, 1.2), (600.0, 0.15)] {
                for sharpen in sharpens {
                    let mut recipe = Recipe::on_paper();
                    recipe.dpi = dpi;
                    recipe.resample = kernel;
                    recipe.sharpen = sharpen;
                    recipe.border_mm = 2.0;
                    recipe.black_mm = 1.0;
                    recipe.size =
                        Some(PrintSize::Physical { width_in, height_in: width_in * h as f64 / w as f64 });
                    let params = PrintParams { recipe, ..PrintParams::default() };
                    let full = render_planes(
                        &PrintSource {
                            plane: &plane,
                            mask: Some(&mask),
                            orientation: 1,
                            name: "t".into(),
                            description: None,
                        },
                        &params,
                    );
                    let lay = layout(w, h, 1, &params);
                    for (nx, ny) in points {
                        let (sx, sy, ex, ey) = loupe_source_rect(w, h, &params, nx, ny, 16);
                        let (lc, mc) = if ex > sx && ey > sy {
                            let mut lc = Plane::zeros(ex - sx, ey - sy);
                            let mut mc = Plane::zeros(ex - sx, ey - sy);
                            for y in 0..lc.height {
                                for x in 0..lc.width {
                                    lc.set(x, y, plane.at(sx + x, sy + y));
                                    mc.set(x, y, mask.at(sx + x, sy + y));
                                }
                            }
                            (lc, mc)
                        } else {
                            (plane.clone(), mask.clone())
                        };
                        let origin_x = if ex > sx { sx } else { 0 };
                        let origin_y = if ey > sy { sy } else { 0 };
                        let got = sharpen_window(
                            PlaneWindow { plane: &lc, origin_x, origin_y, full_w: w, full_h: h },
                            Some(PlaneWindow { plane: &mc, origin_x, origin_y, full_w: w, full_h: h }),
                            &params,
                            nx,
                            ny,
                            16,
                        )
                        .expect("loupe window");
                        let g = loupe_geom(
                            &lay,
                            sharpen,
                            nx,
                            ny,
                            16,
                            peak_halo_px(&params.recipe, lay.enlargement),
                        );
                        assert_eq!((got.width, got.height), (g.view_w, g.view_h));
                        for y in 0..got.height {
                            for x in 0..got.width {
                                let expect = full.plane.at(g.view_x + x, g.view_y + y);
                                let v = got.at(x, y);
                                assert!(
                                    (v - expect).abs() < 1e-9,
                                    "{kernel:?} {dpi} dpi {sharpen:?} at {nx},{ny} +{x},{y}: {v} vs {expect}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }

    fn step_source() -> Plane {
        let (w, h) = (32usize, 8usize);
        let mut plane = Plane::zeros(w, h);
        for y in 0..h {
            for x in w / 2..w {
                plane.set(x, y, 1.0);
            }
        }
        plane
    }

    /// 32×8 enlarged to 100×25 (3.125×) at 300 dpi, viewed from 2 m so the
    /// arc-minute radius is several pixels and the enlargement σ is not.
    fn enlarged_step(sharpen: Sharpen) -> (Plane, Layout) {
        let plane = step_source();
        let params = PrintParams {
            recipe: Recipe {
                sharpen,
                dpi: 300.0,
                resample: ResampleKernel::BSpline,
                size: Some(PrintSize::Physical { width_in: 100.0 / 300.0, height_in: 25.0 / 300.0 }),
                ..Recipe::default()
            },
            viewing_distance_mm: Some(2000.0),
            ..Default::default()
        };
        let out = render_planes(
            &PrintSource {
                plane: &plane,
                mask: None,
                orientation: 1,
                name: "step".into(),
                description: None,
            },
            &params,
        );
        let lay = layout(plane.width, plane.height, 1, &params);
        (out.plane, lay)
    }

    fn flats_and_edge(row: &[f64]) -> (f64, f64, usize) {
        let left = row[..8].iter().sum::<f64>() / 8.0;
        let right = row[row.len() - 8..].iter().sum::<f64>() / 8.0;
        let mid = 0.5 * (left + right);
        let mut edge = 1usize;
        for i in 1..row.len() {
            if (row[i - 1] - mid) * (row[i] - mid) <= 0.0 {
                edge = i;
                break;
            }
        }
        (left, right, edge)
    }

    #[test]
    fn native_and_downscale_sharpening_copies_the_picture() {
        let plane = step_source();
        let run = |sharpen, size: Option<PrintSize>| {
            let params = PrintParams {
                recipe: Recipe { sharpen, size, ..Recipe::default() },
                viewing_distance_mm: Some(2000.0),
                ..Default::default()
            };
            render_planes(
                &PrintSource {
                    plane: &plane,
                    mask: None,
                    orientation: 1,
                    name: "step".into(),
                    description: None,
                },
                &params,
            )
            .plane
        };
        let native = run(Sharpen::Off, None);
        assert_eq!(run(Sharpen::Usm { amount: 1.5 }, None).data, native.data);
        assert_eq!(run(Sharpen::Deconv { passes: 10, usm_amount: 1.0 }, None).data, native.data);
        let small = Some(PrintSize::Physical { width_in: 0.05, height_in: 0.02 });
        let shrunk = run(Sharpen::Off, small);
        assert_eq!(run(Sharpen::Usm { amount: 1.5 }, small).data, shrunk.data);
        assert_eq!(run(Sharpen::Deconv { passes: 10, usm_amount: 0.5 }, small).data, shrunk.data);
    }

    #[test]
    fn enlargement_sharpening_stays_on_the_edge() {
        let (plain, lay) = enlarged_step(Sharpen::Off);
        assert!(lay.enlargement > 3.0 && lay.enlargement < 3.3, "{}", lay.enlargement);
        assert!((lay.resample_sigma_px - 0.515 * lay.enlargement).abs() < 1e-9);
        assert!(lay.usm_radius_px > 6.0, "arc-minute radius {}", lay.usm_radius_px);
        assert!(lay.resample_sigma_px < 2.0, "detail σ {}", lay.resample_sigma_px);

        let row = plain.row(plain.height / 2);
        let (left, right, edge) = flats_and_edge(row);
        let step = right - left;
        let width = |samples: &[f64]| {
            let (_, _, e) = flats_and_edge(samples);
            let mut lo = e;
            let mut hi = e;
            let a = left + 0.1 * step;
            let b = left + 0.9 * step;
            while lo > 0 && samples[lo] > a {
                lo -= 1;
            }
            while hi + 1 < samples.len() && samples[hi] < b {
                hi += 1;
            }
            hi - lo
        };
        let plain_w = width(row);

        let (deconv, _) = enlarged_step(Sharpen::Deconv { passes: 3, usm_amount: 0.0 });
        let drow = deconv.row(deconv.height / 2);
        assert!(width(drow) < plain_w, "deconv {} vs plain {}", width(drow), plain_w);
        for dx in [12, 16] {
            for x in [edge - dx, edge + dx] {
                assert!(
                    (drow[x] - row[x]).abs() < 0.02 * step,
                    "deconv halo at {x} (edge {edge}): {} vs {}",
                    drow[x],
                    row[x]
                );
            }
        }

        let (usm, _) = enlarged_step(Sharpen::Usm { amount: 0.5 });
        let urow = usm.row(usm.height / 2);
        let mut peak_at = 0usize;
        let mut peak = 0.0;
        for (i, (u, p)) in urow.iter().zip(row).enumerate() {
            let d = (u - p).abs();
            if d > peak {
                peak = d;
                peak_at = i;
            }
        }
        // The boost sits on the shoulder of the ramp, a few pixels off the
        // 50 % point. A viewing-distance halo of ~7 px would still be up
        // well past that.
        assert!((peak_at as isize - edge as isize).abs() <= 6, "boost at {peak_at}, edge {edge}");
        for dx in [14, 18] {
            for x in [edge - dx, edge + dx] {
                assert!(
                    (urow[x] - row[x]).abs() < 0.01 * step,
                    "usm halo at {x} (edge {edge}, σ {:.2}): {} vs {}",
                    lay.resample_sigma_px,
                    urow[x],
                    row[x]
                );
            }
        }
    }

    /// A stored radius of 1 px is 2 px on a 2× print and 0.5 px on a half-size print.
    #[test]
    fn the_print_scale_multiplies_the_peak_radius() {
        assert!((peak_print_radius(1.4, 1.0) - 1.4).abs() < 1e-12);
        assert!((peak_print_radius(1.4, 0.5) - 0.7).abs() < 1e-12);
        assert!((peak_print_radius(1.4, 2.0) - 2.8).abs() < 1e-12);

        let (w, h) = (32, 24);
        let mut plane = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                plane.set(x, y, if x < w / 2 { 0.2 } else { 0.8 });
            }
        }
        let mut recipe = Recipe::on_paper();
        recipe.dpi = 100.0;
        recipe.size = Some(PrintSize::Physical { width_in: 0.64, height_in: 0.48 });
        recipe.peak.on = true;
        recipe.peak.radius = 1.0;
        recipe.peak.amount = 0.8;
        let params = PrintParams { recipe, ..PrintParams::default() };
        let lay = layout(w, h, 1, &params);
        assert!((lay.enlargement - 2.0).abs() < 1e-9, "{}", lay.enlargement);
        let src =
            PrintSource { plane: &plane, mask: None, orientation: 1, name: "t".into(), description: None };
        let got = render_planes(&src, &params);
        let mut bare = params.clone();
        bare.recipe.peak.on = false;
        let plain = render_planes(&src, &bare);
        let mut picture = Plane::zeros(lay.image_w, lay.image_h);
        for y in 0..lay.image_h {
            picture.row_mut(y).copy_from_slice(&plain.plane.row(y)[..lay.image_w]);
        }
        let expect = unsharp(&picture, 2.0, 0.8, None);
        let y = lay.image_h / 2;
        for x in 0..lay.image_w {
            let a = got.plane.at(x, y);
            let b = expect.at(x, y);
            assert!((a - b).abs() < 1e-9, "x {x}: {a} vs {b}");
        }

        let mut small = params.clone();
        small.recipe.size = Some(PrintSize::Physical { width_in: 0.16, height_in: 0.12 });
        let lay = layout(w, h, 1, &small);
        assert!((lay.enlargement - 0.5).abs() < 1e-9, "{}", lay.enlargement);
        let got = render_planes(&src, &small);
        let mut bare = small.clone();
        bare.recipe.peak.on = false;
        let plain = render_planes(&src, &bare);
        let mut picture = Plane::zeros(lay.image_w, lay.image_h);
        for y in 0..lay.image_h {
            picture.row_mut(y).copy_from_slice(&plain.plane.row(y)[..lay.image_w]);
        }
        let expect = unsharp(&picture, 0.5, 0.8, None);
        let y = lay.image_h / 2;
        for x in 0..lay.image_w {
            let a = got.plane.at(x, y);
            let b = expect.at(x, y);
            assert!((a - b).abs() < 1e-9, "half-size x {x}: {a} vs {b}");
        }
    }

    /// Peak on a plate: each positive is sharpened like the grey print,
    /// so a grey edge stays grey after it, and a coloured edge is lifted
    /// in every channel. A lamp on top shifts one channel and nothing else.
    #[test]
    fn peak_sharpens_every_positive_of_a_plate() {
        use crate::plate::{PlateInfo, PlateSpace, PlateWeights};
        let (w, h) = (72, 48);
        let step = |lo: f64, hi: f64| {
            let mut p = Plane::zeros(w, h);
            for y in 0..h {
                for x in 0..w {
                    p.set(x, y, if x < w / 2 { lo } else { hi });
                }
            }
            p
        };
        let grey = step(0.15, 0.75);
        let mut recipe = Recipe::on_paper();
        recipe.dpi = 254.0;
        recipe.peak.on = true;
        recipe.peak.radius = 1.0;
        recipe.peak.amount = 0.8;
        let params = PrintParams { recipe, ..PrintParams::default() };
        let mut quiet = params.clone();
        quiet.recipe.peak.on = false;
        fn src(plane: &Plane) -> PrintSource<'_> {
            PrintSource { plane, mask: None, orientation: 1, name: "t".into(), description: None }
        }
        let pi = PlateInfo::new(&PlateWeights::channels(), PlateSpace::AdobeRgb, h);

        // Grey plate: three equal positives, sharpened alike.
        let out = render_plate([src(&grey), src(&grey), src(&grey)], &pi, &params);
        let sheet = out.plate.as_ref().unwrap();
        assert_eq!(out.plane.data, sheet.green.data, "grey stays grey under peak");
        assert_eq!(sheet.green.data, sheet.blue.data);
        let mono = render_planes(&src(&grey), &params);
        assert_eq!(out.plane.data, mono.plane.data, "a positive is the grey print");
        let plain = render_planes(&src(&grey), &quiet);
        let edge = w / 2;
        assert!((out.plane.at(edge, h / 2) - plain.plane.at(edge, h / 2)).abs() > 1e-3, "peak did run");

        // Coloured plate: a red edge, a flat green, a blue step the other way.
        let red = step(0.1, 0.9);
        let green = step(0.4, 0.4);
        let blue = step(0.7, 0.2);
        let out = render_plate([src(&red), src(&green), src(&blue)], &pi, &params);
        let still = render_plate([src(&red), src(&green), src(&blue)], &pi, &quiet);
        let sheet = out.plate.as_ref().unwrap();
        let flat = still.plate.as_ref().unwrap();
        assert!((out.plane.at(edge, h / 2) - still.plane.at(edge, h / 2)).abs() > 1e-3, "red edge lifted");
        assert!((sheet.blue.at(edge, h / 2) - flat.blue.at(edge, h / 2)).abs() > 1e-3, "blue edge lifted");
        assert!(
            (sheet.green.at(edge, h / 2) - flat.green.at(edge, h / 2)).abs() < 1e-9,
            "flat green untouched"
        );
        for ch in [&sheet.green, &flat.green] {
            let v = ch.at(0, 0);
            assert!(ch.data.iter().all(|&x| (x - v).abs() < 1e-9), "green is a flat field");
        }

        // A red lamp on the sharpened plate: red moves, green and blue as before.
        let mut lit = params.clone();
        lit.lamps = [0.3, 0.0, 0.0];
        let warm = render_plate([src(&red), src(&green), src(&blue)], &pi, &lit);
        let ws = warm.plate.as_ref().unwrap();
        assert_eq!(ws.green.data, sheet.green.data);
        assert_eq!(ws.blue.data, sheet.blue.data);
        assert!(warm.plane.data.iter().zip(&out.plane.data).all(|(a, b)| a >= b));
        assert!(warm.plane.data.iter().zip(&out.plane.data).any(|(a, b)| a > b));
        assert_eq!(warm.info.plate.as_ref().unwrap().lamps, [0.3, 0.0, 0.0]);
    }

    /// Peak lifts the picture and leaves the paper. The 1:1 box is those
    /// same pixels, including where the box sits on the border.
    #[test]
    fn peak_matches_the_loupe_and_leaves_the_border() {
        let (w, h) = (72, 48);
        let mut plane = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                plane.set(x, y, if x < w / 2 { 0.15 } else { 0.75 });
            }
        }
        let mut recipe = Recipe::on_paper();
        recipe.dpi = 254.0;
        recipe.border_mm = 1.0;
        recipe.black_mm = 0.5;
        recipe.peak.on = true;
        recipe.peak.radius = 1.0;
        recipe.peak.amount = 0.8;
        let params = PrintParams { recipe, ..PrintParams::default() };
        let src =
            PrintSource { plane: &plane, mask: None, orientation: 1, name: "t".into(), description: None };
        let full = render_planes(&src, &params);
        let frame = full.info.border_px + full.info.black_px;
        assert!(frame >= 5, "frame {frame}");
        let mut quiet = params.clone();
        quiet.recipe.peak.on = false;
        let plain = render_planes(&src, &quiet);
        assert_eq!(full.plane.at(0, 0), plain.plane.at(0, 0), "paper");
        assert_eq!(
            full.plane.at(full.info.border_px, 2),
            plain.plane.at(full.info.border_px, 2),
            "black edge"
        );
        let inside = frame + 4;
        let y = frame + h / 2;
        assert!((full.plane.at(inside, y) - plain.plane.at(inside, y)).abs() < 1e-6, "flat field");
        let edge = frame + w / 2;
        assert!(
            (full.plane.at(edge, y) - plain.plane.at(edge, y)).abs() > 1e-3,
            "edge {} vs {}",
            full.plane.at(edge, y),
            plain.plane.at(edge, y)
        );

        let lay = layout(w, h, 1, &params);
        for (nx, ny) in [(0.5, 0.5), (0.0, 0.0), (1.0, 1.0), (0.5, 0.0)] {
            let (sx, sy, ex, ey) = loupe_source_rect(w, h, &params, nx, ny, 15);
            let (lc, origin_x, origin_y) = if ex > sx && ey > sy {
                let mut lc = Plane::zeros(ex - sx, ey - sy);
                for yy in 0..lc.height {
                    for xx in 0..lc.width {
                        lc.set(xx, yy, plane.at(sx + xx, sy + yy));
                    }
                }
                (lc, sx, sy)
            } else {
                (plane.clone(), 0, 0)
            };
            let got = sharpen_window(
                PlaneWindow { plane: &lc, origin_x, origin_y, full_w: w, full_h: h },
                None,
                &params,
                nx,
                ny,
                15,
            )
            .expect("loupe");
            let g = loupe_geom(
                &lay,
                params.recipe.sharpen,
                nx,
                ny,
                15,
                peak_halo_px(&params.recipe, lay.enlargement),
            );
            assert_eq!((got.width, got.height), (g.view_w, g.view_h));
            for yy in 0..got.height {
                for xx in 0..got.width {
                    let expect = full.plane.at(g.view_x + xx, g.view_y + yy);
                    let v = got.at(xx, yy);
                    assert!((v - expect).abs() < 1e-9, "at {nx},{ny} +{xx},{yy}: {v} vs {expect}");
                }
            }
        }
    }
}
