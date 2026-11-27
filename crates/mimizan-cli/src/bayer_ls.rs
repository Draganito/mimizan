//! A Bayer demosaic by least squares on the 2×2 period.
//!
//! Luminance and both chrominances of the four frequencies that period ties
//! together are solved at once, under the prior the mosaic itself makes most
//! probable, and a bank of those priors is weighed per pixel. A second step
//! follows where the picture has two colours. It reads which photosite is
//! which colour, and that pattern is the Bayer tile written out to six,
//! which is the period the older solver asked for. The separation runs under
//! a tone curve, the one of [`TONES`] the mosaic itself speaks for, and
//! comes back to linear light, as a camera file does. Nothing here is
//! treated as burnt out.

use crate::bayer2::{self, Spectrum, Weighing};
use mimizan_core::cfa::{BayerPhase, Color};
use mimizan_lab_xtrans::convert::{Developed, Frame, WHITE};
use mimizan_lab_xtrans::plane::{Plane, Rgb};
use mimizan_lab_xtrans::xtrans::{self, Curve, Prior, Refine, XTile};
use rawler::rawimage::RawPhotometricInterpretation;
use rawler::{RawImage, RawImageData};
use rayon::prelude::*;
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

/// RGGB, written out so the period is 6. The second step reads this pattern;
/// the solve itself is on the 2×2. Row-major.
pub const BAYER6: &str = "RGRGRGGBGBGBRGRGRGGBGBGBRGRGRGGBGBGB";

/// The slope of the sRGB curve at black.
const BLACK_SLOPE: f64 = 12.92;

/// A tone curve the separation runs under.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Tone {
    /// The sRGB curve, its straight foot continued below zero and its
    /// power above one.
    Srgb,
    /// A power of the light, moved by `foot` so that it is as steep at
    /// black as the sRGB curve and no steeper, and straight below black.
    /// Zero stays zero and one stays one. Made by [`Tone::power`].
    Power { exponent: f64, foot: f64 },
}

/// The curves a mosaic may choose from.
///
/// A picture a lens has drawn is smooth in light, and the sRGB curve bends
/// it: what was below the sensor's limit is partly above it afterwards. A
/// picture with its own fine detail is steadier under the sRGB curve,
/// which is where its noise and its edges are the same size at every
/// level. The two powers lie between that curve and the light itself.
pub const TONES: [f64; 2] = [0.6, 0.7];

impl Tone {
    /// The power `exponent`, between 0 and 1, with its foot.
    pub fn power(exponent: f64) -> Tone {
        assert!(exponent > 0.0 && exponent < 1.0, "the curve is a root of the light");
        let g = exponent;
        let mut foot: f64 = 0.0;
        for _ in 0..40 {
            let span = (1.0 + foot).powf(g) - foot.powf(g);
            foot = (BLACK_SLOPE * span / g).powf(1.0 / (g - 1.0));
        }
        Tone::Power { exponent, foot }
    }

    /// From light to the curve.
    pub fn to(self, v: f64) -> f64 {
        match self {
            Tone::Srgb => Curve::Srgb.to(v),
            Tone::Power { .. } if v < 0.0 => BLACK_SLOPE * v,
            Tone::Power { exponent: g, foot: e } => {
                ((v + e).powf(g) - e.powf(g)) / ((1.0 + e).powf(g) - e.powf(g))
            }
        }
    }

    /// From the curve back to light.
    pub fn back(self, v: f64) -> f64 {
        match self {
            Tone::Srgb => Curve::Srgb.back(v),
            Tone::Power { .. } if v < 0.0 => v / BLACK_SLOPE,
            Tone::Power { exponent: g, foot: e } => {
                (v * ((1.0 + e).powf(g) - e.powf(g)) + e.powf(g)).powf(1.0 / g) - e
            }
        }
    }

    /// How steep the curve is at the light `v`.
    pub fn slope(self, v: f64) -> f64 {
        match self {
            Tone::Srgb if v <= 0.003_130_8 => BLACK_SLOPE,
            Tone::Srgb => 1.055 / 2.4 * v.powf(1.0 / 2.4 - 1.0),
            Tone::Power { .. } if v < 0.0 => BLACK_SLOPE,
            Tone::Power { exponent: g, foot: e } => {
                g * (v + e).powf(g - 1.0) / ((1.0 + e).powf(g) - e.powf(g))
            }
        }
    }

    /// The mosaic under the curve.
    pub fn mosaic(self, m: &Plane) -> Plane {
        self.mosaic_of(&m.data, m.width)
    }

    /// The mosaic under the curve. `samples` is one value a photosite,
    /// `width` of them to a row.
    pub fn mosaic_of(self, samples: &[f64], width: usize) -> Plane {
        assert!(width > 0 && samples.len().is_multiple_of(width), "the mosaic is not a rectangle");
        let height = samples.len() / width;
        Plane::from_vec(width, height, samples.par_iter().map(|v| self.to(*v)).collect())
    }
}

impl std::fmt::Display for Tone {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Tone::Srgb => write!(f, "sRGB"),
            Tone::Power { exponent, .. } => write!(f, "power {exponent}"),
        }
    }
}

