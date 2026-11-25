use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use winlane_cli::capture_frame;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn capture_frame_uses_serialized_control_ipc_and_decodes_artifact() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let frame_path = root.path().join("frame.ppm");
    tokio::fs::write(&frame_path, ppm_fixture_bytes())
        .await
        .expect("write frame fixture");

    let server_socket = socket_path.clone();
    let server_frame_path = frame_path.clone();
    let server_machine = machine();
    let server = tokio::spawn(async move {
        let listener = UnixListener::bind(server_socket).expect("bind control socket");
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

        let frame = FrameRef {
            sha256: "69d84c9c40bbfe1bfa0519120af54a299af34be4eebb31bb6a34b67aaae22f00".to_string(),
            width: 2,
            height: 2,
            captured_unix_ms: 1,
            blob_path: server_frame_path.to_string_lossy().into_owned(),
        };
        let response = ControlResponse {
            ok: true,
            result: Some(serde_json::to_value(frame).expect("encode frame metadata")),
            error: None,
        };
        let mut response_bytes = serde_json::to_vec(&response).expect("encode response");
        response_bytes.push(b'\n');
        write_half
            .write_all(&response_bytes)
            .await
            .expect("write response");
    });

    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let client = UnixControlClient::new(socket_path);
    let frame = capture_frame(&client, &machine())
        .await
        .expect("capture frame");
    server.await.expect("server task");

    assert_eq!(frame.width(), 2);
    assert_eq!(frame.height(), 2);
    assert_eq!(frame.pixel(0, 0), Some(Rgb8::new(255, 0, 0)));
    assert_eq!(frame.pixel(1, 0), Some(Rgb8::new(0, 255, 0)));
    assert_eq!(frame.pixel(0, 1), Some(Rgb8::new(0, 0, 255)));
    assert_eq!(frame.pixel(1, 1), Some(Rgb8::new(255, 255, 255)));
}
