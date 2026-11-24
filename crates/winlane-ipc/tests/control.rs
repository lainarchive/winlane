use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use winlane_contracts::{BootMode, CpuArch, MachineSpec, NetworkMode};
use winlane_core::QemuBackend;
use winlane_ipc::{ControlRequest, UnixControlClient, serve_unix_control};

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join("tests/fixtures/control")
        .join(name)
}

fn spec(name: &str) -> MachineSpec {
    MachineSpec {
        name: name.to_string(),
        arch: CpuArch::X86_64,
        memory_mb: 512,
        cpus: 1,
        disk_gb: 16,
        iso_path: None,
        network: NetworkMode::Isolated,
        tpm: false,
        boot: BootMode::Bios,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unix_control_serializes_concurrent_clients() {
    let root = tempfile::tempdir().expect("temporary machine root");
    let mut backend = QemuBackend::new(root.path())
        .await
        .expect("backend")
        .with_qemu_binaries(fixture("fake_qemu_system.py"), fixture("fake_qemu_img.py"));
    let machine = backend
        .create_machine(&spec("ipctest"))
        .await
        .expect("create machine");

    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
    let server = tokio::spawn(async move { serve_unix_control(server_socket, backend).await });
    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let client = UnixControlClient::new(&socket_path);
    client
        .invoke(&ControlRequest::Start {
            machine: machine.clone(),
        })
        .await
        .expect("start over ipc");

    let mut tasks = Vec::new();
    for _ in 0..8 {
        let path = socket_path.clone();
        let machine = machine.clone();
        tasks.push(tokio::spawn(async move {
            UnixControlClient::new(path)
                .invoke(&ControlRequest::Status { machine })
                .await
        }));
    }
    for task in tasks {
        let status = task.await.expect("status task").expect("status response");
        assert_eq!(status, json!("running"));
    }

    let frame = client
        .invoke(&ControlRequest::CaptureFrame {
            machine: machine.clone(),
        })
        .await
        .expect("frame over ipc");
    assert_eq!(frame["width"], 64);

    client
        .invoke(&ControlRequest::Stop {
            machine: machine.clone(),
        })
        .await
        .expect("stop over ipc");
    server.abort();
}
