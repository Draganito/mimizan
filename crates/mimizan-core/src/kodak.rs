//! Fake-Bayer benchmark on demosaiced reference images (Kodak PhotoCD set).
//!
//! An 8-bit sRGB picture is linearised, sampled through a Bayer CFA and
//! quantised to 16 bit. The same mosaic goes to this pipeline and, as a DNG,
//! to external demosaicers. Ground truth is the luminance `(R + 2G + B)/4` of
//! the linear picture, which is exactly the `Native` mix. Quality is PSNR and
//! SSIM on sRGB-encoded 8-bit-scale values (the convention of the demosaicing
//! literature) plus the linear RMSE.

use crate::cfa::{BayerPhase, Color};
use crate::decode::Rect;
use crate::error::{Error, Result};
use crate::ingest::{CameraInfo, IngestReport, Mosaic, NoiseModel};
use crate::plane::Plane;
use crate::synth::Rgb;
use serde::Serialize;
use std::io::{BufWriter, Read, Write};
use std::path::Path;

/// sRGB electro-optical transfer function (encoded [0,1] -> linear [0,1]).
#[inline]
pub fn srgb_decode(v: f64) -> f64 {
    if v <= 0.04045 {
        v / 12.92
    } else {
        ((v + 0.055) / 1.055).powf(2.4)
    }
}

/// Inverse of [`srgb_decode`] (linear [0,1] -> encoded [0,1]).
#[inline]
pub fn srgb_encode(v: f64) -> f64 {
    let v = v.clamp(0.0, 1.0);
    if v <= 0.003_130_8 {
        v * 12.92
    } else {
        1.055 * v.powf(1.0 / 2.4) - 0.055
    }
}

/// Read an 8-bit sRGB PNG/JPEG into linear planes.
pub fn load_srgb8(path: &Path) -> Result<Rgb> {
    let img = image::open(path)?.to_rgb8();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let lut: Vec<f64> = (0..256).map(|c| srgb_decode(c as f64 / 255.0)).collect();
    let mut rgb = Rgb::new(w, h);
    for (i, px) in img.pixels().enumerate() {
        rgb.r.data[i] = lut[px[0] as usize];
        rgb.g.data[i] = lut[px[1] as usize];
        rgb.b.data[i] = lut[px[2] as usize];
    }
    Ok(rgb)
}

/// Noise model of an 8-bit sRGB source: quantisation only, `σ ≈ 2.2e-3·√s + 1e-4`
/// in linear units (the derivative of the sRGB curve divided by `255·√12`).
pub fn quantisation_noise() -> NoiseModel {
    NoiseModel { a: 0.0022, b: 0.0001 }
}

/// Mosaic of a linear picture: one channel per pixel after the CFA, quantised
/// to 16 bit. The 16-bit samples are what the DNG carries, the `Mosaic`
/// is the same data as the pipeline sees it (balance 1,1,1, no saturation).
pub struct FakeBayer {
    pub mosaic: Mosaic,
    pub samples: Vec<u16>,
}

pub fn mosaic_rgb(rgb: &Rgb, phase: BayerPhase, noise: NoiseModel, name: &str) -> FakeBayer {
    let (w, h) = (rgb.width(), rgb.height());
    let mut samples = vec![0u16; w * h];
    let mut plane = Plane::zeros(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = match phase.color_at(x, y) {
                Color::R => rgb.r.at(x, y),
                Color::G => rgb.g.at(x, y),
                Color::B => rgb.b.at(x, y),
            };
            let q = (v.clamp(0.0, 1.0) * 65535.0).round() as u16;
            samples[y * w + x] = q;
            plane.set(x, y, f64::from(q) / 65535.0);
        }
    }
    let wb = [1.0, 1.0, 1.0];
    let mosaic = Mosaic {
        plane,
        phase: Some(phase),
        saturated: vec![0u8; w * h],
        noise,
        wb,
        orientation: 1,
        camera: CameraInfo {
            make: "mimizan".into(),
            model: format!("fake Bayer {name}"),
            clean_make: "mimizan".into(),
            clean_model: format!("fake_bayer_{name}"),
            bits: 16,
            exposure: Default::default(),
        },
        xyz_to_cam: None,
        report: IngestReport {
            crop: Rect { x: 0, y: 0, w, h },
            saturated_raw: 0,
            saturated_dilated: 0,
            defects: 0,
            defect_hot_rows: vec![],
            min_norm: 0.0,
            max_norm: 1.0,
            wb,
            wb_source: "exact".into(),
        },
    };
    FakeBayer { mosaic, samples }
}

