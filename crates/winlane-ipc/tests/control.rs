use std::path::PathBuf;
use std::time::Duration;

use serde_json::json;
use winlane_contracts::{BootMode, CpuArch, MachineHandle, MachineSpec, NetworkMode};
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
        base_image_path: None,
        guest_agent: false,
        network: NetworkMode::Isolated,
        tpm: false,
        boot: BootMode::Bios,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unix_control_serializes_concurrent_clients() {
    let root = tempfile::tempdir().expect("temporary machine root");
    let image_root = tempfile::tempdir().expect("temporary image root");
    let golden = image_root.path().join("golden.qcow2");
    std::fs::write(&golden, b"golden image fixture\n").expect("write golden image");
    let mut machine_spec = spec("ipctest");
    machine_spec.base_image_path = Some(golden.to_string_lossy().into_owned());
    machine_spec.guest_agent = true;
    let backend = QemuBackend::new(root.path())
        .await
        .expect("backend")
        .with_qemu_binaries(fixture("fake_qemu_system.py"), fixture("fake_qemu_img.py"));

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
    let machine: MachineHandle = serde_json::from_value(
        client
            .invoke(&ControlRequest::Create { spec: machine_spec })
            .await
            .expect("create over ipc"),
    )
    .expect("decode created machine");
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
    assert!(PathBuf::from(&machine.state_dir).join("qga.sock").exists());

    client
        .invoke(&ControlRequest::Stop {
            machine: machine.clone(),
        })
        .await
        .expect("stop over ipc");
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_control_socket_file_is_replaced() {
    let root = tempfile::tempdir().expect("temporary machine root");
    let backend = QemuBackend::new(root.path())
        .await
        .expect("backend")
        .with_qemu_binaries(fixture("fake_qemu_system.py"), fixture("fake_qemu_img.py"));
    let socket_path = root.path().join("control.sock");
    std::fs::write(&socket_path, b"stale").expect("write stale socket");
    let server_socket = socket_path.clone();
    let server = tokio::spawn(async move { serve_unix_control(server_socket, backend).await });

    let mut connected = false;
    for _ in 0..100 {
        let result = UnixControlClient::new(&socket_path)
            .invoke(&ControlRequest::Status {
                machine: MachineHandle {
                    id: "missing".to_string(),
                    name: "missing".to_string(),
                    state_dir: root.path().to_string_lossy().into_owned(),
                    backend: "qemu".to_string(),
                    state: winlane_contracts::MachineState::Defined,
                },
            })
            .await;
        if matches!(result, Err(winlane_ipc::IpcError::Protocol(_))) {
            connected = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(connected);
    server.abort();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn unix_control_handles_independent_machine_lifecycle_requests() {
    let root = tempfile::tempdir().expect("temporary machine root");
    let backend = QemuBackend::new(root.path())
        .await
        .expect("backend")
        .with_qemu_binaries(fixture("fake_qemu_system.py"), fixture("fake_qemu_img.py"));
    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
    let server = tokio::spawn(async move { serve_unix_control(server_socket, backend).await });
    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }

    let create_one = async {
        UnixControlClient::new(&socket_path)
            .invoke(&ControlRequest::Create {
                spec: spec("ipcone"),
            })
            .await
    };
    let create_two = async {
        UnixControlClient::new(&socket_path)
            .invoke(&ControlRequest::Create {
                spec: spec("ipctwo"),
            })
            .await
    };
    let (created_one, created_two) = tokio::join!(create_one, create_two);
    let one: MachineHandle =
        serde_json::from_value(created_one.expect("create first")).expect("decode first machine");
    let two: MachineHandle =
        serde_json::from_value(created_two.expect("create second")).expect("decode second machine");

    let start_one = async {
        UnixControlClient::new(&socket_path)
            .invoke(&ControlRequest::Start {
                machine: one.clone(),
            })
            .await
    };
    let start_two = async {
        UnixControlClient::new(&socket_path)
            .invoke(&ControlRequest::Start {
                machine: two.clone(),
            })
            .await
    };
    let (started_one, started_two) = tokio::join!(start_one, start_two);
    started_one.expect("start first");
    started_two.expect("start second");

    let status_one = async {
        UnixControlClient::new(&socket_path)
            .invoke(&ControlRequest::Status {
                machine: one.clone(),
            })
            .await
    };
    let status_two = async {
        UnixControlClient::new(&socket_path)
            .invoke(&ControlRequest::Status {
                machine: two.clone(),
            })
            .await
    };
    let (status_one, status_two) = tokio::join!(status_one, status_two);
    assert_eq!(status_one.expect("first status"), json!("running"));
    assert_eq!(status_two.expect("second status"), json!("running"));

    let stop_one = async {
        UnixControlClient::new(&socket_path)
            .invoke(&ControlRequest::Stop {
                machine: one.clone(),
            })
            .await
    };
    let stop_two = async {
        UnixControlClient::new(&socket_path)
            .invoke(&ControlRequest::Stop {
                machine: two.clone(),
            })
            .await
    };
    let (stopped_one, stopped_two) = tokio::join!(stop_one, stop_two);
    stopped_one.expect("stop first");
    stopped_two.expect("stop second");

    let destroy_one = async {
        UnixControlClient::new(&socket_path)
            .invoke(&ControlRequest::Destroy { machine: one })
            .await
    };
    let destroy_two = async {
        UnixControlClient::new(&socket_path)
            .invoke(&ControlRequest::Destroy { machine: two })
            .await
    };
    let (destroyed_one, destroyed_two) = tokio::join!(destroy_one, destroy_two);
    destroyed_one.expect("destroy first");
    destroyed_two.expect("destroy second");
    assert!(!root.path().join("ipcone").exists());
    assert!(!root.path().join("ipctwo").exists());
    server.abort();
}
