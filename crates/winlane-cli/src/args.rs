use std::path::PathBuf;

use winlane_contracts::{BootMode, NetworkMode};

use crate::CliError;

pub const USAGE: &str = "usage: winlane view --control-socket PATH --machine-json JSON [--fps N] [--cell-width-px N] [--cell-height-px N]\n       winlane daemon --root PATH --control-socket PATH\n       winlane machine create --control-socket PATH --name NAME [--base-image PATH] [--memory-mb N] [--cpus N] [--disk-gb N] [--network isolated|nat] [--no-tpm] [--bios] [--iso PATH]\n       winlane machine start|stop|status --control-socket PATH --machine-json JSON\n       winlane mcp --control-socket PATH [--evidence-dir PATH]";
pub const VIEW_USAGE: &str = "usage: winlane view --control-socket PATH --machine-json JSON [--fps N] [--cell-width-px N] [--cell-height-px N]";
pub const DAEMON_USAGE: &str = "usage: winlane daemon --root PATH --control-socket PATH";
pub const MACHINE_CREATE_USAGE: &str = "usage: winlane machine create --control-socket PATH --name NAME [--base-image PATH] [--memory-mb N] [--cpus N] [--disk-gb N] [--network isolated|nat] [--no-tpm] [--bios] [--iso PATH]";
pub const MACHINE_ACTION_USAGE: &str =
    "usage: winlane machine start|stop|status --control-socket PATH --machine-json JSON";
pub const MCP_USAGE: &str = "usage: winlane mcp --control-socket PATH [--evidence-dir PATH]";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewArgs {
    pub control_socket: PathBuf,
    pub machine_json: String,
    pub fps: u32,
    pub cell_width_px: u16,
    pub cell_height_px: u16,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonArgs {
    pub root: PathBuf,
    pub control_socket: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateArgs {
    pub control_socket: PathBuf,
    pub name: String,
    pub base_image: Option<String>,
    pub memory_mb: u32,
    pub cpus: u16,
    pub disk_gb: u32,
    pub network: NetworkMode,
    pub tpm: bool,
    pub boot: BootMode,
    pub iso: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineArgs {
    pub control_socket: PathBuf,
    pub machine_json: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineCommand {
    Create(CreateArgs),
    Start(MachineArgs),
    Stop(MachineArgs),
    Status(MachineArgs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpArgs {
    pub control_socket: PathBuf,
    pub evidence_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    View(ViewArgs),
    Daemon(DaemonArgs),
    Machine(MachineCommand),
    Mcp(McpArgs),
}

pub fn parse_args(args: &[String]) -> Result<Command, CliError> {
    if args.is_empty() || args.iter().any(|arg| arg == "--help" || arg == "-h") {
        return Ok(Command::Help);
    }
    match args[0].as_str() {
        "view" => parse_view_args(&args[1..]).map(Command::View),
        "daemon" => parse_daemon_args(&args[1..]).map(Command::Daemon),
        "machine" => parse_machine_args(&args[1..]).map(Command::Machine),
        "mcp" => parse_mcp_args(&args[1..]).map(Command::Mcp),
        command => Err(CliError::Usage(format!(
            "unknown command {command}; {USAGE}"
        ))),
    }
}

fn parse_view_args(args: &[String]) -> Result<ViewArgs, CliError> {
    let mut control_socket = None;
    let mut machine_json = None;
    let mut fps = 5_u32;
    let mut cell_width_px = 8_u16;
    let mut cell_height_px = 16_u16;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].clone();
        match flag.as_str() {
            "--control-socket" => {
                control_socket = Some(PathBuf::from(take_value(args, &mut index, VIEW_USAGE)?));
            }
            "--machine-json" => {
                machine_json = Some(take_value(args, &mut index, VIEW_USAGE)?.to_string());
            }
            "--fps" => {
                fps = parse_bounded(
                    take_value(args, &mut index, VIEW_USAGE)?,
                    "fps",
                    1,
                    30,
                    VIEW_USAGE,
                )?;
            }
            "--cell-width-px" => {
                cell_width_px = parse_bounded(
                    take_value(args, &mut index, VIEW_USAGE)?,
                    "cell width",
                    1,
                    512,
                    VIEW_USAGE,
                )?;
            }
            "--cell-height-px" => {
                cell_height_px = parse_bounded(
                    take_value(args, &mut index, VIEW_USAGE)?,
                    "cell height",
                    1,
                    512,
                    VIEW_USAGE,
                )?;
            }
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown view flag {flag}; {VIEW_USAGE}"
                )));
            }
        }
    }

    Ok(ViewArgs {
        control_socket: required(control_socket, "--control-socket", VIEW_USAGE)?,
        machine_json: required(machine_json, "--machine-json", VIEW_USAGE)?,
        fps,
        cell_width_px,
        cell_height_px,
    })
}

