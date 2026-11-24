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
    (xtrans::refine(&m, &tile, &sep, &refine).mapped(|v| tone.back(v)).rgb(), found)
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
