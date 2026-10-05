//! Calibration files: `cameras/*.json` and `look/*.json` (SPEC section 10).

use crate::cfa::BayerPhase;
use crate::error::{Error, Result};
use crate::ingest::NoiseModel;
use crate::mix::{WeightSpace, Weights};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Fitted {
    pub noise: bool,
    pub weights: bool,
}

/// Measured star MTF of a negative (`calibrate star`), c/px.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct StarCalib {
    pub mtf50_axis: Option<f64>,
    pub mtf50_diag: Option<f64>,
    pub mtf10_axis: Option<f64>,
    pub mtf10_diag: Option<f64>,
    pub ring: bool,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct CameraFile {
    pub schema: u32,
    pub make: String,
    pub model: String,
    pub bayer_phase: String,
    pub noise_a: f64,
    pub noise_b: f64,
    pub weights_rgb: [f64; 3],
    /// Channels `weights_rgb` refers to. `raw` (what `calibrate weights`
    /// writes) is illuminant-independent and converted per image with the
    /// balance multipliers; `balanced` (default, older files) is used as is.
    #[serde(default)]
    pub weights_space: WeightSpace,
    pub usm_amount: f64,
    pub matrix_fallback: bool,
    pub fitted: Fitted,
    /// Provenance of fitted weights (reference file), optional.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weights_fitted_against: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub star: Option<StarCalib>,
}

impl CameraFile {
    /// Defaults for a camera without a file.
    pub fn defaults(make: &str, model: &str, phase: BayerPhase) -> Self {
        Self {
            schema: 1,
            make: make.to_string(),
            model: model.to_string(),
            bayer_phase: phase.name().to_string(),
            noise_a: 0.005,
            noise_b: 0.0002,
            weights_rgb: [0.25, 0.5, 0.25],
            weights_space: WeightSpace::Balanced,
            usm_amount: 0.0,
            matrix_fallback: false,
            fitted: Fitted { noise: false, weights: false },
            weights_fitted_against: None,
            star: None,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        let c: CameraFile = serde_json::from_str(&s)?;
        c.validate()?;
        Ok(c)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != 1 {
            return Err(Error::Invalid(format!("camera file schema {} not supported", self.schema)));
        }
        BayerPhase::from_name(&self.bayer_phase)?;
        self.weights().validate()?;
        if !(0.0..=1.5).contains(&self.usm_amount) {
            return Err(Error::Invalid("usm_amount must be within 0..1.5".into()));
        }
        Ok(())
    }

    /// The stored triple, in `weights_space`.
    pub fn weights(&self) -> Weights {
        Weights { r: self.weights_rgb[0], g: self.weights_rgb[1], b: self.weights_rgb[2] }
    }

    /// Weights for the mix of an image balanced with `wb` (G = 1).
    pub fn weights_balanced(&self, wb: [f64; 3]) -> Weights {
        self.weights().to_balanced(self.weights_space, wb)
    }

    pub fn noise(&self) -> NoiseModel {
        NoiseModel { a: self.noise_a, b: self.noise_b }
    }

    pub fn phase(&self) -> BayerPhase {
        BayerPhase::from_name(&self.bayer_phase).expect("validated")
    }

    /// `cameras/<clean_make>_<clean_model>.json`, lower-case, spaces to `_`.
    pub fn file_name(clean_make: &str, clean_model: &str) -> String {
        let norm = |s: &str| s.trim().to_ascii_lowercase().replace(' ', "_");
        format!("{}_{}.json", norm(clean_make), norm(clean_model))
    }

    pub fn find(dir: &Path, clean_make: &str, clean_model: &str) -> Option<PathBuf> {
        let p = resolve_dir(dir).join(Self::file_name(clean_make, clean_model));
        p.is_file().then_some(p)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

/// A resource directory such as `cameras` or `look`: the path as given when it
/// exists, otherwise the same name next to the executable, in
/// `../share/mimizan/` (Debian package) or two levels up (`target/release`).
pub fn resolve_dir(dir: &Path) -> PathBuf {
    if dir.is_dir() || dir.is_absolute() {
        return dir.to_path_buf();
    }
    let Some(name) = dir.file_name() else { return dir.to_path_buf() };
    if let Ok(exe) = std::env::current_exe() {
        if let Some(bin) = exe.parent() {
            for base in [bin.to_path_buf(), bin.join("../share/mimizan"), bin.join("../..")] {
                let p = base.join(name);
                if p.is_dir() {
                    return p;
                }
            }
        }
    }
    dir.to_path_buf()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LookEncoding {
    /// Linear -> x^(1/2.2) -> points.
    Gamma22,
    /// Points applied to linear directly.
    None,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct LookFile {
    pub schema: u32,
    pub name: String,
    pub points: Vec<[f64; 2]>,
    pub encoding: LookEncoding,
    pub fitted_against: Option<String>,
}

impl LookFile {
    pub fn neutral() -> Self {
        Self {
            schema: 1,
            name: "neutral".into(),
            points: vec![[0.0, 0.0], [1.0, 1.0]],
            encoding: LookEncoding::Gamma22,
            fitted_against: None,
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let s = std::fs::read_to_string(path)?;
        let l: LookFile = serde_json::from_str(&s)?;
        l.validate()?;
        Ok(l)
    }

    pub fn validate(&self) -> Result<()> {
        if self.schema != 1 {
            return Err(Error::Invalid(format!("look schema {} not supported", self.schema)));
        }
        if self.points.len() < 2 {
            return Err(Error::Invalid("look needs at least two points".into()));
        }
        for w in self.points.windows(2) {
            if w[1][0] <= w[0][0] || w[1][1] < w[0][1] {
                return Err(Error::Invalid(
                    "look points must be strictly increasing in x and monotone in y".into(),
                ));
            }
        }
        for p in &self.points {
            if !(0.0..=1.0).contains(&p[0]) || !(0.0..=1.0).contains(&p[1]) {
                return Err(Error::Invalid("look points must lie in [0,1]".into()));
            }
        }
        Ok(())
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        std::fs::write(path, serde_json::to_string_pretty(self)?)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn camera_file_name() {
        assert_eq!(CameraFile::file_name("Nikon", "Z f"), "nikon_z_f.json");
        assert_eq!(CameraFile::file_name(" Acme ", "Mono Camera (Mk II)"), "acme_mono_camera_(mk_ii).json");
    }

    #[test]
    fn neutral_look_is_valid() {
        LookFile::neutral().validate().unwrap();
    }
}
