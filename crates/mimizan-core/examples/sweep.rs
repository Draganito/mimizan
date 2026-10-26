//! Sweep of separation variants on the Kodak conditions (PAPER §8.5).
//! Mosaics are built once per condition, every variant is one separation,
//! the figure is the mean luminance PSNR of `mimizan bench kodak` without
//! the external converters. Needs `testdata/kodak` and, for `raw`/`soft`,
//! the four Z f NEFs of `testdata/README.md`.
//!
//!     cargo run --release -p mimizan-core --example sweep -- lens x2 sharp raw soft
//!       --per-image     one figure per picture (--lin: linear RMSE in dB)
//!       --lum-only      neutral copies of the pictures (every error is leak)
//!       --chroma-only   flat luminance with the pictures' chrominance
//!       --oracle        upper bound of a square chroma band at several cutoffs
//!       --dump kodim19 [--at x,y]   strips and error maps to /tmp/sweep

use mimizan_core::cfa::BayerPhase;
use mimizan_core::dubois::DuboisParams;
use mimizan_core::ingest::Mosaic;
use mimizan_core::kodak;
use mimizan_core::mix::{mix, Weights};
use mimizan_core::plane::Plane;
use mimizan_core::separate::{separate, SeparateParams};
use mimizan_core::synth::Rgb;
use std::path::{Path, PathBuf};

const BORDER: usize = 16;
const RAWS: &[&str] = &[
    "testdata/NikonZF.nef",
    "testdata/NikonZF_2.nef",
    "testdata/nikon_zf_01.nef",
    "testdata/nikon_zf_27.nef",
];

struct Case {
    stem: String,
    mosaic: Mosaic,
    truth: Plane,
    /// True chrominance planes `C1 = (2G − R − B)/4`, `C2 = (R − B)/4`.
    c1: Plane,
    c2: Plane,
}

/// Upper bound of any linear estimator with a square chroma band at `fc`:
/// the true chrominance, low-passed, with no luminance leak at all.
fn oracle(cases: &[Case], fc: f64) -> f64 {
    let phase = BayerPhase::RGGB;
    let k = mimizan_core::filter::LowpassSpec { cutoff: fc, transition: 0.15, attenuation_db: 40.0 }.kernel();
    let mut sum = 0.0;
    for c in cases {
        let (c1, c2) = if fc >= 0.5 {
            (c.c1.clone(), c.c2.clone())
        } else {
            (mimizan_core::filter::lowpass(&c.c1, &k), mimizan_core::filter::lowpass(&c.c2, &k))
        };
        let l = mimizan_core::separate::luminance(&c.mosaic.plane, phase, &c1, &c2);
        sum += kodak::quality(&l, &c.truth, BORDER).psnr_db;
    }
    sum / cases.len() as f64
}

fn kodak_files() -> Vec<PathBuf> {
    let mut v: Vec<PathBuf> = std::fs::read_dir("testdata/kodak")
        .expect("testdata/kodak")
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|e| e == "png"))
        .collect();
    v.sort();
    v
}

