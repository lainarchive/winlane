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
        base_image_path: None,
        guest_agent: false,
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
    let clone_disk_path = root.path().join("controlclone/disk.qcow2");
    assert!(clone_disk_path.exists());
    let clone_disk = std::fs::read_to_string(&clone_disk_path).expect("read clone disk metadata");
    assert!(clone_disk.contains("converted_from"));
    assert!(clone_disk.contains("\"backing\": null"));

    let command_log = root.path().join("controltest/qmp.sock.commands");
    let commands = std::fs::read_to_string(command_log).expect("QMP command log");
    for expected in [
        "qmp_capabilities",
        "query-status",
        "screendump",
        "input-send-event",
        "send-key",
        "snapshot-save",
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
async fn multiple_fake_machines_run_and_clean_up_independently() {
    let root = tempfile::tempdir().expect("temporary machine root");
    let mut backend = QemuBackend::new(root.path())
        .await
        .expect("backend")
        .with_qemu_binaries(fixture("fake_qemu_system.py"), fixture("fake_qemu_img.py"));

    let first = backend
        .create_machine(&spec("machineone"))
        .await
        .expect("create first machine");
    let second = backend
        .create_machine(&spec("machinetwo"))
        .await
        .expect("create second machine");
    backend
        .start_machine(&first)
        .await
        .expect("start first machine");
    backend
        .start_machine(&second)
        .await
        .expect("start second machine");

    assert_eq!(
        backend.status_machine(&first).await.expect("first status"),
        MachineState::Running
    );
    assert_eq!(
        backend
            .status_machine(&second)
            .await
            .expect("second status"),
        MachineState::Running
    );
    let first_frame = backend.capture_frame(&first).await.expect("first frame");
    let second_frame = backend.capture_frame(&second).await.expect("second frame");
    assert_ne!(first_frame.blob_path, second_frame.blob_path);

    let listed = backend.list_machines().await.expect("list machines");
    assert_eq!(listed.len(), 2);
    assert!(
        listed
            .iter()
            .all(|machine| machine.state == MachineState::Running)
    );

    backend
        .stop_machine(&first)
        .await
        .expect("stop first machine");
    assert_eq!(
        backend
            .status_machine(&second)
            .await
            .expect("second status"),
        MachineState::Running
    );
    backend
        .shutdown_machines()
        .await
        .expect("shutdown remaining machines");
    assert_eq!(
        backend
            .status_machine(&second)
            .await
            .expect("second stopped status"),
        MachineState::Stopped
    );
    backend
        .destroy_machine(&first)
        .await
        .expect("destroy first machine");
    backend
        .destroy_machine(&second)
        .await
        .expect("destroy second machine");
    assert!(!root.path().join("machineone").exists());
    assert!(!root.path().join("machinetwo").exists());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn base_image_and_guest_agent_create_overlay_and_qga_socket() {
    let root = tempfile::tempdir().expect("temporary machine root");
    let image_root = tempfile::tempdir().expect("temporary image root");
    let golden = image_root.path().join("golden.qcow2");
    let golden_bytes = b"golden image fixture\n";
    std::fs::write(&golden, golden_bytes).expect("write golden image");
    let linked_golden = image_root.path().join("linked-golden.qcow2");
    std::os::unix::fs::symlink(&golden, &linked_golden).expect("link golden image");
    let canonical_golden = std::fs::canonicalize(&golden).expect("canonical golden image");
    let canonical_golden_string = canonical_golden.to_string_lossy().into_owned();

    let mut machine_spec = spec("runtimeqga");
    machine_spec.base_image_path = Some(linked_golden.to_string_lossy().into_owned());
    machine_spec.guest_agent = true;
    let mut backend = QemuBackend::new(root.path())
        .await
        .expect("backend")
        .with_qemu_binaries(fixture("fake_qemu_system.py"), fixture("fake_qemu_img.py"));
    let machine = backend
        .create_machine(&machine_spec)
        .await
        .expect("create runtime machine");
    let state_dir = PathBuf::from(&machine.state_dir);

    let disk: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(state_dir.join("disk.qcow2")).expect("read overlay metadata"),
    )
    .expect("parse overlay metadata");
    assert_eq!(
        disk["backing"].as_str(),
        Some(canonical_golden_string.as_str())
    );
    assert_eq!(disk["backing_format"].as_str(), Some("qcow2"));

    let record: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(state_dir.join("machine.json")).expect("read machine record"),
    )
    .expect("parse machine record");
    let qga_socket = state_dir.join("qga.sock");
    let qga_socket_string = qga_socket.to_string_lossy().into_owned();
    assert_eq!(
        backend
            .guest_agent_socket_path(&machine)
            .await
            .expect("guest agent socket path"),
        Some(qga_socket.clone())
    );
    assert_eq!(
        record["qga_socket_path"].as_str(),
        Some(qga_socket_string.as_str())
    );
    assert_eq!(
        record["spec"]["base_image_path"].as_str(),
        Some(canonical_golden_string.as_str())
    );

    backend
        .start_machine(&machine)
        .await
        .expect("start runtime machine");
    assert!(qga_socket.exists());
    std::os::unix::net::UnixStream::connect(&qga_socket).expect("connect to fake QGA socket");

    let argv: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(state_dir.join("qmp.sock.argv")).expect("read QEMU argv"),
    )
    .expect("parse QEMU argv");
    let serial_index = argv
        .iter()
        .position(|arg| arg == "virtio-serial-pci")
        .expect("virtio serial device");
    let chardev_index = argv
        .iter()
        .position(|arg| arg.starts_with("socket,id=qga0,path="))
        .expect("QGA chardev");
    let port_index = argv
        .iter()
        .position(|arg| arg == "virtserialport,chardev=qga0,name=org.qemu.guest_agent.0")
        .expect("QGA virtserialport");
    assert!(serial_index < chardev_index);
    assert!(chardev_index < port_index);
    assert!(argv[chardev_index].contains("server=on,wait=off"));
    assert!(argv[chardev_index].contains(&qga_socket_string));

    backend
        .stop_machine(&machine)
        .await
        .expect("stop runtime machine");
    backend
        .destroy_machine(&machine)
        .await
        .expect("destroy runtime machine");
    assert_eq!(
        std::fs::read(&golden).expect("read golden image"),
        golden_bytes
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guest_agent_socket_path_commas_are_escaped_for_qemu() {
    let outer = tempfile::tempdir().expect("temporary outer root");
    let root = outer.path().join("comma,root");
    let mut backend = QemuBackend::new(&root)
        .await
        .expect("backend")
        .with_qemu_binaries(fixture("fake_qemu_system.py"), fixture("fake_qemu_img.py"));
    let mut machine_spec = spec("commaqga");
    machine_spec.guest_agent = true;
    let machine = backend
        .create_machine(&machine_spec)
        .await
        .expect("create comma-path machine");
    backend
        .start_machine(&machine)
        .await
        .expect("start comma-path machine");

    let state_dir = PathBuf::from(&machine.state_dir);
    let argv: Vec<String> = serde_json::from_str(
        &std::fs::read_to_string(state_dir.join("qmp.sock.argv")).expect("read QEMU argv"),
    )
    .expect("parse QEMU argv");
    let qga_socket_string = state_dir.join("qga.sock").to_string_lossy().into_owned();
    let escaped_qga_socket = qga_socket_string.replace(',', ",,");
    let chardev = argv
        .iter()
        .find(|arg| arg.starts_with("socket,id=qga0,path="))
        .expect("QGA chardev");
    assert!(chardev.contains(&escaped_qga_socket));
    assert!(!chardev.contains(&qga_socket_string));

    backend
        .stop_machine(&machine)
        .await
        .expect("stop comma-path machine");
    backend
        .destroy_machine(&machine)
        .await
        .expect("destroy comma-path machine");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn missing_base_image_is_rejected_without_creating_state() {
    let root = tempfile::tempdir().expect("temporary machine root");
    let mut backend = QemuBackend::new(root.path())
        .await
        .expect("backend")
        .with_qemu_binaries(fixture("fake_qemu_system.py"), fixture("fake_qemu_img.py"));
    let mut machine_spec = spec("missingbase");
    machine_spec.base_image_path = Some(
        root.path()
            .join("missing-golden.qcow2")
            .to_string_lossy()
            .into_owned(),
    );
    assert!(backend.create_machine(&machine_spec).await.is_err());
    assert!(!root.path().join("missingbase").exists());
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
    let clone_info = std::process::Command::new("qemu-img")
        .arg("info")
        .arg(PathBuf::from(&clone.state_dir).join("disk.qcow2"))
        .output()
        .expect("inspect real clone");
    assert!(
        clone_info.status.success(),
        "qemu-img clone info failed: {}",
        String::from_utf8_lossy(&clone_info.stderr)
    );
    assert!(!String::from_utf8_lossy(&clone_info.stdout).contains("backing file:"));
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
#[ignore = "requires qemu-system-x86_64, qemu-img, and writable /dev/kvm"]
async fn real_qemu_base_image_guest_agent_control_proof() {
    let root = tempfile::tempdir().expect("temporary machine root");
    let image_root = tempfile::tempdir().expect("temporary image root");
    let golden = image_root.path().join("golden.qcow2");
    let create_base = std::process::Command::new("qemu-img")
        .arg("create")
        .arg("-f")
        .arg("qcow2")
        .arg(&golden)
        .arg("16G")
        .output()
        .expect("create golden image");
    assert!(
        create_base.status.success(),
        "qemu-img create failed: {}",
        String::from_utf8_lossy(&create_base.stderr)
    );
    let golden_before = std::fs::read(&golden).expect("read golden image before use");

    let mut machine_spec = spec("realruntime");
    machine_spec.base_image_path = Some(golden.to_string_lossy().into_owned());
    machine_spec.guest_agent = true;
    let mut backend = QemuBackend::new(root.path()).await.expect("backend");
    let machine = backend
        .create_machine(&machine_spec)
        .await
        .expect("create backed runtime machine");

    let info = std::process::Command::new("qemu-img")
        .arg("info")
        .arg(PathBuf::from(&machine.state_dir).join("disk.qcow2"))
        .output()
        .expect("inspect runtime overlay");
    assert!(
        info.status.success(),
        "qemu-img info failed: {}",
        String::from_utf8_lossy(&info.stderr)
    );
    let info_text = String::from_utf8_lossy(&info.stdout);
    assert!(info_text.contains("backing file:"));
    assert!(info_text.contains(&golden.to_string_lossy().into_owned()));
    assert!(info_text.contains("backing file format: qcow2"));

    backend
        .start_machine(&machine)
        .await
        .expect("start backed runtime machine");
    assert_eq!(
        backend.status_machine(&machine).await.expect("status"),
        MachineState::Running
    );
    let qga_socket = PathBuf::from(&machine.state_dir).join("qga.sock");
    for _ in 0..100 {
        if qga_socket.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(qga_socket.exists());
    let checkpoint = backend
        .checkpoint_machine(&machine, "runtimeqga")
        .await
        .expect("checkpoint backed runtime machine");
    backend
        .stop_machine(&machine)
        .await
        .expect("stop backed runtime machine");
    let restored = backend
        .restore_checkpoint(&checkpoint)
        .await
        .expect("restore backed runtime checkpoint");
    backend
        .start_machine(&restored)
        .await
        .expect("restart backed runtime machine");
    assert_eq!(
        backend.status_machine(&restored).await.expect("status"),
        MachineState::Running
    );
    backend
        .stop_machine(&restored)
        .await
        .expect("stop restored runtime machine");
    backend
        .destroy_machine(&machine)
        .await
        .expect("destroy backed runtime machine");
    assert_eq!(
        std::fs::read(&golden).expect("read golden image after use"),
        golden_before
    );
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