// ------------------------------------------------------------ real RAW truth

/// Decode and ingest a RAW (as-shot balance, defects repaired), then bin every
/// 2x2 CFA cell into one RGB pixel: R and B are the measured sites, G the mean
/// of the two. No interpolation anywhere, so the half-size picture is a true
/// RGB reference from real optics and real noise. Clipped sites stay clipped.
#[cfg(feature = "decode-rawler")]
pub fn binned_raw(path: &Path) -> Result<(Rgb, NoiseModel)> {
    let frame = crate::pipeline::decode(path)?;
    let mosaic = crate::ingest::ingest(&frame, &crate::ingest::IngestParams::default());
    let phase = match mosaic.phase {
        Some(p) => p,
        None => return Err(Error::Invalid(format!("{}: not a Bayer file", path.display()))),
    };
    let (w, h) = (mosaic.plane.width / 2, mosaic.plane.height / 2);
    let mut rgb = Rgb::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let (mut r, mut g, mut b, mut ng) = (0.0, 0.0, 0.0, 0.0);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let v = mosaic.plane.at(2 * x + dx, 2 * y + dy).clamp(0.0, 1.0);
                match phase.color_at(2 * x + dx, 2 * y + dy) {
                    Color::R => r = v,
                    Color::B => b = v,
                    Color::G => {
                        g += v;
                        ng += 1.0;
                    }
                }
            }
            rgb.set(x, y, r, g / ng, b);
        }
    }
    tracing::info!(
        "{}: {} {}, balance R {:.3} B {:.3}, binned to {}x{}",
        path.display(),
        mosaic.camera.clean_make,
        mosaic.camera.clean_model,
        mosaic.wb[0],
        mosaic.wb[2],
        w,
        h
    );
    Ok((rgb, mosaic.noise))
}

// ---------------------------------------------------------------- DNG writer

const T_BYTE: u16 = 1;
const T_ASCII: u16 = 2;
const T_SHORT: u16 = 3;
const T_LONG: u16 = 4;
const T_RATIONAL: u16 = 5;
const T_SRATIONAL: u16 = 10;

struct Entry {
    tag: u16,
    typ: u16,
    count: u32,
    data: Vec<u8>,
}

fn ascii(tag: u16, s: &str) -> Entry {
    let mut data = s.as_bytes().to_vec();
    data.push(0);
    Entry { tag, typ: T_ASCII, count: data.len() as u32, data }
}
fn shorts(tag: u16, v: &[u16]) -> Entry {
    Entry { tag, typ: T_SHORT, count: v.len() as u32, data: v.iter().flat_map(|x| x.to_le_bytes()).collect() }
}
fn longs(tag: u16, v: &[u32]) -> Entry {
    Entry { tag, typ: T_LONG, count: v.len() as u32, data: v.iter().flat_map(|x| x.to_le_bytes()).collect() }
}
fn bytes(tag: u16, v: &[u8]) -> Entry {
    Entry { tag, typ: T_BYTE, count: v.len() as u32, data: v.to_vec() }
}
fn rationals(tag: u16, v: &[(u32, u32)]) -> Entry {
    let data = v.iter().flat_map(|(n, d)| [n.to_le_bytes(), d.to_le_bytes()].concat()).collect();
    Entry { tag, typ: T_RATIONAL, count: v.len() as u32, data }
}
fn srationals(tag: u16, v: &[f64]) -> Entry {
    let data = v
        .iter()
        .flat_map(|x| {
            let n = (x * 10_000.0).round() as i32;
            [n.to_le_bytes(), 10_000i32.to_le_bytes()].concat()
        })
        .collect();
    Entry { tag, typ: T_SRATIONAL, count: v.len() as u32, data }
}