fn build(cond: &str) -> Vec<Case> {
    let phase = BayerPhase::RGGB;
    let (files, scale, blur, raw): (Vec<PathBuf>, usize, f64, bool) = match cond {
        "sharp" => (kodak_files(), 1, 0.0, false),
        "lens" => (kodak_files(), 1, 0.75, false),
        "x2" => (kodak_files(), 2, 0.0, false),
        "raw" => (RAWS.iter().map(PathBuf::from).collect(), 1, 0.0, true),
        "soft" => (RAWS.iter().map(PathBuf::from).collect(), 2, 0.0, true),
        other => panic!("unknown condition {other}"),
    };
    files
        .iter()
        .map(|f| {
            let stem = f.file_stem().unwrap().to_string_lossy().into_owned();
            let (mut rgb, noise) = if raw {
                kodak::binned_raw(f).expect("raw")
            } else {
                (kodak::load_srgb8(f).expect("png"), kodak::quantisation_noise())
            };
            if scale > 1 {
                let up = |p: &Plane| mimizan_core::resize::resize(p, p.width * scale, p.height * scale);
                rgb = Rgb { r: up(&rgb.r), g: up(&rgb.g), b: up(&rgb.b) };
            }
            if blur > 0.0 {
                let bl = |p: &Plane| mimizan_core::usm::gaussian_blur(p, blur);
                rgb = Rgb { r: bl(&rgb.r), g: bl(&rgb.g), b: bl(&rgb.b) };
            }
            let args: Vec<String> = std::env::args().collect();
            if args.iter().any(|a| a == "--lum-only") {
                // Neutral picture: every error of L̂ is luminance leaking
                // into the chroma estimate.
                let l = rgb.luminance();
                rgb = Rgb { r: l.clone(), g: l.clone(), b: l };
            } else if args.iter().any(|a| a == "--chroma-only") {
                // Flat luminance 0.5 with the picture's chrominance: every
                // error is chrominance the estimate did not capture.
                let l = rgb.luminance();
                let sh = |p: &Plane| {
                    Plane::from_vec(
                        p.width,
                        p.height,
                        p.data.iter().zip(&l.data).map(|(v, l)| v - l + 0.5).collect(),
                    )
                };
                rgb = Rgb { r: sh(&rgb.r), g: sh(&rgb.g), b: sh(&rgb.b) };
            }
            let truth = rgb.luminance();
            let (w, h) = (rgb.width(), rgb.height());
            let c1 = Plane::from_vec(
                w,
                h,
                (0..w * h).map(|i| (2.0 * rgb.g.data[i] - rgb.r.data[i] - rgb.b.data[i]) / 4.0).collect(),
            );
            let c2 =
                Plane::from_vec(w, h, (0..w * h).map(|i| (rgb.r.data[i] - rgb.b.data[i]) / 4.0).collect());
            let fb = kodak::mosaic_rgb(&rgb, phase, noise, &stem);
            Case { stem, mosaic: fb.mosaic, truth, c1, c2 }
        })
        .collect()
}

fn d(f: impl Fn(&mut DuboisParams)) -> SeparateParams {
    let mut p = SeparateParams::default();
    f(&mut p.dubois);
    p
}

