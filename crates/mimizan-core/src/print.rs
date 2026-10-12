//! Negative -> print file (SPEC section 9, Zonensystem paper section 7): the
//! one pass. Upright -> size -> place and dodge & burn -> curve -> border ->
//! [Richardson–Lucy ->] USM or screen compensation -> one quantisation ->
//! file. The mix already happened when the negative was written. Everything
//! the pass does is in the [`Recipe`]; [`PrintParams`] adds what is not a
//! print decision (viewing model constant, sidecar use, proof path).

use crate::deconv::{richardson_lucy, DECONV_ITERATIONS};
use crate::error::{Error, Result};
use crate::icc::GrayTrc;
use crate::plane::Plane;
use crate::recipe::{Recipe, Sharpen};
use crate::resize::{fit_dims, resize_with, ResampleKernel};
use crate::screen;
use crate::tiffin::{read_gray, read_mask8};
use crate::tiffout::{write_gray16, TiffMeta};
use crate::usm::{radius_px, unsharp, USM_K};
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{info, warn};

/// Output size: a physical print, or a pixel box for the screen.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PrintSize {
    Physical {
        width_in: f64,
        height_in: f64,
    },
    /// Longest edge in pixels (the image is fitted into a square box).
    LongEdgePx(usize),
}

impl PrintSize {
    /// `"30x40cm"`, `"12x16in"`, `"8.5x11in"`, or `"2048px"` (long edge).
    pub fn parse(s: &str) -> Result<Self> {
        let t = s.trim().to_ascii_lowercase();
        if let Some(n) = t.strip_suffix("px") {
            let n: usize = n
                .trim()
                .parse()
                .map_err(|_| Error::Invalid(format!("size '{s}': pixel size must be an integer")))?;
            if n == 0 {
                return Err(Error::Invalid("size must be positive".into()));
            }
            return Ok(Self::LongEdgePx(n));
        }
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
        match *self {
            Self::Physical { width_in, height_in } => {
                ((width_in * dpi).round().max(1.0) as usize, (height_in * dpi).round().max(1.0) as usize)
            }
            Self::LongEdgePx(n) => (n, n),
        }
    }

    pub fn is_pixels(&self) -> bool {
        matches!(self, Self::LongEdgePx(_))
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
    pub proof_jpeg: Option<PathBuf>,
}

impl Default for PrintParams {
    fn default() -> Self {
        Self {
            recipe: Recipe::default(),
            usm_k: USM_K,
            viewing_distance_mm: None,
            use_mask: true,
            proof_jpeg: None,
        }
    }
}

impl PrintParams {
    /// Just a look, everything else default (the GUI's TIFF export).
    pub fn with_look(look: crate::calib::LookFile) -> Self {
        Self { recipe: Recipe::with_look(look), ..Self::default() }
    }

