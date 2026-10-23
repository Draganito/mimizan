//! Read back our own grey TIFFs (negatives, masks).

use crate::error::{Error, Result};
use crate::plane::Plane;
use rayon::prelude::*;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;
use tiff::decoder::{Decoder, DecodingResult};
use tiff::tags::Tag;

/// A grey TIFF as read: pixels, description, and the two tags the print
/// pass needs.
pub struct GrayTiff {
    /// Samples scaled to [0, 1] by the file's bit depth.
    pub plane: Plane,
    /// `ImageDescription`, when present.
    pub description: Option<String>,
    /// 8 or 16.
    pub bits: u16,
    /// TIFF orientation 1..=8, 1 when absent.
    pub orientation: u16,
}

/// 8- or 16-bit single-channel TIFF to a Plane in [0,1].
pub fn read_gray(path: &Path) -> Result<GrayTiff> {
    let f = BufReader::new(File::open(path)?);
    let mut dec = Decoder::new(f)?;
    let (w, h) = dec.dimensions()?;
    let description = dec.get_tag_ascii_string(Tag::ImageDescription).ok();
    let orientation =
        dec.get_tag_u32(Tag::Orientation).ok().filter(|o| (1..=8).contains(o)).unwrap_or(1) as u16;
    let img = dec.read_image()?;
    let (data, bits): (Vec<f64>, u16) = match img {
        DecodingResult::U16(v) => (v.par_iter().map(|&x| f64::from(x) / 65535.0).collect(), 16),
        DecodingResult::U8(v) => (v.par_iter().map(|&x| f64::from(x) / 255.0).collect(), 8),
        _ => return Err(Error::Invalid("only 8/16-bit grey TIFF is readable".into())),
    };
    if data.len() != (w as usize).saturating_mul(h as usize) {
        return Err(Error::Invalid("TIFF is not single-channel".into()));
    }
    Ok(GrayTiff { plane: Plane::try_from_vec(w as usize, h as usize, data)?, description, bits, orientation })
}

/// Only the ImageDescription, without decoding pixels.
pub fn read_gray_header(path: &Path) -> Result<Option<String>> {
    let f = BufReader::new(File::open(path)?);
    let mut dec = Decoder::new(f)?;
    Ok(dec.get_tag_ascii_string(Tag::ImageDescription).ok())
}

/// Mask sidecar as raw u8 values, `(width, height, samples)`, exactly one
/// sample per pixel.
pub fn read_mask8(path: &Path) -> Result<(usize, usize, Vec<u8>)> {
    let f = BufReader::new(File::open(path)?);
    let mut dec = Decoder::new(f)?;
    let (w, h) = dec.dimensions()?;
    let (w, h) = (w as usize, h as usize);
    match dec.read_image()? {
        DecodingResult::U8(v) if v.len() == w.saturating_mul(h) => Ok((w, h, v)),
        _ => Err(Error::Invalid("mask sidecar must be 8-bit single-channel".into())),
    }
}