/// What may be moved while the method is being measured. The defaults are
/// the separation measured on the Kodak set: the X-Trans bank without its
/// line priors, plus eight directions whose luminance falls only as `f¹`
/// and eight that are the same in every frame, with a colour as strong
/// as the luminance ([`VIVID`]).
#[derive(Clone, Copy, Debug)]
pub struct Knobs {
    /// Gaussian σ of the window the priors are weighed in, px.
    pub sigma: f64,
    /// Gaussian σ of the window their left-out error is taken in, px.
    pub near: f64,
    /// Share of the left-out error in the weight; 0 weighs by the likelihood alone.
    pub left_out: f64,
    /// How many pixels' worth of evidence that weight is given.
    pub sharp: f64,
    /// Head start of the prior the search found, in the units of the score.
    pub bias: f64,
    /// How often the lens and the noise are moved towards what the mosaic
    /// gives them, after the search. 0 leaves the search's Gaussian lens.
    pub lens_rounds: usize,
    /// The curve to separate under. `None` is the one of sRGB and
    /// [`TONES`] the mosaic speaks for.
    pub tone: Option<Tone>,
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
        Self {
            sigma: 6.0,
            near: 1.0,
            left_out: 0.5,
            sharp: 20.0,
            bias: 0.0,
            lens_rounds: 100,
            tone: None,
            rounds: 1,
            check: 4.0,
            hold: 0.15,
        }
    }
}

/// The three linear channels of a Bayer mosaic. `linear` is one sample per
/// photosite, row-major, in the light the sensor recorded, and `width` is
/// the row length. The frame must continue the tile when it is mirrored.
pub fn demosaic(linear: &[f64], width: usize, knobs: &Knobs) -> Rgb {
    let owned = linear.to_vec();
    separate(owned.clone(), owned, width, knobs, &|_| {}).0
}

/// A photosite this near the frame's highest value is taken to be burnt.
const BURNT: f64 = 0.99;

/// The curve the mosaic `fit`, in light, speaks for, of sRGB and [`TONES`].
///
/// Under each curve the spectrum is searched, coarsely, and two things are
/// asked of it, the same two a spectrum of the bank is asked at a pixel,
/// here over the frame and in the measure of the light: how probable the
/// light is ([`bayer2::likelihood`]) and how well a photosite is told from
/// the others ([`bayer2::told`]). They are weighed as the bank weighs
/// them, with `left_out`.
///
/// That sum is a mean of logarithms, and a dark part of the picture
/// counts in it as much as a light one. In the picture it does not: an
/// error in the dark is seen larger. So a power is taken only when the
/// photosites are also told better in the mean square, the error measured
/// under the sRGB curve, where a picture is looked at
/// ([`bayer2::missed`]).
///
/// A burnt photosite does not say how steep a curve is at white. It is a
/// value the light was at least, which is as probable under one curve as
/// under another, and a sky burnt out would otherwise choose the curve
/// that is steepest there.
pub fn tone_of(fit: &Plane, knobs: &Knobs) -> Tone {
    let said = tone_scores(fit, knobs);
    let mut best = said[0];
    for other in &said[1..] {
        if other.missed < said[0].missed && other.against < best.against {
            best = *other;
        }
    }
    best.tone
}

/// What the mosaic says of one curve. Smaller is better, both.
#[derive(Clone, Copy, Debug)]
struct Said {
    tone: Tone,
    /// The likelihood of the light and the left-out error in it, weighed as the bank weighs them.
    against: f64,
    /// The mean square of the left-out error under the sRGB curve.
    missed: f64,
}

/// sRGB first, then [`TONES`].
fn tone_scores(fit: &Plane, knobs: &Knobs) -> Vec<Said> {
    let top = BURNT * fit.data.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    let start = Prior { lens: 0.3, ..Prior::default() };
    let share = knobs.left_out.clamp(0.0, 1.0);
    let plane = |of: &(dyn Fn(f64) -> f64 + Sync)| {
        Plane::from_vec(fit.width, fit.height, fit.data.par_iter().map(|v| of(*v)).collect())
    };
    let mut said = Vec::new();
    for tone in std::iter::once(Tone::Srgb).chain(TONES.map(Tone::power)) {
        let m = tone.mosaic(fit);
        let found = bayer2::fit_spectrum(&m, start, 4, knobs.lens_rounds);
        let slope = plane(&|v| tone.slope(v));
        let stretch = plane(&|v| if v >= top { 0.0 } else { tone.slope(v).ln() });
        let mut against = 0.0;
        if share < 1.0 {
            against -= (1.0 - share) * bayer2::likelihood(&m, &found, &stretch);
        }
        if share > 0.0 {
            against += share * bayer2::told(&m, &found, &slope, knobs.near, 1.0 / f64::from(WHITE));
        }
        let missed = bayer2::missed(&m, &found, &plane(&|v| tone.slope(v) / Tone::Srgb.slope(v)));
        said.push(Said { tone, against, missed });
    }
    said
}

