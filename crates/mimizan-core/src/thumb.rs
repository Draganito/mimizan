//! Small pictures of RAW files for a folder browser: the mosaic binned to
//! the balanced channels, then mixed once to the grey of the development's
//! weights and once to the colour of the plate's taking filters. No
//! separation is needed, the binning removes the chrominance carrier. The
//! camera's embedded JPEG was measured as the alternative and rejected:
//! rawler decodes it in 250-430 ms on a Z f file, the mosaic in 340-420 ms,
//! and the JPEG carries the camera's tone curve, not ours.

use crate::cfa::Color;
use crate::decode::{RawFrame, SensorKind};
use crate::ingest::{colour_ceiling, SAT_MARGIN};
use crate::mix::Weights;
use crate::plane::Plane;
use crate::plate::{PlateSpace, PlateWeights};
use crate::print::orient;
use rayon::prelude::*;

/// 8-bit, gamma 2.2, upright (the file's orientation applied): the grey
/// mix and, interleaved R G B, the plate.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumb {
    pub width: usize,
    pub height: usize,
    pub gray: Vec<u8>,
    /// The colour plate of the same bins, `3 * width * height` bytes.
    pub rgb: Vec<u8>,
}

fn encode(v: f64) -> u8 {
    (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8
}

/// The taking filters a browser shows: the camera's matrix towards sRGB
/// (the screen's primaries), the bare channels when the file has none.
pub fn browser_plate(frame: &RawFrame) -> PlateWeights {
    frame
        .xyz_to_cam
        .as_ref()
        .and_then(|m| PlateWeights::camera(m, PlateSpace::Srgb).ok())
        .unwrap_or_else(PlateWeights::channels)
}

/// The mosaic inside its crop, binned by an even factor so the long edge
/// is at most `long_edge`. Each bin averages its samples per colour after
/// black and white levels and applies `wb` (G = 1); the grey is the mix
/// with `weights` (balanced space), the colour the three rows of `plate`.
/// A monochrome sensor averages everything and is grey in both. Values
/// above 1 are clipped, like the negative's saturated pixels.
///
/// A bin with a photosite above the colour ceiling
/// (`ingest::colour_ceiling`) is mixed as the developed picture is: its
/// luminance from the readings as they are, its colour from the readings
/// held to the ceiling.
pub fn binned(
    frame: &RawFrame,
    weights: Weights,
    plate: &PlateWeights,
    wb: [f64; 3],
    long_edge: usize,
) -> Thumb {
    let c = frame.crop;
    let long = c.w.max(c.h).max(1);
    let mut f = (long as f64 / long_edge.max(1) as f64).ceil().max(2.0) as usize;
    if f % 2 == 1 {
        f += 1;
    }
    let ow = (c.w / f).max(1);
    let oh = (c.h / f).max(1);
    let ceiling = colour_ceiling(frame, wb, SAT_MARGIN).unwrap_or(f64::INFINITY);
    // Three planes: the balanced R, G, B of each bin as the mixes take
    // them, and the mono sensor's one channel in all three.
    let mut chan = [Plane::zeros(ow, oh), Plane::zeros(ow, oh), Plane::zeros(ow, oh)];
    let (r, gb) = chan.split_at_mut(1);
    let (g, b) = gb.split_at_mut(1);
    r[0].data
        .par_chunks_mut(ow)
        .zip(g[0].data.par_chunks_mut(ow))
        .zip(b[0].data.par_chunks_mut(ow))
        .enumerate()
        .for_each(|(oy, ((rr, gg), bb))| {
            for ox in 0..ow {
                let mut sum = [0.0f64; 3];
                let mut held = [0.0f64; 3];
                let mut n = [0usize; 3];
                let mut above = false;
                let y0 = c.y + oy * f;
                let x0 = c.x + ox * f;
                for y in y0..(y0 + f).min(c.y + c.h) {
                    for x in x0..(x0 + f).min(c.x + c.w) {
                        let black = frame.black_at(x, y);
                        let white = frame.white_at(x, y);
                        let raw = frame.data.get(y * frame.width + x);
                        let v = ((raw - black) / (white - black).max(1.0)).max(0.0);
                        let ch = match frame.kind {
                            SensorKind::Bayer(ph) => match ph.color_at(x, y) {
                                Color::R => 0,
                                Color::G => 1,
                                Color::B => 2,
                            },
                            SensorKind::Mono => 1,
                        };
                        sum[ch] += v;
                        held[ch] += (wb[ch] * v).min(ceiling);
                        above |= wb[ch] * v > ceiling;
                        n[ch] += 1;
                    }
                }
                let mean = |i: usize| if n[i] > 0 { wb[i] * sum[i] / n[i] as f64 } else { 0.0 };
                match frame.kind {
                    SensorKind::Bayer(_) if above => {
                        // The luminance of the readings, the colour of the
                        // readings held to the ceiling.
                        let h = |i: usize| if n[i] > 0 { held[i] / n[i] as f64 } else { 0.0 };
                        let l = 0.25 * mean(0) + 0.5 * mean(1) + 0.25 * mean(2);
                        let lh = 0.25 * h(0) + 0.5 * h(1) + 0.25 * h(2);
                        rr[ox] = l + h(0) - lh;
                        gg[ox] = l + h(1) - lh;
                        bb[ox] = l + h(2) - lh;
                    }
                    SensorKind::Bayer(_) => {
                        rr[ox] = mean(0);
                        gg[ox] = mean(1);
                        bb[ox] = mean(2);
                    }
                    SensorKind::Mono => {
                        let v = if n[1] > 0 { sum[1] / n[1] as f64 } else { 0.0 };
                        rr[ox] = v;
                        gg[ox] = v;
                        bb[ox] = v;
                    }
                }
            }
        });
    let upright = |p: &Plane| {
        if (2..=8).contains(&frame.orientation) {
            orient(p, frame.orientation)
        } else {
            p.clone()
        }
    };
    let [r, g, b] = [upright(&chan[0]), upright(&chan[1]), upright(&chan[2])];
    let (width, height) = (r.width, r.height);
    let n = width * height;
    let mono = matches!(frame.kind, SensorKind::Mono);
    let w = [weights.r, weights.g, weights.b];
    let mut gray = vec![0u8; n];
    let mut rgb = vec![0u8; 3 * n];
    gray.par_iter_mut().zip(rgb.par_chunks_mut(3)).enumerate().for_each(|(i, (gy, px))| {
        let c = [r.data[i], g.data[i], b.data[i]];
        if mono {
            let v = encode(c[1]);
            *gy = v;
            px.fill(v);
            return;
        }
        *gy = encode((0..3).map(|k| w[k] * c[k]).sum::<f64>());
        for (out, row) in px.iter_mut().zip(plate.rows.iter()) {
            *out = encode((0..3).map(|k| row[k] * c[k]).sum::<f64>());
        }
    });
    Thumb { width, height, gray, rgb }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfa::BayerPhase;
    use crate::decode::{Exposure, RawData, Rect};

    /// The grey picture alone, with the bare channels as its colour.
    fn binned_gray(frame: &RawFrame, weights: Weights, wb: [f64; 3], long_edge: usize) -> Thumb {
        binned(frame, weights, &PlateWeights::channels(), wb, long_edge)
    }

    fn frame(w: usize, h: usize, fill: impl Fn(usize, usize) -> u16, orientation: u16) -> RawFrame {
        let mut data = vec![0u16; w * h];
        for y in 0..h {
            for x in 0..w {
                data[y * w + x] = fill(x, y);
            }
        }
        RawFrame {
            make: "t".into(),
            model: "t".into(),
            clean_make: "t".into(),
            clean_model: "t".into(),
            width: w,
            height: h,
            bits: 12,
            kind: SensorKind::Bayer(BayerPhase::RGGB),
            black_tile: [100.0; 4],
            white_rgb: [4195.0; 3],
            crop: Rect { x: 0, y: 0, w, h },
            orientation,
            xyz_to_cam: None,
            illuminant: None,
            wb_as_shot: None,
            exposure: Exposure::default(),
            data: RawData::Integer(data),
        }
    }

    #[test]
    fn neutral_gray_is_the_gray_value() {
        // 18 % grey on every photosite: 100 + 0.18 * 4095.
        let f = frame(64, 48, |_, _| 100 + 737, 1);
        let t = binned_gray(&f, Weights::default(), [1.0; 3], 16);
        assert_eq!((t.width, t.height), (16, 12));
        let expect = (0.18f64.powf(1.0 / 2.2) * 255.0).round() as u8;
        for &v in &t.gray {
            assert!((v as i32 - expect as i32).abs() <= 1, "{v} vs {expect}");
        }
        // Grey is grey in colour too, and the colour has three bytes a pixel.
        assert_eq!(t.rgb.len(), 3 * t.gray.len());
        for (px, &g) in t.rgb.as_chunks::<3>().0.iter().zip(&t.gray) {
            assert!(px.iter().all(|&v| (v as i32 - g as i32).abs() <= 1), "{px:?} vs {g}");
        }
    }

    #[test]
    fn balance_and_weights_apply_per_colour() {
        // Red sites at half scale, the rest dark: the grey is w_r * wb_r / 2,
        // and with the bare channels the colour is red alone. (At full
        // scale red would stand above the colour ceiling; that case is the
        // next test.)
        let f = frame(32, 32, |x, y| if x % 2 == 0 && y % 2 == 0 { 100 + 2048 } else { 100 }, 1);
        let t = binned_gray(&f, Weights { r: 0.5, g: 0.25, b: 0.25 }, [1.6, 1.0, 1.2], 8);
        let red = 1.6f64 * 2048.0 / 4095.0;
        let enc = |v: f64| (v.powf(1.0 / 2.2) * 255.0).round() as i32;
        assert!(t.gray.iter().all(|&v| (v as i32 - enc(0.5 * red)).abs() <= 1), "{:?}", &t.gray[..4]);
        for px in t.rgb.as_chunks::<3>().0 {
            assert!((px[0] as i32 - enc(red)).abs() <= 1, "{px:?}");
            assert_eq!((px[1], px[2]), (0, 0), "{px:?}");
        }
    }

    /// A burnt-out bin under a daylight balance reads (k_R, 1, k_B), a
    /// magenta no scene had. Held to the ceiling it is neutral, and the
    /// bin is grey in the plate: the luminance (k_R + 2 + k_B)/4, clipped
    /// to white at the encoding. Where green and red are over the ceiling
    /// and blue is measured below it, blue keeps its distance.
    #[test]
    fn above_the_ceiling_a_bin_keeps_only_the_colour_that_is_measured() {
        let wb = [2.0, 1.0, 1.5];
        let f = frame(32, 32, |_, _| 4195, 1);
        let t = binned_gray(&f, Weights::default(), wb, 8);
        for px in t.rgb.as_chunks::<3>().0 {
            assert_eq!(px, &[255, 255, 255], "{px:?}");
        }
        assert!(t.gray.iter().all(|&v| v == 255));
        // Green at the clip, red balanced just over it, blue at 0.77: the
        // ceiling is (4195·0.98 − 100)/4095 = 0.9795, so red and green are
        // held to it and are equal, and blue stays below. The luminance is
        // the readings' own, (1.026 + 2 + 0.769)/4.
        let f = frame(32, 32, |x, y| if (x + y) % 2 == 1 { 4195 } else { 2200 }, 1);
        let t = binned_gray(&f, Weights::default(), wb, 8);
        let (l, lh): (f64, f64) = ((1.0256 + 2.0 + 0.7692) / 4.0, (3.0 * 0.9795 + 0.7692) / 4.0);
        let blue = ((l + 0.7692 - lh).powf(1.0 / 2.2) * 255.0).round() as i32;
        for px in t.rgb.as_chunks::<3>().0 {
            assert_eq!((px[0], px[1]), (255, 255), "{px:?}");
            assert!((px[2] as i32 - blue).abs() <= 1 && px[2] < 235, "{px:?} vs {blue}");
        }
        let grey = (l.powf(1.0 / 2.2) * 255.0).round() as i32;
        assert!(t.gray.iter().all(|&v| (v as i32 - grey).abs() <= 1), "{:?} vs {grey}", &t.gray[..4]);
        // A bin with nothing above the ceiling is mixed from its means.
        let f = frame(32, 32, |x, y| if (x + y) % 2 == 1 { 3000 } else { 1500 }, 1);
        let t = binned_gray(&f, Weights::default(), wb, 8);
        let (r, g, b) = (2.0 * 1400.0 / 4095.0, 2900.0 / 4095.0, 1.5 * 1400.0 / 4095.0);
        let enc = |v: f64| (v.powf(1.0 / 2.2) * 255.0).round() as i32;
        for px in t.rgb.as_chunks::<3>().0 {
            assert!((px[0] as i32 - enc(r)).abs() <= 1 && (px[1] as i32 - enc(g)).abs() <= 1, "{px:?}");
            assert!((px[2] as i32 - enc(b)).abs() <= 1, "{px:?}");
        }
    }

    #[test]
    fn orientation_turns_the_picture() {
        let f = frame(64, 32, |_, _| 100 + 737, 6);
        let t = binned_gray(&f, Weights::default(), [1.0; 3], 16);
        assert_eq!((t.width, t.height), (8, 16));
        assert_eq!(t.rgb.len(), 3 * 8 * 16);
    }

    #[test]
    fn browser_plate_takes_the_matrix_when_there_is_one() {
        let mut f = frame(8, 8, |_, _| 100 + 737, 1);
        assert_eq!(browser_plate(&f), PlateWeights::channels());
        // The identity matrix is a camera whose channels are XYZ; it has a
        // valid inverse, so the camera set is taken.
        f.xyz_to_cam = Some([[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]);
        assert_eq!(browser_plate(&f).filters, crate::plate::PlateFilters::Camera);
    }
}
