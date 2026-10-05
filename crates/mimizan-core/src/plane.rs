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
        assert_eq!(data.len(), width * height, "plane size mismatch");
        Self { width, height, data }
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
