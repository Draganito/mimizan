//! Bayer phase and the carrier sign table from SPEC section 4.

use crate::error::{Error, Result};
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Color {
    R,
    G,
    B,
}

/// Colour at (0,0),(1,0),(0,1),(1,1) of the repeating 2x2 tile, relative to the
/// full sensor frame (before any crop).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[allow(clippy::upper_case_acronyms)]
pub enum BayerPhase {
    RGGB,
    BGGR,
    GRBG,
    GBRG,
}

/// `m = L + eps*sx*sy*C1 + C2*(a*sx + b*sy)`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Carriers {
    pub eps: f64,
    pub a: f64,
    pub b: f64,
}

impl BayerPhase {
    pub const ALL: [BayerPhase; 4] = [Self::RGGB, Self::BGGR, Self::GRBG, Self::GBRG];

    pub fn from_name(name: &str) -> Result<Self> {
        match name.trim().to_ascii_uppercase().as_str() {
            "RGGB" => Ok(Self::RGGB),
            "BGGR" => Ok(Self::BGGR),
            "GRBG" => Ok(Self::GRBG),
            "GBRG" => Ok(Self::GBRG),
            other => Err(Error::Unsupported(format!(
                "CFA pattern '{other}' is not a 2x2 Bayer pattern (X-Trans, Quad-Bayer, Foveon are out of scope)"
            ))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::RGGB => "RGGB",
            Self::BGGR => "BGGR",
            Self::GRBG => "GRBG",
            Self::GBRG => "GBRG",
        }
    }

    /// Tile as [ (0,0), (1,0), (0,1), (1,1) ] i.e. row-major over the 2x2 cell.
    pub fn tile(self) -> [Color; 4] {
        use Color::*;
        match self {
            Self::RGGB => [R, G, G, B],
            Self::BGGR => [B, G, G, R],
            Self::GRBG => [G, R, B, G],
            Self::GBRG => [G, B, R, G],
        }
    }

    #[inline]
    pub fn color_at(self, x: usize, y: usize) -> Color {
        self.tile()[(y & 1) * 2 + (x & 1)]
    }

    /// Sign table from SPEC section 4.
    pub fn carriers(self) -> Carriers {
        match self {
            Self::RGGB => Carriers { eps: -1.0, a: 1.0, b: 1.0 },
            Self::BGGR => Carriers { eps: -1.0, a: -1.0, b: -1.0 },
            Self::GRBG => Carriers { eps: 1.0, a: -1.0, b: 1.0 },
            Self::GBRG => Carriers { eps: 1.0, a: 1.0, b: -1.0 },
        }
    }

    /// Phase seen from an origin shifted by (dx, dy) pixels.
    pub fn shifted(self, dx: usize, dy: usize) -> Self {
        let t = self.tile();
        let pick = |x: usize, y: usize| t[((y + dy) & 1) * 2 + ((x + dx) & 1)];
        let cell = [pick(0, 0), pick(1, 0), pick(0, 1), pick(1, 1)];
        Self::ALL
            .into_iter()
            .find(|p| p.tile() == cell)
            .expect("every 2x2 shift of a Bayer tile is a Bayer tile")
    }
}

impl fmt::Display for BayerPhase {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Brute-force the decomposition against the sign table for every phase.
    #[test]
    fn sign_table_matches_tile() {
        let (r, g, b) = (0.7, 0.4, 0.2);
        let l = (r + 2.0 * g + b) / 4.0;
        let c1 = (-r + 2.0 * g - b) / 4.0;
        let c2 = (r - b) / 4.0;
        for phase in BayerPhase::ALL {
            let c = phase.carriers();
            for y in 0..4usize {
                for x in 0..4usize {
                    let sx = if x % 2 == 0 { 1.0 } else { -1.0 };
                    let sy = if y % 2 == 0 { 1.0 } else { -1.0 };
                    let model = l + c.eps * sx * sy * c1 + c2 * (c.a * sx + c.b * sy);
                    let actual = match phase.color_at(x, y) {
                        Color::R => r,
                        Color::G => g,
                        Color::B => b,
                    };
                    assert!((model - actual).abs() < 1e-12, "{phase} at ({x},{y}): {model} vs {actual}");
                }
            }
        }
    }

    #[test]
    fn shift_round_trip() {
        for p in BayerPhase::ALL {
            assert_eq!(p.shifted(0, 0), p);
            assert_eq!(p.shifted(2, 2), p);
            assert_eq!(p.shifted(1, 0).shifted(1, 0), p);
            assert_eq!(p.shifted(1, 1).color_at(0, 0), p.color_at(1, 1));
        }
    }
}
