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
//!
//! On paper (`paper.rs`) a shape can act through one filter: a burn with
//! the hard filter deepens shadows and leaves the highlights, a dodge with
//! the soft filter opens highlights, as the split-grade printer does it.
//! [`Filter::Both`] is the plain card over the lens. A look (`curve.rs`)
//! has one exposure and treats every shape as `Both`.

use crate::plane::Plane;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// Which exposure of a split-grade print a shape acts on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Filter {
    /// The card over the lens for both passes (and the only case a look knows).
    #[default]
    Both,
    /// Only the soft pass (filter 00): the highlights.
    Soft,
    /// Only the hard pass (filter 5): the shadows.
    Hard,
}

impl Filter {
    pub const ALL: [Filter; 3] = [Filter::Both, Filter::Soft, Filter::Hard];

    pub fn name(&self) -> &'static str {
        match self {
            Self::Both => "both",
            Self::Soft => "soft",
            Self::Hard => "hard",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|f| f.name() == s.trim().to_ascii_lowercase())
    }

    #[inline]
    pub fn soft(&self) -> bool {
        matches!(self, Self::Both | Self::Soft)
    }

    #[inline]
    pub fn hard(&self) -> bool {
        matches!(self, Self::Both | Self::Hard)
    }
}

fn is_both(f: &Filter) -> bool {
    *f == Filter::Both
}

/// One dodge (positive `ev`, brighter) or burn (negative `ev`, darker) shape.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "kebab-case")]
pub enum Burn {
    /// Disc: weight 1 inside `radius · (1 − feather)`, 0 outside `radius`,
    /// a smoothstep in between. `radius` is a fraction of the long edge.
    Radial {
        x: f64,
        y: f64,
        radius: f64,
        feather: f64,
        ev: f64,
        #[serde(default, skip_serializing_if = "is_both")]
        filter: Filter,
    },
    /// Straight gradient: weight 0 at `(x0, y0)`, 1 at `(x1, y1)`, a
    /// smoothstep along the line between them, constant beyond either end.
    /// The card edge of the darkroom.
    Gradient {
        x0: f64,
        y0: f64,
        x1: f64,
        y1: f64,
        ev: f64,
        #[serde(default, skip_serializing_if = "is_both")]
        filter: Filter,
    },
}

impl Burn {
    pub fn radial(x: f64, y: f64, radius: f64, feather: f64, ev: f64) -> Self {
        Self::Radial { x, y, radius, feather, ev, filter: Filter::Both }
    }

    pub fn gradient(x0: f64, y0: f64, x1: f64, y1: f64, ev: f64) -> Self {
        Self::Gradient { x0, y0, x1, y1, ev, filter: Filter::Both }
    }

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

    pub fn filter(&self) -> Filter {
        match *self {
            Self::Radial { filter, .. } | Self::Gradient { filter, .. } => filter,
        }
    }

    pub fn set_filter(&mut self, f: Filter) {
        match self {
            Self::Radial { filter, .. } | Self::Gradient { filter, .. } => *filter = f,
        }
    }

