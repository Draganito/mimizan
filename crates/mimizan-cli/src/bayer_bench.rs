//! `mimizan bench bayer`: the least-squares Bayer demosaic against the
//! demosaicers, in colour.
//!
//! The same fake-Bayer mosaics as `bench plate`. CPSNR and the PSNR of each
//! channel are on sRGB-encoded values, 16 px of the edge left out, which is
//! how the X-Trans measurement was made.

use crate::kodak::{has_tool, run_external, stem_of, tool_version, Tool, METHODS};
use anyhow::{bail, Context, Result};
use clap::Args;
use mimizan_cli::bayer_ls::{self, Knobs, Tone};
use mimizan_core::cfa::BayerPhase;
use mimizan_core::kodak::{self, colour_quality, ColourQuality};
use mimizan_core::plane::Plane;
use mimizan_core::synth::Rgb;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Args)]
pub struct BayerBenchArgs {
    /// Directory with 8-bit sRGB PNG/JPEG pictures (kodim01.png ... kodim24.png)
    #[arg(long, default_value = "testdata/kodak")]
    pub images: PathBuf,
    /// Work directory: DNGs, converter outputs, JSON, report
    #[arg(long, default_value = "out/bayer")]
    pub out: PathBuf,
    /// Bayer phase the pictures are mosaicked with
    #[arg(long, default_value = "RGGB")]
    pub phase: String,
    /// Border (px) excluded from every metric
    #[arg(long, default_value_t = 16)]
    pub border: usize,
    /// Comma-separated stems to run (default: every picture)
    #[arg(long)]
    pub only: Option<String>,
    /// Do not call libraw / RawTherapee
    #[arg(long)]
    pub no_external: bool,
    /// Magnify the pictures (Lanczos-3) before mosaicking: 1 = pixel-sharp,
    /// 2 = band-limited to half Nyquist
    #[arg(long, default_value_t = 1)]
    pub scale: usize,
    /// Gaussian blur (sigma, px) of the truth before mosaicking; 0 = off
    #[arg(long, default_value_t = 0.0)]
    pub blur: f64,
    /// Gaussian σ of the window the priors are weighed in
    #[arg(long, default_value_t = 6.0)]
    pub sigma: f64,
    /// Gaussian σ of the window the left-out error of the priors is taken in
    #[arg(long, default_value_t = 1.0)]
    pub near: f64,
    /// Share of the left-out error in the weight; 0 = the likelihood alone
    #[arg(long, default_value_t = 0.5)]
    pub left_out: f64,
    /// How many pixels' worth of evidence that weight is given
    #[arg(long, default_value_t = 20.0)]
    pub sharp: f64,
    /// Head start of the prior the search found
    #[arg(long, default_value_t = 0.0)]
    pub bias: f64,
    /// How often the lens and the noise are moved towards the mosaic's own after the search; 0 = the search's Gaussian lens
    #[arg(long, default_value_t = 100)]
    pub lens_rounds: usize,
    /// The curve to separate under: `auto` (the one the mosaic speaks for), `srgb`, or the exponent of a power
    #[arg(long, default_value = "auto")]
    pub tone: String,
    /// How often the second step is repeated
    #[arg(long, default_value_t = 1)]
    pub rounds: usize,
    /// Keep the second step only where the luminance gets smoother; 0 keeps it
    #[arg(long, default_value_t = 4.0)]
    pub check: f64,
    /// Weight of the first step beside the photosite in the second step
    #[arg(long, default_value_t = 0.15)]
    pub hold: f64,
}

#[derive(Clone, Debug, Serialize)]
struct ImageResult {
    stem: String,
    width: usize,
    height: usize,
    /// In `METHODS` order, `None` where the tool is missing or bilinear.
    demosaic: Vec<Option<ColourQuality>>,
    /// The least-squares Bayer demosaic.
    bayer: ColourQuality,
    /// The curve it was separated under.
    tone: String,
}

#[derive(Clone, Debug, Serialize)]
struct Report {
    phase: String,
    border: usize,
    scale: usize,
    blur: f64,
    knobs: KnobDump,
    tools: Vec<(String, Option<String>)>,
    methods: Vec<&'static str>,
    images: Vec<ImageResult>,
}

#[derive(Clone, Debug, Serialize)]
struct KnobDump {
    sigma: f64,
    near: f64,
    left_out: f64,
    sharp: f64,
    bias: f64,
    lens_rounds: usize,
    tone: String,
    rounds: usize,
    check: f64,
    hold: f64,
}

