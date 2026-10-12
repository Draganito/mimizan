//! The zone system as the handle on the print curve (Zonensystem paper,
//! sections 3–6, 13).
//!
//! A zone is a pair of scene value and print value. Zone V is the grey card
//! at 0.18 linear, one zone is one stop: `Z_k = 0.18 · 2^(k−5)`. The curve
//! stays the PCHIP of `curve.rs`, applied after the 1/2.2 encoding; the zone
//! nodes are the encoded zone centres that fall into (0, 1), with output =
//! input in the untouched state, so the untouched bar is bit-identical to
//! the `neutral` look. Nothing here is a second mapping: a zone look is a
//! [`LookFile`] like any other, named `zones`.

use crate::calib::{LookEncoding, LookFile};

/// Adams' zones 0 … X.
pub const ZONES: usize = 11;
/// Zone V, the grey card, linear.
pub const GRAY_CARD: f64 = 0.18;
/// Encoding exponent of the curve domain (SPEC section 9).
pub const GAMMA: f64 = 2.2;
/// Minimum distance between neighbouring output nodes, so the PCHIP never
/// sees a duplicate and a darker zone cannot overtake the next.
pub const MIN_GAP: f64 = 1e-4;
/// Name of the look a zone curve carries.
pub const LOOK_NAME: &str = "zones";

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

/// Encoded value `x^(1/2.2)` of a linear value.
pub fn encode(x: f64) -> f64 {
    x.clamp(0.0, 1.0).powf(1.0 / GAMMA)
}

/// Linear value of an encoded one.
pub fn decode(v: f64) -> f64 {
    v.clamp(0.0, 1.0).powf(GAMMA)
}

/// Bar coordinate of a linear value: stops around the grey card,
/// `e = log2(max(L, ε)) − log2(0.18)` (paper, equation 3).
pub fn stops(l: f64) -> f64 {
    (l.max(1e-6)).log2() - GRAY_CARD.log2()
}

/// Zone whose stop interval `[k − 0.5, k + 0.5)` holds the linear value, as
/// a real number clamped to 0 … X (for hover tinting and the bar).
pub fn zone_of(l: f64) -> f64 {
    (stops(l) + 5.0).clamp(0.0, (ZONES - 1) as f64)
}

/// Zones whose encoded centre lies in the open interval (0, 1): the inner
/// nodes of the curve. Zones 0 … VII; VIII and above are over the clip.
pub fn inner_zones() -> Vec<usize> {
    (0..ZONES).filter(|&k| linear(k) < 1.0).collect()
}

/// Encoded zone centres that are inner nodes.
pub fn default_nodes() -> Vec<f64> {
    inner_zones().into_iter().map(|k| encode(linear(k))).collect()
}

/// The untouched zone look: inner nodes with output = input, end points
/// (0, 0) and (1, 1). Applying it equals the `neutral` look bit for bit.
pub fn default_look() -> LookFile {
    let mut points = vec![[0.0, 0.0]];
    points.extend(default_nodes().into_iter().map(|u| [u, u]));
    points.push([1.0, 1.0]);
    LookFile {
        schema: 1,
        name: LOOK_NAME.into(),
        points,
        encoding: LookEncoding::Gamma22,
        fitted_against: None,
    }
}

/// Is this look a zone curve (named so, with the default input nodes)?
pub fn is_zone_look(look: &LookFile) -> bool {
    if look.name != LOOK_NAME || look.encoding != LookEncoding::Gamma22 {
        return false;
    }
    let nodes = default_nodes();
    look.points.len() == nodes.len() + 2
        && look.points[0][0] == 0.0
        && look.points[look.points.len() - 1][0] == 1.0
        && look.points[1..=nodes.len()].iter().zip(&nodes).all(|(p, &u)| p[0] == u)
}

/// Any look expressed on the zone nodes: output values sampled from the
/// look's own curve at the encoded zone centres, end points kept. For
/// `reference` and custom looks this is the "second source for the same
/// points" of the paper; the result is an approximation of the original
/// curve (PCHIP through fewer nodes) and the paper's bar can pull it.
pub fn look_on_zone_nodes(look: &LookFile) -> LookFile {
    let pchip = crate::curve::Pchip::new(&look.points);
    let y0 = look.points[0][1];
    let y1 = look.points[look.points.len() - 1][1];
    let mut out = default_look();
    let n = out.points.len();
    out.points[0][1] = y0;
    out.points[n - 1][1] = y1;
    for i in 1..n - 1 {
        let u = out.points[i][0];
        let x = match look.encoding {
            LookEncoding::Gamma22 => u,
            // A linear look's points are on the linear axis.
            LookEncoding::None => decode(u),
        };
        out.points[i][1] = pchip.eval(x).clamp(0.0, 1.0);
    }
    enforce_monotone(&mut out);
    out
}

