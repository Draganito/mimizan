//! Property tests over the four CFA phases and random inputs, plus f64
//! reference checks of the fused convolution paths (SPEC sections 4, 5, 11).

use mimizan_core::cfa::{BayerPhase, Color};
use mimizan_core::filter::{convolve_cols_signed, convolve_rows_mod, lowpass_reference, LowpassSpec};
use mimizan_core::ingest::{CameraInfo, IngestReport, Mosaic, NoiseModel};
use mimizan_core::mask::mask_value;
use mimizan_core::plane::Plane;
use mimizan_core::separate::{
    average_in_place, chroma_estimates, luminance, separate, sign_x, SeparateParams,
};
use proptest::prelude::*;

fn phase_strategy() -> impl Strategy<Value = BayerPhase> {
    prop_oneof![
        Just(BayerPhase::RGGB),
        Just(BayerPhase::BGGR),
        Just(BayerPhase::GRBG),
        Just(BayerPhase::GBRG)
    ]
}

fn mosaic_of(phase: BayerPhase, w: usize, h: usize, rgb: impl Fn(usize, usize) -> (f64, f64, f64)) -> Plane {
    let mut m = Plane::zeros(w, h);
    for y in 0..h {
        for x in 0..w {
            let (r, g, b) = rgb(x, y);
            let v = match phase.color_at(x, y) {
                Color::R => r,
                Color::G => g,
                Color::B => b,
            };
            m.set(x, y, v);
        }
    }
    m
}

fn wrap(plane: Plane, phase: BayerPhase) -> Mosaic {
    let (w, h) = (plane.width, plane.height);
    Mosaic {
        plane,
        phase: Some(phase),
        saturated: vec![0; w * h],
        noise: NoiseModel::default(),
        wb: [1.0; 3],
        orientation: 1,
        camera: CameraInfo {
            make: "test".into(),
            model: "test".into(),
            clean_make: "test".into(),
            clean_model: "test".into(),
            bits: 16,
            exposure: Default::default(),
        },
        xyz_to_cam: None,
        report: IngestReport {
            crop: mimizan_core::decode::Rect { x: 0, y: 0, w, h },
            saturated_raw: 0,
            saturated_dilated: 0,
            defects: 0,
            defect_hot_rows: vec![],
            min_norm: 0.0,
            max_norm: 1.0,
            wb: [1.0; 3],
            wb_source: "exact".into(),
        },
    }
}

