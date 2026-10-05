//! `mimizan bench kodak`: fake-Bayer benchmark against external demosaicers.
//!
//! Every picture is linearised, mosaicked (RGGB, 16 bit) and handed both to
//! this pipeline and, as a DNG, to libraw (`dcraw_emu`) and RawTherapee
//! (`rawtherapee-cli`). Their demosaiced RGB is reduced to `(R + 2G + B)/4`,
//! the same luminance the ground truth and the `Native` mix use, so the
//! numbers compare reconstruction only. Converters that are not installed
//! are skipped and the report says so.

use anyhow::{bail, Context, Result};
use clap::Args;
use mimizan_core::baseline::bilinear_luminance;
use mimizan_core::cfa::BayerPhase;
use mimizan_core::kodak::{self, Quality};
use mimizan_core::mix::{mix, Weights};
use mimizan_core::plane::Plane;
use mimizan_core::separate::{separate, MaskMode, SeparateParams};
use serde::Serialize;
use std::path::{Path, PathBuf};
use std::process::Command;

#[derive(Args)]
pub struct KodakArgs {
    /// Directory with 8-bit sRGB PNG/JPEG pictures (kodim01.png ... kodim24.png)
    #[arg(long, default_value = "testdata/kodak")]
    pub images: PathBuf,
    /// Work directory: DNGs, converter outputs, full-size results, JSON
    #[arg(long, default_value = "out/kodak")]
    pub out: PathBuf,
    /// Directory for the comparison strips referenced by the report
    #[arg(long, default_value = "docs/kodak")]
    pub strips: PathBuf,
    /// Markdown report
    #[arg(long, default_value = "docs/KODAK.md")]
    pub report: PathBuf,
    #[arg(long, default_value = "RGGB")]
    pub phase: String,
    /// Border (px) excluded from every metric
    #[arg(long, default_value_t = 16)]
    pub border: usize,
    /// Crop size (px, before 2x magnification) of the comparison strips
    #[arg(long, default_value_t = 128)]
    pub crop: usize,
    /// Comma-separated stems to run (default: every picture in the directory)
    #[arg(long)]
    pub only: Option<String>,
    /// Do not call libraw / RawTherapee
    #[arg(long)]
    pub no_external: bool,
    /// Magnify the pictures (Lanczos-3) before mosaicking: 1 = pixel-sharp
    /// literature condition, 2 = content band-limited to half Nyquist, as a
    /// lens and sensor deliver it
    #[arg(long, default_value_t = 1)]
    pub scale: usize,
    /// Real RAW files instead of pictures: each 2x2 CFA cell is binned to one
    /// RGB pixel without interpolation (half size, every channel measured),
    /// that picture is the ground truth and is mosaicked again. Real optics,
    /// real noise, real colours; the camera's noise model is used.
    #[arg(long, num_args = 1.., conflicts_with = "only")]
    pub raw: Vec<PathBuf>,
    /// Gaussian blur (sigma, px) of the ground truth before mosaicking, a
    /// lens-like MTF that keeps content up to Nyquist: 0.75 puts MTF50 at
    /// 0.25 c/px. 0 = off.
    #[arg(long, default_value_t = 0.0)]
    pub blur: f64,
}

