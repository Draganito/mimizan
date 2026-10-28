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
use crate::mix::Weights;
use crate::plate::{PlateFilters, PlateParams, PlateSpace};
use crate::print::{PrintSize, LAMP_LIMIT};
use crate::resize::ResampleKernel;
use crate::tone::Tone;
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Largest USM amount a recipe accepts (the GUI slider ends here too).
pub const USM_MAX: f64 = 1.5;

/// Gaussian σ of the peak high-pass, in pixels of the negative. The search in
/// [`crate::peak`] starts at the same radius.
pub const PEAK_RADIUS_DEFAULT: f64 = 0.7;
/// Finest and widest radius the print panel will store.
pub const PEAK_RADIUS_MIN: f64 = 0.3;
pub const PEAK_RADIUS_MAX: f64 = 8.0;
/// Largest peak amount. The search itself stops near 1.5; the field goes
/// further so a measured value can be pushed by hand.
pub const PEAK_AMOUNT_MAX: f64 = 4.0;

fn default_peak_radius() -> f64 {
    PEAK_RADIUS_DEFAULT
}

/// Peak high-pass on the picture. The paper border is not part of it.
/// `on` is the checkbox: radius and amount stay when it is off, and neither
/// the tile nor the TIFF is sharpened.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct Peak {
    #[serde(default)]
    pub on: bool,
    /// Gaussian σ in pixels of the negative. The print multiplies it by the
    /// long-side scale, above 1 and below.
    #[serde(default = "default_peak_radius")]
    pub radius: f64,
    /// `x + amount · (x − blur(radius))`.
    #[serde(default)]
    pub amount: f64,
}

impl Default for Peak {
    fn default() -> Self {
        Self { on: false, radius: default_peak_radius(), amount: 0.0 }
    }
}

impl Peak {
    /// The high-pass runs. Off, or a zero amount, leaves every pixel as it was.
    pub fn applies(&self) -> bool {
        self.on && self.radius > 0.0 && self.amount > 0.0
    }

    /// Omitted from a recipe file. A saved radius or a ticked checkbox is kept.
    pub fn is_unset(&self) -> bool {
        !self.on && self.amount == 0.0 && self.radius == PEAK_RADIUS_DEFAULT
    }

    /// A measured pair, pulled into the range [`Self::validate`] accepts.
    pub fn clamp_measured(radius: f64, amount: f64) -> (f64, f64) {
        let radius = if radius.is_finite() { radius } else { PEAK_RADIUS_DEFAULT };
        let amount = if amount.is_finite() { amount } else { 0.0 };
        (radius.clamp(PEAK_RADIUS_MIN, PEAK_RADIUS_MAX), amount.clamp(0.0, PEAK_AMOUNT_MAX))
    }

    pub fn validate(&self) -> Result<()> {
        if !self.radius.is_finite() || !(PEAK_RADIUS_MIN..=PEAK_RADIUS_MAX).contains(&self.radius) {
            return Err(Error::Invalid(format!(
                "peak radius {} not within {PEAK_RADIUS_MIN}..{PEAK_RADIUS_MAX}",
                self.radius
            )));
        }
        if !self.amount.is_finite() || !(0.0..=PEAK_AMOUNT_MAX).contains(&self.amount) {
            return Err(Error::Invalid(format!(
                "peak amount {} not within 0..{PEAK_AMOUNT_MAX}",
                self.amount
            )));
        }
        Ok(())
    }
}

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

/// Highest dpi a recipe accepts.
pub const DPI_MAX: f64 = 10_000.0;
/// Widest white border or black edge, mm (one metre).
pub const BORDER_MAX_MM: f64 = 1_000.0;
/// Most samples a sheet may have: 2^30, eight gigabytes of f64. A recipe
/// that asks for more is a typo, and the allocation would abort the process
/// instead of failing.
pub const SHEET_MAX_PX: usize = 1 << 30;

fn default_dpi() -> f64 {
    300.0
}

fn default_schema() -> u32 {
    SCHEMA
}

fn is_zero(v: &f64) -> bool {
    *v == 0.0
}