/// Clamp output node `i` (0 = fog, last = paper white, 1 … n−2 the zones)
/// to `v` while keeping the nodes strictly increasing by at least
/// [`MIN_GAP`]. Returns the value actually set.
pub fn set_output(look: &mut LookFile, i: usize, v: f64) -> f64 {
    let n = look.points.len();
    assert!(i < n, "node index");
    let lo = if i == 0 { 0.0 } else { look.points[i - 1][1] + MIN_GAP };
    let hi = if i + 1 == n { 1.0 } else { look.points[i + 1][1] - MIN_GAP };
    let v = if lo <= hi { v.clamp(lo, hi) } else { look.points[i][1] };
    look.points[i][1] = v;
    v
}

/// Repair a node set so outputs are strictly increasing by [`MIN_GAP`] and
/// stay in [0, 1]: a forward pass pushes later nodes up, a backward pass
/// pulls earlier nodes down from paper white.
pub fn enforce_monotone(look: &mut LookFile) {
    let n = look.points.len();
    debug_assert!((n as f64) * MIN_GAP < 1.0, "too many nodes for the gap");
    for p in &mut look.points {
        p[1] = p[1].clamp(0.0, 1.0);
    }
    // Forward: every node at least one gap above its predecessor.
    for i in 1..n {
        let lo = look.points[i - 1][1] + MIN_GAP;
        if look.points[i][1] < lo {
            look.points[i][1] = lo;
        }
    }
    // Paper white caps the chain; backward: every node at least one gap
    // below its successor. The forward pass guarantees this cannot push the
    // fog below zero.
    look.points[n - 1][1] = look.points[n - 1][1].min(1.0);
    for i in (0..n - 1).rev() {
        let hi = look.points[i + 1][1] - MIN_GAP;
        if look.points[i][1] > hi {
            look.points[i][1] = hi;
        }
    }
}

/// Contrast change per paper grade, as a slope factor around Zone V in the
/// encoded domain. One grade is a quarter stop of contrast; N+2 spreads the
/// mid zones by 1.25² and so on.
pub const GRADE_FACTOR: f64 = 1.25;