/// [`demosaic`], with the prior searched on `fit` rather than on `linear`.
/// A burnt photosite is kept in `linear` and left out of `fit`, so the
/// highlight does not decide the prior and still reaches the picture.
/// `progress` is told the share of the first step that is done. The curve
/// it ran under comes back with the picture and the prior.
pub fn separate(
    linear: Vec<f64>,
    fit: Vec<f64>,
    width: usize,
    knobs: &Knobs,
    progress: &dyn Fn(f64),
) -> (Rgb, Prior, Tone) {
    assert!(width > 0 && linear.len().is_multiple_of(width), "the mosaic is not a rectangle");
    assert_eq!(linear.len(), fit.len(), "the prior is searched on another frame");
    let height = linear.len() / width;
    let tile = XTile::from_name(BAYER6).expect("the Bayer tile");
    assert!(tile.mirrors(width, height), "this frame does not mirror the Bayer tile");
    let start = Prior { lens: 0.3, ..Prior::default() };
    // Every set, not every eighth. An eighth is enough on a smooth picture
    // and not on a fine one: kodim19 and kodim22 moved by half a decibel
    // once the search was shown the whole mosaic.
    // The plane the search reads is not used again.
    let (tone, found) = {
        let fit = Plane::from_vec(width, height, fit);
        let tone = knobs.tone.unwrap_or_else(|| tone_of(&fit, knobs));
        let found = bayer2::fit_spectrum(&tone.mosaic(&fit), start, 1, knobs.lens_rounds);
        (tone, found)
    };
    let rgb = {
        let m = tone.mosaic_of(&linear, width);
        // On the curve. The buffer the samples came from is not read again.
        drop(linear);
        let (priors, behind) = bayer_priors(&found);
        let sel = Weighing {
            sigma: knobs.sigma,
            near: knobs.near,
            left_out: knobs.left_out,
            sharp: knobs.sharp,
            bias: knobs.bias,
        };
        let (c1, c2) = bayer2::separate_handicapped(&m, &priors, &behind, &sel, progress);
        let sep = xtrans::from_chroma(&m, &tile, c1, c2);
        let refine =
            Refine { rounds: knobs.rounds, check: knobs.check, hold: knobs.hold, ..Refine::default() };
        let refined = xtrans::refine(&m, &tile, &sep, &refine);
        // The second step has read the mosaic and the first separation.
        drop((m, sep));
        refined.mapped(|v| tone.back(v)).rgb()
    };
    (without_line_checker(rgb), found.prior, tone)
}

/// The colour of the spectra of [`bayer_priors`] that take nothing from
/// the frame, in the measure of [`Prior::chroma`]: as strong as the
/// luminance at 0.25 c/px.
///
/// What the search finds is one spectrum for all of the frame, and it is
/// the likeliest one: no other says the whole mosaic better. But a part
/// of the frame can be nothing like the whole. Where saturated colours
/// lie side by side among photographs, sixteen times the colour found is
/// still a tenth of what lies there, and in a frame of nothing but such
/// colours the likeliest spectrum is noise behind a soft lens. So how the
/// same coloured rings of 5 to 8 px came out hung on the rest of the
/// frame: 17.5 to 34.7 dB below 0.2 c/px, over four frames. With spectra
/// that are the same in every frame it is 36.5 to 38.2 dB.
///
/// The spectra are the same, not what comes of them: the curve the frame
/// is solved under and the spectra they are weighed against are still the
/// frame's.
pub const VIVID: f64 = 1.0;

/// The lens of a [`VIVID`] spectrum, Gaussian σ in px: the lens of the
/// Kodak bench, which the README takes for the sharpest a real lens is on
/// a real sensor. It is a middle. Measured on the ring chart among
/// photographs: at 0.5 px the coloured rings gain 0.8 dB and the black
/// and white ones lose 2.0 dB more to colour; at 1 px the coloured rings
/// lose 0.7 dB and the black and white ones gain 0.6 dB.
pub const VIVID_LENS: f64 = 0.75;

/// The noise of a [`VIVID`] spectrum, in the measure of [`Prior::noise`].
/// A hundred times the search's start: what such a spectrum cannot say is
/// then noise to it and not colour. With a tenth of it the black and white
/// rings lose 2.1 dB more; with ten times it the coloured rings below
/// 0.2 c/px fall from 38.2 to 31.7 dB.
pub const VIVID_NOISE: f64 = 0.01;

/// What a [`VIVID`] spectrum starts behind, in the units of the score.
///
/// At the sensor's limit a black and white line and a colour are the same
/// mosaic, and a spectrum with this much colour takes the line for colour.
/// So it has to say the mosaic this much better than the others before it
/// is taken. Measured on a ring chart lying among photographs: with none,
/// black and white rings lose 3.0 dB, all of it at 2 px and finer, and
/// the 24 Kodak pictures up to 0.07 dB; with this, 1.6 dB and nothing,
/// and the coloured rings keep 38.2 of their 38.7 dB below 0.2 c/px.
pub const VIVID_BEHIND: f64 = 0.5;

