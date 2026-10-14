//! The print recipe: every parameter of the output stage, one object
//! (Zonensystem paper, sections 5, 7 and 11; appendix A).
//!
//! The recipe is what the GUI edits for five hours and what the export runs
//! once against the linear negative. It holds no pixel. The same object is
//! the element of the history, the argument of the preview and of the
//! export, the `recipe` field of the print's `ImageDescription`, and the
//! file `mimizan print --recipe` reads. Identical recipes give identical
//! 16-bit files; that is why the export never needs to see the history.

use crate::calib::LookFile;
use crate::error::{Error, Result};
use crate::print::PrintSize;
use crate::resize::ResampleKernel;
use crate::tone::Tone;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Largest USM amount a recipe accepts (the GUI slider ends here too).
pub const USM_MAX: f64 = 1.5;

/// Scale compensation after the curve: the enlargement blur, if any.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode", rename_all = "kebab-case")]
pub enum Sharpen {
    #[default]
    Off,
    /// Unsharp mask of the enlargement blur, masked by the sidecar.
    /// Native size and reductions have no such blur, so the amount does nothing.
    Usm { amount: f64 },
    /// Richardson–Lucy of the enlargement blur, `passes` (1..10) on every
    /// pixel, then an optional USM of that same blur.
    Deconv {
        passes: u32,
        #[serde(default)]
        usm_amount: f64,
    },
}

impl Sharpen {
    /// USM amount, 0 when none follows.
    pub fn usm_amount(&self) -> f64 {
        match *self {
            Self::Usm { amount } => amount,
            Self::Deconv { usm_amount, .. } => usm_amount,
            Self::Off => 0.0,
        }
    }

    pub fn deconv_passes(&self) -> u32 {
        match *self {
            Self::Deconv { passes, .. } => passes,
            _ => 0,
        }
    }

    /// Is a USM, which consults the mask, on?
    pub fn uses_mask(&self) -> bool {
        match *self {
            Self::Off => false,
            Self::Usm { amount } => amount > 0.0,
            Self::Deconv { usm_amount, .. } => usm_amount > 0.0,
        }
    }