/// XYZ (D65) -> linear sRGB, i.e. the "camera" of a fake sensor whose channels
/// are the sRGB primaries. With `AsShotNeutral = 1,1,1` a converter reproduces
/// the source colours; the benchmark only needs the demosaiced channels.
const XYZ_TO_SRGB: [f64; 9] = [3.2406, -1.5372, -0.4986, -0.9689, 1.8758, 0.0415, 0.0557, -0.2040, 1.0570];

/// Write a minimal, uncompressed 16-bit CFA DNG (one IFD, one strip) that
/// libraw and RawTherapee open. Black level 0, white level 65535.
pub fn write_dng(
    path: &Path,
    width: usize,
    height: usize,
    samples: &[u16],
    phase: BayerPhase,
    model: &str,
) -> Result<()> {
    if samples.len() != width * height {
        return Err(Error::Invalid("dng: sample count does not match size".into()));
    }
    let cfa: Vec<u8> = phase
        .tile()
        .iter()
        .map(|c| match c {
            Color::R => 0,
            Color::G => 1,
            Color::B => 2,
        })
        .collect();
    let strip_bytes = (samples.len() * 2) as u32;
    let software = concat!("mimizan ", env!("CARGO_PKG_VERSION"));
    let mut entries = vec![
        longs(254, &[0]),
        longs(256, &[width as u32]),
        longs(257, &[height as u32]),
        shorts(258, &[16]),
        shorts(259, &[1]),
        shorts(262, &[32803]),
        ascii(271, "Mimizan"),
        ascii(272, model),
        longs(273, &[0]), // StripOffsets, patched below
        shorts(274, &[1]),
        shorts(277, &[1]),
        longs(278, &[height as u32]),
        longs(279, &[strip_bytes]),
        shorts(284, &[1]),
        ascii(305, software),
        shorts(33421, &[2, 2]),
        bytes(33422, &cfa),
        bytes(50706, &[1, 4, 0, 0]),
        bytes(50707, &[1, 1, 0, 0]),
        ascii(50708, &format!("Mimizan {model}")),
        shorts(50711, &[1]),
        longs(50714, &[0]),
        longs(50717, &[65535]),
        srationals(50721, &XYZ_TO_SRGB),
        rationals(50728, &[(1, 1), (1, 1), (1, 1)]),
        shorts(50778, &[21]),
    ];
    entries.sort_by_key(|e| e.tag);

    // Layout: header (8) | IFD (2 + 12n + 4) | out-of-line values | strip.
    let ifd_len = 2 + 12 * entries.len() as u32 + 4;
    let mut extra_off = 8 + ifd_len;
    let mut offsets = Vec::with_capacity(entries.len());
    for e in &entries {
        if e.data.len() > 4 {
            offsets.push(Some(extra_off));
            extra_off += e.data.len() as u32 + (e.data.len() as u32 & 1);
        } else {
            offsets.push(None);
        }
    }
    let strip_off = extra_off;
    if let Some(e) = entries.iter_mut().find(|e| e.tag == 273) {
        e.data = strip_off.to_le_bytes().to_vec();
    }

    let mut w = BufWriter::new(std::fs::File::create(path)?);
    w.write_all(&[b'I', b'I', 42, 0])?;
    w.write_all(&8u32.to_le_bytes())?;
    w.write_all(&(entries.len() as u16).to_le_bytes())?;
    for (e, off) in entries.iter().zip(&offsets) {
        w.write_all(&e.tag.to_le_bytes())?;
        w.write_all(&e.typ.to_le_bytes())?;
        w.write_all(&e.count.to_le_bytes())?;
        match off {
            Some(o) => w.write_all(&o.to_le_bytes())?,
            None => {
                let mut v = [0u8; 4];
                v[..e.data.len()].copy_from_slice(&e.data);
                w.write_all(&v)?;
            }
        }
    }
    w.write_all(&0u32.to_le_bytes())?;
    for (e, off) in entries.iter().zip(&offsets) {
        if off.is_some() {
            w.write_all(&e.data)?;
            if e.data.len() & 1 == 1 {
                w.write_all(&[0])?;
            }
        }
    }
    let mut buf = Vec::with_capacity(samples.len() * 2);
    for s in samples {
        buf.extend_from_slice(&s.to_le_bytes());
    }
    w.write_all(&buf)?;
    w.flush()?;
    Ok(())
}

