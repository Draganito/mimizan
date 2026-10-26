//! The three-colour plate: Prokudin-Gorsky's principle on a Bayer mosaic.
//!
//! Prokudin-Gorsky exposed one glass plate three times, through a blue, a
//! green and a red filter, copied the three negatives together onto one
//! positive plate, and projected the three positives through the same
//! filters so the light added up on the screen. Here the three exposures
//! are three mixes of one separation: every plate is `L̂ + k1·Ĉ1' + k2·Ĉ2'`,
//! the same full-resolution luminance with a different band-limited
//! chrominance added, and each pixel of each plate is a weighted sum of
//! photosites the sensor measured. Nothing is interpolated, and the three
//! plates are in register by construction: one exposure, one lattice.
//!
//! The "taking filters" are three weight triples on the balanced channels,
//! each summing to one so that a grey subject gives three equal plates:
//!
//! * [`PlateFilters::Camera`]: the rows of the camera's colour matrix,
//!   mapped to the output primaries. Colorimetric; the weights may be
//!   negative. What colour film does with its orange mask.
//! * [`PlateFilters::Wratten`]: the panchromatic base weights behind the
//!   red 25, green 58 and blue 47 glass of `mix.rs`, crosstalk included.
//!   The softer palette of the historical filters.
//! * [`PlateFilters::Channels`]: the balanced sensor channels as they are,
//!   which is how the Library of Congress composites the plates: each
//!   frame straight into one channel.
//!
//! The plate file is one 16-bit grey TIFF with the three frames stacked,
//! blue on top, green, red at the bottom, the order of the originals. The
//! print stage copies the three frames through the same paper with the
//! same two exposures, and the three positives become the three channels
//! of an RGB file whose primaries are the projection filters.

use crate::error::{Error, Result};
use crate::mix::{mix, ColorFilter, Weights};
use crate::plane::Plane;
use crate::separate::Separation;
use serde::{Deserialize, Serialize};

/// How the three plates are derived from the balanced channels.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlateFilters {
    /// The camera's colour matrix towards the output primaries (default).
    /// Falls back to `Channels` when the file carries no matrix.
    #[default]
    Camera,
    /// Wratten 25 / 58 / 47 in front of the panchromatic mix.
    Wratten,
    /// The balanced channels as they are.
    Channels,
}

impl PlateFilters {
    pub const ALL: [PlateFilters; 3] = [PlateFilters::Camera, PlateFilters::Wratten, PlateFilters::Channels];

    pub fn name(self) -> &'static str {
        match self {
            PlateFilters::Camera => "camera",
            PlateFilters::Wratten => "wratten",
            PlateFilters::Channels => "channels",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let k = s.trim().to_ascii_lowercase();
        Self::ALL.iter().copied().find(|f| k == f.name())
    }
}

/// The primaries the three positives are projected through: the RGB
/// colour space of the print file.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum PlateSpace {
    /// Adobe RGB (1998): its transfer curve is exactly the gamma 2.2 of
    /// the paper's encoding, and its gamut holds the saturated filter
    /// primaries better than sRGB.
    #[default]
    AdobeRgb,
    /// sRGB primaries (with the paper's gamma 2.2, not the sRGB curve).
    Srgb,
}

impl PlateSpace {
    pub const ALL: [PlateSpace; 2] = [PlateSpace::AdobeRgb, PlateSpace::Srgb];

    pub fn name(self) -> &'static str {
        match self {
            PlateSpace::AdobeRgb => "adobe-rgb",
            PlateSpace::Srgb => "srgb",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        let k = s.trim().to_ascii_lowercase().replace(['_', ' '], "-");
        match k.as_str() {
            "adobe-rgb" | "adobergb" | "adobe" | "argb" => Some(PlateSpace::AdobeRgb),
            "srgb" => Some(PlateSpace::Srgb),
            _ => None,
        }
    }