fn parse_daemon_args(args: &[String]) -> Result<DaemonArgs, CliError> {
    let mut root = None;
    let mut control_socket = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].clone();
        match flag.as_str() {
            "--root" => root = Some(PathBuf::from(take_value(args, &mut index, DAEMON_USAGE)?)),
            "--control-socket" => {
                control_socket = Some(PathBuf::from(take_value(args, &mut index, DAEMON_USAGE)?));
            }
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown daemon flag {flag}; {DAEMON_USAGE}"
                )));
            }
        }
    }
    Ok(DaemonArgs {
        root: required(root, "--root", DAEMON_USAGE)?,
        control_socket: required(control_socket, "--control-socket", DAEMON_USAGE)?,
    })
}

fn parse_machine_args(args: &[String]) -> Result<MachineCommand, CliError> {
    let Some(action) = args.first() else {
        return Err(CliError::Usage(format!("missing machine action; {USAGE}")));
    };
    match action.as_str() {
        "create" => parse_machine_create_args(&args[1..]).map(MachineCommand::Create),
        "start" => parse_machine_action_args(&args[1..]).map(MachineCommand::Start),
        "stop" => parse_machine_action_args(&args[1..]).map(MachineCommand::Stop),
        "status" => parse_machine_action_args(&args[1..]).map(MachineCommand::Status),
        action => Err(CliError::Usage(format!(
            "unknown machine action {action}; {USAGE}"
        ))),
    }
}

fn parse_machine_create_args(args: &[String]) -> Result<CreateArgs, CliError> {
    let mut control_socket = None;
    let mut name = None;
    let mut base_image = None;
    let mut memory_mb = 4096_u32;
    let mut cpus = 4_u16;
    let mut disk_gb = 64_u32;
    let mut network = NetworkMode::Isolated;
    let mut tpm = true;
    let mut boot = BootMode::Uefi;
    let mut iso = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].clone();
        match flag.as_str() {
            "--control-socket" => {
                control_socket = Some(PathBuf::from(take_value(
                    args,
                    &mut index,
                    MACHINE_CREATE_USAGE,
                )?));
            }
            "--name" => {
                name = Some(take_value(args, &mut index, MACHINE_CREATE_USAGE)?.to_string());
            }
            "--base-image" => {
                base_image = Some(take_value(args, &mut index, MACHINE_CREATE_USAGE)?.to_string());
            }
            "--memory-mb" => {
                memory_mb = parse_number(
                    take_value(args, &mut index, MACHINE_CREATE_USAGE)?,
                    "memory-mb",
                    MACHINE_CREATE_USAGE,
                )?;
            }
            "--cpus" => {
                cpus = parse_number(
                    take_value(args, &mut index, MACHINE_CREATE_USAGE)?,
                    "cpus",
                    MACHINE_CREATE_USAGE,
                )?;
            }
            "--disk-gb" => {
                disk_gb = parse_number(
                    take_value(args, &mut index, MACHINE_CREATE_USAGE)?,
                    "disk-gb",
                    MACHINE_CREATE_USAGE,
                )?;
            }
            "--network" => {
                network = match take_value(args, &mut index, MACHINE_CREATE_USAGE)? {
                    "isolated" => NetworkMode::Isolated,
                    "nat" => NetworkMode::Nat,
                    value => {
                        return Err(CliError::Usage(format!(
                            "network must be isolated or nat, got {value}; {MACHINE_CREATE_USAGE}"
                        )));
                    }
                };
            }
            "--no-tpm" => {
                tpm = false;
                index += 1;
            }
            "--bios" => {
                boot = BootMode::Bios;
                index += 1;
            }
            "--iso" => {
                iso = Some(take_value(args, &mut index, MACHINE_CREATE_USAGE)?.to_string());
            }
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown machine create flag {flag}; {MACHINE_CREATE_USAGE}"
                )));
            }
        }
    }
    Ok(CreateArgs {
        control_socket: required(control_socket, "--control-socket", MACHINE_CREATE_USAGE)?,
        name: required(name, "--name", MACHINE_CREATE_USAGE)?,
        base_image,
        memory_mb,
        cpus,
        disk_gb,
        network,
        tpm,
        boot,
        iso,
    })
}

