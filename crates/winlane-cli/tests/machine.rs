use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixListener;
use winlane_cli::args::{GuestInvokeArgs, GuestMachineArgs};
use winlane_cli::{
    GuestCommand, MachineArgs, MachineCommand, run_guest_command, run_machine_command,
};
use winlane_contracts::{MachineHandle, MachineState};
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
