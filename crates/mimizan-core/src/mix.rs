//! Three-weight mix via the low-pass chrominance (SPEC section 6).

use crate::error::{Error, Result};
use crate::plane::Plane;
use crate::separate::Separation;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Weights {
    pub r: f64,
    pub g: f64,
    pub b: f64,
}

impl Default for Weights {
    fn default() -> Self {
        Self { r: 0.25, g: 0.50, b: 0.25 }
    }
}

/// Which channels a weight triple refers to.
///
/// `Raw` weights act on the sensor channels as recorded, i.e. they describe a
/// fixed spectral response (what a monochrome sensor has) and do not depend on
/// the illuminant. `Balanced` weights act on the white-balanced channels the
/// separation works in; they are what the mix formula consumes and they change
/// with the balance multipliers. The two are related exactly by
/// `w_bal,i ∝ w_raw,i / wb_i`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum WeightSpace {
    #[default]
    Balanced,
    Raw,
}

impl Weights {
    fn normalised(v: [f64; 3]) -> Self {
        let s = v[0] + v[1] + v[2];
        Self { r: v[0] / s, g: v[1] / s, b: v[2] / s }
    }

    /// Raw-channel weights → weights on the balanced channels (`balanced = raw × wb`).
    pub fn raw_to_balanced(&self, wb: [f64; 3]) -> Self {
        Self::normalised([self.r / wb[0], self.g / wb[1], self.b / wb[2]])
    }

    /// Weights on the balanced channels → raw-channel weights.
    pub fn balanced_to_raw(&self, wb: [f64; 3]) -> Self {
        Self::normalised([self.r * wb[0], self.g * wb[1], self.b * wb[2]])
    }

    /// Weights expressed in `space` → weights for the mix (balanced channels).
    pub fn to_balanced(&self, space: WeightSpace, wb: [f64; 3]) -> Self {
        match space {
            WeightSpace::Balanced => *self,
            WeightSpace::Raw => self.raw_to_balanced(wb),
        }
    }

    pub fn validate(&self) -> Result<()> {
        if self.r < 0.0 || self.g < 0.0 || self.b < 0.0 {
            return Err(Error::Invalid("weights must be non-negative".into()));
        }
        let s = self.r + self.g + self.b;
        if (s - 1.0).abs() > 1e-6 {
            return Err(Error::Invalid(format!("weights must sum to 1 (got {s})")));
        }
        Ok(())
    }

    /// True when the mix is exactly the Bayer luminance.
    pub fn is_native(&self) -> bool {
        (self.r - 0.25).abs() < 1e-12 && (self.g - 0.5).abs() < 1e-12 && (self.b - 0.25).abs() < 1e-12
    }

    /// The same weights behind a contrast filter: `w_i T_i / Σ_j w_j T_j`.
    /// The filter factor (light lost) is normalised away; digitally there is
    /// no exposure to pay for it.
    pub fn filtered(&self, f: ColorFilter) -> Self {
        let t = f.transmission();
        Self::normalised([self.r * t[0], self.g * t[1], self.b * t[2]])
    }

    /// Weights that reproduce CIE Y from the *balanced* camera channels: the
    /// middle row of the camera->XYZ matrix, divided by the balance multipliers
    /// (balanced = raw × wb) and normalised to sum 1. Falls back to native if
    /// the matrix is singular or a weight turns negative.
    pub fn from_xyz_to_cam(m: &[[f64; 3]; 3], wb: [f64; 3]) -> Self {
        let inv = invert3(m);
        match inv {
            Some(i) => {
                let y = [i[1][0] / wb[0], i[1][1] / wb[1], i[1][2] / wb[2]];
                let s: f64 = y.iter().sum();
                let w = Weights { r: y[0] / s, g: y[1] / s, b: y[2] / s };
                if w.r < 0.0 || w.g < 0.0 || w.b < 0.0 {
                    Self::default()
                } else {
                    w
                }
            }
            None => Self::default(),
        }
    }
}

