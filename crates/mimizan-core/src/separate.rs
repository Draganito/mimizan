//! Luminance / chrominance separation of a Bayer mosaic.

use crate::cfa::BayerPhase;
use crate::filter::{convolve_cols_signed, convolve_rows_mod, convolve_rows_mod_into, LowpassSpec};
use crate::ingest::Mosaic;
use crate::plane::Plane;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};

/// How the ambiguous band around each carrier is divided between luminance
/// and colour.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum MaskMode {
    /// Fixed separation, g1 = g2 = 1 (Phase 2 estimator).
    Off,
    /// Block-spectrum mask with Dubois weighting of the two C2 copies.
    Adaptive,
    /// Pixel-adaptive weighting of the two C2 copies and of two
    /// direction-dependent C1 bands, elongated pass bands (`dubois.rs`).
    /// Default.
    Dubois,
}

/// Parameters of the separation. `cutoff`, `transition` and `attenuation_db`
/// describe the Kaiser low-pass of the `Off` and `Adaptive` modes; the
/// `Dubois` mode has its own bands in `dubois` and does not read them.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SeparateParams {
    /// Chroma cutoff, cycles per pixel (`Off`, `Adaptive`).
    pub cutoff: f64,
    /// Transition width of that low-pass, cycles per pixel.
    pub transition: f64,
    /// Stop-band attenuation of that low-pass, dB.
    pub attenuation_db: f64,
    pub mask: MaskMode,
    /// Rounds of the cross-term refinement in the `Adaptive` mode.
    pub rounds_max: usize,
    /// Parameters of the `Dubois` mode (ignored otherwise).
    #[serde(default)]
    pub dubois: crate::dubois::DuboisParams,
}

impl Default for SeparateParams {
    fn default() -> Self {
        // rounds_max = 1: the cross-term refinement changed no bench figure
        // beyond the fourth digit while doubling the run time.
        Self {
            cutoff: 0.15,
            transition: 0.05,
            attenuation_db: 60.0,
            mask: MaskMode::Dubois,
            rounds_max: 1,
            dubois: Default::default(),
        }
    }
}

impl SeparateParams {
    /// The Kaiser low-pass of the `Off` and `Adaptive` modes.
    pub fn lowpass_spec(&self) -> LowpassSpec {
        LowpassSpec { cutoff: self.cutoff, transition: self.transition, attenuation_db: self.attenuation_db }
    }
}

/// Result of the separation: everything the mixer and the writers need.
#[derive(Clone, Debug)]
pub struct Separation {
    /// Estimated luminance `L̂` (full resolution).
    pub lum: Plane,
    /// Low-pass chrominance `Ĉ1'`, mask gain and the colour gate
    /// ([`Mosaic::chroma_gate`]) already folded in.
    pub c1: Plane,
    /// Combined low-pass chrominance `Ĉ2'`, mask gain and the colour gate
    /// already folded in.
    pub c2: Plane,
    /// Where output sharpening is allowed, per pixel in [0,1]. `Adaptive`:
    /// `max(M_1, M_2a, M_2b)` of the block mask. `Off` and `Dubois`:
    /// `FIXED_MASK_VALUE` everywhere.
    pub mask_max: Plane,
    /// Refinement rounds that ran.
    pub rounds: usize,
    /// Bayer phase of the mosaic the separation was made from.
    pub phase: BayerPhase,
}

/// The Nyquist carrier `(−1)^x`: +1 on even, −1 on odd indices.
#[inline]
pub fn sign_x(x: usize) -> f64 {
    if x & 1 == 0 {
        1.0
    } else {
        -1.0
    }
}

