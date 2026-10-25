//! `mimizan synth` and `mimizan measure`: the synthetic bench and the
//! measurements on grey TIFFs.

use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use mimizan_core::bench::{markdown, run_scene_with};
use mimizan_core::cfa::BayerPhase;
use mimizan_core::decode::Rect;
use mimizan_core::icc::GrayTrc;
use mimizan_core::ingest::NoiseModel;
use mimizan_core::metrics::{carrier_energy, region_stats, star_mtf, wedge_fit, StarReport};
use mimizan_core::plane::Plane;
use mimizan_core::separate::SeparateParams;
use mimizan_core::synth::{self, Scene, SynthParams};
use mimizan_core::tiffin::read_gray;
use mimizan_core::tiffout::{write_gray16, TiffMeta};
use std::path::{Path, PathBuf};

/// Smallest `--size` the scenes make sense at: the wedge needs 16 patches
/// of at least 32 px for its insets, the patches scene 6 of 32.
pub const SYNTH_MIN_SIZE: usize = 512;

#[derive(Args)]
pub struct SynthArgs {
    /// star | zoneplate | edge | patches | wedge | fabric | alias
    #[arg(long, default_value = "star")]
    pub scene: String,
    /// Side of the square scene, px (at least 512)
    #[arg(long, default_value_t = 1024)]
    pub size: usize,
    /// Bayer phase: RGGB, BGGR, GRBG or GBRG
    #[arg(long, default_value = "RGGB")]
    pub phase: String,
    /// Raw channel gains r,g,b before mosaicing (positive)
    #[arg(long, default_value = "0.5,1.0,0.7")]
    pub gains: String,
    /// Noise model a,b or "none"
    #[arg(long, default_value = "0.005,0.0002")]
    pub noise: String,
    /// Seed of the noise generator
    #[arg(long, default_value_t = 1)]
    pub seed: u64,
    /// Output directory (mosaic.tif, truth.tif, previews)
    #[arg(short, long)]
    pub out: PathBuf,
}

#[derive(Subcommand)]
pub enum MeasureCmd {
    /// Run the synthetic bench (all scenes or one) and print a table
    Synth {
        /// Scene name (star, zoneplate, edge, patches, wedge, fabric, alias) or "all"
        #[arg(long, default_value = "all")]
        scene: String,
        /// Side of the square scenes, px (at least 512)
        #[arg(long, default_value_t = 1024)]
        size: usize,
        /// Bayer phase: RGGB, BGGR, GRBG or GBRG
        #[arg(long, default_value = "RGGB")]
        phase: String,
        /// Raw channel gains r,g,b before mosaicing (positive)
        #[arg(long, default_value = "0.5,1.0,0.7")]
        gains: String,
        /// Noise model a,b or "none"
        #[arg(long, default_value = "0.005,0.0002")]
        noise: String,
        /// Seed of the noise generator
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// dubois | adaptive | off
        #[arg(long, default_value = "dubois")]
        mask: String,
        /// Chrominance cut-off, c/px (adaptive and off only; dubois has its own bands)
        #[arg(long, default_value_t = 0.15)]
        cutoff: f64,
        /// Cross-term refinement rounds, 1 = single pass (adaptive only)
        #[arg(long, default_value_t = 1)]
        rounds: usize,
        /// Print the results as JSON instead of a Markdown table
        #[arg(long)]
        json: bool,
        /// Write mimizan/baseline/truth PNG previews into this directory
        #[arg(long)]
        previews: Option<PathBuf>,
    },
    /// Siemens star MTF on a grey TIFF
    Star {
        tif: PathBuf,
        /// Centre "x,y" in pixels
        #[arg(long)]
        center: String,
        /// Radii "rmin,rmax" in pixels, rmin < rmax
        #[arg(long)]
        radius: String,
        /// Spokes of the star
        #[arg(long, default_value_t = 72)]
        spokes: usize,
        /// Print the report as JSON
        #[arg(long)]
        json: bool,
    },
    /// Carrier energy relative to noise inside a rectangle of a grey TIFF
    Carrier {
        tif: PathBuf,
        /// "x,y,w,h"
        #[arg(long)]
        rect: Option<String>,
        /// Noise model a,b (normalised units)
        #[arg(long, default_value = "0.005,0.0002")]
        noise: String,
    },
    /// Pixel difference between two grey TIFFs (regression checks)
    Diff {
        a: PathBuf,
        b: PathBuf,
        /// Write |a − b| as PNG (scaled so --diff-scale steps map to white)
        #[arg(long)]
        out: Option<PathBuf>,
        /// Code-value difference that maps to white in the PNG
        #[arg(long, default_value_t = 256.0)]
        diff_scale: f64,
        /// Magnification of the PNG (nearest neighbour)
        #[arg(long, default_value_t = 4)]
        preview_scale: usize,
    },
    /// 1:1 crop as PNG (inspection). Grey TIFFs are taken as linear and
    /// gamma-encoded; JPEG/PNG are already encoded and copied as they are.
    Crop {
        tif: PathBuf,
        /// "x,y,w,h"
        #[arg(long)]
        rect: String,
        #[arg(short, long)]
        out: PathBuf,
        /// Linear gain before encoding
        #[arg(long, default_value_t = 1.0)]
        gain: f64,
        /// The TIFF is already gamma-encoded (a print file): copy values as they are
        #[arg(long)]
        encoded: bool,
    },
    /// Grey wedge linearity: patch rectangles and relative exposures
    Wedge {
        tif: PathBuf,
        /// Patches "x,y,w,h;x,y,w,h;..."
        #[arg(long)]
        patches: String,
        /// Relative exposures "1,0.5,0.25,..." (same count as patches)
        #[arg(long)]
        exposures: String,
    },
}

