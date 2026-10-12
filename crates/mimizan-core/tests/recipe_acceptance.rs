//! Acceptance of the print stage (Zonensystem paper, table 4): bitwise, not
//! visual. A negative is written to disk once; every case renders through
//! `print::render` from that file, as the CLI does, and compares 16-bit
//! values.

use mimizan_core::calib::LookFile;
use mimizan_core::icc::GrayTrc;
use mimizan_core::paper::{self, SplitGrade};
use mimizan_core::plane::Plane;
use mimizan_core::print::{render, PrintParams, PrintSize};
use mimizan_core::recipe::{Recipe, Sharpen};
use mimizan_core::resize::ResampleKernel;
use mimizan_core::tiffout::{write_gray16, write_gray8, TiffMeta};
use mimizan_core::tone::Tone;
use mimizan_core::zone;
use std::path::{Path, PathBuf};

fn q16(p: &Plane) -> Vec<u16> {
    p.data.iter().map(|&v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16).collect()
}

/// A small synthetic negative with a mask sidecar, in a fresh temp dir.
fn negative_on_disk(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mimizan-acceptance-{}-{}", std::process::id(), name));
    std::fs::create_dir_all(&dir).unwrap();
    let (w, h) = (96, 64);
    let mut p = Plane::zeros(w, h);
    for y in 0..h {
        for x in 0..w {
            // A ramp with a step and a dark corner: every zone present.
            let base = 0.02 + 0.9 * x as f64 / (w - 1) as f64;
            let step = if y > h / 2 { 0.15 } else { 0.0 };
            p.set(x, y, (base * (1.0 - 0.5 * (y as f64 / h as f64).powi(2)) + step).min(1.0));
        }
    }
    let path = dir.join("neg.tif");
    write_gray16(
        &path,
        &p,
        &TiffMeta {
            description: r#"{"stage":"negative","test":true}"#,
            trc: GrayTrc::Linear,
            orientation: 6,
            dpi: None,
        },
    )
    .unwrap();
    let mask: Vec<u8> = (0..w * h).map(|i| if i % 11 == 0 { 255 } else { 204 }).collect();
    write_gray8(&dir.join("neg.mask.tif"), w, h, &mask, "mask").unwrap();
    path
}

fn run(neg: &Path, recipe: Recipe) -> Vec<u16> {
    let p = PrintParams { recipe, viewing_distance_mm: Some(400.0), ..Default::default() };
    q16(&render(neg, &p).unwrap().plane)
}

fn on_paper(soft: f64, hard: f64) -> Recipe {
    Recipe { curve: Tone::Paper(SplitGrade::new(soft, hard)), ..Recipe::default() }
}

/// Rows 1 and 7 of table 4: place = 0 changes nothing, for the neutral
/// look and for the paper; the paper at its base exposure is a print, not
/// the identity.
#[test]
fn untouched_steps_change_nothing() {
    let neg = negative_on_disk("neutral");
    let neutral = run(&neg, Recipe::default());
    assert_eq!(run(&neg, Recipe { place: 0.0, ..Recipe::default() }), neutral);
    assert_eq!(run(&neg, Recipe::with_look(LookFile::neutral())), neutral);
    let base = run(&neg, Recipe::on_paper());
    assert_eq!(run(&neg, on_paper(0.0, 0.0)), base);
    assert_eq!(run(&neg, Recipe { place: 0.0, ..Recipe::on_paper() }), base);
    assert_ne!(base, neutral);
    // With a size and a kernel too: the same paper, the same picture.
    let sized = |curve: Tone| Recipe {
        curve,
        size: Some(PrintSize::LongEdgePx(50)),
        resample: ResampleKernel::CatmullRom,
        ..Recipe::default()
    };
    assert_eq!(run(&neg, sized(Tone::paper())), run(&neg, sized(Tone::paper())));
}

/// Row 2: an exposure moved and moved back is a run that was never moved.
/// Row 3: the last of a long history equals a fresh run with that set.
/// Row 4: exporting twice is bit-identical; the recipe through JSON too.
#[test]
fn history_does_not_exist_for_the_export() {
    let neg = negative_on_disk("history");
    let mut r = Recipe::on_paper();
    r.curve.as_paper_mut().unwrap().hard += 0.75;
    r.curve.as_paper_mut().unwrap().hard -= 0.75;
    assert_eq!(r, Recipe::on_paper());
    assert_eq!(run(&neg, r), run(&neg, Recipe::on_paper()));

    // Five hours: a list of recipes. The export knows only the last.
    let mut history = vec![Recipe::on_paper()];
    for i in 1..40 {
        let mut r = history.last().unwrap().clone();
        {
            let s = r.curve.as_paper_mut().unwrap();
            if i % 2 == 0 {
                s.soft = (s.soft + if i % 3 == 0 { -0.3 } else { 0.2 })
                    .clamp(-paper::STOP_LIMIT, paper::STOP_LIMIT);
            } else {
                s.hard = (s.hard + if i % 3 == 0 { 0.25 } else { -0.15 })
                    .clamp(-paper::STOP_LIMIT, paper::STOP_LIMIT);
            }
        }
        r.place = (i as f64 * 0.07).sin() * 0.5;
        r.validate().unwrap();
        history.push(r);
    }
    let last = history.last().unwrap().clone();
    let from_history = run(&neg, last.clone());
    let fresh = run(&neg, last.clone());
    assert_eq!(from_history, fresh);
    // And the recipe saved as JSON and read back gives the same picture.
    let dir = neg.parent().unwrap();
    let file = dir.join("recipe.json");
    last.save(&file).unwrap();
    let loaded = Recipe::load(&file).unwrap();
    assert_eq!(loaded, last);
    assert_eq!(run(&neg, loaded), fresh);
}