// ------------------------------------------------------------- RGB readers

/// 16-bit binary PPM (`P6`, maxval 65535, big-endian), as libraw's
/// `dcraw_emu -4` writes it. Values scaled to [0,1].
pub fn read_ppm16(path: &Path) -> Result<Rgb> {
    let mut bytes = Vec::new();
    std::fs::File::open(path)?.read_to_end(&mut bytes)?;
    let mut pos = 0usize;
    let mut fields = Vec::new();
    while fields.len() < 4 {
        while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if pos < bytes.len() && bytes[pos] == b'#' {
            while pos < bytes.len() && bytes[pos] != b'\n' {
                pos += 1;
            }
            continue;
        }
        let start = pos;
        while pos < bytes.len() && !bytes[pos].is_ascii_whitespace() {
            pos += 1;
        }
        if start == pos {
            return Err(Error::Invalid(format!("{}: truncated PPM header", path.display())));
        }
        fields.push(String::from_utf8_lossy(&bytes[start..pos]).to_string());
    }
    pos += 1; // single whitespace after maxval
    if fields[0] != "P6" {
        return Err(Error::Invalid(format!("{}: not a P6 PPM", path.display())));
    }
    let w: usize = fields[1].parse().map_err(|_| Error::Invalid("ppm width".into()))?;
    let h: usize = fields[2].parse().map_err(|_| Error::Invalid("ppm height".into()))?;
    let maxval: f64 = fields[3].parse().map_err(|_| Error::Invalid("ppm maxval".into()))?;
    let two_bytes = maxval > 255.0;
    let bpp = if two_bytes { 6 } else { 3 };
    if bytes.len() < pos + w * h * bpp {
        return Err(Error::Invalid(format!("{}: PPM data too short", path.display())));
    }
    let mut rgb = Rgb::new(w, h);
    for i in 0..w * h {
        let p = pos + i * bpp;
        let sample = |k: usize| -> f64 {
            if two_bytes {
                f64::from(u16::from_be_bytes([bytes[p + 2 * k], bytes[p + 2 * k + 1]])) / maxval
            } else {
                f64::from(bytes[p + k]) / maxval
            }
        };
        rgb.r.data[i] = sample(0);
        rgb.g.data[i] = sample(1);
        rgb.b.data[i] = sample(2);
    }
    Ok(rgb)
}

/// 8- or 16-bit RGB PNG scaled to [0,1]; `decode_srgb` applies the sRGB EOTF
/// (for converters that can only write gamma-encoded output).
pub fn read_rgb_png(path: &Path, decode_srgb: bool) -> Result<Rgb> {
    let img = image::open(path)?.to_rgb16();
    let (w, h) = (img.width() as usize, img.height() as usize);
    let mut rgb = Rgb::new(w, h);
    let f = |v: u16| {
        let x = f64::from(v) / 65535.0;
        if decode_srgb {
            srgb_decode(x)
        } else {
            x
        }
    };
    for (i, px) in img.pixels().enumerate() {
        rgb.r.data[i] = f(px[0]);
        rgb.g.data[i] = f(px[1]);
        rgb.b.data[i] = f(px[2]);
    }
    Ok(rgb)
}

// ------------------------------------------------------------------ metrics