/// The spectra weighed per pixel, and what each starts behind.
///
/// The one found; that one with a sixteenth, a quarter, four and sixteen
/// times the colour; and twenty-four with a direction. Eight hold four
/// times the colour to their direction with `cos⁸`. In the next eight the
/// luminance falls only as `f¹`, so a fine edge can keep its contrast, and
/// the direction is held with `cos³²`: a Bayer edge is one photosite wide.
/// Their colour stays the one found. All of these are behind the lens the
/// mosaic was found to have.
///
/// The last eight take nothing from the frame: the spectrum the search
/// starts from, with a colour of [`VIVID`] held to its direction with
/// `cos⁸`, behind a lens of [`VIVID_LENS`] and with a noise of
/// [`VIVID_NOISE`]. They start [`VIVID_BEHIND`] behind; nothing else does.
/// With them a file of 24 megapixels takes 75 s where it took 69 s, on
/// four cores, and no more memory.
///
/// In all of them the colour still falls with frequency. A colour that
/// does not fall is taken on a saturated picture and the colour goes: that
/// is what the X-Trans line priors did here (kodim03 and kodim15, about
/// 7 dB), and they are not in this bank.
pub fn bayer_priors(found: &Spectrum) -> (Vec<Spectrum>, Vec<f64>) {
    let colour = |f: f64| found.with(Prior { chroma: found.prior.chroma.map(|c| c * f), ..found.prior });
    let towards = |q: Spectrum, k: f64| {
        (0..8).map(move |j| q.with(Prior { oriented: Some((PI * j as f64 / 8.0, k)), ..q.prior }))
    };
    let along = found.with(Prior { alpha: found.prior.alpha.min(1.0), ..found.prior });
    let vivid = Spectrum::plain(Prior {
        chroma: [VIVID; 2],
        lens: VIVID_LENS,
        noise: VIVID_NOISE,
        ..Prior::default()
    });
    let mut priors = vec![found.clone()];
    priors.extend([0.0625, 0.25, 4.0, 16.0].map(colour));
    priors.extend(towards(colour(4.0), 8.0));
    priors.extend(towards(along, 32.0));
    let mut behind = vec![0.0; priors.len()];
    priors.extend(towards(vivid, 8.0));
    behind.resize(priors.len(), VIVID_BEHIND);
    (priors, behind)
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
pub fn without_line_checker(rgb: Rgb) -> Rgb {
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

/// The picture, in the camera's own channels. The frame's corner is a red
/// photosite, so the tile is the RGGB one the bench measured. `tell` hears
/// each step.
pub fn develop(img: &RawImage, tell: &dyn Fn(String)) -> Result<Developed, String> {
    let (rx, ry) = red_site(bayer_phase(img).ok_or("the file holds no Bayer mosaic")?);
    let (cx, cy, cw, ch) = crop_of(img)?;
    let x0 = cx + usize::from(cx % 2 != rx);
    let y0 = cy + usize::from(cy % 2 != ry);
    let width = (cx + cw).saturating_sub(x0);
    let height = (cy + ch).saturating_sub(y0);
    if width < 24 || height < 24 {
        return Err("the frame is too small to separate".into());
    }
    let RawImageData::Integer(raw) = &img.data else {
        return Err("the mosaic is not in whole numbers".into());
    };
    if img.cpp != 1 || raw.len() != img.width * img.height {
        return Err("the mosaic does not have one value per photosite".into());
    }
    let white = f64::from(*img.whitelevel.0.first().ok_or("the file gives no white level")?);
    let levels = &img.blacklevel;
    if levels.cpp != 1 || levels.levels.len() != levels.width * levels.height || levels.levels.is_empty() {
        return Err("the file's black levels are not per photosite".into());
    }
    let blacks: Vec<f64> = levels.levels.iter().map(|l| f64::from(l.as_f32())).collect();
    let black_at = |x: usize, y: usize| blacks[(y % levels.height) * levels.width + x % levels.width];
    let black = blacks.iter().sum::<f64>() / blacks.len() as f64;
    if !(white.is_finite() && black.is_finite() && white > black && black >= 0.0 && white <= 65535.0) {
        return Err(format!("black {black} and white {white} are no range"));
    }

    let mut mosaic = vec![0.0; width * height];
    mosaic.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let src = y0 + y;
        let line = &raw[src * img.width + x0..][..width];
        for (x, (v, sample)) in row.iter_mut().zip(line).enumerate() {
            *v = (f64::from(*sample) - black_at(x0 + x, src)) / (white - black);
        }
    });
    let burnt: usize = (0..height)
        .into_par_iter()
        .map(|y| {
            let src = y0 + y;
            raw[src * img.width + x0..][..width].iter().filter(|v| f64::from(**v) >= white - 1.0).count()
        })
        .sum::<usize>();

    // The balance: the camera's, or the one that makes the frame grey.
    // Green is 1. The frame's corner is red, then green, then green, then blue.
    let from_file = [img.wb_coeffs[0], img.wb_coeffs[1], img.wb_coeffs[2]].map(f64::from);
    let balance_from_file = from_file.iter().all(|v| v.is_finite() && *v > 0.0);
    let balance = if balance_from_file {
        [from_file[0] / from_file[1], 1.0, from_file[2] / from_file[1]]
    } else {
        let mut sums = [(0.0, 0.0); 3];
        for (i, v) in mosaic.iter().enumerate() {
            let c = channel_at(i % width, i / width);
            sums[c] = (sums[c].0 + v, sums[c].1 + 1.0);
        }
        let mean = sums.map(|(s, n)| (s / n).max(1e-6));
        [mean[1] / mean[0], 1.0, mean[1] / mean[2]]
    };
    mosaic.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        for (x, v) in row.iter_mut().enumerate() {
            *v *= balance[channel_at(x, y)];
        }
    });
    // A photosite at white measures nothing but "at least this much".
    // The prior is searched on the mosaic cut there, and the picture keeps
    // what was recorded above it.
    let ceiling = balance.iter().copied().fold(f64::INFINITY, f64::min);
    let fit: Vec<f64> = mosaic.par_iter().map(|v| v.min(ceiling)).collect();

    tell("searching the prior".into());
    let (rgb, prior, _) = separate(mosaic, fit, width, &Knobs::default(), &|share| {
        tell(format!("separating luminance and colour, {:.0}%", share.clamp(0.0, 1.0) * 100.0));
    });

    let mut data = vec![0u16; width * height * 3];
    let below: usize = data
        .par_chunks_mut(3 * width)
        .enumerate()
        .map(|(y, row)| {
            let lines = [rgb.r.row(y), rgb.g.row(y), rgb.b.row(y)];
            let mut below = 0;
            for (x, px) in row.chunks_mut(3).enumerate() {
                for (c, v) in px.iter_mut().enumerate() {
                    let level = (lines[c][x] / balance[c] * f64::from(WHITE)).round();
                    below += usize::from(level < 0.0);
                    *v = level.clamp(0.0, f64::from(WHITE)) as u16;
                }
            }
            below
        })
        .sum();
    let tile = XTile::from_name(BAYER6).expect("the Bayer tile");
    Ok(Developed {
        frame: Frame { x: x0, y: y0, width, height, tile },
        data,
        below,
        balance,
        balance_from_file,
        prior,
        burnt,
        sums: [0.0; 3],
        seconds: [0.0; 3],
    })
}