/// Grade preset N+`n` (`n` may be negative): the default nodes with their
/// output spread (or compressed) around Zone V by `GRADE_FACTOR^n`, clamped
/// into the fog/paper-white interval and made strictly increasing. These are
/// lists of nodes, not a second filter; after the preset only the node set
/// counts. Zone V stays on the grey card.
pub fn grade(n: i32) -> LookFile {
    let mut look = default_look();
    if n == 0 {
        // Exactly the identity, not `pivot + (u − pivot) · 1` with its rounding.
        return look;
    }
    let g = GRADE_FACTOR.powi(n);
    let pivot = encode(GRAY_CARD);
    let last = look.points.len() - 1;
    for p in &mut look.points[1..last] {
        p[1] = (pivot + (p[0] - pivot) * g).clamp(0.0, 1.0);
    }
    enforce_monotone(&mut look);
    look
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::curve::Curve;

    #[test]
    fn zone_table() {
        assert!((linear(5) - 0.18).abs() < 1e-15);
        assert!((linear(4) - 0.09).abs() < 1e-15);
        assert!((linear(7) - 0.72).abs() < 1e-15);
        assert!(linear(8) > 1.0);
        assert_eq!(inner_zones(), vec![0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(numeral(5), "V");
        assert_eq!(numeral(10), "X");
        assert!((stops(0.18)).abs() < 1e-12);
        assert!((stops(0.36) - 1.0).abs() < 1e-12);
        assert!((zone_of(0.18) - 5.0).abs() < 1e-12);
        assert!((zone_of(0.045) - 3.0).abs() < 1e-12);
        assert_eq!(zone_of(0.0), 0.0);
        assert_eq!(zone_of(100.0), 10.0);
        assert!((decode(encode(0.3)) - 0.3).abs() < 1e-12);
    }

    /// Paper, table 4, row 1: the untouched bar is the neutral look.
    #[test]
    fn default_look_is_neutral_bit_for_bit() {
        let z = Curve::from_look(&default_look());
        let n = Curve::from_look(&LookFile::neutral());
        assert!(is_zone_look(&default_look()));
        for i in 0..=65535 {
            let x = i as f64 / 65535.0;
            assert_eq!(z.eval(x), n.eval(x), "at {x}");
        }
        for x in [0.0, 1e-9, 0.18, 0.5, 0.999999, 1.0, 1.7] {
            assert_eq!(z.eval(x), n.eval(x));
        }
    }

    /// Dragging a node clamps against its neighbours with the minimum gap.
    #[test]
    fn set_output_clamps_to_neighbours() {
        let mut look = default_look();
        let n = look.points.len();
        let v3 = look.points[3][1];
        let v5 = look.points[5][1];
        // Over the next node: stops one gap below it.
        let got = set_output(&mut look, 4, 0.99);
        assert!((got - (v5 - MIN_GAP)).abs() < 1e-15);
        // Under the previous node: stops one gap above it.
        let got = set_output(&mut look, 4, -1.0);
        assert!((got - (v3 + MIN_GAP)).abs() < 1e-15);
        // The neighbours themselves did not move (paper, table 4).
        assert_eq!(look.points[3][1], v3);
        assert_eq!(look.points[5][1], v5);
        // Paper white stays in [0, 1] and above the last inner node.
        let got = set_output(&mut look, n - 1, 2.0);
        assert_eq!(got, 1.0);
        let got = set_output(&mut look, n - 1, 0.0);
        assert!((got - (look.points[n - 2][1] + MIN_GAP)).abs() < 1e-15);
        look.validate().unwrap();
    }

    /// Presets are strictly increasing with the minimum gap, Zone V does not
    /// move, N+1 spreads and N−1 compresses the mid zones.
    #[test]
    fn grades_are_monotone_and_pivot_on_zone_v() {
        let v_index = 1 + inner_zones().iter().position(|&k| k == 5).unwrap();
        let pivot = encode(GRAY_CARD);
        for n in -3..=3 {
            let g = grade(n);
            g.validate().unwrap();
            for w in g.points.windows(2) {
                assert!(w[1][1] - w[0][1] >= MIN_GAP - 1e-12, "N{n:+}: {:?}", g.points);
            }
            assert!((g.points[v_index][1] - pivot).abs() < 1e-12, "N{n:+}");
        }
        let (hard, soft, normal) = (grade(1), grade(-1), grade(0));
        assert_eq!(normal.points, default_look().points);
        let vi = v_index + 1;
        let iv = v_index - 1;
        assert!(hard.points[vi][1] > normal.points[vi][1] && hard.points[iv][1] < normal.points[iv][1]);
        assert!(soft.points[vi][1] < normal.points[vi][1] && soft.points[iv][1] > normal.points[iv][1]);
    }

    /// The reference look on zone nodes reproduces the reference curve at
    /// the nodes exactly and closely in between.
    #[test]
    fn reference_on_zone_nodes_is_the_same_curve_at_the_nodes() {
        let reference = LookFile::load(std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../look/reference.json"
        )))
        .unwrap();
        let z = look_on_zone_nodes(&reference);
        z.validate().unwrap();
        assert!(is_zone_look(&z));
        let r = Curve::from_look(&reference);
        let zc = Curve::from_look(&z);
        for p in &z.points[1..z.points.len() - 1] {
            let lin = decode(p[0]);
            assert!((zc.eval(lin) - r.eval(lin)).abs() < 1e-9);
        }
        let mut worst: f64 = 0.0;
        for i in 0..=200 {
            let lin = i as f64 / 200.0;
            worst = worst.max((zc.eval(lin) - r.eval(lin)).abs());
        }
        // Eight nodes cannot carry every bend of a 20-point fit; the
        // deviation is below the 3/255 acceptance of the look itself.
        assert!(worst < 3.0 / 255.0, "{worst}");
    }

    #[test]
    fn enforce_monotone_repairs_any_order() {
        let mut look = default_look();
        for p in &mut look.points {
            p[1] = 1.0 - p[1];
        }
        enforce_monotone(&mut look);
        look.validate().unwrap();
        for w in look.points.windows(2) {
            assert!(w[1][1] - w[0][1] >= MIN_GAP - 1e-12);
        }
        assert!(look.points[0][1] >= 0.0 && look.points[look.points.len() - 1][1] <= 1.0);
    }
}
