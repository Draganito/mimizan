//! `mimizan calibrate`: look curve from a grey wedge, mix weights from a
//! colour chart, star MTF into the camera file.

use crate::measure::{parse_list, parse_rect, print_star, read_gray_image, star_report};
use anyhow::{bail, Context, Result};
use clap::Subcommand;
use mimizan_core::calib::{CameraFile, LookEncoding, StarCalib};
use mimizan_core::calibrate::{self, fit_look, fit_weights, grid_rects, patch_mean, patch_rgb, RefEncoding};
use mimizan_core::decode::{Rect, SensorKind};
use mimizan_core::ingest::{ingest, IngestParams};
use mimizan_core::metrics::wedge_fit;
use mimizan_core::mix::WeightSpace;
use mimizan_core::tiffin::read_gray;
use std::path::PathBuf;
use tracing::{info, warn};

#[derive(Subcommand)]
pub enum CalibrateCmd {
    /// Look curve from a grey wedge: negative TIFF patch means vs. reference JPEG
    Wedge {
        /// Linear negative TIFF (mimizan negative) of the wedge
        negative: Option<PathBuf>,
        /// Reference image (out-of-camera JPEG of the monochrome reference camera) of the same wedge
        #[arg(long)]
        reference: Option<PathBuf>,
        /// Patches on the negative "x,y,w,h;x,y,w,h;..."
        #[arg(long)]
        patches: Option<String>,
        /// Patch grid on the negative "x,y,w,h,cols,rows" (cells shrunk by --inset)
        #[arg(long)]
        grid: Option<String>,
        /// Patches on the reference (default: same as negative)
        #[arg(long)]
        ref_patches: Option<String>,
        /// Patch grid on the reference (default: same as negative)
        #[arg(long)]
        ref_grid: Option<String>,
        #[arg(long, default_value_t = 0.25)]
        inset: f64,
        /// Relative exposures per patch "1,0.5,..." for the linearity check
        #[arg(long)]
        exposures: Option<String>,
        /// Linear gain on the negative before the fit (mid-grey anchor)
        #[arg(long, default_value_t = 1.0)]
        gain: f64,
        /// gamma22 | none
        #[arg(long, default_value = "gamma22")]
        encoding: String,
        #[arg(long, default_value = "fitted")]
        name: String,
        /// Output look/*.json
        #[arg(short, long)]
        out: Option<PathBuf>,
        /// Synthetic self-test instead of files (wedge scene, known camera curve)
        #[arg(long)]
        self_test: bool,
        #[arg(long, default_value_t = 512)]
        size: usize,
        #[arg(long)]
        json: bool,
    },
    /// Mix weights from colour patches: balanced RAW means vs. reference brightness
    Weights {
        /// RAW file of the colour chart
        raw: Option<PathBuf>,
        /// Reference image of the same chart (monochrome camera JPEG)
        #[arg(long)]
        reference: Option<PathBuf>,
        /// Reference brightness per patch (linear) instead of an image
        #[arg(long)]
        ref_values: Option<String>,
        /// srgb | gamma22 | none: encoding of the reference image
        #[arg(long, default_value = "srgb")]
        ref_encoding: String,
        #[arg(long)]
        patches: Option<String>,
        #[arg(long)]
        grid: Option<String>,
        #[arg(long)]
        ref_patches: Option<String>,
        #[arg(long)]
        ref_grid: Option<String>,
        #[arg(long, default_value_t = 0.25)]
        inset: f64,
        /// Channel balance: as-shot | gray | none | "r,g,b"
        #[arg(long, default_value = "as-shot")]
        wb: String,
        #[arg(long, default_value = "cameras")]
        cameras: PathBuf,
        /// Write weights into the camera file (created from defaults if missing)
        #[arg(long)]
        write: bool,
        /// Synthetic self-test with the given true weights "r,g,b"
        #[arg(long)]
        self_test: Option<String>,
        #[arg(long, default_value_t = 384)]
        size: usize,
        #[arg(long)]
        json: bool,
    },
    /// Star MTF of a negative, recorded in the camera file
    Star {
        negative: PathBuf,
        /// Centre "x,y" in negative pixels
        #[arg(long)]
        center: String,
        /// Radii "rmin,rmax" in pixels
        #[arg(long)]
        radius: String,
        #[arg(long, default_value_t = 72)]
        spokes: usize,
        /// Explicit camera file (default: from the negative's description + --cameras)
        #[arg(long)]
        camera: Option<PathBuf>,
        #[arg(long, default_value = "cameras")]
        cameras: PathBuf,
        /// Also set usm_amount in the camera file
        #[arg(long)]
        usm_amount: Option<f64>,
        #[arg(long)]
        write: bool,
        #[arg(long)]
        json: bool,
    },
}

