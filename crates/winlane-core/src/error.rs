use std::path::PathBuf;

use thiserror::Error;
use winlane_contracts::ContractError;

use crate::qmp::QmpError;

pub type CoreResult<T> = Result<T, CoreError>;

#[derive(Debug, Error)]
pub enum CoreError {
    #[error(transparent)]
    Contract(#[from] ContractError),
    #[error(transparent)]
    Qmp(#[from] QmpError),
    #[error("io error during {operation} on {}: {source}", path.display())]
    Io {
        operation: &'static str,
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("machine {machine} is {actual}, expected {expected}")]
    InvalidState {
        machine: String,
        actual: &'static str,
        expected: &'static str,
    },
    #[error("machine not found: {0}")]
    MachineNotFound(String),
    #[error("invalid machine handle: {0}")]
    InvalidHandle(String),
    #[error("qemu command failed: {0}")]
    Qemu(String),
    #[error("unsupported control operation: {0}")]
    Unsupported(&'static str),
    #[error("timed out waiting for {0}")]
    Timeout(&'static str),
}

impl CoreError {
    pub(crate) fn io(
        operation: &'static str,
        path: impl Into<PathBuf>,
    ) -> impl FnOnce(std::io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io {
            operation,
            path,
            source,
        }
    }
}

impl From<CoreError> for ContractError {
    fn from(value: CoreError) -> Self {
        ContractError::new(value.to_string())
    }
}
