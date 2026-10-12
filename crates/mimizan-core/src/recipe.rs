//! The print recipe: every parameter of the output stage, one object
//! (Zonensystem paper, sections 5, 7 and 11; appendix A).
//!
//! The recipe is what the GUI edits for five hours and what the export runs
//! once against the linear negative. It holds no pixel. The same object is
//! the element of the history, the argument of the preview and of the
//! export, the `recipe` field of the print's `ImageDescription`, and the
//! file `mimizan print --recipe` reads. Identical recipes give identical
//! 16-bit files; that is why the export never needs to see the history.

use crate::burn::Burn;
use crate::calib::LookFile;
use crate::error::{Error, Result};
use crate::print::PrintSize;
use crate::resize::ResampleKernel;
use crate::screen::ScreenSharpen;
use crate::tone::Tone;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// What sharpens the print after the curve, if anything.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum Sharpen {
    #[default]
    Off,
    /// Unsharp mask, radius from the viewing model, masked by the sidecar.
    Usm { amount: f64 },
    /// Richardson–Lucy, `passes` (1..10) on every pixel, then an optional USM.
    Deconv {
        passes: u32,
        #[serde(default)]
        usm_amount: f64,
    },
    /// Exact compensation of the known screen losses (replaces the USM).
    Screen { amount: f64, max_gain: f64 },
}

impl Sharpen {
    /// USM amount, 0 when none follows.
    pub fn usm_amount(&self) -> f64 {
        match *self {
            Self::Usm { amount } => amount,
            Self::Deconv { usm_amount, .. } => usm_amount,
            Self::Off | Self::Screen { .. } => 0.0,
        }
    }

    pub fn deconv_passes(&self) -> u32 {
        match *self {
            Self::Deconv { passes, .. } => passes,
            _ => 0,
        }
    }

    pub fn screen(&self) -> Option<ScreenSharpen> {
        match *self {
            Self::Screen { amount, max_gain } => Some(ScreenSharpen { amount, max_gain }),
            _ => None,
        }
    }

    /// Anything that consults the mask (USM or screen) is on.
    pub fn uses_mask(&self) -> bool {
        match *self {
            Self::Off => false,
            Self::Usm { amount } => amount > 0.0,
            Self::Deconv { usm_amount, .. } => usm_amount > 0.0,
            Self::Screen { amount, .. } => amount > 0.0,
        }
    }

    pub fn validate(&self) -> Result<()> {
        let usm = |a: f64| {
            if !(0.0..=1.5).contains(&a) {
                return Err(Error::Invalid(format!("usm amount {a} not within 0..1.5")));
            }
            Ok(())
        };
        match *self {
            Self::Off => Ok(()),
            Self::Usm { amount } => usm(amount),
            Self::Deconv { passes, usm_amount } => {
                if !(1..=crate::deconv::DECONV_ITERATIONS as u32).contains(&passes) {
                    return Err(Error::Invalid(format!(
                        "deconv passes {passes} not within 1..{}",
                        crate::deconv::DECONV_ITERATIONS
                    )));
                }
                usm(usm_amount)
            }
            Self::Screen { amount, max_gain } => {
                if !(0.0..=1.0).contains(&amount) {
                    return Err(Error::Invalid(format!("screen amount {amount} not within 0..1")));
                }
                if !(1.0..=4.0).contains(&max_gain) {
                    return Err(Error::Invalid(format!("screen max gain {max_gain} not within 1..4")));
                }
                Ok(())
            }
        }
    }
}

/// Current recipe schema.
pub const SCHEMA: u32 = 1;

fn default_dpi() -> f64 {
    300.0
}

fn default_schema() -> u32 {
    SCHEMA
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    #[serde(default = "default_schema")]
    pub schema: u32,
    /// Placing, in stops, before the curve (paper, equation 1).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub place: f64,
    /// The tone: the paper with its two exposures (`paper.rs`), or a look
    /// (neutral, reference, custom) for the measurement pipeline and older
    /// recipes (`tone.rs`). The field keeps its old name in the JSON.
    pub curve: Tone,
    /// Dodge & burn shapes, local placing (`burn.rs`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub dodge_burn: Vec<Burn>,
    /// Resampling kernel of the size step.
    #[serde(default)]
    pub resample: ResampleKernel,
    /// Output size; `None` keeps the negative's pixels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<PrintSize>,
    #[serde(default = "default_dpi")]
    pub dpi: f64,
    /// Paper border around the fitted picture, mm (converted with `dpi`),
    /// filled with paper white, the curve's value at linear 1.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub border_mm: f64,
    #[serde(default)]
    pub sharpen: Sharpen,
}

impl Default for Recipe {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            place: 0.0,
            curve: Tone::default(),
            dodge_burn: Vec::new(),
            resample: ResampleKernel::default(),
            size: None,
            dpi: default_dpi(),
            border_mm: 0.0,
            sharpen: Sharpen::Off,
        }
    }
}

impl Recipe {
    /// A recipe with just a look, everything else default.
    pub fn with_look(look: LookFile) -> Self {
        Self { curve: Tone::Look(look), ..Self::default() }
    }

