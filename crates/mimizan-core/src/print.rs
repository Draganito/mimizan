//! Negative -> print file (SPEC section 9): size -> curve -> USM (or screen
//! compensation) -> file. The mix already happened when the negative was
//! written.

use crate::calib::LookFile;
use crate::curve::Curve;
use crate::deconv::{richardson_lucy, DECONV_ITERATIONS};
use crate::error::{Error, Result};
use crate::icc::GrayTrc;
use crate::plane::Plane;
use crate::resize::{fit_dims, resize};
use crate::screen::{self, ScreenSharpen};
use crate::tiffin::{read_gray, read_mask8};
use crate::tiffout::{write_gray16, TiffMeta};
use crate::usm::{radius_px, unsharp, USM_K};
use rayon::prelude::*;
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{info, warn};

/// Output size: a physical print, or a pixel box for the screen.
#[derive(Clone, Copy, Debug, PartialEq)]
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
    pub size: Option<PrintSize>,
    pub dpi: f64,
    pub look: LookFile,
    pub usm_amount: f64,
    pub usm_k: f64,
    /// Viewing distance for the USM radius, mm. `None` = print diagonal.
    pub viewing_distance_mm: Option<f64>,
    /// Use the `.mask.tif` sidecar to restrict USM.
    pub use_mask: bool,
    pub proof_jpeg: Option<PathBuf>,
    /// Screen compensation instead of USM (see `screen.rs`).
    pub screen: Option<ScreenSharpen>,
    /// Richardson–Lucy instead of USM, on every pixel, no mask.
    /// Screen compensation still wins when `screen` is set.
    pub deconvolution: bool,
    /// Passes when `deconvolution` is set (1..10). Zero skips it.
    pub deconv_iterations: u32,
}

impl Default for PrintParams {
    fn default() -> Self {
        Self {
            size: None,
            dpi: 300.0,
            look: LookFile::neutral(),
            usm_amount: 0.0,
            usm_k: USM_K,
            viewing_distance_mm: None,
            use_mask: true,
            proof_jpeg: None,
            screen: None,
            deconvolution: false,
            deconv_iterations: 0,
        }
    }
}

