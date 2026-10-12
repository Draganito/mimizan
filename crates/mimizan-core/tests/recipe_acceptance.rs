//! Acceptance of the print stage (Zonensystem paper, table 4): bitwise, not
//! visual. A negative is written to disk once; every case renders through
//! `print::render` from that file, as the CLI does, and compares 16-bit
//! values.

use mimizan_core::burn::Burn;
use mimizan_core::calib::LookFile;
use mimizan_core::icc::GrayTrc;
use mimizan_core::plane::Plane;
use mimizan_core::print::{render, PrintParams, PrintSize};
use mimizan_core::recipe::{Recipe, Sharpen};
use mimizan_core::resize::ResampleKernel;
use mimizan_core::tiffout::{write_gray16, write_gray8, TiffMeta};
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

/// Rows 1 and 7 of table 4: the untouched bar is `neutral`, place = 0 moves
/// nothing, and grade 0 is the untouched bar.
#[test]
fn untouched_bar_exports_as_neutral() {
    let neg = negative_on_disk("neutral");
    let neutral = run(&neg, Recipe::default());
    assert_eq!(run(&neg, Recipe::with_look(zone::default_look())), neutral);
    assert_eq!(run(&neg, Recipe::with_look(zone::grade(0))), neutral);
    assert_eq!(run(&neg, Recipe { place: 0.0, ..Recipe::default() }), neutral);
    // With a size and a kernel too: the untouched curve stays neutral.
    let sized = |curve: LookFile| Recipe {
        curve,
        size: Some(PrintSize::LongEdgePx(50)),
        resample: ResampleKernel::CatmullRom,
        ..Recipe::default()
    };
    assert_eq!(run(&neg, sized(zone::default_look())), run(&neg, sized(LookFile::neutral())));
}

