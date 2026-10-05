//! 16-bit grey TIFF output with embedded ICC and JSON description (SPEC section 7).

use crate::error::Result;
use crate::icc::{gray_profile, GrayTrc};
use crate::plane::Plane;
use rayon::prelude::*;
use std::fs::File;
use std::io::BufWriter;
use std::path::Path;
use tiff::encoder::{colortype, Rational, TiffEncoder};
use tiff::tags::{ResolutionUnit, Tag};

pub struct TiffMeta<'a> {
    pub description: &'a str,
    pub trc: GrayTrc,
    pub orientation: u16,
    pub dpi: Option<f64>,
}

/// Quantise `clamp(v,0,1)*65535` and write a single-strip-set grey TIFF.
pub fn write_gray16(path: &Path, plane: &Plane, meta: &TiffMeta<'_>) -> Result<()> {
    let data: Vec<u16> =
        plane.data.par_iter().map(|&v| (v.clamp(0.0, 1.0) * 65535.0).round() as u16).collect();
    write_gray16_u16(path, plane.width, plane.height, &data, meta)
}

pub fn write_gray16_u16(
    path: &Path,
    width: usize,
    height: usize,
    data: &[u16],
    meta: &TiffMeta<'_>,
) -> Result<()> {
    let file = BufWriter::new(File::create(path)?);
    let mut enc = TiffEncoder::new(file)?;
    let mut img = enc.new_image::<colortype::Gray16>(width as u32, height as u32)?;
    img.rows_per_strip(64)?;
    let icc = gray_profile(meta.trc);
    {
        let d = img.encoder();
        d.write_tag(Tag::IccProfile, icc.as_slice())?;
        d.write_tag(Tag::ImageDescription, meta.description)?;
        d.write_tag(Tag::Software, concat!("mimizan ", env!("CARGO_PKG_VERSION")))?;
        if meta.orientation >= 1 && meta.orientation <= 8 {
            d.write_tag(Tag::Orientation, meta.orientation)?;
        }
    }
    if let Some(dpi) = meta.dpi {
        img.resolution(ResolutionUnit::Inch, Rational { n: (dpi * 1000.0).round() as u32, d: 1000 });
    }
    img.write_data(data)?;
    Ok(())
}

/// 8-bit grey TIFF (mask sidecar).
pub fn write_gray8(path: &Path, width: usize, height: usize, data: &[u8], description: &str) -> Result<()> {
    let file = BufWriter::new(File::create(path)?);
    let mut enc = TiffEncoder::new(file)?;
    let mut img = enc.new_image::<colortype::Gray8>(width as u32, height as u32)?;
    img.rows_per_strip(256)?;
    img.encoder().write_tag(Tag::ImageDescription, description)?;
    img.write_data(data)?;
    Ok(())
}