    /// USM or screen compensation, both of which consult the mask.
    /// Deconvolution does not: it runs on the whole picture, and USM may follow it.
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
    /// curve nodes, shapes, kernel, size, border, sharpening mode.
    pub recipe: Recipe,
    /// Border in pixels on each side, when there is one.
    #[serde(default, skip_serializing_if = "is_zero_usize")]
    pub border_px: usize,
    pub usm_amount: f64,
    pub usm_radius_px: f64,
    pub usm_masked: bool,
    /// Richardson–Lucy iterations when that path ran; omitted when it did not.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub deconvolution_iterations: u32,
    /// Screen compensation, when used: amount, gain cap, the 7 taps and
    /// the resulting response at 0.25 and 0.4 c/px.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub screen: Option<ScreenInfo>,
    pub ms: u128,
}

#[derive(Clone, Debug, Serialize)]
pub struct ScreenInfo {
    pub amount: f64,
    pub max_gain: f64,
    pub downscaled: bool,
    pub taps: Vec<f64>,
    pub response_025: f64,
    pub response_040: f64,
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
/// `orient(_, orientation)`: the inverse of the lookup inside `orient`.
#[inline]
pub fn upright_pixel(orientation: u16, w: usize, h: usize, sx: usize, sy: usize) -> (usize, usize) {
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
    /// USM radius, pixels.
    pub usm_radius_px: f64,
}

/// Layout for a negative of `w×h` (before orientation). The border is taken
/// from the paper box before the fit, so picture plus border never exceeds
/// the size asked for.
pub fn layout(w: usize, h: usize, orientation: u16, p: &PrintParams) -> Layout {
    let r = &p.recipe;
    let (w, h) = if matches!(orientation, 5..=8) { (h, w) } else { (w, h) };
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
    Layout {
        image_w,
        image_h,
        border_px: b,
        out_w,
        out_h,
        viewing_mm,
        usm_radius_px: radius_px(p.usm_k, viewing_mm, r.dpi),
    }
}

/// Print geometry for a negative of `w×h` (before orientation): picture
/// pixel size, viewing distance (mm) and USM radius (px).
pub fn geometry(w: usize, h: usize, orientation: u16, p: &PrintParams) -> (usize, usize, f64, f64) {
    let l = layout(w, h, orientation, p);
    (l.image_w, l.image_h, l.viewing_mm, l.usm_radius_px)
}

/// Is the picture smaller than the upright source (so the kernel's roll-off
/// is part of the known loss)?
pub fn is_downscale(w: usize, h: usize, orientation: u16, p: &PrintParams) -> bool {
    let l = layout(w, h, orientation, p);
    let upright_w = if matches!(orientation, 5..=8) { h } else { w };
    l.image_w < upright_w
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

    let lay = layout(src.plane.width, src.plane.height, src.orientation, p);
    let (tw, th, d_mm, r_px) = (lay.image_w, lay.image_h, lay.viewing_mm, lay.usm_radius_px);
    let downscaled = if tw < plane.width { Some(r.resample) } else { None };
    if (tw, th) != (plane.width, plane.height) {
        info!("resize {}x{} -> {}x{} ({})", plane.width, plane.height, tw, th, r.resample.name());
        plane = resize_with(&plane, tw, th, r.resample);
        if let Some(m) = mask.take() {
            // The mask is a weight field; a plain resample is enough.
            mask = Some(resize_with(&m, tw, th, ResampleKernel::Lanczos3));
        }
    }

    // Placing, dodge & burn and the tone in one pass: local stops on the
    // linear luminance, then paper or look.
    if r.place != 0.0 || r.dodge_burn.iter().any(|b| b.ev() != 0.0) {
        info!("place {:+.2} stops, {} dodge & burn shapes", r.place, r.dodge_burn.len());
    }
    let tone = r.curve.compile();
    info!("tone {}", r.curve.name());
    crate::tone::render(&mut plane, r.place, &r.dodge_burn, &tone);

    if lay.border_px > 0 {
        info!("border {} px ({} mm) in paper white {:.4}", lay.border_px, r.border_mm, tone.white());
        plane = add_border(&plane, lay.border_px, tone.white());
        if let Some(m) = mask.take() {
            mask = Some(add_border(&m, lay.border_px, 0.0));
        }
    }

    let mut screen_info = None;
    let mut deconv_n = 0usize;
    match r.sharpen {
        Sharpen::Screen { amount, max_gain } if amount > 0.0 => {
            let s = screen::ScreenSharpen { amount, max_gain };
            let h = screen::kernel(&s, downscaled);
            info!(
                "screen compensation amount {:.2}, gain cap {:.1}, downscaled {}, response 0.25 c/px {:.2}, 0.40 c/px {:.2}, masked: {}",
                amount,
                max_gain,
                downscaled.is_some(),
                screen::response(&h, 0.25),
                screen::response(&h, 0.40),
                mask.is_some()
            );
            plane = screen::sharpen(&plane, &h, mask.as_ref());
            screen_info = Some(ScreenInfo {
                amount,
                max_gain,
                downscaled: downscaled.is_some(),
                taps: h.to_vec(),
                response_025: screen::response(&h, 0.25),
                response_040: screen::response(&h, 0.40),
            });
        }
        Sharpen::Screen { .. } | Sharpen::Off => {}
        Sharpen::Usm { .. } | Sharpen::Deconv { .. } => {
            let passes = r.sharpen.deconv_passes();
            if passes > 0 && r_px > 0.0 {
                deconv_n = (passes as usize).clamp(1, DECONV_ITERATIONS);
                info!("richardson-lucy {deconv_n} iterations, sigma {r_px:.2} px, no mask");
                plane = richardson_lucy(&plane, r_px, deconv_n);
            }
            let amount = r.sharpen.usm_amount();
            if amount > 0.0 {
                info!(
                    "usm amount {:.2}, radius {:.2} px (d = {:.0} mm), masked: {}",
                    amount,
                    r_px,
                    d_mm,
                    mask.is_some()
                );
                plane = unsharp(&plane, r_px, amount, mask.as_ref());
            }
        }
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
        usm_radius_px: r_px,
        usm_masked: mask.is_some(),
        deconvolution_iterations: deconv_n as u32,
        screen: screen_info,
        ms: t.elapsed().as_millis(),
    };
    debug_assert_eq!((plane.width, plane.height), (lay.out_w, lay.out_h));
    PrintOutput { plane, info }
}

/// Write the print: a 16-bit TIFF (grey ICC) unless `path` ends in
/// `.jpg`/`.jpeg`, in which case only the JPEG is written; plus the optional
/// JPEG proof.
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
    if let Some(jp) = &p.proof_jpeg {
        if jp != path {
            write_jpeg(&out.plane, jp, trc)?;
        }
    }
    Ok(())
}

pub fn is_jpeg_path(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
        Some("jpg" | "jpeg")
    )
}