/// Comma-separated numbers.
pub fn parse_list(s: &str) -> Result<Vec<f64>> {
    s.split(',')
        .map(|t| t.trim().parse::<f64>().with_context(|| format!("'{}' is not a number", t.trim())))
        .collect()
}

/// `"a,b"`.
pub fn parse_pair(s: &str) -> Result<(f64, f64)> {
    let v = parse_list(s)?;
    if v.len() != 2 {
        bail!("expected two numbers, got '{s}'");
    }
    Ok((v[0], v[1]))
}

/// `"x,y,w,h"` in whole pixels.
pub fn parse_rect(s: &str) -> Result<Rect> {
    let v: Vec<usize> =
        s.split(',').map(|t| t.trim().parse::<usize>()).collect::<std::result::Result<_, _>>()?;
    if v.len() != 4 {
        bail!("expected x,y,w,h, got '{s}'");
    }
    Ok(Rect { x: v[0], y: v[1], w: v[2], h: v[3] })
}

/// `"a,b"` of a noise model, or `"none"`. Both must be zero or positive.
fn parse_noise(s: &str) -> Result<Option<NoiseModel>> {
    if s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let (a, b) = parse_pair(s)?;
    if !(a.is_finite() && a >= 0.0 && b.is_finite() && b >= 0.0) {
        bail!("noise a,b must be zero or positive, got '{s}'");
    }
    Ok(Some(NoiseModel { a, b }))
}

/// `"r,g,b"` channel gains; the balance divides by them, so they must be
/// positive and finite.
fn parse_gains(s: &str) -> Result<[f64; 3]> {
    let v = parse_list(s)?;
    if v.len() != 3 {
        bail!("gains need r,g,b");
    }
    if v.iter().any(|g| !(g.is_finite() && *g > 0.0)) {
        bail!("gains must be positive, got '{s}'");
    }
    Ok([v[0], v[1], v[2]])
}

fn synth_params(phase: &str, gains: &str, noise: &str, seed: u64) -> Result<SynthParams> {
    Ok(SynthParams {
        phase: BayerPhase::from_name(phase)?,
        gains: parse_gains(gains)?,
        noise: parse_noise(noise)?,
        seed,
    })
}

fn check_size(size: usize) -> Result<()> {
    if size < SYNTH_MIN_SIZE {
        bail!("--size must be at least {SYNTH_MIN_SIZE} (the wedge and patch insets need it)");
    }
    Ok(())
}

/// `"rmin,rmax"` with `rmin < rmax`, both zero or positive.
fn parse_radii(s: &str) -> Result<(f64, f64)> {
    let (rmin, rmax) = parse_pair(s)?;
    if !(rmin.is_finite() && rmax.is_finite() && rmin >= 0.0 && rmin < rmax) {
        bail!("--radius wants rmin,rmax with 0 <= rmin < rmax, got '{s}'");
    }
    Ok((rmin, rmax))
}

