//! Unix-socket control protocol and serialized server-side access to QEMU state.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, mpsc};
use winlane_contracts::MachineState;
use winlane_contracts::{CheckpointRef, ControlEvent, MachineHandle, MachineSpec};
use winlane_core::{CoreError, CoreResult, QemuBackend};
use winlane_guest::qga::{QgaClient, UnixQgaTransport};

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
    List,
    Start {
        machine: MachineHandle,
    },
    Stop {
        machine: MachineHandle,
    },
    Status {
        machine: MachineHandle,
    },
    GuestAgentReady {
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
    CheckpointList {
        machine: MachineHandle,
    },
    CheckpointDelete {
        machine: MachineHandle,
        checkpoint_id: String,
    },
    CaptureFrame {
        machine: MachineHandle,
    },
    Shutdown,
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
        match UnixStream::connect(socket_path).await {
            Ok(_) => {
                return Err(IpcError::Protocol(format!(
                    "control socket is already active: {}",
                    socket_path.display()
                )));
            }
            Err(_) => {
                tokio::fs::remove_file(socket_path).await?;
            }
        }
    }
    if let Some(parent) = socket_path.parent() {
        tokio::fs::create_dir_all(parent).await?;
    }
    let listener = UnixListener::bind(socket_path)?;
    tokio::fs::set_permissions(socket_path, std::fs::Permissions::from_mode(0o600)).await?;
    let backend = Arc::new(Mutex::new(backend));
    let shutting_down = Arc::new(AtomicBool::new(false));
    let (shutdown_tx, mut shutdown_rx) = mpsc::channel(1);
    let mut terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?;
    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, _) = accepted?;
                let backend = Arc::clone(&backend);
                let shutting_down = Arc::clone(&shutting_down);
                let shutdown_tx = shutdown_tx.clone();
                tokio::spawn(async move {
                    let _ = handle_connection(stream, backend, shutting_down, shutdown_tx).await;
                });
            }
            interrupted = tokio::signal::ctrl_c() => {
                interrupted?;
                shutting_down.store(true, Ordering::SeqCst);
                break;
            }
            terminated = terminate.recv() => {
                if terminated.is_some() {
                    shutting_down.store(true, Ordering::SeqCst);
                    break;
                }
            }
            shutdown_requested = shutdown_rx.recv() => {
                if shutdown_requested.is_some() {
                    shutting_down.store(true, Ordering::SeqCst);
                    break;
                }
            }
        }
    }
    shutting_down.store(true, Ordering::SeqCst);
    let shutdown_result = {
        let mut backend = backend.lock().await;
        backend.shutdown_machines().await
    };
    if tokio::fs::try_exists(socket_path).await? {
        tokio::fs::remove_file(socket_path).await?;
    }
    shutdown_result?;
    Ok(())
}

async fn handle_connection(
    stream: UnixStream,
    backend: Arc<Mutex<QemuBackend>>,
    shutting_down: Arc<AtomicBool>,
    shutdown_tx: mpsc::Sender<()>,
) -> IpcResult<()> {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    loop {
        let bytes = match read_bounded_line(&mut reader).await {
            Ok(bytes) => bytes,
            Err(IpcError::Closed) => return Ok(()),
            Err(error) => return Err(error),
        };
        let (response, shutdown_request) = match serde_json::from_slice::<ControlRequest>(&bytes) {
            Ok(request) => {
                let shutdown_request = matches!(request, ControlRequest::Shutdown);
                let response =
                    dispatch(request, Arc::clone(&backend), Arc::clone(&shutting_down)).await;
                (response, shutdown_request)
            }
            Err(error) => (
                ControlResponse::failure(format!("invalid request: {error}")),
                false,
            ),
        };
        let mut encoded = serde_json::to_vec(&response)?;
        encoded.push(b'\n');
        write_half.write_all(&encoded).await?;
        write_half.flush().await?;
        if shutdown_request && response.ok {
            let _ = shutdown_tx.try_send(());
        }
    }
}