proptest! {
    #![proptest_config(ProptestConfig { cases: 24, ..ProptestConfig::default() })]

    /// Any flat colour, any phase: the fixed estimator returns exactly
    /// L = (R+2G+B)/4, C1 = (−R+2G−B)/4, C2 = (R−B)/4 away from the border.
    #[test]
    fn flat_field_is_exact_for_every_phase(
        phase in phase_strategy(),
        r in 0.0f64..1.0, g in 0.0f64..1.0, b in 0.0f64..1.0,
    ) {
        let (w, h) = (176, 160);
        let k = LowpassSpec::default().kernel();
        let m = mosaic_of(phase, w, h, |_, _| (r, g, b));
        let (c1, mut c2, c2b) = chroma_estimates(&m, phase, &k);
        average_in_place(&mut c2, &c2b);
        let lum = luminance(&m, phase, &c1, &c2);
        let l = (r + 2.0 * g + b) / 4.0;
        for y in 40..h - 40 {
            for x in 40..w - 40 {
                prop_assert!((lum.at(x, y) - l).abs() < 1e-9, "{phase} L {}", lum.at(x, y));
                prop_assert!((c1.at(x, y) - (-r + 2.0 * g - b) / 4.0).abs() < 1e-9);
                prop_assert!((c2.at(x, y) - (r - b) / 4.0).abs() < 1e-9);
            }
        }
    }

    /// The adaptive path on a flat colour field equals the fixed one: the mask
    /// is zero where there is no luminance texture.
    #[test]
    fn adaptive_equals_fixed_on_flat_colour(
        phase in phase_strategy(),
        r in 0.05f64..0.9, g in 0.05f64..0.9, b in 0.05f64..0.9,
    ) {
        let (w, h) = (192, 160);
        let m = wrap(mosaic_of(phase, w, h, |_, _| (r, g, b)), phase);
        let fixed = separate(&m, &SeparateParams { mask: mimizan_core::separate::MaskMode::Off, ..Default::default() });
        let adaptive = separate(&m, &SeparateParams::default());
        for (a, f) in adaptive.lum.data.iter().zip(&fixed.lum.data) {
            prop_assert!((a - f).abs() < 1e-12);
        }
    }

    /// A neutral smooth gradient is luminance only: it passes untouched in
    /// every phase (no residual chroma estimate above 1e-9).
    #[test]
    fn neutral_gradient_passes_for_every_phase(
        phase in phase_strategy(),
        a in 0.1f64..0.5, slope in -0.3f64..0.3, tilt in -0.3f64..0.3,
    ) {
        let (w, h) = (192, 160);
        let k = LowpassSpec::default().kernel();
        let m = mosaic_of(phase, w, h, |x, y| {
            let v = a + slope * (x as f64 / w as f64) + tilt * (y as f64 / h as f64);
            (v, v, v)
        });
        let (c1, mut c2, c2b) = chroma_estimates(&m, phase, &k);
        average_in_place(&mut c2, &c2b);
        let lum = luminance(&m, phase, &c1, &c2);
        for y in 40..h - 40 {
            for x in 40..w - 40 {
                prop_assert!((lum.at(x, y) - m.at(x, y)).abs() < 1e-9);
            }
        }
    }

    /// The fused row/column passes equal "modulate, then naive 2-D reference".
    #[test]
    fn fused_demodulation_matches_reference(
        phase in phase_strategy(),
        seed in 0u64..1000,
    ) {
        let (w, h) = (29, 23);
        let mut rng = mimizan_core::synth::Rng::new(seed + 1);
        let mut m = Plane::zeros(w, h);
        for v in m.data.iter_mut() {
            *v = rng.uniform();
        }
        let k = LowpassSpec { cutoff: 0.2, transition: 0.1, attenuation_db: 40.0 }.kernel();
        let c = phase.carriers();
        let (c1, c2a, c2b) = chroma_estimates(&m, phase, &k);
        let modulated = |f: &dyn Fn(f64, f64) -> f64| {
            let mut z = Plane::zeros(w, h);
            for y in 0..h {
                for x in 0..w {
                    z.set(x, y, f(sign_x(x), sign_x(y)) * m.at(x, y));
                }
            }
            lowpass_reference(&z, &k)
        };
        let r1 = modulated(&|sx, sy| c.eps * sx * sy);
        let r2a = modulated(&|sx, _| c.a * sx);
        let r2b = modulated(&|_, sy| c.b * sy);
        for i in 0..w * h {
            prop_assert!((c1.data[i] - r1.data[i]).abs() < 1e-12);
            prop_assert!((c2a.data[i] - r2a.data[i]).abs() < 1e-12);
            prop_assert!((c2b.data[i] - r2b.data[i]).abs() < 1e-12);
        }
        // Signed column pass alone against the plain reference of a signed source.
        let rows = convolve_rows_mod(&m, &k, |_, _| 1.0);
        let signed = convolve_cols_signed(&rows, &k, sign_x);
        let mut z = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                z.set(x, y, sign_x(y) * m.at(x, y));
            }
        }
        let reference = lowpass_reference(&z, &k);
        for i in 0..w * h {
            prop_assert!((signed.data[i] - reference.data[i]).abs() < 1e-12);
        }
    }

    /// The mask value is always within [0,1] and monotone: more ring energy
    /// never lowers it, more band energy never raises it.
    #[test]
    fn mask_value_is_bounded_and_monotone(
        p_c in 0.0f64..1e6, p_l in 0.0f64..1e6, n_bin in 1e-3f64..10.0,
        d in 0.0f64..1e5,
    ) {
        let (n_c, n_l) = (300.0, 1200.0);
        let m = mask_value(p_c, p_l, n_c, n_l, n_bin);
        prop_assert!((0.0..=1.0).contains(&m), "{m}");
        prop_assert!(mask_value(p_c, p_l + d, n_c, n_l, n_bin) >= m - 1e-12);
        prop_assert!(mask_value(p_c + d, p_l, n_c, n_l, n_bin) <= m + 1e-12);
    }
}

/// Two runs of the whole separation on the same input are bit-identical
/// (deterministic reductions, SPEC section 12), also with a different thread count.
#[test]
fn separation_is_deterministic_across_thread_counts() {
    use mimizan_core::synth::{mosaic, Scene, SynthParams};
    let s = mosaic(Scene::Fabric, 512, 384, &SynthParams::default());
    let a = separate(&s.mosaic, &SeparateParams::default());
    let pool = rayon::ThreadPoolBuilder::new().num_threads(3).build().unwrap();
    let b = pool.install(|| separate(&s.mosaic, &SeparateParams::default()));
    assert_eq!(a.lum.data, b.lum.data);
    assert_eq!(a.c1.data, b.c1.data);
    assert_eq!(a.c2.data, b.c2.data);
    assert_eq!(a.mask_max.data, b.mask_max.data);
}
