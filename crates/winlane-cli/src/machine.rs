use std::io::Write;
use std::path::PathBuf;

use serde_json::Value;
use winlane_contracts::{CpuArch, MachineHandle, MachineSpec};
use winlane_core::QemuBackend;
use winlane_ipc::{ControlRequest, UnixControlClient, serve_unix_control};

use crate::CliResult;
use crate::args::{CloneArgs, CreateArgs, DaemonArgs, MachineArgs, MachineCommand, McpArgs};

pub async fn run_daemon(args: DaemonArgs) -> CliResult<()> {
    let backend = QemuBackend::new(args.root).await?;
    serve_unix_control(args.control_socket, backend).await?;
    Ok(())
}

pub async fn run_machine_command(
    command: &MachineCommand,
    output: &mut impl Write,
) -> CliResult<()> {
    let (control_socket, request) = machine_request(command)?;
    let result = UnixControlClient::new(control_socket)
        .invoke(&request)
        .await?;
    write_json_line(output, &result)
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
    Ok(serde_json::from_str(&args.machine_json)?)
}

fn clone_machine_handle(args: &CloneArgs) -> CliResult<MachineHandle> {
    Ok(serde_json::from_str(&args.machine_json)?)
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
            machine_json: serde_json::to_string(&MachineHandle {
                id: "machine-1".to_string(),
                name: "builder".to_string(),
                state_dir: "/tmp/builder".to_string(),
                backend: "qemu".to_string(),
                state: MachineState::Stopped,
            })
            .unwrap(),
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
        args.machine_json = "not json".to_string();
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
                name: name.to_string(),
            };
            assert!(machine_request(&MachineCommand::Clone(clone)).is_err());
        }
    }
}
