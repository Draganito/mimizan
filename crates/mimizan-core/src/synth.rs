//! Synthetic scenes with ground truth: RMSE against the truth.

use crate::cfa::{BayerPhase, Color};
use crate::decode::Rect;
use crate::ingest::{CameraInfo, IngestReport, Mosaic, NoiseModel};
use crate::plane::Plane;
use rayon::prelude::*;
use serde::{Deserialize, Serialize};
use std::f64::consts::PI;

/// Three linear planes of the same size: a demosaiced or synthetic picture.
#[derive(Clone, Debug)]
pub struct Rgb {
    pub r: Plane,
    pub g: Plane,
    pub b: Plane,
}

impl Rgb {
    /// Black picture of `w × h`.
    pub fn new(w: usize, h: usize) -> Self {
        Self { r: Plane::zeros(w, h), g: Plane::zeros(w, h), b: Plane::zeros(w, h) }
    }
    pub fn width(&self) -> usize {
        self.r.width
    }
    pub fn height(&self) -> usize {
        self.r.height
    }
    /// Store one pixel.
    pub fn set(&mut self, x: usize, y: usize, r: f64, g: f64, b: f64) {
        self.r.set(x, y, r);
        self.g.set(x, y, g);
        self.b.set(x, y, b);
    }
    /// `L_true = (R + 2G + B)/4`.
    pub fn luminance(&self) -> Plane {
        let mut l = Plane::zeros(self.width(), self.height());
        l.data
            .par_iter_mut()
            .zip(&self.r.data)
            .zip(self.g.data.par_iter().zip(&self.b.data))
            .for_each(|((o, r), (g, b))| *o = (r + 2.0 * g + b) / 4.0);
        l
    }
    /// Multiply each channel by its gain.
    pub fn scale(&mut self, gr: f64, gg: f64, gb: f64) {
        self.r.data.par_iter_mut().for_each(|v| *v *= gr);
        self.g.data.par_iter_mut().for_each(|v| *v *= gg);
        self.b.data.par_iter_mut().for_each(|v| *v *= gb);
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Scene {
    /// Neutral Siemens star, 72 spokes.
    Star,
    /// Neutral zone plate.
    Zoneplate,
    /// Neutral slanted edge (5 degrees).
    Edge,
    /// 6x4 flat colour patches (ColorChecker-like chroma range).
    Patches,
    /// Neutral grey wedge, 16 steps over 6 stops, log spaced.
    Wedge,
    /// Coloured texture: red/green stripes at 0.12 c/px (inside the chroma band)
    /// over a luminance ramp.
    Fabric,
    /// Coloured stripes at 0.30 c/px: chroma beyond its own Nyquist. Nobody can
    /// reconstruct this; it measures how each estimator fails.
    Alias,
}

impl Scene {
    /// Scene from its lowercase name.
    pub fn parse(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().as_str() {
            "star" => Some(Self::Star),
            "zoneplate" => Some(Self::Zoneplate),
            "edge" => Some(Self::Edge),
            "patches" => Some(Self::Patches),
            "wedge" => Some(Self::Wedge),
            "fabric" => Some(Self::Fabric),
            "alias" => Some(Self::Alias),
            _ => None,
        }
    }
    /// Every scene, in the order of the bench table.
    pub const ALL: [Scene; 7] =
        [Self::Star, Self::Zoneplate, Self::Edge, Self::Patches, Self::Wedge, Self::Fabric, Self::Alias];

    /// Name as `parse` reads it.
    pub fn name(self) -> &'static str {
        match self {
            Self::Star => "star",
            Self::Zoneplate => "zoneplate",
            Self::Edge => "edge",
            Self::Patches => "patches",
            Self::Wedge => "wedge",
            Self::Fabric => "fabric",
            Self::Alias => "alias",
        }
    }
}

/// Area-averaged sampling of a function over the pixel (4x4 supersampling)
/// so synthetic edges carry a physical pixel aperture. Rows in parallel;
/// every pixel sums its 16 samples in the same order, so the result does
/// not depend on the thread count.
fn supersample(w: usize, h: usize, f: impl Fn(f64, f64) -> (f64, f64, f64) + Sync) -> Rgb {
    let mut rgb = Rgb::new(w, h);
    let s = 4usize;
    let inv = 1.0 / (s * s) as f64;
    if w == 0 {
        return rgb;
    }
    rgb.r
        .data
        .par_chunks_mut(w)
        .zip(rgb.g.data.par_chunks_mut(w))
        .zip(rgb.b.data.par_chunks_mut(w))
        .enumerate()
        .for_each(|(y, ((rr, gr), br))| {
            for x in 0..w {
                let (mut r, mut g, mut b) = (0.0, 0.0, 0.0);
                for sy in 0..s {
                    for sx in 0..s {
                        let px = x as f64 + (sx as f64 + 0.5) / s as f64;
                        let py = y as f64 + (sy as f64 + 0.5) / s as f64;
                        let (a, c, d) = f(px, py);
                        r += a;
                        g += c;
                        b += d;
                    }
                }
                rr[x] = r * inv;
                gr[x] = g * inv;
                br[x] = b * inv;
            }
        });
    rgb
}

/// Spokes of the Siemens star.
pub const STAR_SPOKES: usize = 72;

/// 24 ColorChecker-like reflectances in linear camera RGB (no white balance):
/// roughly the classic chart, scaled into [0.05, 0.9].
pub const PATCH_RGB: [[f64; 3]; 24] = [
    [0.17, 0.08, 0.05],
    [0.56, 0.30, 0.20],
    [0.11, 0.17, 0.30],
    [0.11, 0.13, 0.04],
    [0.26, 0.21, 0.42],
    [0.27, 0.52, 0.42],
    [0.78, 0.22, 0.03],
    [0.06, 0.08, 0.36],
    [0.60, 0.09, 0.11],
    [0.09, 0.04, 0.14],
    [0.37, 0.56, 0.06],
    [0.84, 0.42, 0.02],
    [0.02, 0.03, 0.30],
    [0.05, 0.28, 0.05],
    [0.49, 0.03, 0.03],
    [0.90, 0.68, 0.03],
    [0.55, 0.09, 0.30],
    [0.00, 0.21, 0.36],
    [0.90, 0.90, 0.90],
    [0.57, 0.57, 0.57],
    [0.35, 0.35, 0.35],
    [0.19, 0.19, 0.19],
    [0.09, 0.09, 0.09],
    [0.04, 0.04, 0.04],
];

/// The scene in linear RGB, before gains and noise. Any size works; the
/// patch and wedge scenes just get patches of at least one pixel.
pub fn render(scene: Scene, w: usize, h: usize) -> Rgb {
    let cx = w as f64 / 2.0;
    let cy = h as f64 / 2.0;
    match scene {
        Scene::Star => supersample(w, h, |x, y| {
            let dx = x - cx;
            let dy = y - cy;
            let r = (dx * dx + dy * dy).sqrt();
            let rmax = 0.46 * w.min(h) as f64;
            let v = if r > rmax || r < 2.0 {
                0.5
            } else {
                let th = dy.atan2(dx);
                0.5 + 0.4 * (STAR_SPOKES as f64 * th).cos().signum()
            };
            (v, v, v)
        }),
        Scene::Zoneplate => supersample(w, h, |x, y| {
            let dx = x - cx;
            let dy = y - cy;
            let r2 = dx * dx + dy * dy;
            // frequency reaches 0.5 c/px at the border
            let rmax = 0.5 * w.min(h) as f64;
            let k = 0.5 / rmax;
            let v = 0.5 + 0.4 * (PI * k * r2).cos();
            (v, v, v)
        }),
        Scene::Edge => supersample(w, h, |x, y| {
            let t = (5.0f64).to_radians();
            let d = (x - cx) * t.cos() + (y - cy) * t.sin();
            let v = if d < 0.0 { 0.2 } else { 0.8 };
            (v, v, v)
        }),
        Scene::Patches => {
            let mut rgb = Rgb::new(w, h);
            let cols = 6;
            let rows = 4;
            let pw = (w / cols).max(1);
            let ph = (h / rows).max(1);
            for y in 0..h {
                for x in 0..w {
                    let i = ((y / ph).min(rows - 1)) * cols + (x / pw).min(cols - 1);
                    let c = PATCH_RGB[i];
                    rgb.set(x, y, c[0], c[1], c[2]);
                }
            }
            rgb
        }
        Scene::Wedge => {
            let mut rgb = Rgb::new(w, h);
            let steps = 16usize;
            let pw = (w / steps).max(1);
            for y in 0..h {
                for x in 0..w {
                    let i = (x / pw).min(steps - 1);
                    let v = wedge_level(i, steps);
                    rgb.set(x, y, v, v, v);
                }
            }
            rgb
        }
        Scene::Fabric => supersample(w, h, |x, y| {
            let s = (2.0 * PI * 0.12 * (0.8 * x + 0.6 * y)).sin();
            let ramp = 0.3 + 0.3 * (x / w as f64);
            let r = ramp * (1.0 + 0.6 * s);
            let g = ramp * (1.0 - 0.6 * s);
            let b = ramp * 0.6;
            (r, g, b)
        }),
        Scene::Alias => supersample(w, h, |x, y| {
            let s = (2.0 * PI * 0.30 * (0.8 * x + 0.6 * y)).sin();
            let ramp = 0.3 + 0.3 * (x / w as f64);
            let r = ramp * (1.0 + 0.6 * s);
            let g = ramp * (1.0 - 0.6 * s);
            let b = ramp * 0.6;
            (r, g, b)
        }),
    }
}

/// 6 stops in `steps` log-spaced levels, top level 0.9.
pub fn wedge_level(i: usize, steps: usize) -> f64 {
    let stops = 6.0;
    0.9 * 2f64.powf(-stops * i as f64 / steps.saturating_sub(1).max(1) as f64)
}

/// Deterministic Gaussian generator (xorshift64* + Box-Muller).
pub struct Rng {
    state: u64,
}

impl Rng {
    /// Seeded generator; a zero seed is replaced by one (xorshift has no
    /// zero state).
    pub fn new(seed: u64) -> Self {
        Self { state: seed.max(1) }
    }
    /// Next raw 64-bit value.
    pub fn next_u64(&mut self) -> u64 {
        let mut x = self.state;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.state = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    /// Uniform in `[0, 1)` with 53 random bits.
    pub fn uniform(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 / (1u64 << 53) as f64
    }
    /// One draw per call. The Box-Muller pair is deliberately not reused:
    /// both members share the radius, so neighbouring pixels would have
    /// correlated noise power and the block periodograms would fluctuate far
    /// more than independent noise does.
    pub fn gaussian(&mut self) -> f64 {
        let u1 = self.uniform().max(1e-300);
        let u2 = self.uniform();
        let r = (-2.0 * u1.ln()).sqrt();
        r * (2.0 * PI * u2).cos()
    }
}

/// How a scene is turned into a mosaic.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct SynthParams {
    pub phase: BayerPhase,
    /// Raw channel gains applied before mosaicing: a neutral scene is not
    /// neutral in raw DN (no white balance in this pipeline). Must be
    /// positive and finite; the balance divides by them.
    pub gains: [f64; 3],
    /// Shot and read noise added to the mosaic; `None` is a noiseless sensor.
    pub noise: Option<NoiseModel>,
    /// Seed of the noise generator.
    pub seed: u64,
}

impl Default for SynthParams {
    fn default() -> Self {
        Self { phase: BayerPhase::RGGB, gains: [0.5, 1.0, 0.7], noise: Some(NoiseModel::default()), seed: 1 }
    }
}

/// A mosaiced scene with its truth.
pub struct Synthetic {
    /// What the pipeline sees.
    pub mosaic: Mosaic,
    /// The balanced scene.
    pub rgb: Rgb,
    /// Luminance of the balanced scene.
    pub l_true: Plane,
}

/// Mosaic a scene: apply gains, sample the CFA, add noise, undo the gains
/// exactly (the ideal channel balance), pack as `Mosaic`. The ground truth is
/// the luminance of the balanced scene.
pub fn mosaic(scene: Scene, w: usize, h: usize, p: &SynthParams) -> Synthetic {
    let mut rgb = render(scene, w, h);
    let l_true = rgb.luminance();
    rgb.scale(p.gains[0], p.gains[1], p.gains[2]);
    let mut m = Plane::zeros(w, h);
    let mut rng = Rng::new(p.seed);
    for y in 0..h {
        for x in 0..w {
            let v = match p.phase.color_at(x, y) {
                Color::R => rgb.r.at(x, y),
                Color::G => rgb.g.at(x, y),
                Color::B => rgb.b.at(x, y),
            };
            let n = match &p.noise {
                Some(nm) => nm.sigma(v) * rng.gaussian(),
                None => 0.0,
            };
            m.set(x, y, v + n);
        }
    }
    let noise = p.noise.unwrap_or(NoiseModel { a: 0.0, b: 1e-9 });
    let wb = [p.gains[1] / p.gains[0], 1.0, p.gains[1] / p.gains[2]];
    crate::ingest::apply_balance(&mut m, p.phase, wb);
    let mosaic = Mosaic {
        plane: m,
        phase: Some(p.phase),
        saturated: vec![0u8; w * h],
        chroma_gate: None,
        noise,
        wb,
        orientation: 1,
        camera: CameraInfo {
            make: "mimizan".into(),
            model: format!("synth {scene:?}"),
            clean_make: "mimizan".into(),
            clean_model: format!("synth_{scene:?}").to_ascii_lowercase(),
            bits: 16,
            exposure: Default::default(),
        },
        xyz_to_cam: None,
        report: IngestReport {
            crop: Rect { x: 0, y: 0, w, h },
            saturated_raw: 0,
            saturated_dilated: 0,
            saturated_green: 0,
            defects: 0,
            defects_repaired: false,
            defect_hot_rows: vec![],
            min_norm: 0.0,
            max_norm: 1.0,
            wb,
            wb_source: "exact".into(),
        },
    };
    // Hand back the balanced scene so callers compare like with like.
    rgb.scale(wb[0], wb[1], wb[2]);
    Synthetic { mosaic, rgb, l_true }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rng_is_roughly_gaussian() {
        let mut r = Rng::new(42);
        let n = 200_000;
        let (mut s, mut s2) = (0.0, 0.0);
        for _ in 0..n {
            let v = r.gaussian();
            s += v;
            s2 += v * v;
        }
        let mean = s / n as f64;
        let var = s2 / n as f64 - mean * mean;
        assert!(mean.abs() < 0.01, "{mean}");
        assert!((var - 1.0).abs() < 0.02, "{var}");
    }

    #[test]
    fn mosaic_without_noise_matches_truth_on_flat_patches() {
        let p = SynthParams { noise: None, ..Default::default() };
        let s = mosaic(Scene::Patches, 120, 80, &p);
        // centre of first patch (20x20): R at even/even; gains are undone exactly
        let v = s.mosaic.plane.at(10, 10);
        assert!((v - PATCH_RGB[0][0]).abs() < 1e-12);
    }
}
