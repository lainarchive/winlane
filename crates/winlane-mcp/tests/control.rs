use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use winlane_contracts::{BootMode, CpuArch, MachineSpec, NetworkMode};
use winlane_core::QemuBackend;
use winlane_ipc::serve_unix_control;
use winlane_mcp::McpControlPlane;

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
