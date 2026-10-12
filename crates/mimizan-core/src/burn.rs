//! Placing and parametric dodge & burn (Zonensystem paper, section 3 and
//! appendix A, as implemented).
//!
//! Placing is one scalar in stops, applied to the linear luminance before
//! the curve: `L ← L · 2^place`. Dodge & burn is *local placing*: the same
//! operation with a geometric weight `m(x, y) ∈ [0, 1]` per shape,
//!
//! `L'(x, y) = L(x, y) · 2^(place + Σ_i ev_i · m_i(x, y))`.
//!
//! That is what the darkroom does: more or less light through the same
//! paper curve. An EV is a stop and stays one; the curve's shoulder handles a
//! burnt sky like any other highlight; nothing can be pushed over paper
//! white by addition. The sign follows `place`: positive `ev` places the
//! region higher (brighter, the darkroom's *dodge*, light held back from
//! the paper), negative `ev` places it lower (darker, *burn*, more light on
//! the paper). The masks are geometric (a disc with a feather, a straight
//! gradient), never derived from the picture, so they produce no halo
//! wherever they sit in the pipeline; what sections 1 and 10 of the paper
//! forbid are content-adaptive local operators (tone equalizers), and they
//! stay forbidden.
//!
//! Coordinates are normalised to the upright picture: `x`, `y` in [0, 1] of
//! width and height (pixel centres at `(i + 0.5) / w`), radii as a fraction
//! of the long edge, so one recipe means the same place on the preview, the
//! 1:1 window and the print at any size.

use crate::plane::Plane;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// One dodge (positive `ev`, brighter) or burn (negative `ev`, darker) shape.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Burn {
    /// Disc: weight 1 inside `radius · (1 − feather)`, 0 outside `radius`,
    /// a smoothstep in between. `radius` is a fraction of the long edge.
    Radial { x: f64, y: f64, radius: f64, feather: f64, ev: f64 },
    /// Straight gradient: weight 0 at `(x0, y0)`, 1 at `(x1, y1)`, a
    /// smoothstep along the line between them, constant beyond either end.
    /// The card edge of the darkroom.
    Gradient { x0: f64, y0: f64, x1: f64, y1: f64, ev: f64 },
}

impl Burn {
    pub fn ev(&self) -> f64 {
        match *self {
            Self::Radial { ev, .. } | Self::Gradient { ev, .. } => ev,
        }
    }

    pub fn set_ev(&mut self, v: f64) {
        match self {
            Self::Radial { ev, .. } | Self::Gradient { ev, .. } => *ev = v,
        }
    }

    /// Command-line form: `radial:x,y,radius,feather,ev` or
    /// `gradient:x0,y0,x1,y1,ev` (normalised coordinates, stops).
    pub fn parse(s: &str) -> crate::Result<Self> {
        let bad = |m: &str| crate::Error::Invalid(format!("dodge & burn '{s}': {m}"));
        let (kind, rest) = s.trim().split_once(':').ok_or_else(|| bad("expected kind:values"))?;
        let v: Vec<f64> = rest
            .split(',')
            .map(|t| t.trim().parse::<f64>().map_err(|_| bad("bad number")))
            .collect::<crate::Result<_>>()?;
        let b = match (kind.trim().to_ascii_lowercase().as_str(), v.len()) {
            ("radial", 5) => Self::Radial { x: v[0], y: v[1], radius: v[2], feather: v[3], ev: v[4] },
            ("gradient", 5) => Self::Gradient { x0: v[0], y0: v[1], x1: v[2], y1: v[3], ev: v[4] },
            ("radial", _) => return Err(bad("radial needs x,y,radius,feather,ev")),
            ("gradient", _) => return Err(bad("gradient needs x0,y0,x1,y1,ev")),
            _ => return Err(bad("kind is radial or gradient")),
        };
        b.validate()?;
        Ok(b)
    }