pub fn bayer_bench(a: &BayerBenchArgs) -> Result<()> {
    let phase = BayerPhase::from_name(&a.phase)?;
    if phase != BayerPhase::RGGB {
        bail!("the least-squares tile is RGGB; other phases are not measured yet");
    }
    std::fs::create_dir_all(&a.out)?;
    let tone = match a.tone.as_str() {
        "auto" => None,
        "srgb" => Some(Tone::Srgb),
        power => match power.parse::<f64>() {
            Ok(g) if g > 0.0 && g < 1.0 => Some(Tone::power(g)),
            _ => bail!("--tone is auto, srgb or an exponent between 0 and 1, not {power}"),
        },
    };
    let knobs = Knobs {
        sigma: a.sigma,
        near: a.near,
        left_out: a.left_out,
        sharp: a.sharp,
        bias: a.bias,
        lens_rounds: a.lens_rounds,
        tone,
        rounds: a.rounds,
        check: a.check,
        hold: a.hold,
    };
    let tools: Vec<(String, Option<String>)> = if a.no_external {
        vec![("dcraw_emu".into(), None), ("rawtherapee-cli".into(), None)]
    } else {
        vec![
            ("dcraw_emu".into(), tool_version("dcraw_emu", "").filter(|s| s.contains("dcraw"))),
            (
                "rawtherapee-cli".into(),
                tool_version("rawtherapee-cli", "-h").filter(|s| s.contains("RawTherapee")),
            ),
        ]
    };
    for (name, v) in &tools {
        match v {
            Some(v) => eprintln!("{name}: {v}"),
            None => eprintln!("{name}: not found, skipped"),
        }
    }

    let mut files: Vec<PathBuf> = std::fs::read_dir(&a.images)
        .with_context(|| format!("reading {}", a.images.display()))?
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| {
            matches!(
                p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
                Some("png" | "jpg" | "jpeg")
            )
        })
        .collect();
    files.sort();
    if let Some(only) = &a.only {
        let keep: Vec<&str> = only.split(',').map(str::trim).collect();
        files.retain(|p| p.file_stem().is_some_and(|s| keep.contains(&s.to_string_lossy().as_ref())));
    }
    if files.is_empty() {
        bail!("no pictures in {}", a.images.display());
    }

    let mut images = Vec::new();
    for file in &files {
        let stem = stem_of(file)?;
        let mut rgb = kodak::load_srgb8(file)?;
        if a.scale > 1 {
            let up = |p: &Plane| mimizan_core::resize::resize(p, p.width * a.scale, p.height * a.scale);
            rgb = Rgb { r: up(&rgb.r), g: up(&rgb.g), b: up(&rgb.b) };
        }
        if a.blur > 0.0 {
            let bl = |p: &Plane| mimizan_core::usm::gaussian_blur(p, a.blur);
            rgb = Rgb { r: bl(&rgb.r), g: bl(&rgb.g), b: bl(&rgb.b) };
        }
        let (w, h) = (rgb.width(), rgb.height());
        if w <= 2 * a.border || h <= 2 * a.border {
            bail!("{stem}: {w}x{h} leaves nothing inside a border of {} px", a.border);
        }
        let fb = kodak::mosaic_rgb(&rgb, phase, kodak::quantisation_noise(), &stem);
        let dng = a.out.join(format!("{stem}.dng"));
        kodak::write_dng(&dng, w, h, &fb.samples, phase, &stem)?;

        let mut demosaic = Vec::new();
        for m in METHODS {
            if m.tool == Tool::Bilinear || !has_tool(m.tool, &tools) {
                demosaic.push(None);
                continue;
            }
            match run_external(m.tool, &dng, &a.out) {
                Ok(Some(est)) => {
                    if (est.width(), est.height()) != (w, h) {
                        bail!(
                            "{}: {} returned {}x{}, expected {}x{}",
                            stem,
                            m.name,
                            est.width(),
                            est.height(),
                            w,
                            h
                        );
                    }
                    demosaic.push(Some(colour_quality(&est, &rgb, a.border)));
                }
                Ok(None) => demosaic.push(None),
                Err(e) => {
                    eprintln!("{stem}: {} failed: {e}", m.name);
                    demosaic.push(None);
                }
            }
        }
        let linear: Vec<f64> = fb.samples.iter().map(|v| f64::from(*v) / 65535.0).collect();
        let (got, _, tone) = bayer_ls::separate(linear.clone(), linear, w, &knobs, &|_| {});
        let est = Rgb {
            r: Plane::from_vec(w, h, got.r.data),
            g: Plane::from_vec(w, h, got.g.data),
            b: Plane::from_vec(w, h, got.b.data),
        };
        let bayer = colour_quality(&est, &rgb, a.border);
        let best_ext = demosaic.iter().flatten().map(|q| q.cpsnr_db).fold(f64::NEG_INFINITY, f64::max);
        eprintln!(
            "{stem}: bayer {:.2} dB (R {:.2} G {:.2} B {:.2}) under {tone}, best demosaicer {:.2} dB",
            bayer.cpsnr_db, bayer.psnr_r_db, bayer.psnr_g_db, bayer.psnr_b_db, best_ext
        );
        images.push(ImageResult { stem, width: w, height: h, demosaic, bayer, tone: tone.to_string() });
    }

    let report = Report {
        phase: a.phase.clone(),
        border: a.border,
        scale: a.scale,
        blur: a.blur,
        knobs: KnobDump {
            sigma: knobs.sigma,
            near: knobs.near,
            left_out: knobs.left_out,
            sharp: knobs.sharp,
            bias: knobs.bias,
            lens_rounds: knobs.lens_rounds,
            tone: a.tone.clone(),
            rounds: knobs.rounds,
            check: knobs.check,
            hold: knobs.hold,
        },
        tools,
        methods: METHODS.iter().map(|m| m.name).collect(),
        images,
    };
    mimizan_core::outfile::write_bytes(
        &a.out.join("bayer.json"),
        serde_json::to_string_pretty(&report)?.as_bytes(),
    )?;
    let md = markdown(&report);
    mimizan_core::outfile::write_bytes(&a.out.join("report.md"), md.as_bytes())?;
    print!("{md}");
    Ok(())
}

