use std::io::Write;
use std::path::PathBuf;

use serde_json::Value;
use winlane_contracts::{CheckpointRef, CpuArch, MachineHandle, MachineSpec};
use winlane_core::QemuBackend;
use winlane_ipc::{ControlRequest, UnixControlClient, serve_unix_control};

use crate::CliResult;
use crate::args::{
    CheckpointCreateArgs, CheckpointDeleteArgs, CheckpointListArgs, CheckpointRestoreArgs,
    CloneArgs, CreateArgs, DaemonArgs, DaemonStopArgs, MachineArgs, MachineCommand, McpArgs,
    WaitArgs,
};

pub async fn run_daemon(args: DaemonArgs) -> CliResult<()> {
    let backend = QemuBackend::new(args.root).await?;
    serve_unix_control(args.control_socket, backend).await?;
    Ok(())
}

pub async fn run_daemon_stop(args: &DaemonStopArgs, output: &mut impl Write) -> CliResult<()> {
    let result = UnixControlClient::new(args.control_socket.clone())
        .invoke(&ControlRequest::Shutdown)
        .await?;
    write_json_line(output, &result)
}

pub async fn run_machine_command(
    command: &MachineCommand,
    output: &mut impl Write,
) -> CliResult<()> {
    if let MachineCommand::Wait(args) = command {
        return run_machine_wait(args, output).await;
    }
    let command = resolve_machine_command(command).await?;
    let (control_socket, request) = machine_request(&command)?;
    let result = UnixControlClient::new(control_socket)
        .invoke(&request)
        .await?;
    write_json_line(output, &result)
}

