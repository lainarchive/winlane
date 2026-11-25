use std::io;

use thiserror::Error;

#[derive(Debug, Error)]
pub enum ViewError {
    #[error("frame dimensions must be nonzero")]
    EmptyFrame,
    #[error("frame has {actual} pixels but {expected} were required for {width}x{height}")]
    PixelLengthMismatch {
        width: u32,
        height: u32,
        expected: usize,
        actual: usize,
    },
    #[error("frame has {pixels} pixels; maximum supported is {maximum}")]
    FrameTooLarge { pixels: usize, maximum: usize },
    #[error("terminal geometry dimensions must be nonzero")]
    InvalidGeometry,
    #[error("zoom must be 100..=400 percent, got {zoom_percent}")]
    InvalidZoom { zoom_percent: u16 },
    #[error("crop rectangle is outside the frame")]
    CropOutOfBounds,
    #[error("{protocol} was detected but its renderer is not implemented")]
    UnsupportedProtocol { protocol: &'static str },
    #[error("invalid frame artifact: {0}")]
    InvalidFrameArtifact(String),
    #[error(
        "frame artifact dimensions {actual_width}x{actual_height} do not match metadata {expected_width}x{expected_height}"
    )]
    FrameArtifactDimensionMismatch {
        expected_width: u32,
        expected_height: u32,
        actual_width: u32,
        actual_height: u32,
    },
    #[error("frame artifact sha256 does not match metadata")]
    FrameArtifactHashMismatch,
    #[error("PNG encoding failed: {0}")]
    ImageEncode(#[from] png::EncodingError),
    #[error("terminal output failed")]
    Output(#[from] io::Error),
}

pub type ViewResult<T> = Result<T, ViewError>;