/// What the paper cannot do: for any pair of exposures within the limits
/// the print is monotone in the scene and the border is unexposed paper;
/// within the working range of the exposures no zone collapses (every
/// zone keeps a share of the paper; at the extremes the outer zones sit on
/// the paper's black or white, as they do on a real sheet). The soft pass
/// is the highlights' exposure, the hard pass the shadows'.
#[test]
fn the_paper_is_monotone_and_keeps_every_zone() {
    let probes = [-3.0, -2.0, -1.0, 0.0, 0.5, 1.5, 2.5, 3.0];
    for &soft in &probes {
        for &hard in &probes {
            let c = SplitGrade::new(soft, hard).curve();
            let mut last = c.eval(0.0);
            assert_eq!(last, 0.0);
            assert_eq!(c.white(), 1.0);
            for i in 1..=2000 {
                let v = c.eval(i as f64 / 2000.0);
                assert!(v >= last, "not monotone at {i} for {soft},{hard}");
                last = v;
            }
            // Zone widths on the print: positive for every zone under the clip
            // while the exposures stay within a stop and a half of the base.
            let working = soft.abs() <= 1.5 && hard.abs() <= 1.5;
            let mut prev = c.eval(0.0);
            for k in zone::boundaries() {
                let v = c.eval(zone::boundary_linear(k));
                assert!(
                    v >= prev && (!working || v > prev),
                    "zone {} collapsed at {soft},{hard}",
                    zone::numeral(k)
                );
                prev = v;
            }
            assert!(c.eval(1.0) >= prev && (!working || c.eval(1.0) > prev));
        }
    }
    let base = SplitGrade::default().curve();
    assert!((base.density(zone::GRAY_CARD) - paper::GREY_DENSITY).abs() < 1e-9);
    assert!((base.density(1.0) - paper::CLIP_DENSITY).abs() < 1e-9);
    let (shadow, light) = (zone::linear(3), zone::linear(7));
    let more_soft = SplitGrade::new(1.0, 0.0).curve();
    let less_hard = SplitGrade::new(0.0, -1.0).curve();
    let d = |c: &paper::PaperCurve, l: f64| (c.eval(l) - base.eval(l)).abs();
    assert!(d(&more_soft, light) > 4.0 * d(&more_soft, shadow));
    assert!(d(&less_hard, shadow) > 10.0 * d(&less_hard, light));
}

/// Row 6, on paper: a stop more of both exposures is the scene placed down
/// by 1/γ_n stops — the bar dragged as a whole is a placing, nothing else.
#[test]
fn both_exposures_together_are_a_placing() {
    let neg = negative_on_disk("placing");
    let shifted = run(&neg, on_paper(0.6, 0.6));
    let placed = run(&neg, Recipe { place: -0.6 / paper::NEGATIVE_GAMMA, ..Recipe::on_paper() });
    let worst = shifted.iter().zip(&placed).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
    assert!(worst <= 1, "{worst}");
}

/// Placing and the paper have no reduction over threads: 16 and 3 threads
/// give the same 16-bit values, with a resize and a border in the pass.
#[test]
fn the_pass_is_deterministic_across_thread_counts() {
    let neg = negative_on_disk("threads");
    let recipe = Recipe {
        place: 0.3,
        curve: Tone::Paper(SplitGrade::new(-0.5, 0.5)),
        resample: ResampleKernel::Mitchell,
        size: Some(PrintSize::Physical { width_in: 2.0, height_in: 3.0 }),
        dpi: 40.0,
        border_mm: 5.0,
        sharpen: Sharpen::Usm { amount: 0.8 },
        ..Recipe::default()
    };
    let a = run(&neg, recipe.clone());
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
    let b = pool.install(|| run(&neg, recipe));
    assert_eq!(a, b);
}

/// The description of the print carries the recipe, and the older fields
/// stay where a foreign reader expects them.
#[test]
fn description_carries_the_recipe_and_the_old_fields() {
    let neg = negative_on_disk("description");
    let recipe = Recipe {
        place: -0.25,
        curve: Tone::Paper(SplitGrade::new(0.5, -0.25)),
        resample: ResampleKernel::BSpline,
        ..Recipe::default()
    };
    let p = PrintParams { recipe, viewing_distance_mm: Some(400.0), ..Default::default() };
    let out = render(&neg, &p).unwrap();
    let out_path = neg.with_file_name("print.tif");
    mimizan_core::print::write(&out, &out_path, &p).unwrap();
    let g = mimizan_core::tiffin::read_gray(&out_path).unwrap();
    let v: serde_json::Value = serde_json::from_str(g.description.as_deref().unwrap()).unwrap();
    assert_eq!(v["stage"], "print");
    assert_eq!(v["look"], "paper multigrade soft +0.50 hard -0.25");
    assert_eq!(v["look_encoding"], "gamma22");
    assert_eq!(v["recipe"]["place"], -0.25);
    assert_eq!(v["recipe"]["resample"], "b-spline");
    assert_eq!(v["recipe"]["sharpen"]["mode"], "off");
    assert_eq!(v["recipe"]["curve"]["paper"], "multigrade");
    assert_eq!(v["recipe"]["curve"]["soft"], 0.5);
    // The negative's own description is still inside.
    assert_eq!(v["source_description"]["stage"], "negative");
    assert!(v["usm_amount"].is_number() && v["width"].is_number());
    // The recipe inside the file is the recipe that ran.
    let back: Recipe = serde_json::from_value(v["recipe"].clone()).unwrap();
    assert_eq!(back, p.recipe);
}
