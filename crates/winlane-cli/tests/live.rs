use std::path::{Path, PathBuf};

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::task::JoinHandle;
use winlane_cli::capture_transient_frame;
use winlane_contracts::{FrameRef, MachineHandle, MachineState};
use winlane_ipc::{ControlRequest, ControlResponse, UnixControlClient};
use winlane_view::Rgb8;

fn machine() -> MachineHandle {
    MachineHandle {
        id: "machine-1".to_string(),
        name: "live-test".to_string(),
        state_dir: "/tmp/live-test".to_string(),
        backend: "qemu".to_string(),
        state: MachineState::Running,
    }
}

fn ppm_fixture_bytes() -> Vec<u8> {
    let hex = include_str!("../../../tests/fixtures/terminal/frame-2x2.ppm.hex").trim();
    (0..hex.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&hex[index..index + 2], 16).expect("hex fixture byte"))
        .collect()
}

fn spawn_capture_server(
    socket_path: PathBuf,
    frame_path: PathBuf,
    metadata_result: serde_json::Value,
) -> JoinHandle<()> {
    let server_machine = machine();
    tokio::spawn(async move {
        let listener = UnixListener::bind(socket_path).expect("bind control socket");
        let (stream, _) = listener.accept().await.expect("accept control client");
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let mut request_bytes = Vec::new();
        reader
            .read_until(b'\n', &mut request_bytes)
            .await
            .expect("read request");
        let request: ControlRequest =
            serde_json::from_slice(&request_bytes).expect("decode control request");
        assert_eq!(
            request,
            ControlRequest::CaptureFrame {
                machine: server_machine
            }
        );
        assert_eq!(
            metadata_result
                .get("blob_path")
                .and_then(|path| path.as_str()),
            Some(frame_path.to_string_lossy().as_ref())
        );
        let response = ControlResponse {
            ok: true,
            result: Some(metadata_result),
            error: None,
        };
        let mut response_bytes = serde_json::to_vec(&response).expect("encode response");
        response_bytes.push(b'\n');
        write_half
            .write_all(&response_bytes)
            .await
            .expect("write response");
    })
}

fn valid_frame_metadata(frame_path: PathBuf) -> serde_json::Value {
    let frame = FrameRef {
        sha256: "69d84c9c40bbfe1bfa0519120af54a299af34be4eebb31bb6a34b67aaae22f00".to_string(),
        width: 2,
        height: 2,
        captured_unix_ms: 1,
        blob_path: frame_path.to_string_lossy().into_owned(),
    };
    serde_json::to_value(frame).expect("encode frame metadata")
}

async fn wait_for_socket(socket_path: &Path) {
    for _ in 0..100 {
        if socket_path.exists() {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    panic!("control socket did not appear");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_transient_frame_decodes_artifact_and_removes_transient_file() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let frame_path = root.path().join("frame.ppm");
    tokio::fs::write(&frame_path, ppm_fixture_bytes())
        .await
        .expect("write frame fixture");
    let server = spawn_capture_server(
        socket_path.clone(),
        frame_path.clone(),
        valid_frame_metadata(frame_path.clone()),
    );
    wait_for_socket(&socket_path).await;

    let client = UnixControlClient::new(socket_path);
    let captured = capture_transient_frame(&client, &machine())
        .await
        .expect("capture frame");
    server.await.expect("server task");

    assert_eq!(captured.frame.width(), 2);
    assert_eq!(captured.frame.height(), 2);
    assert_eq!(captured.frame.pixel(0, 0), Some(Rgb8::new(255, 0, 0)));
    assert_eq!(captured.frame.pixel(1, 0), Some(Rgb8::new(0, 255, 0)));
    assert_eq!(captured.frame.pixel(0, 1), Some(Rgb8::new(0, 0, 255)));
    assert_eq!(captured.frame.pixel(1, 1), Some(Rgb8::new(255, 255, 255)));
    assert!(!frame_path.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_transient_frame_removes_transient_file_after_artifact_parse_error() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let frame_path = root.path().join("frame.ppm");
    tokio::fs::write(&frame_path, b"not a ppm")
        .await
        .expect("write invalid frame fixture");
    let server = spawn_capture_server(
        socket_path.clone(),
        frame_path.clone(),
        valid_frame_metadata(frame_path.clone()),
    );
    wait_for_socket(&socket_path).await;

    let client = UnixControlClient::new(socket_path);
    let result = capture_transient_frame(&client, &machine()).await;
    server.await.expect("server task");

    assert!(result.is_err());
    assert!(!frame_path.exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_transient_frame_removes_transient_file_after_metadata_parse_error() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let frame_path = root.path().join("frame.ppm");
    tokio::fs::write(&frame_path, ppm_fixture_bytes())
        .await
        .expect("write frame fixture");
    let mut metadata = valid_frame_metadata(frame_path.clone());
    metadata["width"] = serde_json::Value::String("not a width".to_string());
    let server = spawn_capture_server(socket_path.clone(), frame_path.clone(), metadata);
    wait_for_socket(&socket_path).await;

    let client = UnixControlClient::new(socket_path);
    let result = capture_transient_frame(&client, &machine()).await;
    server.await.expect("server task");

    assert!(result.is_err());
    assert!(!frame_path.exists());
}
