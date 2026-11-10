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
    if w == 0 || h == 0 {
        return Err(crate::Error::Invalid(format!(
            "preview of {}x{} at 1/{f} has no pixels",
            plane.width, plane.height
        )));
    }
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

/// Crop a window (for 1:1 inspection) and write it full-res. The window
/// is cut to the picture; one that lies outside it is an error.
pub fn write_png_crop(
    plane: &Plane,
    path: &Path,
    x0: usize,
    y0: usize,
    w: usize,
    h: usize,
    gamma_encode: bool,
) -> Result<()> {
    let (asked_w, asked_h) = (w, h);
    let w = w.min(plane.width.saturating_sub(x0));
    let h = h.min(plane.height.saturating_sub(y0));
    if w == 0 || h == 0 {
        return Err(crate::Error::Invalid(format!(
            "the window {asked_w}x{asked_h} at {x0},{y0} holds no pixel of the {}x{} picture",
            plane.width, plane.height
        )));
    }
    let mut sub = Plane::zeros(w, h);
    for y in 0..h {
        sub.row_mut(y).copy_from_slice(&plane.row(y0 + y)[x0..x0 + w]);
    }
    write_png(&sub, path, 1, gamma_encode)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A window is cut to the picture; one that lies outside it is an
    /// error the command reports, not a slice out of range.
    #[test]
    fn a_window_outside_the_picture_is_an_error() {
        let dir = std::env::temp_dir().join(format!("mimizan-preview-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let mut p = Plane::zeros(8, 6);
        p.data.iter_mut().enumerate().for_each(|(i, v)| *v = i as f64 / 48.0);
        let path = dir.join("crop.png");
        assert!(write_png_crop(&p, &path, 7000, 0, 4, 4, false).is_err());
        assert!(write_png_crop(&p, &path, 0, 6, 4, 4, false).is_err());
        assert!(write_png_crop(&p, &path, 8, 6, 4, 4, false).is_err());
        assert!(write_png_crop(&p, &path, 0, 0, 0, 4, false).is_err());
        assert!(!path.exists(), "nothing is written for a window that is not there");
        // A window over the edge is cut to what the picture has.
        write_png_crop(&p, &path, 6, 4, 100, 100, false).unwrap();
        assert_eq!(image::image_dimensions(&path).unwrap(), (2, 2));
        let _ = std::fs::remove_dir_all(&dir);
    }
}