    /// Linear RGB → XYZ (D65), rows X, Y, Z.
    pub fn rgb_to_xyz(self) -> [[f64; 3]; 3] {
        match self {
            PlateSpace::AdobeRgb => [
                [0.576_730_9, 0.185_554_0, 0.188_185_2],
                [0.297_376_9, 0.627_349_1, 0.075_274_1],
                [0.027_034_3, 0.070_687_2, 0.991_108_5],
            ],
            PlateSpace::Srgb => [
                [0.412_456_4, 0.357_576_1, 0.180_437_5],
                [0.212_672_9, 0.715_152_2, 0.072_175_0],
                [0.019_333_9, 0.119_192_0, 0.950_304_1],
            ],
        }
    }

    /// The colorants as an ICC profile states them: Bradford-adapted to D50.
    /// `[r, g, b]`, each `[X, Y, Z]`.
    pub fn colorants_d50(self) -> [[f64; 3]; 3] {
        match self {
            PlateSpace::AdobeRgb => [
                [0.609_755_9, 0.311_124_2, 0.019_481_1],
                [0.205_240_1, 0.625_656_0, 0.060_890_2],
                [0.149_224_0, 0.063_219_7, 0.744_838_7],
            ],
            PlateSpace::Srgb => [
                [0.436_074_7, 0.222_504_5, 0.013_932_2],
                [0.385_064_9, 0.716_878_6, 0.097_104_5],
                [0.143_080_4, 0.060_616_9, 0.714_173_3],
            ],
        }
    }
}

/// What `negative` needs to make a plate instead of one negative.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PlateParams {
    pub filters: PlateFilters,
    pub space: PlateSpace,
}

/// The three taking filters as weight triples on the balanced channels,
/// by output channel: `[red, green, blue]`, each `[w_R, w_G, w_B]` summing
/// to one.
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlateWeights {
    pub rows: [[f64; 3]; 3],
    /// Which set the rows came from (`Camera` becomes `Channels` when the
    /// file has no matrix).
    pub filters: PlateFilters,
}

