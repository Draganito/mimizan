//! The "standard way" for comparison only: bilinear demosaic, then (R+2G+B)/4.
//! Never used in the product path.

use crate::cfa::{BayerPhase, Color};
use crate::plane::Plane;
use rayon::prelude::*;

#[inline]
fn at(p: &Plane, x: isize, y: isize) -> f64 {
    let w = p.width as isize;
    let h = p.height as isize;
    let xr = if x < 0 {
        -x
    } else if x >= w {
        2 * w - 2 - x
    } else {
        x
    };
    let yr = if y < 0 {
        -y
    } else if y >= h {
        2 * h - 2 - y
    } else {
        y
    };
    p.at(xr as usize, yr as usize)
}

pub fn bilinear_luminance(m: &Plane, phase: BayerPhase) -> Plane {
    let w = m.width;
    let mut out = Plane::zeros(w, m.height);
    out.data.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let yi = y as isize;
        for (x, o) in row.iter_mut().enumerate() {
            let xi = x as isize;
            let here = m.at(x, y);
            let cross =
                0.25 * (at(m, xi - 1, yi) + at(m, xi + 1, yi) + at(m, xi, yi - 1) + at(m, xi, yi + 1));
            let diag = 0.25
                * (at(m, xi - 1, yi - 1)
                    + at(m, xi + 1, yi - 1)
                    + at(m, xi - 1, yi + 1)
                    + at(m, xi + 1, yi + 1));
            let horiz = 0.5 * (at(m, xi - 1, yi) + at(m, xi + 1, yi));
            let vert = 0.5 * (at(m, xi, yi - 1) + at(m, xi, yi + 1));
            let (r, g, b) = match phase.color_at(x, y) {
                Color::R => (here, cross, diag),
                Color::B => (diag, cross, here),
                Color::G => {
                    // Row neighbours are the other colour of this row.
                    let row_color = phase.color_at(x + 1, y);
                    if row_color == Color::R {
                        (horiz, here, vert)
                    } else {
                        (vert, here, horiz)
                    }
                }
            };
            *o = (r + 2.0 * g + b) / 4.0;
        }
    });
    out
}