#[derive(Clone, Copy, Debug, Serialize)]
pub struct Quality {
    /// PSNR of sRGB-encoded values on the 0..255 scale, border excluded.
    pub psnr_db: f64,
    /// Mean SSIM (Gaussian 11x11, σ 1.5, K1 0.01, K2 0.03) of the same values.
    pub ssim: f64,
    /// RMSE of linear values in [0,1], border excluded.
    pub rmse_lin: f64,
}

fn encoded(p: &Plane) -> Plane {
    Plane::from_vec(p.width, p.height, p.data.iter().map(|&v| srgb_encode(v) * 255.0).collect())
}

/// Separable Gaussian blur, reflect padding.
fn gauss(p: &Plane, sigma: f64, radius: usize) -> Plane {
    let k: Vec<f64> = (0..=2 * radius)
        .map(|i| {
            let d = i as f64 - radius as f64;
            (-d * d / (2.0 * sigma * sigma)).exp()
        })
        .collect();
    let s: f64 = k.iter().sum();
    let k: Vec<f64> = k.iter().map(|v| v / s).collect();
    let (w, h) = (p.width, p.height);
    let refl = |i: isize, n: usize| -> usize {
        let n = n as isize;
        let mut i = i;
        if i < 0 {
            i = -i;
        }
        if i >= n {
            i = 2 * n - 2 - i;
        }
        i.clamp(0, n - 1) as usize
    };
    let mut tmp = Plane::zeros(w, h);
    for y in 0..h {
        let row = p.row(y);
        let out = tmp.row_mut(y);
        for x in 0..w {
            let mut acc = 0.0;
            for (i, kv) in k.iter().enumerate() {
                acc += kv * row[refl(x as isize + i as isize - radius as isize, w)];
            }
            out[x] = acc;
        }
    }
    let mut out = Plane::zeros(w, h);
    for y in 0..h {
        for (i, kv) in k.iter().enumerate() {
            let sy = refl(y as isize + i as isize - radius as isize, h);
            let src = tmp.row(sy);
            let dst = out.row_mut(y);
            for x in 0..w {
                dst[x] += kv * src[x];
            }
        }
    }
    out
}

pub fn quality(est: &Plane, truth: &Plane, border: usize) -> Quality {
    assert_eq!((est.width, est.height), (truth.width, truth.height));
    let (w, h) = (est.width, est.height);
    let (x0, x1, y0, y1) = (border, w.saturating_sub(border), border, h.saturating_sub(border));
    let n = ((x1 - x0) * (y1 - y0)) as f64;

    let mut se_lin = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            let d = est.at(x, y) - truth.at(x, y);
            se_lin += d * d;
        }
    }
    let a = encoded(est);
    let b = encoded(truth);
    let mut se = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            let d = a.at(x, y) - b.at(x, y);
            se += d * d;
        }
    }
    let mse = se / n;
    let psnr_db = if mse > 0.0 { 10.0 * (255.0 * 255.0 / mse).log10() } else { f64::INFINITY };

    // SSIM (Wang et al. 2004) on the encoded planes.
    let (sigma, radius) = (1.5, 5);
    let mu_a = gauss(&a, sigma, radius);
    let mu_b = gauss(&b, sigma, radius);
    let sq = |p: &Plane, q: &Plane| {
        Plane::from_vec(w, h, p.data.iter().zip(&q.data).map(|(u, v)| u * v).collect())
    };
    let s_aa = gauss(&sq(&a, &a), sigma, radius);
    let s_bb = gauss(&sq(&b, &b), sigma, radius);
    let s_ab = gauss(&sq(&a, &b), sigma, radius);
    let c1 = (0.01f64 * 255.0).powi(2);
    let c2 = (0.03f64 * 255.0).powi(2);
    let mut ssim_sum = 0.0;
    for y in y0..y1 {
        for x in x0..x1 {
            let (ma, mb) = (mu_a.at(x, y), mu_b.at(x, y));
            let va = s_aa.at(x, y) - ma * ma;
            let vb = s_bb.at(x, y) - mb * mb;
            let cov = s_ab.at(x, y) - ma * mb;
            ssim_sum +=
                ((2.0 * ma * mb + c1) * (2.0 * cov + c2)) / ((ma * ma + mb * mb + c1) * (va + vb + c2));
        }
    }
    Quality { psnr_db, ssim: ssim_sum / n, rmse_lin: (se_lin / n).sqrt() }
}

