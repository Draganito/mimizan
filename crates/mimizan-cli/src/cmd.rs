//! The production commands: `negative`, `print`, `info`, `dump`, and the
//! option parsers they share with the bench and calibration commands.

use crate::measure::{parse_list, parse_rect};
use anyhow::{bail, Context, Result};
use mimizan_core::calib::CameraFile;
use mimizan_core::decode::{RawDecoder, RawlerDecoder, SensorKind};
use mimizan_core::icc::GrayTrc;
use mimizan_core::ingest::{ingest, IngestParams, WhiteBalance};
use mimizan_core::mix::{ColorFilter, WeightSpace, Weights};
use mimizan_core::pipeline::{self, NegativeParams};
use mimizan_core::plate::{PlateFilters, PlateParams, PlateSpace};
use mimizan_core::print::LAMP_LIMIT;
use mimizan_core::separate::{MaskMode, SeparateParams};
use mimizan_core::tiffout::{write_gray16, write_gray8, TiffMeta};
use serde_json::json;
use std::path::{Path, PathBuf};
use std::time::Instant;
use tracing::{info, warn};

/// Options of `mimizan negative`, already parsed by clap.
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
    pub repair_defects: bool,
    pub preview: Option<PathBuf>,
    pub preview_scale: usize,
    pub crop: Option<String>,
    /// `Some(filter set)` makes a three-colour plate.
    pub plate: Option<String>,
    pub plate_space: String,
    /// A recipe whose `mix` and `plate` develop this negative; the flags
    /// above override them.
    pub recipe: Option<PathBuf>,
    /// Write a linear DNG of the channels plate instead of the negative.
    pub dng: bool,
}

/// `camera`, `wratten` or `channels`.
pub fn parse_plate_filters(s: &str) -> Result<PlateFilters> {
    PlateFilters::parse(s).ok_or_else(|| {
        let names: Vec<&str> = PlateFilters::ALL.iter().map(|f| f.name()).collect();
        anyhow::anyhow!("unknown plate filters '{s}' ({})", names.join("|"))
    })
}

/// `adobe-rgb` or `srgb`.
pub fn parse_plate_space(s: &str) -> Result<PlateSpace> {
    PlateSpace::parse(s).ok_or_else(|| {
        let names: Vec<&str> = PlateSpace::ALL.iter().map(|f| f.name()).collect();
        anyhow::anyhow!("unknown plate space '{s}' ({})", names.join("|"))
    })
}

/// `"R,B"` lamp stops for a plate print, green fixed at zero.
pub fn parse_lamps(s: &str) -> Result<[f64; 3]> {
    let v = parse_list(s)?;
    if v.len() != 2 {
        bail!("--lamp wants two values R,B (stops; green is the reference)");
    }
    let lamps = [v[0], 0.0, v[1]];
    if lamps.iter().any(|l| !l.is_finite() || l.abs() > LAMP_LIMIT) {
        bail!("lamps must be within ±{LAMP_LIMIT} stops");
    }
    Ok(lamps)
}

/// `"r,g,b"` mix weights, validated.
pub fn parse_weights(s: &str) -> Result<Weights> {
    let v = parse_list(s)?;
    if v.len() != 3 {
        bail!("weights need three values r,g,b");
    }
    let w = Weights { r: v[0], g: v[1], b: v[2] };
    w.validate()?;
    Ok(w)
}

/// `balanced` or `raw`.
pub fn parse_weight_space(s: &str) -> Result<WeightSpace> {
    match s.to_ascii_lowercase().as_str() {
        "balanced" | "bal" => Ok(WeightSpace::Balanced),
        "raw" | "sensor" => Ok(WeightSpace::Raw),
        other => bail!("unknown weight space '{other}' (balanced|raw)"),
    }
}

