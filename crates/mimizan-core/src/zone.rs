//! The zone system on the scene side (Zonensystem paper, sections 3–6).
//!
//! A zone is a stop of scene luminance: Zone V is the grey card at 0.18
//! linear, `Z_k = 0.18 · 2^(k−5)`, and zone k is the interval of a stop
//! around it. This module only names and locates the zones in the negative;
//! where they land on the print is the paper's business (`paper.rs`), and
//! the bar in the GUI is that answer drawn as a strip of print tones.

/// Adams' zones 0 … X.
pub const ZONES: usize = 11;
/// Zone V, the grey card, linear.
pub const GRAY_CARD: f64 = 0.18;
/// Encoding exponent of print values.
pub const GAMMA: f64 = 2.2;

/// Roman numeral of zone `k` (0 … X).
pub fn numeral(k: usize) -> &'static str {
    const N: [&str; ZONES] = ["0", "I", "II", "III", "IV", "V", "VI", "VII", "VIII", "IX", "X"];
    N[k.min(ZONES - 1)]
}

/// Linear scene value at the centre of zone `k`: `0.18 · 2^(k−5)`. Zones
/// VIII and above lie over the clip (> 1).
pub fn linear(k: usize) -> f64 {
    GRAY_CARD * 2f64.powi(k as i32 - 5)
}

/// Stops around the grey card, `e = log2(max(L, ε)) − log2(0.18)` (paper,
/// equation 3). The pipeline works in linear values; this is the reading
/// the tests check the zone table against.
#[cfg(test)]
pub fn stops(l: f64) -> f64 {
    (l.max(1e-6)).log2() - GRAY_CARD.log2()
}

/// Linear scene value at the boundary between zone `k` and `k + 1`:
/// `0.18 · 2^(k + 0.5 − 5)`.
pub fn boundary_linear(k: usize) -> f64 {
    GRAY_CARD * 2f64.powf(k as f64 + 0.5 - 5.0)
}

/// Zone boundaries that lie under the clip (linear < 1): `k` for the
/// boundary k | k+1, 0|I … VI|VII. VII|VIII at 1.018 is over the clip, so
/// zone VII runs up to the clip and zone 0 down to black.
pub fn boundaries() -> Vec<usize> {
    (0..ZONES - 1).filter(|&k| boundary_linear(k) < 1.0).collect()
}

/// Zones the bar shows: 0 … the last one with a boundary below it, plus
/// the open-ended top zone (VII).
pub fn bar_zones() -> usize {
    boundaries().len() + 1
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zone_table() {
        assert!((linear(5) - 0.18).abs() < 1e-15);
        assert!((linear(4) - 0.09).abs() < 1e-15);
        assert!((linear(7) - 0.72).abs() < 1e-15);
        assert!(linear(8) > 1.0);
        assert_eq!(boundaries(), vec![0, 1, 2, 3, 4, 5, 6]);
        assert_eq!(bar_zones(), 8);
        assert!((boundary_linear(4) - 0.18 / 2f64.sqrt()).abs() < 1e-15);
        assert!((boundary_linear(5) - 0.18 * 2f64.sqrt()).abs() < 1e-15);
        assert!(boundary_linear(7) > 1.0);
        assert_eq!(numeral(5), "V");
        assert_eq!(numeral(10), "X");
        assert!((stops(0.18)).abs() < 1e-12);
        assert!((stops(0.36) - 1.0).abs() < 1e-12);
        assert!((stops(0.045) + 2.0).abs() < 1e-12);
    }
}