fn parse_grid(s: &str, inset: f64) -> Result<Vec<Rect>> {
    let v: Vec<usize> =
        s.split(',').map(|t| t.trim().parse::<usize>()).collect::<std::result::Result<_, _>>()?;
    if v.len() != 6 {
        bail!("grid needs x,y,w,h,cols,rows, got '{s}'");
    }
    Ok(grid_rects(Rect { x: v[0], y: v[1], w: v[2], h: v[3] }, v[4], v[5], inset))
}

fn rects_from(patches: Option<&str>, grid: Option<&str>, inset: f64) -> Result<Option<Vec<Rect>>> {
    match (patches, grid) {
        (Some(p), _) => Ok(Some(p.split(';').map(parse_rect).collect::<Result<_>>()?)),
        (None, Some(g)) => Ok(Some(parse_grid(g, inset)?)),
        (None, None) => Ok(None),
    }
}

/// Share of a patch the saturation sidecar marks as clipped (255). Pixels
/// outside the sidecar count as clean.
fn saturated_fraction(sidecar: &(usize, usize, Vec<u8>), r: Rect) -> f64 {
    let (mw, mh, data) = sidecar;
    let hits = (r.y..r.y + r.h)
        .filter(|y| y < mh)
        .flat_map(|y| (r.x..r.x + r.w).filter(|x| x < mw).map(move |x| (x, y)))
        .filter(|(x, y)| data.get(y * mw + x).is_some_and(|&v| v == 255))
        .count();
    hits as f64 / (r.w * r.h).max(1) as f64
}

