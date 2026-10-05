use anyhow::{bail, Context, Result};
use mimizan_core::calib::CameraFile;
use mimizan_core::decode::{RawDecoder, RawlerDecoder, SensorKind};
use mimizan_core::icc::GrayTrc;
use mimizan_core::ingest::{ingest, IngestParams, WhiteBalance};
use mimizan_core::mix::{ColorFilter, WeightSpace, Weights};
use mimizan_core::pipeline::{self, NegativeParams};
use mimizan_core::separate::MaskMode;
use mimizan_core::tiffout::{write_gray16, write_gray8, TiffMeta};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{info, warn};

pub struct NegativeArgs {
    pub raw: PathBuf,
    pub out: PathBuf,
    pub cameras: PathBuf,
    pub camera: Option<PathBuf>,
    pub mask: String,
    pub weights: Option<String>,
    pub weights_space: String,
    pub filter: Option<String>,
    pub wb: String,
    pub cutoff: f64,
    pub rounds: usize,
    pub reconstruct: bool,
    pub fix_defects: bool,
    pub preview: Option<PathBuf>,
    pub preview_scale: usize,
    pub crop: Option<String>,
}

pub fn parse_weights(s: &str) -> Result<Weights> {
    let v: Vec<f64> = s.split(',').map(|t| t.trim().parse::<f64>()).collect::<std::result::Result<_, _>>()?;
    if v.len() != 3 {
        bail!("weights need three values r,g,b");
    }
    let w = Weights { r: v[0], g: v[1], b: v[2] };
    w.validate()?;
    Ok(w)
}

pub fn parse_weight_space(s: &str) -> Result<WeightSpace> {
    match s.to_ascii_lowercase().as_str() {
        "balanced" | "bal" => Ok(WeightSpace::Balanced),
        "raw" | "sensor" => Ok(WeightSpace::Raw),
        other => bail!("unknown weight space '{other}' (balanced|raw)"),
    }
}

pub fn parse_wb(s: &str) -> Result<WhiteBalance> {
    match s.to_ascii_lowercase().as_str() {
        "as-shot" | "asshot" | "camera" => Ok(WhiteBalance::AsShot),
        "gray" | "grey" | "gray-world" => Ok(WhiteBalance::GrayWorld),
        "none" | "off" => Ok(WhiteBalance::None),
        other => {
            let v: Vec<f64> = other
                .split(',')
                .map(|t| t.trim().parse::<f64>())
                .collect::<std::result::Result<_, _>>()
                .with_context(|| format!("white balance '{other}' is not as-shot|gray|none|r,g,b"))?;
            if v.len() != 3 || v.iter().any(|x| *x <= 0.0) {
                bail!("manual white balance needs three positive values r,g,b");
            }
            Ok(WhiteBalance::Manual([v[0], v[1], v[2]]))
        }
    }
}

pub fn parse_mask(s: &str) -> Result<MaskMode> {
    match s.to_ascii_lowercase().as_str() {
        "off" => Ok(MaskMode::Off),
        "adaptive" | "on" => Ok(MaskMode::Adaptive),
        "dubois" => Ok(MaskMode::Dubois),
        other => bail!("unknown mask mode '{other}' (dubois|adaptive|off)"),
    }
}

pub fn sidecar_path(out: &Path) -> PathBuf {
    let stem = out.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    out.with_file_name(format!("{stem}.mask.tif"))
}