/// The phase of a 2×2 Bayer mosaic, `None` for anything else.
///
/// Four letters over 2×2 are not yet Bayer: rawler names an emerald
/// photosite so (`ERBG`, `RGEB`), and it hands a Fuji Super CCD over under
/// a Bayer name on a grid turned by 45 degrees. Both would be developed
/// into a wrong picture without a word; the names the core reads are the
/// ones read here.
pub fn bayer_phase(img: &RawImage) -> Option<BayerPhase> {
    let RawPhotometricInterpretation::Cfa(config) = &img.photometric else {
        return None;
    };
    if config.cfa.width != 2 || config.cfa.height != 2 || img.fuji_rotation_width.is_some() {
        return None;
    }
    BayerPhase::from_name(&config.cfa.name).ok()
}

/// Column and row, inside the 2×2, of the red photosite.
fn red_site(phase: BayerPhase) -> (usize, usize) {
    let i = phase.tile().iter().position(|c| *c == Color::R).unwrap_or(0);
    (i % 2, i / 2)
}

fn crop_of(img: &RawImage) -> Result<(usize, usize, usize, usize), String> {
    let crop = img
        .crop_area
        .or(img.active_area)
        .map_or((0, 0, img.width, img.height), |r| (r.p.x, r.p.y, r.d.w, r.d.h));
    if crop.0 + crop.2 > img.width || crop.1 + crop.3 > img.height || crop.2 == 0 || crop.3 == 0 {
        return Err("the file's crop lies outside its mosaic".into());
    }
    Ok(crop)
}

/// 0 red, 1 green, 2 blue, on a frame whose corner is red.
fn channel_at(x: usize, y: usize) -> usize {
    match (x % 2, y % 2) {
        (0, 0) => 0,
        (1, 1) => 2,
        _ => 1,
    }
}

#[cfg(test)]
mod tests {
    use super::{
        bayer2, bayer_priors, channel_at, demosaic, from_luma, luma, red_site, tone_scores,
        without_line_checker, xtrans, BayerPhase, Knobs, Plane, Prior, Spectrum, Tone, Weighing, XTile,
        BAYER6, BLACK_SLOPE, VIVID, VIVID_BEHIND, VIVID_LENS, VIVID_NOISE,
    };
    use std::f64::consts::PI;

    #[test]
    fn a_curve_comes_back_and_is_as_steep_as_it_says() {
        for tone in [Tone::Srgb, Tone::power(0.6), Tone::power(0.7)] {
            assert!(tone.to(0.0).abs() < 1e-12, "{tone} moves black");
            assert!((tone.to(1.0) - 1.0).abs() < 1e-12, "{tone} moves white");
            for v in [-0.02, -1e-4, 1e-5, 2e-3, 0.02, 0.18, 0.7, 1.0, 1.3] {
                let back = tone.back(tone.to(v));
                assert!((back - v).abs() < 1e-12, "{tone} takes {v} to {back}");
                let d = 1e-7;
                let rise = (tone.to(v + d) - tone.to(v - d)) / (2.0 * d);
                assert!(
                    (rise / tone.slope(v) - 1.0).abs() < 1e-5,
                    "{tone} at {v}: {rise} for {}",
                    tone.slope(v)
                );
            }
            for v in [-1e-12, 1e-12] {
                let s = tone.slope(v);
                assert!((s / BLACK_SLOPE - 1.0).abs() < 1e-6, "{tone} is {s} steep at black");
            }
        }
    }