/// Fixed-filter estimate of the three chrominance planes from a mosaic `m`:
/// `Ĉ1 = LP(ε·sx·sy·m)`, `Ĉ2a = LP(a·sx·m)`, `Ĉ2b = LP(b·sy·m)`.
///
/// The carriers are ±1 per pixel, so the row pass over `sx·m` serves both Ĉ1
/// and Ĉ2a (the remaining `ε·sy` / `a` factors are constant per row and move
/// into the column pass), and the row pass over `m` serves Ĉ2b. Two row passes
/// and three column passes instead of three modulations and six passes; the
/// per-output tap order is unchanged, so the result is bit-identical.
pub fn chroma_estimates(m: &Plane, phase: BayerPhase, k: &[f64]) -> (Plane, Plane, Plane) {
    let c = phase.carriers();
    let mut r = convolve_rows_mod(m, k, |x, _| sign_x(x));
    let c1 = convolve_cols_signed(&r, k, |y| c.eps * sign_x(y));
    let c2a = convolve_cols_signed(&r, k, |_| c.a);
    convolve_rows_mod_into(m, k, |_, _| 1.0, &mut r);
    let c2b = convolve_cols_signed(&r, k, |y| c.b * sign_x(y));
    (c1, c2a, c2b)
}

/// `L̂ = m − ε·sx·sy·Ĉ1 − (a·sx + b·sy)·Ĉ2` (any gains are already inside Ĉ).
pub fn luminance(m: &Plane, phase: BayerPhase, c1: &Plane, c2: &Plane) -> Plane {
    let c = phase.carriers();
    let w = m.width;
    let mut out = Plane::zeros(w, m.height);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let sy = sign_x(y);
        let (mr, c1r, c2r) = (m.row(y), c1.row(y), c2.row(y));
        for x in 0..w {
            let sx = sign_x(x);
            row[x] = mr[x] - c.eps * sx * sy * c1r[x] - (c.a * sx + c.b * sy) * c2r[x];
        }
    });
    out
}

/// `a ← (a + b) / 2` in place.
pub fn average_in_place(a: &mut Plane, b: &Plane) {
    a.data.par_iter_mut().zip(&b.data).for_each(|(x, &y)| *x = 0.5 * (*x + y));
}

/// Mask value the fixed estimator reports everywhere: the carriers are
/// suppressed by the 60 dB low-pass in every block, so output sharpening is
/// allowed everywhere except at saturation (the sidecar's 255). The value is
/// exactly the sharpening gate's threshold, so the gate reads it as "allowed".
pub const FIXED_MASK_VALUE: f64 = crate::usm::MASK_THRESHOLD;

fn fixed(m: &Plane, phase: BayerPhase, c1: Plane, mut c2a: Plane, c2b: Plane) -> Separation {
    average_in_place(&mut c2a, &c2b);
    drop(c2b);
    let lum = luminance(m, phase, &c1, &c2a);
    let mask_max = Plane::from_vec(m.width, m.height, vec![FIXED_MASK_VALUE; m.width * m.height]);
    Separation { lum, c1, c2: c2a, mask_max, rounds: 1, phase }
}

/// Separate a Bayer mosaic in the mode `p.mask` names. Every output pixel
/// is a fixed-weight sum of measured values; nothing is computed from a
/// model of the picture. A monochrome mosaic is an error.
///
/// The luminance is estimated with the full chrominance subtracted. The
/// chrominance handed on to the mixes is then gated by
/// [`Mosaic::chroma_gate`]: where a green photosite is burnt out, the
/// colour differences are false (towards magenta, whatever the scene), so
/// the mixes get none of them there and the three plates of a colour
/// print, like a filtered negative, take the luminance alone. `L̂` and the
/// native mix are untouched.
pub fn separate(mosaic: &Mosaic, p: &SeparateParams) -> crate::Result<Separation> {
    let phase = mosaic.phase.ok_or_else(|| crate::Error::Invalid("separate needs a Bayer mosaic".into()))?;
    let mut sep = separate_linear(mosaic, phase, p);
    if let Some(gate) = &mosaic.chroma_gate {
        gate_chroma(&mut sep, gate)?;
    }
    Ok(sep)
}

