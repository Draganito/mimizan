//! A Bayer demosaic by the least-squares separation that leads on X-Trans.
//!
//! The 2×2 mosaic is written as a 6×6 tile, which is the period the solver
//! works on. Luminance and both chrominances of the frequencies that tile
//! ties together are solved at once, under the prior the mosaic itself
//! makes most probable, and a bank of those priors is weighed per pixel.
//! A second step follows where the picture has two colours. The separation
//! runs under the sRGB curve and comes back to linear light, as a camera
//! file does. Nothing here is treated as burnt out.

use mimizan_core::cfa::BayerPhase;
use mimizan_core::decode::Rect;
use mimizan_core::ingest::{CameraInfo, IngestReport, Mosaic, NoiseModel};
use mimizan_core::plane::Plane as Core;
use mimizan_core::separate::{self, SeparateParams};
use mimizan_core::usm;
use mimizan_lab_xtrans::convert;
use mimizan_lab_xtrans::plane::{Plane, Rgb};
use mimizan_lab_xtrans::xtrans::{self, Curve, Prior, Refine, Selection, XParams, XTile};
use std::f64::consts::PI;

/// Least-squares colour must exceed the separation's by this much before a
/// grey place is repainted.
const CAST_RATIO: f64 = 1.3;
/// And it must be at least this, in the linear chrominance `(2G−R−B)/4` and
/// `(R−B)/4`. Below it the difference is noise.
const CAST_FLOOR: f64 = 0.003;
/// The separation's own colour must stay under this. A red field is above
/// it and keeps the least-squares colour.
const CAST_NEUTRAL: f64 = 0.012;
/// The colour is compared after a blur of this many pixels. The cast on a
/// resolution line is slow; a one-pixel flicker is not what this removes.
const CAST_SIGMA: f64 = 3.0;

/// RGGB, repeated so the period is 6. Row-major.
pub const BAYER6: &str = "RGRGRGGBGBGBRGRGRGGBGBGBRGRGRGGBGBGB";

/// What may be moved while the method is being measured. The defaults are
/// the separation measured on the Kodak set: the X-Trans bank without its
/// line priors, plus eight directions whose luminance falls only as `f¹`.
#[derive(Clone, Copy, Debug)]
pub struct Knobs {
    /// Gaussian σ of the window the priors are weighed in, px.
    pub sigma: f64,
    /// How many pixels' worth of evidence that weight is given.
    pub sharp: f64,
    /// Head start of the prior the search found, in the units of the score.
    pub bias: f64,
    /// The eight priors for lines near the sensor's limit. They were measured
    /// for the X-Trans tile, which holds every colour in every row. A Bayer
    /// row does not, and on a saturated picture they take the colour out.
    pub lines: bool,
    /// How often the second step is repeated.
    pub rounds: usize,
    /// The test that keeps the second step only where the luminance gets
    /// smoother. 0 keeps it everywhere.
    pub check: f64,
    /// Weight of the first step beside the photosite in the second step.
    pub hold: f64,
}

impl Default for Knobs {
    fn default() -> Self {
        Self { sigma: 6.0, sharp: 20.0, bias: 0.0, lines: false, rounds: 1, check: 4.0, hold: 0.15 }
    }
}

/// The three linear channels of a Bayer mosaic. `linear` is one sample per
/// photosite, row-major, in the light the sensor recorded, and `width` is
/// the row length. The frame must continue the tile when it is mirrored.
pub fn demosaic(linear: &[f64], width: usize, knobs: &Knobs) -> Rgb {
    separate(linear, linear, width, knobs, &|_| {}).0
}

/// [`demosaic`], with the prior searched on `fit` rather than on `linear`.
/// A burnt photosite is kept in `linear` and left out of `fit`, so the
/// highlight does not decide the prior and still reaches the picture.
/// `progress` is told the share of the first step that is done.
pub fn separate(
    linear: &[f64],
    fit: &[f64],
    width: usize,
    knobs: &Knobs,
    progress: &dyn Fn(f64),
) -> (Rgb, Prior) {
    assert!(width > 0 && linear.len().is_multiple_of(width), "the mosaic is not a rectangle");
    assert_eq!(linear.len(), fit.len(), "the prior is searched on another frame");
    let height = linear.len() / width;
    let tile = XTile::from_name(BAYER6).expect("the Bayer tile");
    assert!(tile.mirrors(width, height), "this frame does not mirror the Bayer tile");
    let tone = Curve::Srgb;
    let p = XParams::default();
    let start = Prior { lens: 0.3, ..Prior::default() };
    let found = xtrans::fit_prior(
        &tone.mosaic(&Plane::from_vec(width, height, fit.to_vec())),
        &tile,
        &p,
        start,
        8,
        [true; 6],
    );
    let m = tone.mosaic(&Plane::from_vec(width, height, linear.to_vec()));
    let priors = bayer_priors(found, knobs.lines);
    let sel = Selection { sigma: knobs.sigma, sharp: knobs.sharp, bias: knobs.bias };
    let sep = xtrans::separate_bank_watched(&m, &tile, &p, &priors, &sel, progress);
    let refine = Refine { rounds: knobs.rounds, check: knobs.check, hold: knobs.hold, ..Refine::default() };
    let rgb = xtrans::refine(&m, &tile, &sep, &refine).mapped(|v| tone.back(v)).rgb();
    (without_grey_cast(rgb, linear, width), found)
}