impl PrintParams {
    /// USM or screen compensation, both of which consult the mask.
    /// Deconvolution does not: it runs on the whole picture, and USM may follow it.
    pub fn sharpens(&self) -> bool {
        match self.screen {
            Some(s) => s.amount > 0.0,
            None => self.usm_amount > 0.0,
        }
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

/// Print geometry for a negative of `w×h` (before orientation): final pixel
/// size, viewing distance (mm) and USM radius (px).
pub fn geometry(w: usize, h: usize, orientation: u16, p: &PrintParams) -> (usize, usize, f64, f64) {
    let (w, h) = if matches!(orientation, 5..=8) { (h, w) } else { (w, h) };
    let (tw, th) = match p.size {
        Some(sz) => {
            let (bw, bh) = sz.pixels(p.dpi);
            fit_dims(w, h, bw, bh)
        }
        None => (w, h),
    };
    let d_mm = p.viewing_distance_mm.unwrap_or_else(|| {
        let diag_in = ((tw as f64 / p.dpi).powi(2) + (th as f64 / p.dpi).powi(2)).sqrt();
        diag_in * 25.4
    });
    (tw, th, d_mm, radius_px(p.usm_k, d_mm, p.dpi))
}

/// Is the output smaller than the upright source (so the Lanczos-3 roll-off
/// is part of the known loss)?
pub fn is_downscale(w: usize, h: usize, orientation: u16, p: &PrintParams) -> bool {
    let (tw, _th, _d, _r) = geometry(w, h, orientation, p);
    let upright_w = if matches!(orientation, 5..=8) { h } else { w };
    tw < upright_w
}

/// Size, curve and USM on in-memory planes. `info.ms` covers this call only.
pub fn render_planes(src: &PrintSource<'_>, p: &PrintParams) -> PrintOutput {
    let t = Instant::now();
    let mut mask: Option<Plane> = None;
    // Upright first, so the fit uses the displayed aspect ratio.
    let mut plane = orient(src.plane, src.orientation);
    if p.sharpens() {
        if let Some(m) = src.mask {
            mask = Some(orient(m, src.orientation));
        }
    }

    let (tw, th, d_mm, r_px) = geometry(src.plane.width, src.plane.height, src.orientation, p);
    let downscaled = tw < plane.width;
    if (tw, th) != (plane.width, plane.height) {
        info!("resize {}x{} -> {}x{} (Lanczos-3)", plane.width, plane.height, tw, th);
        plane = resize(&plane, tw, th);
        if let Some(m) = mask.take() {
            // The mask is a weight field; a plain resample is enough.
            mask = Some(resize(&m, tw, th));
        }
    }

    let curve = Curve::from_look(&p.look);
    let mut plane = curve.apply(&plane);

    let mut screen_info = None;
    match p.screen {
        Some(s) if s.amount > 0.0 => {
            let h = screen::kernel(&s, downscaled);
            info!(
                "screen compensation amount {:.2}, gain cap {:.1}, downscaled {}, response 0.25 c/px {:.2}, 0.40 c/px {:.2}, masked: {}",
                s.amount,
                s.max_gain,
                downscaled,
                screen::response(&h, 0.25),
                screen::response(&h, 0.40),
                mask.is_some()
            );
            plane = screen::sharpen(&plane, &h, mask.as_ref());
            screen_info = Some(ScreenInfo {
                amount: s.amount,
                max_gain: s.max_gain,
                downscaled,
                taps: h.to_vec(),
                response_025: screen::response(&h, 0.25),
                response_040: screen::response(&h, 0.40),
            });
        }
        Some(_) => {}
        None => {
            if p.deconvolution && p.deconv_iterations > 0 && r_px > 0.0 {
                let n = (p.deconv_iterations as usize).clamp(1, DECONV_ITERATIONS);
                info!("richardson-lucy {n} iterations, sigma {r_px:.2} px, no mask");
                plane = richardson_lucy(&plane, r_px, n);
            }
            if p.usm_amount > 0.0 {
                info!(
                    "usm amount {:.2}, radius {:.2} px (d = {:.0} mm), masked: {}",
                    p.usm_amount,
                    r_px,
                    d_mm,
                    mask.is_some()
                );
                plane = unsharp(&plane, r_px, p.usm_amount, mask.as_ref());
            }
        }
    }
    let deconv_n = if p.screen.is_none() && p.deconvolution && p.deconv_iterations > 0 && r_px > 0.0 {
        (p.deconv_iterations as usize).clamp(1, DECONV_ITERATIONS)
    } else {
        0
    };
    let usm_amount = if p.screen.is_some() { 0.0 } else { p.usm_amount };

    let info = PrintInfo {
        mimizan: env!("CARGO_PKG_VERSION"),
        stage: "print",
        source: src.name.clone(),
        source_description: src.description.clone(),
        look: p.look.name.clone(),
        look_encoding: format!("{:?}", p.look.encoding).to_ascii_lowercase(),
        width: tw,
        height: th,
        dpi: p.dpi,
        orientation_applied: src.orientation,
        usm_amount,
        usm_radius_px: r_px,
        usm_masked: mask.is_some(),
        deconvolution_iterations: deconv_n as u32,
        screen: screen_info,
        ms: t.elapsed().as_millis(),
    };
    PrintOutput { plane, info }
}

/// Write the print: a 16-bit TIFF (grey ICC) unless `path` ends in
/// `.jpg`/`.jpeg`, in which case only the JPEG is written; plus the optional
/// JPEG proof.
pub fn write(out: &PrintOutput, path: &Path, p: &PrintParams) -> Result<()> {
    let desc = serde_json::to_string(&out.info)?;
    let trc = match p.look.encoding {
        crate::calib::LookEncoding::Gamma22 => GrayTrc::Gamma(2.2),
        crate::calib::LookEncoding::None => GrayTrc::Linear,
    };
    if is_jpeg_path(path) {
        write_jpeg(&out.plane, path, trc)?;
    } else {
        write_gray16(
            path,
            &out.plane,
            &TiffMeta { description: &desc, trc, orientation: 1, dpi: Some(p.dpi) },
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
            deconvolution: true,
            deconv_iterations: crate::deconv::DECONV_ITERATIONS as u32,
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
        let usm = PrintParams { usm_amount: 1.0, viewing_distance_mm: Some(400.0), ..Default::default() };
        let masked = render_planes(&src, &usm);
        assert_eq!(masked.plane.data, base.plane.data);
    }

    #[test]
    fn downscale_detection_respects_orientation() {
        let p = PrintParams { size: Some(PrintSize::LongEdgePx(1000)), ..Default::default() };
        assert!(is_downscale(4000, 3000, 1, &p));
        assert!(is_downscale(4000, 3000, 6, &p));
        let big = PrintParams { size: Some(PrintSize::LongEdgePx(8000)), ..Default::default() };
        assert!(!is_downscale(4000, 3000, 1, &big));
        assert!(is_jpeg_path(Path::new("a/b.JPG")) && !is_jpeg_path(Path::new("a/b.tif")));
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
}