/// JPEG proof of an already encoded plane: full print resolution, 8-bit grey
/// (one channel, so no chroma subsampling), quality 100, the same grey ICC
/// as the TIFF embedded.
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
        let PrintSize::Physical { width_in, .. } = s else { panic!() };
        assert!((width_in - 11.811).abs() < 1e-3);
        assert_eq!(s.pixels(300.0), (3543, 4724));
        assert_eq!(PrintSize::parse("8x10in").unwrap().pixels(300.0), (2400, 3000));
        assert!(PrintSize::parse("30x40").is_err());
        assert!(PrintSize::parse("0x40cm").is_err());
        let px = PrintSize::parse("2048px").unwrap();
        assert_eq!(px, PrintSize::LongEdgePx(2048));
        assert!(px.is_pixels());
        assert_eq!(px.pixels(300.0), (2048, 2048));
        assert!(PrintSize::parse("0px").is_err());
        assert!(PrintSize::parse("2048.5px").is_err());
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
        // 400 mm at 300 DPI is the spec's 1.4 px USM radius.
        let plain = PrintParams { viewing_distance_mm: Some(400.0), ..Default::default() };
        let base = render_planes(&src, &plain);
        let deconv = PrintParams {
            recipe: Recipe {
                sharpen: Sharpen::Deconv { passes: crate::deconv::DECONV_ITERATIONS as u32, usm_amount: 0.0 },
                ..Recipe::default()
            },
            viewing_distance_mm: Some(400.0),
            ..Default::default()
        };
        let out = render_planes(&src, &deconv);
        assert_eq!(out.info.deconvolution_iterations, crate::deconv::DECONV_ITERATIONS as u32);
        assert!(!out.info.usm_masked);
        let contrast = |p: &Plane| p.at(32, 16) - p.at(31, 16);
        assert!(
            contrast(&out.plane) > contrast(&base.plane),
            "deconv {:.4} base {:.4}",
            contrast(&out.plane),
            contrast(&base.plane)
        );
        let usm = PrintParams {
            recipe: Recipe { sharpen: Sharpen::Usm { amount: 1.0 }, ..Recipe::default() },
            viewing_distance_mm: Some(400.0),
            ..Default::default()
        };
        let masked = render_planes(&src, &usm);
        assert_eq!(masked.plane.data, base.plane.data);
    }

    fn sized(size: PrintSize) -> PrintParams {
        PrintParams { recipe: Recipe { size: Some(size), ..Recipe::default() }, ..Default::default() }
    }

    #[test]
    fn downscale_detection_respects_orientation() {
        let p = sized(PrintSize::LongEdgePx(1000));
        assert!(is_downscale(4000, 3000, 1, &p));
        assert!(is_downscale(4000, 3000, 6, &p));
        let big = sized(PrintSize::LongEdgePx(8000));
        assert!(!is_downscale(4000, 3000, 1, &big));
        assert!(is_jpeg_path(Path::new("a/b.JPG")) && !is_jpeg_path(Path::new("a/b.tif")));
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

    /// Paper, table 4: place = 0 and empty shapes are the run that never had
    /// the step; the same recipe twice is the same picture; paper at its
    /// base exposure is a different picture from the neutral look.
    #[test]
    fn untouched_recipe_is_bit_identical_to_neutral() {
        let (w, h) = (48, 32);
        let plane = Plane::from_vec(w, h, (0..w * h).map(|i| (i % 97) as f64 / 96.0).collect());
        let src =
            PrintSource { plane: &plane, mask: None, orientation: 6, name: "ramp".into(), description: None };
        let neutral = render_planes(&src, &PrintParams::default());
        let with_shapes = PrintParams {
            recipe: Recipe {
                place: 0.0,
                dodge_burn: vec![crate::burn::Burn::radial(0.5, 0.5, 0.4, 0.5, 0.0)],
                ..Recipe::default()
            },
            ..Default::default()
        };
        assert_eq!(render_planes(&src, &with_shapes).plane.data, neutral.plane.data);
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
}
