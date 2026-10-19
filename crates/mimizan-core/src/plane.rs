//! Row-major f64 image plane.

/// A single-channel image, row-major, `f64` per sample.
#[derive(Clone, Debug, PartialEq)]
pub struct Plane {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f64>,
}

impl Plane {
    pub fn zeros(width: usize, height: usize) -> Self {
        Self { width, height, data: vec![0.0; width * height] }
    }

    pub fn from_vec(width: usize, height: usize, data: Vec<f64>) -> Self {
        match Self::try_from_vec(width, height, data) {
            Ok(p) => p,
            Err(e) => panic!("{e}"),
        }
    }

    /// The buffer must be exactly `width * height` samples. A mismatch is an
    /// error, so a short mask or a bad decode does not abort the process.
    pub fn try_from_vec(width: usize, height: usize, data: Vec<f64>) -> crate::Result<Self> {
        let Some(n) = width.checked_mul(height) else {
            return Err(crate::Error::Invalid(format!("plane {width}×{height} does not fit")));
        };
        if data.len() != n {
            return Err(crate::Error::Invalid(format!(
                "plane is {width}×{height} ({n} samples) but the buffer has {}",
                data.len()
            )));
        }
        Ok(Self { width, height, data })
    }

    #[inline]
    pub fn at(&self, x: usize, y: usize) -> f64 {
        self.data[y * self.width + x]
    }

    #[inline]
    pub fn set(&mut self, x: usize, y: usize, v: f64) {
        self.data[y * self.width + x] = v;
    }

    pub fn row(&self, y: usize) -> &[f64] {
        &self.data[y * self.width..(y + 1) * self.width]
    }

    pub fn row_mut(&mut self, y: usize) -> &mut [f64] {
        &mut self.data[y * self.width..(y + 1) * self.width]
    }
}

#[cfg(test)]
mod tests {
    use super::Plane;

    #[test]
    fn a_short_buffer_is_an_error() {
        let err = Plane::try_from_vec(2, 2, vec![1.0]).unwrap_err();
        assert!(err.to_string().contains("2×2"), "{err}");
        assert!(err.to_string().contains("has 1"), "{err}");
    }
}