/// Row 2: a node dragged and dragged back is a run that was never dragged.
/// Row 3: the last of a long history equals a fresh run with that set.
/// Row 4: exporting twice is bit-identical.
#[test]
fn history_does_not_exist_for_the_export() {
    let neg = negative_on_disk("history");
    let mut look = zone::default_look();
    let v4 = look.points[4][1];
    zone::set_output(&mut look, 4, 0.6);
    zone::set_output(&mut look, 4, v4);
    assert_eq!(look.points, zone::default_look().points);
    assert_eq!(run(&neg, Recipe::with_look(look)), run(&neg, Recipe::default()));

    // Five hours: a list of recipes. The export knows only the last.
    let mut history = vec![Recipe::default()];
    for i in 1..40 {
        let mut r = history.last().unwrap().clone();
        if r.curve.name != zone::LOOK_NAME {
            r.curve = zone::default_look();
        }
        let n = r.curve.points.len();
        let idx = 1 + i % (n - 2);
        let cur = r.curve.points[idx][1];
        zone::set_output(&mut r.curve, idx, cur + if i % 3 == 0 { -0.03 } else { 0.02 });
        r.place = (i as f64 * 0.07).sin() * 0.5;
        if i % 5 == 0 {
            r.dodge_burn.push(Burn::Radial {
                x: 0.1 + 0.02 * i as f64,
                y: 0.5,
                radius: 0.2,
                feather: 0.5,
                ev: if i % 10 == 0 { 0.5 } else { -0.5 },
            });
        }
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

/// Row 5, as the interpolation allows it: a dragged node changes the slope
/// at its two direct neighbours (Fritsch–Carlson is C¹, the slope at a node
/// is a mean of the two adjacent secants), so two intervals on each side
/// move; beyond the second neighbour the curve is bit-identical. The
/// neighbours' values themselves do not move. **[Abweichung]** from the
/// paper's wording "beyond the next node", documented in the SPEC.
#[test]
fn a_drag_stays_inside_two_intervals_per_side() {
    use mimizan_core::curve::Curve;
    let before = zone::default_look();
    let mut after = before.clone();
    let i = 4;
    let target = after.points[i][1] + 0.05;
    zone::set_output(&mut after, i, target);
    for (k, (a, b)) in after.points.iter().zip(&before.points).enumerate() {
        if k != i {
            assert_eq!(a, b, "neighbour {k} moved");
        }
    }
    let (cb, ca) = (Curve::from_look(&before), Curve::from_look(&after));
    let (lo, hi) = (before.points[i - 2][0], before.points[i + 2][0]);
    let mut changed_inside = false;
    for k in 0..=2000 {
        let u = k as f64 / 2000.0;
        let lin = zone::decode(u);
        let d = (ca.eval(lin) - cb.eval(lin)).abs();
        if u <= lo || u >= hi {
            assert!(d < 1e-12, "changed outside at u = {u}: {d}");
        } else if u > before.points[i - 1][0] && u < before.points[i + 1][0] {
            changed_inside |= d > 1e-6;
        }
    }
    assert!(changed_inside);
    assert!(ca.eval(zone::decode(before.points[i][0])) > cb.eval(zone::decode(before.points[i][0])));
}

/// Row 6: preset N+1 then clamping: strictly increasing, gap at least 1e-4.
/// Row 8: place = 0 with only the shoulder pulled keeps Zone V's boundaries
/// exactly and its centre within a few percent of the pull.
#[test]
fn presets_clamp_and_the_shoulder_leaves_zone_v() {
    use mimizan_core::curve::Curve;
    for n in -5..=5 {
        let g = zone::grade(n);
        for w in g.points.windows(2) {
            assert!(w[1][1] - w[0][1] >= zone::MIN_GAP - 1e-12, "N{n:+}");
        }
    }
    let mut look = zone::default_look();
    let n = look.points.len();
    let pull = 0.95 - look.points[n - 2][1];
    zone::set_output(&mut look, n - 2, 0.95); // boundary VI|VII up: Zone VII narrower, VI wider
    zone::set_output(&mut look, n - 1, 0.97); // paper white down
    let c = Curve::from_look(&look);
    let neutral = Curve::from_look(&LookFile::neutral());
    // The two boundaries of Zone V are nodes and did not move: exact.
    for i in [n - 4, n - 3] {
        let lin = zone::decode(look.points[i][0]);
        assert!((c.eval(lin) - neutral.eval(lin)).abs() < 1e-12, "boundary {i} moved");
    }
    // The grey card itself lies in the interval next to the changed slope
    // (second interval from the pulled node, SPEC [Abweichung]): the C¹
    // curve bends towards the pulled node, by a few percent of the pull.
    let d = (c.eval(zone::GRAY_CARD) - neutral.eval(zone::GRAY_CARD)).abs();
    assert!(d < 0.03 * pull, "Zone V moved by {d} for a pull of {pull}");
}

/// Shapes and placing have no reduction over threads: 16 and 3 threads give
/// the same 16-bit values, with a resize and a border in the pass.
#[test]
fn the_pass_is_deterministic_across_thread_counts() {
    let neg = negative_on_disk("threads");
    let recipe = Recipe {
        place: 0.3,
        curve: zone::grade(1),
        dodge_burn: vec![
            Burn::Radial { x: 0.4, y: 0.6, radius: 0.3, feather: 0.6, ev: 0.8 },
            Burn::Gradient { x0: 0.0, y0: 0.0, x1: 0.0, y1: 0.5, ev: -0.7 },
        ],
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
        curve: zone::grade(-1),
        dodge_burn: vec![Burn::Radial { x: 0.452, y: 0.61, radius: 0.15, feather: 0.5, ev: 0.75 }],
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
    assert_eq!(v["look"], "zones");
    assert_eq!(v["recipe"]["place"], -0.25);
    assert_eq!(v["recipe"]["resample"], "b-spline");
    assert_eq!(v["recipe"]["dodge_burn"][0]["type"], "radial");
    assert_eq!(v["recipe"]["dodge_burn"][0]["ev"], 0.75);
    assert_eq!(v["recipe"]["sharpen"]["mode"], "off");
    assert_eq!(v["recipe"]["curve"]["points"].as_array().unwrap().len(), zone::default_nodes().len() + 2);
    // The negative's own description is still inside.
    assert_eq!(v["source_description"]["stage"], "negative");
    assert!(v["usm_amount"].is_number() && v["width"].is_number());
    // The recipe inside the file is the recipe that ran.
    let back: Recipe = serde_json::from_value(v["recipe"].clone()).unwrap();
    assert_eq!(back, p.recipe);
}