async fn run_machine_wait(args: &WaitArgs, output: &mut impl Write) -> CliResult<()> {
    let machine =
        resolve_machine(&args.control_socket, &args.machine_json, &args.machine_name).await?;
    let client = UnixControlClient::new(args.control_socket.clone());
    let started = std::time::Instant::now();
    let deadline = started + std::time::Duration::from_secs(args.timeout_seconds);
    loop {
        let probe = client
            .invoke(&ControlRequest::GuestAgentReady {
                machine: machine.clone(),
            })
            .await?;
        let state = probe
            .get("state")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let elapsed_ms = started.elapsed().as_millis() as u64;
        if probe.get("ready").and_then(serde_json::Value::as_bool) == Some(true) {
            write_json_line(
                output,
                &serde_json::json!({
                    "ready": true,
                    "for": "guest-agent",
                    "state": state,
                    "elapsed_ms": elapsed_ms,
                }),
            )?;
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            write_json_line(
                output,
                &serde_json::json!({
                    "ready": false,
                    "for": "guest-agent",
                    "state": state,
                    "elapsed_ms": elapsed_ms,
                    "error": "timeout",
                }),
            )?;
            return Err(crate::CliError::Operation(
                "timed out waiting for guest agent".to_string(),
            ));
        }
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

async fn resolve_machine_command(command: &MachineCommand) -> CliResult<MachineCommand> {
    match command {
        MachineCommand::Start(args) => Ok(MachineCommand::Start(resolve_machine_args(args).await?)),
        MachineCommand::Stop(args) => Ok(MachineCommand::Stop(resolve_machine_args(args).await?)),
        MachineCommand::Status(args) => {
            Ok(MachineCommand::Status(resolve_machine_args(args).await?))
        }
        MachineCommand::Wait(args) => Ok(MachineCommand::Wait(args.clone())),
        MachineCommand::Logs(args) => {
            let machine =
                resolve_machine(&args.control_socket, &args.machine_json, &args.machine_name)
                    .await?;
            let mut args = args.clone();
            args.machine_json = Some(serde_json::to_string(&machine)?);
            args.machine_name = None;
            Ok(MachineCommand::Logs(args))
        }
        MachineCommand::Destroy(args) => {
            Ok(MachineCommand::Destroy(resolve_machine_args(args).await?))
        }
        MachineCommand::Clone(args) => {
            let machine =
                resolve_machine(&args.control_socket, &args.machine_json, &args.machine_name)
                    .await?;
            let mut args = args.clone();
            args.machine_json = Some(serde_json::to_string(&machine)?);
            args.machine_name = None;
            Ok(MachineCommand::Clone(args))
        }
        MachineCommand::CheckpointCreate(args) => {
            checkpoint_label(args)?;
            let machine =
                resolve_machine(&args.control_socket, &args.machine_json, &args.machine_name)
                    .await?;
            let mut args = args.clone();
            args.machine_json = Some(serde_json::to_string(&machine)?);
            args.machine_name = None;
            Ok(MachineCommand::CheckpointCreate(args))
        }
        MachineCommand::CheckpointList(args) => {
            let machine =
                resolve_machine(&args.control_socket, &args.machine_json, &args.machine_name)
                    .await?;
            let mut args = args.clone();
            args.machine_json = Some(serde_json::to_string(&machine)?);
            args.machine_name = None;
            Ok(MachineCommand::CheckpointList(args))
        }
        MachineCommand::CheckpointDelete(args) => {
            let machine =
                resolve_machine(&args.control_socket, &args.machine_json, &args.machine_name)
                    .await?;
            let mut args = args.clone();
            args.machine_json = Some(serde_json::to_string(&machine)?);
            args.machine_name = None;
            Ok(MachineCommand::CheckpointDelete(args))
        }
        MachineCommand::CheckpointRestore(args) if args.checkpoint_file.is_none() => {
            let machine =
                resolve_machine(&args.control_socket, &args.machine_json, &args.machine_name)
                    .await?;
            let mut args = args.clone();
            args.machine_json = Some(serde_json::to_string(&machine)?);
            args.machine_name = None;
            Ok(MachineCommand::CheckpointRestore(args))
        }
        MachineCommand::Create(_)
        | MachineCommand::List(_)
        | MachineCommand::CheckpointRestore(_) => Ok(command.clone()),
    }
}

async fn resolve_machine_args(args: &MachineArgs) -> CliResult<MachineArgs> {
    let machine =
        resolve_machine(&args.control_socket, &args.machine_json, &args.machine_name).await?;
    let mut args = args.clone();
    args.machine_json = Some(serde_json::to_string(&machine)?);
    args.machine_name = None;
    Ok(args)
}

pub(crate) fn parse_machine_json_arg(machine_json: &str) -> CliResult<MachineHandle> {
    serde_json::from_str(machine_json).map_err(|error| crate::CliError::Usage(error.to_string()))
}

pub(crate) async fn resolve_machine(
    control_socket: &std::path::Path,
    machine_json: &Option<String>,
    machine_name: &Option<String>,
) -> CliResult<MachineHandle> {
    if let Some(machine_json) = machine_json {
        return parse_machine_json_arg(machine_json);
    }
    let Some(name) = machine_name else {
        return Err(crate::CliError::Usage(
            "one of --machine-json or --machine is required".to_string(),
        ));
    };
    let result = UnixControlClient::new(control_socket)
        .invoke(&ControlRequest::List)
        .await?;
    let machines: Vec<MachineHandle> = serde_json::from_value(result)?;
    let matches = machines
        .into_iter()
        .filter(|machine| machine.name == *name)
        .collect::<Vec<_>>();
    match matches.len() {
        0 => Err(crate::CliError::Usage(format!(
            "no machine named '{name}' is known to this daemon"
        ))),
        1 => Ok(matches.into_iter().next().ok_or_else(|| {
            crate::CliError::Usage("machine resolution produced no result".to_string())
        })?),
        count => Err(crate::CliError::Usage(format!(
            "{count} machines named '{name}' are known to this daemon"
        ))),
    }
}

pub async fn run_mcp(args: McpArgs) -> CliResult<()> {
    winlane_mcp::server::serve_stdio(args.control_socket, args.evidence_dir).await?;
    Ok(())
}

fn machine_request(command: &MachineCommand) -> CliResult<(PathBuf, ControlRequest)> {
    match command {
        MachineCommand::Create(args) => {
            let spec = machine_spec(args);
            spec.validate()?;
            Ok((args.control_socket.clone(), ControlRequest::Create { spec }))
        }
        MachineCommand::List(args) => Ok((args.control_socket.clone(), ControlRequest::List)),
        MachineCommand::Start(args) => Ok((
            args.control_socket.clone(),
            ControlRequest::Start {
                machine: machine_handle(args)?,
            },
        )),
        MachineCommand::Stop(args) => Ok((
            args.control_socket.clone(),
            ControlRequest::Stop {
                machine: machine_handle(args)?,
            },
        )),
        MachineCommand::Status(args) => Ok((
            args.control_socket.clone(),
            ControlRequest::Status {
                machine: machine_handle(args)?,
            },
        )),
        MachineCommand::Wait(args) => {
            let machine_json = args.machine_json.as_deref().ok_or_else(|| {
                crate::CliError::Usage("machine JSON missing after resolution".to_string())
            })?;
            Ok((
                args.control_socket.clone(),
                ControlRequest::GuestAgentReady {
                    machine: serde_json::from_str(machine_json)?,
                },
            ))
        }
        MachineCommand::Logs(args) => {
            let machine_json = args.machine_json.as_deref().ok_or_else(|| {
                crate::CliError::Usage("machine JSON missing after resolution".to_string())
            })?;
            Ok((
                args.control_socket.clone(),
                ControlRequest::MachineLogs {
                    machine: serde_json::from_str(machine_json)?,
                    lines: args.lines,
                },
            ))
        }
        MachineCommand::Clone(args) => Ok((
            args.control_socket.clone(),
            ControlRequest::CloneMachine {
                machine: clone_machine_handle(args)?,
                name: clone_name(args)?,
            },
        )),
        MachineCommand::CheckpointCreate(args) => Ok((
            args.control_socket.clone(),
            ControlRequest::Checkpoint {
                machine: checkpoint_machine_handle(args)?,
                label: checkpoint_label(args)?,
            },
        )),
        MachineCommand::CheckpointList(args) => Ok((
            args.control_socket.clone(),
            ControlRequest::CheckpointList {
                machine: checkpoint_list_machine_handle(args)?,
            },
        )),
        MachineCommand::CheckpointDelete(args) => Ok((
            args.control_socket.clone(),
            ControlRequest::CheckpointDelete {
                machine: checkpoint_delete_machine_handle(args)?,
                checkpoint_id: args.checkpoint_id.clone(),
            },
        )),
        MachineCommand::CheckpointRestore(args) => Ok((
            args.control_socket.clone(),
            ControlRequest::Restore {
                checkpoint: checkpoint_ref(args)?,
            },
        )),
        MachineCommand::Destroy(args) => Ok((
            args.control_socket.clone(),
            ControlRequest::Destroy {
                machine: machine_handle(args)?,
            },
        )),
    }
}

fn machine_spec(args: &CreateArgs) -> MachineSpec {
    MachineSpec {
        name: args.name.clone(),
        arch: CpuArch::X86_64,
        memory_mb: args.memory_mb,
        cpus: args.cpus,
        disk_gb: args.disk_gb,
        iso_path: args.iso.clone(),
        base_image_path: args.base_image.clone(),
        guest_agent: true,
        network: args.network.clone(),
        tpm: args.tpm,
        boot: args.boot.clone(),
    }
}

fn machine_handle(args: &MachineArgs) -> CliResult<MachineHandle> {
    let machine_json = args.machine_json.as_deref().ok_or_else(|| {
        crate::CliError::Usage("machine JSON missing after resolution".to_string())
    })?;
    Ok(serde_json::from_str(machine_json)?)
}

fn checkpoint_list_machine_handle(args: &CheckpointListArgs) -> CliResult<MachineHandle> {
    let machine_json = args.machine_json.as_deref().ok_or_else(|| {
        crate::CliError::Usage("machine JSON missing after resolution".to_string())
    })?;
    Ok(serde_json::from_str(machine_json)?)
}

fn checkpoint_machine_handle(args: &CheckpointCreateArgs) -> CliResult<MachineHandle> {
    let machine_json = args.machine_json.as_deref().ok_or_else(|| {
        crate::CliError::Usage("machine JSON missing after resolution".to_string())
    })?;
    Ok(serde_json::from_str(machine_json)?)
}

fn checkpoint_delete_machine_handle(args: &CheckpointDeleteArgs) -> CliResult<MachineHandle> {
    let machine_json = args.machine_json.as_deref().ok_or_else(|| {
        crate::CliError::Usage("machine JSON missing after resolution".to_string())
    })?;
    Ok(serde_json::from_str(machine_json)?)
}

fn checkpoint_label(args: &CheckpointCreateArgs) -> CliResult<String> {
    let label = &args.label;
    if label.is_empty() || label.len() > 64 {
        return Err(crate::CliError::Usage(
            "checkpoint label must be 1-64 characters".to_string(),
        ));
    }
    if !label
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return Err(crate::CliError::Usage(
            "checkpoint label may contain only ASCII letters, digits, '-' and '_'".to_string(),
        ));
    }
    Ok(label.clone())
}

fn checkpoint_ref(args: &CheckpointRestoreArgs) -> CliResult<CheckpointRef> {
    if let Some(checkpoint_file) = &args.checkpoint_file {
        let bytes = std::fs::read(checkpoint_file)?;
        return Ok(serde_json::from_slice(&bytes)?);
    }
    checkpoint_ref_by_id(args)
}

fn checkpoint_ref_by_id(args: &CheckpointRestoreArgs) -> CliResult<CheckpointRef> {
    let checkpoint_id = args
        .checkpoint_id
        .as_deref()
        .ok_or_else(|| crate::CliError::Usage("checkpoint id missing after parsing".to_string()))?;
    validate_restore_checkpoint_id(checkpoint_id)?;
    let machine_json = args.machine_json.as_deref().ok_or_else(|| {
        crate::CliError::Usage("machine JSON missing after resolution".to_string())
    })?;
    let machine: MachineHandle = serde_json::from_str(machine_json)?;
    let checkpoint_dir = PathBuf::from(&machine.state_dir)
        .join("checkpoints")
        .join(checkpoint_id);
    let metadata_path = checkpoint_dir.join("checkpoint.json");
    for (path, directory) in [(&checkpoint_dir, true), (&metadata_path, false)] {
        let metadata = std::fs::symlink_metadata(path)?;
        let valid = if directory {
            metadata.file_type().is_dir()
        } else {
            metadata.file_type().is_file()
        };
        if !valid {
            return Err(crate::CliError::Operation(format!(
                "checkpoint path is not a regular {}: {}",
                if directory { "directory" } else { "file" },
                path.display()
            )));
        }
    }
    let bytes = std::fs::read(&metadata_path)?;
    let metadata: Value = serde_json::from_slice(&bytes)?;
    let checkpoint_value = metadata.get("checkpoint").cloned().ok_or_else(|| {
        crate::CliError::Operation(format!(
            "checkpoint metadata does not contain a checkpoint object: {}",
            metadata_path.display()
        ))
    })?;
    let checkpoint: CheckpointRef = serde_json::from_value(checkpoint_value)?;
    if checkpoint.id != checkpoint_id
        || checkpoint.machine_id != machine.id
        || checkpoint.state_dir != machine.state_dir
    {
        return Err(crate::CliError::Operation(
            "checkpoint metadata does not match the selected machine".to_string(),
        ));
    }
    Ok(checkpoint)
}

fn validate_restore_checkpoint_id(checkpoint_id: &str) -> CliResult<()> {
    if checkpoint_id.is_empty()
        || checkpoint_id.len() > 128
        || !checkpoint_id.chars().all(|character| {
            character.is_ascii_alphanumeric() || character == '-' || character == '_'
        })
    {
        return Err(crate::CliError::Usage(
            "checkpoint id must be 1-128 ASCII letters, digits, '-' or '_'".to_string(),
        ));
    }
    Ok(())
}

fn clone_machine_handle(args: &CloneArgs) -> CliResult<MachineHandle> {
    let machine_json = args.machine_json.as_deref().ok_or_else(|| {
        crate::CliError::Usage("machine JSON missing after resolution".to_string())
    })?;
    Ok(serde_json::from_str(machine_json)?)
}

fn clone_name(args: &CloneArgs) -> CliResult<String> {
    if args.name.is_empty()
        || args.name.len() > 48
        || !args
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Err(crate::CliError::Usage(
            "clone name must be 1..=48 ASCII alphanumeric, dash, or underscore characters"
                .to_string(),
        ));
    }
    Ok(args.name.clone())
}