pub fn negative(a: &NegativeArgs) -> Result<()> {
    let t_all = Instant::now();
    let t = Instant::now();
    let frame = pipeline::decode(&a.raw).with_context(|| format!("decoding {}", a.raw.display()))?;
    let decode_ms = t.elapsed().as_millis();
    info!("decode {} ms", decode_ms);

    let mut params = NegativeParams::default();
    params.ingest.fix_defects = a.fix_defects;
    params.ingest.wb = parse_wb(&a.wb)?;
    params.separate.mask = parse_mask(&a.mask)?;
    params.separate.cutoff = a.cutoff;
    params.separate.rounds_max = a.rounds;
    if a.reconstruct {
        params.separate.reconstruct = Some(Default::default());
    }

    let cam_path = match &a.camera {
        Some(p) => Some(p.clone()),
        None => CameraFile::find(&a.cameras, &frame.clean_make, &frame.clean_model),
    };
    match cam_path {
        Some(p) => {
            let cam = CameraFile::load(&p).with_context(|| format!("camera file {}", p.display()))?;
            info!("camera file {}", p.display());
            params = params.with_camera_file(cam);
        }
        None => info!("no camera file for '{} {}', using defaults", frame.clean_make, frame.clean_model),
    }
    if let Some(w) = &a.weights {
        params = params.with_weights(parse_weights(w)?, parse_weight_space(&a.weights_space)?);
    }
    if let Some(f) = &a.filter {
        params.filter = ColorFilter::parse(f).ok_or_else(|| {
            let names: Vec<&str> = ColorFilter::ALL.iter().map(|f| f.name()).collect();
            anyhow::anyhow!("unknown filter '{f}' ({})", names.join("|"))
        })?;
    }

    let neg = pipeline::negative_from_frame(&frame, &params, decode_ms)?;
    if neg.info.reconstruct {
        info!("reconstruct on: {:.1} % of the picture computed, not measured", 100.0 * neg.info.computed);
    }
    let desc = serde_json::to_string(&neg.info)?;
    let t = Instant::now();
    write_gray16(
        &a.out,
        &neg.plane,
        &TiffMeta { description: &desc, trc: GrayTrc::Linear, orientation: neg.orientation, dpi: None },
    )?;
    let side = sidecar_path(&a.out);
    write_gray8(
        &side,
        neg.plane.width,
        neg.plane.height,
        &neg.mask8,
        "mimizan mask: 255*max(M), 255=saturated",
    )?;
    info!("wrote {} + {} in {} ms", a.out.display(), side.display(), t.elapsed().as_millis());

    if let Some(p) = &a.preview {
        mimizan_core::preview::write_png(&neg.plane, p, a.preview_scale, true)?;
        if let Some(c) = &a.crop {
            let v: Vec<usize> =
                c.split(',').map(|t| t.trim().parse::<usize>()).collect::<std::result::Result<_, _>>()?;
            if v.len() != 4 {
                bail!("crop needs x,y,w,h");
            }
            let cp = p.with_extension("crop.png");
            mimizan_core::preview::write_png_crop(&neg.plane, &cp, v[0], v[1], v[2], v[3], true)?;
            info!("crop {}", cp.display());
        }
        info!("preview {}", p.display());
    }
    if neg.info.report.defects as f64 > 0.001 * neg.plane.data.len() as f64 {
        warn!(
            "defect rate {:.3} % is high; check the noise model",
            100.0 * neg.info.report.defects as f64 / neg.plane.data.len() as f64
        );
    }
    info!("total {} ms", t_all.elapsed().as_millis());
    Ok(())
}

pub struct PrintArgs {
    pub negative: PathBuf,
    pub out: PathBuf,
    pub look: Option<PathBuf>,
    pub size: Option<String>,
    pub screen: Option<f64>,
    pub screen_max_gain: f64,
    pub dpi: f64,
    pub usm_amount: Option<f64>,
    pub usm_k: f64,
    pub distance_mm: Option<f64>,
    pub use_mask: bool,
    pub deconv: Option<u32>,
    pub cameras: PathBuf,
    pub proof: Option<PathBuf>,
}

/// USM default: the camera file of the camera named in the negative's description.
fn usm_default_from_negative(negative: &Path, cameras: &Path) -> Option<f64> {
    let g = mimizan_core::tiffin::read_gray_header(negative).ok()?;
    let v: serde_json::Value = serde_json::from_str(g.as_deref()?).ok()?;
    let cam = v.get("camera")?;
    let (make, model) = (cam.get("clean_make")?.as_str()?, cam.get("clean_model")?.as_str()?);
    let p = CameraFile::find(cameras, make, model)?;
    let c = CameraFile::load(&p).ok()?;
    info!("usm amount {} from {}", c.usm_amount, p.display());
    Some(c.usm_amount)
}