/// Every decision of the print pass. See the module doc.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Recipe {
    /// [`SCHEMA`]; anything else fails validation.
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
    /// Pixels per inch: converts `size`, `border_mm` and `black_mm`, and is
    /// written into the file.
    #[serde(default = "default_dpi")]
    pub dpi: f64,
    /// White paper around the fitted picture, mm (converted with `dpi`),
    /// filled with paper white, the curve's value at linear 1.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub border_mm: f64,
    /// Black film edge around the picture, mm, inside the white border
    /// when there is one. Each ring stands on its own: black alone, white
    /// alone, or black inside white. Old recipes omit it.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub black_mm: f64,
    /// Scale compensation after the curve.
    #[serde(default)]
    pub sharpen: Sharpen,
    /// Peak high-pass. Absent in older recipes, which stay unsharpened.
    #[serde(default, skip_serializing_if = "Peak::is_unset")]
    pub peak: Peak,
    /// Window on the upright negative, whole pixels, before the size step.
    /// `None` is the full frame. Old recipes omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub crop: Option<CropRect>,
    /// The monochrome mix on the balanced channels, for a darkroom that
    /// develops from the RAW. `None` is the camera's default. A print of
    /// an already mixed negative ignores it. Old recipes omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mix: Option<Weights>,
    /// The three-colour plate: taking filters, primaries and the two lamps.
    /// `None` is the monochrome print. Old recipes omit it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plate: Option<PlateRecipe>,
}

/// The plate as the recipe carries it: which process, and its two lamps.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlateRecipe {
    /// The taking filters.
    #[serde(default)]
    pub filters: PlateFilters,
    /// The projection primaries, and the profile of the file.
    #[serde(default)]
    pub space: PlateSpace,
    /// The red and the blue lamp, stops added to that positive; green is
    /// the reference.
    #[serde(default, skip_serializing_if = "lamps_off")]
    pub lamps: [f64; 2],
}

fn lamps_off(l: &[f64; 2]) -> bool {
    *l == [0.0; 2]
}

impl PlateRecipe {
    /// The filters and the primaries, as `negative --plate` takes them.
    pub fn params(&self) -> PlateParams {
        PlateParams { filters: self.filters, space: self.space }
    }

    /// The three lamps the print takes: red, green (zero), blue.
    pub fn lamps_rgb(&self) -> [f64; 3] {
        [self.lamps[0], 0.0, self.lamps[1]]
    }

    fn validate(&self) -> Result<()> {
        if self.lamps.iter().any(|l| !l.is_finite() || l.abs() > LAMP_LIMIT) {
            return Err(Error::Invalid(format!("lamps {:?} not within ±{LAMP_LIMIT} stops", self.lamps)));
        }
        Ok(())
    }
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
            black_mm: 0.0,
            sharpen: Sharpen::Off,
            peak: Peak::default(),
            crop: None,
            mix: None,
            plate: None,
        }
    }
}

/// Which corner of a crop window is being dragged.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CropCorner {
    /// Top left.
    Nw,
    /// Top right.
    Ne,
    /// Bottom right.
    Se,
    /// Bottom left.
    Sw,
}