pub fn calibrate(cmd: &CalibrateCmd) -> Result<()> {
    match cmd {
        CalibrateCmd::Wedge {
            negative,
            reference,
            patches,
            grid,
            ref_patches,
            ref_grid,
            inset,
            exposures,
            gain,
            encoding,
            name,
            out,
            self_test,
            size,
            json,
        } => {
            if *self_test {
                let r = calibrate::wedge_self_test(*size, true);
                if *json {
                    println!("{}", serde_json::to_string_pretty(&r)?);
                } else {
                    println!("wedge self-test ({size} px, noise on)");
                    println!("  fit error      {:.2}/255", r.fit_max_err_255);
                    println!("  hold-out error {:.2}/255 (between the steps)", r.holdout_max_err_255);
                    println!(
                        "  linearity      slope {:.4}, max residual {:.2} %",
                        r.linearity_slope,
                        100.0 * r.linearity_max_rel_residual
                    );
                    println!("  {}", if r.pass { "PASS" } else { "FAIL" });
                }
                return Ok(());
            }
            let neg_path = negative.as_ref().context("negative TIFF required (or --self-test)")?;
            let ref_path = reference.as_ref().context("--reference required")?;
            let enc = match encoding.to_ascii_lowercase().as_str() {
                "gamma22" => LookEncoding::Gamma22,
                "none" | "linear" => LookEncoding::None,
                o => bail!("encoding '{o}' is gamma22|none"),
            };
            let rects = rects_from(patches.as_deref(), grid.as_deref(), *inset)?
                .context("--patches or --grid required")?;
            let ref_rects = rects_from(ref_patches.as_deref(), ref_grid.as_deref(), *inset)?
                .unwrap_or_else(|| rects.clone());
            if rects.len() != ref_rects.len() {
                bail!("{} negative patches but {} reference patches", rects.len(), ref_rects.len());
            }
            let neg = read_gray(neg_path)?;
            let mask_path = mimizan_core::print::sidecar_path(neg_path);
            // A sidecar of another size belongs to another negative.
            let sat = mimizan_core::tiffin::read_mask8(&mask_path)
                .ok()
                .filter(|(mw, mh, _)| (*mw, *mh) == (neg.plane.width, neg.plane.height));
            let refp = read_gray_image(ref_path).context("reference")?;
            let mut pairs = Vec::with_capacity(rects.len());
            let mut n_sat = 0;
            for (r, q) in rects.iter().zip(&ref_rects) {
                let mut m = patch_mean(&neg.plane, *r) * gain;
                if sat.as_ref().is_some_and(|s| saturated_fraction(s, *r) > 0.01) {
                    n_sat += 1;
                    m = f64::NAN;
                }
                pairs.push((m, patch_mean(&refp, *q)));
            }
            if n_sat > 0 {
                warn!("{n_sat} patches saturated in the negative, dropped");
            }
            let fit = fit_look(
                name,
                &pairs,
                enc,
                Some(ref_path.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()),
            );
            let lin = match exposures {
                Some(e) => {
                    let ex = parse_list(e)?;
                    if ex.len() != pairs.len() {
                        bail!("{} exposures but {} patches", ex.len(), pairs.len());
                    }
                    Some(wedge_fit(&ex.iter().zip(&pairs).map(|(e, p)| (*e, p.0)).collect::<Vec<_>>()))
                }
                None => None,
            };
            if *json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({"fit": fit, "linearity": lin}))?
                );
            } else {
                println!("{:>3} {:>10} {:>10} {:>10}", "#", "neg lin", "ref", "err/255");
                let curve = mimizan_core::curve::Curve::from_look(&fit.look);
                for (i, (n, r)) in pairs.iter().enumerate() {
                    if n.is_finite() {
                        println!("{i:>3} {n:>10.5} {r:>10.4} {:>10.2}", (curve.eval(*n) - r).abs() * 255.0);
                    } else {
                        println!("{i:>3} {:>10} {r:>10.4} {:>10}", "sat", "-");
                    }
                }
                println!(
                    "max {:.2}/255, rms {:.2}/255, {} points, {}",
                    fit.max_err_255,
                    fit.rms_err_255,
                    fit.look.points.len(),
                    if fit.pass { "PASS" } else { "FAIL (> 3/255)" }
                );
                if let Some(l) = &lin {
                    println!(
                        "linearity slope {:.4}, max residual {:.2} % -> {}",
                        l.slope,
                        100.0 * l.max_rel_residual,
                        if l.pass { "PASS" } else { "FAIL" }
                    );
                }
            }
            if let Some(o) = out {
                fit.look.validate()?;
                fit.look.save(o)?;
                info!("wrote {}", o.display());
            }
            Ok(())
        }
        CalibrateCmd::Weights {
            raw,
            reference,
            ref_values,
            ref_encoding,
            patches,
            grid,
            ref_patches,
            ref_grid,
            inset,
            wb,
            cameras,
            write,
            self_test,
            size,
            json,
        } => {
            if let Some(t) = self_test {
                let v = parse_list(t)?;
                if v.len() != 3 {
                    bail!("--self-test needs r,g,b");
                }
                let r = calibrate::weights_self_test(*size, [v[0], v[1], v[2]], true);
                if *json {
                    println!("{}", serde_json::to_string_pretty(&r)?);
                } else {
                    println!("weights self-test ({size} px, noise on)");
                    println!("  truth  {:.3?}", r.truth);
                    println!("  fitted {:.3?}", r.fitted);
                    println!("  max |dev| {:.4}, RMSE {:.2} %", r.max_abs_dev, 100.0 * r.rmse_rel);
                    println!("  {}", if r.pass { "PASS" } else { "FAIL" });
                }
                return Ok(());
            }
            let raw_path = raw.as_ref().context("RAW file required (or --self-test)")?;
            let rects = rects_from(patches.as_deref(), grid.as_deref(), *inset)?
                .context("--patches or --grid required")?;
            let renc = RefEncoding::parse(ref_encoding).context("--ref-encoding is srgb|gamma22|none")?;
            let refs: Vec<f64> = match (ref_values, reference) {
                (Some(v), _) => parse_list(v)?,
                (None, Some(rp)) => {
                    let ref_rects = rects_from(ref_patches.as_deref(), ref_grid.as_deref(), *inset)?
                        .unwrap_or_else(|| rects.clone());
                    let p = read_gray_image(rp).context("reference")?;
                    ref_rects.iter().map(|q| renc.to_linear(patch_mean(&p, *q))).collect()
                }
                (None, None) => bail!("--reference or --ref-values required"),
            };
            if refs.len() != rects.len() {
                bail!("{} reference values but {} patches", refs.len(), rects.len());
            }
            let frame = mimizan_core::pipeline::decode(raw_path)?;
            let SensorKind::Bayer(phase) = frame.kind else {
                bail!("weights need a Bayer sensor");
            };
            let params = IngestParams { wb: crate::cmd::parse_wb(wb)?, ..IngestParams::default() };
            let mosaic = ingest(&frame, &params)?;
            info!("balance {} R {:.4} B {:.4}", mosaic.report.wb_source, mosaic.wb[0], mosaic.wb[2]);
            let mut samples = Vec::with_capacity(rects.len());
            let mut dropped = 0;
            for (r, v) in rects.iter().zip(&refs) {
                let (rgb, sat) = patch_rgb(&mosaic, *r);
                if sat > 0.01 || rgb.iter().any(|x| !x.is_finite()) {
                    dropped += 1;
                    continue;
                }
                samples.push((rgb, *v));
            }
            if dropped > 0 {
                warn!("{dropped} patches saturated or empty, dropped");
            }
            let fit = fit_weights(&samples);
            // The fit lives in the balanced space of this frame; the camera
            // file stores the illuminant-independent raw-channel form.
            let raw_w = fit.weights.balanced_to_raw(mosaic.wb);
            if *json {
                let mut v = serde_json::to_value(&fit)?;
                v["weights_raw"] = serde_json::to_value(raw_w)?;
                v["wb"] = serde_json::to_value(mosaic.wb)?;
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                println!("{:>3} {:>8} {:>8} {:>8} {:>8} {:>8}", "#", "R", "G", "B", "ref", "res %");
                for (i, ((rgb, v), res)) in samples.iter().zip(&fit.residuals).enumerate() {
                    println!(
                        "{i:>3} {:>8.4} {:>8.4} {:>8.4} {v:>8.4} {:>8.2}",
                        rgb[0],
                        rgb[1],
                        rgb[2],
                        100.0 * res
                    );
                }
                println!(
                    "weights R {:.3} G {:.3} B {:.3}, scale {:.4}, RMSE {:.2} % (max {:.2} %), {}",
                    fit.weights.r,
                    fit.weights.g,
                    fit.weights.b,
                    fit.scale,
                    100.0 * fit.rmse_rel,
                    100.0 * fit.max_rel,
                    if fit.pass { "PASS" } else { "FAIL (> 5 %)" }
                );
                println!(
                    "raw-channel weights R {:.3} G {:.3} B {:.3} (balance R {:.4} B {:.4}; this is what the camera file stores)",
                    raw_w.r, raw_w.g, raw_w.b, mosaic.wb[0], mosaic.wb[2]
                );
            }
            if *write {
                let path = CameraFile::path_in(cameras, &frame.clean_make, &frame.clean_model);
                let mut cam = if path.is_file() {
                    CameraFile::load(&path)?
                } else {
                    if let Some(dir) = path.parent() {
                        std::fs::create_dir_all(dir)?;
                    }
                    CameraFile::defaults(frame.clean_make.trim(), frame.clean_model.trim(), phase)
                };
                cam.weights_rgb = [raw_w.r, raw_w.g, raw_w.b];
                cam.weights_space = WeightSpace::Raw;
                cam.fitted.weights = true;
                cam.weights_fitted_against = Some(match (ref_values, reference) {
                    (Some(_), _) => "values".to_string(),
                    (None, Some(rp)) => {
                        rp.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default()
                    }
                    _ => String::new(),
                });
                cam.validate()?;
                cam.save(&path)?;
                info!("wrote {}", path.display());
            }
            Ok(())
        }
        CalibrateCmd::Star { negative, center, radius, spokes, camera, cameras, usm_amount, write, json } => {
            let g = read_gray(negative)?;
            let rep = star_report(&g.plane, center, radius, *spokes)?;
            if *json {
                println!("{}", serde_json::to_string_pretty(&rep)?);
            } else {
                print_star(&rep);
            }
            if *write {
                let path = match camera {
                    Some(p) => p.clone(),
                    None => {
                        let desc = g.description.as_deref().context("negative has no description")?;
                        let v: serde_json::Value = serde_json::from_str(desc)?;
                        let cam = v.get("camera").context("negative description has no camera")?;
                        let make = cam.get("clean_make").and_then(|m| m.as_str()).unwrap_or_default();
                        let model = cam.get("clean_model").and_then(|m| m.as_str()).unwrap_or_default();
                        if make.is_empty() && model.is_empty() {
                            bail!("negative description names no camera; give --camera");
                        }
                        CameraFile::path_in(cameras, make, model)
                    }
                };
                let mut cam =
                    CameraFile::load(&path).with_context(|| format!("camera file {}", path.display()))?;
                cam.star = Some(StarCalib {
                    mtf50_axis: rep.mtf50_axis,
                    mtf50_diag: rep.mtf50_diag,
                    mtf10_axis: rep.mtf10_axis,
                    mtf10_diag: rep.mtf10_diag,
                    ring: rep.ring_axis || rep.ring_diag,
                    source: negative.file_name().map(|s| s.to_string_lossy().to_string()).unwrap_or_default(),
                });
                if let Some(a) = usm_amount {
                    cam.usm_amount = *a;
                }
                cam.validate()?;
                cam.save(&path)?;
                info!("wrote {}", path.display());
            }
            Ok(())
        }
    }
}
