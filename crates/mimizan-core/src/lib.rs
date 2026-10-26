//! mimizan-core: Bayer RAW to linear monochrome negative.
//!
//! The method is written up in `docs/PAPER.md`. The code and its tests are the contract.

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
pub mod paper;
pub mod peak;
pub mod pipeline;
pub mod plane;
pub mod plate;
pub mod preview;
pub mod print;
pub mod recipe;
pub mod resize;
pub mod separate;
pub mod synth;
pub mod thumb;
pub mod tiffin;
pub mod tiffout;
pub mod tone;
pub mod usm;
pub mod zone;

pub use error::{Error, Result};