/// The classic black-and-white contrast filters, as transmissions of the
/// three balanced channels. A filter in front of a monochrome sensor is
/// exactly a spectral re-weighting before the sum, which is what the mix
/// does, so a filter is only another weight triple (`Weights::filtered`).
///
/// The transmissions are *approximations*: typical filter curves averaged
/// over typical camera channel bands, not a measured fit for a given glass.
/// They give the character of each filter (what it darkens and lightens),
/// not the curve of one Wratten number. A measured version would be fitted
/// like the monochrome weights: reference camera plus physical filter plus
/// colour chart (`calibrate weights`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ColorFilter {
    #[default]
    None,
    /// Yellow (Wratten 8, K2): cuts blue; sky a little darker, clouds stand out.
    Yellow8,
    /// Yellow-green (Wratten 11): like yellow, foliage lighter, skin a little darker.
    YellowGreen11,
    /// Orange (Wratten 16): sky clearly darker, freckles and redness vanish.
    Orange16,
    /// Red (Wratten 25): sky near black, foliage dark, skin porcelain.
    Red25,
    /// Green (Wratten 58): foliage light, lips and skin dark, red flowers black.
    Green58,
    /// Blue (Wratten 47): haze emphasised, skin dark and blotchy; the orthochromatic look.
    Blue47,
}

impl ColorFilter {
    pub const ALL: [ColorFilter; 7] = [
        ColorFilter::None,
        ColorFilter::Yellow8,
        ColorFilter::YellowGreen11,
        ColorFilter::Orange16,
        ColorFilter::Red25,
        ColorFilter::Green58,
        ColorFilter::Blue47,
    ];

    /// Transmission of the balanced R, G, B channels.
    pub fn transmission(self) -> [f64; 3] {
        match self {
            ColorFilter::None => [1.0, 1.0, 1.0],
            ColorFilter::Yellow8 => [1.0, 0.90, 0.10],
            ColorFilter::YellowGreen11 => [0.55, 1.0, 0.25],
            ColorFilter::Orange16 => [1.0, 0.50, 0.02],
            ColorFilter::Red25 => [1.0, 0.08, 0.0],
            ColorFilter::Green58 => [0.10, 1.0, 0.10],
            ColorFilter::Blue47 => [0.03, 0.25, 1.0],
        }
    }

    /// Short name as used on the command line and in the GUI.
    pub fn name(self) -> &'static str {
        match self {
            ColorFilter::None => "none",
            ColorFilter::Yellow8 => "yellow-8",
            ColorFilter::YellowGreen11 => "yellow-green-11",
            ColorFilter::Orange16 => "orange-16",
            ColorFilter::Red25 => "red-25",
            ColorFilter::Green58 => "green-58",
            ColorFilter::Blue47 => "blue-47",
        }
    }

    /// Colour word only ("yellow", "red", …).
    pub fn colour(self) -> &'static str {
        match self {
            ColorFilter::None => "none",
            ColorFilter::Yellow8 => "yellow",
            ColorFilter::YellowGreen11 => "yellow-green",
            ColorFilter::Orange16 => "orange",
            ColorFilter::Red25 => "red",
            ColorFilter::Green58 => "green",
            ColorFilter::Blue47 => "blue",
        }
    }

    /// Wratten number the approximation is modelled on (none for `None`).
    pub fn wratten(self) -> Option<u8> {
        match self {
            ColorFilter::None => None,
            ColorFilter::Yellow8 => Some(8),
            ColorFilter::YellowGreen11 => Some(11),
            ColorFilter::Orange16 => Some(16),
            ColorFilter::Red25 => Some(25),
            ColorFilter::Green58 => Some(58),
            ColorFilter::Blue47 => Some(47),
        }
    }

    /// What the filter does to a scene, one line.
    pub fn effect(self) -> &'static str {
        match self {
            ColorFilter::None => "no filter",
            ColorFilter::Yellow8 => "sky a little darker, clouds stand out, skin natural",
            ColorFilter::YellowGreen11 => "like yellow; foliage lighter, skin a little darker",
            ColorFilter::Orange16 => "sky clearly darker, freckles and redness vanish, brick and wood light",
            ColorFilter::Red25 => "sky near black, foliage dark, skin porcelain",
            ColorFilter::Green58 => "foliage light, lips and skin dark, red flowers black",
            ColorFilter::Blue47 => "haze emphasised, skin dark and blotchy; the orthochromatic look",
        }
    }

    /// Parse `name()`, the Wratten number, or the colour word.
    pub fn parse(s: &str) -> Option<Self> {
        let k = s.trim().to_ascii_lowercase().replace([' ', '_'], "-");
        Self::ALL.iter().copied().find(|f| {
            k == f.name()
                || k == f.colour()
                || f.wratten().is_some_and(|n| k == n.to_string() || k == format!("wratten-{n}"))
        })
    }
}