/// Top-left corner of the `size`×`size` window (inside `border`) with the
/// largest summed squared error of all `errs` planes combined: the region
/// hardest for every method, used for the comparison crops.
pub fn hardest_window(ests: &[&Plane], truth: &Plane, size: usize, border: usize) -> (usize, usize) {
    let (w, h) = (truth.width, truth.height);
    let enc_t = encoded(truth);
    let mut e2 = vec![0.0f64; w * h];
    for est in ests {
        let enc = encoded(est);
        for (o, (a, b)) in e2.iter_mut().zip(enc.data.iter().zip(&enc_t.data)) {
            let d = a - b;
            *o += d * d;
        }
    }
    // Integral image.
    let mut ii = vec![0.0f64; (w + 1) * (h + 1)];
    for y in 0..h {
        let mut row = 0.0;
        for x in 0..w {
            row += e2[y * w + x];
            ii[(y + 1) * (w + 1) + x + 1] = ii[y * (w + 1) + x + 1] + row;
        }
    }
    let sum = |x: usize, y: usize| {
        ii[(y + size) * (w + 1) + x + size] - ii[y * (w + 1) + x + size] - ii[(y + size) * (w + 1) + x]
            + ii[y * (w + 1) + x]
    };
    let mut best = (border, border, -1.0);
    if w < size + 2 * border || h < size + 2 * border {
        return (0, 0);
    }
    for y in border..=h - size - border {
        for x in border..=w - size - border {
            let s = sum(x, y);
            if s > best.2 {
                best = (x, y, s);
            }
        }
    }
    (best.0, best.1)
}

/// Side-by-side crops of several planes (same window), each magnified
/// `scale`× with nearest neighbour, gamma-encoded to sRGB, separated by a
/// white gap. Written as an 8-bit grey PNG.
pub fn write_strip(
    path: &Path,
    panels: &[&Plane],
    x0: usize,
    y0: usize,
    size: usize,
    scale: usize,
) -> Result<()> {
    let gap = 4usize;
    let pw = size * scale;
    let total_w = panels.len() * pw + (panels.len() - 1) * gap;
    let total_h = pw;
    let mut buf = vec![255u8; total_w * total_h];
    for (i, p) in panels.iter().enumerate() {
        let ox = i * (pw + gap);
        for y in 0..pw {
            let sy = (y0 + y / scale).min(p.height - 1);
            for x in 0..pw {
                let sx = (x0 + x / scale).min(p.width - 1);
                buf[y * total_w + ox + x] = (srgb_encode(p.at(sx, sy)) * 255.0).round() as u8;
            }
        }
    }
    image::save_buffer(path, &buf, total_w as u32, total_h as u32, image::ColorType::L8)?;
    Ok(())
}

/// Write a full-size grey PNG (sRGB-encoded) of a linear plane.
pub fn write_gray_png(path: &Path, p: &Plane) -> Result<()> {
    let buf: Vec<u8> = p.data.iter().map(|&v| (srgb_encode(v) * 255.0).round() as u8).collect();
    image::save_buffer(path, &buf, p.width as u32, p.height as u32, image::ColorType::L8)?;
    Ok(())
}

