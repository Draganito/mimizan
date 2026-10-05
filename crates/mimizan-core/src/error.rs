use thiserror::Error;

#[derive(Debug, Error)]
pub enum Error {
    #[error("decode failed: {0}")]
    Decode(String),
    #[error("unsupported sensor: {0}")]
    Unsupported(String),
    #[error("invalid input: {0}")]
    Invalid(String),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error("tiff: {0}")]
    Tiff(#[from] tiff::TiffError),
    #[error("json: {0}")]
    Json(#[from] serde_json::Error),
    #[error("image: {0}")]
    Image(#[from] image::ImageError),
}

pub type Result<T> = std::result::Result<T, Error>;