pub fn print(a: &PrintArgs) -> Result<()> {
    use mimizan_core::calib::LookFile;
    use mimizan_core::print::{self as pr, PrintParams, PrintSize};
    use mimizan_core::screen::ScreenSharpen;
    if a.dpi.is_nan() || a.dpi <= 0.0 {
        bail!("dpi must be positive");
    }
    let look = match &a.look {
        Some(p) => LookFile::load(p).with_context(|| format!("look file {}", p.display()))?,
        None => LookFile::neutral(),
    };
    let size = a.size.as_deref().map(PrintSize::parse).transpose()?;
    let usm_amount = match a.usm_amount {
        Some(v) => v,
        None => usm_default_from_negative(&a.negative, &a.cameras).unwrap_or(0.0),
    };
    if !(0.0..=1.5).contains(&usm_amount) {
        bail!("usm amount must be within 0..1.5");
    }
    let screen = match a.screen {
        Some(amount) => {
            if !(0.0..=1.0).contains(&amount) {
                bail!("screen amount must be within 0..1");
            }
            if !(1.0..=4.0).contains(&a.screen_max_gain) {
                bail!("screen max gain must be within 1..4");
            }
            Some(ScreenSharpen { amount, max_gain: a.screen_max_gain })
        }
        None => None,
    };
    if let Some(n) = a.deconv {
        if !(1..=mimizan_core::deconv::DECONV_ITERATIONS as u32).contains(&n) {
            bail!("deconv passes must be within 1..{}", mimizan_core::deconv::DECONV_ITERATIONS);
        }
        if screen.is_some() {
            bail!("--deconv and --screen exclude each other");
        }
    }
    let p = PrintParams {
        size,
        dpi: a.dpi,
        look,
        usm_amount,
        usm_k: a.usm_k,
        viewing_distance_mm: a.distance_mm,
        use_mask: a.use_mask,
        proof_jpeg: a.proof.clone(),
        screen,
        deconvolution: a.deconv.is_some(),
        deconv_iterations: a.deconv.unwrap_or(0),
    };
    let out = pr::render(&a.negative, &p).with_context(|| format!("rendering {}", a.negative.display()))?;
    let t = Instant::now();
    pr::write(&out, &a.out, &p)?;
    if out.info.deconvolution_iterations > 0 {
        info!(
            "richardson-lucy: {} passes, sigma {:.2} px, every pixel",
            out.info.deconvolution_iterations, out.info.usm_radius_px
        );
    }
    if let Some(s) = &out.info.screen {
        info!(
            "screen compensation: amount {:.2}, gain cap {:.1}, {} taps, response {:.2} @ 0.25 c/px, {:.2} @ 0.40 c/px{}",
            s.amount,
            s.max_gain,
            s.taps.len(),
            s.response_025,
            s.response_040,
            if s.downscaled { "" } else { " (no downscale: aperture only)" }
        );
    }
    info!(
        "wrote {} ({}x{}{}{}) in {} ms, render {} ms",
        a.out.display(),
        out.info.width,
        out.info.height,
        if size.is_some_and(|s| s.is_pixels()) { String::new() } else { format!(" @ {} dpi", a.dpi) },
        a.proof.as_ref().map(|p| format!(", proof {}", p.display())).unwrap_or_default(),
        t.elapsed().as_millis(),
        out.info.ms
    );
    Ok(())
}

