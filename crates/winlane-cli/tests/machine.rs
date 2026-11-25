use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use winlane_cli::args::{GuestInvokeArgs, GuestMachineArgs};
use winlane_cli::{
    DaemonStopArgs, GuestCommand, MachineArgs, MachineCommand, run_daemon_stop, run_guest_command,
    run_machine_command,
};
use winlane_contracts::{CheckpointRef, MachineHandle, MachineState};
use winlane_ipc::{ControlRequest, ControlResponse};

fn machine() -> MachineHandle {
    MachineHandle {
        id: "machine-1".to_string(),
        name: "builder".to_string(),
        state_dir: "/tmp/builder".to_string(),
        backend: "qemu".to_string(),
        state: MachineState::Stopped,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_status_invokes_exact_ipc_operation_and_prints_json() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
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
            ControlRequest::Status {
                machine: server_machine
            }
        );

        let response = ControlResponse {
            ok: true,
            result: Some(serde_json::to_value(MachineState::Running).expect("encode state")),
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

    let args = MachineArgs {
        control_socket: socket_path,
        machine_json: Some(serde_json::to_string(&machine()).expect("encode machine")),
        machine_name: None,
    };
    let mut output = Vec::new();
    run_machine_command(&MachineCommand::Status(args), &mut output)
        .await
        .expect("run machine status");
    server.await.expect("server task");
    assert_eq!(output, b"\"running\"\n");
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_stop_invokes_exact_ipc_operation_and_prints_json() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
    let server = tokio::spawn(async move {
        let listener = UnixListener::bind(server_socket).expect("bind control socket");
        let (stream, _) = listener.accept().await.expect("accept control client");
        let request =
            write_control_response(stream, serde_json::json!({"shutdown":"initiated"})).await;
        assert_eq!(request, ControlRequest::Shutdown);
    });
    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let mut output = Vec::new();
    run_daemon_stop(
        &DaemonStopArgs {
            control_socket: socket_path,
        },
        &mut output,
    )
    .await
    .expect("run daemon stop");
    server.await.expect("server task");
    assert_eq!(output, b"{\"shutdown\":\"initiated\"}\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_list_invokes_exact_ipc_operation_and_prints_array() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
    let listed = machine();
    let server = tokio::spawn(async move {
        let listener = UnixListener::bind(server_socket).expect("bind control socket");
        let (stream, _) = listener.accept().await.expect("accept control client");
        let request = write_control_response(
            stream,
            serde_json::to_value(vec![listed]).expect("encode machines"),
        )
        .await;
        assert_eq!(request, ControlRequest::List);
    });

    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let mut output = Vec::new();
    run_machine_command(
        &MachineCommand::List(winlane_cli::args::ListArgs {
            control_socket: socket_path,
        }),
        &mut output,
    )
    .await
    .expect("run machine list");
    server.await.expect("server task");
    assert!(output.ends_with(b"\n"));
    let output_json: serde_json::Value =
        serde_json::from_slice(&output).expect("decode machine list output");
    assert_eq!(
        output_json,
        serde_json::to_value(vec![machine()]).expect("encode expected")
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_name_resolves_through_list_before_lifecycle_ipc() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
    let resolved = machine();
    let server = tokio::spawn(async move {
        let listener = UnixListener::bind(server_socket).expect("bind control socket");
        let (stream, _) = listener.accept().await.expect("accept list client");
        let list_request = write_control_response(
            stream,
            serde_json::to_value(vec![resolved.clone()]).expect("encode machines"),
        )
        .await;
        assert_eq!(list_request, ControlRequest::List);
        let (stream, _) = listener.accept().await.expect("accept status client");
        let status_request = write_control_response(
            stream,
            serde_json::to_value(MachineState::Running).expect("encode state"),
        )
        .await;
        assert_eq!(status_request, ControlRequest::Status { machine: resolved });
    });

    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let args = MachineArgs {
        control_socket: socket_path,
        machine_json: None,
        machine_name: Some("builder".to_string()),
    };
    let mut output = Vec::new();
    run_machine_command(&MachineCommand::Status(args), &mut output)
        .await
        .expect("run machine status by name");
    server.await.expect("server task");
    assert_eq!(output, b"\"running\"\n");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_name_resolution_errors_on_no_match_and_multiple_matches() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
    let server = tokio::spawn(async move {
        let listener = UnixListener::bind(server_socket).expect("bind control socket");
        for machines in [Vec::new(), vec![machine(), machine()]] {
            let (stream, _) = listener.accept().await.expect("accept list client");
            let request = write_control_response(
                stream,
                serde_json::to_value(machines).expect("encode machines"),
            )
            .await;
            assert_eq!(request, ControlRequest::List);
        }
    });

    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    for expected in ["no machine named", "2 machines named"] {
        let args = MachineArgs {
            control_socket: socket_path.clone(),
            machine_json: None,
            machine_name: Some("builder".to_string()),
        };
        let mut output = Vec::new();
        let error = run_machine_command(&MachineCommand::Status(args), &mut output)
            .await
            .expect_err("name resolution should fail");
        assert!(error.to_string().contains(expected), "{error}");
    }
    server.await.expect("server task");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_list_invokes_exact_ipc_operation_and_prints_compact_array() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
    let expected_machine = machine();
    let catalog = serde_json::json!([
        {
            "id": "checkpoint-a",
            "label": "before",
            "created_unix_ms": 10,
            "kind": "running",
            "state_dir_bytes": 123,
            "registered": true,
            "restorable": true,
            "error": null,
        },
        {
            "id": "checkpoint-b",
            "label": "after",
            "created_unix_ms": 20,
            "kind": "stopped",
            "state_dir_bytes": 45,
            "registered": true,
            "restorable": false,
            "error": null,
        }
    ]);
    let server_catalog = catalog.clone();
    let server = tokio::spawn(async move {
        let listener = UnixListener::bind(server_socket).expect("bind control socket");
        let (stream, _) = listener.accept().await.expect("accept control client");
        let request = write_control_response(stream, server_catalog).await;
        assert_eq!(
            request,
            ControlRequest::CheckpointList {
                machine: expected_machine
            }
        );
    });
    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let args = winlane_cli::args::CheckpointListArgs {
        control_socket: socket_path,
        machine_json: Some(serde_json::to_string(&machine()).expect("encode machine")),
        machine_name: None,
    };
    let mut output = Vec::new();
    run_machine_command(&MachineCommand::CheckpointList(args), &mut output)
        .await
        .expect("run checkpoint list");
    server.await.expect("server task");
    assert!(output.ends_with(b"\n"));
    assert!(!output[..output.len() - 1].contains(&b'\n'));
    let output_json: serde_json::Value = serde_json::from_slice(&output).expect("decode output");
    assert_eq!(output_json, catalog);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checkpoint_restore_by_name_and_id_resolves_and_invokes_exact_ipc_operation() {
    let root = tempfile::tempdir().expect("temporary directory");
    let state_dir = root.path().join("builder");
    let checkpoint_dir = state_dir.join("checkpoints").join("checkpoint-1");
    std::fs::create_dir_all(&checkpoint_dir).expect("create checkpoint directory");
    let machine = MachineHandle {
        id: "machine-1".to_string(),
        name: "builder".to_string(),
        state_dir: state_dir.to_string_lossy().into_owned(),
        backend: "qemu".to_string(),
        state: MachineState::Stopped,
    };
    let checkpoint = CheckpointRef {
        id: "checkpoint-1".to_string(),
        machine_id: machine.id.clone(),
        created_unix_ms: 42,
        state_dir: machine.state_dir.clone(),
        sha256: Some("abc".to_string()),
    };
    std::fs::write(
        checkpoint_dir.join("checkpoint.json"),
        serde_json::to_vec(&serde_json::json!({ "checkpoint": checkpoint }))
            .expect("encode metadata"),
    )
    .expect("write checkpoint metadata");

    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
    let server_machine = machine.clone();
    let server_checkpoint = checkpoint.clone();
    let server = tokio::spawn(async move {
        let listener = UnixListener::bind(server_socket).expect("bind control socket");
        let (stream, _) = listener.accept().await.expect("accept list client");
        let request = write_control_response(
            stream,
            serde_json::to_value(vec![server_machine.clone()]).expect("encode machines"),
        )
        .await;
        assert_eq!(request, ControlRequest::List);

        let (stream, _) = listener.accept().await.expect("accept restore client");
        let request = write_control_response(
            stream,
            serde_json::to_value(server_machine.clone()).expect("encode machine"),
        )
        .await;
        assert_eq!(
            request,
            ControlRequest::Restore {
                checkpoint: server_checkpoint
            }
        );
    });
    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let args = winlane_cli::args::CheckpointRestoreArgs {
        control_socket: socket_path,
        checkpoint_file: None,
        machine_json: None,
        machine_name: Some("builder".to_string()),
        checkpoint_id: Some("checkpoint-1".to_string()),
    };
    let mut output = Vec::new();
    run_machine_command(&MachineCommand::CheckpointRestore(args), &mut output)
        .await
        .expect("run checkpoint restore");
    server.await.expect("server task");
    let output_json: serde_json::Value = serde_json::from_slice(&output).expect("decode output");
    assert_eq!(output_json["id"], "machine-1");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn guest_machine_name_resolves_through_control_list_before_qga_connect() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let server_socket = socket_path.clone();
    let server = tokio::spawn(async move {
        let listener = UnixListener::bind(server_socket).expect("bind control socket");
        let (stream, _) = listener.accept().await.expect("accept list client");
        let request = write_control_response(
            stream,
            serde_json::to_value(vec![machine()]).expect("encode machines"),
        )
        .await;
        assert_eq!(request, ControlRequest::List);
    });

    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    let command = GuestCommand::Invoke(GuestInvokeArgs {
        machine: GuestMachineArgs {
            control_socket: Some(socket_path),
            machine_json: None,
            machine_name: Some("builder".to_string()),
        },
        node: "0".to_string(),
        view: winlane_guest::uia::UiaView::Control,
        runtime_id: None,
    });
    let mut output = Vec::new();
    let error = run_guest_command(&command, &mut output)
        .await
        .expect_err("missing qga socket should fail after list resolution");
    drop(error);
    server.await.expect("server task");
}

async fn wait_server(
    socket_path: std::path::PathBuf,
    result: serde_json::Value,
    expected: MachineHandle,
) {
    let listener = UnixListener::bind(socket_path).expect("bind control socket");
    loop {
        let Ok((stream, _)) = listener.accept().await else {
            break;
        };
        let request = write_control_response(stream, result.clone()).await;
        assert_eq!(
            request,
            ControlRequest::GuestAgentReady {
                machine: expected.clone()
            }
        );
        if result["ready"] == true {
            break;
        }
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_wait_prints_ready_json_and_uses_exact_ipc_operation() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let expected = machine();
    let server = tokio::spawn(wait_server(
        socket_path.clone(),
        serde_json::json!({"ready":true,"state":"running"}),
        expected.clone(),
    ));
    for _ in 0..100 {
        if socket_path.exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let args = winlane_cli::args::WaitArgs {
        control_socket: socket_path,
        machine_json: Some(serde_json::to_string(&expected).expect("encode machine")),
        machine_name: None,
        timeout_seconds: 1,
    };
    let mut output = Vec::new();
    run_machine_command(&MachineCommand::Wait(args), &mut output)
        .await
        .expect("wait succeeds");
    server.await.expect("server task");
    let output_json: serde_json::Value = serde_json::from_slice(&output).expect("decode output");
    assert_eq!(output_json["ready"], true);
    assert_eq!(output_json["for"], "guest-agent");
    assert_eq!(output_json["state"], "running");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn machine_wait_timeout_prints_result_and_fails_nonzero_path() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let expected = machine();
    let _server = tokio::spawn(async move {
        let listener = UnixListener::bind(socket_path).expect("bind control socket");
        for _ in 0..6 {
            let Ok((stream, _)) = listener.accept().await else {
                break;
            };
            let request = write_control_response(
                stream,
                serde_json::json!({"ready":false,"state":"stopped"}),
            )
            .await;
            assert_eq!(
                request,
                ControlRequest::GuestAgentReady { machine: machine() }
            );
        }
    });
    for _ in 0..100 {
        if root.path().join("control.sock").exists() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    let args = winlane_cli::args::WaitArgs {
        control_socket: root.path().join("control.sock"),
        machine_json: Some(serde_json::to_string(&expected).expect("encode machine")),
        machine_name: None,
        timeout_seconds: 1,
    };
    let mut output = Vec::new();
    let result = run_machine_command(&MachineCommand::Wait(args), &mut output).await;
    assert!(matches!(result, Err(winlane_cli::CliError::Operation(_))));
    let output_json: serde_json::Value = serde_json::from_slice(&output).expect("decode output");
    assert_eq!(output_json["ready"], false);
    assert_eq!(output_json["state"], "stopped");
    assert_eq!(output_json["error"], "timeout");
}

#[tokio::test]
async fn machine_wait_timeout_prints_json_and_exits_one() {
    let root = tempfile::tempdir().expect("temporary directory");
    let socket_path = root.path().join("control.sock");
    let listener = UnixListener::bind(&socket_path).expect("bind control socket");
    let server = tokio::spawn(async move {
        loop {
            let (stream, _) = listener.accept().await.expect("accept control client");
            let request = write_control_response(
                stream,
                serde_json::json!({"ready":false,"state":"stopped"}),
            )
            .await;
            assert_eq!(
                request,
                ControlRequest::GuestAgentReady { machine: machine() }
            );
        }
    });
    let output = tokio::process::Command::new(env!("CARGO_BIN_EXE_winlane"))
        .args(["machine", "wait", "--control-socket"])
        .arg(socket_path)
        .arg("--machine-json")
        .arg(serde_json::to_string(&machine()).expect("encode machine"))
        .args(["--for", "guest-agent", "--timeout-seconds", "1"])
        .output()
        .await
        .expect("run winlane");
    assert!(
        !server.is_finished(),
        "control server stopped before timeout"
    );
    server.abort();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        output.stderr,
        b"winlane: timed out waiting for guest agent\n"
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).expect("decode output");
    assert_eq!(report["ready"], false);
    assert_eq!(report["for"], "guest-agent");
    assert_eq!(report["state"], "stopped");
    assert_eq!(report["error"], "timeout");
}