/// The priors weighed per pixel.
///
/// The bank measured for X-Trans, without the eight line priors unless
/// asked. Those priors assume every row holds red, green and blue, which
/// is true of the X-Trans tile and false of a Bayer row, and on a saturated
/// picture they take the colour out (kodim03 and kodim15, about 7 dB).
///
/// In their place, eight directions. The luminance falls only as `f¹`, so a
/// fine edge can keep its contrast, and the direction is held with `cos³²`
/// rather than the `cos⁸` of the bank: a Bayer edge is one photosite wide.
/// The colour stays the one the search found, and it still falls with
/// frequency. A colour that does not fall is taken on a saturated picture
/// and the colour goes.
fn bayer_priors(found: Prior, lines: bool) -> Vec<Prior> {
    let mut priors = convert::bank(found);
    if !lines {
        priors.truncate(13);
    }
    let along = Prior { alpha: found.alpha.min(1.0), ..found };
    priors.extend((0..8).map(|j| Prior { oriented: Some((PI * j as f64 / 8.0, 32.0)), ..along }));
    priors
}

/// Take the frequency separation's colour where a grey place was painted.
///
/// On the Z f studio chart the 50–30 lines survive in the luminance and come
/// out blue: the cast is slow, a few levels of `R−B`, so a high-pass never
/// sees it. The separation draws those lines grey. Where its colour stays
/// under [`CAST_NEUTRAL`] and the least squares has more, the separation's
/// colour replaces it and the luminance is the one already solved. A coloured
/// edge is over that limit for both and is left alone. On the 24 Kodak
/// pictures, pixel-sharp and behind a lens of σ 0.75, the replacement moves
/// the colour PSNR by less than 0.01 dB.
fn without_grey_cast(rgb: Rgb, linear: &[f64], width: usize) -> Rgb {
    let height = linear.len() / width;
    let Some((s1, s2)) = separation_chroma(linear, width, height) else { return rgb };
    let (c1, c2) = chroma(&rgb.r.data, &rgb.g.data, &rgb.b.data);
    let lum = luma(&rgb.r.data, &rgb.g.data, &rgb.b.data);
    let mask = cast_mask(&c1, &c2, &s1, &s2, width, height);
    let c1 = mix(&c1, &s1, &mask);
    let c2 = mix(&c2, &s2, &mask);
    from_luma(&lum, &c1, &c2, width, height)
}

fn separation_chroma(linear: &[f64], width: usize, height: usize) -> Option<(Vec<f64>, Vec<f64>)> {
    let sep = separate::separate(&mosaic_of(linear, width, height), &SeparateParams::default()).ok()?;
    Some((sep.c1.data, sep.c2.data))
}

fn mosaic_of(linear: &[f64], width: usize, height: usize) -> Mosaic {
    let n = width * height;
    Mosaic {
        plane: Core::from_vec(width, height, linear.to_vec()),
        phase: Some(BayerPhase::RGGB),
        saturated: vec![0; n],
        colour_ceiling: None,
        noise: NoiseModel::default(),
        wb: [1.0; 3],
        orientation: 1,
        camera: CameraInfo {
            make: String::new(),
            model: String::new(),
            clean_make: String::new(),
            clean_model: String::new(),
            bits: 16,
            exposure: mimizan_core::decode::Exposure::default(),
        },
        xyz_to_cam: None,
        report: IngestReport {
            crop: Rect { x: 0, y: 0, w: width, h: height },
            saturated_raw: 0,
            saturated_dilated: 0,
            colour_ceiling: None,
            above_ceiling: 0,
            defects: 0,
            defects_repaired: false,
            defect_hot_rows: vec![],
            min_norm: 0.0,
            max_norm: 1.0,
            wb: [1.0; 3],
            wb_source: "none".into(),
        },
    }
}

