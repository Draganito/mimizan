//! Tone curve: monotone PCHIP through the look points (SPEC section 9, "Kurve").

use crate::calib::{LookEncoding, LookFile};
use crate::plane::Plane;
use rayon::prelude::*;

/// Fritsch–Carlson monotone cubic Hermite interpolant on strictly increasing x.
#[derive(Clone, Debug)]
pub struct Pchip {
    x: Vec<f64>,
    y: Vec<f64>,
    d: Vec<f64>,
}

impl Pchip {
    pub fn new(points: &[[f64; 2]]) -> Self {
        let n = points.len();
        assert!(n >= 2, "PCHIP needs at least two points");
        let x: Vec<f64> = points.iter().map(|p| p[0]).collect();
        let y: Vec<f64> = points.iter().map(|p| p[1]).collect();
        let h: Vec<f64> = (0..n - 1).map(|i| x[i + 1] - x[i]).collect();
        let delta: Vec<f64> = (0..n - 1).map(|i| (y[i + 1] - y[i]) / h[i]).collect();
        let mut d = vec![0.0; n];
        if n == 2 {
            d[0] = delta[0];
            d[1] = delta[0];
        } else {
            for i in 1..n - 1 {
                if delta[i - 1] * delta[i] <= 0.0 {
                    d[i] = 0.0;
                } else {
                    // Weighted harmonic mean (Fritsch–Butland), monotone by construction.
                    let w1 = 2.0 * h[i] + h[i - 1];
                    let w2 = h[i] + 2.0 * h[i - 1];
                    d[i] = (w1 + w2) / (w1 / delta[i - 1] + w2 / delta[i]);
                }
            }
            d[0] = end_slope(h[0], h[1], delta[0], delta[1]);
            d[n - 1] = end_slope(h[n - 2], h[n - 3], delta[n - 2], delta[n - 3]);
        }
        Self { x, y, d }
    }

    /// Evaluate; clamps outside the point range to the end values.
    pub fn eval(&self, v: f64) -> f64 {
        let n = self.x.len();
        if v <= self.x[0] {
            return self.y[0];
        }
        if v >= self.x[n - 1] {
            return self.y[n - 1];
        }
        // Binary search for the interval.
        let mut lo = 0usize;
        let mut hi = n - 1;
        while hi - lo > 1 {
            let mid = (lo + hi) / 2;
            if self.x[mid] <= v {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        let h = self.x[hi] - self.x[lo];
        let t = (v - self.x[lo]) / h;
        let t2 = t * t;
        let t3 = t2 * t;
        let h00 = 2.0 * t3 - 3.0 * t2 + 1.0;
        let h10 = t3 - 2.0 * t2 + t;
        let h01 = -2.0 * t3 + 3.0 * t2;
        let h11 = t3 - t2;
        h00 * self.y[lo] + h10 * h * self.d[lo] + h01 * self.y[hi] + h11 * h * self.d[hi]
    }
}

/// Three-point end slope with the Fritsch–Carlson shape-preserving limiter.
fn end_slope(h0: f64, h1: f64, d0: f64, d1: f64) -> f64 {
    let s = ((2.0 * h0 + h1) * d0 - h0 * d1) / (h0 + h1);
    if s * d0 <= 0.0 {
        0.0
    } else if d0 * d1 <= 0.0 && s.abs() > 3.0 * d0.abs() {
        3.0 * d0
    } else {
        s
    }
}

/// A look ready to apply: encoding plus the interpolated points.
#[derive(Clone, Debug)]
pub struct Curve {
    encoding: LookEncoding,
    pchip: Pchip,
    identity_points: bool,
}

impl Curve {
    pub fn from_look(look: &LookFile) -> Self {
        let identity_points =
            look.points.len() == 2 && look.points[0] == [0.0, 0.0] && look.points[1] == [1.0, 1.0];
        Self { encoding: look.encoding, pchip: Pchip::new(&look.points), identity_points }
    }

    /// Linear in [0,1] -> encoded in [0,1].
    #[inline]
    pub fn eval(&self, linear: f64) -> f64 {
        let x = linear.clamp(0.0, 1.0);
        let e = match self.encoding {
            LookEncoding::Gamma22 => x.powf(1.0 / 2.2),
            LookEncoding::None => x,
        };
        if self.identity_points {
            e
        } else {
            self.pchip.eval(e).clamp(0.0, 1.0)
        }
    }

    pub fn apply(&self, p: &Plane) -> Plane {
        let mut out = Plane::zeros(p.width, p.height);
        out.data.par_iter_mut().zip(&p.data).for_each(|(o, &v)| *o = self.eval(v));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pchip_interpolates_points_and_stays_monotone() {
        let pts = [[0.0, 0.0], [0.2, 0.1], [0.5, 0.6], [0.8, 0.9], [1.0, 1.0]];
        let p = Pchip::new(&pts);
        for q in pts {
            assert!((p.eval(q[0]) - q[1]).abs() < 1e-12);
        }
        let mut last = -1.0;
        for i in 0..=1000 {
            let v = p.eval(i as f64 / 1000.0);
            assert!(v >= last - 1e-12, "not monotone at {i}");
            assert!((0.0..=1.0).contains(&v));
            last = v;
        }
    }

    #[test]
    fn pchip_on_a_line_is_the_line() {
        let p = Pchip::new(&[[0.0, 0.0], [0.3, 0.3], [1.0, 1.0]]);
        for i in 0..=100 {
            let x = i as f64 / 100.0;
            assert!((p.eval(x) - x).abs() < 1e-12);
        }
    }

    #[test]
    fn neutral_look_is_gamma22() {
        let c = Curve::from_look(&LookFile::neutral());
        assert!((c.eval(0.18) - 0.18f64.powf(1.0 / 2.2)).abs() < 1e-12);
        assert_eq!(c.eval(0.0), 0.0);
        assert_eq!(c.eval(1.0), 1.0);
        assert_eq!(c.eval(1.7), 1.0);
    }
}