fn parse_machine_action_args(args: &[String]) -> Result<MachineArgs, CliError> {
    let mut control_socket = None;
    let mut machine_json = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].clone();
        match flag.as_str() {
            "--control-socket" => {
                control_socket = Some(PathBuf::from(take_value(
                    args,
                    &mut index,
                    MACHINE_ACTION_USAGE,
                )?));
            }
            "--machine-json" => {
                machine_json =
                    Some(take_value(args, &mut index, MACHINE_ACTION_USAGE)?.to_string());
            }
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown machine flag {flag}; {MACHINE_ACTION_USAGE}"
                )));
            }
        }
    }
    Ok(MachineArgs {
        control_socket: required(control_socket, "--control-socket", MACHINE_ACTION_USAGE)?,
        machine_json: required(machine_json, "--machine-json", MACHINE_ACTION_USAGE)?,
    })
}

fn parse_mcp_args(args: &[String]) -> Result<McpArgs, CliError> {
    let mut control_socket = None;
    let mut evidence_dir = None;
    let mut index = 0;
    while index < args.len() {
        let flag = args[index].clone();
        match flag.as_str() {
            "--control-socket" => {
                control_socket = Some(PathBuf::from(take_value(args, &mut index, MCP_USAGE)?));
            }
            "--evidence-dir" => {
                evidence_dir = Some(PathBuf::from(take_value(args, &mut index, MCP_USAGE)?));
            }
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown mcp flag {flag}; {MCP_USAGE}"
                )));
            }
        }
    }
    Ok(McpArgs {
        control_socket: required(control_socket, "--control-socket", MCP_USAGE)?,
        evidence_dir,
    })
}

fn take_value<'a>(args: &'a [String], index: &mut usize, usage: &str) -> Result<&'a str, CliError> {
    let flag = &args[*index];
    *index += 1;
    let value = args
        .get(*index)
        .ok_or_else(|| CliError::Usage(format!("missing value for {flag}; {usage}")))?;
    *index += 1;
    Ok(value)
}

fn required<T>(value: Option<T>, flag: &str, usage: &str) -> Result<T, CliError> {
    value.ok_or_else(|| CliError::Usage(format!("missing {flag}; {usage}")))
}

fn parse_number<T>(value: &str, name: &str, usage: &str) -> Result<T, CliError>
where
    T: std::str::FromStr,
{
    value
        .parse::<T>()
        .map_err(|_| CliError::Usage(format!("{name} must be an integer; {usage}")))
}