    /// A mosaic of many waves, in light. Under `tone` it is a sum of
    /// waves; the three channels share most of them.
    fn waves(w: usize, h: usize, tone: Tone, seed: u64) -> Plane {
        let mut s = seed;
        let mut next = move || {
            s = s.wrapping_mul(6_364_136_223_846_793_005).wrapping_add(1_442_695_040_888_963_407);
            (s >> 11) as f64 / (1u64 << 53) as f64
        };
        // Frequency, direction, strength, and a phase for each channel.
        let parts: Vec<(f64, f64, f64, [f64; 3])> = (0..240)
            .map(|k| {
                let f = 0.004 * (0.45f64 / 0.004).powf(next());
                let dir = PI * next();
                let phase = 2.0 * PI * next();
                // One wave in six is a colour: slow, and not the same in the channels.
                let own = if k % 6 == 0 { [0.0, 2.0 * next(), 4.0 * next()] } else { [0.0; 3] };
                let f = if k % 6 == 0 { f.min(0.08) } else { f };
                (f * dir.cos(), f * dir.sin(), f.powf(-1.3), own.map(|o| phase + o))
            })
            .collect();
        let field: Vec<[f64; 3]> = (0..w * h)
            .map(|i| {
                let (x, y) = ((i % w) as f64, (i / w) as f64);
                let mut v = [0.0; 3];
                for (fx, fy, a, phase) in &parts {
                    for (c, p) in v.iter_mut().zip(phase) {
                        *c += a * (2.0 * PI * (fx * x + fy * y) + p).cos();
                    }
                }
                v
            })
            .collect();
        let span = field.iter().flatten().fold(0.0f64, |m, v| m.max(v.abs()));
        let data = field
            .iter()
            .enumerate()
            .map(|(i, v)| tone.back(0.5 + 0.45 * v[channel_at(i % w, i / w)] / span))
            .collect();
        Plane::from_vec(w, h, data)
    }

    #[test]
    fn the_light_speaks_for_the_curve_it_was_drawn_under() {
        // The two powers are near each other, and waves drawn under one
        // may speak for the other. They do not speak for the sRGB curve,
        // and waves drawn under that speak for neither power.
        let (w, h) = (160usize, 128usize);
        let said = |tone: Tone| tone_scores(&waves(w, h, tone, 17), &Knobs::default());
        let srgb = said(Tone::Srgb);
        assert!(srgb[1..].iter().all(|s| s.against > srgb[0].against), "drawn under sRGB: {srgb:?}");
        let power = said(Tone::power(0.7));
        assert!(power[2].against < power[0].against, "drawn under power 0.7: {power:?}");
    }

    #[test]
    fn a_burnt_sky_does_not_choose_the_curve() {
        // White is where the sRGB curve is flattest of the three. Were the
        // burnt photosites asked how steep a curve is, they would all say
        // the same thing, and there are many of them.
        let (w, h) = (160usize, 128usize);
        let mut frame = waves(w, h, Tone::Srgb, 17);
        let top = frame.data.iter().copied().fold(0.0, f64::max);
        for v in &mut frame.data[..w * h / 2] {
            *v = top;
        }
        let said = tone_scores(&frame, &Knobs::default());
        assert!(said[1..].iter().all(|s| s.against > said[0].against), "half the frame burnt: {said:?}");
    }

    #[test]
    fn the_red_photosite_names_the_tile() {
        assert_eq!(red_site(BayerPhase::RGGB), (0, 0));
        assert_eq!(red_site(BayerPhase::BGGR), (1, 1));
        assert_eq!(red_site(BayerPhase::GRBG), (1, 0));
        assert_eq!(red_site(BayerPhase::GBRG), (0, 1));
        assert_eq!(channel_at(0, 0), 0);
        assert_eq!(channel_at(1, 0), 1);
        assert_eq!(channel_at(0, 1), 1);
        assert_eq!(channel_at(1, 1), 2);
    }

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

    /// A picture with edges in every direction and three unlike channels.
    fn scene(w: usize, h: usize) -> impl Fn(usize, usize) -> [f64; 3] {
        move |x: usize, y: usize| {
            let (u, v) = (x as f64 / w as f64, y as f64 / h as f64);
            let ring = ((u - 0.4).hypot(v - 0.55) * 23.0).sin();
            let bar = if (3.0 * u + 5.0 * v).fract() < 0.5 { 0.12 } else { -0.08 };
            [0.42 + 0.20 * ring + bar, 0.38 + 0.16 * ring - 0.5 * bar, 0.30 - 0.12 * ring + 0.7 * bar * u]
        }
    }

    fn mosaic_of(w: usize, h: usize, at: &dyn Fn(usize, usize) -> [f64; 3]) -> Vec<f64> {
        (0..w * h).map(|i| at(i % w, i / w)[channel_at(i % w, i / w)]).collect()
    }

    /// How far a demosaiced frame is from the scene it was cut from, root mean square.
    fn error_of(w: usize, h: usize, at: &dyn Fn(usize, usize) -> [f64; 3]) -> f64 {
        let got = demosaic(&mosaic_of(w, h, at), w, &Knobs::default());
        let mut sum = 0.0;
        for y in 0..h {
            for x in 0..w {
                let (i, want) = (y * w + x, at(x, y));
                sum += (got.r.data[i] - want[0]).powi(2)
                    + (got.g.data[i] - want[1]).powi(2)
                    + (got.b.data[i] - want[2]).powi(2);
            }
        }
        (sum / (3 * w * h) as f64).sqrt()
    }

