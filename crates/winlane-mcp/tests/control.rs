use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use tokio::sync::Mutex;
use winlane_contracts::{BootMode, CpuArch, MachineHandle, MachineSpec, MachineState, NetworkMode};
use winlane_core::QemuBackend;
use winlane_evidence::EvidenceStore;
use winlane_ipc::{ControlRequest, ControlResponse, serve_unix_control};
use winlane_mcp::McpControlPlane;
use winlane_mcp::server::handle_message_with_evidence;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/control")
        .join(name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mcp_tools_map_to_serialized_control_plane() {
    let root = tempfile::tempdir().expect("temporary machine root");
    let mut backend = QemuBackend::new(root.path())
        .await
        .expect("backend")
        .with_qemu_binaries(fixture("fake_qemu_system.py"), fixture("fake_qemu_img.py"));
    let spec = MachineSpec {
        name: "mcptest".to_string(),
        arch: CpuArch::X86_64,
        memory_mb: 512,
        cpus: 1,
        disk_gb: 16,
        iso_path: None,
        base_image_path: None,
        guest_agent: false,
        network: NetworkMode::Isolated,
        tpm: false,
        boot: BootMode::Bios,
    };
    let machine = backend.create_machine(&spec).await.expect("create machine");
    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
    let server = tokio::spawn(async move { serve_unix_control(server_socket, backend).await });
    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let mcp = McpControlPlane::new(&socket_path);
    let machine_json = serde_json::to_value(&machine).expect("machine json");
    mcp.dispatch("machine_start", &json!({"machine": machine_json}))
        .await
        .expect("machine start");
    let status = mcp
        .dispatch("machine_status", &json!({"machine": machine_json}))
        .await
        .expect("machine status");
    assert_eq!(status, json!("running"));
    let named_status = mcp
        .dispatch("machine_status", &json!({"machine": "mcptest"}))
        .await
        .expect("machine status by name");
    assert_eq!(named_status, json!("running"));
    let frame = mcp
        .dispatch("screenshot", &json!({"machine": machine_json}))
        .await
        .expect("screenshot");
    assert_eq!(frame["width"], 64);
    mcp.dispatch(
        "click",
        &json!({"machine": machine_json, "x": 2, "y": 3, "button": "left"}),
    )
    .await
    .expect("click");
    mcp.dispatch(
        "type_text",
        &json!({"machine": machine_json, "text": "abc"}),
    )
    .await
    .expect("type text");
    mcp.dispatch("key", &json!({"machine": machine_json, "chord": "ctrl+c"}))
        .await
        .expect("key");
    mcp.dispatch(
        "scroll",
        &json!({"machine": machine_json, "direction": "down", "steps": 1}),
    )
    .await
    .expect("scroll");
    let checkpoint = mcp
        .dispatch(
            "checkpoint_create",
            &json!({"machine": machine_json, "label": "mcpproof"}),
        )
        .await
        .expect("checkpoint create");
    assert!(checkpoint["sha256"].is_string());
    mcp.dispatch("machine_stop", &json!({"machine": machine_json}))
        .await
        .expect("machine stop");
    server.abort();
}

fn referenced_machine() -> MachineHandle {
    MachineHandle {
        id: "machine-1".to_string(),
        name: "builder".to_string(),
        state_dir: "/tmp/builder".to_string(),
        backend: "qemu".to_string(),
        state: MachineState::Stopped,
    }
}

async fn write_control_response(
    stream: tokio::net::UnixStream,
    result: serde_json::Value,
) -> ControlRequest {
    let (read_half, mut write_half) = stream.into_split();
    let mut reader = BufReader::new(read_half);
    let mut request_bytes = Vec::new();
    reader
        .read_until(b'\n', &mut request_bytes)
        .await
        .expect("read request");
    let request: ControlRequest =
        serde_json::from_slice(&request_bytes).expect("decode control request");
    let response = ControlResponse {
        ok: true,
        result: Some(result),
        error: None,
    };
    let mut response_bytes = serde_json::to_vec(&response).expect("encode response");
    response_bytes.push(b'\n');
    write_half
        .write_all(&response_bytes)
        .await
        .expect("write response");
    request
}

async fn wait_for_socket(socket_path: &std::path::Path) {
    for _ in 0..100 {
        if socket_path.exists() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    panic!("control socket was not created");
}

fn tool_call_message(name: &str, arguments: serde_json::Value) -> serde_json::Value {
    json!({
        "jsonrpc": "2.0",
        "id": 7,
        "method": "tools/call",
        "params": {
            "name": name,
            "arguments": arguments,
        }
    })
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_wait_uses_guest_agent_ready_ipc_and_returns_ready_json() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind control socket");
    let machine = referenced_machine();
    let expected = machine.clone();
    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.expect("accept wait client");
        let request =
            write_control_response(stream, json!({"ready": true, "state": "running"})).await;
        assert_eq!(
            request,
            ControlRequest::GuestAgentReady { machine: expected }
        );
    });

    let mcp = McpControlPlane::new(&socket_path);
    let result = mcp
        .dispatch(
            "machine_wait",
            &json!({
                "machine": serde_json::to_value(machine).expect("encode machine"),
                "for": "guest-agent",
                "timeout_seconds": 1,
            }),
        )
        .await
        .expect("machine wait");
    assert_eq!(result["ready"], true);
    assert_eq!(result["for"], "guest-agent");
    assert_eq!(result["state"], "running");
    server.await.expect("server task");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_wait_timeout_returns_ready_false_json() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind control socket");
    let machine = referenced_machine();
    let expected = machine.clone();
    let server = tokio::spawn(async move {
        loop {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let request =
                write_control_response(stream, json!({"ready": false, "state": "running"})).await;
            assert_eq!(
                request,
                ControlRequest::GuestAgentReady {
                    machine: expected.clone()
                }
            );
        }
    });

    let mcp = McpControlPlane::new(&socket_path);
    let result = mcp
        .dispatch(
            "machine_wait",
            &json!({
                "machine": serde_json::to_value(machine).expect("encode machine"),
                "for": "guest-agent",
                "timeout_seconds": 1,
            }),
        )
        .await
        .expect("machine wait timeout result");
    assert_eq!(result["ready"], false);
    assert_eq!(result["for"], "guest-agent");
    assert_eq!(result["state"], "running");
    assert_eq!(result["error"], "timeout");
    assert!(result["elapsed_ms"].as_u64().expect("elapsed") >= 1000);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn name_string_evidence_records_resolved_machine_id_with_one_list_lookup() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
    let machine = referenced_machine();
    let server_machine = machine.clone();
    let server = tokio::spawn(async move {
        let listener = UnixListener::bind(server_socket).expect("bind control socket");
        let (stream, _) = listener.accept().await.expect("accept list client");
        let request = write_control_response(
            stream,
            serde_json::to_value(vec![server_machine.clone()]).expect("encode machines"),
        )
        .await;
        assert_eq!(request, ControlRequest::List);

        let (stream, _) = listener.accept().await.expect("accept status client");
        let request = write_control_response(
            stream,
            serde_json::to_value(MachineState::Running).expect("encode state"),
        )
        .await;
        assert_eq!(
            request,
            ControlRequest::Status {
                machine: server_machine
            }
        );
    });
    wait_for_socket(&socket_path).await;

    let evidence_root = tempfile::tempdir().expect("temporary evidence root");
    let evidence = Arc::new(Mutex::new(
        EvidenceStore::create(evidence_root.path()).expect("create evidence store"),
    ));
    let mcp = McpControlPlane::new(&socket_path);
    let response = handle_message_with_evidence(
        &mcp,
        &tool_call_message("machine_status", json!({"machine": "builder"})),
        Some(Arc::clone(&evidence)),
    )
    .await
    .expect("tool response");
    assert_eq!(response["result"]["isError"], false);
    server.await.expect("server task");

    let events = evidence
        .lock()
        .await
        .read_events()
        .expect("read evidence events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].machine_id.as_deref(), Some("machine-1"));
    assert_eq!(events[0].payload["input"]["machine"]["id"], "machine-1");
    assert_eq!(events[0].payload["ok"], true);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn name_resolution_errors_are_tool_errors_and_failed_evidence() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
    let server = tokio::spawn(async move {
        let listener = UnixListener::bind(server_socket).expect("bind control socket");
        let (stream, _) = listener.accept().await.expect("accept list client");
        let request = write_control_response(
            stream,
            serde_json::to_value(vec![referenced_machine()]).expect("encode machines"),
        )
        .await;
        assert_eq!(request, ControlRequest::List);
    });
    wait_for_socket(&socket_path).await;

    let evidence_root = tempfile::tempdir().expect("temporary evidence root");
    let evidence = Arc::new(Mutex::new(
        EvidenceStore::create(evidence_root.path()).expect("create evidence store"),
    ));
    let mcp = McpControlPlane::new(&socket_path);
    let response = handle_message_with_evidence(
        &mcp,
        &tool_call_message("machine_status", json!({"machine": "missing"})),
        Some(Arc::clone(&evidence)),
    )
    .await
    .expect("tool response");
    assert_eq!(response["result"]["isError"], true);
    assert!(
        response["result"]["content"][0]["text"]
            .as_str()
            .expect("error text")
            .contains("no machine named 'missing'")
    );
    server.await.expect("server task");

    let events = evidence
        .lock()
        .await
        .read_events()
        .expect("read evidence events");
    assert_eq!(events.len(), 1);
    assert_eq!(events[0].machine_id, None);
    assert_eq!(events[0].payload["input"]["machine"], "missing");
    assert_eq!(events[0].payload["ok"], false);
    assert!(
        events[0].payload["error"]
            .as_str()
            .expect("evidence error")
            .contains("no machine named 'missing'")
    );
}