fn parse_bounded<T>(
    value: &str,
    name: &str,
    minimum: T,
    maximum: T,
    usage: &str,
) -> Result<T, CliError>
where
    T: std::str::FromStr + PartialOrd + Copy,
{
    let parsed = parse_number(value, name, usage)?;
    if parsed < minimum || parsed > maximum {
        return Err(CliError::Usage(format!(
            "{name} is outside the supported range; {usage}"
        )));
    }
    Ok(parsed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_minimal_view_command() {
        let args = vec![
            "view".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
        ];
        assert_eq!(
            parse_args(&args).unwrap(),
            Command::View(ViewArgs {
                control_socket: PathBuf::from("/tmp/control.sock"),
                machine_json: "{}".to_string(),
                fps: 5,
                cell_width_px: 8,
                cell_height_px: 16,
            })
        );
    }

    #[test]
    fn parses_daemon_command() {
        let args = vec![
            "daemon".to_string(),
            "--root".to_string(),
            "/tmp/machines".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
        ];
        assert_eq!(
            parse_args(&args).unwrap(),
            Command::Daemon(DaemonArgs {
                root: PathBuf::from("/tmp/machines"),
                control_socket: PathBuf::from("/tmp/control.sock"),
            })
        );
    }

    #[test]
    fn machine_create_defaults_to_windows_tier() {
        let args = vec![
            "machine".to_string(),
            "create".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
            "--name".to_string(),
            "builder".to_string(),
        ];
        assert_eq!(
            parse_args(&args).unwrap(),
            Command::Machine(MachineCommand::Create(CreateArgs {
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
            }))
        );
    }

    #[test]
    fn machine_create_parses_overrides() {
        let args = vec![
            "machine".to_string(),
            "create".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
            "--name".to_string(),
            "builder".to_string(),
            "--base-image".to_string(),
            "/tmp/base.qcow2".to_string(),
            "--memory-mb".to_string(),
            "8192".to_string(),
            "--cpus".to_string(),
            "8".to_string(),
            "--disk-gb".to_string(),
            "128".to_string(),
            "--network".to_string(),
            "nat".to_string(),
            "--no-tpm".to_string(),
            "--bios".to_string(),
            "--iso".to_string(),
            "/tmp/windows.iso".to_string(),
        ];
        let Command::Machine(MachineCommand::Create(create)) = parse_args(&args).unwrap() else {
            panic!("expected machine create command");
        };
        assert_eq!(create.memory_mb, 8192);
        assert_eq!(create.cpus, 8);
        assert_eq!(create.disk_gb, 128);
        assert_eq!(create.network, NetworkMode::Nat);
        assert!(!create.tpm);
        assert_eq!(create.boot, BootMode::Bios);
        assert_eq!(create.base_image.as_deref(), Some("/tmp/base.qcow2"));
        assert_eq!(create.iso.as_deref(), Some("/tmp/windows.iso"));
    }

    #[test]
    fn machine_action_parse_is_shared_by_start_stop_and_status() {
        for action in ["start", "stop", "status"] {
            let args = vec![
                "machine".to_string(),
                action.to_string(),
                "--control-socket".to_string(),
                "/tmp/control.sock".to_string(),
                "--machine-json".to_string(),
                "{}".to_string(),
            ];
            let parsed = parse_args(&args).unwrap();
            let expected = MachineArgs {
                control_socket: PathBuf::from("/tmp/control.sock"),
                machine_json: "{}".to_string(),
            };
            match action {
                "start" => assert_eq!(parsed, Command::Machine(MachineCommand::Start(expected))),
                "stop" => assert_eq!(parsed, Command::Machine(MachineCommand::Stop(expected))),
                _ => assert_eq!(parsed, Command::Machine(MachineCommand::Status(expected))),
            }
        }
    }

    #[test]
    fn parses_mcp_command() {
        let args = vec![
            "mcp".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
            "--evidence-dir".to_string(),
            "/tmp/evidence".to_string(),
        ];
        assert_eq!(
            parse_args(&args).unwrap(),
            Command::Mcp(McpArgs {
                control_socket: PathBuf::from("/tmp/control.sock"),
                evidence_dir: Some(PathBuf::from("/tmp/evidence")),
            })
        );
    }

    #[test]
    fn rejects_unknown_command() {
        let result = parse_args(&["clone".to_string()]);
        assert!(matches!(result, Err(CliError::Usage(_))));
    }

    #[test]
    fn rejects_missing_flag_value() {
        let result = parse_args(&["view".to_string(), "--fps".to_string()]);
        assert!(matches!(result, Err(CliError::Usage(_))));
    }

    #[test]
    fn rejects_value_after_boolean_create_flag() {
        let result = parse_args(&[
            "machine".to_string(),
            "create".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
            "--name".to_string(),
            "builder".to_string(),
            "--no-tpm".to_string(),
            "false".to_string(),
        ]);
        assert!(matches!(result, Err(CliError::Usage(_))));
    }
}