    /// Parameters within their ranges and finite.
    pub fn validate(&self) -> crate::Result<()> {
        let unit = |v: f64, what: &str| {
            if !(0.0..=1.0).contains(&v) {
                return Err(crate::Error::Invalid(format!("dodge & burn: {what} {v} not within 0..1")));
            }
            Ok(())
        };
        match *self {
            Self::Radial { x, y, radius, feather, ev } => {
                unit(x, "x")?;
                unit(y, "y")?;
                unit(feather, "feather")?;
                if !(radius > 0.0 && radius <= 2.0) {
                    return Err(crate::Error::Invalid(format!(
                        "dodge & burn: radius {radius} not within 0..2"
                    )));
                }
                check_ev(ev)
            }
            Self::Gradient { x0, y0, x1, y1, ev } => {
                for (v, w) in [(x0, "x0"), (y0, "y0"), (x1, "x1"), (y1, "y1")] {
                    unit(v, w)?;
                }
                if (x1 - x0).abs() < 1e-9 && (y1 - y0).abs() < 1e-9 {
                    return Err(crate::Error::Invalid(
                        "dodge & burn: gradient needs two distinct points".into(),
                    ));
                }
                check_ev(ev)
            }
        }
    }

    /// Weight in [0, 1] at normalised position `(nx, ny)` of a `w × h`
    /// picture. `long` is `max(w, h)` as f64 (passed in so the inner loop
    /// does not recompute it).
    #[inline]
    pub fn weight(&self, nx: f64, ny: f64, w: f64, h: f64, long: f64) -> f64 {
        match *self {
            Self::Radial { x, y, radius, feather, .. } => {
                // Distances in long-edge units so the disc is round in pixels.
                let dx = (nx - x) * w / long;
                let dy = (ny - y) * h / long;
                let d = (dx * dx + dy * dy).sqrt();
                let inner = radius * (1.0 - feather);
                if d <= inner {
                    1.0
                } else if d >= radius {
                    0.0
                } else {
                    1.0 - smoothstep((d - inner) / (radius - inner))
                }
            }
            Self::Gradient { x0, y0, x1, y1, .. } => {
                let ax = (x1 - x0) * w / long;
                let ay = (y1 - y0) * h / long;
                let len2 = ax * ax + ay * ay;
                let px = (nx - x0) * w / long;
                let py = (ny - y0) * h / long;
                let t = ((px * ax + py * ay) / len2).clamp(0.0, 1.0);
                smoothstep(t)
            }
        }
    }
}

/// Stops within ±5: more than that is not a print decision.
pub const EV_LIMIT: f64 = 5.0;

fn check_ev(ev: f64) -> crate::Result<()> {
    if !ev.is_finite() || ev.abs() > EV_LIMIT {
        return Err(crate::Error::Invalid(format!("dodge & burn: ev {ev} not within ±{EV_LIMIT}")));
    }
    Ok(())
}

#[inline]
fn smoothstep(t: f64) -> f64 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

/// Exponent field `place + Σ ev_i m_i` at one pixel (centre convention).
#[inline]
pub fn exponent_at(place: f64, burns: &[Burn], x: usize, y: usize, w: usize, h: usize) -> f64 {
    let (wf, hf) = (w as f64, h as f64);
    let long = wf.max(hf);
    let nx = (x as f64 + 0.5) / wf;
    let ny = (y as f64 + 0.5) / hf;
    burns.iter().fold(place, |e, b| e + b.ev() * b.weight(nx, ny, wf, hf, long))
}