pub(crate) fn write_json_line(output: &mut impl Write, value: &Value) -> CliResult<()> {
    serde_json::to_writer(&mut *output, value)?;
    output.write_all(b"\n")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use winlane_contracts::{BootMode, MachineState, NetworkMode};

    use super::*;

    fn create_args() -> CreateArgs {
        CreateArgs {
            control_socket: PathBuf::from("/tmp/control.sock"),
            name: "builder".to_string(),
            base_image: None,
            memory_mb: 4096,
            cpus: 4,
            disk_gb: 64,
            network: NetworkMode::Isolated,
            tpm: true,
            boot: BootMode::Uefi,
            iso: None,
        }
    }

    fn machine_args() -> MachineArgs {
        MachineArgs {
            control_socket: PathBuf::from("/tmp/control.sock"),
            machine_json: Some(
                serde_json::to_string(&MachineHandle {
                    id: "machine-1".to_string(),
                    name: "builder".to_string(),
                    state_dir: "/tmp/builder".to_string(),
                    backend: "qemu".to_string(),
                    state: MachineState::Stopped,
                })
                .unwrap(),
            ),
            machine_name: None,
        }
    }

    #[test]
    fn create_request_serializes_windows_defaults() {
        let (_socket, request) = machine_request(&MachineCommand::Create(create_args())).unwrap();
        assert_eq!(
            serde_json::to_value(request).unwrap(),
            json!({
                "op": "create",
                "spec": {
                    "name": "builder",
                    "arch": "x86_64",
                    "memory_mb": 4096,
                    "cpus": 4,
                    "disk_gb": 64,
                    "iso_path": null,
                    "base_image_path": null,
                    "guest_agent": true,
                    "network": "isolated",
                    "tpm": true,
                    "boot": "uefi"
                }
            })
        );
    }

    #[test]
    fn create_request_serializes_overrides() {
        let mut args = create_args();
        args.base_image = Some("/tmp/base.qcow2".to_string());
        args.iso = Some("/tmp/windows.iso".to_string());
        args.network = NetworkMode::Nat;
        args.tpm = false;
        args.boot = BootMode::Bios;
        let (_socket, request) = machine_request(&MachineCommand::Create(args)).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["spec"]["base_image_path"], "/tmp/base.qcow2");
        assert_eq!(encoded["spec"]["iso_path"], "/tmp/windows.iso");
        assert_eq!(encoded["spec"]["network"], "nat");
        assert_eq!(encoded["spec"]["tpm"], false);
        assert_eq!(encoded["spec"]["boot"], "bios");
    }

    #[test]
    fn lifecycle_requests_use_exact_ipc_operations() {
        let args = machine_args();
        for (command, operation) in [
            (MachineCommand::Start(args.clone()), "start"),
            (MachineCommand::Stop(args.clone()), "stop"),
            (MachineCommand::Status(args), "status"),
        ] {
            let (_socket, request) = machine_request(&command).unwrap();
            let encoded = serde_json::to_value(request).unwrap();
            assert_eq!(encoded["op"], operation);
            assert_eq!(encoded["machine"]["id"], "machine-1");
            assert_eq!(encoded["machine"]["state"], "stopped");
        }
    }

    #[test]
    fn logs_request_uses_exact_ipc_operation_and_line_bound() {
        let machine = machine_args();
        let args = crate::args::LogsArgs {
            control_socket: machine.control_socket.clone(),
            machine_json: machine.machine_json.clone(),
            machine_name: None,
            lines: 25,
        };
        let (_socket, request) = machine_request(&MachineCommand::Logs(args)).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["op"], "machine_logs");
        assert_eq!(encoded["lines"], 25);
        assert_eq!(encoded["machine"]["id"], "machine-1");
    }

    #[test]
    fn wait_request_uses_exact_ipc_operation() {
        let args = machine_args();
        let wait = WaitArgs {
            control_socket: args.control_socket.clone(),
            machine_json: args.machine_json.clone(),
            machine_name: None,
            timeout_seconds: 3,
        };
        let (_socket, request) = machine_request(&MachineCommand::Wait(wait)).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["op"], "guest_agent_ready");
        assert_eq!(encoded["machine"]["id"], "machine-1");
    }

    #[test]
    fn invalid_machine_json_is_rejected_before_ipc() {
        let mut args = machine_args();
        args.machine_json = Some("not json".to_string());
        assert!(machine_request(&MachineCommand::Status(args)).is_err());
    }

    #[test]
    fn invalid_create_spec_is_rejected_before_ipc() {
        let mut args = create_args();
        args.name = "bad/name".to_string();
        assert!(machine_request(&MachineCommand::Create(args)).is_err());
    }

    #[test]
    fn json_output_is_compact_and_newline_terminated() {
        let mut output = Vec::new();
        write_json_line(&mut output, &json!({"ok":true})).unwrap();
        assert_eq!(output, b"{\"ok\":true}\n");
    }
    #[test]
    fn clone_destroy_requests_use_exact_ipc_operations() {
        let machine = machine_args();
        let clone = super::CloneArgs {
            control_socket: machine.control_socket.clone(),
            machine_json: machine.machine_json.clone(),
            machine_name: None,
            name: "copy-1".to_string(),
        };
        let (_socket, request) = machine_request(&MachineCommand::Clone(clone)).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["op"], "clone_machine");
        assert_eq!(encoded["name"], "copy-1");
        assert_eq!(encoded["machine"]["id"], "machine-1");

        let (_socket, request) = machine_request(&MachineCommand::Destroy(machine)).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["op"], "destroy");
        assert_eq!(encoded["machine"]["id"], "machine-1");
    }

    #[test]
    fn invalid_clone_name_is_rejected_before_ipc() {
        let machine = machine_args();
        for name in ["", "bad/name", "bad.name"] {
            let clone = super::CloneArgs {
                control_socket: machine.control_socket.clone(),
                machine_json: machine.machine_json.clone(),
                machine_name: None,
                name: name.to_string(),
            };
            assert!(machine_request(&MachineCommand::Clone(clone)).is_err());
        }
    }
    #[test]
    fn checkpoint_create_request_uses_exact_ipc_operation() {
        let machine = machine_args();
        let args = CheckpointCreateArgs {
            control_socket: machine.control_socket.clone(),
            machine_json: machine.machine_json,
            machine_name: None,
            label: "before-risky-change".to_string(),
        };
        let (_socket, request) = machine_request(&MachineCommand::CheckpointCreate(args)).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["op"], "checkpoint");
        assert_eq!(encoded["label"], "before-risky-change");
        assert_eq!(encoded["machine"]["id"], "machine-1");
    }

    #[test]
    fn checkpoint_list_request_uses_exact_ipc_operation() {
        let machine = machine_args();
        let args = CheckpointListArgs {
            control_socket: machine.control_socket.clone(),
            machine_json: machine.machine_json.clone(),
            machine_name: None,
        };
        let (_socket, request) = machine_request(&MachineCommand::CheckpointList(args)).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["op"], "checkpoint_list");
        assert_eq!(encoded["machine"]["id"], "machine-1");
    }

    #[test]
    fn checkpoint_delete_request_uses_exact_ipc_operation() {
        let machine = machine_args();
        let args = CheckpointDeleteArgs {
            control_socket: machine.control_socket.clone(),
            machine_json: machine.machine_json,
            machine_name: None,
            checkpoint_id: "checkpoint-1".to_string(),
        };
        let (_socket, request) = machine_request(&MachineCommand::CheckpointDelete(args)).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["op"], "checkpoint_delete");
        assert_eq!(encoded["checkpoint_id"], "checkpoint-1");
        assert_eq!(encoded["machine"]["id"], "machine-1");
    }

    #[test]
    fn checkpoint_restore_request_reads_checkpoint_file() {
        let directory = tempfile::tempdir().unwrap();
        let checkpoint = CheckpointRef {
            id: "checkpoint-1".to_string(),
            machine_id: "machine-1".to_string(),
            created_unix_ms: 42,
            state_dir: "/tmp/builder".to_string(),
            sha256: Some("abc".to_string()),
        };
        let path = directory.path().join("checkpoint.json");
        std::fs::write(&path, serde_json::to_vec(&checkpoint).unwrap()).unwrap();
        let args = CheckpointRestoreArgs {
            control_socket: PathBuf::from("/tmp/control.sock"),
            checkpoint_file: Some(path),
            machine_json: None,
            machine_name: None,
            checkpoint_id: None,
        };
        let (_socket, request) = machine_request(&MachineCommand::CheckpointRestore(args)).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["op"], "restore");
        assert_eq!(encoded["checkpoint"]["id"], "checkpoint-1");
        assert_eq!(encoded["checkpoint"]["machine_id"], "machine-1");
    }

    #[test]
    fn checkpoint_restore_by_id_reads_machine_checkpoint_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let state_dir = directory.path().join("builder");
        let checkpoint_dir = state_dir.join("checkpoints").join("checkpoint-1");
        std::fs::create_dir_all(&checkpoint_dir).unwrap();
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
            serde_json::to_vec(&json!({ "checkpoint": checkpoint })).unwrap(),
        )
        .unwrap();
        let args = CheckpointRestoreArgs {
            control_socket: PathBuf::from("/tmp/control.sock"),
            checkpoint_file: None,
            machine_json: Some(serde_json::to_string(&machine).unwrap()),
            machine_name: None,
            checkpoint_id: Some("checkpoint-1".to_string()),
        };
        let (_socket, request) = machine_request(&MachineCommand::CheckpointRestore(args)).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["op"], "restore");
        assert_eq!(encoded["checkpoint"]["id"], "checkpoint-1");
        assert_eq!(encoded["checkpoint"]["machine_id"], "machine-1");
    }

    #[test]
    fn checkpoint_restore_by_id_rejects_traversal_before_ipc() {
        let machine = machine_args();
        let args = CheckpointRestoreArgs {
            control_socket: machine.control_socket,
            checkpoint_file: None,
            machine_json: machine.machine_json,
            machine_name: None,
            checkpoint_id: Some("../checkpoint-1".to_string()),
        };
        assert!(machine_request(&MachineCommand::CheckpointRestore(args)).is_err());
    }

    #[test]
    fn invalid_checkpoint_label_is_rejected_before_ipc() {
        let machine = machine_args();
        for label in ["", "bad label", "bad/label"] {
            let args = CheckpointCreateArgs {
                control_socket: machine.control_socket.clone(),
                machine_json: machine.machine_json.clone(),
                machine_name: None,
                label: label.to_string(),
            };
            assert!(machine_request(&MachineCommand::CheckpointCreate(args)).is_err());
        }
    }
}