/// `as-shot`, `gray`, `none` or manual `"r,g,b"` multipliers.
pub fn parse_wb(s: &str) -> Result<WhiteBalance> {
    match s.to_ascii_lowercase().as_str() {
        "as-shot" | "asshot" | "camera" => Ok(WhiteBalance::AsShot),
        "gray" | "grey" | "gray-world" => Ok(WhiteBalance::GrayWorld),
        "none" | "off" => Ok(WhiteBalance::None),
        other => {
            let v = parse_list(other)
                .with_context(|| format!("white balance '{other}' is not as-shot|gray|none|r,g,b"))?;
            if v.len() != 3 || v.iter().any(|x| !(x.is_finite() && *x > 0.0)) {
                bail!("manual white balance needs three positive values r,g,b");
            }
            Ok(WhiteBalance::Manual([v[0], v[1], v[2]]))
        }
    }
}

/// `dubois`, `adaptive` or `off`.
pub fn parse_mask(s: &str) -> Result<MaskMode> {
    match s.to_ascii_lowercase().as_str() {
        "off" => Ok(MaskMode::Off),
        "adaptive" | "on" => Ok(MaskMode::Adaptive),
        "dubois" => Ok(MaskMode::Dubois),
        other => bail!("unknown mask mode '{other}' (dubois|adaptive|off)"),
    }
}

/// Tell the user when `--cutoff` or `--rounds` were given but the chosen
/// mask mode does not read them.
pub fn warn_inert_flags(p: &SeparateParams, cutoff: f64, rounds: usize) {
    let d = SeparateParams::default();
    if p.mask == MaskMode::Dubois && cutoff != d.cutoff {
        warn!("--cutoff has no effect with --mask dubois (its bands are fixed)");
    }
    if p.mask != MaskMode::Adaptive && rounds != d.rounds_max {
        warn!("--rounds has no effect without --mask adaptive");
    }
}

/// `<out stem>.mask.tif` next to the negative.
pub fn sidecar_path(out: &Path) -> PathBuf {
    let stem = out.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_default();
    out.with_file_name(format!("{stem}.mask.tif"))
}