    /// Command-line form: `radial:x,y,radius,feather,ev[,filter]` or
    /// `gradient:x0,y0,x1,y1,ev[,filter]` (normalised coordinates, stops,
    /// filter `both` (default), `soft` or `hard`).
    pub fn parse(s: &str) -> crate::Result<Self> {
        let bad = |m: &str| crate::Error::Invalid(format!("dodge & burn '{s}': {m}"));
        let (kind, rest) = s.trim().split_once(':').ok_or_else(|| bad("expected kind:values"))?;
        let mut parts: Vec<&str> = rest.split(',').map(str::trim).collect();
        let filter = match parts.last().and_then(|t| Filter::parse(t)) {
            Some(f) => {
                parts.pop();
                f
            }
            None => Filter::Both,
        };
        let v: Vec<f64> = parts
            .iter()
            .map(|t| t.parse::<f64>().map_err(|_| bad("bad number")))
            .collect::<crate::Result<_>>()?;
        let b = match (kind.trim().to_ascii_lowercase().as_str(), v.len()) {
            ("radial", 5) => Self::Radial { x: v[0], y: v[1], radius: v[2], feather: v[3], ev: v[4], filter },
            ("gradient", 5) => Self::Gradient { x0: v[0], y0: v[1], x1: v[2], y1: v[3], ev: v[4], filter },
            ("radial", _) => return Err(bad("radial needs x,y,radius,feather,ev[,filter]")),
            ("gradient", _) => return Err(bad("gradient needs x0,y0,x1,y1,ev[,filter]")),
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
            Self::Radial { x, y, radius, feather, ev, .. } => {
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
            Self::Gradient { x0, y0, x1, y1, ev, .. } => {
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

/// Exponent field `place + Σ ev_i m_i` at one pixel (centre convention),
/// every shape counted (a look has one exposure).
#[inline]
pub fn exponent_at(place: f64, burns: &[Burn], x: usize, y: usize, w: usize, h: usize) -> f64 {
    let (wf, hf) = (w as f64, h as f64);
    let long = wf.max(hf);
    let nx = (x as f64 + 0.5) / wf;
    let ny = (y as f64 + 0.5) / hf;
    burns.iter().fold(place, |e, b| e + b.ev() * b.weight(nx, ny, wf, hf, long))
}

/// Does any active shape act through one filter only? Then the two passes
/// of the paper see different exponents and [`exponents_at`] is needed.
pub fn needs_split(burns: &[Burn]) -> bool {
    burns.iter().any(|b| b.ev() != 0.0 && b.filter() != Filter::Both)
}

/// The two exponent fields at a normalised position: (soft, hard), each
/// `place + Σ ev_i m_i` over the shapes that act on that pass.
#[inline]
pub fn exponents_at(place: f64, burns: &[Burn], nx: f64, ny: f64, w: f64, h: f64, long: f64) -> (f64, f64) {
    burns.iter().fold((place, place), |(es, eh), b| {
        let d = b.ev() * b.weight(nx, ny, w, h, long);
        let f = b.filter();
        (es + if f.soft() { d } else { 0.0 }, eh + if f.hard() { d } else { 0.0 })
    })
}

/// `L ← L · 2^(place + Σ ev_i m_i)` in place. `place == 0` with no shapes
/// (or every `ev == 0`) returns without touching a value, so the untouched
/// recipe is bit-identical to the run that never had this step.
pub fn apply(plane: &mut Plane, place: f64, burns: &[Burn]) {
    let (w, h) = (plane.width, plane.height);
    apply_window(plane, place, burns, (w, h), (0, 0));
}

/// The same on a window of a `full` (w, h) picture whose top-left pixel is
/// `origin` in that picture: the shapes are evaluated in the coordinates of
/// the whole picture, so a 1:1 crop and the whole preview agree.
pub fn apply_window(
    plane: &mut Plane,
    place: f64,
    burns: &[Burn],
    full: (usize, usize),
    origin: (usize, usize),
) {
    let active: Vec<Burn> = burns.iter().copied().filter(|b| b.ev() != 0.0).collect();
    if place == 0.0 && active.is_empty() {
        return;
    }
    let w = plane.width;
    let global = 2f64.powf(place);
    if active.is_empty() {
        plane.data.par_iter_mut().for_each(|v| *v *= global);
        return;
    }
    let (wf, hf) = (full.0 as f64, full.1 as f64);
    let long = wf.max(hf);
    plane.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let ny = ((origin.1 + y) as f64 + 0.5) / hf;
        for (x, v) in row.iter_mut().enumerate() {
            let nx = ((origin.0 + x) as f64 + 0.5) / wf;
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
        let burns = [Burn::radial(0.5, 0.5, 0.3, 0.5, 0.0), Burn::gradient(0.0, 0.0, 1.0, 0.0, 0.0)];
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
        let b = Burn::radial(0.5, 0.5, 0.2, 0.5, 1.0);
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
        let b = Burn::gradient(0.2, 0.5, 0.8, 0.5, -1.0);
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
        let a = Burn::radial(0.5, 0.5, 1.0, 0.0, 1.0);
        let b = Burn::radial(0.5, 0.5, 1.0, 0.0, 0.5);
        let e = exponent_at(0.25, &[a, b], 20, 20, w, h);
        assert!((e - 1.75).abs() < 1e-12);
        let src = ramp(w, h);
        let mut p = src.clone();
        apply(&mut p, 0.25, &[a, b]);
        assert!((p.at(20, 20) - src.at(20, 20) * 2f64.powf(1.75)).abs() < 1e-14);
    }

    /// A window rendered with `apply_window` equals the same pixels of the
    /// whole picture rendered with `apply`.
    #[test]
    fn window_matches_the_whole() {
        let (w, h) = (90, 70);
        let src = ramp(w, h);
        let burns = [Burn::radial(0.3, 0.6, 0.25, 0.4, 0.6), Burn::gradient(0.9, 0.1, 0.5, 0.5, -0.4)];
        let mut whole = src.clone();
        apply(&mut whole, 0.2, &burns);
        let (x0, y0, cw, ch) = (17, 23, 40, 30);
        let mut win = Plane::zeros(cw, ch);
        for y in 0..ch {
            win.row_mut(y).copy_from_slice(&src.row(y0 + y)[x0..x0 + cw]);
        }
        apply_window(&mut win, 0.2, &burns, (w, h), (x0, y0));
        for y in 0..ch {
            for x in 0..cw {
                assert_eq!(win.at(x, y), whole.at(x0 + x, y0 + y), "at {x},{y}");
            }
        }
    }

    #[test]
    fn validation() {
        assert!(Burn::radial(1.2, 0.5, 0.1, 0.0, 0.0).validate().is_err());
        assert!(Burn::radial(0.5, 0.5, 0.0, 0.0, 0.0).validate().is_err());
        assert!(Burn::radial(0.5, 0.5, 0.1, 0.0, 9.0).validate().is_err());
        assert!(Burn::gradient(0.5, 0.5, 0.5, 0.5, 1.0).validate().is_err());
        let json = serde_json::to_string(&Burn::radial(0.452, 0.61, 0.15, 0.5, 0.75)).unwrap();
        assert!(json.starts_with(r#"{"type":"radial","x":0.452"#), "{json}");
        let back: Burn = serde_json::from_str(&json).unwrap();
        assert_eq!(back.ev(), 0.75);
        assert_eq!(
            Burn::parse("radial:0.452,0.61,0.15,0.5,0.75").unwrap(),
            Burn::radial(0.452, 0.61, 0.15, 0.5, 0.75)
        );
        assert_eq!(
            Burn::parse(" Gradient: 0.5, 0, 0.5, 0.4, 1 ").unwrap(),
            Burn::gradient(0.5, 0.0, 0.5, 0.4, 1.0)
        );
        assert!(Burn::parse("radial:0.5,0.5,0.1").is_err());
        assert!(Burn::parse("disc:0.5,0.5,0.1,0,1").is_err());
        assert!(Burn::parse("radial:0.5,0.5,0.1,0,9").is_err());
        // A filter at the end; absent or `both` is not written.
        let hard = Burn::parse("radial:0.5,0.5,0.1,0,-1,hard").unwrap();
        assert_eq!(hard.filter(), Filter::Hard);
        let json = serde_json::to_string(&hard).unwrap();
        assert!(json.ends_with(r#""filter":"hard"}"#), "{json}");
        assert!(!serde_json::to_string(&Burn::radial(0.5, 0.5, 0.1, 0.0, 1.0)).unwrap().contains("filter"));
        assert!(Burn::parse("radial:0.5,0.5,0.1,0,-1,blue").is_err());
        assert!(needs_split(&[hard]));
        assert!(!needs_split(&[Burn::radial(0.5, 0.5, 0.1, 0.0, 1.0)]));
        let mut soft = Burn::gradient(0.0, 0.0, 1.0, 0.0, 1.0);
        soft.set_filter(Filter::Soft);
        // At the far end of the gradient the soft pass sees +1, the hard pass nothing.
        let (es, eh) = exponents_at(0.25, &[soft, hard], 1.0, 0.5, 100.0, 100.0, 100.0);
        assert!((es - 1.25).abs() < 1e-12 && (eh - 0.25).abs() < 1e-12);
    }
}
