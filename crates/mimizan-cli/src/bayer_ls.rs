//! A Bayer demosaic by the least-squares separation that leads on X-Trans.
//!
//! The 2×2 mosaic is written as a 6×6 tile, which is the period the solver
//! works on. Luminance and both chrominances of the frequencies that tile
//! ties together are solved at once, under the prior the mosaic itself
//! makes most probable, and a bank of those priors is weighed per pixel.
//! A second step follows where the picture has two colours. The separation
//! runs under the sRGB curve and comes back to linear light, as a camera
//! file does. Nothing here is treated as burnt out.

use mimizan_lab_xtrans::convert;
use mimizan_lab_xtrans::plane::{Plane, Rgb};
use mimizan_lab_xtrans::xtrans::{self, Curve, Prior, Refine, Selection, XParams, XTile};
use std::f64::consts::PI;

/// A horizontal line, in linear light: it has to differ from the rows above
/// and below by at least this before a checker on it is touched.
const CHECK_FLOOR: f64 = 0.008;
/// The sub-Nyquist horizontal contrast must stay under this times the
/// vertical one. A period-2 checker has none, so the gate is open; a real
/// horizontal edge closes it.
const CHECK_RATIO: f64 = 1.0;
/// The checker has to keep alternating for this many pixels. A short stroke
/// does not.
const CHECK_RUN: usize = 8;
/// The most linear light taken off one pixel. The checker on the Z f lines
/// is inside this; a larger oscillation keeps the rest.
const CHECK_CAP: f64 = 0.012;
/// Below this, a Nyquist sample is not a flicker worth following.
const CHECK_FLICKER: f64 = 0.0015;

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
    (without_line_checker(rgb), found)
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

fn chroma(r: &[f64], g: &[f64], b: &[f64]) -> (Vec<f64>, Vec<f64>) {
    let c1 = r.iter().zip(g).zip(b).map(|((rv, gv), bv)| (2.0 * gv - rv - bv) / 4.0).collect();
    let c2 = r.iter().zip(b).map(|(rv, bv)| (rv - bv) / 4.0).collect();
    (c1, c2)
}

fn luma(r: &[f64], g: &[f64], b: &[f64]) -> Vec<f64> {
    r.iter().zip(g).zip(b).map(|((rv, gv), bv)| (rv + 2.0 * gv + bv) / 4.0).collect()
}

/// Take a one-pixel checker off a horizontal line.
///
/// On the Z f 50–30 lines the brightness itself alternates by a few levels,
/// in step with the photosites, and that is the checker one sees. It keeps
/// the same phase for many pixels and the line has almost no slower
/// horizontal change, which a real edge does. Where both hold, half of that
/// Nyquist sample is put back, and no more than [`CHECK_CAP`]. On the 24
/// Kodak pictures the colour PSNR moves by a few hundredths of a decibel.
fn without_line_checker(rgb: Rgb) -> Rgb {
    let (width, height) = (rgb.r.width, rgb.r.height);
    let (c1, c2) = chroma(&rgb.r.data, &rgb.g.data, &rgb.b.data);
    let lum = luma(&rgb.r.data, &rgb.g.data, &rgb.b.data);
    let mut out_l = lum.clone();
    let mut out_1 = c1.clone();
    let mut out_2 = c2.clone();
    if width < 3 || height < 3 {
        return rgb;
    }
    for y in 1..height - 1 {
        for x in 1..width - 1 {
            let i = y * width + x;
            let up = (lum[i] - lum[i - width]).abs();
            let down = (lum[i] - lum[i + width]).abs();
            let gv = up.max(down);
            let gh = (lum[i + 1] - lum[i - 1]).abs();
            if gv <= CHECK_FLOOR || gh >= CHECK_RATIO * gv || checker_run(&lum, width, x, y) < CHECK_RUN {
                continue;
            }
            let t = ((CHECK_RATIO * gv - gh) / (CHECK_RATIO * gv)).clamp(0.0, 1.0);
            let pull = |src: &[f64], dst: &mut [f64]| {
                let nyq = src[i] - 0.5 * (src[i - 1] + src[i + 1]);
                dst[i] = src[i] - (0.5 * t * nyq).clamp(-CHECK_CAP, CHECK_CAP);
            };
            pull(&lum, &mut out_l);
            pull(&c1, &mut out_1);
            pull(&c2, &mut out_2);
        }
    }
    from_luma(&out_l, &out_1, &out_2, width, height)
}

/// How far the horizontal Nyquist sample keeps flipping sign.
fn checker_run(lum: &[f64], width: usize, x: usize, y: usize) -> usize {
    let nyq = |x: usize| {
        let i = y * width + x;
        lum[i] - 0.5 * (lum[i - 1] + lum[i + 1])
    };
    let s0 = nyq(x);
    if s0.abs() < CHECK_FLICKER {
        return 0;
    }
    let mut n = 1usize;
    let mut prev = s0;
    let mut xx = x;
    while xx > 1 && n < 24 {
        let s = nyq(xx - 1);
        if s * prev >= 0.0 || s.abs() < CHECK_FLICKER {
            break;
        }
        n += 1;
        prev = s;
        xx -= 1;
    }
    prev = s0;
    xx = x;
    while xx + 2 < width && n < 24 {
        let s = nyq(xx + 1);
        if s * prev >= 0.0 || s.abs() < CHECK_FLICKER {
            break;
        }
        n += 1;
        prev = s;
        xx += 1;
    }
    n
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
    use super::{from_luma, luma, without_line_checker};

    #[test]
    fn a_checker_on_a_horizontal_line_goes_and_the_line_stays() {
        let (w, h) = (48usize, 16usize);
        let mut v = vec![0.15; w * h];
        for x in 0..w {
            v[8 * w + x] = if x % 2 == 0 { 0.200 } else { 0.186 };
        }
        let src = from_luma(&v, &vec![0.0; w * h], &vec![0.0; w * h], w, h);
        let got = without_line_checker(src);
        let l = luma(&got.r.data, &got.g.data, &got.b.data);
        let i = 8 * w + 20;
        let nyq = l[i] - 0.5 * (l[i - 1] + l[i + 1]);
        assert!(nyq.abs() < 1e-6, "the checker stays {nyq}");
        assert!(l[i] > l[i - w] + 0.02, "the line was flattened into the ground");
    }

    #[test]
    fn a_vertical_grating_is_not_a_horizontal_checker() {
        let (w, h) = (48usize, 16usize);
        let mut v = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                v[y * w + x] = if x % 2 == 0 { 0.10 } else { 0.30 };
            }
        }
        let src = from_luma(&v, &vec![0.0; w * h], &vec![0.0; w * h], w, h);
        let got = without_line_checker(src);
        let l = luma(&got.r.data, &got.g.data, &got.b.data);
        let i = 8 * w + 20;
        assert!((l[i] - 0.10).abs() < 1e-9, "the vertical lines moved");
    }
}
