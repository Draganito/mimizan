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
    /// Low-pass chrominance `Ĉ1'` of the mosaic held to its colour ceiling
    /// ([`Mosaic::colour_ceiling`]), mask gain already folded in.
    pub c1: Plane,
    /// Combined low-pass chrominance `Ĉ2'` of the mosaic held to its
    /// colour ceiling, mask gain already folded in.
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
/// The luminance is estimated from the mosaic as it is, with its full
/// chrominance subtracted. The chrominance handed on to the mixes is the
/// estimate of the mosaic held to its colour ceiling
/// ([`Mosaic::colour_ceiling`]): what a photosite reads above the level at
/// which the first channel burns out counts as light and not as colour.
/// A burnt-out neutral is then neutral, a channel that is still measured
/// beside burnt-out ones keeps its distance to them, and the false colour
/// of one clip against another never enters the estimate. `L̂` and the
/// native mix are untouched, and every mix has `L̂` as its luminance.
pub fn separate(mosaic: &Mosaic, p: &SeparateParams) -> crate::Result<Separation> {
    let phase = mosaic.phase.ok_or_else(|| crate::Error::Invalid("separate needs a Bayer mosaic".into()))?;
    let mut sep = separate_linear(mosaic, phase, p);
    if let Some(ceiling) = mosaic.colour_ceiling {
        let t = std::time::Instant::now();
        let tiles = chroma_below(mosaic, ceiling, phase, p, &mut sep);
        if tiles > 0 {
            tracing::debug!("chrominance below the ceiling: {tiles} tiles in {} ms", t.elapsed().as_millis());
        }
    }
    Ok(sep)
}

/// Side, in pixels, of the tiles [`chroma_below`] works in. A multiple of
/// the block size of the `adaptive` mask and of both margins.
const TILE: usize = 1024;

/// How far outside a tile the mosaic is read to estimate the chrominance
/// inside it. The `dubois` estimate at a pixel reads 16 pixels around it
/// (a 15-tap band and a Gaussian of radius 9 behind it), the fixed one 36
/// (73 taps); the `adaptive` one adds its 64-pixel blocks. Even, so a
/// window starts on the phase of the frame.
fn tile_margin(mask: MaskMode) -> usize {
    match mask {
        MaskMode::Dubois => 64,
        MaskMode::Off | MaskMode::Adaptive => 128,
    }
}

/// Replace the chrominance of `sep` by the estimate of the mosaic held to
/// `ceiling`: `min(m, ceiling)` through the same estimator. Returns the
/// number of tiles estimated again.
///
/// The two estimates differ only as far around a photosite above the
/// ceiling as the estimator reads, so the second one runs only on the
/// tiles that have such a photosite inside their margin, each on its own
/// window of the mosaic, one after the other. In the `dubois` and `off`
/// modes every filter is finite and shorter than the margin, so the
/// result is, to the bit, the chrominance of a separation of the whole
/// held mosaic; in the `adaptive` mode the block mask of a window is the
/// mask of the frame up to what its blocks see of their neighbours.
/// A tile's window is at most `TILE + 2·margin` on a side, so the pass
/// stays far under the peak of the first one on any frame above 3 MP; a
/// smaller frame is one window, and holds a second estimate beside the
/// first.
fn chroma_below(
    mosaic: &Mosaic,
    ceiling: f64,
    phase: BayerPhase,
    p: &SeparateParams,
    sep: &mut Separation,
) -> usize {
    let m = &mosaic.plane;
    let (w, h) = (m.width, m.height);
    let margin = tile_margin(p.mask);
    // Cells of `margin` pixels that hold a photosite above the ceiling.
    let (cw, ch) = (w.div_ceil(margin), h.div_ceil(margin));
    let mut cells = vec![false; cw * ch];
    cells.par_chunks_mut(cw).enumerate().for_each(|(cy, flags)| {
        for y in cy * margin..((cy + 1) * margin).min(h) {
            for (x, &v) in m.row(y).iter().enumerate() {
                if v > ceiling {
                    flags[x / margin] = true;
                }
            }
        }
    });
    if !cells.iter().any(|&c| c) {
        return 0;
    }

    let mut tiles = 0usize;
    for ty in (0..h).step_by(TILE) {
        for tx in (0..w).step_by(TILE) {
            // The window: the tile and its margin, cut at the frame.
            let (x0, y0) = (tx.saturating_sub(margin), ty.saturating_sub(margin));
            let (x1, y1) = ((tx + TILE + margin).min(w), (ty + TILE + margin).min(h));
            let above = (y0 / margin..y1.div_ceil(margin))
                .any(|cy| cells[cy * cw + x0 / margin..cy * cw + x1.div_ceil(margin)].iter().any(|&c| c));
            if !above {
                continue;
            }
            let window = held_window(mosaic, ceiling, x0, y0, x1 - x0, y1 - y0);
            let part = separate_linear(&window, phase, p);
            drop(window);
            let (tw, th) = ((tx + TILE).min(w) - tx, (ty + TILE).min(h) - ty);
            for (dst, src) in [(&mut sep.c1, &part.c1), (&mut sep.c2, &part.c2)] {
                for y in 0..th {
                    let from = &src.row(ty - y0 + y)[tx - x0..tx - x0 + tw];
                    dst.row_mut(ty + y)[tx..tx + tw].copy_from_slice(from);
                }
            }
            tiles += 1;
        }
    }
    tiles
}