/// 1 where the least squares colour is the cast and the separation is grey.
fn cast_mask(c1: &[f64], c2: &[f64], s1: &[f64], s2: &[f64], width: usize, height: usize) -> Vec<f64> {
    let ls = chroma_mag(c1, c2, width, height);
    let sep = chroma_mag(s1, s2, width, height);
    let mut hard = Core::zeros(width, height);
    for i in 0..hard.data.len() {
        let extra = ls.data[i] > CAST_RATIO * sep.data[i].max(1e-6) && ls.data[i] > CAST_FLOOR;
        let grey = sep.data[i] < CAST_NEUTRAL;
        hard.data[i] = if extra && grey { 1.0 } else { 0.0 };
    }
    let soft = usm::gaussian_blur(&hard, CAST_SIGMA);
    soft.data.iter().map(|v| if *v > 0.30 { ((*v - 0.30) / 0.70).clamp(0.0, 1.0) } else { 0.0 }).collect()
}

fn chroma_mag(c1: &[f64], c2: &[f64], width: usize, height: usize) -> Core {
    let mag: Vec<f64> = c1.iter().zip(c2).map(|(a, b)| a.abs() + b.abs()).collect();
    usm::gaussian_blur(&Core::from_vec(width, height, mag), CAST_SIGMA)
}

fn chroma(r: &[f64], g: &[f64], b: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let c1 = r.iter().zip(g).zip(b).map(|((rv, gv), bv)| (2.0 * gv - rv - bv) / 4.0).collect();
    let c2 = r.iter().zip(b).map(|(rv, bv)| (rv - bv) / 4.0).collect();
    (c1, c2)
}

fn luma(r: &[f64], g: &[f64], b: &[f64]) -> Vec<f64> {
    r.iter().zip(g).zip(b).map(|((rv, gv), bv)| (rv + 2.0 * gv + bv) / 4.0).collect()
}

fn mix(ls: &[f64], sep: &[f64], mask: &[f64]) -> Vec<f64> {
    ls.iter().zip(sep).zip(mask).map(|((a, s), t)| (1.0 - t) * a + t * s).collect()
}

fn from_luma(lum: &[f64], c1: &[f64], c2: &[f64], width: usize, height: usize) -> Rgb {
    let mut r = Vec::with_capacity(lum.len());
    let mut g = Vec::with_capacity(lum.len());
    let mut b = Vec::with_capacity(lum.len());
    for i in 0..lum.len() {
        let (l, u, v) = (lum[i], c1[i], c2[i]);
        r.push(l - u + 2.0 * v);
        g.push(l + u);
        b.push(l - u - 2.0 * v);
    }
    Rgb {
        r: Plane::from_vec(width, height, r),
        g: Plane::from_vec(width, height, g),
        b: Plane::from_vec(width, height, b),
    }
}

#[cfg(test)]
mod tests {
    use super::{chroma, from_luma, luma, without_grey_cast};
    use mimizan_lab_xtrans::plane::{Plane, Rgb};

    fn picture(r: f64, g: f64, b: f64, n: usize) -> Rgb {
        Rgb {
            r: Plane::from_vec(n, n, vec![r; n * n]),
            g: Plane::from_vec(n, n, vec![g; n * n]),
            b: Plane::from_vec(n, n, vec![b; n * n]),
        }
    }

    #[test]
    fn a_blue_cast_on_grey_goes_and_the_luminance_stays() {
        let n = 48;
        let grey = vec![0.22; n * n];
        let got = without_grey_cast(picture(0.20, 0.20, 0.26, n), &grey, n);
        let (c1, c2) = chroma(&got.r.data, &got.g.data, &got.b.data);
        let mid = n * n / 2 + n / 2;
        assert!(c1[mid].abs() < 1e-6 && c2[mid].abs() < 1e-6, "the cast remains");
        let l = luma(&got.r.data, &got.g.data, &got.b.data);
        assert!((l[mid] - (0.20 + 0.40 + 0.26) / 4.0).abs() < 1e-9);
    }

    #[test]
    fn a_red_field_keeps_the_least_squares_colour() {
        let n = 48;
        let mut mosaic = vec![0.0; n * n];
        for y in 0..n {
            for x in 0..n {
                mosaic[y * n + x] = match (x % 2, y % 2) {
                    (0, 0) => 0.55,
                    (1, 1) => 0.05,
                    _ => 0.06,
                };
            }
        }
        let src = from_luma(
            &vec![(0.55 + 0.12 + 0.05) / 4.0; n * n],
            &vec![(0.12 - 0.55 - 0.05) / 4.0; n * n],
            &vec![(0.55 - 0.05) / 4.0; n * n],
            n,
            n,
        );
        let (c1, _) = chroma(&src.r.data, &src.g.data, &src.b.data);
        let got = without_grey_cast(src, &mosaic, n);
        let (d1, _) = chroma(&got.r.data, &got.g.data, &got.b.data);
        let mid = n * n / 2 + n / 2;
        assert!((d1[mid] - c1[mid]).abs() < 1e-6, "the red moved");
    }
}