/// A window on the upright negative, in whole pixels. The origin is the
/// top-left. Nothing here resamples: the size step is the only resize.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct CropRect {
    /// Left edge, pixels from the left of the upright negative.
    pub x: u32,
    /// Top edge, pixels from the top.
    pub y: u32,
    /// Width, pixels, at least 1 once sanitized.
    pub w: u32,
    /// Height, pixels, at least 1 once sanitized.
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

    /// Move, keeping the size, staying inside the image. A window larger
    /// than the image is first pulled back inside it.
    pub fn shift(self, dx: i32, dy: i32, img_w: u32, img_h: u32) -> Self {
        let s = self.sanitized(img_w, img_h);
        let max_x = i64::from(img_w).saturating_sub(i64::from(s.w)).max(0);
        let max_y = i64::from(img_h).saturating_sub(i64::from(s.h)).max(0);
        let x = (i64::from(s.x) + i64::from(dx)).clamp(0, max_x);
        let y = (i64::from(s.y) + i64::from(dy)).clamp(0, max_y);
        Self { x: x as u32, y: y as u32, ..s }
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

    /// Every field within its range, and the sheet the recipe asks for
    /// within [`SHEET_MAX_PX`]. A recipe that fails here is refused before
    /// a pixel is touched.
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
        if !(self.dpi.is_finite() && self.dpi > 0.0 && self.dpi <= DPI_MAX) {
            return Err(Error::Invalid(format!("dpi {} not within 0..{DPI_MAX}", self.dpi)));
        }
        let mm = |v: f64| v.is_finite() && (0.0..=BORDER_MAX_MM).contains(&v);
        if !mm(self.border_mm) {
            return Err(Error::Invalid(format!(
                "border {} mm not within 0..{BORDER_MAX_MM}",
                self.border_mm
            )));
        }
        if !mm(self.black_mm) {
            return Err(Error::Invalid(format!(
                "black edge {} mm not within 0..{BORDER_MAX_MM}",
                self.black_mm
            )));
        }
        let frame2 = self.frame_px().saturating_mul(2);
        if frame2.saturating_mul(frame2) > SHEET_MAX_PX {
            return Err(Error::Invalid(format!(
                "border and black edge at {} dpi exceed the sheet limit",
                self.dpi
            )));
        }
        if let Some(size) = self.size {
            size.validate()?;
            let (w, h) = size.pixels(self.dpi);
            if w.checked_mul(h).is_none_or(|n| n > SHEET_MAX_PX) {
                return Err(Error::Invalid(format!(
                    "size {w}x{h} px at {} dpi exceeds the sheet limit of {SHEET_MAX_PX} samples",
                    self.dpi
                )));
            }
        }
        if let Some(c) = self.crop {
            if c.w == 0 || c.h == 0 {
                return Err(Error::Invalid("crop must cover at least one pixel".into()));
            }
        }
        if let Some(m) = self.mix {
            let w = [m.r, m.g, m.b];
            if w.iter().any(|v| !v.is_finite() || *v < 0.0) || (w.iter().sum::<f64>() - 1.0).abs() > 1e-6 {
                return Err(Error::Invalid(format!(
                    "mix R {} G {} B {} must be non-negative and sum to one",
                    m.r, m.g, m.b
                )));
            }
        }
        if let Some(p) = &self.plate {
            p.validate()?;
        }
        self.sharpen.validate()?;
        self.peak.validate()
    }

    /// Read and validate a recipe file.
    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        let r: Recipe = serde_json::from_str(&s)?;
        r.validate()?;
        Ok(r)
    }

    /// Write the recipe as pretty JSON.
    pub fn save(&self, path: &Path) -> Result<()> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        std::fs::write(path, json)?;
        Ok(())
    }

    /// Millimetres at this recipe's dpi, rounded to whole pixels.
    fn mm_px(&self, mm: f64) -> usize {
        if mm <= 0.0 || !mm.is_finite() || !(self.dpi.is_finite() && self.dpi > 0.0) {
            0
        } else {
            (mm / 25.4 * self.dpi).round() as usize
        }
    }

    /// White border in pixels on each side.
    pub fn border_px(&self) -> usize {
        self.mm_px(self.border_mm)
    }

    /// Black edge in pixels on each side.
    pub fn black_px(&self) -> usize {
        self.mm_px(self.black_mm)
    }

    /// Both rings, pixels on each side. The picture sits inside this.
    pub fn frame_px(&self) -> usize {
        self.black_px() + self.border_px()
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
        // Zero place, no size, no border, no black edge are omitted.
        assert!(!json.contains("place") && !json.contains("border") && !json.contains("black"));
        assert!(json.contains(r#""resample":"b-spline""#), "{json}");
        assert!(json.contains(r#""sharpen":{"mode":"off"}"#), "{json}");
        assert!(!json.contains("peak"), "{json}");
        let back: Recipe = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
    }

    #[test]
    fn plate_and_mix_round_trip_and_are_absent_by_default() {
        let json = serde_json::to_string(&Recipe::default()).unwrap();
        assert!(!json.contains("plate") && !json.contains("mix"), "{json}");
        let r = Recipe {
            mix: Some(Weights { r: 0.3, g: 0.5, b: 0.2 }),
            plate: Some(PlateRecipe {
                filters: PlateFilters::Wratten,
                space: PlateSpace::Srgb,
                lamps: [0.1, -0.2],
            }),
            ..Recipe::default()
        };
        r.validate().unwrap();
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains(r#""filters":"wratten""#) && json.contains(r#""space":"srgb""#), "{json}");
        let back: Recipe = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        assert_eq!(back.plate.unwrap().lamps_rgb(), [0.1, 0.0, -0.2]);
        // Zero lamps are left out; an older file without them reads as zero.
        let quiet = Recipe { plate: Some(PlateRecipe { lamps: [0.0; 2], ..r.plate.unwrap() }), ..r.clone() };
        assert!(!serde_json::to_string(&quiet).unwrap().contains("lamps"));
        let bare = format!(r#"{{"curve":{},"plate":{{}}}}"#, serde_json::to_string(&Tone::paper()).unwrap());
        let old: Recipe = serde_json::from_str(&bare).unwrap();
        assert_eq!(
            old.plate,
            Some(PlateRecipe { filters: PlateFilters::Camera, space: PlateSpace::AdobeRgb, lamps: [0.0; 2] })
        );
        // Out of range is refused.
        let hot = Recipe { plate: Some(PlateRecipe { lamps: [3.0, 0.0], ..r.plate.unwrap() }), ..r.clone() };
        assert!(hot.validate().is_err());
        let off = Recipe { mix: Some(Weights { r: 0.5, g: 0.5, b: 0.5 }), ..Recipe::default() };
        assert!(off.validate().is_err());
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
            black_mm: 8.0,
            sharpen: Sharpen::Deconv { passes: 5, usm_amount: 0.5 },
            ..Recipe::default()
        };
        r.validate().unwrap();
        let json = serde_json::to_string_pretty(&r).unwrap();
        assert!(json.contains(r#""paper": "multigrade""#), "{json}");
        let back: Recipe = serde_json::from_str(&json).unwrap();
        assert_eq!(back, r);
        assert_eq!(r.border_px(), 283);
        assert_eq!(r.black_px(), 113);
        assert_eq!(r.frame_px(), 396);
        // Each ring on its own: black may be wider than white, or stand alone.
        let wider = Recipe { black_mm: 40.0, ..r.clone() };
        assert_eq!(wider.black_px(), 567);
        let alone = Recipe { border_mm: 0.0, black_mm: 8.0, ..r.clone() };
        assert_eq!((alone.border_px(), alone.black_px(), alone.frame_px()), (0, 113, 113));
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
        let bad = Recipe { black_mm: -1.0, ..Recipe::default() };
        assert!(bad.validate().is_err());
        let bad = Recipe { schema: 2, ..Recipe::default() };
        assert!(bad.validate().is_err());
        let bad = Recipe { dpi: 1e7, ..Recipe::default() };
        assert!(bad.validate().is_err());
        let bad = Recipe { border_mm: 1e6, ..Recipe::default() };
        assert!(bad.validate().is_err());
        let bad = Recipe { border_mm: 1000.0, dpi: 10_000.0, ..Recipe::default() };
        assert!(bad.validate().is_err(), "a metre of border at 10 000 dpi is 400 000 px a side");
        let bad = Recipe {
            size: Some(PrintSize::Physical { width_in: f64::NAN, height_in: 1.0 }),
            ..Recipe::default()
        };
        assert!(bad.validate().is_err());
        let bad = Recipe {
            size: Some(PrintSize::Physical { width_in: 300.0, height_in: 300.0 }),
            dpi: 1000.0,
            ..Recipe::default()
        };
        assert!(bad.validate().is_err(), "90 gigapixels");
        let big_but_fine = Recipe {
            size: Some(PrintSize::Physical { width_in: 80.0, height_in: 60.0 }),
            dpi: 360.0,
            ..Recipe::default()
        };
        big_but_fine.validate().unwrap();
        let bad = Recipe { curve: Tone::Paper(crate::paper::SplitGrade::new(0.0, 9.0)), ..Recipe::default() };
        assert!(bad.validate().is_err());
        Recipe::on_paper().validate().unwrap();
        let peak_on = Recipe { peak: Peak { on: true, radius: 1.4, amount: 0.35 }, ..Recipe::default() };
        peak_on.validate().unwrap();
        let json = serde_json::to_string(&peak_on).unwrap();
        let back: Recipe = serde_json::from_str(&json).unwrap();
        assert_eq!(back.peak, peak_on.peak, "{json}");
        let off = Recipe { peak: Peak { on: false, radius: 1.4, amount: 0.35 }, ..Recipe::default() };
        let kept: Recipe = serde_json::from_str(&serde_json::to_string(&off).unwrap()).unwrap();
        assert!(!kept.peak.on);
        assert!((kept.peak.radius - 1.4).abs() < 1e-12);
        let bad = Recipe { peak: Peak { on: true, radius: 0.1, amount: 0.2 }, ..Recipe::default() };
        assert!(bad.validate().is_err());
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
        // A stale window larger than the image is pulled inside, not a panic.
        let stale = CropRect { x: 0, y: 0, w: 300, h: 300 }.shift(1, 1, 100, 80);
        assert_eq!(stale, CropRect { x: 0, y: 0, w: 100, h: 80 });

        let shrunk = sq.scale_corner(CropCorner::Se, (sq.x as i32 + 50, sq.y as i32 + 80), 100, 100, (10, 8));
        assert_eq!(shrunk.x, sq.x);
        assert_eq!(shrunk.y, sq.y);
        assert!(shrunk.w < sq.w);
        assert_eq!(shrunk.w * 8, shrunk.h * 10);
    }
}
