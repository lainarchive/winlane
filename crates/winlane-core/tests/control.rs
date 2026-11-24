use std::path::PathBuf;

use winlane_contracts::{
    BootMode, ControlEvent, CpuArch, MachineSpec, MachineState, MouseButton, NetworkMode,
    ScrollDirection,
};
use winlane_core::{QemuBackend, QmpClient};

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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn fake_qmp_machine_lifecycle_frames_input_checkpoints_and_clones() {
    let root = tempfile::tempdir().expect("temporary machine root");
    let mut backend = QemuBackend::new(root.path())
        .await
        .expect("backend")
        .with_qemu_binaries(fixture("fake_qemu_system.py"), fixture("fake_qemu_img.py"));

    let machine = backend
        .create_machine(&spec("controltest"))
        .await
        .expect("create machine");
    assert_eq!(machine.state, MachineState::Defined);
    assert!(root.path().join("controltest/disk.qcow2").exists());

    backend
        .start_machine(&machine)
        .await
        .expect("start machine");
    assert_eq!(
        backend.status_machine(&machine).await.expect("status"),
        MachineState::Running
    );

    let frame = backend
        .capture_frame(&machine)
        .await
        .expect("capture frame");
    assert_eq!((frame.width, frame.height), (64, 48));
    assert_eq!(frame.sha256.len(), 64);
    assert!(PathBuf::from(&frame.blob_path).exists());

    for event in [
        ControlEvent::Move { x: 10, y: 12 },
        ControlEvent::Click {
            x: 10,
            y: 12,
            button: MouseButton::Left,
        },
        ControlEvent::Type {
            text: "Hi!".to_string(),
        },
        ControlEvent::Key {
            chord: "ctrl+alt+delete".to_string(),
        },
        ControlEvent::Scroll {
            direction: ScrollDirection::Down,
            steps: 2,
        },
    ] {
        backend
            .send_control_event(&machine, &event)
            .await
            .expect("control event");
    }

    let checkpoint = backend
        .checkpoint_machine(&machine, "before_restore")
        .await
        .expect("checkpoint");
    assert!(checkpoint.sha256.is_some());

    backend.stop_machine(&machine).await.expect("stop machine");
    assert_eq!(
        backend
            .status_machine(&machine)
            .await
            .expect("stopped status"),
        MachineState::Stopped
    );

    let restored = backend
        .restore_checkpoint(&checkpoint)
        .await
        .expect("restore checkpoint");
    assert_eq!(restored.state, MachineState::Stopped);
    backend
        .start_machine(&restored)
        .await
        .expect("start restored machine");
    backend
        .stop_machine(&restored)
        .await
        .expect("stop restored machine");

    let clone = backend
        .clone_machine(&restored, "controlclone")
        .await
        .expect("clone machine");
    assert!(root.path().join("controlclone/disk.qcow2").exists());

    let command_log = root.path().join("controltest/qmp.sock.commands");
    let commands = std::fs::read_to_string(command_log).expect("QMP command log");
    for expected in [
        "qmp_capabilities",
        "query-status",
        "screendump",
        "input-send-event",
        "send-key",
        "savevm",
    ] {
        assert!(commands.contains(expected), "missing {expected} in QMP log");
    }

    backend
        .destroy_machine(&clone)
        .await
        .expect("destroy clone");
    backend
        .destroy_machine(&restored)
        .await
        .expect("destroy machine");
    assert!(!root.path().join("controltest").exists());
    assert!(!root.path().join("controlclone").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn qmp_correlates_commands_and_receives_async_events() {
    let root = tempfile::tempdir().expect("temporary qmp root");
    let socket = root.path().join("qmp.sock");
    let mut child = tokio::process::Command::new(fixture("fake_qemu_system.py"))
        .arg("-qmp")
        .arg(format!("unix:{},server=on,wait=off", socket.display()))
        .spawn()
        .expect("spawn fake qemu");

    let client = {
        let mut connected = None;
        for _ in 0..100 {
            match QmpClient::connect(&socket).await {
                Ok(client) => {
                    connected = Some(client);
                    break;
                }
                Err(_) => tokio::time::sleep(std::time::Duration::from_millis(10)).await,
            }
        }
        connected.expect("connect to fake QMP")
    };

    let mut events = client.subscribe_events();
    let result = client
        .execute("input-send-event", Some(serde_json::json!({"events": []})))
        .await
        .expect("input command");
    assert_eq!(result, serde_json::json!({}));
    let event = tokio::time::timeout(std::time::Duration::from_secs(2), events.recv())
        .await
        .expect("event timeout")
        .expect("event receive");
    assert_eq!(event.event, "INPUT_PROCESSED");

    let status = client
        .execute("query-status", None)
        .await
        .expect("status command");
    assert_eq!(status["status"], "running");
    let _ = child.kill().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires qemu-system-x86_64, qemu-img, and writable /dev/kvm"]
async fn real_qemu_machine_control_proof() {
    let root = tempfile::tempdir().expect("temporary machine root");
    let mut backend = QemuBackend::new(root.path()).await.expect("backend");
    let machine = backend
        .create_machine(&spec("realqemu"))
        .await
        .expect("create real machine");

    backend
        .start_machine(&machine)
        .await
        .expect("start real qemu machine");
    assert_eq!(
        backend.status_machine(&machine).await.expect("real status"),
        MachineState::Running
    );
    let frame = backend
        .capture_frame(&machine)
        .await
        .expect("real frame capture");
    assert!(frame.width > 0);
    assert!(frame.height > 0);
    backend
        .send_control_event(&machine, &ControlEvent::Move { x: 4, y: 4 })
        .await
        .expect("real pointer input");
    backend
        .send_control_event(
            &machine,
            &ControlEvent::Key {
                chord: "ctrl+alt+delete".to_string(),
            },
        )
        .await
        .expect("real keyboard input");
    let checkpoint = backend
        .checkpoint_machine(&machine, "realproof")
        .await
        .expect("real checkpoint");
    assert!(checkpoint.sha256.is_some());
    backend
        .stop_machine(&machine)
        .await
        .expect("stop real machine");
    let restored = backend
        .restore_checkpoint(&checkpoint)
        .await
        .expect("restore real checkpoint");
    backend
        .start_machine(&restored)
        .await
        .expect("restart restored real machine");
    backend
        .stop_machine(&restored)
        .await
        .expect("stop restored real machine");
    let stopped_checkpoint = backend
        .checkpoint_machine(&restored, "stoppedproof")
        .await
        .expect("stopped real checkpoint");
    let restored_stopped = backend
        .restore_checkpoint(&stopped_checkpoint)
        .await
        .expect("restore stopped real checkpoint");
    assert_eq!(restored_stopped.state, MachineState::Stopped);
    let clone = backend
        .clone_machine(&machine, "realclone")
        .await
        .expect("real clone");
    backend
        .destroy_machine(&clone)
        .await
        .expect("destroy real clone");
    backend
        .destroy_machine(&machine)
        .await
        .expect("destroy real machine");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires qemu, KVM, OVMF, and swtpm"]
async fn real_qemu_uefi_tpm_control_proof() {
    let root = tempfile::tempdir().expect("temporary machine root");
    let mut backend = QemuBackend::new(root.path()).await.expect("backend");
    let mut machine_spec = spec("uefitpm");
    machine_spec.boot = BootMode::Uefi;
    machine_spec.tpm = true;
    let machine = backend
        .create_machine(&machine_spec)
        .await
        .expect("create uefi tpm machine");
    backend
        .start_machine(&machine)
        .await
        .expect("start uefi tpm machine");
    assert_eq!(
        backend.status_machine(&machine).await.expect("status"),
        MachineState::Running
    );
    let checkpoint = backend
        .checkpoint_machine(&machine, "firmwaretpm")
        .await
        .expect("firmware tpm checkpoint");
    assert!(checkpoint.sha256.is_some());
    backend
        .stop_machine(&machine)
        .await
        .expect("stop uefi tpm machine");
    backend
        .destroy_machine(&machine)
        .await
        .expect("destroy uefi tpm machine");
}