    #[test]
    fn an_odd_frame_is_separated() {
        // A sensor whose red is not in the corner loses a row or a column,
        // and an even crop comes out odd. It is the same picture, less one
        // row or one column, and it is told as well.
        let at = scene(64, 48);
        let even = error_of(64, 48, &at);
        for (w, h) in [(63usize, 48usize), (64, 47), (63, 47)] {
            let odd = error_of(w, h, &at);
            assert!(odd < 1.1 * even, "{w}×{h} is {odd} from the scene, the even frame {even}");
        }
    }

    #[test]
    fn a_frame_turned_about_its_diagonal_gives_the_same_colours_turned() {
        // Red stays in the corner and blue beside it, so nothing but the
        // directions of the bank changes, and those change into each other.
        let (w, h) = (72usize, 56usize);
        let at = scene(w, h);
        let turned = move |x: usize, y: usize| at(y, x);
        let (bank, behind) = bayer_priors(&Spectrum::plain(Prior {
            lens: 0.4,
            noise: 1e-3,
            chroma: [0.02, 0.01],
            ..Prior::default()
        }));
        let weighing = Weighing::default();
        let straight = Plane::from_vec(w, h, mosaic_of(w, h, &scene(w, h)));
        let across = Plane::from_vec(h, w, mosaic_of(h, w, &turned));
        let (a1, a2) = bayer2::separate_handicapped(&straight, &bank, &behind, &weighing, &|_| {});
        let (b1, b2) = bayer2::separate_handicapped(&across, &bank, &behind, &weighing, &|_| {});
        let mut worst = 0.0f64;
        for y in 0..h {
            for x in 0..w {
                worst = worst.max((a1.data[y * w + x] - b1.data[x * h + y]).abs());
                worst = worst.max((a2.data[y * w + x] - b2.data[x * h + y]).abs());
            }
        }
        assert!(worst < 1e-9, "the turned frame differs by {worst}");
    }

    #[test]
    fn the_vivid_spectra_have_a_direction_and_start_behind() {
        let found = Spectrum::plain(Prior { chroma: [0.003, 0.002], ..Prior::default() });
        let (bank, behind) = bayer_priors(&found);
        assert_eq!(bank.len(), behind.len(), "a handicap for every spectrum");
        let mut vivid = 0;
        for (spectrum, behind) in bank.iter().zip(&behind) {
            if spectrum.prior.chroma == [VIVID; 2] {
                vivid += 1;
                assert!(spectrum.prior.oriented.is_some(), "a vivid spectrum without a direction");
                assert_eq!(*behind, VIVID_BEHIND);
            } else {
                assert_eq!(*behind, 0.0, "a spectrum with the frame's colour starts behind");
            }
        }
        assert_eq!(vivid, 8);
        assert_eq!(bank[0].prior.chroma, found.prior.chroma, "the first is not the one found");
    }

    #[test]
    fn the_vivid_spectra_are_the_same_in_every_frame() {
        // One frame of little colour behind a sharp lens, one that is all
        // noise behind a soft one, as the search finds a frame of nothing
        // but coloured rings, and that one with a measured lens.
        let quiet = Spectrum::plain(Prior { lens: 0.4, noise: 1e-3, chroma: [0.002; 2], ..Prior::default() });
        let loud = Spectrum::plain(Prior {
            alpha: 1.3,
            alpha_c: 6.0,
            lens: 4.0,
            noise: 0.1,
            chroma: [0.4, 0.2],
            ..Prior::default()
        });
        let measured = Spectrum { lens: Some(std::sync::Arc::new(vec![1.0, 0.5, 0.1])), ..loud.clone() };
        let last = |found: &Spectrum| {
            let (bank, _) = bayer_priors(found);
            bank[bank.len() - 8..].to_vec()
        };
        let first = last(&quiet);
        for spectrum in &first {
            assert_eq!(spectrum.prior.chroma, [VIVID; 2]);
            assert_eq!(spectrum.prior.lens, VIVID_LENS);
            assert_eq!(spectrum.prior.noise, VIVID_NOISE);
            assert!(spectrum.lens.is_none(), "a vivid spectrum behind the frame's lens");
        }
        for other in [last(&loud), last(&measured)] {
            for (a, b) in first.iter().zip(&other) {
                assert_eq!(a.prior, b.prior, "a vivid spectrum took something from the frame");
                assert!(b.lens.is_none(), "a vivid spectrum behind the frame's lens");
            }
        }
    }

