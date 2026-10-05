//! Small grey pictures of RAW files for a folder browser: the mosaic
//! binned to a grey picture with the development's balance and mix
//! weights. No separation is needed, the binning removes the chrominance
//! carrier. The camera's embedded JPEG was measured as the alternative and
//! rejected: rawler decodes it in 250-430 ms on a Z f file, the mosaic in
//! 340-420 ms, and the JPEG carries the camera's tone curve, not ours.

use crate::cfa::Color;
use crate::decode::{RawFrame, SensorKind};
use crate::mix::Weights;
use crate::plane::Plane;
use crate::print::orient;
use rayon::prelude::*;

/// 8-bit grey, gamma 2.2, upright (the file's orientation applied).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumb {
    pub width: usize,
    pub height: usize,
    pub gray: Vec<u8>,
}

impl Thumb {
    fn from_plane(p: &Plane, orientation: u16) -> Self {
        let p = if (2..=8).contains(&orientation) { orient(p, orientation) } else { p.clone() };
        let gray = p.data.iter().map(|v| (v.clamp(0.0, 1.0).powf(1.0 / 2.2) * 255.0).round() as u8).collect();
        Self { width: p.width, height: p.height, gray }
    }
}

/// The mosaic inside its crop, binned by an even factor so the long edge
/// is at most `long_edge`. Each bin averages its samples per colour after
/// black and white levels, applies `wb` (G = 1) and mixes with `weights`
/// (balanced space); a monochrome sensor averages everything. Values above
/// 1 are clipped, like the negative's saturated pixels.
pub fn binned_gray(frame: &RawFrame, weights: Weights, wb: [f64; 3], long_edge: usize) -> Thumb {
    let c = frame.crop;
    let long = c.w.max(c.h).max(1);
    let mut f = (long as f64 / long_edge.max(1) as f64).ceil().max(2.0) as usize;
    if f % 2 == 1 {
        f += 1;
    }
    let ow = (c.w / f).max(1);
    let oh = (c.h / f).max(1);
    let w = [weights.r, weights.g, weights.b];
    let mut out = Plane::zeros(ow, oh);
    out.data.par_chunks_mut(ow).enumerate().for_each(|(oy, row)| {
        for (ox, px) in row.iter_mut().enumerate() {
            let mut sum = [0.0f64; 3];
            let mut n = [0usize; 3];
            let y0 = c.y + oy * f;
            let x0 = c.x + ox * f;
            for y in y0..(y0 + f).min(c.y + c.h) {
                for x in x0..(x0 + f).min(c.x + c.w) {
                    let black = frame.black_at(x, y);
                    let white = frame.white_at(x, y);
                    let v =
                        ((frame.data.get(y * frame.width + x) - black) / (white - black).max(1.0)).max(0.0);
                    let ch = match frame.kind {
                        SensorKind::Bayer(ph) => match ph.color_at(x, y) {
                            Color::R => 0,
                            Color::G => 1,
                            Color::B => 2,
                        },
                        SensorKind::Mono => 1,
                    };
                    sum[ch] += v;
                    n[ch] += 1;
                }
            }
            *px = match frame.kind {
                SensorKind::Bayer(_) => (0..3)
                    .filter(|&i| n[i] > 0)
                    .map(|i| w[i] * wb[i] * sum[i] / n[i] as f64)
                    .sum::<f64>()
                    .min(1.0),
                SensorKind::Mono => {
                    if n[1] > 0 {
                        (sum[1] / n[1] as f64).min(1.0)
                    } else {
                        0.0
                    }
                }
            };
        }
    });
    Thumb::from_plane(&out, frame.orientation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cfa::BayerPhase;
    use crate::decode::{Exposure, RawData, Rect};

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
    }

    #[test]
    fn balance_and_weights_apply_per_colour() {
        // Red sites at full scale, the rest dark: the grey is w_r * wb_r.
        let f = frame(32, 32, |x, y| if x % 2 == 0 && y % 2 == 0 { 4195 } else { 100 }, 1);
        let t = binned_gray(&f, Weights { r: 0.5, g: 0.25, b: 0.25 }, [1.6, 1.0, 1.2], 8);
        let expect = ((0.5f64 * 1.6).min(1.0).powf(1.0 / 2.2) * 255.0).round() as u8;
        assert!(t.gray.iter().all(|&v| (v as i32 - expect as i32).abs() <= 1), "{:?}", &t.gray[..4]);
    }

    #[test]
    fn orientation_turns_the_picture() {
        let f = frame(64, 32, |_, _| 100 + 737, 6);
        let t = binned_gray(&f, Weights::default(), [1.0; 3], 16);
        assert_eq!((t.width, t.height), (8, 16));
    }
}