fn mat_mul(a: &[[f64; 3]; 3], b: &[[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut out = [[0.0; 3]; 3];
    for (i, row) in out.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    out
}

fn mat_inv(m: &[[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let det = m[0][0] * (m[1][1] * m[2][2] - m[1][2] * m[2][1])
        - m[0][1] * (m[1][0] * m[2][2] - m[1][2] * m[2][0])
        + m[0][2] * (m[1][0] * m[2][1] - m[1][1] * m[2][0]);
    if !det.is_finite() || det.abs() < 1e-12 {
        return None;
    }
    let c = |a: f64, b: f64, c: f64, d: f64| (a * d - b * c) / det;
    Some([
        [
            c(m[1][1], m[1][2], m[2][1], m[2][2]),
            c(m[0][2], m[0][1], m[2][2], m[2][1]),
            c(m[0][1], m[0][2], m[1][1], m[1][2]),
        ],
        [
            c(m[1][2], m[1][0], m[2][2], m[2][0]),
            c(m[0][0], m[0][2], m[2][0], m[2][2]),
            c(m[0][2], m[0][0], m[1][2], m[1][0]),
        ],
        [
            c(m[1][0], m[1][1], m[2][0], m[2][1]),
            c(m[0][1], m[0][0], m[2][1], m[2][0]),
            c(m[0][0], m[0][1], m[1][0], m[1][1]),
        ],
    ])
}

fn normalise_rows(m: &mut [[f64; 3]; 3]) -> Result<()> {
    for row in m.iter_mut() {
        let s: f64 = row.iter().sum();
        if !s.is_finite() || s.abs() < 1e-9 {
            return Err(Error::Invalid("colour matrix has a row summing to zero".into()));
        }
        for v in row.iter_mut() {
            *v /= s;
        }
    }
    Ok(())
}

impl PlateWeights {
    /// The balanced channels straight into the output channels.
    pub fn channels() -> Self {
        Self { rows: [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]], filters: PlateFilters::Channels }
    }

    /// The panchromatic `base` behind Wratten 25, 58 and 47.
    pub fn wratten(base: Weights) -> Result<Self> {
        let row = |f: ColorFilter| -> Result<[f64; 3]> {
            let w = base.filtered(f);
            w.validate().map_err(|e| {
                Error::Invalid(format!("filter {} passes none of the base weights: {e}", f.name()))
            })?;
            Ok([w.r, w.g, w.b])
        };
        Ok(Self {
            rows: [row(ColorFilter::Red25)?, row(ColorFilter::Green58)?, row(ColorFilter::Blue47)?],
            filters: PlateFilters::Wratten,
        })
    }

    /// The camera matrix towards `space`. `xyz_to_cam` has rows R, G, B as
    /// the RAW file states it. The product `cam_rgb = xyz_to_cam · rgb_to_xyz`
    /// is normalised so each camera channel sees the output white as 1, and
    /// its inverse maps balanced camera channels to linear output RGB; its
    /// rows then sum to one as well.
    pub fn camera(xyz_to_cam: &[[f64; 3]; 3], space: PlateSpace) -> Result<Self> {
        let mut cam_rgb = mat_mul(xyz_to_cam, &space.rgb_to_xyz());
        normalise_rows(&mut cam_rgb)?;
        let rows = mat_inv(&cam_rgb).ok_or_else(|| Error::Invalid("colour matrix is singular".into()))?;
        if rows.iter().flatten().any(|v| !v.is_finite()) {
            return Err(Error::Invalid("colour matrix inverse is not finite".into()));
        }
        Ok(Self { rows, filters: PlateFilters::Camera })
    }

    /// The set `p` asks for. `base` is the panchromatic mix the Wratten set
    /// filters; `xyz_to_cam` the file's matrix, if any.
    pub fn resolve(p: &PlateParams, base: Weights, xyz_to_cam: Option<&[[f64; 3]; 3]>) -> Result<Self> {
        match p.filters {
            PlateFilters::Channels => Ok(Self::channels()),
            PlateFilters::Wratten => Self::wratten(base),
            PlateFilters::Camera => match xyz_to_cam {
                Some(m) => Self::camera(m, p.space),
                None => {
                    tracing::warn!("no colour matrix in the file: the plate takes the channels as they are");
                    Ok(Self::channels())
                }
            },
        }
    }

    /// The three `Weights` the mixer takes, by output channel R, G, B.
    pub fn weights(&self) -> [Weights; 3] {
        let w = |r: [f64; 3]| Weights { r: r[0], g: r[1], b: r[2] };
        [w(self.rows[0]), w(self.rows[1]), w(self.rows[2])]
    }
}

/// The three plates, by output channel R, G, B, each the full-resolution
/// `L̂` plus its band-limited chrominance.
pub fn mix_plate(sep: &Separation, w: &PlateWeights) -> [Plane; 3] {
    let [r, g, b] = w.weights();
    [mix(sep, &r), mix(sep, &g), mix(sep, &b)]
}

/// Frame order on the plate, top to bottom.
pub const FRAMES: [&str; 3] = ["blue", "green", "red"];

/// What the plate file records about itself, inside the negative's JSON.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct PlateInfo {
    /// Frames top to bottom: `["blue", "green", "red"]`.
    pub frames: Vec<String>,
    /// Height of one frame, pixels; the file is three of them.
    pub frame_height: usize,
    pub filters: PlateFilters,
    pub space: PlateSpace,
    /// The weight triples on the balanced channels, by output channel R, G, B.
    pub weights: [[f64; 3]; 3],
}

impl PlateInfo {
    pub fn new(w: &PlateWeights, space: PlateSpace, frame_height: usize) -> Self {
        Self {
            frames: FRAMES.iter().map(|s| s.to_string()).collect(),
            frame_height,
            filters: w.filters,
            space,
            weights: w.rows,
        }
    }

    /// Read the `plate` object out of a negative's description, if it is one.
    pub fn from_description(desc: Option<&serde_json::Value>) -> Option<Self> {
        let v = desc?.get("plate")?;
        serde_json::from_value(v.clone()).ok()
    }
}

/// The three mixes written straight into one stacked plane in [`FRAMES`]
/// order, one frame alive at a time beside the separation.
pub fn mix_plate_stacked(sep: &Separation, w: &PlateWeights) -> Result<Plane> {
    let [r, g, b] = w.weights();
    let (width, h) = (sep.lum.width, sep.lum.height);
    let mut data = Vec::with_capacity(3 * width * h);
    for weights in [b, g, r] {
        let frame = mix(sep, &weights);
        data.extend_from_slice(&frame.data);
    }
    Plane::try_from_vec(width, 3 * h, data)
}

/// Stack the three plates into one plane, frames in [`FRAMES`] order.
pub fn stack(rgb: &[Plane; 3]) -> Result<Plane> {
    let [r, g, b] = rgb;
    let (w, h) = (r.width, r.height);
    if (g.width, g.height) != (w, h) || (b.width, b.height) != (w, h) {
        return Err(Error::Invalid("the three plates differ in size".into()));
    }
    let mut data = Vec::with_capacity(3 * w * h);
    for frame in frames_by_name(rgb) {
        data.extend_from_slice(&frame.data);
    }
    Plane::try_from_vec(w, 3 * h, data)
}

/// The planes in [`FRAMES`] order.
fn frames_by_name(rgb: &[Plane; 3]) -> [&Plane; 3] {
    let [r, g, b] = rgb;
    [b, g, r]
}

/// Split a stacked plate back into `[red, green, blue]`.
pub fn split(plate: &Plane, info: &PlateInfo) -> Result<[Plane; 3]> {
    let h = info.frame_height;
    if h == 0 || plate.height != 3 * h {
        return Err(Error::Invalid(format!(
            "plate is {} rows tall, expected three frames of {h}",
            plate.height
        )));
    }
    if info.frames.len() != 3 {
        return Err(Error::Invalid("plate must name three frames".into()));
    }
    let frame = |i: usize| -> Plane {
        let mut out = Plane::zeros(plate.width, h);
        for y in 0..h {
            out.row_mut(y).copy_from_slice(plate.row(i * h + y));
        }
        out
    };
    let mut rgb: [Option<Plane>; 3] = [None, None, None];
    for (i, name) in info.frames.iter().enumerate() {
        let slot = match name.as_str() {
            "red" => 0,
            "green" => 1,
            "blue" => 2,
            other => return Err(Error::Invalid(format!("unknown plate frame '{other}'"))),
        };
        rgb[slot] = Some(frame(i));
    }
    match rgb {
        [Some(r), Some(g), Some(b)] => Ok([r, g, b]),
        _ => Err(Error::Invalid("plate does not name red, green and blue".into())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn flat_sep(r: f64, g: f64, b: f64) -> Separation {
        let one = |v: f64| Plane::from_vec(2, 2, vec![v; 4]);
        Separation {
            lum: one((r + 2.0 * g + b) / 4.0),
            c1: one((-r + 2.0 * g - b) / 4.0),
            c2: one((r - b) / 4.0),
            mask_max: one(0.5),
            rounds: 1,
            phase: crate::cfa::BayerPhase::RGGB,
        }
    }

    #[test]
    fn channels_recover_the_flat_field() {
        let sep = flat_sep(0.6, 0.3, 0.1);
        let [r, g, b] = mix_plate(&sep, &PlateWeights::channels());
        assert!((r.at(0, 0) - 0.6).abs() < 1e-12);
        assert!((g.at(0, 0) - 0.3).abs() < 1e-12);
        assert!((b.at(0, 0) - 0.1).abs() < 1e-12);
    }

    #[test]
    fn every_set_keeps_grey_grey() {
        // Nikon Z f, D65 matrix as rawler's camera table states it, rows R, G, B.
        let m = [[1.1607, -0.4491, -0.0977], [-0.4522, 1.246, 0.2304], [-0.0458, 0.1519, 0.7616]];
        let base = Weights { r: 0.171, g: 0.489, b: 0.340 };
        for w in [
            PlateWeights::channels(),
            PlateWeights::wratten(base).unwrap(),
            PlateWeights::camera(&m, PlateSpace::AdobeRgb).unwrap(),
            PlateWeights::camera(&m, PlateSpace::Srgb).unwrap(),
        ] {
            for row in w.rows {
                assert!((row.iter().sum::<f64>() - 1.0).abs() < 1e-9, "{:?}", w.filters);
            }
            let sep = flat_sep(0.4, 0.4, 0.4);
            let [r, g, b] = mix_plate(&sep, &w);
            assert!(
                (r.at(1, 1) - 0.4).abs() < 1e-9
                    && (g.at(1, 1) - 0.4).abs() < 1e-9
                    && (b.at(1, 1) - 0.4).abs() < 1e-9
            );
        }
    }

    #[test]
    fn stacked_mix_equals_stack_of_mixes() {
        let sep = flat_sep(0.6, 0.3, 0.1);
        let w = PlateWeights::wratten(Weights::default()).unwrap();
        let a = mix_plate_stacked(&sep, &w).unwrap();
        let b = stack(&mix_plate(&sep, &w)).unwrap();
        assert_eq!(a.data, b.data);
        assert_eq!((a.width, a.height), (2, 6));
    }

    #[test]
    fn camera_matrix_inverts_the_camera() {
        // A camera whose channels are exactly the output primaries has the
        // identity as its weights.
        let xyz_to_rgb = mat_inv(&PlateSpace::AdobeRgb.rgb_to_xyz()).unwrap();
        let w = PlateWeights::camera(&xyz_to_rgb, PlateSpace::AdobeRgb).unwrap();
        for i in 0..3 {
            for j in 0..3 {
                let want = if i == j { 1.0 } else { 0.0 };
                assert!((w.rows[i][j] - want).abs() < 1e-6, "{:?}", w.rows);
            }
        }
        // The Z f matrix sharpens the colour: every diagonal above one, and
        // towards sRGB every off-diagonal negative. (Towards Adobe RGB the
        // red row takes some camera green, since Adobe's green primary is
        // more saturated than the camera's.)
        let m = [[1.1607, -0.4491, -0.0977], [-0.4522, 1.246, 0.2304], [-0.0458, 0.1519, 0.7616]];
        let w = PlateWeights::camera(&m, PlateSpace::Srgb).unwrap();
        for i in 0..3 {
            for j in 0..3 {
                assert!(if i == j { w.rows[i][j] > 1.0 } else { w.rows[i][j] < 0.0 }, "{:?}", w.rows);
            }
        }
        let w = PlateWeights::camera(&m, PlateSpace::AdobeRgb).unwrap();
        assert!((0..3).all(|i| w.rows[i][i] > 1.0), "{:?}", w.rows);
        assert!(mat_inv(&[[1.0, 2.0, 3.0], [2.0, 4.0, 6.0], [0.0, 0.0, 1.0]]).is_none());
    }

    #[test]
    fn stack_and_split_round_trip() {
        let p = |v: f64| Plane::from_vec(3, 2, (0..6).map(|i| v + i as f64 * 0.01).collect());
        let rgb = [p(0.1), p(0.5), p(0.9)];
        let plate = stack(&rgb).unwrap();
        assert_eq!((plate.width, plate.height), (3, 6));
        // Blue on top, red at the bottom.
        assert_eq!(plate.at(0, 0), 0.9);
        assert_eq!(plate.at(0, 2), 0.5);
        assert_eq!(plate.at(0, 4), 0.1);
        let info = PlateInfo::new(&PlateWeights::channels(), PlateSpace::AdobeRgb, 2);
        let back = split(&plate, &info).unwrap();
        for i in 0..3 {
            assert_eq!(back[i].data, rgb[i].data);
        }
        let json = serde_json::json!({ "plate": info });
        assert_eq!(PlateInfo::from_description(Some(&json)).unwrap(), info);
        assert!(PlateInfo::from_description(Some(&serde_json::json!({ "stage": "negative" }))).is_none());
        assert!(split(&p(0.0), &info).is_err());
    }

    /// A plate through the whole path: mosaic → separation → stacked plate
    /// file → three positives on one paper → RGB file. Grey stays grey at
    /// every density (no crossover), a colour keeps its channel order, and
    /// the lamps shift one channel only.
    #[test]
    fn plate_prints_grey_as_grey_and_colour_in_order() {
        use crate::kodak::{mosaic_rgb, quantisation_noise};
        use crate::print::{render_within, PrintParams};
        use crate::recipe::Recipe;
        use crate::synth::Rgb;
        use crate::tiffout::{write_gray16, TiffMeta};

        // Left half: a grey ramp. Right half: a flat orange.
        let (w, h) = (128, 64);
        let mut rgb = Rgb::new(w, h);
        for y in 0..h {
            for x in 0..w {
                if x < 64 {
                    let v = 0.02 + 0.9 * (y as f64 / h as f64);
                    rgb.set(x, y, v, v, v);
                } else {
                    rgb.set(x, y, 0.6, 0.3, 0.05);
                }
            }
        }
        let fb = mosaic_rgb(&rgb, crate::cfa::BayerPhase::RGGB, quantisation_noise(), "plate");
        let sep = crate::separate::separate(&fb.mosaic, &Default::default()).unwrap();
        let weights = PlateWeights::channels();
        let plate = mix_plate_stacked(&sep, &weights).unwrap();
        let info = PlateInfo::new(&weights, PlateSpace::AdobeRgb, h);
        let desc = serde_json::json!({ "stage": "negative", "plate": info }).to_string();

        let dir = std::env::temp_dir().join(format!("mimizan_plate_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("plate.tif");
        write_gray16(
            &path,
            &plate,
            &TiffMeta { description: &desc, trc: crate::icc::GrayTrc::Linear, orientation: 1, dpi: None },
        )
        .unwrap();

        let p = PrintParams { recipe: Recipe::on_paper(), ..Default::default() };
        let out = render_within(&path, &p, None).unwrap();
        let sheet = out.plate.as_ref().expect("a plate print");
        assert_eq!((out.plane.width, out.plane.height), (w, h));
        assert!(out.info.plate.is_some());
        for y in 8..h - 8 {
            let (r, g, b) = (out.plane.at(20, y), sheet.green.at(20, y), sheet.blue.at(20, y));
            assert!((r - g).abs() < 1e-9 && (g - b).abs() < 1e-9, "grey at row {y}: {r} {g} {b}");
            let (r, g, b) = (out.plane.at(100, y), sheet.green.at(100, y), sheet.blue.at(100, y));
            assert!(r > g && g > b, "orange at row {y}: {r} {g} {b}");
        }
        // The ramp is monotone on the paper.
        assert!(out.plane.at(20, 50) > out.plane.at(20, 10));

        // A lamp on red alone lifts red and leaves green and blue as they were.
        let lit = PrintParams { lamps: [0.5, 0.0, 0.0], ..p.clone() };
        let out2 = render_within(&path, &lit, None).unwrap();
        let sheet2 = out2.plate.as_ref().unwrap();
        assert!(out2.plane.at(100, 30) > out.plane.at(100, 30));
        assert_eq!(sheet2.green.data, sheet.green.data);
        assert_eq!(sheet2.blue.data, sheet.blue.data);

        // The files write and carry an RGB profile.
        let tif = dir.join("print.tif");
        crate::print::write(&out, &tif, &p).unwrap();
        let back = crate::tiffin::read_rgb16(&tif).unwrap();
        assert_eq!((back[0].width, back[0].height), (w, h));
        assert!((back[0].at(20, 30) - out.plane.at(20, 30)).abs() < 1.0 / 65535.0);
        assert!((back[2].at(100, 30) - sheet.blue.at(100, 30)).abs() < 1.0 / 65535.0);
        let jpg = dir.join("print.jpg");
        crate::print::write(&out, &jpg, &p).unwrap();
        assert!(std::fs::metadata(&jpg).unwrap().len() > 0);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_parse() {
        for f in PlateFilters::ALL {
            assert_eq!(PlateFilters::parse(f.name()), Some(f));
        }
        for s in PlateSpace::ALL {
            assert_eq!(PlateSpace::parse(s.name()), Some(s));
        }
        assert_eq!(PlateSpace::parse("AdobeRGB"), Some(PlateSpace::AdobeRgb));
        assert_eq!(PlateFilters::parse("lippmann"), None);
    }
}
