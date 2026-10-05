//! mimizan-core: Bayer RAW to linear monochrome negative.
//!
//! The contract for every number in this crate is `docs/SPEC.md`.

pub mod baseline;
pub mod bench;
pub mod blockfft;
pub mod calib;
pub mod calibrate;
pub mod cfa;
pub mod curve;
pub mod decode;
pub mod deconv;
pub mod dubois;
pub mod error;
pub mod filter;
pub mod icc;
pub mod ingest;
pub mod kodak;
pub mod mask;
pub mod metrics;
pub mod mix;
pub mod pipeline;
pub mod plane;
pub mod preview;
pub mod print;
pub mod reconstruct;
pub mod resize;
pub mod screen;
pub mod separate;
pub mod synth;
pub mod tiffin;
pub mod tiffout;
pub mod usm;

pub use error::{Error, Result};

/// Crate version, the `mimizan` field every negative and print records.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
