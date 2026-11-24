//! Unix-socket control protocol and serialized server-side access to QEMU state.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use winlane_contracts::{CheckpointRef, ControlEvent, MachineHandle, MachineSpec};
use winlane_core::{CoreError, QemuBackend};

const MAX_IPC_MESSAGE_BYTES: usize = 1024 * 1024;

pub type IpcResult<T> = Result<T, IpcError>;

#[derive(Debug, Error)]
pub enum IpcError {
    #[error(transparent)]
    Core(#[from] CoreError),
    #[error("ipc io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("ipc serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("ipc protocol error: {0}")]
    Protocol(String),
    #[error("ipc connection closed")]
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ControlRequest {
    Create {
        spec: MachineSpec,
    },
    Start {
        machine: MachineHandle,
    },
    Stop {
        machine: MachineHandle,
    },
    Status {
        machine: MachineHandle,
    },
    CloneMachine {
        machine: MachineHandle,
        name: String,
    },
    Destroy {
        machine: MachineHandle,
    },
    Checkpoint {
        machine: MachineHandle,
        label: String,
    },
    Restore {
        checkpoint: CheckpointRef,
    },
    CaptureFrame {
        machine: MachineHandle,
    },
    Control {
        machine: MachineHandle,
        event: ControlEvent,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct ControlResponse {
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl ControlResponse {
    fn success(result: Value) -> Self {
        Self {
            ok: true,
            result: Some(result),
            error: None,
        }
    }

    fn failure(error: impl Into<String>) -> Self {
        Self {
            ok: false,
            result: None,
            error: Some(error.into()),
        }
    }
}

/// Line-delimited JSON client. Each call uses a short-lived connection, so a
/// failed call cannot leave partially written command state behind.
pub struct UnixControlClient {
    socket_path: PathBuf,
}

impl UnixControlClient {
    pub fn new(socket_path: impl Into<PathBuf>) -> Self {
        Self {
            socket_path: socket_path.into(),
        }
    }

    pub async fn invoke(&self, request: &ControlRequest) -> IpcResult<Value> {
        let stream = UnixStream::connect(&self.socket_path).await?;
        invoke_on_stream(stream, request).await
    }
}

pub async fn invoke_on_stream(stream: UnixStream, request: &ControlRequest) -> IpcResult<Value> {
    let (read_half, mut write_half) = stream.into_split();
    let mut encoded = serde_json::to_vec(request)?;
    encoded.push(b'\n');
    write_half.write_all(&encoded).await?;
    write_half.shutdown().await?;
    let mut reader = BufReader::new(read_half);
    let bytes = read_bounded_line(&mut reader).await?;
    let response: ControlResponse = serde_json::from_slice(&bytes)?;
    if response.ok {
        Ok(response.result.unwrap_or(Value::Null))
    } else {
        Err(IpcError::Protocol(
            response
                .error
                .unwrap_or_else(|| "control request failed".to_string()),
        ))
    }
}

/// Serve newline-delimited control requests. Every request is serialized
/// through one backend mutex, keeping concurrent clients from interleaving
/// writes to the same QMP monitor connection.
pub async fn serve_unix_control(
    socket_path: impl AsRef<Path>,
    backend: QemuBackend,
) -> IpcResult<()> {
    let socket_path = socket_path.as_ref();
    if tokio::fs::try_exists(socket_path).await? {
        return Err(IpcError::Protocol(format!(
            "control socket already exists: {}",
            socket_path.display()
        )));
    }
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let listener = UnixListener::bind(socket_path)?;
    tokio::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600)).await?;
    let backend = Arc::new(Mutex::new(backend));
    loop {
        let (stream, _) = listener.accept().await?;
        let backend = Arc::clone(&backend);
        tokio::spawn(async move {
            let _ = handle_connection(stream, backend).await;
        });
    }
}

async fn handle_connection(stream: UnixStream, backend: Arc<Mutex<QemuBackend>>) -> IpcResult<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    loop {
        let bytes = match read_bounded_line(&mut reader).await {
            Ok(bytes) => bytes,
            Err(IpcError::Closed) => return Ok(()),
            Err(error) => return Err(error),
        };
        let response = match serde_json::from_slice::<ControlRequest>(&bytes) {
            Ok(request) => dispatch(request, Arc::clone(&backend)).await,
            Err(error) => ControlResponse::failure(format!("invalid request: {error}")),
        };
        let mut encoded = serde_json::to_vec(&response)?;
        encoded.push(b'\n');
        write_half.write_all(&encoded).await?;
        write_half.flush().await?;
    }
}

async fn dispatch(request: ControlRequest, backend: Arc<Mutex<QemuBackend>>) -> ControlResponse {
    let mut backend = backend.lock().await;
    let result = match request {
        ControlRequest::Create { spec } => backend
            .create_machine(&spec)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
        ControlRequest::Start { machine } => backend
            .start_machine(&machine)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
        ControlRequest::Stop { machine } => backend
            .stop_machine(&machine)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
        ControlRequest::Status { machine } => backend
            .status_machine(&machine)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
        ControlRequest::CloneMachine { machine, name } => backend
            .clone_machine(&machine, &name)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
        ControlRequest::Destroy { machine } => backend
            .destroy_machine(&machine)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
        ControlRequest::Checkpoint { machine, label } => backend
            .checkpoint_machine(&machine, &label)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
        ControlRequest::Restore { checkpoint } => backend
            .restore_checkpoint(&checkpoint)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
        ControlRequest::CaptureFrame { machine } => backend
            .capture_frame(&machine)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
        ControlRequest::Control { machine, event } => backend
            .send_control_event(&machine, &event)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
    };
    match result {
        Ok(value) => ControlResponse::success(value),
        Err(error) => ControlResponse::failure(error.to_string()),
    }
}

async fn read_bounded_line(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> IpcResult<Vec<u8>> {
    let mut bytes = Vec::new();
    loop {
        let byte = match reader.read_u8().await {
            Ok(byte) => byte,
            Err(error) if error.kind() == std::io::ErrorKind::UnexpectedEof => {
                if bytes.is_empty() {
                    return Err(IpcError::Closed);
                }
                return Err(IpcError::Protocol(
                    "connection closed in the middle of a message".to_string(),
                ));
            }
            Err(error) => return Err(IpcError::Io(error)),
        };
        if byte == b'\n' {
            return Ok(bytes);
        }
        if bytes.len() >= MAX_IPC_MESSAGE_BYTES {
            return Err(IpcError::Protocol(
                "IPC message exceeded one MiB".to_string(),
            ));
        }
        bytes.push(byte);
    }
}

pub fn implementation_status() -> &'static str {
    "unix-control-serialization"
}