/// One demosaicing reference.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
enum Tool {
    Bilinear,
    Libraw(u8),
    RawTherapee(&'static str),
}

#[derive(Clone, Copy, Debug, Serialize)]
struct Method {
    name: &'static str,
    tool: Tool,
}

/// Every standalone demosaicer libraw and RawTherapee 5.11 implement.
/// Dual blends (AMaZE+VNG4 and the like) are omitted: they are mixtures of
/// two entries already in this list, not a further algorithm.
const METHODS: &[Method] = &[
    Method { name: "Bilinear", tool: Tool::Bilinear },
    Method { name: "VNG (libraw)", tool: Tool::Libraw(1) },
    Method { name: "PPG (libraw)", tool: Tool::Libraw(2) },
    Method { name: "AHD (libraw)", tool: Tool::Libraw(3) },
    Method { name: "DCB (libraw)", tool: Tool::Libraw(4) },
    Method { name: "DHT (libraw)", tool: Tool::Libraw(11) },
    Method { name: "AAHD (libraw)", tool: Tool::Libraw(12) },
    Method { name: "fast (RawTherapee)", tool: Tool::RawTherapee("fast") },
    Method { name: "VNG4 (RawTherapee)", tool: Tool::RawTherapee("vng4") },
    Method { name: "AHD (RawTherapee)", tool: Tool::RawTherapee("ahd") },
    Method { name: "EAHD (RawTherapee)", tool: Tool::RawTherapee("eahd") },
    Method { name: "HPHD (RawTherapee)", tool: Tool::RawTherapee("hphd") },
    Method { name: "DCB (RawTherapee)", tool: Tool::RawTherapee("dcb") },
    Method { name: "LMMSE (RawTherapee)", tool: Tool::RawTherapee("lmmse") },
    Method { name: "IGV (RawTherapee)", tool: Tool::RawTherapee("igv") },
    Method { name: "AMaZE (RawTherapee)", tool: Tool::RawTherapee("amaze") },
    Method { name: "RCD (RawTherapee)", tool: Tool::RawTherapee("rcd") },
];

#[derive(Clone, Debug, Serialize)]
struct ImageResult {
    stem: String,
    width: usize,
    height: usize,
    /// Demosaic -> mono results in `METHODS` order, `None` where the tool is missing.
    demosaic: Vec<Option<Quality>>,
    /// Fixed separation (`--mask off`).
    mimizan_fixed: Quality,
    /// Block-spectrum mask (`--mask adaptive`).
    mimizan_block: Quality,
    /// Default pipeline (`--mask dubois`).
    mimizan: Quality,
    /// Default pipeline plus the optional nonlinear step (`--reconstruct`).
    mimizan_reconstruct: Quality,
    /// Mean β of that step: share of the picture it computed.
    computed: f64,
    /// Index into `METHODS` of the best external result on this picture.
    best: Option<usize>,
    crop: (usize, usize),
    strip: String,
}

#[derive(Clone, Debug, Serialize)]
struct Report {
    phase: String,
    border: usize,
    crop: usize,
    scale: usize,
    blur: f64,
    from_raw: bool,
    tools: Vec<(String, Option<String>)>,
    images: Vec<ImageResult>,
}

fn tool_version(cmd: &str, arg: &str) -> Option<String> {
    let mut c = Command::new(cmd);
    if !arg.is_empty() {
        c.arg(arg);
    }
    let out = c.output().ok()?;
    let text =
        String::from_utf8_lossy(if out.stdout.is_empty() { &out.stderr } else { &out.stdout }).to_string();
    Some(text.lines().next().unwrap_or("").trim().to_string())
}

/// RawTherapee profile: chosen demosaicer, everything else neutral, camera
/// channels taken as-is (no input matrix), sRGB output transfer curve
/// (undone when the PNG is read).
fn rt_profile(method: &str) -> String {
    format!(
        "[Version]\nAppVersion=5.11\nVersion=351\n\n\
[Exposure]\nAuto=false\nClip=0\nCompensation=0\nBrightness=0\nContrast=0\nSaturation=0\nBlack=0\n\
HighlightCompr=0\nHighlightComprThreshold=0\nShadowCompr=0\nHistogramMatching=false\n\
CurveFromHistogramMatching=false\nClampOOG=true\nCurveMode=Standard\nCurveMode2=Standard\nCurve=0\nCurve2=0\n\n\
[HLRecovery]\nEnabled=false\nMethod=Blend\n\n\
[White Balance]\nEnabled=true\nSetting=Camera\nTemperature=6504\nGreen=1\nEqual=1\n\n\
[Color Management]\nInputProfile=(none)\nToneCurve=false\nApplyLookTable=false\nApplyBaselineExposureOffset=false\n\
ApplyHueSatMap=false\nDCPIlluminant=0\nWorkingProfile=sRGB\nOutputProfile=RTv4_sRGB\nOutputProfileIntent=Relative\nOutputBPC=true\n\n\
[Sharpening]\nEnabled=false\n\n[SharpenEdge]\nEnabled=false\n\n[SharpenMicro]\nEnabled=false\n\n\
[Impulse Denoising]\nEnabled=false\n\n[Defringing]\nEnabled=false\n\n[Directional Pyramid Denoising]\nEnabled=false\n\n\
[RAW]\nCA=false\nHotPixelFilter=false\nDeadPixelFilter=false\nPreExposure=1\n\n\
[RAW Bayer]\nMethod={method}\nBorder=0\nCcSteps=0\nPreBlack0=0\nPreBlack1=0\nPreBlack2=0\nPreBlack3=0\n\
PreTwoGreen=true\nLineDenoise=0\nGreenEqThreshold=0\nDCBIterations=2\nDCBEnhance=true\nLMMSEIterations=2\n\
DualDemosaicAutoContrast=false\nDualDemosaicContrast=0\nPDAFLinesFilter=false\n\n\
[RAW X-Trans]\nMethod=3-pass (best)\n"
    )
}

/// Run one external converter on `dng`; returns the demosaiced linear RGB.
fn run_external(tool: Tool, dng: &Path, out_dir: &Path) -> Result<Option<mimizan_core::synth::Rgb>> {
    let stem = dng.file_stem().unwrap().to_string_lossy().to_string();
    match tool {
        Tool::Bilinear => Ok(None),
        Tool::Libraw(q) => {
            let suffix = format!(".q{q}.ppm");
            let out = out_dir.join(format!("{stem}.dng{suffix}"));
            let status = Command::new("dcraw_emu")
                .args(["-4", "-o", "0", "-r", "1", "1", "1", "1", "-c", "0", "-H", "0", "-q"])
                .arg(q.to_string())
                .arg("-Z")
                .arg(&suffix)
                .arg(dng)
                .output()
                .with_context(|| "dcraw_emu")?;
            if !status.status.success() {
                bail!("dcraw_emu failed: {}", String::from_utf8_lossy(&status.stderr));
            }
            Ok(Some(kodak::read_ppm16(&out)?))
        }
        Tool::RawTherapee(m) => {
            let pp3 = out_dir.join(format!("rt_{m}.pp3"));
            std::fs::write(&pp3, rt_profile(m))?;
            let out = out_dir.join(format!("{stem}.{m}.png"));
            let status = Command::new("rawtherapee-cli")
                .arg("-o")
                .arg(&out)
                .args(["-n", "-b16", "-Y", "-q", "-p"])
                .arg(&pp3)
                .arg("-c")
                .arg(dng)
                .output()
                .with_context(|| "rawtherapee-cli")?;
            if !status.status.success() || !out.exists() {
                bail!(
                    "rawtherapee-cli failed: {}{}",
                    String::from_utf8_lossy(&status.stdout),
                    String::from_utf8_lossy(&status.stderr)
                );
            }
            Ok(Some(kodak::read_rgb_png(&out, true)?))
        }
    }
}

fn has_tool(tool: Tool, tools: &[(String, Option<String>)]) -> bool {
    match tool {
        Tool::Bilinear => true,
        Tool::Libraw(_) => tools[0].1.is_some(),
        Tool::RawTherapee(_) => tools[1].1.is_some(),
    }
}

pub fn kodak(a: &KodakArgs) -> Result<()> {
    let phase = BayerPhase::from_name(&a.phase)?;
    std::fs::create_dir_all(&a.out)?;
    std::fs::create_dir_all(&a.strips)?;
    if let Some(dir) = a.report.parent() {
        std::fs::create_dir_all(dir)?;
    }

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
        files.retain(|p| keep.contains(&p.file_stem().unwrap().to_string_lossy().as_ref()));
    }
    if files.is_empty() {
        bail!("no pictures in {}", a.images.display());
    }

