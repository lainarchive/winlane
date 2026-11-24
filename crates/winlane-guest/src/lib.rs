//! In-guest helper protocol, validation, and framed message encoding.

pub mod qga;
pub mod uia;

use serde::{Deserialize, Serialize};
use std::io::{self, Read, Write};
use thiserror::Error;
use winlane_contracts::{FileTransferRef, ProcessResult, UiNodeRef};

pub const PROTOCOL_VERSION: u8 = 1;
pub const MAX_FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_CAPTURE_BYTES: u64 = 1024 * 1024;
pub const MAX_TRANSFER_BYTES: u64 = 64 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum GuestRequest {
    Hello {
        protocol_version: u8,
    },
    UiTree {
        max_depth: u16,
        max_nodes: u32,
    },
    Invoke {
        target: UiNodeRef,
    },
    SetText {
        target: UiNodeRef,
        text: String,
    },
    RunProcess {
        command: String,
        args: Vec<String>,
        timeout_ms: u64,
        capture_bytes: u64,
    },
    ImportFile {
        guest_path: String,
        sha256: String,
        bytes: u64,
    },
    ExportFile {
        guest_path: String,
        max_bytes: u64,
    },
    Shutdown,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GuestResponse {
    Hello {
        protocol_version: u8,
        helper_version: String,
    },
    UiTree {
        generation: u64,
        captured_unix_ms: u64,
        tree: serde_json::Value,
    },
    Invoke,
    SetText,
    Process {
        result: ProcessResult,
    },
    FileTransfer {
        transfer: FileTransferRef,
    },
    Shutdown,
    Error {
        code: GuestErrorCode,
        message: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GuestErrorCode {
    UnsupportedProtocol,
    InvalidRequest,
    ElementNotFound,
    ElementStale,
    AccessDenied,
    Timeout,
    Io,
    Internal,
}

#[derive(Debug, Error)]
pub enum GuestProtocolError {
    #[error("guest protocol I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("guest protocol JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("frame is {actual} bytes; maximum is {maximum}")]
    FrameTooLarge { actual: usize, maximum: usize },
    #[error("unsupported protocol version {0}")]
    UnsupportedProtocol(u8),
    #[error("request field is invalid")]
    InvalidRequest,
    #[error("command must be nonempty and contain no control characters")]
    InvalidCommand,
    #[error("argument contains a NUL or control character")]
    InvalidArgument,
    #[error("guest path must be nonempty and contain no control characters")]
    InvalidGuestPath,
    #[error("capture limit must be 1..={MAX_CAPTURE_BYTES} bytes")]
    InvalidCaptureLimit,
    #[error("transfer size must be 1..={MAX_TRANSFER_BYTES} bytes")]
    InvalidTransferSize,
    #[error("timeout must be 1..=600000 milliseconds")]
    InvalidTimeout,
    #[error("UI tree bounds must be depth 1..=32 and nodes 1..=20000")]
    InvalidUiTreeBounds,
    #[error("sha256 must be 64 lowercase hex characters")]
    InvalidSha256,
}

pub type GuestProtocolResult<T> = Result<T, GuestProtocolError>;

pub fn validate_request(request: &GuestRequest) -> GuestProtocolResult<()> {
    match request {
        GuestRequest::Hello { protocol_version } => {
            if *protocol_version == PROTOCOL_VERSION {
                Ok(())
            } else {
                Err(GuestProtocolError::UnsupportedProtocol(*protocol_version))
            }
        }
        GuestRequest::UiTree {
            max_depth,
            max_nodes,
        } => {
            if (1..=32).contains(max_depth) && (1..=20_000).contains(max_nodes) {
                Ok(())
            } else {
                Err(GuestProtocolError::InvalidUiTreeBounds)
            }
        }
        GuestRequest::Shutdown => Ok(()),
        GuestRequest::Invoke { target } => validate_node_ref(target),
        GuestRequest::SetText { target, text } => {
            validate_node_ref(target)?;
            validate_text(text)
        }
        GuestRequest::RunProcess {
            command,
            args,
            timeout_ms,
            capture_bytes,
        } => {
            validate_command(command)?;
            for arg in args {
                validate_argument(arg)?;
            }
            if !(1..=600_000).contains(timeout_ms) {
                return Err(GuestProtocolError::InvalidTimeout);
            }
            if !(1..=MAX_CAPTURE_BYTES).contains(capture_bytes) {
                return Err(GuestProtocolError::InvalidCaptureLimit);
            }
            Ok(())
        }
        GuestRequest::ImportFile {
            guest_path,
            sha256,
            bytes,
        } => {
            validate_guest_path(guest_path)?;
            validate_sha256(sha256)?;
            if !(1..=MAX_TRANSFER_BYTES).contains(bytes) {
                return Err(GuestProtocolError::InvalidTransferSize);
            }
            Ok(())
        }
        GuestRequest::ExportFile {
            guest_path,
            max_bytes,
        } => {
            validate_guest_path(guest_path)?;
            if !(1..=MAX_TRANSFER_BYTES).contains(max_bytes) {
                return Err(GuestProtocolError::InvalidTransferSize);
            }
            Ok(())
        }
    }
}

pub fn encode_frame<T: Serialize>(value: &T) -> GuestProtocolResult<Vec<u8>> {
    let payload = serde_json::to_vec(value)?;
    if payload.len() > MAX_FRAME_BYTES {
        return Err(GuestProtocolError::FrameTooLarge {
            actual: payload.len(),
            maximum: MAX_FRAME_BYTES,
        });
    }
    let length = (payload.len() as u32).to_be_bytes();
    let mut frame = Vec::with_capacity(payload.len() + 4);
    frame.extend_from_slice(&length);
    frame.extend_from_slice(&payload);
    Ok(frame)
}

pub fn write_frame<T: Serialize>(mut writer: impl Write, value: &T) -> GuestProtocolResult<()> {
    writer.write_all(&encode_frame(value)?)?;
    writer.flush()?;
    Ok(())
}

pub fn read_frame<T: for<'de> Deserialize<'de>>(mut reader: impl Read) -> GuestProtocolResult<T> {
    let mut length_bytes = [0_u8; 4];
    reader.read_exact(&mut length_bytes)?;
    let length = u32::from_be_bytes(length_bytes) as usize;
    if length > MAX_FRAME_BYTES {
        return Err(GuestProtocolError::FrameTooLarge {
            actual: length,
            maximum: MAX_FRAME_BYTES,
        });
    }
    let mut payload = vec![0_u8; length];
    reader.read_exact(&mut payload)?;
    Ok(serde_json::from_slice(&payload)?)
}

fn validate_command(command: &str) -> GuestProtocolResult<()> {
    if valid_strict_text(command, 32_767) {
        Ok(())
    } else {
        Err(GuestProtocolError::InvalidCommand)
    }
}

fn validate_argument(argument: &str) -> GuestProtocolResult<()> {
    if valid_strict_text(argument, 8192) {
        Ok(())
    } else {
        Err(GuestProtocolError::InvalidArgument)
    }
}

fn validate_text(text: &str) -> GuestProtocolResult<()> {
    if valid_text_input(text, 8192) {
        Ok(())
    } else {
        Err(GuestProtocolError::InvalidArgument)
    }
}

fn validate_guest_path(path: &str) -> GuestProtocolResult<()> {
    if valid_strict_text(path, 32_767) {
        Ok(())
    } else {
        Err(GuestProtocolError::InvalidGuestPath)
    }
}

fn valid_strict_text(text: &str, maximum: usize) -> bool {
    !text.is_empty()
        && text.len() <= maximum
        && text.chars().all(|c| !c.is_control())
        && !text.contains('\0')
}

fn valid_text_input(text: &str, maximum: usize) -> bool {
    text.len() <= maximum
        && text
            .chars()
            .all(|c| !c.is_control() || c == '\t' || c == '\n' || c == '\r')
        && !text.contains('\0')
}

fn validate_node_ref(target: &UiNodeRef) -> GuestProtocolResult<()> {
    if !target.id.is_empty()
        && target.id.len() <= 1024
        && target.id.chars().all(|c| !c.is_control())
    {
        Ok(())
    } else {
        Err(GuestProtocolError::InvalidRequest)
    }
}

fn validate_sha256(sha256: &str) -> GuestProtocolResult<()> {
    if sha256.len() == 64
        && sha256
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        Ok(())
    } else {
        Err(GuestProtocolError::InvalidSha256)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> UiNodeRef {
        UiNodeRef {
            id: "node-1".into(),
            generation: 1,
            automation_id: Some("edit".into()),
            control_type: Some("Edit".into()),
            bounds: Some([10, 10, 100, 30]),
        }
    }

    #[test]
    fn validates_process_request() {
        let request = GuestRequest::RunProcess {
            command: "notepad.exe".into(),
            args: vec![],
            timeout_ms: 5_000,
            capture_bytes: 4096,
        };
        assert!(validate_request(&request).is_ok());
    }

    #[test]
    fn rejects_control_characters_in_process_input() {
        let request = GuestRequest::RunProcess {
            command: "bad\0.exe".into(),
            args: vec![],
            timeout_ms: 5_000,
            capture_bytes: 4096,
        };
        assert!(matches!(
            validate_request(&request),
            Err(GuestProtocolError::InvalidCommand)
        ));
    }

    #[test]
    fn accepts_empty_set_text() {
        let request = GuestRequest::SetText {
            target: target(),
            text: String::new(),
        };
        assert!(validate_request(&request).is_ok());
    }

    #[test]
    fn frame_round_trips_invoke_request() {
        let request = GuestRequest::Invoke { target: target() };
        let frame = encode_frame(&request).unwrap();
        let decoded: GuestRequest = read_frame(frame.as_slice()).unwrap();
        assert_eq!(decoded, request);
    }

    #[test]
    fn rejects_oversized_frame_header() {
        let mut frame = ((MAX_FRAME_BYTES + 1) as u32).to_be_bytes().to_vec();
        frame.extend_from_slice(&[0_u8; 8]);
        let result = read_frame::<serde_json::Value>(frame.as_slice());
        assert!(matches!(
            result,
            Err(GuestProtocolError::FrameTooLarge { .. })
        ));
    }
}
