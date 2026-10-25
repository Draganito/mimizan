//! `mimizan bench plate`: the three-colour plate against the demosaicers,
//! in colour.
//!
//! The same fake-Bayer mosaics as `bench kodak`, but the comparison is the
//! RGB picture: the plate's three positives (`channels` filters, since a
//! fake mosaic has no camera matrix and the truth is the picture itself)
//! against the full demosaiced RGB of libraw and RawTherapee. CPSNR and the
//! PSNR of each channel on sRGB-encoded values, the convention of the
//! demosaicing literature.

use crate::kodak::{has_tool, run_external, stem_of, tool_version, Tool, METHODS};
use anyhow::{bail, Context, Result};
use clap::Args;
use mimizan_core::cfa::BayerPhase;
use mimizan_core::kodak::{self, colour_quality, ColourQuality};
use mimizan_core::plane::Plane;
use mimizan_core::plate::{mix_plate, PlateWeights};
use mimizan_core::separate::{separate, SeparateParams};
use mimizan_core::synth::Rgb;
use serde::Serialize;
use std::path::PathBuf;

#[derive(Args)]
pub struct PlateBenchArgs {
    /// Directory with 8-bit sRGB PNG/JPEG pictures (kodim01.png ... kodim24.png)
    #[arg(long, default_value = "testdata/kodak")]
    pub images: PathBuf,
    /// Work directory: DNGs, converter outputs, JSON, report
    #[arg(long, default_value = "out/plate")]
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
    /// Real RAW files instead of pictures (2x2 binned truth, see `bench kodak`)
    #[arg(long, num_args = 1.., conflicts_with = "only")]
    pub raw: Vec<PathBuf>,
}

#[derive(Clone, Debug, Serialize)]
struct ImageResult {
    stem: String,
    width: usize,
    height: usize,
    /// In `METHODS` order, `None` where the tool is missing or bilinear.
    demosaic: Vec<Option<ColourQuality>>,
    /// The plate, default separation.
    plate: ColourQuality,
    /// The plate with `--reconstruct`.
    plate_reconstruct: ColourQuality,
}

#[derive(Clone, Debug, Serialize)]
struct Report {
    phase: String,
    border: usize,
    scale: usize,
    blur: f64,
    from_raw: bool,
    tools: Vec<(String, Option<String>)>,
    methods: Vec<&'static str>,
    images: Vec<ImageResult>,
}

fn plate_rgb(mosaic: &mimizan_core::ingest::Mosaic, p: &SeparateParams) -> Result<Rgb> {
    let sep = separate(mosaic, p)?;
    let [r, g, b] = mix_plate(&sep, &PlateWeights::channels());
    Ok(Rgb { r, g, b })
}

fn mean(vals: impl Iterator<Item = f64>) -> Option<f64> {
    let v: Vec<f64> = vals.filter(|x| x.is_finite()).collect();
    (!v.is_empty()).then(|| v.iter().sum::<f64>() / v.len() as f64)
}

fn fmt(v: Option<f64>) -> String {
    v.map(|x| format!("{x:.2}")).unwrap_or_else(|| "–".into())
}

/// One line of the means table: name, CPSNR, R, G, B, pictures led.
type MeanRow = (String, Option<f64>, Option<f64>, Option<f64>, Option<f64>, usize);

fn markdown(r: &Report) -> String {
    let mut s = String::new();
    s.push_str("# The plate in colour: Kodak set\n\n");
    s.push_str(&format!(
        "Phase {}, border {} px, scale {}, blur {}{}. CPSNR and per-channel PSNR in dB on sRGB-encoded 0..255 values; mean over {} pictures, and how many pictures each method leads.\n\n",
        r.phase,
        r.border,
        r.scale,
        r.blur,
        if r.from_raw { ", binned RAW truth" } else { "" },
        r.images.len()
    ));
    let n = r.images.len();
    // Per-method means: name, CPSNR, R, G, B, leads.
    let mut rows: Vec<MeanRow> = Vec::new();
    let pick = |f: &dyn Fn(&ImageResult) -> Option<ColourQuality>| {
        let c = mean(r.images.iter().filter_map(f).map(|q| q.cpsnr_db));
        let rr = mean(r.images.iter().filter_map(f).map(|q| q.psnr_r_db));
        let g = mean(r.images.iter().filter_map(f).map(|q| q.psnr_g_db));
        let b = mean(r.images.iter().filter_map(f).map(|q| q.psnr_b_db));
        (c, rr, g, b)
    };
    // Leads: per picture, which method has the highest CPSNR.
    let mut leads = vec![0usize; r.methods.len() + 2];
    for img in &r.images {
        let mut best = (f64::NEG_INFINITY, 0usize);
        for (k, q) in img.demosaic.iter().enumerate() {
            if let Some(q) = q {
                if q.cpsnr_db > best.0 {
                    best = (q.cpsnr_db, k);
                }
            }
        }
        if img.plate.cpsnr_db > best.0 {
            best = (img.plate.cpsnr_db, r.methods.len());
        }
        if img.plate_reconstruct.cpsnr_db > best.0 {
            best = (img.plate_reconstruct.cpsnr_db, r.methods.len() + 1);
        }
        leads[best.1] += 1;
    }
    for (k, name) in r.methods.iter().enumerate() {
        let (c, rr, g, b) = pick(&|i: &ImageResult| i.demosaic[k]);
        if c.is_some() {
            rows.push((name.to_string(), c, rr, g, b, leads[k]));
        }
    }
    let (c, rr, g, b) = pick(&|i: &ImageResult| Some(i.plate));
    rows.push(("**Mimizan plate** (linear)".into(), c, rr, g, b, leads[r.methods.len()]));
    let (c, rr, g, b) = pick(&|i: &ImageResult| Some(i.plate_reconstruct));
    rows.push(("**Mimizan plate** + reconstruct".into(), c, rr, g, b, leads[r.methods.len() + 1]));
    rows.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    s.push_str("| Method | CPSNR | R | G | B | leads |\n|---|---|---|---|---|---|\n");
    for (name, c, rr, g, b, l) in &rows {
        s.push_str(&format!(
            "| {name} | {} | {} | {} | {} | {l}/{n} |\n",
            fmt(*c),
            fmt(*rr),
            fmt(*g),
            fmt(*b)
        ));
    }
    s.push_str("\n## Per picture (CPSNR)\n\n| Picture |");
    let present: Vec<usize> =
        (0..r.methods.len()).filter(|&k| r.images.iter().any(|i| i.demosaic[k].is_some())).collect();
    for &k in &present {
        s.push_str(&format!(" {} |", r.methods[k]));
    }
    s.push_str(" plate | plate+rec |\n|---|");
    for _ in 0..present.len() + 2 {
        s.push_str("---|");
    }
    s.push('\n');
    for img in &r.images {
        s.push_str(&format!("| {} |", img.stem));
        for &k in &present {
            s.push_str(&format!(" {} |", fmt(img.demosaic[k].map(|q| q.cpsnr_db))));
        }
        s.push_str(&format!(" {:.2} | {:.2} |\n", img.plate.cpsnr_db, img.plate_reconstruct.cpsnr_db));
    }
    s
}

