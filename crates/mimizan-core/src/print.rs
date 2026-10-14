//! Negative -> print file (SPEC section 9, Zonensystem paper section 7): the
//! one pass. Upright -> crop window -> size -> place -> curve -> border ->
//! [Richardson–Lucy ->] USM -> one quantisation ->
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
        if w <= 0.0 || h <= 0.0 {
            return Err(Error::Invalid("size must be positive".into()));
        }
        Ok(Self::Physical { width_in: w, height_in: h })
    }

    /// Bounding box in pixels.
    pub fn pixels(&self, dpi: f64) -> (usize, usize) {
        let Self::Physical { width_in, height_in } = *self;
        ((width_in * dpi).round().max(1.0) as usize, (height_in * dpi).round().max(1.0) as usize)
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct PrintParams {
    /// Every print decision.
    pub recipe: Recipe,
    /// USM radius constant of the viewing model (`usm.rs`).
    pub usm_k: f64,
    /// Viewing distance for the USM radius, mm. `None` = print diagonal.
    pub viewing_distance_mm: Option<f64>,
    /// Use the `.mask.tif` sidecar to restrict USM.
    pub use_mask: bool,
}

impl Default for PrintParams {
    fn default() -> Self {
        Self { recipe: Recipe::default(), usm_k: USM_K, viewing_distance_mm: None, use_mask: true }
    }
}

impl PrintParams {
    /// Just a look, everything else default (the GUI's TIFF export).
    pub fn with_look(look: crate::calib::LookFile) -> Self {
        Self { recipe: Recipe::with_look(look), ..Self::default() }
    }

    /// USM consults the mask. Deconvolution does not: it runs on the whole
    /// picture, and USM may follow it.
    pub fn sharpens(&self) -> bool {
        self.recipe.sharpen.uses_mask()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct PrintInfo {
    pub mimizan: &'static str,
    pub stage: &'static str,
    pub source: String,
    pub source_description: Option<serde_json::Value>,
    pub look: String,
    pub look_encoding: String,
    pub width: usize,
    pub height: usize,
    pub dpi: f64,
    pub orientation_applied: u16,
    /// The recipe this pass ran (Zonensystem paper, section 11): place,
    /// curve, kernel, size, border, sharpening mode.
    pub recipe: Recipe,
    /// Border in pixels on each side, when there is one.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub border_px: usize,
    pub usm_amount: f64,
    /// One arc minute at the viewing distance, in print pixels. Recorded
    /// only: sharpening uses [`Self::resample_sigma_px`].
    pub usm_radius_px: f64,
    /// Gaussian σ of the enlargement, in print pixels. Zero at native size
    /// and when shrinking. USM and Richardson–Lucy use this.
    pub resample_sigma_px: f64,
    pub usm_masked: bool,
    /// Richardson–Lucy iterations when that path ran; omitted when it did not.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub deconvolution_iterations: u32,
    pub ms: u128,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_zero_usize(v: &usize) -> bool {
    *v == 0
}

/// Apply a TIFF orientation (1..8) to the pixels so the result is upright.
pub fn orient(p: &Plane, orientation: u16) -> Plane {
    let (w, h) = (p.width, p.height);
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
/// new (width, height, pixels).
pub fn rotate_gray8_cw(w: usize, h: usize, g: &[u8], quarter_turns: u8) -> (usize, usize, Vec<u8>) {
    let k = quarter_turns % 4;
    if k == 0 {
        return (w, h, g.to_vec());
    }
    let (ow, oh) = if k % 2 == 1 { (h, w) } else { (w, h) };
    let mut out = vec![0u8; ow * oh];
    out.par_chunks_mut(ow).enumerate().for_each(|(oy, row)| {
        for (ox, o) in row.iter_mut().enumerate() {
            let (sx, sy) = match k {
                1 => (oy, h - 1 - ox),
                2 => (w - 1 - ox, h - 1 - oy),
                _ => (w - 1 - oy, ox),
            };
            *o = g[sy * w + sx];
        }
    });
    (ow, oh, out)
}

pub fn sidecar_path(neg: &Path) -> PathBuf {
    let stem = neg.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    neg.with_file_name(format!("{stem}.mask.tif"))
}

pub struct PrintOutput {
    pub plane: Plane,
    pub info: PrintInfo,
}

/// Full print rendering in memory.
pub fn render(neg_path: &Path, p: &PrintParams) -> Result<PrintOutput> {
    let t = Instant::now();
    let neg = read_gray(neg_path)?;
    let src_desc = neg.description.as_deref().and_then(|d| serde_json::from_str::<serde_json::Value>(d).ok());
    info!(
        "negative {}x{} {} bit, orientation {}",
        neg.plane.width, neg.plane.height, neg.bits, neg.orientation
    );

    let mut mask: Option<Plane> = None;
    if p.use_mask && p.sharpens() {
        let sp = sidecar_path(neg_path);
        match read_mask8(&sp) {
            Ok((mw, mh, data)) if mw == neg.plane.width && mh == neg.plane.height => {
                mask = Some(Plane::from_vec(mw, mh, data.iter().map(|&v| f64::from(v) / 255.0).collect()));
            }
            Ok(_) => warn!("mask sidecar {} has the wrong size; USM everywhere", sp.display()),
            Err(_) => warn!("no mask sidecar {}; USM everywhere", sp.display()),
        }
    }

    let source = PrintSource {
        plane: &neg.plane,
        mask: mask.as_ref(),
        orientation: neg.orientation,
        name: neg_path.display().to_string(),
        description: src_desc,
    };
    let mut out = render_planes(&source, p);
    out.info.ms = t.elapsed().as_millis();
    Ok(out)
}

/// A negative already in memory (GUI session, or the file just read).
pub struct PrintSource<'a> {
    pub plane: &'a Plane,
    /// Mask in [0,1], same size as `plane`; `None` = USM everywhere.
    pub mask: Option<&'a Plane>,
    pub orientation: u16,
    pub name: String,
    pub description: Option<serde_json::Value>,
}

/// Where the picture lands: its pixel size, the border around it, the sheet
/// (picture plus border), and the viewing model's numbers.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Layout {
    /// Picture size after the fit, pixels.
    pub image_w: usize,
    pub image_h: usize,
    /// Border on each side, pixels.
    pub border_px: usize,
    /// Output file size: picture plus twice the border.
    pub out_w: usize,
    pub out_h: usize,
    /// Viewing distance, mm (given, or the sheet diagonal).
    pub viewing_mm: f64,
    /// Long-side scale, destination / source. Above 1 when the picture is enlarged.
    pub enlargement: f64,
    /// Blur the resampler added, print pixels. Zero unless `enlargement > 1`.
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

/// Shift a full-image window into crop coordinates. The plane itself is unchanged.
fn window_frame<'a>(src: PlaneWindow<'a>, ox: usize, oy: usize, sw: usize, sh: usize) -> PlaneWindow<'a> {
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

/// Layout for a negative of `w×h` (before orientation). The border is taken
/// from the paper box before the fit, so picture plus border never exceeds
/// the size asked for.
pub fn layout(w: usize, h: usize, orientation: u16, p: &PrintParams) -> Layout {
    let r = &p.recipe;
    let (w, h) = if matches!(orientation, 5..=8) { (h, w) } else { (w, h) };
    // The fit and the enlargement are measured from the crop window.
    let (_, _, w, h) = source_window(w, h, r.crop);
    let b = r.border_px();
    let (image_w, image_h) = match r.size {
        Some(sz) => {
            let (bw, bh) = sz.pixels(r.dpi);
            // A border wider than the paper leaves one pixel of picture.
            fit_dims(w, h, bw.saturating_sub(2 * b).max(1), bh.saturating_sub(2 * b).max(1))
        }
        None => (w, h),
    };
    let (out_w, out_h) = (image_w + 2 * b, image_h + 2 * b);
    let viewing_mm = p.viewing_distance_mm.unwrap_or_else(|| {
        let diag_in = ((out_w as f64 / r.dpi).powi(2) + (out_h as f64 / r.dpi).powi(2)).sqrt();
        diag_in * 25.4
    });
    let enlargement = enlargement_scale(w, h, image_w, image_h);
    let enlargement_px = enlargement_sigma(w, h, image_w, image_h, r.resample);
    Layout {
        image_w,
        image_h,
        border_px: b,
        out_w,
        out_h,
        viewing_mm,
        enlargement,
        resample_sigma_px: enlargement_px,
        usm_radius_px: radius_px(p.usm_k, viewing_mm, r.dpi),
    }
}

/// Print geometry for a negative of `w×h` (before orientation): picture
/// pixel size, viewing distance (mm) and USM radius (px).
pub fn geometry(w: usize, h: usize, orientation: u16, p: &PrintParams) -> (usize, usize, f64, f64) {
    let l = layout(w, h, orientation, p);
    (l.image_w, l.image_h, l.viewing_mm, l.usm_radius_px)
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
    /// Box origin on the sheet (picture plus border).
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

fn loupe_geom(lay: &Layout, sharpen: Sharpen, nx: f64, ny: f64, view: usize) -> LoupeGeom {
    let (image_w, image_h, border) = (lay.image_w, lay.image_h, lay.border_px);
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
    let halo = if lay.resample_sigma_px > 0.0 && (passes > 0 || usm) {
        sharpen_halo(lay.resample_sigma_px, passes, usm)
    } else {
        0
    } as isize;
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
/// resampled with Lanczos-3 (wider than the other kernels) and that span is
/// united with the picture kernel, so one crop feeds both. `(0, 0, 0, 0)`
/// when the box lies entirely on the border.
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
    let g = loupe_geom(&lay, p.recipe.sharpen, nx, ny, view);
    if g.pic_w == 0 || g.pic_h == 0 {
        return (0, 0, 0, 0);
    }
    let span = |kernel| {
        let rect = PixelRect { x: g.pic_x, y: g.pic_y, w: g.pic_w, h: g.pic_h };
        crate::resize::source_coverage(sw, sh, g.image_w, g.image_h, kernel, rect)
    };
    let (x0, y0, x1, y1) = span(p.recipe.resample);
    let (x0, y0, x1, y1) = if p.recipe.sharpen.usm_amount() > 0.0 {
        let (mx0, my0, mx1, my1) = span(ResampleKernel::Lanczos3);
        (x0.min(mx0), y0.min(my0), x1.max(mx1), y1.max(my1))
    } else {
        (x0, y0, x1, y1)
    };
    (x0 + ox, y0 + oy, x1 + ox, y1 + oy)
}

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
) -> Plane {
    let (ox, oy, sw, sh) = source_window(linear.full_w, linear.full_h, p.recipe.crop);
    let lay = layout(linear.full_w, linear.full_h, 1, p);
    let g = loupe_geom(&lay, p.recipe.sharpen, nx, ny, view);
    let tone = p.recipe.curve.compile();
    let white = tone.white();
    let mut tile = Plane::from_vec(g.tile_w, g.tile_h, vec![white; g.tile_w * g.tile_h]);
    let mut mask_tile = mask.map(|_| Plane::zeros(g.tile_w, g.tile_h));
    // The plane window is in full-image coordinates. The resize maps the
    // crop, so its coordinate system is the window.
    let linear = window_frame(linear, ox, oy, sw, sh);
    let mask = mask.map(|m| window_frame(m, ox, oy, sw, sh));
    if g.pic_w > 0 && g.pic_h > 0 {
        let pic = PixelRect { x: g.pic_x, y: g.pic_y, w: g.pic_w, h: g.pic_h };
        let mut picture = resize_rect(linear, g.image_w, g.image_h, p.recipe.resample, pic);
        crate::tone::render(&mut picture, p.recipe.place, &tone);
        let ox = g.pic_x + g.border - g.tile_x;
        let oy = g.pic_y + g.border - g.tile_y;
        for y in 0..g.pic_h {
            tile.row_mut(oy + y)[ox..ox + g.pic_w].copy_from_slice(picture.row(y));
        }
        // The print resamples the mask with Lanczos-3, which is wider than
        // Mitchell, Catmull–Rom and the B-spline. Only do it when a USM will
        // read the mask: the crop from `loupe_source_rect` includes those
        // taps only then. Resampling it anyway reads off the window.
        if p.recipe.sharpen.usm_amount() > 0.0 {
            if let (Some(m), Some(dst)) = (mask, mask_tile.as_mut()) {
                let resampled = resize_rect(m, g.image_w, g.image_h, ResampleKernel::Lanczos3, pic);
                for y in 0..g.pic_h {
                    dst.row_mut(oy + y)[ox..ox + g.pic_w].copy_from_slice(resampled.row(y));
                }
            }
        }
    }
    tile = apply_sharpen(tile, mask_tile.as_ref(), &p.recipe, &lay).0;
    let mut out = Plane::zeros(g.view_w, g.view_h);
    let ox = g.view_x - g.tile_x;
    let oy = g.view_y - g.tile_y;
    for y in 0..g.view_h {
        out.row_mut(y).copy_from_slice(&tile.row(oy + y)[ox..ox + g.view_w]);
    }
    out
}

/// Surround `p` with `b` pixels of `value` on every side. The mask gets 0
/// there (nothing to sharpen on paper).
pub fn add_border(p: &Plane, b: usize, value: f64) -> Plane {
    if b == 0 {
        return p.clone();
    }
    let (w, h) = (p.width + 2 * b, p.height + 2 * b);
    let mut out = Plane::from_vec(w, h, vec![value; w * h]);
    out.data.par_chunks_mut(w).enumerate().skip(b).take(p.height).for_each(|(y, row)| {
        row[b..b + p.width].copy_from_slice(p.row(y - b));
    });
    out
}

/// Richardson–Lucy and USM at the enlargement σ. A width of zero leaves
/// the plane as it is.
fn apply_sharpen(mut plane: Plane, mask: Option<&Plane>, recipe: &Recipe, lay: &Layout) -> (Plane, usize) {
    let detail = lay.resample_sigma_px;
    let mut deconv_n = 0usize;
    let passes = recipe.sharpen.deconv_passes() as usize;
    if passes > 0 && detail > 0.0 {
        deconv_n = passes.min(DECONV_ITERATIONS);
        plane = richardson_lucy(&plane, detail, deconv_n);
    }
    let amount = recipe.sharpen.usm_amount();
    if amount > 0.0 && detail > 0.0 {
        plane = unsharp(&plane, detail, amount, mask);
    }
    (plane, deconv_n)
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
            // The mask is a weight field; a plain resample is enough.
            mask = Some(resize_with(&m, tw, th, ResampleKernel::Lanczos3));
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

    if lay.border_px > 0 {
        info!("border {} px ({} mm) in paper white {:.4}", lay.border_px, r.border_mm, tone.white());
        plane = add_border(&plane, lay.border_px, tone.white());
        if let Some(m) = mask.take() {
            mask = Some(add_border(&m, lay.border_px, 0.0));
        }
    }

    let (plane, deconv_n) = apply_sharpen(plane, mask.as_ref(), r, &lay);
    if deconv_n > 0 {
        info!(
            "richardson-lucy {deconv_n} iterations, enlargement sigma {:.2} px, no mask",
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
        usm_amount: r.sharpen.usm_amount(),
        usm_radius_px: lay.usm_radius_px,
        resample_sigma_px: lay.resample_sigma_px,
        usm_masked: mask.is_some(),
        deconvolution_iterations: deconv_n as u32,
        ms: t.elapsed().as_millis(),
    };
    debug_assert_eq!((plane.width, plane.height), (lay.out_w, lay.out_h));
    PrintOutput { plane, info }
}

/// Write the print: a 16-bit TIFF (grey ICC) unless `path` ends in
/// `.jpg`/`.jpeg`, in which case an 8-bit JPEG is written (the mobile
/// app's album; the desktop writes TIFF).
pub fn write(out: &PrintOutput, path: &Path, p: &PrintParams) -> Result<()> {
    let desc = serde_json::to_string(&out.info)?;
    let trc = match p.recipe.curve.encoding() {
        crate::calib::LookEncoding::Gamma22 => GrayTrc::Gamma(2.2),
        crate::calib::LookEncoding::None => GrayTrc::Linear,
    };
    if is_jpeg_path(path) {
        write_jpeg(&out.plane, path, trc)?;
    } else {
        write_gray16(
            path,
            &out.plane,
            &TiffMeta { description: &desc, trc, orientation: 1, dpi: Some(p.recipe.dpi) },
        )?;
    }
    Ok(())
}

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
        assert!(PrintSize::parse("2048px").is_err());
        assert_eq!(PrintSize::parse("100x100mm").unwrap().pixels(254.0), (1000, 1000));
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
            );
            let g = loupe_geom(&lay, params.recipe.sharpen, nx, ny, 11);
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
                        );
                        let g = loupe_geom(&lay, sharpen, nx, ny, 16);
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
        assert!((peak_at as isize - edge as isize).abs() <= 6, "peak at {peak_at}, edge {edge}");
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
}
