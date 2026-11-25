//! The Bayer least-squares demosaic.
//!
//! The bench in the `mimizan` command measured it. The desktop writes a
//! camera file out with it when *least-squares demosaic* is on, and then
//! opens that DNG the way it opens a Fuji X-Trans file.
pub mod bayer2;
pub mod bayer_ls;