pub fn plate_bench(a: &PlateBenchArgs) -> Result<()> {
    let phase = BayerPhase::from_name(&a.phase)?;
    std::fs::create_dir_all(&a.out)?;
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

    let mut files: Vec<PathBuf> = if a.raw.is_empty() {
        std::fs::read_dir(&a.images)
            .with_context(|| format!("reading {}", a.images.display()))?
            .filter_map(|e| e.ok().map(|e| e.path()))
            .filter(|p| {
                matches!(
                    p.extension().and_then(|e| e.to_str()).map(|e| e.to_ascii_lowercase()).as_deref(),
                    Some("png" | "jpg" | "jpeg")
                )
            })
            .collect()
    } else {
        a.raw.clone()
    };
    files.sort();
    if let Some(only) = &a.only {
        let keep: Vec<&str> = only.split(',').map(str::trim).collect();
        files.retain(|p| p.file_stem().is_some_and(|s| keep.contains(&s.to_string_lossy().as_ref())));
    }
    if files.is_empty() {
        bail!("no pictures in {}", a.images.display());
    }

    let default = SeparateParams::default();
    let reconstruct = SeparateParams { reconstruct: Some(Default::default()), ..SeparateParams::default() };
    let mut images = Vec::new();
    for file in &files {
        let stem = stem_of(file)?;
        let (mut rgb, noise) = if a.raw.is_empty() {
            (kodak::load_srgb8(file)?, kodak::quantisation_noise())
        } else {
            kodak::binned_raw(file)?
        };
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
        let fb = kodak::mosaic_rgb(&rgb, phase, noise, &stem);
        let dng = a.out.join(format!("{stem}.dng"));
        kodak::write_dng(&dng, w, h, &fb.samples, phase, &stem)?;

        let mut demosaic = Vec::new();
        for m in METHODS {
            if m.tool == Tool::Bilinear || !has_tool(m.tool, &tools) {
                demosaic.push(None);
                continue;
            }
            match run_external(m.tool, &dng, &a.out) {
                Ok(Some(est)) => demosaic.push(Some(colour_quality(&est, &rgb, a.border))),
                Ok(None) => demosaic.push(None),
                Err(e) => {
                    eprintln!("{stem}: {} failed: {e}", m.name);
                    demosaic.push(None);
                }
            }
        }
        let plate = colour_quality(&plate_rgb(&fb.mosaic, &default)?, &rgb, a.border);
        let plate_reconstruct = colour_quality(&plate_rgb(&fb.mosaic, &reconstruct)?, &rgb, a.border);
        let best_ext = demosaic.iter().flatten().map(|q| q.cpsnr_db).fold(f64::NEG_INFINITY, f64::max);
        eprintln!(
            "{stem}: plate {:.2} dB (R {:.2} G {:.2} B {:.2}), +reconstruct {:.2} dB, best demosaicer {:.2} dB",
            plate.cpsnr_db, plate.psnr_r_db, plate.psnr_g_db, plate.psnr_b_db, plate_reconstruct.cpsnr_db, best_ext
        );
        images.push(ImageResult { stem, width: w, height: h, demosaic, plate, plate_reconstruct });
    }

    let report = Report {
        phase: a.phase.clone(),
        border: a.border,
        scale: a.scale,
        blur: a.blur,
        from_raw: !a.raw.is_empty(),
        tools,
        methods: METHODS.iter().map(|m| m.name).collect(),
        images,
    };
    std::fs::write(a.out.join("plate.json"), serde_json::to_string_pretty(&report)?)?;
    let md = markdown(&report);
    std::fs::write(a.out.join("report.md"), &md)?;
    print!("{md}");
    Ok(())
}