/// A JPEG/PNG as a grey plane in [0,1], encoded as stored.
pub fn read_gray_image(path: &Path) -> Result<Plane> {
    let img = image::open(path).with_context(|| format!("reading {}", path.display()))?.to_luma16();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let data: Vec<f64> = img.as_raw().iter().map(|&v| f64::from(v) / 65535.0).collect();
    Ok(Plane::try_from_vec(w, h, data)?)
}

/// Print a star report as a table.
pub fn print_star(rep: &StarReport) {
    println!("MTF50 axis {:?}  diag {:?}", rep.mtf50_axis, rep.mtf50_diag);
    println!("MTF10 axis {:?}  diag {:?}", rep.mtf10_axis, rep.mtf10_diag);
    println!("ring axis {}  diag {}", rep.ring_axis, rep.ring_diag);
    println!("(contrast relative to the outermost ring)");
    println!("radius  freq    axis    diag    all");
    for s in &rep.samples {
        println!("{:7.1} {:6.3} {:7.3} {:7.3} {:7.3}", s.radius, s.freq, s.axis, s.diag, s.all);
    }
}

/// Star MTF of a plane from the command-line `center` and `radius` strings.
pub fn star_report(plane: &Plane, center: &str, radius: &str, spokes: usize) -> Result<StarReport> {
    let (cx, cy) = parse_pair(center)?;
    let (rmin, rmax) = parse_radii(radius)?;
    Ok(star_mtf(plane, cx, cy, spokes, rmin, rmax))
}

pub fn synth(a: &SynthArgs) -> Result<()> {
    let scene = Scene::parse(&a.scene).with_context(|| format!("unknown scene '{}'", a.scene))?;
    check_size(a.size)?;
    let sp = synth_params(&a.phase, &a.gains, &a.noise, a.seed)?;
    std::fs::create_dir_all(&a.out)?;
    let s = synth::mosaic(scene, a.size, a.size, &sp);
    let meta = |d: &'static str| TiffMeta { description: d, trc: GrayTrc::Linear, orientation: 1, dpi: None };
    write_gray16(&a.out.join("mosaic.tif"), &s.mosaic.plane, &meta("mimizan synth mosaic"))?;
    write_gray16(&a.out.join("truth.tif"), &s.l_true, &meta("mimizan synth L_true"))?;
    mimizan_core::preview::write_png(&s.mosaic.plane, &a.out.join("mosaic.png"), 1, true)?;
    mimizan_core::preview::write_png(&s.l_true, &a.out.join("truth.png"), 1, true)?;
    println!("wrote {}", a.out.display());
    Ok(())
}