async fn dispatch(
    request: ControlRequest,
    backend: Arc<Mutex<QemuBackend>>,
    shutting_down: Arc<AtomicBool>,
) -> ControlResponse {
    if shutting_down.load(Ordering::SeqCst) {
        return ControlResponse::failure("control daemon is shutting down");
    }
    if matches!(request, ControlRequest::Shutdown) {
        shutting_down.store(true, Ordering::SeqCst);
        return ControlResponse::success(serde_json::json!({"shutdown":"initiated"}));
    }
    if let ControlRequest::GuestAgentReady { machine } = request {
        let result = guest_agent_ready(backend, machine).await;
        return match result {
            Ok(value) => ControlResponse::success(value),
            Err(error) => ControlResponse::failure(error.to_string()),
        };
    }
    let mut backend = backend.lock().await;
    let result = match request {
        ControlRequest::Create { spec } => backend
            .create_machine(&spec)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
        ControlRequest::List => backend
            .list_machines()
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
        ControlRequest::GuestAgentReady { .. } => Err(CoreError::InvalidHandle(
            "guest readiness dispatch invariant violated".to_string(),
        )),
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
        ControlRequest::CheckpointList { machine } => backend.list_checkpoints(&machine).await,
        ControlRequest::CheckpointDelete {
            machine,
            checkpoint_id,
        } => backend
            .delete_checkpoint(&machine, &checkpoint_id)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
        ControlRequest::CaptureFrame { machine } => backend
            .capture_frame(&machine)
            .await
            .and_then(|value| serde_json::to_value(value).map_err(CoreError::from)),
        ControlRequest::Shutdown => Err(CoreError::InvalidHandle(
            "shutdown dispatch invariant violated".to_string(),
        )),
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

async fn guest_agent_ready(
    backend: Arc<Mutex<QemuBackend>>,
    machine: MachineHandle,
) -> CoreResult<serde_json::Value> {
    let (state, path) = {
        let mut backend = backend.lock().await;
        let state = backend.status_machine(&machine).await?;
        let path = backend.guest_agent_socket_path(&machine).await?;
        (state, path)
    };
    let mut ready = false;
    if matches!(state, MachineState::Running)
        && let Some(path) = path
    {
        ready = probe_guest_agent(path).await;
    }
    serde_json::to_value(serde_json::json!({
        "ready": ready,
        "state": state,
    }))
    .map_err(CoreError::from)
}

async fn probe_guest_agent(path: std::path::PathBuf) -> bool {
    let timeout = std::time::Duration::from_millis(250);
    let stream = match tokio::time::timeout(timeout, tokio::net::UnixStream::connect(path)).await {
        Ok(Ok(stream)) => stream,
        Ok(Err(_)) | Err(_) => return false,
    };
    tokio::task::spawn_blocking(move || {
        let stream = stream.into_std()?;
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let transport = UnixQgaTransport::from_stream(stream)?;
        let mut client = QgaClient::new(transport);
        client
            .ping()
            .map_err(|error| std::io::Error::other(error.to_string()))
    })
    .await
    .map(|result| result.is_ok())
    .unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn dispatch_rejects_requests_once_shutdown_begins() {
        let root = tempfile::tempdir().expect("temporary machine root");
        let backend = QemuBackend::new(root.path()).await.expect("backend");
        let backend = Arc::new(Mutex::new(backend));
        let shutting_down = Arc::new(AtomicBool::new(true));
        let response = dispatch(
            ControlRequest::GuestAgentReady {
                machine: MachineHandle {
                    id: "machine".to_string(),
                    name: "machine".to_string(),
                    state_dir: root.path().to_string_lossy().into_owned(),
                    backend: "qemu".to_string(),
                    state: winlane_contracts::MachineState::Defined,
                },
            },
            backend,
            shutting_down,
        )
        .await;
        assert!(!response.ok);
        assert_eq!(
            response.error.as_deref(),
            Some("control daemon is shutting down")
        );
    }

    #[tokio::test]
    async fn shutdown_dispatch_rejects_new_requests_after_success() {
        let root = tempfile::tempdir().expect("temporary machine root");
        let backend = QemuBackend::new(root.path()).await.expect("backend");
        let backend = Arc::new(Mutex::new(backend));
        let shutting_down = Arc::new(AtomicBool::new(false));
        let response = dispatch(
            ControlRequest::Shutdown,
            Arc::clone(&backend),
            Arc::clone(&shutting_down),
        )
        .await;
        assert!(response.ok);
        assert_eq!(
            response.result,
            Some(serde_json::json!({"shutdown":"initiated"}))
        );
        let rejected = dispatch(ControlRequest::List, backend, shutting_down).await;
        assert!(!rejected.ok);
        assert_eq!(
            rejected.error.as_deref(),
            Some("control daemon is shutting down")
        );
    }

    #[tokio::test]
    async fn qga_probe_missing_socket_is_false() {
        let root = tempfile::tempdir().expect("temporary root");
        assert!(!probe_guest_agent(root.path().join("missing.sock")).await);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn qga_probe_accepts_live_agent_response() {
        let root = tempfile::tempdir().expect("temporary root");
        let path = root.path().join("qga.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind qga socket");
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept");
            let mut request = Vec::new();
            loop {
                let mut byte = [0];
                stream.read_exact(&mut byte).await.expect("read request");
                request.push(byte[0]);
                if byte[0] == b'\n' {
                    break;
                }
            }
            let request: serde_json::Value =
                serde_json::from_slice(&request).expect("decode ping request");
            assert_eq!(request["execute"], "guest-ping");
            stream
                .write_all(br#"{"return":{}}"#)
                .await
                .expect("write response");
            stream.write_all(b"\r\n").await.expect("write newline");
        });
        assert!(probe_guest_agent(path).await);
        server.await.expect("server task");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn qga_probe_unresponsive_socket_is_bounded_false() {
        let root = tempfile::tempdir().expect("temporary root");
        let path = root.path().join("qga.sock");
        let listener = tokio::net::UnixListener::bind(&path).expect("bind qga socket");
        let server = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.expect("accept");
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
        });
        let started = std::time::Instant::now();
        assert!(!probe_guest_agent(path).await);
        assert!(started.elapsed() < std::time::Duration::from_secs(1));
        server.abort();
    }
}