/// `Ĉ1' ← g·Ĉ1'`, `Ĉ2' ← g·Ĉ2'` with `g = gate / 255` per pixel.
pub fn gate_chroma(sep: &mut Separation, gate: &[u8]) -> crate::Result<()> {
    if gate.len() != sep.c1.data.len() || gate.len() != sep.c2.data.len() {
        return Err(crate::Error::Invalid(format!(
            "colour gate has {} entries, the separation {}",
            gate.len(),
            sep.c1.data.len()
        )));
    }
    for c in [&mut sep.c1, &mut sep.c2] {
        c.data.par_iter_mut().zip(gate.par_iter()).for_each(|(v, &g)| {
            if g != 255 {
                *v *= f64::from(g) / 255.0;
            }
        });
    }
    Ok(())
}

fn separate_linear(mosaic: &Mosaic, phase: BayerPhase, p: &SeparateParams) -> Separation {
    let m = &mosaic.plane;
    if p.mask == MaskMode::Dubois {
        return crate::dubois::separate_dubois(mosaic, phase, &p.dubois);
    }
    let k = p.lowpass_spec().kernel();
    let t = std::time::Instant::now();
    let (c1, c2a, c2b) = chroma_estimates(m, phase, &k);
    tracing::debug!("chroma estimates ({} taps) {} ms", k.len(), t.elapsed().as_millis());

    // Too small for a block grid: the adaptive mode falls back to the fixed
    // estimator.
    let adaptive = p.mask == MaskMode::Adaptive && crate::mask::grid_dims(m.width, m.height) != (0, 0);
    if adaptive {
        crate::mask::separate_adaptive(mosaic, phase, &k, c1, c2a, c2b, p)
    } else {
        fixed(m, phase, c1, c2a, c2b)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfa::Color;

    fn mosaic_of(
        phase: BayerPhase,
        w: usize,
        h: usize,
        rgb: impl Fn(usize, usize) -> (f64, f64, f64),
    ) -> Plane {
        let mut m = Plane::zeros(w, h);
        for y in 0..h {
            for x in 0..w {
                let (r, g, b) = rgb(x, y);
                let v = match phase.color_at(x, y) {
                    Color::R => r,
                    Color::G => g,
                    Color::B => b,
                };
                m.set(x, y, v);
            }
        }
        m
    }

    /// A flat colour field must come out as flat L = (R+2G+B)/4 with no 2-px grid.
    #[test]
    fn flat_colour_field_separates_exactly() {
        let (w, h) = (192, 160);
        let (r, g, b) = (0.6, 0.3, 0.1);
        let l_true = (r + 2.0 * g + b) / 4.0;
        let k = LowpassSpec::default().kernel();
        for phase in BayerPhase::ALL {
            let m = mosaic_of(phase, w, h, |_, _| (r, g, b));
            let (c1, mut c2, c2b) = chroma_estimates(&m, phase, &k);
            average_in_place(&mut c2, &c2b);
            let lum = luminance(&m, phase, &c1, &c2);
            for y in 40..h - 40 {
                for x in 40..w - 40 {
                    assert!((lum.at(x, y) - l_true).abs() < 1e-9, "{phase} ({x},{y}) {}", lum.at(x, y));
                    assert!((c1.at(x, y) - (-r + 2.0 * g - b) / 4.0).abs() < 1e-9);
                    assert!((c2.at(x, y) - (r - b) / 4.0).abs() < 1e-9);
                }
            }
        }
    }

    /// A neutral field with a burnt-out patch, balanced R 2, G 1, B 1.5:
    /// inside the patch the mosaic reads (2, 1, 1.5), a false magenta. The
    /// gate takes that colour out of the mixes, so the three plates are
    /// equal there and equal to the luminance; the luminance itself, and
    /// so the native negative, is what the ungated separation gives.
    #[test]
    fn burnt_out_green_leaves_the_plates_neutral_and_the_luminance_alone() {
        use crate::decode::{RawData, RawFrame, Rect, SensorKind};
        use crate::ingest::{ingest, IngestParams, WhiteBalance};
        use crate::plate::{mix_plate, PlateWeights};

        let (w, h) = (96usize, 96usize);
        let phase = BayerPhase::RGGB;
        let patch = |x: usize, y: usize| (36..60).contains(&x) && (36..60).contains(&y);
        // The field is a neutral 0.3 once balanced; the patch is at white.
        let raw: Vec<u16> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                if patch(x, y) {
                    1000
                } else {
                    match phase.color_at(x, y) {
                        Color::R => 150,
                        Color::G => 300,
                        Color::B => 200,
                    }
                }
            })
            .collect();
        let frame = RawFrame {
            make: String::new(),
            model: String::new(),
            clean_make: String::new(),
            clean_model: String::new(),
            width: w,
            height: h,
            bits: 12,
            kind: SensorKind::Bayer(phase),
            black_tile: [0.0; 4],
            white_rgb: [1000.0; 3],
            crop: Rect { x: 0, y: 0, w, h },
            orientation: 1,
            xyz_to_cam: None,
            illuminant: None,
            wb_as_shot: None,
            exposure: Default::default(),
            data: RawData::Integer(raw),
        };
        let wb = [2.0, 1.0, 1.5];
        let p = IngestParams { wb: WhiteBalance::Manual(wb), ..IngestParams::default() };
        let mosaic = ingest(&frame, &p).unwrap();
        assert!(mosaic.chroma_gate.is_some());
        let mut ungated = mosaic.clone();
        ungated.chroma_gate = None;

        for sp in [
            SeparateParams::default(),
            SeparateParams { mask: MaskMode::Off, ..Default::default() },
            SeparateParams { mask: MaskMode::Adaptive, ..Default::default() },
        ] {
            let gated = separate(&mosaic, &sp).unwrap();
            let plain = separate(&ungated, &sp).unwrap();
            assert_eq!(gated.lum.data, plain.lum.data, "{:?}: the luminance does not see the gate", sp.mask);
            // The mixes see it: the camera's channels straight into the plates.
            let [r, g, b] = mix_plate(&gated, &PlateWeights::channels());
            let [r0, g0, b0] = mix_plate(&plain, &PlateWeights::channels());
            for y in 40..56 {
                for x in 40..56 {
                    let l = gated.lum.at(x, y);
                    assert!((r.at(x, y) - l).abs() < 1e-9 && (g.at(x, y) - l).abs() < 1e-9, "{:?}", sp.mask);
                    assert!((b.at(x, y) - l).abs() < 1e-9, "{:?}", sp.mask);
                    // Without the gate the patch is magenta: red and blue over green.
                    assert!(
                        r0.at(x, y) > g0.at(x, y) + 0.5 && b0.at(x, y) > g0.at(x, y) + 0.2,
                        "{:?}",
                        sp.mask
                    );
                }
            }
            // At the centre, the luminance is the sum of the three clips.
            let l = (wb[0] + 2.0 * wb[1] + wb[2]) / 4.0;
            assert!((gated.lum.at(48, 48) - l).abs() < 0.05, "{:?}: {}", sp.mask, gated.lum.at(48, 48));
            // Far from the patch the field is neutral and measured either way.
            for (x, y) in [(8, 8), (8, 88), (88, 8), (88, 88)] {
                let v = (r.at(x, y), g.at(x, y), b.at(x, y));
                assert!((v.0 - 0.3).abs() < 1e-4 && (v.1 - 0.3).abs() < 1e-4, "{:?} {v:?}", sp.mask);
                assert!((v.2 - 0.3).abs() < 1e-4, "{:?} {v:?}", sp.mask);
                assert_eq!(r.at(x, y), r0.at(x, y));
            }
        }
    }

    /// A neutral low-frequency ramp is luminance only and must pass untouched.
    #[test]
    fn neutral_ramp_is_preserved() {
        let (w, h) = (256, 128);
        let k = LowpassSpec::default().kernel();
        let m = mosaic_of(BayerPhase::RGGB, w, h, |x, _| {
            let v = 0.2 + 0.6 * (x as f64 / w as f64);
            (v, v, v)
        });
        let (c1, mut c2, c2b) = chroma_estimates(&m, BayerPhase::RGGB, &k);
        average_in_place(&mut c2, &c2b);
        let lum = luminance(&m, BayerPhase::RGGB, &c1, &c2);
        for y in 40..h - 40 {
            for x in 40..w - 40 {
                assert!((lum.at(x, y) - m.at(x, y)).abs() < 1e-9);
            }
        }
    }
}