    let native = Weights::default();
    let fixed = SeparateParams { mask: MaskMode::Off, ..SeparateParams::default() };
    let block = SeparateParams { mask: MaskMode::Adaptive, ..SeparateParams::default() };
    let default = SeparateParams::default();
    let reconstruct = SeparateParams { reconstruct: Some(Default::default()), ..SeparateParams::default() };
    let mut images = Vec::new();

    for file in &files {
        let stem = file.file_stem().unwrap().to_string_lossy().to_string();
        let (mut rgb, noise) = if a.raw.is_empty() {
            (kodak::load_srgb8(file)?, kodak::quantisation_noise())
        } else {
            kodak::binned_raw(file)?
        };
        if a.scale > 1 {
            let up = |p: &Plane| mimizan_core::resize::resize(p, p.width * a.scale, p.height * a.scale);
            rgb = mimizan_core::synth::Rgb { r: up(&rgb.r), g: up(&rgb.g), b: up(&rgb.b) };
        }
        if a.blur > 0.0 {
            let g = gaussian_kernel(a.blur);
            let bl = |p: &Plane| mimizan_core::filter::lowpass(p, &g);
            rgb = mimizan_core::synth::Rgb { r: bl(&rgb.r), g: bl(&rgb.g), b: bl(&rgb.b) };
        }
        let (w, h) = (rgb.width(), rgb.height());
        let truth = rgb.luminance();
        let fb = kodak::mosaic_rgb(&rgb, phase, noise, &stem);
        let dng = a.out.join(format!("{stem}.dng"));
        kodak::write_dng(&dng, w, h, &fb.samples, phase, &stem)?;

        let mim_fixed = mix(&separate(&fb.mosaic, &fixed), &native);
        let mim_block = mix(&separate(&fb.mosaic, &block), &native);
        let mim = mix(&separate(&fb.mosaic, &default), &native);
        let sep_rec = separate(&fb.mosaic, &reconstruct);
        let computed = sep_rec.computed;
        let mim_rec = mix(&sep_rec, &native);
        drop(sep_rec);
        let q_fixed = kodak::quality(&mim_fixed, &truth, a.border);
        let q_block = kodak::quality(&mim_block, &truth, a.border);
        let q_mim = kodak::quality(&mim, &truth, a.border);
        let q_rec = kodak::quality(&mim_rec, &truth, a.border);

        let mut demosaic: Vec<Option<Quality>> = Vec::new();
        let mut best: Option<(usize, f64, Plane)> = None;
        for (i, m) in METHODS.iter().enumerate() {
            if !has_tool(m.tool, &tools) {
                demosaic.push(None);
                continue;
            }
            let mono = match m.tool {
                Tool::Bilinear => bilinear_luminance(&fb.mosaic.plane, phase),
                t => {
                    let rgb = run_external(t, &dng, &a.out)?.expect("external tool returns RGB");
                    if (rgb.width(), rgb.height()) != (w, h) {
                        bail!(
                            "{}: {} returned {}x{}, expected {}x{}",
                            stem,
                            m.name,
                            rgb.width(),
                            rgb.height(),
                            w,
                            h
                        );
                    }
                    kodak::luminance(&rgb)
                }
            };
            let q = kodak::quality(&mono, &truth, a.border);
            demosaic.push(Some(q));
            if m.tool != Tool::Bilinear && best.as_ref().is_none_or(|b| q.psnr_db > b.1) {
                best = Some((i, q.psnr_db, mono));
            }
        }
        // Without any external converter the bilinear baseline stands in.
        if best.is_none() {
            if let Some(q) = demosaic[0] {
                best = Some((0, q.psnr_db, bilinear_luminance(&fb.mosaic.plane, phase)));
            }
        }
        let (best_idx, best_plane) = match &best {
            Some((i, _, p)) => (Some(*i), Some(p)),
            None => (None, None),
        };

        let mut ests: Vec<&Plane> = vec![&mim];
        if let Some(p) = best_plane {
            ests.push(p);
        }
        let (cx, cy) = kodak::hardest_window(&ests, &truth, a.crop, a.border);
        let strip_name = format!("{stem}.png");
        let mut panels: Vec<&Plane> = vec![&fb.mosaic.plane];
        if let Some(p) = best_plane {
            panels.push(p);
        }
        panels.push(&mim);
        panels.push(&truth);
        kodak::write_strip(&a.strips.join(&strip_name), &panels, cx, cy, a.crop, 2)?;
        kodak::write_gray_png(&a.out.join(format!("{stem}_mimizan.png")), &mim)?;
        kodak::write_gray_png(&a.out.join(format!("{stem}_truth.png")), &truth)?;
        if let Some(p) = best_plane {
            kodak::write_gray_png(&a.out.join(format!("{stem}_best_demosaic.png")), p)?;
        }

        let line: Vec<String> = METHODS
            .iter()
            .zip(&demosaic)
            .map(|(m, q)| match q {
                Some(q) => format!("{} {:.2}", m.name, q.psnr_db),
                None => format!("{} -", m.name),
            })
            .collect();
        eprintln!(
            "{stem}: {} | Mimizan {:.2} (fixed {:.2}, block mask {:.2}) | + reconstruct {:.2} ({:.1} % computed)",
            line.join(" | "),
            q_mim.psnr_db,
            q_fixed.psnr_db,
            q_block.psnr_db,
            q_rec.psnr_db,
            100.0 * computed
        );

        images.push(ImageResult {
            stem,
            width: w,
            height: h,
            demosaic,
            mimizan_fixed: q_fixed,
            mimizan_block: q_block,
            mimizan: q_mim,
            mimizan_reconstruct: q_rec,
            computed,
            best: best_idx,
            crop: (cx, cy),
            strip: strip_name,
        });
    }