    /// Rings of two colours about the middle of a frame, `period` px from
    /// one ring to the next of its colour, and how far a bank puts them
    /// from that: the root mean square over the three channels, 16 px in.
    fn rings_off(period: f64, one: [f64; 3], other: [f64; 3], bank: &[Spectrum], behind: &[f64]) -> f64 {
        let (w, h) = (96usize, 96usize);
        let at = |x: usize, y: usize| {
            let r = ((x as f64 - 47.3).powi(2) + (y as f64 - 48.6).powi(2)).sqrt();
            let t = 0.5 + 0.5 * (2.0 * PI * r / period).cos();
            [0, 1, 2].map(|c| one[c] + t * (other[c] - one[c]))
        };
        let m =
            Plane::from_vec(w, h, (0..w * h).map(|i| at(i % w, i / w)[channel_at(i % w, i / w)]).collect());
        let tile = XTile::from_name(BAYER6).expect("the Bayer tile");
        let (c1, c2) = bayer2::separate_handicapped(&m, bank, behind, &Weighing::default(), &|_| {});
        let got = xtrans::from_chroma(&m, &tile, c1, c2).rgb();
        let mut sum = 0.0;
        for y in 16..h - 16 {
            for x in 16..w - 16 {
                let want = at(x, y);
                for (plane, v) in [&got.r, &got.g, &got.b].into_iter().zip(want) {
                    sum += (plane.data[y * w + x] - v).powi(2);
                }
            }
        }
        (sum / (3 * (w - 32) * (h - 32)) as f64).sqrt()
    }

    /// Two spectra the search has found around a chart. Among photographs
    /// the colour, all of the frame taken together, is a five-hundredth of
    /// the luminance. In a frame of nothing but coloured rings the
    /// likeliest spectrum is noise behind the softest lens the search has.
    fn found_around_a_chart() -> [Prior; 2] {
        let among_photographs = Prior { lens: 0.4, noise: 1e-3, chroma: [0.002; 2], ..Prior::default() };
        let alone = Prior {
            alpha: 2.9,
            alpha_c: 5.5,
            lens: 4.0,
            noise: 0.1,
            chroma: [0.0, 2e-4],
            ..Prior::default()
        };
        [among_photographs, alone]
    }

    #[test]
    fn fine_colours_side_by_side_come_back_whatever_the_frame_holds() {
        // Blue on red is a colour and nothing else; magenta on green has a
        // step in the luminance as well. With sixteen times the frame's
        // colour for the most, the rings' colour went to the luminance: they
        // were 0.146 to 0.263 off. With the vivid spectra blue on red is
        // 0.011 off among photographs and 0.013 alone, magenta on green
        // 0.016 and 0.038.
        let pairs = [([0.15, 0.20, 0.70], [0.70, 0.20, 0.15]), ([0.75, 0.15, 0.60], [0.15, 0.65, 0.20])];
        for (one, other) in pairs {
            let mut errors = Vec::new();
            for found in found_around_a_chart() {
                let (bank, behind) = bayer_priors(&Spectrum::plain(found));
                let n = bank.len() - 8;
                let without = rings_off(6.0, one, other, &bank[..n], &behind[..n]);
                let with = rings_off(6.0, one, other, &bank, &behind);
                assert!(
                    with < 0.25 * without,
                    "the rings are {with} off with the vivid spectra, {without} without, under {found:?}"
                );
                errors.push(with);
            }
            let (least, most) = (errors[0].min(errors[1]), errors[0].max(errors[1]));
            assert!(most < 3.0 * least, "the same rings are {least} off in one frame and {most} in another");
        }
    }

    #[test]
    fn a_black_and_white_ring_is_not_taken_for_colour_when_the_vivid_spectra_start_behind() {
        // At 2.2 px the rings are close to the sensor's limit, where a
        // black and white line and a colour are the same mosaic. Without
        // the vivid spectra they are 0.01512 off, with them 0.01514, and
        // 0.01869 if those start level with the others.
        let (black, white) = ([0.05; 3], [0.85; 3]);
        let [found, _] = found_around_a_chart();
        let (bank, behind) = bayer_priors(&Spectrum::plain(found));
        let n = bank.len() - 8;
        let without = rings_off(2.2, black, white, &bank[..n], &behind[..n]);
        let level = rings_off(2.2, black, white, &bank, &vec![0.0; bank.len()]);
        let with = rings_off(2.2, black, white, &bank, &behind);
        assert!(with < 1.05 * without, "the rings are {with} off with the vivid spectra, {without} without");
        assert!(level > 1.1 * with, "the rings are {with} off behind and {level} level");
    }

    #[test]
    fn two_flat_colours_meet_on_a_line_and_both_come_back() {
        let (w, h) = (128usize, 96usize);
        let left = [0.72, 0.34, 0.22];
        let right = [0.22, 0.34, 0.78];
        let mut mosaic = vec![0.0; w * h];
        for y in 0..h {
            for x in 0..w {
                let rgb = if x < w / 2 { left } else { right };
                mosaic[y * w + x] = match (x % 2, y % 2) {
                    (0, 0) => rgb[0],
                    (1, 1) => rgb[2],
                    _ => rgb[1],
                };
            }
        }
        let got = demosaic(&mosaic, w, &Knobs::default());
        let mut worst = 0.0f64;
        for y in 24..h - 24 {
            for x in [24usize, w - 25] {
                let rgb = if x < w / 2 { left } else { right };
                let i = y * w + x;
                worst = worst.max((got.r.data[i] - rgb[0]).abs());
                worst = worst.max((got.g.data[i] - rgb[1]).abs());
                worst = worst.max((got.b.data[i] - rgb[2]).abs());
            }
        }
        assert!(worst < 0.03, "a flat colour beside an edge moved by {worst}");
    }
}
