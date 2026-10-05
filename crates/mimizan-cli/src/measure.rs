use anyhow::{bail, Context, Result};
use clap::{Args, Subcommand};
use mimizan_core::bench::{markdown, run_scene};
use mimizan_core::cfa::BayerPhase;
use mimizan_core::decode::Rect;
use mimizan_core::icc::GrayTrc;
use mimizan_core::ingest::NoiseModel;
use mimizan_core::metrics::{carrier_energy, region_stats, star_mtf, wedge_fit};
use mimizan_core::separate::{MaskMode, SeparateParams};
use mimizan_core::synth::{self, Scene, SynthParams};
use mimizan_core::tiffin::read_gray;
use mimizan_core::tiffout::{write_gray16, TiffMeta};
use std::path::PathBuf;

#[derive(Args)]
pub struct SynthArgs {
    /// star | zoneplate | edge | patches | wedge | fabric
    #[arg(long, default_value = "star")]
    pub scene: String,
    #[arg(long, default_value_t = 1024)]
    pub size: usize,
    #[arg(long, default_value = "RGGB")]
    pub phase: String,
    /// Raw channel gains r,g,b before mosaicing
    #[arg(long, default_value = "0.5,1.0,0.7")]
    pub gains: String,
    /// Noise model a,b or "none"
    #[arg(long, default_value = "0.005,0.0002")]
    pub noise: String,
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
        /// Scene name or "all"
        #[arg(long, default_value = "all")]
        scene: String,
        #[arg(long, default_value_t = 1024)]
        size: usize,
        #[arg(long, default_value = "RGGB")]
        phase: String,
        #[arg(long, default_value = "0.5,1.0,0.7")]
        gains: String,
        #[arg(long, default_value = "0.005,0.0002")]
        noise: String,
        #[arg(long, default_value_t = 1)]
        seed: u64,
        /// dubois | adaptive | off
        #[arg(long, default_value = "dubois")]
        mask: String,
        #[arg(long, default_value_t = 0.15)]
        cutoff: f64,
        /// Cross-term refinement rounds (1 = single pass)
        #[arg(long, default_value_t = 1)]
        rounds: usize,
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
        /// Radii "rmin,rmax" in pixels
        #[arg(long)]
        radius: String,
        #[arg(long, default_value_t = 72)]
        spokes: usize,
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
        #[arg(long, default_value_t = 256.0)]
        diff_scale: f64,
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

pub fn parse_pair(s: &str) -> Result<(f64, f64)> {
    let v: Vec<f64> = s.split(',').map(|t| t.trim().parse::<f64>()).collect::<std::result::Result<_, _>>()?;
    if v.len() != 2 {
        bail!("expected two numbers, got '{s}'");
    }
    Ok((v[0], v[1]))
}

pub fn parse_rect(s: &str) -> Result<Rect> {
    let v: Vec<usize> =
        s.split(',').map(|t| t.trim().parse::<usize>()).collect::<std::result::Result<_, _>>()?;
    if v.len() != 4 {
        bail!("expected x,y,w,h, got '{s}'");
    }
    Ok(Rect { x: v[0], y: v[1], w: v[2], h: v[3] })
}

fn parse_noise(s: &str) -> Result<Option<NoiseModel>> {
    if s.eq_ignore_ascii_case("none") {
        return Ok(None);
    }
    let (a, b) = parse_pair(s)?;
    Ok(Some(NoiseModel { a, b }))
}

fn parse_gains(s: &str) -> Result<[f64; 3]> {
    let v: Vec<f64> = s.split(',').map(|t| t.trim().parse::<f64>()).collect::<std::result::Result<_, _>>()?;
    if v.len() != 3 {
        bail!("gains need r,g,b");
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

pub fn synth(a: &SynthArgs) -> Result<()> {
    let scene = Scene::parse(&a.scene).with_context(|| format!("unknown scene '{}'", a.scene))?;
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
            let sp = synth_params(phase, gains, noise, *seed)?;
            let p = SeparateParams {
                cutoff: *cutoff,
                rounds_max: *rounds,
                mask: match mask.to_ascii_lowercase().as_str() {
                    "off" => MaskMode::Off,
                    "adaptive" | "on" => MaskMode::Adaptive,
                    "dubois" => MaskMode::Dubois,
                    o => bail!("unknown mask mode '{o}' (dubois|adaptive|off)"),
                },
                ..SeparateParams::default()
            };
            let scenes: Vec<Scene> = if scene.eq_ignore_ascii_case("all") {
                Scene::ALL.to_vec()
            } else {
                vec![Scene::parse(scene).with_context(|| format!("unknown scene '{scene}'"))?]
            };
            let mut results = Vec::new();
            for sc in scenes {
                let r = run_scene(sc, *size, *size, &sp, &p);
                if let Some(dir) = previews {
                    std::fs::create_dir_all(dir)?;
                    let s = synth::mosaic(sc, *size, *size, &sp);
                    let sep = mimizan_core::separate::separate(&s.mosaic, &p);
                    let base = mimizan_core::baseline::bilinear_luminance(&s.mosaic.plane, sp.phase);
                    let tag = format!("{sc:?}").to_ascii_lowercase();
                    mimizan_core::preview::write_png(
                        &sep.lum,
                        &dir.join(format!("{tag}_mimizan.png")),
                        1,
                        true,
                    )?;
                    mimizan_core::preview::write_png(
                        &base,
                        &dir.join(format!("{tag}_baseline.png")),
                        1,
                        true,
                    )?;
                    mimizan_core::preview::write_png(
                        &s.l_true,
                        &dir.join(format!("{tag}_truth.png")),
                        1,
                        true,
                    )?;
                    mimizan_core::preview::write_png(
                        &sep.mask_max,
                        &dir.join(format!("{tag}_mask.png")),
                        1,
                        false,
                    )?;
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
            let (cx, cy) = parse_pair(center)?;
            let (rmin, rmax) = parse_pair(radius)?;
            let rep = star_mtf(&g.plane, cx, cy, *spokes, rmin, rmax);
            if *json {
                println!("{}", serde_json::to_string_pretty(&rep)?);
            } else {
                println!("MTF50 axis {:?}  diag {:?}", rep.mtf50_axis, rep.mtf50_diag);
                println!("MTF10 axis {:?}  diag {:?}", rep.mtf10_axis, rep.mtf10_diag);
                println!("ring axis {}  diag {}", rep.ring_axis, rep.ring_diag);
                println!("radius  freq    axis    diag    all");
                for s in &rep.samples {
                    println!("{:7.1} {:6.3} {:7.3} {:7.3} {:7.3}", s.radius, s.freq, s.axis, s.diag, s.all);
                }
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
            if let Some(o) = out {
                let scale = if pa.bits == 16 { 65535.0 } else { 255.0 };
                let d: Vec<f64> = pa
                    .plane
                    .data
                    .iter()
                    .zip(&pb.plane.data)
                    .map(|(x, y)| ((x - y).abs() * scale / diff_scale).min(1.0))
                    .collect();
                let dp = mimizan_core::plane::Plane::from_vec(pa.plane.width, pa.plane.height, d);
                mimizan_core::preview::write_png(&dp, o, *preview_scale, false)?;
            }
            if (pa.plane.width, pa.plane.height) != (pb.plane.width, pb.plane.height) {
                bail!(
                    "sizes differ: {}x{} vs {}x{}",
                    pa.plane.width,
                    pa.plane.height,
                    pb.plane.width,
                    pb.plane.height
                );
            }
            let scale = if pa.bits == 16 { 65535.0 } else { 255.0 };
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
                    "max_steps": max, "rms_steps": (sum2 / n as f64).sqrt(),
                    "identical": differing == 0
                }))?
            );
            Ok(())
        }
        MeasureCmd::Crop { tif, rect, out, gain, encoded } => {
            let is_tiff = tif
                .extension()
                .is_some_and(|e| e.eq_ignore_ascii_case("tif") || e.eq_ignore_ascii_case("tiff"));
            let (mut plane, encode) = if is_tiff {
                (read_gray(tif)?.plane, !*encoded)
            } else {
                let img = image::open(tif).with_context(|| format!("reading {}", tif.display()))?.to_luma16();
                let (w, h) = (img.width() as usize, img.height() as usize);
                let data: Vec<f64> = img.as_raw().iter().map(|&v| f64::from(v) / 65535.0).collect();
                (mimizan_core::plane::Plane::from_vec(w, h, data), false)
            };
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
            let exps: Vec<f64> = exposures
                .split(',')
                .map(|t| t.trim().parse::<f64>())
                .collect::<std::result::Result<_, _>>()?;
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
