use std::io::Write;
use std::path::PathBuf;

use serde_json::Value;
use winlane_contracts::{CheckpointRef, CpuArch, MachineHandle, MachineSpec};
use winlane_core::QemuBackend;
use winlane_ipc::{ControlRequest, UnixControlClient, serve_unix_control};

use crate::CliResult;
use crate::args::{
    CheckpointCreateArgs, CheckpointRestoreArgs, CloneArgs, CreateArgs, DaemonArgs, MachineArgs,
    MachineCommand, McpArgs,
};

pub async fn run_daemon(args: DaemonArgs) -> CliResult<()> {
    let backend = QemuBackend::new(args.root).await?;
    serve_unix_control(args.control_socket, backend).await?;
    Ok(())
}

pub async fn run_machine_command(
    command: &MachineCommand,
    output: &mut impl Write,
) -> CliResult<()> {
    let command = resolve_machine_command(command).await?;
    let (control_socket, request) = machine_request(&command)?;
    let result = UnixControlClient::new(control_socket)
        .invoke(&request)
        .await?;
    write_json_line(output, &result)
}

async fn resolve_machine_command(command: &MachineCommand) -> CliResult<MachineCommand> {
    match command {
        MachineCommand::Start(args) => Ok(MachineCommand::Start(resolve_machine_args(args).await?)),
        MachineCommand::Stop(args) => Ok(MachineCommand::Stop(resolve_machine_args(args).await?)),
        MachineCommand::Status(args) => {
            Ok(MachineCommand::Status(resolve_machine_args(args).await?))
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
            let machine =
                resolve_machine(&args.control_socket, &args.machine_json, &args.machine_name)
                    .await?;
            let mut args = args.clone();
            args.machine_json = Some(serde_json::to_string(&machine)?);
            args.machine_name = None;
            Ok(MachineCommand::CheckpointCreate(args))
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

pub(crate) async fn resolve_machine(
    control_socket: &std::path::Path,
    machine_json: &Option<String>,
    machine_name: &Option<String>,
) -> CliResult<MachineHandle> {
    if let Some(machine_json) = machine_json {
        return Ok(serde_json::from_str(machine_json)?);
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

fn checkpoint_machine_handle(args: &CheckpointCreateArgs) -> CliResult<MachineHandle> {
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
    let bytes = std::fs::read(&args.checkpoint_file)?;
    Ok(serde_json::from_slice(&bytes)?)
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
            checkpoint_file: path,
        };
        let (_socket, request) = machine_request(&MachineCommand::CheckpointRestore(args)).unwrap();
        let encoded = serde_json::to_value(request).unwrap();
        assert_eq!(encoded["op"], "restore");
        assert_eq!(encoded["checkpoint"]["id"], "checkpoint-1");
        assert_eq!(encoded["checkpoint"]["machine_id"], "machine-1");
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