    let report = Report {
        phase: phase.name().to_string(),
        border: a.border,
        crop: a.crop,
        scale: a.scale,
        blur: a.blur,
        from_raw: !a.raw.is_empty(),
        tools,
        images,
    };
    std::fs::write(a.out.join("kodak.json"), serde_json::to_string_pretty(&report)?)?;
    let strips_rel = pathdiff(&a.strips, a.report.parent().unwrap_or(Path::new(".")));
    std::fs::write(&a.report, markdown(&report, &strips_rel))?;
    println!("wrote {} and {} strips in {}", a.report.display(), report.images.len(), a.strips.display());
    Ok(())
}

/// `target` relative to `base` for simple sibling/child layouts (both relative
/// to the working directory). Falls back to `target` as given.
fn pathdiff(target: &Path, base: &Path) -> PathBuf {
    if let Ok(rel) = target.strip_prefix(base) {
        return rel.to_path_buf();
    }
    let base_parts: Vec<_> = base.components().collect();
    let target_parts: Vec<_> = target.components().collect();
    let common = base_parts.iter().zip(&target_parts).take_while(|(a, b)| a == b).count();
    let mut out = PathBuf::new();
    for _ in common..base_parts.len() {
        out.push("..");
    }
    for c in &target_parts[common..] {
        out.push(c);
    }
    out
}