    pub fn validate(&self) -> Result<()> {
        let usm = |a: f64| {
            if !(0.0..=USM_MAX).contains(&a) {
                return Err(Error::Invalid(format!("usm amount {a} not within 0..{USM_MAX}")));
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
    /// Window on the upright negative, whole pixels, before the size step.
    /// `None` is the full frame. Old recipes omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<CropRect>,
}

impl Default for Recipe {
    fn default() -> Self {
        Self {
            schema: SCHEMA,
            place: 0.0,
            curve: Tone::default(),
            resample: ResampleKernel::default(),
            size: None,
            dpi: default_dpi(),
            border_mm: 0.0,
            sharpen: Sharpen::Off,
            crop: None,
        }
    }
}

/// Which corner of a crop window is being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropCorner {
    Nw,
    Ne,
    Se,
    Sw,
}

/// A window on the upright negative, in whole pixels. The origin is the
/// top-left. Nothing here resamples: the size step is the only resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CropRect {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl CropRect {
    /// Pull the window back inside `img_w`×`img_h`. A zero image is left as it is.
    pub fn sanitized(self, img_w: u32, img_h: u32) -> Self {
        if img_w == 0 || img_h == 0 {
            return self;
        }
        let w = self.w.max(1).min(img_w);
        let h = self.h.max(1).min(img_h);
        Self { x: self.x.min(img_w - w), y: self.y.min(img_h - h), w, h }
    }

    /// 90° clockwise inside an image of `w`×`h`. The image becomes `h`×`w`.
    pub fn rotate_cw(self, w: u32, h: u32) -> Self {
        Self { x: h.saturating_sub(self.y.saturating_add(self.h)), y: self.x, w: self.h, h: self.w }
            .sanitized(h, w)
    }

    /// `quarters` clockwise turns. `w`×`h` is the image the rectangle is in now.
    pub fn turned(self, quarters: u8, w: u32, h: u32) -> Self {
        let mut c = self;
        let (mut w, mut h) = (w, h);
        for _ in 0..quarters % 4 {
            c = c.rotate_cw(w, h);
            std::mem::swap(&mut w, &mut h);
        }
        c
    }

    /// Largest window of aspect `aspect_w`:`aspect_h` that fits in the image,
    /// centered. The caller orients the two sides (long with long).
    pub fn inscribed(img_w: u32, img_h: u32, aspect_w: u32, aspect_h: u32) -> Self {
        let img_w = img_w.max(1);
        let img_h = img_h.max(1);
        let aw = u64::from(aspect_w.max(1));
        let ah = u64::from(aspect_h.max(1));
        let (cw, ch) = if u64::from(img_w) * ah >= u64::from(img_h) * aw {
            let ch = img_h;
            let cw = (u64::from(ch) * aw / ah) as u32;
            (cw.max(1).min(img_w), ch)
        } else {
            let cw = img_w;
            let ch = (u64::from(cw) * ah / aw) as u32;
            (cw, ch.max(1).min(img_h))
        };
        Self { x: (img_w - cw) / 2, y: (img_h - ch) / 2, w: cw, h: ch }
    }

    /// Move, keeping the size, staying inside the image.
    pub fn shift(self, dx: i32, dy: i32, img_w: u32, img_h: u32) -> Self {
        let x = (self.x as i32 + dx).clamp(0, img_w as i32 - self.w as i32);
        let y = (self.y as i32 + dy).clamp(0, img_h as i32 - self.h as i32);
        Self { x: x as u32, y: y as u32, ..self }
    }

    /// Drag `corner` to `pointer` in image pixels. The opposite corner stays.
    /// The window keeps `aspect` (`width`, `height`) and stays inside.
    pub fn scale_corner(
        self,
        corner: CropCorner,
        pointer: (i32, i32),
        img_w: u32,
        img_h: u32,
        aspect: (u32, u32),
    ) -> Self {
        let (px, py) = pointer;
        let aw = i64::from(aspect.0.max(1));
        let ah = i64::from(aspect.1.max(1));
        let (fx, fy, sx, sy) = match corner {
            CropCorner::Se => (self.x as i64, self.y as i64, 1i64, 1i64),
            CropCorner::Nw => ((self.x + self.w) as i64, (self.y + self.h) as i64, -1, -1),
            CropCorner::Ne => (self.x as i64, (self.y + self.h) as i64, 1, -1),
            CropCorner::Sw => ((self.x + self.w) as i64, self.y as i64, -1, 1),
        };
        let max_w = match corner {
            CropCorner::Se | CropCorner::Ne => i64::from(img_w) - fx,
            CropCorner::Nw | CropCorner::Sw => fx,
        }
        .max(1);
        let max_h = match corner {
            CropCorner::Se | CropCorner::Sw => i64::from(img_h) - fy,
            CropCorner::Nw | CropCorner::Ne => fy,
        }
        .max(1);
        let pw = (sx * (i64::from(px) - fx)).clamp(1, max_w);
        let ph = (sy * (i64::from(py) - fy)).clamp(1, max_h);
        let mut w = pw;
        let mut h = w * ah / aw;
        if h > ph {
            h = ph;
            w = h * aw / ah;
        }
        w = w.clamp(1, max_w);
        h = (w * ah / aw).clamp(1, max_h);
        w = (h * aw / ah).clamp(1, max_w);
        let min_w = if img_w >= 8 { 8 } else { 1 };
        let min_h = if img_h >= 8 { 8 } else { 1 };
        if w < i64::from(min_w) || h < i64::from(min_h) {
            w = i64::from(min_w).min(max_w);
            h = (w * ah / aw).clamp(1, max_h);
            if h < i64::from(min_h) {
                h = i64::from(min_h).min(max_h);
                w = (h * aw / ah).clamp(1, max_w);
            }
        }
        let (x, y) = match corner {
            CropCorner::Se => (fx, fy),
            CropCorner::Nw => (fx - w, fy - h),
            CropCorner::Ne => (fx, fy - h),
            CropCorner::Sw => (fx - w, fy),
        };
        Self { x: x.max(0) as u32, y: y.max(0) as u32, w: w as u32, h: h as u32 }.sanitized(img_w, img_h)
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
        if !self.place.is_finite() || self.place.abs() > crate::tone::PLACE_LIMIT {
            return Err(Error::Invalid(format!(
                "place {} not within ±{}",
                self.place,
                crate::tone::PLACE_LIMIT
            )));
        }
        self.curve.validate()?;
        if !(self.dpi.is_finite() && self.dpi > 0.0) {
            return Err(Error::Invalid("dpi must be positive".into()));
        }
        if !(self.border_mm.is_finite() && self.border_mm >= 0.0) {
            return Err(Error::Invalid("border must be zero or positive".into()));
        }
        if let Some(c) = self.crop {
            if c.w == 0 || c.h == 0 {
                return Err(Error::Invalid("crop must cover at least one pixel".into()));
            }
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_round_trips_and_is_small() {
        let r = Recipe::default();
        r.validate().unwrap();
        let json = serde_json::to_string(&r).unwrap();
        // Zero place, no size, no border are omitted.
        assert!(!json.contains("place") && !json.contains("border"));
        assert!(json.contains(r#""resample":"b-spline""#), "{json}");
        assert!(json.contains(r#""sharpen":{"mode":"off"}"#), "{json}");
        let back: Recipe = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn full_recipe_round_trips() {
        let r = Recipe {
            place: -0.5,
            curve: Tone::Paper(crate::paper::SplitGrade::new(0.25, -0.5)),
            resample: ResampleKernel::BSpline,
            size: Some(PrintSize::Physical { width_in: 11.811, height_in: 15.748 }),
            dpi: 360.0,
            border_mm: 20.0,
            sharpen: Sharpen::Deconv { passes: 5, usm_amount: 0.5 },
            ..Recipe::default()
        };
        r.validate().unwrap();
        let json = serde_json::to_string_pretty(&r).unwrap();
        assert!(json.contains(r#""paper": "multigrade""#), "{json}");
        let back: Recipe = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        assert_eq!(r.border_px(), 283);
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
        let bad = Recipe { sharpen: Sharpen::Usm { amount: 2.0 }, ..Recipe::default() };
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

    #[test]
    fn crop_turns_and_fits() {
        let r = CropRect { x: 10, y: 20, w: 30, h: 40 };
        let back = r.turned(4, 100, 80);
        assert_eq!(back, r.sanitized(100, 80));
        let once = r.rotate_cw(100, 80);
        assert_eq!(once, CropRect { x: 20, y: 10, w: 40, h: 30 });
        assert_eq!(once.turned(3, 80, 100), r);

        // 8×10 in a 100×80 landscape: long side across, so 10:8, width-limited.
        let fit = CropRect::inscribed(100, 80, 10, 8);
        assert_eq!(fit.h, 80);
        assert_eq!(fit.w, 100);
        assert_eq!(fit.x, 0);

        // 8×10 on a square, long side across: 10 wide, 8 tall.
        let sq = CropRect::inscribed(100, 100, 10, 8);
        assert_eq!((sq.w, sq.h), (100, 80));
        assert_eq!(sq.y, 10);

        let square = CropRect::inscribed(90, 40, 1, 1);
        assert_eq!((square.w, square.h), (40, 40));

        let moved = fit.shift(5, -3, 100, 80);
        assert_eq!(moved.y, 0);
        assert!(moved.x <= 100 - moved.w);

        let shrunk = sq.scale_corner(CropCorner::Se, (sq.x as i32 + 50, sq.y as i32 + 80), 100, 100, (10, 8));
        assert_eq!(shrunk.x, sq.x);
        assert_eq!(shrunk.y, sq.y);
        assert!(shrunk.w < sq.w);
        assert_eq!(shrunk.w * 8, shrunk.h * 10);
    }
}