/// `L ← L · 2^(place + Σ ev_i m_i)` in place. `place == 0` with no shapes
/// (or every `ev == 0`) returns without touching a value, so the untouched
/// recipe is bit-identical to the run that never had this step.
pub fn apply(plane: &mut Plane, place: f64, burns: &[Burn]) {
    let active: Vec<Burn> = burns.iter().copied().filter(|b| b.ev() != 0.0).collect();
    if place == 0.0 && active.is_empty() {
        return;
    }
    let (w, h) = (plane.width, plane.height);
    let global = 2f64.powf(place);
    if active.is_empty() {
        plane.data.par_iter_mut().for_each(|v| *v *= global);
        return;
    }
    let (wf, hf) = (w as f64, h as f64);
    let long = wf.max(hf);
    plane.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let ny = (y as f64 + 0.5) / hf;
        for (x, v) in row.iter_mut().enumerate() {
            let nx = (x as f64 + 0.5) / wf;
            let e = active.iter().fold(place, |e, b| e + b.ev() * b.weight(nx, ny, wf, hf, long));
            *v *= 2f64.powf(e);
        }
    });
}

/// The weight field of all shapes together, `Σ ev_i m_i` in stops, for the
/// preview overlay. Same size as the picture it is asked for.
pub fn stops_field(burns: &[Burn], w: usize, h: usize) -> Plane {
    let mut out = Plane::zeros(w, h);
    let (wf, hf) = (w as f64, h as f64);
    let long = wf.max(hf);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let ny = (y as f64 + 0.5) / hf;
        for (x, v) in row.iter_mut().enumerate() {
            let nx = (x as f64 + 0.5) / wf;
            *v = burns.iter().fold(0.0, |e, b| e + b.ev() * b.weight(nx, ny, wf, hf, long));
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ramp(w: usize, h: usize) -> Plane {
        Plane::from_vec(w, h, (0..w * h).map(|i| 0.05 + 0.9 * (i % w) as f64 / w as f64).collect())
    }

    /// Paper, table 4: place = 0 changes no pixel; ev = 0 changes no pixel.
    #[test]
    fn zero_is_the_identity_bit_for_bit() {
        let src = ramp(64, 32);
        let mut p = src.clone();
        apply(&mut p, 0.0, &[]);
        assert_eq!(p.data, src.data);
        let burns = [
            Burn::Radial { x: 0.5, y: 0.5, radius: 0.3, feather: 0.5, ev: 0.0 },
            Burn::Gradient { x0: 0.0, y0: 0.0, x1: 1.0, y1: 0.0, ev: 0.0 },
        ];
        apply(&mut p, 0.0, &burns);
        assert_eq!(p.data, src.data);
    }

    /// Placing is one stop per unit, everywhere.
    #[test]
    fn place_is_stops() {
        let src = ramp(64, 32);
        let mut p = src.clone();
        apply(&mut p, 1.0, &[]);
        for (a, b) in p.data.iter().zip(&src.data) {
            assert_eq!(*a, b * 2.0);
        }
        let mut p = src.clone();
        apply(&mut p, -2.0, &[]);
        for (a, b) in p.data.iter().zip(&src.data) {
            assert!((a - b * 0.25).abs() < 1e-15);
        }
    }

    /// A disc burns fully inside, not at all outside, and the feather is
    /// monotone in the distance; it is round in pixels on a 2:1 picture.
    #[test]
    fn radial_mask_is_round_and_monotone() {
        let (w, h) = (200, 100);
        let b = Burn::Radial { x: 0.5, y: 0.5, radius: 0.2, feather: 0.5, ev: 1.0 };
        b.validate().unwrap();
        let f = stops_field(&[b], w, h);
        // Long edge 200 px: radius 40 px, inner 20 px.
        assert_eq!(f.at(100, 50), 1.0);
        assert_eq!(f.at(115, 50), 1.0);
        assert_eq!(f.at(100, 65), 1.0, "round: the same 15 px vertically");
        assert_eq!(f.at(145, 50), 0.0);
        assert_eq!(f.at(100, 95), 0.0);
        let mut last = 1.0;
        for x in 100..150 {
            let v = f.at(x, 50);
            assert!(v <= last + 1e-12, "not monotone at {x}");
            assert!((0.0..=1.0).contains(&v));
            last = v;
        }
        // The applied plane equals plane · 2^field.
        let src = ramp(w, h);
        let mut p = src.clone();
        apply(&mut p, 0.0, &[b]);
        for i in 0..w * h {
            assert!((p.data[i] - src.data[i] * 2f64.powf(f.data[i])).abs() < 1e-15);
        }
    }

    /// A gradient is 0 before its first point, 1 after its second, and
    /// monotone along the line; a burn (negative ev) brightens nothing.
    #[test]
    fn gradient_mask() {
        let (w, h) = (100, 60);
        let b = Burn::Gradient { x0: 0.2, y0: 0.5, x1: 0.8, y1: 0.5, ev: -1.0 };
        b.validate().unwrap();
        let f = stops_field(&[b], w, h);
        assert_eq!(f.at(5, 30), 0.0);
        assert_eq!(f.at(95, 30), -1.0);
        assert_eq!(f.at(95, 2), -1.0, "constant across the line");
        let mut last = 0.0;
        for x in 20..80 {
            let v = f.at(x, 30);
            assert!(v <= last + 1e-12);
            last = v;
        }
        let src = ramp(w, h);
        let mut p = src.clone();
        apply(&mut p, 0.0, &[b]);
        for i in 0..w * h {
            assert!(p.data[i] <= src.data[i] + 1e-15);
        }
    }

    /// Two shapes add in stops; place adds to both.
    #[test]
    fn shapes_add_in_stops() {
        let (w, h) = (40, 40);
        let a = Burn::Radial { x: 0.5, y: 0.5, radius: 1.0, feather: 0.0, ev: 1.0 };
        let b = Burn::Radial { x: 0.5, y: 0.5, radius: 1.0, feather: 0.0, ev: 0.5 };
        let e = exponent_at(0.25, &[a, b], 20, 20, w, h);
        assert!((e - 1.75).abs() < 1e-12);
        let src = ramp(w, h);
        let mut p = src.clone();
        apply(&mut p, 0.25, &[a, b]);
        assert!((p.at(20, 20) - src.at(20, 20) * 2f64.powf(1.75)).abs() < 1e-14);
    }

    #[test]
    fn validation() {
        assert!(Burn::Radial { x: 1.2, y: 0.5, radius: 0.1, feather: 0.0, ev: 0.0 }.validate().is_err());
        assert!(Burn::Radial { x: 0.5, y: 0.5, radius: 0.0, feather: 0.0, ev: 0.0 }.validate().is_err());
        assert!(Burn::Radial { x: 0.5, y: 0.5, radius: 0.1, feather: 0.0, ev: 9.0 }.validate().is_err());
        assert!(Burn::Gradient { x0: 0.5, y0: 0.5, x1: 0.5, y1: 0.5, ev: 1.0 }.validate().is_err());
        let json =
            serde_json::to_string(&Burn::Radial { x: 0.452, y: 0.61, radius: 0.15, feather: 0.5, ev: 0.75 })
                .unwrap();
        assert!(json.starts_with(r#"{"type":"radial","x":0.452"#), "{json}");
        let back: Burn = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ev(), 0.75);
        assert_eq!(
            Burn::parse("radial:0.452,0.61,0.15,0.5,0.75").unwrap(),
            Burn::Radial { x: 0.452, y: 0.61, radius: 0.15, feather: 0.5, ev: 0.75 }
        );
        assert_eq!(
            Burn::parse(" Gradient: 0.5, 0, 0.5, 0.4, 1 ").unwrap(),
            Burn::Gradient { x0: 0.5, y0: 0.0, x1: 0.5, y1: 0.4, ev: 1.0 }
        );
        assert!(Burn::parse("radial:0.5,0.5,0.1").is_err());
        assert!(Burn::parse("disc:0.5,0.5,0.1,0,1").is_err());
        assert!(Burn::parse("radial:0.5,0.5,0.1,0,9").is_err());
    }
}