fn mean(vals: impl Iterator<Item = f64>) -> Option<f64> {
    let v: Vec<f64> = vals.collect();
    if v.is_empty() {
        None
    } else {
        Some(v.iter().sum::<f64>() / v.len() as f64)
    }
}

fn fmt(v: Option<f64>, prec: usize) -> String {
    v.map(|x| format!("{x:.prec$}")).unwrap_or_else(|| "–".into())
}

fn gaussian_kernel(sigma: f64) -> Vec<f64> {
    let r = (3.0 * sigma).ceil().max(1.0) as usize;
    let k: Vec<f64> = (0..=2 * r)
        .map(|i| {
            let d = i as f64 - r as f64;
            (-d * d / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let s: f64 = k.iter().sum();
    k.into_iter().map(|v| v / s).collect()
}

fn markdown(r: &Report, strips_rel: &Path) -> String {
    let mut s = String::new();
    if r.from_raw {
        s.push_str("# Fake-Bayer benchmark on real RAW files\n\n");
        s.push_str(
            "Generated by `mimizan bench kodak --raw`. Each RAW is decoded and channel-balanced as the pipeline does \
             it, then every 2×2 CFA cell is binned to one RGB pixel (R and B measured, G the mean of the two \
             measured greens): a half-size picture in which every channel of every pixel was measured, from the \
             real lens, the real sensor and its real noise. That picture is the ground truth and is sampled through an ",
        );
        s.push_str(&format!("{} Bayer pattern again, quantised to 16 bit. ", r.phase));
        s.push_str(
            "Relative to the new pixel grid the content is about twice as sharp as the camera saw it, so this \
             sits between the pixel-sharp and the band-limited Kodak conditions, with real noise added. ",
        );
    } else {
        s.push_str("# Kodak fake-Bayer benchmark\n\n");
        s.push_str(
            "Generated by `mimizan bench kodak`. The 24 Kodak PhotoCD pictures (768×512, 8-bit sRGB, film scans \
             without CFA artefacts) are linearised, sampled through an ",
        );
        s.push_str(&format!("{} Bayer pattern and quantised to 16 bit. ", r.phase));
    }
    if r.from_raw && r.scale > 1 {
        s.push_str(&format!(
            "**Condition: binned, then {0}× magnified (Lanczos-3) before mosaicking**, so the ground truth has the \
             camera's own pixel count and its content is band-limited to 1/{0} of Nyquist: real lens, real noise, \
             and the sharpness the camera actually delivers, not twice it. ",
            r.scale
        ));
    } else if r.from_raw {
        // condition described above
    } else if r.scale > 1 {
        s.push_str(&format!(
            "**Condition: {0}× magnified (Lanczos-3) before mosaicking**, so the content is band-limited to \
             1/{0} of Nyquist, the situation a lens and sensor produce (a real sensor never sees pixel-sharp \
             colour edges). ",
            r.scale
        ));
    } else if r.blur > 0.0 {
        let mtf50 = (2.0f64.ln() / (2.0 * std::f64::consts::PI.powi(2) * r.blur * r.blur)).sqrt();
        s.push_str(&format!(
            "**Condition: lens-like**, the pictures blurred with a Gaussian of σ = {:.2} px before mosaicking \
             (MTF50 at {:.2} c/px, MTF at Nyquist {:.0} %), the shape of a lens and pixel-aperture MTF: content \
             reaches Nyquist, attenuated, instead of stopping at half of it. ",
            r.blur,
            mtf50,
            100.0 * (-2.0 * std::f64::consts::PI.powi(2) * r.blur * r.blur * 0.25).exp()
        ));
    } else {
        s.push_str(
            "**Condition: pixel-sharp**, the pictures as they are, the convention of the demosaicing literature \
             (colour edges one pixel wide, which no lens delivers). ",
        );
    }
    s.push_str(
        "The identical mosaic goes to Mimizan and, as a DNG, to the external demosaicers. \
         Their RGB output is reduced with the same weights as the ground truth, `(R + 2G + B)/4` \
         (Mimizan's `Native` mix), so every number measures reconstruction of the luminance only: \
         no tone curve, no sharpening, no noise reduction anywhere.\n\n",
    );
    s.push_str(&format!(
        "Metrics on sRGB-encoded 8-bit-scale values, {} px border excluded: PSNR in dB and SSIM \
         (Gaussian 11×11, σ = 1.5). Higher is better. {}\n\n",
        r.border,
        if r.from_raw {
            "The mosaic reaches Mimizan with the camera's noise model, so the block mask works as in production."
        } else {
            "The mosaic reaches Mimizan with a noise model of the 8-bit quantisation only \
             (`σ ≈ 2.2·10⁻³·√s + 10⁻⁴`), so the block mask treats film grain as signal."
        }
    ));
    s.push_str("Converters:\n\n");
    for (name, v) in &r.tools {
        match v {
            Some(v) => s.push_str(&format!("- `{name}`: {v}\n")),
            None => s.push_str(&format!(
                "- `{name}`: not installed when this report was generated, columns empty\n"
            )),
        }
    }
    s.push_str("\nlibraw: `dcraw_emu -4 -o 0 -r 1 1 1 1 -c 0 -H 0 -q N` (linear 16-bit, camera channels, unit balance). \
                RawTherapee: `rawtherapee-cli -n -b16 -p <neutral profile with the demosaicer>`, sRGB transfer curve inverted on read. \
                Both were verified to return the source channels unchanged on a smooth picture (see `docs/RESULTS.md`).\n\n");

    let n = r.images.len();
    let header_methods: Vec<&str> = METHODS.iter().map(|m| m.name).collect();

    // PSNR table
    s.push_str("## PSNR (dB, luminance)\n\n");
    s.push_str(&format!(
        "| Picture | {} | Mimizan fixed | Mimizan block mask | Mimizan | Mimizan + reconstruct | computed |\n",
        header_methods.join(" | ")
    ));
    s.push_str(&format!("|---|{}---|---|---|---|---|\n", "---|".repeat(METHODS.len())));
    for im in &r.images {
        let cols: Vec<String> = im.demosaic.iter().map(|q| fmt(q.map(|q| q.psnr_db), 2)).collect();
        let best_ext = im.best.and_then(|i| im.demosaic[i].map(|q| q.psnr_db));
        let bold = |v: f64| {
            if best_ext.is_some_and(|b| v >= b) {
                format!("**{v:.2}**")
            } else {
                format!("{v:.2}")
            }
        };
        s.push_str(&format!(
            "| {} | {} | {:.2} | {:.2} | {} | {} | {:.1} % |\n",
            im.stem,
            cols.join(" | "),
            im.mimizan_fixed.psnr_db,
            im.mimizan_block.psnr_db,
            bold(im.mimizan.psnr_db),
            bold(im.mimizan_reconstruct.psnr_db),
            100.0 * im.computed
        ));
    }
    let means: Vec<String> = (0..METHODS.len())
        .map(|i| fmt(mean(r.images.iter().filter_map(|im| im.demosaic[i].map(|q| q.psnr_db))), 2))
        .collect();
    s.push_str(&format!(
        "| **mean ({n})** | {} | {:.2} | {:.2} | **{:.2}** | **{:.2}** | {:.1} % |\n\n",
        means.iter().map(|m| format!("**{m}**")).collect::<Vec<_>>().join(" | "),
        mean(r.images.iter().map(|im| im.mimizan_fixed.psnr_db)).unwrap_or(f64::NAN),
        mean(r.images.iter().map(|im| im.mimizan_block.psnr_db)).unwrap_or(f64::NAN),
        mean(r.images.iter().map(|im| im.mimizan.psnr_db)).unwrap_or(f64::NAN),
        mean(r.images.iter().map(|im| im.mimizan_reconstruct.psnr_db)).unwrap_or(f64::NAN),
        100.0 * mean(r.images.iter().map(|im| im.computed)).unwrap_or(f64::NAN)
    ));
    s.push_str(
        "Bold in the two Mimizan columns: at least as good as the best external demosaicer on that picture. \
         \"computed\" is the share of the picture the reconstruction step took over (mean β).\n\n",
    );

    // SSIM table
    s.push_str("## SSIM (luminance)\n\n");
    s.push_str(&format!(
        "| Picture | {} | Mimizan fixed | Mimizan block mask | Mimizan | Mimizan + reconstruct |\n",
        header_methods.join(" | ")
    ));
    s.push_str(&format!("|---|{}---|---|---|---|\n", "---|".repeat(METHODS.len())));
    for im in &r.images {
        let cols: Vec<String> = im.demosaic.iter().map(|q| fmt(q.map(|q| q.ssim), 4)).collect();
        s.push_str(&format!(
            "| {} | {} | {:.4} | {:.4} | {:.4} | {:.4} |\n",
            im.stem,
            cols.join(" | "),
            im.mimizan_fixed.ssim,
            im.mimizan_block.ssim,
            im.mimizan.ssim,
            im.mimizan_reconstruct.ssim
        ));
    }
    let means: Vec<String> = (0..METHODS.len())
        .map(|i| fmt(mean(r.images.iter().filter_map(|im| im.demosaic[i].map(|q| q.ssim))), 4))
        .collect();
    s.push_str(&format!(
        "| **mean ({n})** | {} | {:.4} | {:.4} | **{:.4}** | **{:.4}** |\n\n",
        means.iter().map(|m| format!("**{m}**")).collect::<Vec<_>>().join(" | "),
        mean(r.images.iter().map(|im| im.mimizan_fixed.ssim)).unwrap_or(f64::NAN),
        mean(r.images.iter().map(|im| im.mimizan_block.ssim)).unwrap_or(f64::NAN),
        mean(r.images.iter().map(|im| im.mimizan.ssim)).unwrap_or(f64::NAN),
        mean(r.images.iter().map(|im| im.mimizan_reconstruct.ssim)).unwrap_or(f64::NAN)
    ));

    // Wins
    let mut wins = 0;
    let mut wins_rec = 0;
    let mut compared = 0;
    for im in &r.images {
        if let Some(b) = im.best.and_then(|i| im.demosaic[i]) {
            compared += 1;
            if im.mimizan.psnr_db >= b.psnr_db {
                wins += 1;
            }
            if im.mimizan_reconstruct.psnr_db >= b.psnr_db {
                wins_rec += 1;
            }
        }
    }
    if compared > 0 {
        s.push_str(&format!(
            "Mimizan is at or above the best external demosaicer on {wins} of {compared} pictures (PSNR); \
             with the reconstruction step on {wins_rec} of {compared}. \
             \"Mimizan\" is the default pipeline (`--mask dubois`: pixel-adaptive weighting of the two C2 copies \
             and of two direction-dependent C1 bands, elongated pass bands). \"Mimizan fixed\" is the same \
             separation with square 0.15 c/px bands and the two C2 copies averaged (`--mask off`); \
             \"Mimizan block mask\" is the fixed separation with the 64 px block-spectrum mask (`--mask adaptive`). \
             The differences between the three columns are what the adaptive steps contribute. \
             \"Mimizan + reconstruct\" is the default pipeline with the optional nonlinear step (`--reconstruct`, \
             off by default): where the linear luminance still carries energy near the carriers, a per-pixel \
             direction decision on the colour differences replaces the filter output. Those pixels are computed, \
             not measured; the \"computed\" column says how many.\n\n"
        ));
    }

    // Gallery
    s.push_str("## Crops\n\n");
    s.push_str(&format!(
        "Each strip: **mosaic** as the sensor records it (grey, CFA pattern visible) · **best external demosaicer → mono** · \
         **Mimizan** · **ground truth**. {0}×{0} px window at 2× (nearest neighbour), chosen automatically as the region \
         with the largest combined error of the two estimates, i.e. the hardest spot for both, not a hand-picked win.\n\n",
        r.crop
    ));
    for im in &r.images {
        let best_name = im.best.map(|i| METHODS[i].name).unwrap_or("–");
        let best_psnr = fmt(im.best.and_then(|i| im.demosaic[i].map(|q| q.psnr_db)), 2);
        s.push_str(&format!(
            "### {}\n\nMosaic · {} ({} dB) · Mimizan ({:.2} dB) · ground truth — window at ({}, {})\n\n![{}]({})\n\n",
            im.stem,
            best_name,
            best_psnr,
            im.mimizan.psnr_db,
            im.crop.0,
            im.crop.1,
            im.stem,
            strips_rel.join(&im.strip).display()
        ));
    }
    s.push_str("Kodak pictures: released by Eastman Kodak for unrestricted use; copies at <https://r0k.us/graphics/kodak/> \
                and <https://github.com/lemire/kodakimagecollection>.\n");
    s
}