pub fn measure(cmd: &MeasureCmd) -> Result<()> {
    match cmd {
        MeasureCmd::Synth {
            scene,
            size,
            phase,
            gains,
            noise,
            seed,
            mask,
            cutoff,
            rounds,
            json,
            previews,
        } => {
            check_size(*size)?;
            let sp = synth_params(phase, gains, noise, *seed)?;
            let p = SeparateParams {
                cutoff: *cutoff,
                rounds_max: *rounds,
                mask: crate::cmd::parse_mask(mask)?,
                ..SeparateParams::default()
            };
            crate::cmd::warn_inert_flags(&p, *cutoff, *rounds);
            let scenes: Vec<Scene> = if scene.eq_ignore_ascii_case("all") {
                Scene::ALL.to_vec()
            } else {
                vec![Scene::parse(scene).with_context(|| format!("unknown scene '{scene}'"))?]
            };
            if let Some(dir) = previews {
                std::fs::create_dir_all(dir)?;
            }
            let mut results = Vec::new();
            for sc in scenes {
                // The previews come out of the same run as the numbers.
                let mut preview_err: Option<anyhow::Error> = None;
                let r = run_scene_with(sc, *size, *size, &sp, &p, |s, sep| {
                    let Some(dir) = previews else { return };
                    if preview_err.is_some() {
                        return;
                    }
                    let base = mimizan_core::baseline::bilinear_luminance(&s.mosaic.plane, sp.phase);
                    let tag = sc.name();
                    let png = |plane: &Plane, suffix: &str, encode: bool| {
                        mimizan_core::preview::write_png(
                            plane,
                            &dir.join(format!("{tag}_{suffix}.png")),
                            1,
                            encode,
                        )
                    };
                    let written = png(&sep.lum, "mimizan", true)
                        .and_then(|_| png(&base, "baseline", true))
                        .and_then(|_| png(&s.l_true, "truth", true))
                        .and_then(|_| png(&sep.mask_max, "mask", false));
                    if let Err(e) = written {
                        preview_err = Some(e.into());
                    }
                });
                if let Some(e) = preview_err {
                    return Err(e);
                }
                results.push(r);
            }
            if *json {
                println!("{}", serde_json::to_string_pretty(&results)?);
            } else {
                print!("{}", markdown(&results));
            }
            Ok(())
        }
        MeasureCmd::Star { tif, center, radius, spokes, json } => {
            let g = read_gray(tif)?;
            let rep = star_report(&g.plane, center, radius, *spokes)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&rep)?);
            } else {
                print_star(&rep);
            }
            Ok(())
        }
        MeasureCmd::Carrier { tif, rect, noise } => {
            let g = read_gray(tif)?;
            let r = rect.as_deref().map(parse_rect).transpose()?;
            let nm = parse_noise(noise)?.unwrap_or_default();
            let rep = carrier_energy(&g.plane, &nm, r, 0.15);
            let (mean, sd) =
                region_stats(&g.plane, r.unwrap_or(Rect { x: 0, y: 0, w: g.plane.width, h: g.plane.height }));
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "carrier_over_noise": {"c1_half_half": rep.ratio[0], "c2_half_zero": rep.ratio[1], "c2_zero_half": rep.ratio[2]},
                    "blocks": rep.blocks, "region_mean": mean, "region_std": sd, "model_sigma": nm.sigma(mean)
                }))?
            );
            Ok(())
        }
        MeasureCmd::Diff { a, b, out, diff_scale, preview_scale } => {
            let (pa, pb) = (read_gray(a)?, read_gray(b)?);
            if (pa.plane.width, pa.plane.height) != (pb.plane.width, pb.plane.height) {
                bail!(
                    "sizes differ: {}x{} vs {}x{}",
                    pa.plane.width,
                    pa.plane.height,
                    pb.plane.width,
                    pb.plane.height
                );
            }
            if pa.bits != pb.bits {
                bail!("bit depths differ: {} vs {}", pa.bits, pb.bits);
            }
            if diff_scale.is_nan() || *diff_scale <= 0.0 {
                bail!("--diff-scale must be positive");
            }
            let scale = if pa.bits == 16 { 65535.0 } else { 255.0 };
            if let Some(o) = out {
                let d: Vec<f64> = pa
                    .plane
                    .data
                    .iter()
                    .zip(&pb.plane.data)
                    .map(|(x, y)| ((x - y).abs() * scale / diff_scale).min(1.0))
                    .collect();
                let dp = Plane::try_from_vec(pa.plane.width, pa.plane.height, d)?;
                mimizan_core::preview::write_png(&dp, o, *preview_scale, false)?;
            }
            let (mut max, mut sum2, mut differing) = (0.0f64, 0.0f64, 0usize);
            for (x, y) in pa.plane.data.iter().zip(&pb.plane.data) {
                let d = (x - y).abs() * scale;
                if d > 0.0 {
                    differing += 1;
                }
                max = max.max(d);
                sum2 += d * d;
            }
            let n = pa.plane.data.len();
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "pixels": n, "differing": differing,
                    "max_steps": max, "rms_steps": (sum2 / n.max(1) as f64).sqrt(),
                    "identical": differing == 0
                }))?
            );
            Ok(())
        }
        MeasureCmd::Crop { tif, rect, out, gain, encoded } => {
            let is_tiff = tif
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"));
            let (mut plane, encode) =
                if is_tiff { (read_gray(tif)?.plane, !*encoded) } else { (read_gray_image(tif)?, false) };
            if *gain != 1.0 {
                plane.data.iter_mut().for_each(|v| *v *= gain);
            }
            let r = parse_rect(rect)?;
            mimizan_core::preview::write_png_crop(&plane, out, r.x, r.y, r.w, r.h, encode)?;
            Ok(())
        }
        MeasureCmd::Wedge { tif, patches, exposures } => {
            let g = read_gray(tif)?;
            let rects: Vec<Rect> = patches.split(';').map(parse_rect).collect::<Result<_>>()?;
            let exps = parse_list(exposures)?;
            if rects.len() != exps.len() {
                bail!("{} patches but {} exposures", rects.len(), exps.len());
            }
            let samples: Vec<(f64, f64)> =
                rects.iter().zip(&exps).map(|(r, e)| (*e, region_stats(&g.plane, *r).0)).collect();
            let fit = wedge_fit(&samples);
            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({"fit": fit, "samples": samples}))?
            );
            Ok(())
        }
    }
}
