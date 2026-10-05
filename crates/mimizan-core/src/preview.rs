//! Quick-look PNGs for eyeballing (never part of the deliverable pipeline).

use crate::error::Result;
use crate::plane::Plane;
use rayon::prelude::*;
use std::path::Path;

/// Box-average by `factor`, gamma-encode (2.2) and write an 8-bit PNG.
/// `factor = 1` keeps full resolution.
pub fn write_png(plane: &Plane, path: &Path, factor: usize, gamma_encode: bool) -> Result<()> {
    let f = factor.max(1);
    let w = plane.width / f;
    let h = plane.height / f;
    let inv = 1.0 / (f * f) as f64;
    let mut buf = vec![0u8; w * h];
    buf.par_chunks_mut(w).enumerate().for_each(|(oy, row)| {
        for (ox, px) in row.iter_mut().enumerate() {
            let mut acc = 0.0;
            for dy in 0..f {
                let r = plane.row(oy * f + dy);
                for dx in 0..f {
                    acc += r[ox * f + dx];
                }
            }
            let v = (acc * inv).clamp(0.0, 1.0);
            let e = if gamma_encode { v.powf(1.0 / 2.2) } else { v };
            *px = (e * 255.0).round() as u8;
        }
    });
    image::save_buffer(path, &buf, w as u32, h as u32, image::ColorType::L8)?;
    Ok(())
}

/// Crop a window (for 1:1 inspection) and write it full-res.
pub fn write_png_crop(
    plane: &Plane,
    path: &Path,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    gamma_encode: bool,
) -> Result<()> {
    let w = w.min(plane.width.saturating_sub(x0));
    let h = h.min(plane.height.saturating_sub(y0));
    let mut sub = Plane::zeros(w, h);
    for y in 0..h {
        sub.row_mut(y).copy_from_slice(&plane.row(y0 + y)[x0..x0 + w]);
    }
    write_png(&sub, path, 1, gamma_encode)
}