/// The `w×h` window of `mosaic` at `(x0, y0)`, held to `ceiling`. The
/// origin is even, so the window has the phase of the frame.
fn held_window(mosaic: &Mosaic, ceiling: f64, x0: usize, y0: usize, w: usize, h: usize) -> Mosaic {
    debug_assert!(x0.is_multiple_of(2) && y0.is_multiple_of(2));
    let full = mosaic.plane.width;
    let mut plane = Plane::zeros(w, h);
    plane.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        for (o, &v) in row.iter_mut().zip(&mosaic.plane.row(y0 + y)[x0..x0 + w]) {
            *o = v.min(ceiling);
        }
    });
    let mut saturated = vec![0u8; w * h];
    saturated.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let at = (y0 + y) * full + x0;
        row.copy_from_slice(&mosaic.saturated[at..at + w]);
    });
    Mosaic {
        plane,
        phase: mosaic.phase,
        saturated,
        colour_ceiling: None,
        noise: mosaic.noise,
        wb: mosaic.wb,
        orientation: mosaic.orientation,
        camera: mosaic.camera.clone(),
        xyz_to_cam: mosaic.xyz_to_cam,
        report: mosaic.report.clone(),
    }
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

    /// A 12-bit frame with white at 1000 and black at 0, as-shot balance off.
    fn raw_frame(w: usize, h: usize, phase: BayerPhase, raw: Vec<u16>) -> crate::decode::RawFrame {
        use crate::decode::{RawData, RawFrame, Rect, SensorKind};
        RawFrame {
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
        }
    }

    const MODES: [MaskMode; 3] = [MaskMode::Dubois, MaskMode::Off, MaskMode::Adaptive];

    /// A neutral field with a burnt-out patch, balanced R 2, G 1, B 1.5:
    /// inside the patch the mosaic reads (2, 1, 1.5), a false magenta. Held
    /// to the ceiling the patch is neutral, so the three plates are equal
    /// there and equal to the luminance; the luminance itself, and so the
    /// native negative, is what the separation of the mosaic as it is
    /// gives. No colour is left around the patch either.
    #[test]
    fn a_burnt_out_neutral_has_no_colour_and_the_luminance_is_untouched() {
        use crate::ingest::{ingest, IngestParams, WhiteBalance};
        use crate::plate::{mix_plate, PlateWeights};

        let (w, h) = (192usize, 192usize);
        let phase = BayerPhase::RGGB;
        let patch = |x: usize, y: usize| (84..108).contains(&x) && (84..108).contains(&y);
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
        let wb = [2.0, 1.0, 1.5];
        let p = IngestParams { wb: WhiteBalance::Manual(wb), ..IngestParams::default() };
        let mosaic = ingest(&raw_frame(w, h, phase, raw), &p).unwrap();
        assert!(mosaic.colour_ceiling.is_some());
        let mut as_it_is = mosaic.clone();
        as_it_is.colour_ceiling = None;

        for mask in MODES {
            let sp = SeparateParams { mask, ..Default::default() };
            let held = separate(&mosaic, &sp).unwrap();
            let plain = separate(&as_it_is, &sp).unwrap();
            assert_eq!(held.lum.data, plain.lum.data, "{mask:?}: the luminance does not see the ceiling");
            // The mixes see it: the camera's channels straight into the plates.
            let [r, g, b] = mix_plate(&held, &PlateWeights::channels());
            let [r0, g0, b0] = mix_plate(&plain, &PlateWeights::channels());
            let (mut worst, mut least) = (0.0f64, f64::INFINITY);
            for y in 88..104 {
                for x in 88..104 {
                    let l = held.lum.at(x, y);
                    for v in [r.at(x, y), g.at(x, y), b.at(x, y)] {
                        worst = worst.max((v - l).abs());
                        least = least.min(v);
                    }
                    // As it is, the patch is magenta: red and blue over green.
                    assert!(r0.at(x, y) > g0.at(x, y) + 0.5 && b0.at(x, y) > g0.at(x, y) + 0.2, "{mask:?}");
                }
            }
            // What is left is what the estimator makes of any neutral
            // corner, a few hundredths four pixels inside it, and every
            // plate is over the range of the file: white.
            assert!(worst < 0.05 && least > 1.25, "{mask:?}: {worst} {least}");
            if mask == MaskMode::Dubois {
                let l = held.lum.at(96, 96);
                assert!((r.at(96, 96) - l).abs() < 1e-9 && (b.at(96, 96) - l).abs() < 1e-9);
            }
            // At the centre, the luminance is the sum of the three clips.
            let l = (wb[0] + 2.0 * wb[1] + wb[2]) / 4.0;
            assert!((held.lum.at(96, 96) - l).abs() < 0.05, "{mask:?}: {}", held.lum.at(96, 96));
            // The patch throws its false colour into the field around it,
            // as far as the chrominance low-pass reaches; held to the
            // ceiling it is a neutral step and throws none. The pixel-
            // adaptive estimate keeps a neutral step neutral; the fixed
            // low-pass leaks a step's luminance into the colour, the same
            // on any edge, and far less than the clip did.
            let fringe = |r: &Plane, g: &Plane, b: &Plane| {
                let mut worst = 0.0f64;
                for y in 90..102 {
                    for x in 108..116 {
                        worst =
                            worst.max((r.at(x, y) - g.at(x, y)).abs()).max((b.at(x, y) - g.at(x, y)).abs());
                    }
                }
                worst
            };
            let (now, before) = (fringe(&r, &g, &b), fringe(&r0, &g0, &b0));
            assert!(before > 0.2, "{mask:?}: {before}");
            match mask {
                MaskMode::Dubois => assert!(now < 0.005, "{mask:?}: {now}"),
                _ => assert!(now < 0.4 * before, "{mask:?}: {now} against {before}"),
            }
            // Far from the patch the field is neutral and measured either way.
            for (x, y) in [(8, 8), (8, 184), (184, 8), (184, 184)] {
                let v = (r.at(x, y), g.at(x, y), b.at(x, y));
                assert!((v.0 - 0.3).abs() < 1e-4 && (v.1 - 0.3).abs() < 1e-4, "{mask:?} {v:?}");
                assert!((v.2 - 0.3).abs() < 1e-4, "{mask:?} {v:?}");
                if mask != MaskMode::Adaptive {
                    assert_eq!(r.at(x, y), r0.at(x, y), "{mask:?}");
                }
            }
        }
    }

    /// A field where red and green are burnt out and blue is measured, far
    /// below them: the sky around a low sun. The blue plate keeps its
    /// distance to the other two, where a gate on burnt-out green made all
    /// three the luminance and the field a white hole. The three plates
    /// still add up to the luminance of the monochrome negative.
    #[test]
    fn a_measured_channel_keeps_its_colour_beside_burnt_out_ones() {
        use crate::ingest::{ingest, IngestParams, WhiteBalance};
        use crate::plate::{mix_plate, PlateWeights};

        let (w, h) = (128usize, 128usize);
        let phase = BayerPhase::RGGB;
        // Red and green at white, blue at 200: balanced (2, 1, 0.3).
        let raw: Vec<u16> =
            (0..w * h).map(|i| if phase.color_at(i % w, i / w) == Color::B { 200 } else { 1000 }).collect();
        let p = IngestParams { wb: WhiteBalance::Manual([2.0, 1.0, 1.5]), ..IngestParams::default() };
        let mosaic = ingest(&raw_frame(w, h, phase, raw), &p).unwrap();
        let ceiling = mosaic.colour_ceiling.unwrap();
        for mask in MODES {
            let sep = separate(&mosaic, &SeparateParams { mask, ..Default::default() }).unwrap();
            let [r, g, b] = mix_plate(&sep, &PlateWeights::channels());
            // The luminance is the mosaic's own: (2 + 2·1 + 0.3) / 4.
            let l = (2.0 + 2.0 + 0.3) / 4.0;
            // The colour is that of (ceiling, ceiling, 0.3).
            let lc = (3.0 * ceiling + 0.3) / 4.0;
            for (x, y) in [(64, 64), (50, 77), (81, 40)] {
                assert!((sep.lum.at(x, y) - l).abs() < 1e-9, "{mask:?}");
                assert!((r.at(x, y) - (l + ceiling - lc)).abs() < 1e-9, "{mask:?} {}", r.at(x, y));
                assert!((g.at(x, y) - (l + ceiling - lc)).abs() < 1e-9, "{mask:?} {}", g.at(x, y));
                assert!((b.at(x, y) - (l + 0.3 - lc)).abs() < 1e-9, "{mask:?} {}", b.at(x, y));
                // Red and green are over the file's range, blue far inside it.
                assert!(r.at(x, y) > 1.0 && g.at(x, y) > 1.0 && b.at(x, y) < 0.6, "{mask:?}");
                let sum = 0.25 * r.at(x, y) + 0.5 * g.at(x, y) + 0.25 * b.at(x, y);
                assert!((sum - sep.lum.at(x, y)).abs() < 1e-12, "{mask:?}");
            }
        }
    }

    /// The chrominance is estimated again only on the tiles that hold a
    /// photosite above the ceiling, each on its own window. On a frame of
    /// several tiles, with such photosites across a seam, in a corner and
    /// nowhere in one tile, the result is the chrominance of the whole
    /// mosaic held to the ceiling: to the bit where every filter is
    /// finite, closely under the block mask. The luminance is that of the
    /// mosaic as it is.
    #[test]
    fn the_tiles_give_the_chrominance_of_the_held_mosaic() {
        use crate::ingest::{ingest, IngestParams, WhiteBalance};

        let (w, h) = (2 * TILE + 90, TILE + 150);
        let phase = BayerPhase::GRBG;
        // A coloured texture of a few hundred counts, the same on any run.
        let mut state = 0x2545_f491_4f6c_dd1du64;
        let mut noise = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            (state >> 40) as f64 / (1u64 << 24) as f64
        };
        let bright = |x: usize, y: usize| {
            // Across the seam at x = TILE, in the last corner, and a
            // speck a few pixels inside the second tile's margin.
            ((TILE - 20..TILE + 30).contains(&x) && (300..360).contains(&y))
                || (x >= w - 12 && y >= h - 9)
                || ((TILE - 40..TILE - 36).contains(&x) && (700..704).contains(&y))
        };
        let raw: Vec<u16> = (0..w * h)
            .map(|i| {
                let (x, y) = (i % w, i / w);
                let wave = 0.5 + 0.5 * ((x as f64 * 0.045).sin() * (y as f64 * 0.031).cos());
                let level = match phase.color_at(x, y) {
                    Color::R => 120.0 + 200.0 * wave,
                    Color::G => 260.0 + 300.0 * wave,
                    Color::B => 90.0 + 160.0 * (1.0 - wave),
                } + 12.0 * noise();
                if bright(x, y) {
                    // Red and green at white, blue as the scene has it.
                    match phase.color_at(x, y) {
                        Color::B => (2.5 * level) as u16,
                        _ => 1000,
                    }
                } else {
                    level as u16
                }
            })
            .collect();
        let p = IngestParams { wb: WhiteBalance::Manual([1.9, 1.0, 1.6]), ..IngestParams::default() };
        let mosaic = ingest(&raw_frame(w, h, phase, raw), &p).unwrap();
        let ceiling = mosaic.colour_ceiling.unwrap();
        assert!(mosaic.report.above_ceiling > 1000);
        let mut as_it_is = mosaic.clone();
        as_it_is.colour_ceiling = None;
        let mut held = as_it_is.clone();
        held.plane.data.iter_mut().for_each(|v| *v = v.min(ceiling));

        for mask in MODES {
            let sp = SeparateParams { mask, ..Default::default() };
            let sep = separate(&mosaic, &sp).unwrap();
            let plain = separate(&as_it_is, &sp).unwrap();
            let whole = separate(&held, &sp).unwrap();
            assert_eq!(sep.lum.data, plain.lum.data, "{mask:?}: luminance");
            // Of the six tiles, the two at the seam and the one with the
            // corner hold a bright photosite; the wider margin of the
            // longer filters reaches the corner from its neighbour too.
            let tiles = chroma_below(&mosaic, ceiling, phase, &sp, &mut plain.clone());
            assert_eq!(tiles, if mask == MaskMode::Dubois { 3 } else { 4 }, "{mask:?}");
            if mask == MaskMode::Adaptive {
                let worst = |a: &Plane, b: &Plane| {
                    a.data.iter().zip(&b.data).map(|(a, b)| (a - b).abs()).fold(0.0f64, f64::max)
                };
                let (d1, d2) = (worst(&sep.c1, &whole.c1), worst(&sep.c2, &whole.c2));
                assert!(d1 < 1e-3 && d2 < 1e-3, "{mask:?}: {d1} {d2}");
            } else {
                assert!(sep.c1.data == whole.c1.data, "{mask:?}: C1 is not the held mosaic's");
                assert!(sep.c2.data == whole.c2.data, "{mask:?}: C2 is not the held mosaic's");
            }
            // And it is not the chrominance of the mosaic as it is.
            assert!(sep.c1.data != plain.c1.data, "{mask:?}");
        }
    }

    /// A mosaic with nothing above its ceiling separates as one without a
    /// ceiling, to the bit: the second estimate does not run.
    #[test]
    fn a_mosaic_below_its_ceiling_is_separated_once() {
        let (w, h) = (160usize, 96usize);
        let phase = BayerPhase::BGGR;
        let plane = mosaic_of(phase, w, h, |x, y| {
            let v = 0.2 + 0.5 * (x as f64 / w as f64);
            (v, 0.9 * v + 0.1 * (y as f64 / h as f64), 0.4)
        });
        let raw: Vec<u16> = plane.data.iter().map(|v| (v * 1000.0) as u16).collect();
        let p = crate::ingest::IngestParams { wb: crate::ingest::WhiteBalance::None, ..Default::default() };
        let mosaic = crate::ingest::ingest(&raw_frame(w, h, phase, raw), &p).unwrap();
        assert_eq!(mosaic.report.above_ceiling, 0);
        let mut as_it_is = mosaic.clone();
        as_it_is.colour_ceiling = None;
        for mask in MODES {
            let sp = SeparateParams { mask, ..Default::default() };
            let (a, b) = (separate(&mosaic, &sp).unwrap(), separate(&as_it_is, &sp).unwrap());
            assert!(a.lum.data == b.lum.data && a.c1.data == b.c1.data && a.c2.data == b.c2.data, "{mask:?}");
            assert_eq!(chroma_below(&mosaic, mosaic.colour_ceiling.unwrap(), phase, &sp, &mut a.clone()), 0);
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