fn invert3(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if det.abs() < 1e-12 {
        return None;
    }
    let d = 1.0 / det;
    Some([
        [
            (m[1][1] * m[2][2] - m[1][2] * m[2][1]) * d,
            (m[0][2] * m[2][1] - m[0][1] * m[2][2]) * d,
            (m[0][1] * m[1][2] - m[0][2] * m[1][1]) * d,
        ],
        [
            (m[1][2] * m[2][0] - m[1][0] * m[2][2]) * d,
            (m[0][0] * m[2][2] - m[0][2] * m[2][0]) * d,
            (m[0][2] * m[1][0] - m[0][0] * m[1][2]) * d,
        ],
        [
            (m[1][0] * m[2][1] - m[1][1] * m[2][0]) * d,
            (m[0][1] * m[2][0] - m[0][0] * m[2][1]) * d,
            (m[0][0] * m[1][1] - m[0][1] * m[1][0]) * d,
        ],
    ])
}

/// `out = L̂ + (w_G − w_R − w_B)·Ĉ1' + 2·(w_R − w_B)·Ĉ2'` (mask gains are inside Ĉ').
pub fn mix(sep: &Separation, w: &Weights) -> Plane {
    if w.is_native() {
        return sep.lum.clone();
    }
    let k1 = w.g - w.r - w.b;
    let k2 = 2.0 * (w.r - w.b);
    let width = sep.lum.width;
    let mut out = Plane::zeros(width, sep.lum.height);
    out.data.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let (l, c1, c2) = (sep.lum.row(y), sep.c1.row(y), sep.c2.row(y));
        for x in 0..width {
            row[x] = l[x] + k1 * c1[x] + k2 * c2[x];
        }
    });
    out
}