    /// A recipe on paper at the base exposure, everything else default.
    pub fn on_paper() -> Self {
        Self { curve: Tone::paper(), ..Self::default() }
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != SCHEMA {
            return Err(Error::Invalid(format!("recipe schema {} not supported", self.schema)));
        }
        if !self.place.is_finite() || self.place.abs() > crate::burn::EV_LIMIT {
            return Err(Error::Invalid(format!(
                "place {} not within ±{}",
                self.place,
                crate::burn::EV_LIMIT
            )));
        }
        self.curve.validate()?;
        for b in &self.dodge_burn {
            b.validate()?;
        }
        if !(self.dpi.is_finite() && self.dpi > 0.0) {
            return Err(Error::Invalid("dpi must be positive".into()));
        }
        if !(self.border_mm.is_finite() && self.border_mm >= 0.0) {
            return Err(Error::Invalid("border must be zero or positive".into()));
        }
        self.sharpen.validate()
    }

    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        let r: Recipe = serde_json::from_str(&s)?;
        r.validate()?;
        Ok(r)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }

    /// Border in pixels at this recipe's dpi.
    pub fn border_px(&self) -> usize {
        if self.border_mm <= 0.0 {
            0
        } else {
            (self.border_mm / 25.4 * self.dpi).round() as usize
        }
    }

    /// Does this recipe change any pixel beyond the plain curve (place,
    /// shapes, border)? For log lines and the description.
    pub fn has_local_work(&self) -> bool {
        self.place != 0.0 || self.dodge_burn.iter().any(|b| b.ev() != 0.0) || self.border_mm > 0.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips_and_is_small() {
        let r = Recipe::default();
        r.validate().unwrap();
        let json = serde_json::to_string(&r).unwrap();
        // Zero place, no shapes, no size, no border are omitted.
        assert!(!json.contains("place") && !json.contains("dodge_burn") && !json.contains("border"));
        assert!(json.contains(r#""resample":"lanczos3""#), "{json}");
        assert!(json.contains(r#""sharpen":{"mode":"off"}"#), "{json}");
        let back: Recipe = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn full_recipe_round_trips() {
        let r = Recipe {
            place: -0.5,
            curve: Tone::Paper(crate::paper::SplitGrade::new(0.25, -0.5)),
            dodge_burn: vec![
                Burn::radial(0.452, 0.61, 0.15, 0.5, 0.75),
                Burn::Gradient {
                    x0: 0.5,
                    y0: 0.0,
                    x1: 0.5,
                    y1: 0.4,
                    ev: 1.0,
                    filter: crate::burn::Filter::Hard,
                },
            ],
            resample: ResampleKernel::BSpline,
            size: Some(PrintSize::Physical { width_in: 11.811, height_in: 15.748 }),
            dpi: 360.0,
            border_mm: 20.0,
            sharpen: Sharpen::Deconv { passes: 5, usm_amount: 0.5 },
            ..Recipe::default()
        };
        r.validate().unwrap();
        let json = serde_json::to_string_pretty(&r).unwrap();
        assert!(json.contains(r#""paper": "multigrade""#) && json.contains(r#""filter": "hard""#), "{json}");
        let back: Recipe = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        assert_eq!(r.border_px(), 283);
        assert!(r.has_local_work());
        // Older fields missing: defaults fill in.
        let minimal: Recipe =
            serde_json::from_str(r#"{"curve":{"schema":1,"name":"neutral","points":[[0,0],[1,1]],"encoding":"gamma22","fitted_against":null}}"#)
                .unwrap();
        assert_eq!(minimal, Recipe::default());
    }

    #[test]
    fn validation_catches_ranges() {
        let bad = Recipe { place: 9.0, ..Recipe::default() };
        assert!(bad.validate().is_err());
        let bad = Recipe { sharpen: Sharpen::Screen { amount: 2.0, max_gain: 2.0 }, ..Recipe::default() };
        assert!(bad.validate().is_err());
        let bad = Recipe { sharpen: Sharpen::Deconv { passes: 0, usm_amount: 0.0 }, ..Recipe::default() };
        assert!(bad.validate().is_err());
        let bad = Recipe { border_mm: -1.0, ..Recipe::default() };
        assert!(bad.validate().is_err());
        let bad = Recipe { schema: 2, ..Recipe::default() };
        assert!(bad.validate().is_err());
        let bad = Recipe { curve: Tone::Paper(crate::paper::SplitGrade::new(0.0, 9.0)), ..Recipe::default() };
        assert!(bad.validate().is_err());
        Recipe::on_paper().validate().unwrap();
        assert!(Sharpen::Usm { amount: 1.5 }.validate().is_ok());
        assert!(Sharpen::Usm { amount: 1.6 }.validate().is_err());
        assert!(!Sharpen::Deconv { passes: 3, usm_amount: 0.0 }.uses_mask());
        assert!(Sharpen::Deconv { passes: 3, usm_amount: 0.3 }.uses_mask());
    }
}
