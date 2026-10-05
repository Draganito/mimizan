//! AMaZE, RCD, DCB and LMMSE, the four demosaicers that lead the Kodak
//! comparison, via the vendored [librtprocess](https://github.com/CarVac/librtprocess)
//! (GPL-3.0-or-later).
//!
//! Mimizan Lab is GPL-3.0-or-later. This crate links librtprocess. The
//! separation does not call these demosaicers.
//!
//! Samples in and out are linear and normalised to 0..1, one plane, row-major.
//! The CFA tile is `[[(0,0), (1,0)], [(0,1), (1,1)]]` with 0 = red, 1 = green,
//! 2 = blue.

use std::fmt;

/// Which demosaicer to run. Not the Mimizan separation.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    Amaze,
    Rcd,
    Dcb,
    Lmmse,
}

impl Method {
    pub fn label(self) -> &'static str {
        match self {
            Self::Amaze => "AMaZE",
            Self::Rcd => "RCD",
            Self::Dcb => "DCB",
            Self::Lmmse => "LMMSE",
        }
    }

    fn code(self) -> i32 {
        match self {
            Self::Amaze => 0,
            Self::Rcd => 1,
            Self::Dcb => 2,
            Self::Lmmse => 3,
        }
    }
}

impl fmt::Display for Method {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// Full-resolution red, green and blue after demosaicing.
pub struct Rgb {
    pub r: Vec<f64>,
    pub g: Vec<f64>,
    pub b: Vec<f64>,
}

#[derive(Debug)]
pub struct DemosaicError(String);

impl fmt::Display for DemosaicError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DemosaicError {}

/// Demosaic one Bayer mosaic. `raw` is row-major, length `width * height`.
/// `cfa` uses 0 = red, 1 = green, 2 = blue.
pub fn demosaic(
    method: Method,
    width: usize,
    height: usize,
    raw: &[f64],
    cfa: [[u32; 2]; 2],
) -> Result<Rgb, DemosaicError> {
    if width < 64 || height < 64 {
        return Err(DemosaicError("demosaic needs at least 64×64".into()));
    }
    if !width.is_multiple_of(2) || !height.is_multiple_of(2) {
        return Err(DemosaicError("demosaic needs an even width and height".into()));
    }
    if raw.len() != width * height {
        return Err(DemosaicError("mosaic length does not match the size".into()));
    }
    let raw_f: Vec<f32> = raw.iter().map(|&v| v.clamp(0.0, 1.0) as f32).collect();
    let mut r = vec![0f32; raw.len()];
    let mut g = vec![0f32; raw.len()];
    let mut b = vec![0f32; raw.len()];
    let rc = unsafe {
        let raw_rows = row_ptrs(raw_f.as_ptr(), width, height);
        let mut r_rows = row_ptrs_mut(r.as_mut_ptr(), width, height);
        let mut g_rows = row_ptrs_mut(g.as_mut_ptr(), width, height);
        let mut b_rows = row_ptrs_mut(b.as_mut_ptr(), width, height);
        mimizan_rt_demosaic(
            method.code(),
            width as i32,
            height as i32,
            raw_rows.as_ptr(),
            r_rows.as_mut_ptr(),
            g_rows.as_mut_ptr(),
            b_rows.as_mut_ptr(),
            cfa.as_ptr(),
        )
    };
    if rc != 0 {
        return Err(DemosaicError(format!("{method} failed (code {rc})")));
    }
    let to64 = |p: Vec<f32>| p.into_iter().map(|v| f64::from(v).clamp(0.0, 1.0)).collect();
    Ok(Rgb { r: to64(r), g: to64(g), b: to64(b) })
}

fn row_ptrs(base: *const f32, width: usize, height: usize) -> Vec<*const f32> {
    (0..height).map(|y| unsafe { base.add(y * width) }).collect()
}

fn row_ptrs_mut(base: *mut f32, width: usize, height: usize) -> Vec<*mut f32> {
    (0..height).map(|y| unsafe { base.add(y * width) }).collect()
}

extern "C" {
    fn mimizan_rt_demosaic(
        method: i32,
        width: i32,
        height: i32,
        raw: *const *const f32,
        red: *mut *mut f32,
        green: *mut *mut f32,
        blue: *mut *mut f32,
        cfa: *const [u32; 2],
    ) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Flat grey RGGB mosaic. Every demosaicer should come back near that grey.
    #[test]
    fn flat_grey_stays_grey() {
        let (w, h) = (64, 64);
        let raw = vec![0.4; w * h];
        let cfa = [[0, 1], [1, 2]];
        for method in [Method::Amaze, Method::Rcd, Method::Dcb, Method::Lmmse] {
            let rgb = demosaic(method, w, h, &raw, cfa).unwrap();
            let mid = (h / 2) * w + w / 2;
            for (name, plane) in [("r", &rgb.r), ("g", &rgb.g), ("b", &rgb.b)] {
                let v = plane[mid];
                assert!((v - 0.4).abs() < 0.02, "{method} {name} at the centre is {v}, expected 0.4");
            }
        }
    }
}