/// Consume the separation: the luminance buffer becomes the mix (no extra plane).
pub fn mix_into(sep: Separation, w: &Weights) -> Plane {
    if w.is_native() {
        return sep.lum;
    }
    let k1 = w.g - w.r - w.b;
    let k2 = 2.0 * (w.r - w.b);
    let Separation { mut lum, c1, c2, .. } = sep;
    let width = lum.width;
    lum.data.par_chunks_mut(width).enumerate().for_each(|(y, row)| {
        let (c1, c2) = (c1.row(y), c2.row(y));
        for x in 0..width {
            row[x] += k1 * c1[x] + k2 * c2[x];
        }
    });
    lum
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pure_red_weights_recover_r_on_flat_field() {
        // On a flat field L̂ = (R+2G+B)/4, C1 = (-R+2G-B)/4, C2 = (R-B)/4.
        let (r, g, b) = (0.6, 0.3, 0.1);
        let one = |v: f64| Plane::from_vec(2, 1, vec![v; 2]);
        let sep = Separation {
            lum: one((r + 2.0 * g + b) / 4.0),
            c1: one((-r + 2.0 * g - b) / 4.0),
            c2: one((r - b) / 4.0),
            mask_max: one(0.5),
            rounds: 1,
            phase: crate::cfa::BayerPhase::RGGB,
            computed: 0.0,
        };
        let red = mix(&sep, &Weights { r: 1.0, g: 0.0, b: 0.0 });
        assert!((red.at(0, 0) - r).abs() < 1e-12);
        let green = mix(&sep, &Weights { r: 0.0, g: 1.0, b: 0.0 });
        assert!((green.at(0, 0) - g).abs() < 1e-12);
        let blue = mix(&sep, &Weights { r: 0.0, g: 0.0, b: 1.0 });
        assert!((blue.at(0, 0) - b).abs() < 1e-12);
        let y = mix(&sep, &Weights { r: 0.2126, g: 0.7152, b: 0.0722 });
        assert!((y.at(0, 0) - (0.2126 * r + 0.7152 * g + 0.0722 * b)).abs() < 1e-12);
    }

    #[test]
    fn filters_reweight_and_normalise() {
        let base = Weights { r: 0.171, g: 0.489, b: 0.340 };
        for f in ColorFilter::ALL {
            let w = base.filtered(f);
            assert!((w.r + w.g + w.b - 1.0).abs() < 1e-12, "{f:?}");
            assert!(w.r >= 0.0 && w.g >= 0.0 && w.b >= 0.0, "{f:?}");
            assert_eq!(ColorFilter::parse(f.name()), Some(f));
            if let Some(n) = f.wratten() {
                assert_eq!(ColorFilter::parse(&n.to_string()), Some(f));
            }
        }
        assert_eq!(base.filtered(ColorFilter::None), base);
        // Red passes no blue at all; yellow darkens blue relative to the base.
        assert_eq!(base.filtered(ColorFilter::Red25).b, 0.0);
        assert!(base.filtered(ColorFilter::Yellow8).b < base.b);
        assert!(base.filtered(ColorFilter::Blue47).b > base.b);
        assert_eq!(ColorFilter::parse("Wratten 25"), Some(ColorFilter::Red25));
        assert_eq!(ColorFilter::parse("purple"), None);
    }

    #[test]
    fn raw_and_balanced_spaces_round_trip_and_match_measurement() {
        // Session 1 of the Z f against the monochrome reference: as-shot wb and the fit in
        // balanced space, which must equal the raw-space fit converted.
        let wb = [2.015625, 1.0, 1.111328125];
        let raw = Weights { r: 0.284, g: 0.404, b: 0.312 };
        let bal = raw.raw_to_balanced(wb);
        assert!(
            (bal.r - 0.171).abs() < 1.5e-3
                && (bal.g - 0.489).abs() < 1.5e-3
                && (bal.b - 0.340).abs() < 1.5e-3
        );
        let back = bal.balanced_to_raw(wb);
        assert!(
            (back.r - raw.r).abs() < 1e-12
                && (back.g - raw.g).abs() < 1e-12
                && (back.b - raw.b).abs() < 1e-12
        );
        assert!((bal.r + bal.g + bal.b - 1.0).abs() < 1e-12);
        assert_eq!(raw.to_balanced(WeightSpace::Balanced, wb), raw);
        assert_eq!(raw.to_balanced(WeightSpace::Raw, wb), bal);
    }

    #[test]
    fn raw_weights_mix_the_unbalanced_channels() {
        // A flat field with raw (R, G, B) = (0.2, 0.5, 0.1) under wb (2, 1, 3):
        // raw-space weights applied through the balanced mix must give
        // Σ w_raw,i · raw_i up to the free scale, i.e. the same ratios between
        // two flat fields.
        let wb = [2.0, 1.0, 3.0];
        let raw_w = Weights { r: 0.5, g: 0.25, b: 0.25 };
        let bal_w = raw_w.raw_to_balanced(wb);
        let field = |r: f64, g: f64, b: f64| {
            let (rb, gb, bb) = (r * wb[0], g * wb[1], b * wb[2]);
            let one = |v: f64| Plane::from_vec(2, 1, vec![v; 2]);
            let sep = Separation {
                lum: one((rb + 2.0 * gb + bb) / 4.0),
                c1: one((-rb + 2.0 * gb - bb) / 4.0),
                c2: one((rb - bb) / 4.0),
                mask_max: one(0.5),
                rounds: 1,
                phase: crate::cfa::BayerPhase::RGGB,
                computed: 0.0,
            };
            mix(&sep, &bal_w).at(0, 0)
        };
        let a = field(0.2, 0.5, 0.1);
        let b = field(0.4, 0.1, 0.3);
        let want_a = 0.5 * 0.2 + 0.25 * 0.5 + 0.25 * 0.1;
        let want_b = 0.5 * 0.4 + 0.25 * 0.1 + 0.25 * 0.3;
        assert!((a / b - want_a / want_b).abs() < 1e-12);
    }
}