fn markdown(r: &Report) -> String {
    let n = r.images.len();
    let mut lines = vec![format!(
        "Bayer least squares, {} pictures, scale {}, blur {}, border {} px. CPSNR on sRGB-encoded values.\n",
        n, r.scale, r.blur, r.border
    )];
    lines.push("| Method | CPSNR | R | G | B | leads |".into());
    lines.push("|---|---|---|---|---|---|".into());
    let mean = |pick: &dyn Fn(&ImageResult) -> Option<ColourQuality>, ch: usize| {
        let v: Vec<f64> = r
            .images
            .iter()
            .filter_map(pick)
            .map(|q| match ch {
                0 => q.cpsnr_db,
                1 => q.psnr_r_db,
                2 => q.psnr_g_db,
                _ => q.psnr_b_db,
            })
            .collect();
        if v.is_empty() {
            f64::NAN
        } else {
            v.iter().sum::<f64>() / v.len() as f64
        }
    };
    let leads = |better: &dyn Fn(&ImageResult) -> Option<f64>| {
        r.images
            .iter()
            .filter(|img| {
                let Some(mine) = better(img) else { return false };
                let best = img
                    .demosaic
                    .iter()
                    .flatten()
                    .map(|q| q.cpsnr_db)
                    .chain(std::iter::once(img.bayer.cpsnr_db))
                    .fold(f64::NEG_INFINITY, f64::max);
                (mine - best).abs() < 1e-9 || mine >= best
            })
            .count()
    };
    let row = |name: &str, pick: &dyn Fn(&ImageResult) -> Option<ColourQuality>| {
        format!(
            "| {name} | {:.2} | {:.2} | {:.2} | {:.2} | {}/{n} |",
            mean(pick, 0),
            mean(pick, 1),
            mean(pick, 2),
            mean(pick, 3),
            leads(&|img| pick(img).map(|q| q.cpsnr_db))
        )
    };
    lines.push(row("Mimizan Bayer LS", &|img| Some(img.bayer)));
    for (k, name) in r.methods.iter().enumerate() {
        if r.images.iter().any(|img| img.demosaic.get(k).and_then(|q| q.as_ref()).is_some()) {
            lines.push(row(name, &|img| img.demosaic.get(k).and_then(|q| *q)));
        }
    }
    let ahead = r
        .images
        .iter()
        .filter(|img| {
            let best = img.demosaic.iter().flatten().map(|q| q.cpsnr_db).fold(f64::NEG_INFINITY, f64::max);
            img.bayer.cpsnr_db > best
        })
        .count();
    lines.push(format!("\nAhead of every demosaicer on {ahead}/{n} pictures."));
    lines.join("\n") + "\n"
}