/// Luminance `(R + 2G + B)/4` of demosaiced planes: the same weights as the
/// ground truth and as the `Native` mix.
pub fn luminance(rgb: &Rgb) -> Plane {
    rgb.luminance()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn srgb_roundtrip() {
        for i in 0..=255 {
            let v = i as f64 / 255.0;
            assert!((srgb_encode(srgb_decode(v)) - v).abs() < 1e-9);
        }
    }

    /// The `tiff` crate refuses photometric 32803 (CFA), so the IFD is walked
    /// by hand: sorted tags, in-line vs out-of-line values, strip contents.
    #[test]
    fn dng_structure_is_valid_tiff() {
        let dir = std::env::temp_dir().join(format!("mimizan_dng_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("t.dng");
        let (w, h) = (8usize, 6usize);
        let samples: Vec<u16> = (0..w * h).map(|i| (i * 1000) as u16).collect();
        write_dng(&path, w, h, &samples, BayerPhase::GRBG, "test").unwrap();
        let b = std::fs::read(&path).unwrap();
        std::fs::remove_dir_all(&dir).unwrap();

        let u16_at = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]);
        let u32_at = |o: usize| u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]);
        assert_eq!(&b[0..4], b"II\x2a\x00");
        let ifd = u32_at(4) as usize;
        let n = u16_at(ifd) as usize;
        let mut tags = Vec::new();
        let mut strip_off = 0;
        let mut strip_len = 0;
        let mut cfa = Vec::new();
        let mut photometric = 0;
        for i in 0..n {
            let e = ifd + 2 + 12 * i;
            let (tag, typ, count, val) = (u16_at(e), u16_at(e + 2), u32_at(e + 4), e + 8);
            tags.push(tag);
            match tag {
                262 => photometric = u16_at(val),
                273 => strip_off = u32_at(val) as usize,
                279 => strip_len = u32_at(val) as usize,
                33422 => {
                    assert_eq!((typ, count), (T_BYTE, 4));
                    cfa = b[val..val + 4].to_vec();
                }
                _ => {}
            }
        }
        assert!(tags.windows(2).all(|p| p[0] < p[1]), "tags sorted");
        assert_eq!(photometric, 32803);
        assert_eq!(cfa, vec![1, 0, 2, 1]);
        assert_eq!(strip_len, w * h * 2);
        assert_eq!(strip_off + strip_len, b.len());
        let back: Vec<u16> = (0..w * h).map(|i| u16_at(strip_off + 2 * i)).collect();
        assert_eq!(back, samples);
    }

    #[test]
    fn quality_of_identical_planes_is_perfect() {
        let p = Plane::from_vec(32, 32, (0..1024).map(|i| (i % 97) as f64 / 97.0).collect());
        let q = quality(&p, &p, 4);
        assert!(q.psnr_db.is_infinite());
        assert!((q.ssim - 1.0).abs() < 1e-9);
        assert_eq!(q.rmse_lin, 0.0);
    }

    #[test]
    fn psnr_matches_known_offset() {
        // A constant encoded offset of 1/255 gives PSNR = 20 log10(255) = 48.13 dB.
        let w = 64;
        let truth = Plane::from_vec(w, w, vec![0.2; w * w]);
        let e = srgb_encode(0.2) + 1.0 / 255.0;
        let est = Plane::from_vec(w, w, vec![srgb_decode(e); w * w]);
        let q = quality(&est, &truth, 0);
        assert!((q.psnr_db - 48.13).abs() < 0.01, "{}", q.psnr_db);
    }

    #[test]
    fn mosaic_samples_follow_cfa() {
        let mut rgb = Rgb::new(4, 4);
        for y in 0..4 {
            for x in 0..4 {
                rgb.set(x, y, 0.1, 0.5, 0.9);
            }
        }
        let fb = mosaic_rgb(&rgb, BayerPhase::RGGB, quantisation_noise(), "t");
        assert_eq!(fb.samples[0], (0.1f64 * 65535.0).round() as u16);
        assert_eq!(fb.samples[1], (0.5f64 * 65535.0).round() as u16);
        assert_eq!(fb.samples[5], (0.9f64 * 65535.0).round() as u16);
        assert!((fb.mosaic.plane.at(1, 1) - 0.9).abs() < 1e-4);
    }
}