pub fn info(raw: &Path, as_json: bool) -> Result<()> {
    let t = Instant::now();
    let frame = RawlerDecoder.decode(raw).with_context(|| format!("decoding {}", raw.display()))?;
    let decode_ms = t.elapsed().as_millis();
    let (lo, hi) = frame.raw_range();
    let kind = match frame.kind {
        SensorKind::Bayer(p) => format!("Bayer {p}"),
        SensorKind::Mono => "Mono (no CFA)".to_string(),
    };
    let v = json!({
        "file": raw.display().to_string(),
        "make": frame.make, "model": frame.model,
        "clean_make": frame.clean_make, "clean_model": frame.clean_model,
        "width": frame.width, "height": frame.height,
        "bits_container": frame.bits, "bits_effective": frame.effective_bits(),
        "sensor": kind,
        "black_tile": frame.black_tile,
        "white_rgb": frame.white_rgb,
        "crop": frame.crop,
        "orientation": frame.orientation,
        "raw_min": lo, "raw_max": hi,
        "wb_as_shot": frame.wb_as_shot,
        "xyz_to_cam": frame.xyz_to_cam,
        "exposure": frame.exposure,
        "decode_ms": decode_ms,
    });
    if as_json {
        println!("{}", serde_json::to_string_pretty(&v)?);
    } else {
        println!("{:<14} {} {}", "camera", frame.make.trim(), frame.model.trim());
        let exposure = frame.exposure.summary();
        if !exposure.is_empty() {
            println!("{:<14} {}", "exposure", exposure);
        }
        if let Some(lens) = &frame.exposure.lens {
            println!("{:<14} {}", "lens", lens.trim());
        }
        println!(
            "{:<14} {} x {} @ {} bit effective ({} bit container)",
            "sensor",
            frame.width,
            frame.height,
            frame.effective_bits(),
            frame.bits
        );
        println!("{:<14} {}", "pattern", kind);
        println!("{:<14} {:?} (tile (0,0),(1,0),(0,1),(1,1))", "black", frame.black_tile);
        println!("{:<14} {:?} (R,G,B)", "white", frame.white_rgb);
        println!(
            "{:<14} x={} y={} w={} h={}",
            "crop", frame.crop.x, frame.crop.y, frame.crop.w, frame.crop.h
        );
        println!("{:<14} {}", "orientation", frame.orientation);
        println!("{:<14} {} .. {}", "raw range", lo, hi);
        match frame.wb_as_shot {
            Some(w) => println!("{:<14} R {:.4}  G 1  B {:.4}", "wb as-shot", w[0], w[2]),
            None => println!("{:<14} none in file (gray-world fallback)", "wb as-shot"),
        }
        println!("{:<14} {} ms", "decode", decode_ms);
    }
    Ok(())
}

pub fn dump(
    raw: &Path,
    out: &Path,
    fix_defects: bool,
    preview: Option<&Path>,
    preview_scale: usize,
) -> Result<()> {
    let t = Instant::now();
    let frame = RawlerDecoder.decode(raw).with_context(|| format!("decoding {}", raw.display()))?;
    info!("decoded in {} ms", t.elapsed().as_millis());
    let params = IngestParams { fix_defects, ..IngestParams::default() };
    let t = Instant::now();
    let mosaic = ingest(&frame, &params);
    info!("ingest in {} ms: {}", t.elapsed().as_millis(), serde_json::to_string(&mosaic.report)?);
    let desc = json!({
        "mimizan": env!("CARGO_PKG_VERSION"),
        "stage": "mosaic-dump",
        "camera": mosaic.camera,
        "phase": mosaic.phase.map(|p| p.name()),
        "noise": mosaic.noise,
        "report": mosaic.report,
    });
    let t = Instant::now();
    write_gray16(
        out,
        &mosaic.plane,
        &TiffMeta {
            description: &desc.to_string(),
            trc: GrayTrc::Linear,
            orientation: mosaic.orientation,
            dpi: None,
        },
    )?;
    info!("wrote {} in {} ms", out.display(), t.elapsed().as_millis());
    if let Some(p) = preview {
        mimizan_core::preview::write_png(&mosaic.plane, p, preview_scale, true)?;
        info!("preview {}", p.display());
    }
    Ok(())
}
