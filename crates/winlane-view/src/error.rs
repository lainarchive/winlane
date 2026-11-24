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
    #[error("{protocol} was detected but its renderer is not implemented")]
    UnsupportedProtocol { protocol: &'static str },
    #[error("terminal output failed")]
    Output(#[from] io::Error),
}

pub type ViewResult<T> = Result<T, ViewError>;