/// `mimizan negative`: RAW → linear 16-bit negative TIFF plus mask sidecar.
pub fn negative(a: &NegativeArgs) -> Result<()> {
    let t_all = Instant::now();
    let t = Instant::now();
    let frame = pipeline::decode(&a.raw).with_context(|| format!("decoding {}", a.raw.display()))?;
    let decode_ms = t.elapsed().as_millis();
    info!("decode {} ms", decode_ms);

    // Past the machine's whole memory the separation ends in the OOM
    // killer, not in an error; below it, it is only slow.
    let need = pipeline::develop_memory(frame.width, frame.height, frame.kind);
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    if sys.total_memory() > 0 && need > sys.total_memory() {
        bail!(
            "{}x{} would hold about {} while separating, this machine has {} in all",
            frame.width,
            frame.height,
            mimizan_core::print::memory_text(need),
            mimizan_core::print::memory_text(sys.total_memory())
        );
    }

    let mut params = NegativeParams::default();
    params.ingest.repair_defects = a.repair_defects;
    params.ingest.wb = parse_wb(&a.wb)?;
    params.separate.mask = parse_mask(&a.mask)?;
    params.separate.cutoff = a.cutoff;
    params.separate.rounds_max = a.rounds;
    warn_inert_flags(&params.separate, a.cutoff, a.rounds);

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
    // The desktop's file beside the RAW: its mix and its plate, unless a
    // flag says otherwise. The print half of it is `print --recipe`'s.
    if let Some(p) = &a.recipe {
        let r =
            mimizan_core::recipe::Recipe::load(p).with_context(|| format!("recipe file {}", p.display()))?;
        if let Some(m) = r.mix.filter(|_| a.weights.is_none()) {
            params = params.with_weights(m, WeightSpace::Balanced);
            info!("recipe mix R {:.3} G {:.3} B {:.3}", m.r, m.g, m.b);
        }
        if let Some(pl) = r.plate.filter(|_| a.plate.is_none()) {
            params.plate = Some(pl.params());
            info!("recipe plate: {} filters, {} primaries", pl.filters.name(), pl.space.name());
        }
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
    if let Some(f) = &a.plate {
        if a.filter.is_some() {
            bail!("--filter is a contrast filter for one negative; a plate has its own three (--plate)");
        }
        params.plate =
            Some(PlateParams { filters: parse_plate_filters(f)?, space: parse_plate_space(&a.plate_space)? });
    }
    if params.plate.is_some() && a.weights.is_some() {
        warn!(
            "--weights is the mix of one negative; the plate's emulsion is the camera file's response or \
             the native one, so it has no effect here"
        );
    }
    // The DNG is the channels plate, nothing else: the converter does the
    // colour. The recipe's crop still applies.
    let recipe_crop = a
        .recipe
        .as_ref()
        .map(|p| mimizan_core::recipe::Recipe::load(p).map(|r| r.crop))
        .transpose()?
        .flatten();
    if a.dng {
        if frame.kind == SensorKind::Mono {
            bail!(
                "a linear DNG is the colour plate of a Bayer sensor; this file is monochrome, write the TIFF"
            );
        }
        if a.filter.is_some() {
            bail!("--filter is a contrast filter for one negative; the DNG carries the three channels");
        }
        if let Some(pl) = &params.plate {
            if pl.filters != PlateFilters::Channels {
                info!("DNG: the plate goes out as the channels, not through {} filters", pl.filters.name());
            }
        }
        params.plate =
            Some(PlateParams { filters: PlateFilters::Channels, space: parse_plate_space(&a.plate_space)? });
    }

    let neg = pipeline::negative_from_frame(&frame, &params, decode_ms)?;
    if let Some(pi) = &neg.info.plate {
        info!(
            "plate: three frames of {}x{} ({}), projected through {}",
            neg.plane.width,
            pi.frame_height,
            pi.frames.join(", "),
            pi.space.name()
        );
    }
    let desc = serde_json::to_string(&neg.info)?;
    let t = Instant::now();
    if a.dng {
        let pi = neg.info.plate.as_ref().context("the DNG needs the plate")?;
        let [r, g, b] = mimizan_core::plate::split(&neg.plane, pi)?;
        let meta = mimizan_core::linear::DngMeta::of(&frame, neg.info.wb, &desc)
            .context("the file carries no colour matrix, so no DNG can name the camera's colour")?;
        let w = mimizan_core::linear::write_dng(&a.out, [&r, &g, &b], neg.orientation, recipe_crop, &meta)?;
        info!(
            "wrote {} ({}x{} linear DNG, {}) in {} ms",
            a.out.display(),
            w.width,
            w.height,
            match recipe_crop {
                Some(_) => "the recipe's crop",
                None => "the whole frame",
            },
            t.elapsed().as_millis()
        );
    } else {
        write_gray16(
            &a.out,
            &neg.plane,
            &TiffMeta { description: &desc, trc: GrayTrc::Linear, orientation: neg.orientation, dpi: None },
        )?;
        let side = sidecar_path(&a.out);
        // The mask is one frame: the same for the three plates of a plate.
        let mask_h = neg.info.plate.as_ref().map_or(neg.plane.height, |pi| pi.frame_height);
        write_gray8(&side, neg.plane.width, mask_h, &neg.mask8, "mimizan mask: 255*max(M), 255=saturated")?;
        info!("wrote {} + {} in {} ms", a.out.display(), side.display(), t.elapsed().as_millis());
    }

    if let Some(p) = &a.preview {
        mimizan_core::preview::write_png(&neg.plane, p, a.preview_scale, true)?;
        if let Some(c) = &a.crop {
            let r = parse_rect(c).context("--crop")?;
            let cp = p.with_extension("crop.png");
            mimizan_core::preview::write_png_crop(&neg.plane, &cp, r.x, r.y, r.w, r.h, true)?;
            info!("crop {}", cp.display());
        }
        info!("preview {}", p.display());
    }
    let pixels = neg.plane.data.len().max(1) as f64;
    let defects = neg.info.report.defects;
    if defects as f64 > 0.001 * pixels {
        warn!("defect rate {:.3} % is high; check the noise model", 100.0 * defects as f64 / pixels);
    } else if defects > 0 && !neg.info.report.defects_repaired {
        info!("{defects} defective photosite(s) left as measured (--repair-defects replaces them)");
    }
    info!("total {} ms", t_all.elapsed().as_millis());
    Ok(())
}

/// Options of `mimizan print`, already parsed by clap. `usm_k` and
/// `distance_mm` describe the viewing model; they are recorded in the print's
/// metadata, not applied to the pixels.
pub struct PrintArgs {
    pub negative: PathBuf,
    pub out: PathBuf,
    pub recipe: Option<PathBuf>,
    pub write_recipe: Option<PathBuf>,
    pub look: Option<PathBuf>,
    pub paper: Option<String>,
    pub place: Option<f64>,
    pub size: Option<String>,
    pub resample: Option<String>,
    pub border_mm: Option<f64>,
    pub black_mm: Option<f64>,
    pub dpi: Option<f64>,
    pub usm_amount: Option<f64>,
    pub usm_k: f64,
    pub distance_mm: Option<f64>,
    pub use_mask: bool,
    pub deconv: Option<u32>,
    pub cameras: PathBuf,
    /// `"R,B"` lamp stops, plates only.
    pub lamp: Option<String>,
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

/// The recipe of a `print` run: the file, then every explicit flag on top.
pub fn print_recipe(a: &PrintArgs) -> Result<mimizan_core::recipe::Recipe> {
    use mimizan_core::calib::LookFile;
    use mimizan_core::paper::SplitGrade;
    use mimizan_core::print::PrintSize;
    use mimizan_core::recipe::{Recipe, Sharpen};
    use mimizan_core::resize::ResampleKernel;
    use mimizan_core::tone::Tone;

    let mut r = match &a.recipe {
        Some(p) => Recipe::load(p).with_context(|| format!("recipe file {}", p.display()))?,
        None => Recipe::default(),
    };
    if a.look.is_some() && a.paper.is_some() {
        bail!("--look and --paper exclude each other");
    }
    if let Some(p) = &a.look {
        r.curve = Tone::Look(LookFile::load(p).with_context(|| format!("look file {}", p.display()))?);
    }
    if let Some(p) = &a.paper {
        let (soft, hard) =
            p.split_once(',').ok_or_else(|| anyhow::anyhow!("--paper wants SOFT,HARD stops"))?;
        let split = SplitGrade::new(
            soft.trim().parse().with_context(|| format!("soft exposure '{soft}'"))?,
            hard.trim().parse().with_context(|| format!("hard exposure '{hard}'"))?,
        );
        split.validate()?;
        r.curve = Tone::Paper(split);
    }
    if let Some(v) = a.place {
        r.place = v;
    }
    if let Some(s) = &a.size {
        r.size = Some(PrintSize::parse(s)?);
    }
    if let Some(k) = &a.resample {
        r.resample = ResampleKernel::parse(k).ok_or_else(|| {
            let names: Vec<&str> = ResampleKernel::ALL.iter().map(|k| k.name()).collect();
            anyhow::anyhow!("unknown kernel '{k}' ({})", names.join("|"))
        })?;
    }
    if let Some(b) = a.border_mm {
        r.border_mm = b;
    }
    if let Some(b) = a.black_mm {
        r.black_mm = b;
    }
    if let Some(d) = a.dpi {
        r.dpi = d;
    }

    // Sharpening: explicit flags replace the recipe's mode; without any, a
    // recipe keeps its own, and a bare run takes the camera file's USM.
    let explicit = a.deconv.is_some() || a.usm_amount.is_some();
    if explicit {
        let usm_amount = match a.usm_amount {
            Some(v) => v,
            None => usm_default_from_negative(&a.negative, &a.cameras).unwrap_or(0.0),
        };
        r.sharpen = match a.deconv {
            Some(passes) => Sharpen::Deconv { passes, usm_amount },
            None if usm_amount > 0.0 => Sharpen::Usm { amount: usm_amount },
            None => Sharpen::Off,
        };
    } else if a.recipe.is_none() {
        let usm_amount = usm_default_from_negative(&a.negative, &a.cameras).unwrap_or(0.0);
        if usm_amount > 0.0 {
            r.sharpen = Sharpen::Usm { amount: usm_amount };
        }
    }
    r.validate()?;
    Ok(r)
}

/// `mimizan print`: negative TIFF → print TIFF through a recipe.
pub fn print(a: &PrintArgs) -> Result<()> {
    use mimizan_core::print::{self as pr, PrintParams};
    if pr::is_jpeg_path(&a.out) {
        bail!("the print is written as a 16-bit TIFF; give -o a .tif path");
    }
    if !(a.usm_k.is_finite() && a.usm_k > 0.0) {
        bail!("--usm-k must be positive");
    }
    if a.distance_mm.is_some_and(|d| !(d.is_finite() && d > 0.0)) {
        bail!("--distance-mm must be positive");
    }
    let recipe = print_recipe(a)?;
    if let Some(p) = &a.write_recipe {
        recipe.save(p).with_context(|| format!("writing recipe {}", p.display()))?;
        info!("recipe {}", p.display());
    }
    // The lamps: the flag, else the recipe's plate, else off.
    let lamps = match &a.lamp {
        Some(l) => parse_lamps(l)?,
        None => recipe.plate.map(|p| p.lamps_rgb()).unwrap_or([0.0; 3]),
    };
    let p = PrintParams {
        recipe,
        usm_k: a.usm_k,
        viewing_distance_mm: a.distance_mm,
        use_mask: a.use_mask,
        lamps,
    };
    // The render is refused above the machine's whole memory: past that it
    // ends in the OOM killer or a frozen machine, not in an error. Below
    // it, a print larger than what is free is only slow.
    let mut sys = sysinfo::System::new();
    sys.refresh_memory();
    let budget = (sys.total_memory() > 0).then_some(sys.total_memory());
    let out = pr::render_within(&a.negative, &p, budget)
        .with_context(|| format!("rendering {}", a.negative.display()))?;
    if a.lamp.is_some() && out.plate.is_none() {
        warn!("--lamp has no effect: {} is not a plate", a.negative.display());
    }
    if let Some(pp) = &out.info.plate {
        info!("plate print: {} filters, {} primaries", pp.filters.name(), pp.space.name());
    }
    let t = Instant::now();
    pr::write(&out, &a.out, &p)?;
    if out.info.deconvolution_iterations > 0 {
        info!(
            "richardson-lucy: {} passes, scale sigma {:.2} px, picture area",
            out.info.deconvolution_iterations, out.info.resample_sigma_px
        );
    }
    info!(
        "wrote {} ({}x{} @ {} dpi) in {} ms, render {} ms",
        a.out.display(),
        out.info.width,
        out.info.height,
        p.recipe.dpi,
        t.elapsed().as_millis(),
        out.info.ms
    );
    Ok(())
}

/// `mimizan info`: what the decoder reads from a RAW file.
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

/// `mimizan dump`: the balanced mosaic after ingest as a 16-bit TIFF.
pub fn dump(
    raw: &Path,
    out: &Path,
    repair_defects: bool,
    preview: Option<&Path>,
    preview_scale: usize,
) -> Result<()> {
    let t = Instant::now();
    let frame = RawlerDecoder.decode(raw).with_context(|| format!("decoding {}", raw.display()))?;
    info!("decoded in {} ms", t.elapsed().as_millis());
    let params = IngestParams { repair_defects, ..IngestParams::default() };
    let t = Instant::now();
    let mosaic = ingest(&frame, &params)?;
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