/// `default` is the shipped estimator; `0.11` the one before the tensor.
/// Add variants here when sweeping.
fn variants() -> Vec<(String, SeparateParams)> {
    vec![
        ("0.11 (no tensor)".into(), d(|p| *p = DuboisParams::without_tensor())),
        ("tensor weight only".into(), d(|p| p.ring_tensor = 0.0)),
        ("default".into(), SeparateParams::default()),
    ]
}

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let per_image = args.iter().any(|a| a == "--per-image");
    let conds: Vec<&String> = args.iter().filter(|a| !a.starts_with("--")).collect();
    let native = Weights::default();
    let mut table: Vec<(String, Vec<f64>)> = Vec::new();
    let mut variants = variants();
    if args.iter().any(|a| a == "--quick") {
        variants.retain(|(n, _)| n == "default");
    }
    for (name, _) in &variants {
        table.push((name.clone(), Vec::new()));
    }
    for cond in &conds {
        let cases = build(cond);
        eprintln!("{cond}: {} pictures", cases.len());
        if args.iter().any(|a| a == "--oracle") {
            for fc in [0.10, 0.15, 0.20, 0.25, 0.30, 0.35, 0.40, 0.5] {
                eprintln!("  oracle square band {fc:4.2}        {:6.2}", oracle(&cases, fc));
            }
        }
        if args.iter().any(|a| a == "--oracle-only") {
            continue;
        }
        if let Some(i) = args.iter().position(|a| a == "--dump") {
            // Error maps (|err| × 8, sRGB-encoded) of the 0.11 separation and
            // the default for one picture, into /tmp/sweep.
            let stem = &args[i + 1];
            let c = cases.iter().find(|c| &c.stem == stem).expect("stem");
            std::fs::create_dir_all("/tmp/sweep").unwrap();
            let mut outs = Vec::new();
            let mut errs = Vec::new();
            let old = d(|p| *p = DuboisParams::without_tensor());
            for (name, p) in [("0.11", old), ("default", SeparateParams::default())] {
                let out = mix(&separate(&c.mosaic, &p).unwrap(), &native);
                outs.push(out.clone());
                errs.push(Plane::from_vec(
                    out.width,
                    out.height,
                    out.data
                        .iter()
                        .zip(&c.truth.data)
                        .map(|(a, b)| (0.5 + (a - b) * 8.0).clamp(0.0, 1.0))
                        .collect(),
                ));
                let err = Plane::from_vec(
                    out.width,
                    out.height,
                    out.data.iter().zip(&c.truth.data).map(|(a, b)| ((a - b).abs() * 8.0).min(1.0)).collect(),
                );
                kodak::write_gray_png(Path::new(&format!("/tmp/sweep/{stem}_{name}_err.png")), &err).unwrap();
                kodak::write_gray_png(Path::new(&format!("/tmp/sweep/{stem}_{name}.png")), &out).unwrap();
            }
            kodak::write_gray_png(Path::new(&format!("/tmp/sweep/{stem}_truth.png")), &c.truth).unwrap();
            // Strip 0.11 | default | truth and the two error maps at the
            // hardest window of 0.11, and at an optional --at x,y.
            let (cx, cy) = kodak::hardest_window(&[&outs[0]], &c.truth, 96, BORDER);
            let panels = [&outs[0], &outs[1], &c.truth, &errs[0], &errs[1]];
            kodak::write_strip(Path::new(&format!("/tmp/sweep/{stem}_strip.png")), &panels, cx, cy, 96, 4)
                .unwrap();
            eprintln!("hardest window at {cx},{cy}");
            if let Some(j) = args.iter().position(|a| a == "--at") {
                let (x, y) = args[j + 1].split_once(',').unwrap();
                let (x, y): (usize, usize) = (x.parse().unwrap(), y.parse().unwrap());
                kodak::write_strip(
                    Path::new(&format!("/tmp/sweep/{stem}_strip_at.png")),
                    &panels,
                    x,
                    y,
                    96,
                    4,
                )
                .unwrap();
            }
            continue;
        }
        for (vi, (name, params)) in variants.iter().enumerate() {
            let mut sum = 0.0;
            let mut sum_lin = 0.0;
            let mut per = Vec::new();
            for c in &cases {
                let sep = separate(&c.mosaic, params).expect("phase");
                let out = mix(&sep, &native);
                let q = kodak::quality(&out, &c.truth, BORDER);
                sum += q.psnr_db;
                sum_lin += -20.0 * q.rmse_lin.log10();
                let lin = args.iter().any(|a| a == "--lin");
                per.push((c.stem.clone(), if lin { -20.0 * q.rmse_lin.log10() } else { q.psnr_db }));
            }
            let mean = sum / cases.len() as f64;
            let mean_lin = sum_lin / cases.len() as f64;
            table[vi].1.push(mean);
            if per_image {
                let line: Vec<String> = per
                    .iter()
                    .map(|(s, v)| format!("{} {:.2}", &s[s.len().saturating_sub(2)..], v))
                    .collect();
                eprintln!("  {name:32} {mean:6.2} (lin {mean_lin:6.2}) | {}", line.join(" "));
            } else {
                eprintln!("  {name:32} {mean:6.2} (lin {mean_lin:6.2})");
            }
        }
    }
    println!("\n{:32} {}", "variant", conds.iter().map(|c| format!("{c:>7}")).collect::<Vec<_>>().join(" "));
    for (name, vals) in &table {
        println!("{:32} {}", name, vals.iter().map(|v| format!("{v:7.2}")).collect::<Vec<_>>().join(" "));
    }
    let _ = Path::new(".");
}
