//! Small pictures of RAW files for a folder browser: the mosaic binned to
//! the balanced channels, then mixed once to the grey of the development's
//! weights and once to the colour of the plate's taking filters. No
//! separation is needed, the binning removes the chrominance carrier. The
//! camera's embedded JPEG was measured as the alternative and rejected:
//! rawler decodes it in 250-430 ms on a Z f file, the mosaic in 340-420 ms,
//! and the JPEG carries the camera's tone curve, not ours.

use crate::cfa::Color;
use crate::decode::{RawFrame, SensorKind};
use crate::ingest::SAT_MARGIN;
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
    // Four planes: the balanced R, G, B means of each bin, and the mono
    // sensor's one channel in G.
    // The fourth marks bins with a burnt-out green photosite: there the
    // colour is false (towards magenta, `ingest::Mosaic::chroma_gate`)
    // and the plate shows the luminance, as the developed picture does.
    let mut chan = [Plane::zeros(ow, oh), Plane::zeros(ow, oh), Plane::zeros(ow, oh), Plane::zeros(ow, oh)];
    let (r, gbs) = chan.split_at_mut(1);
    let (g, bs) = gbs.split_at_mut(1);
    let (b, s) = bs.split_at_mut(1);
    r[0].data
        .par_chunks_mut(ow)
        .zip(g[0].data.par_chunks_mut(ow))
        .zip(b[0].data.par_chunks_mut(ow))
        .zip(s[0].data.par_chunks_mut(ow))
        .enumerate()
        .for_each(|(oy, (((rr, gg), bb), ss))| {
            for ox in 0..ow {
                let mut sum = [0.0f64; 3];
                let mut n = [0usize; 3];
                let mut green_burnt = false;
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
                        if ch == 1 && raw >= white * (1.0 - SAT_MARGIN) {
                            green_burnt = true;
                        }
                        sum[ch] += v;
                        n[ch] += 1;
                    }
                }
                let mean = |i: usize| if n[i] > 0 { wb[i] * sum[i] / n[i] as f64 } else { 0.0 };
                match frame.kind {
                    SensorKind::Bayer(_) => {
                        rr[ox] = mean(0);
                        gg[ox] = mean(1);
                        bb[ox] = mean(2);
                        ss[ox] = f64::from(u8::from(green_burnt));
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
    let [r, g, b, s] = [upright(&chan[0]), upright(&chan[1]), upright(&chan[2]), upright(&chan[3])];
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
        if s.data[i] != 0.0 {
            // Burnt-out green: no colour, the luminance in the grey and in
            // all three of the plate, as the separation's gate leaves them.
            let l = encode(0.25 * c[0] + 0.5 * c[1] + 0.25 * c[2]);
            *gy = l;
            px.fill(l);
            return;
        }
        *gy = encode((0..3).map(|k| w[k] * c[k]).sum::<f64>());
        for (out, row) in px.iter_mut().zip(plate.rows.iter()) {
            *out = encode((0..3).map(|k| row[k] * c[k]).sum::<f64>());
        }
    });
    Thumb { width, height, gray, rgb }
}

/// The grey picture alone, with the bare channels as its colour: the
/// older entry point, kept for callers that have no plate.
pub fn binned_gray(frame: &RawFrame, weights: Weights, wb: [f64; 3], long_edge: usize) -> Thumb {
    binned(frame, weights, &PlateWeights::channels(), wb, long_edge)
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
        // Grey is grey in colour too, and the colour has three bytes a pixel.
        assert_eq!(t.rgb.len(), 3 * t.gray.len());
        for (px, &g) in t.rgb.as_chunks::<3>().0.iter().zip(&t.gray) {
            assert!(px.iter().all(|&v| (v as i32 - g as i32).abs() <= 1), "{px:?} vs {g}");
        }
    }

    #[test]
    fn balance_and_weights_apply_per_colour() {
        // Red sites at full scale, the rest dark: the grey is w_r * wb_r,
        // and with the bare channels the colour is red alone.
        let f = frame(32, 32, |x, y| if x % 2 == 0 && y % 2 == 0 { 4195 } else { 100 }, 1);
        let t = binned_gray(&f, Weights { r: 0.5, g: 0.25, b: 0.25 }, [1.6, 1.0, 1.2], 8);
        let expect = ((0.5f64 * 1.6).min(1.0).powf(1.0 / 2.2) * 255.0).round() as u8;
        assert!(t.gray.iter().all(|&v| (v as i32 - expect as i32).abs() <= 1), "{:?}", &t.gray[..4]);
        for px in t.rgb.as_chunks::<3>().0 {
            assert_eq!(px[0], 255, "{px:?}");
            assert_eq!((px[1], px[2]), (0, 0), "{px:?}");
        }
    }

    /// A burnt-out bin under a daylight balance reads (k_R, 1, k_B), a
    /// magenta no scene had. The red-alone test above keeps its colour;
    /// here green is burnt out too, and the bin is grey in the plate: the
    /// luminance (k_R + 2 + k_B)/4, clipped to white at the encoding.
    #[test]
    fn burnt_out_green_bins_have_no_colour() {
        let wb = [2.0, 1.0, 1.5];
        let f = frame(32, 32, |_, _| 4195, 1);
        let t = binned_gray(&f, Weights::default(), wb, 8);
        for px in t.rgb.as_chunks::<3>().0 {
            assert_eq!(px, &[255, 255, 255], "{px:?}");
        }
        assert!(t.gray.iter().all(|&v| v == 255));
        // The same bins with the channels unbalanced show the magenta the
        // gate removes: white level but 1.0 everywhere → grey; so use a
        // half-burnt field: green at the clip, red and blue below it.
        let f = frame(32, 32, |x, y| if (x + y) % 2 == 1 { 4195 } else { 2200 }, 1);
        let t = binned_gray(&f, Weights::default(), wb, 8);
        for px in t.rgb.as_chunks::<3>().0 {
            assert_eq!(px[0], px[1], "{px:?}");
            assert_eq!(px[1], px[2], "{px:?}");
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
