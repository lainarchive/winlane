use std::collections::HashMap;
use std::path::PathBuf;

use winlane_contracts::{BootMode, NetworkMode};
use winlane_guest::uia::UiaView;

use crate::CliError;

pub const USAGE: &str = "usage: winlane view --control-socket PATH (--machine-json JSON | --machine NAME) [--fps N] [--cell-width-px N] [--cell-height-px N]\n       winlane daemon --root PATH --control-socket PATH\n       winlane machine create --control-socket PATH --name NAME [--base-image PATH] [--memory-mb N] [--cpus N] [--disk-gb N] [--network isolated|nat] [--no-tpm] [--bios] [--iso PATH]\n       winlane machine list --control-socket PATH\n       winlane machine start|stop|status --control-socket PATH (--machine-json JSON | --machine NAME)\n       winlane machine clone --control-socket PATH (--machine-json JSON | --machine NAME) --name NAME\n       winlane machine destroy --control-socket PATH (--machine-json JSON | --machine NAME)\n       winlane evidence export --evidence-dir PATH --output PATH\n       winlane evidence tail --evidence-dir PATH [--limit N]\n       winlane mcp --control-socket PATH [--evidence-dir PATH]\n       winlane guest ui-tree (--machine-json JSON | --machine NAME) [--control-socket PATH] [--max-depth N] [--max-nodes N] [--view control|raw]\n       winlane guest invoke (--machine-json JSON | --machine NAME) [--control-socket PATH] --node PATH [--view control|raw]\n       winlane guest set-text (--machine-json JSON | --machine NAME) [--control-socket PATH] --node PATH --text TEXT [--view control|raw]\n       winlane guest run (--machine-json JSON | --machine NAME) [--control-socket PATH] --command ABSOLUTE_GUEST_PATH [--arg ARG]... [--timeout-ms N]\n       winlane guest import (--machine-json JSON | --machine NAME) [--control-socket PATH] --host-path PATH --guest-path PATH\n       winlane guest export (--machine-json JSON | --machine NAME) [--control-socket PATH] --guest-path PATH --host-path PATH";
pub const VIEW_USAGE: &str = "usage: winlane view --control-socket PATH (--machine-json JSON | --machine NAME) [--fps N] [--cell-width-px N] [--cell-height-px N]";
pub const DAEMON_USAGE: &str = "usage: winlane daemon --root PATH --control-socket PATH";
pub const MACHINE_CREATE_USAGE: &str = "usage: winlane machine create --control-socket PATH --name NAME [--base-image PATH] [--memory-mb N] [--cpus N] [--disk-gb N] [--network isolated|nat] [--no-tpm] [--bios] [--iso PATH]";
pub const MACHINE_LIST_USAGE: &str = "usage: winlane machine list --control-socket PATH";
pub const MACHINE_ACTION_USAGE: &str = "usage: winlane machine start|stop|status|destroy --control-socket PATH (--machine-json JSON | --machine NAME)";
pub const MACHINE_CLONE_USAGE: &str = "usage: winlane machine clone --control-socket PATH (--machine-json JSON | --machine NAME) --name NAME";
pub const MCP_USAGE: &str = "usage: winlane mcp --control-socket PATH [--evidence-dir PATH]";
pub const EVIDENCE_EXPORT_USAGE: &str =
    "usage: winlane evidence export --evidence-dir PATH --output PATH";
pub const EVIDENCE_TAIL_USAGE: &str =
    "usage: winlane evidence tail --evidence-dir PATH [--limit N]";
pub const GUEST_UI_TREE_USAGE: &str = "usage: winlane guest ui-tree (--machine-json JSON | --machine NAME) [--control-socket PATH] [--max-depth N] [--max-nodes N] [--view control|raw]";
pub const GUEST_INVOKE_USAGE: &str = "usage: winlane guest invoke (--machine-json JSON | --machine NAME) [--control-socket PATH] --node PATH [--view control|raw]";
pub const GUEST_SET_TEXT_USAGE: &str = "usage: winlane guest set-text (--machine-json JSON | --machine NAME) [--control-socket PATH] --node PATH --text TEXT [--view control|raw]";
pub const GUEST_RUN_USAGE: &str = "usage: winlane guest run (--machine-json JSON | --machine NAME) [--control-socket PATH] --command ABSOLUTE_GUEST_PATH [--arg ARG]... [--timeout-ms N]";
pub const GUEST_IMPORT_USAGE: &str = "usage: winlane guest import (--machine-json JSON | --machine NAME) [--control-socket PATH] --host-path PATH --guest-path PATH";
pub const GUEST_EXPORT_USAGE: &str = "usage: winlane guest export (--machine-json JSON | --machine NAME) [--control-socket PATH] --guest-path PATH --host-path PATH";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewArgs {
    pub control_socket: PathBuf,
    pub machine_json: Option<String>,
    pub machine_name: Option<String>,
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
    pub machine_json: Option<String>,
    pub machine_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListArgs {
    pub control_socket: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloneArgs {
    pub control_socket: PathBuf,
    pub machine_json: Option<String>,
    pub machine_name: Option<String>,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineCommand {
    Create(CreateArgs),
    List(ListArgs),
    Start(MachineArgs),
    Stop(MachineArgs),
    Status(MachineArgs),
    Clone(CloneArgs),
    Destroy(MachineArgs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct McpArgs {
    pub control_socket: PathBuf,
    pub evidence_dir: Option<PathBuf>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestMachineArgs {
    pub control_socket: Option<PathBuf>,
    pub machine_json: Option<String>,
    pub machine_name: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestUiTreeArgs {
    pub machine: GuestMachineArgs,
    pub max_depth: u16,
    pub max_nodes: u32,
    pub view: UiaView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestInvokeArgs {
    pub machine: GuestMachineArgs,
    pub node: String,
    pub view: UiaView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestSetTextArgs {
    pub machine: GuestMachineArgs,
    pub node: String,
    pub text: String,
    pub view: UiaView,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestRunArgs {
    pub machine: GuestMachineArgs,
    pub command: String,
    pub args: Vec<String>,
    pub timeout_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestImportArgs {
    pub machine: GuestMachineArgs,
    pub host_path: PathBuf,
    pub guest_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestExportArgs {
    pub machine: GuestMachineArgs,
    pub guest_path: String,
    pub host_path: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GuestCommand {
    UiTree(GuestUiTreeArgs),
    Invoke(GuestInvokeArgs),
    SetText(GuestSetTextArgs),
    Run(GuestRunArgs),
    Import(GuestImportArgs),
    Export(GuestExportArgs),
}

impl GuestCommand {
    pub fn machine(&self) -> &GuestMachineArgs {
        match self {
            Self::UiTree(args) => &args.machine,
            Self::Invoke(args) => &args.machine,
            Self::SetText(args) => &args.machine,
            Self::Run(args) => &args.machine,
            Self::Import(args) => &args.machine,
            Self::Export(args) => &args.machine,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceExportArgs {
    pub evidence_dir: PathBuf,
    pub output: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EvidenceTailArgs {
    pub evidence_dir: PathBuf,
    pub limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EvidenceCommand {
    Export(EvidenceExportArgs),
    Tail(EvidenceTailArgs),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    Help,
    View(ViewArgs),
    Daemon(DaemonArgs),
    Machine(MachineCommand),
    Mcp(McpArgs),
    Guest(GuestCommand),
    Evidence(EvidenceCommand),
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
        "guest" => parse_guest_args(&args[1..]).map(Command::Guest),
        "evidence" => parse_evidence_args(&args[1..]).map(Command::Evidence),
        command => Err(CliError::Usage(format!(
            "unknown command {command}; {USAGE}"
        ))),
    }
}

fn parse_view_args(args: &[String]) -> Result<ViewArgs, CliError> {
    let mut control_socket = None;
    let mut machine_json = None;
    let mut machine_name = None;
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
            "--machine" => {
                machine_name = Some(take_value(args, &mut index, VIEW_USAGE)?.to_string());
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

    validate_machine_reference(&machine_json, &machine_name, VIEW_USAGE)?;
    Ok(ViewArgs {
        control_socket: required(control_socket, "--control-socket", VIEW_USAGE)?,
        machine_json,
        machine_name,
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
        "list" => parse_machine_list_args(&args[1..]).map(MachineCommand::List),
        "start" => parse_machine_action_args(&args[1..]).map(MachineCommand::Start),
        "stop" => parse_machine_action_args(&args[1..]).map(MachineCommand::Stop),
        "status" => parse_machine_action_args(&args[1..]).map(MachineCommand::Status),
        "clone" => parse_machine_clone_args(&args[1..]).map(MachineCommand::Clone),
        "destroy" => parse_machine_action_args(&args[1..]).map(MachineCommand::Destroy),
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

fn parse_machine_list_args(args: &[String]) -> Result<ListArgs, CliError> {
    let options = ParsedOptions::parse(args, MACHINE_LIST_USAGE, &["--control-socket"])?;
    Ok(ListArgs {
        control_socket: PathBuf::from(options.required("--control-socket", MACHINE_LIST_USAGE)?),
    })
}

fn parse_machine_action_args(args: &[String]) -> Result<MachineArgs, CliError> {
    let mut control_socket = None;
    let mut machine_json = None;
    let mut machine_name = None;
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
            "--machine" => {
                machine_name =
                    Some(take_value(args, &mut index, MACHINE_ACTION_USAGE)?.to_string());
            }
            _ => {
                return Err(CliError::Usage(format!(
                    "unknown machine flag {flag}; {MACHINE_ACTION_USAGE}"
                )));
            }
        }
    }
    validate_machine_reference(&machine_json, &machine_name, MACHINE_ACTION_USAGE)?;
    Ok(MachineArgs {
        control_socket: required(control_socket, "--control-socket", MACHINE_ACTION_USAGE)?,
        machine_json,
        machine_name,
    })
}

fn parse_machine_clone_args(args: &[String]) -> Result<CloneArgs, CliError> {
    let options = ParsedOptions::parse(
        args,
        MACHINE_CLONE_USAGE,
        &["--control-socket", "--machine-json", "--machine", "--name"],
    )?;
    let machine_json = options
        .one("--machine-json", MACHINE_CLONE_USAGE)?
        .map(str::to_string);
    let machine_name = options
        .one("--machine", MACHINE_CLONE_USAGE)?
        .map(str::to_string);
    validate_machine_reference(&machine_json, &machine_name, MACHINE_CLONE_USAGE)?;
    Ok(CloneArgs {
        control_socket: PathBuf::from(options.required("--control-socket", MACHINE_CLONE_USAGE)?),
        machine_json,
        machine_name,
        name: options.required("--name", MACHINE_CLONE_USAGE)?.to_string(),
    })
}

fn parse_evidence_args(args: &[String]) -> Result<EvidenceCommand, CliError> {
    let Some(action) = args.first() else {
        return Err(CliError::Usage(format!("missing evidence action; {USAGE}")));
    };
    match action.as_str() {
        "export" => {
            let options = ParsedOptions::parse(
                &args[1..],
                EVIDENCE_EXPORT_USAGE,
                &["--evidence-dir", "--output"],
            )?;
            Ok(EvidenceCommand::Export(EvidenceExportArgs {
                evidence_dir: PathBuf::from(
                    options.required("--evidence-dir", EVIDENCE_EXPORT_USAGE)?,
                ),
                output: PathBuf::from(options.required("--output", EVIDENCE_EXPORT_USAGE)?),
            }))
        }
        "tail" => {
            let options = ParsedOptions::parse(
                &args[1..],
                EVIDENCE_TAIL_USAGE,
                &["--evidence-dir", "--limit"],
            )?;
            let limit = match options.one("--limit", EVIDENCE_TAIL_USAGE)? {
                Some(value) => parse_bounded(value, "limit", 1, 1000, EVIDENCE_TAIL_USAGE)?,
                None => 20,
            };
            Ok(EvidenceCommand::Tail(EvidenceTailArgs {
                evidence_dir: PathBuf::from(
                    options.required("--evidence-dir", EVIDENCE_TAIL_USAGE)?,
                ),
                limit,
            }))
        }
        action => Err(CliError::Usage(format!(
            "unknown evidence action {action}; {USAGE}"
        ))),
    }
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

fn parse_guest_args(args: &[String]) -> Result<GuestCommand, CliError> {
    let Some(action) = args.first() else {
        return Err(CliError::Usage(format!("missing guest action; {USAGE}")));
    };
    match action.as_str() {
        "ui-tree" => parse_guest_ui_tree_args(&args[1..]).map(GuestCommand::UiTree),
        "invoke" => parse_guest_invoke_args(&args[1..]).map(GuestCommand::Invoke),
        "set-text" => parse_guest_set_text_args(&args[1..]).map(GuestCommand::SetText),
        "run" => parse_guest_run_args(&args[1..]).map(GuestCommand::Run),
        "import" => parse_guest_import_args(&args[1..]).map(GuestCommand::Import),
        "export" => parse_guest_export_args(&args[1..]).map(GuestCommand::Export),
        action => Err(CliError::Usage(format!(
            "unknown guest action {action}; {USAGE}"
        ))),
    }
}

fn parse_guest_ui_tree_args(args: &[String]) -> Result<GuestUiTreeArgs, CliError> {
    let options = ParsedOptions::parse(
        args,
        GUEST_UI_TREE_USAGE,
        &[
            "--machine-json",
            "--machine",
            "--control-socket",
            "--max-depth",
            "--max-nodes",
            "--view",
        ],
    )?;
    let max_depth = match options.one("--max-depth", GUEST_UI_TREE_USAGE)? {
        Some(value) => parse_bounded(value, "max-depth", 1, 32, GUEST_UI_TREE_USAGE)?,
        None => 8,
    };
    let max_nodes = match options.one("--max-nodes", GUEST_UI_TREE_USAGE)? {
        Some(value) => parse_bounded(value, "max-nodes", 1, 20_000, GUEST_UI_TREE_USAGE)?,
        None => 2_000,
    };
    Ok(GuestUiTreeArgs {
        machine: parse_guest_machine(&options, GUEST_UI_TREE_USAGE)?,
        max_depth,
        max_nodes,
        view: parse_uia_view(&options, GUEST_UI_TREE_USAGE)?,
    })
}

fn parse_guest_invoke_args(args: &[String]) -> Result<GuestInvokeArgs, CliError> {
    let options = ParsedOptions::parse(
        args,
        GUEST_INVOKE_USAGE,
        &[
            "--machine-json",
            "--machine",
            "--control-socket",
            "--node",
            "--view",
        ],
    )?;
    Ok(GuestInvokeArgs {
        machine: parse_guest_machine(&options, GUEST_INVOKE_USAGE)?,
        node: options.required("--node", GUEST_INVOKE_USAGE)?.to_string(),
        view: parse_uia_view(&options, GUEST_INVOKE_USAGE)?,
    })
}

fn parse_guest_set_text_args(args: &[String]) -> Result<GuestSetTextArgs, CliError> {
    let options = ParsedOptions::parse(
        args,
        GUEST_SET_TEXT_USAGE,
        &[
            "--machine-json",
            "--machine",
            "--control-socket",
            "--node",
            "--text",
            "--view",
        ],
    )?;
    Ok(GuestSetTextArgs {
        machine: parse_guest_machine(&options, GUEST_SET_TEXT_USAGE)?,
        node: options
            .required("--node", GUEST_SET_TEXT_USAGE)?
            .to_string(),
        text: options
            .required("--text", GUEST_SET_TEXT_USAGE)?
            .to_string(),
        view: parse_uia_view(&options, GUEST_SET_TEXT_USAGE)?,
    })
}

fn parse_guest_run_args(args: &[String]) -> Result<GuestRunArgs, CliError> {
    let options = ParsedOptions::parse(
        args,
        GUEST_RUN_USAGE,
        &[
            "--machine-json",
            "--machine",
            "--control-socket",
            "--command",
            "--arg",
            "--timeout-ms",
        ],
    )?;
    let timeout_ms = match options.one("--timeout-ms", GUEST_RUN_USAGE)? {
        Some(value) => parse_bounded(value, "timeout-ms", 1, 600_000, GUEST_RUN_USAGE)?,
        None => 30_000,
    };
    Ok(GuestRunArgs {
        machine: parse_guest_machine(&options, GUEST_RUN_USAGE)?,
        command: options.required("--command", GUEST_RUN_USAGE)?.to_string(),
        args: options
            .repeated("--arg")
            .into_iter()
            .map(str::to_string)
            .collect(),
        timeout_ms,
    })
}

fn parse_guest_import_args(args: &[String]) -> Result<GuestImportArgs, CliError> {
    let options = ParsedOptions::parse(
        args,
        GUEST_IMPORT_USAGE,
        &[
            "--machine-json",
            "--machine",
            "--control-socket",
            "--host-path",
            "--guest-path",
        ],
    )?;
    Ok(GuestImportArgs {
        machine: parse_guest_machine(&options, GUEST_IMPORT_USAGE)?,
        host_path: PathBuf::from(options.required("--host-path", GUEST_IMPORT_USAGE)?),
        guest_path: options
            .required("--guest-path", GUEST_IMPORT_USAGE)?
            .to_string(),
    })
}

fn parse_guest_export_args(args: &[String]) -> Result<GuestExportArgs, CliError> {
    let options = ParsedOptions::parse(
        args,
        GUEST_EXPORT_USAGE,
        &[
            "--machine-json",
            "--machine",
            "--control-socket",
            "--guest-path",
            "--host-path",
        ],
    )?;
    Ok(GuestExportArgs {
        machine: parse_guest_machine(&options, GUEST_EXPORT_USAGE)?,
        guest_path: options
            .required("--guest-path", GUEST_EXPORT_USAGE)?
            .to_string(),
        host_path: PathBuf::from(options.required("--host-path", GUEST_EXPORT_USAGE)?),
    })
}

struct ParsedOptions<'a> {
    values: HashMap<&'a str, Vec<&'a str>>,
}

impl<'a> ParsedOptions<'a> {
    fn parse(
        args: &'a [String],
        usage: &str,
        value_flags: &[&str],
    ) -> Result<ParsedOptions<'a>, CliError> {
        let mut values = HashMap::new();
        let mut index = 0;
        while index < args.len() {
            let flag = args[index].as_str();
            if !value_flags.contains(&flag) {
                return Err(CliError::Usage(format!("unknown flag {flag}; {usage}")));
            }
            index += 1;
            let value = args
                .get(index)
                .ok_or_else(|| CliError::Usage(format!("missing value for {flag}; {usage}")))?;
            values
                .entry(flag)
                .or_insert_with(Vec::new)
                .push(value.as_str());
            index += 1;
        }
        Ok(ParsedOptions { values })
    }

    fn one(&self, flag: &str, usage: &str) -> Result<Option<&'a str>, CliError> {
        match self.values.get(flag).map(Vec::as_slice) {
            None => Ok(None),
            Some([value]) => Ok(Some(*value)),
            Some(_) => Err(CliError::Usage(format!("duplicate {flag}; {usage}"))),
        }
    }

    fn required(&self, flag: &str, usage: &str) -> Result<&'a str, CliError> {
        self.one(flag, usage)?
            .ok_or_else(|| CliError::Usage(format!("missing {flag}; {usage}")))
    }

    fn repeated(&self, flag: &str) -> Vec<&'a str> {
        self.values.get(flag).cloned().unwrap_or_default()
    }
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

fn parse_uia_view(options: &ParsedOptions<'_>, usage: &str) -> Result<UiaView, CliError> {
    match options.one("--view", usage)? {
        None => Ok(UiaView::Control),
        Some(value) => UiaView::parse(value).ok_or_else(|| {
            CliError::Usage(format!("view must be control or raw, got {value}; {usage}"))
        }),
    }
}

fn parse_guest_machine(
    options: &ParsedOptions<'_>,
    usage: &str,
) -> Result<GuestMachineArgs, CliError> {
    let control_socket = options.one("--control-socket", usage)?.map(PathBuf::from);
    let machine_json = options.one("--machine-json", usage)?.map(str::to_string);
    let machine_name = options.one("--machine", usage)?.map(str::to_string);
    validate_machine_reference(&machine_json, &machine_name, usage)?;
    if machine_name.is_some() && control_socket.is_none() {
        return Err(CliError::Usage(format!(
            "--control-socket is required with --machine; {usage}"
        )));
    }
    Ok(GuestMachineArgs {
        control_socket,
        machine_json,
        machine_name,
    })
}

fn validate_machine_reference(
    machine_json: &Option<String>,
    machine_name: &Option<String>,
    usage: &str,
) -> Result<(), CliError> {
    match (machine_json, machine_name) {
        (Some(_), None) | (None, Some(_)) => Ok(()),
        (None, None) => Err(CliError::Usage(format!(
            "one of --machine-json or --machine is required; {usage}"
        ))),
        (Some(_), Some(_)) => Err(CliError::Usage(format!(
            "--machine-json and --machine are mutually exclusive; {usage}"
        ))),
    }
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

    fn guest_json_target() -> GuestMachineArgs {
        GuestMachineArgs {
            control_socket: None,
            machine_json: Some("{}".to_string()),
            machine_name: None,
        }
    }

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
                machine_json: Some("{}".to_string()),
                machine_name: None,
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
                machine_json: Some("{}".to_string()),
                machine_name: None,
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

    #[test]
    fn guest_ui_tree_defaults_and_bounds_parse() {
        let minimal = vec![
            "guest".to_string(),
            "ui-tree".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
        ];
        assert_eq!(
            parse_args(&minimal).unwrap(),
            Command::Guest(GuestCommand::UiTree(GuestUiTreeArgs {
                machine: guest_json_target(),
                max_depth: 8,
                max_nodes: 2_000,
                view: UiaView::Control,
            }))
        );

        let bounded = vec![
            "guest".to_string(),
            "ui-tree".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
            "--max-depth".to_string(),
            "4".to_string(),
            "--max-nodes".to_string(),
            "50".to_string(),
            "--view".to_string(),
            "raw".to_string(),
        ];
        let Command::Guest(GuestCommand::UiTree(tree)) = parse_args(&bounded).unwrap() else {
            panic!("expected ui-tree command");
        };
        assert_eq!(tree.max_depth, 4);
        assert_eq!(tree.max_nodes, 50);
        assert_eq!(tree.view, UiaView::Raw);

        let invalid = parse_args(&[
            "guest".to_string(),
            "invoke".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
            "--node".to_string(),
            "0".to_string(),
            "--view".to_string(),
            "content".to_string(),
        ]);
        assert!(matches!(invalid, Err(CliError::Usage(_))));
    }

    #[test]
    fn guest_run_preserves_repeated_args_in_order() {
        let args = vec![
            "guest".to_string(),
            "run".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
            "--command".to_string(),
            r"C:\Windows\System32\cmd.exe".to_string(),
            "--arg".to_string(),
            "/c".to_string(),
            "--arg".to_string(),
            "echo ok".to_string(),
            "--timeout-ms".to_string(),
            "5000".to_string(),
        ];
        assert_eq!(
            parse_args(&args).unwrap(),
            Command::Guest(GuestCommand::Run(GuestRunArgs {
                machine: guest_json_target(),
                command: r"C:\Windows\System32\cmd.exe".to_string(),
                args: vec!["/c".to_string(), "echo ok".to_string()],
                timeout_ms: 5_000,
            }))
        );
    }

    #[test]
    fn guest_machine_name_requires_control_socket_and_is_mutually_exclusive() {
        let parsed = parse_args(&[
            "guest".to_string(),
            "invoke".to_string(),
            "--machine".to_string(),
            "builder".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
            "--node".to_string(),
            "0".to_string(),
        ])
        .unwrap();
        let Command::Guest(GuestCommand::Invoke(invoke)) = parsed else {
            panic!("expected invoke command");
        };
        assert_eq!(invoke.machine.machine_name.as_deref(), Some("builder"));
        assert_eq!(
            invoke.machine.control_socket,
            Some(PathBuf::from("/tmp/control.sock"))
        );
        assert_eq!(invoke.machine.machine_json, None);

        let missing_socket = parse_args(&[
            "guest".to_string(),
            "invoke".to_string(),
            "--machine".to_string(),
            "builder".to_string(),
            "--node".to_string(),
            "0".to_string(),
        ]);
        assert!(missing_socket.is_err());
        let both = parse_args(&[
            "guest".to_string(),
            "invoke".to_string(),
            "--machine".to_string(),
            "builder".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
            "--node".to_string(),
            "0".to_string(),
        ]);
        assert!(both.is_err());
    }

    #[test]
    fn guest_machine_name_is_accepted_by_all_commands() {
        let target = [
            "--machine".to_string(),
            "builder".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
        ];
        let cases = [
            vec!["guest".to_string(), "ui-tree".to_string()],
            vec![
                "guest".to_string(),
                "invoke".to_string(),
                "--node".to_string(),
                "0".to_string(),
            ],
            vec![
                "guest".to_string(),
                "set-text".to_string(),
                "--node".to_string(),
                "0".to_string(),
                "--text".to_string(),
                "hello".to_string(),
            ],
            vec![
                "guest".to_string(),
                "run".to_string(),
                "--command".to_string(),
                r"C:\Windows\System32\cmd.exe".to_string(),
            ],
            vec![
                "guest".to_string(),
                "import".to_string(),
                "--host-path".to_string(),
                "/tmp/in.txt".to_string(),
                "--guest-path".to_string(),
                r"C:\Temp\in.txt".to_string(),
            ],
            vec![
                "guest".to_string(),
                "export".to_string(),
                "--guest-path".to_string(),
                r"C:\Temp\out.txt".to_string(),
                "--host-path".to_string(),
                "/tmp/out.txt".to_string(),
            ],
        ];
        for mut args in cases {
            args.extend(target.iter().cloned());
            let parsed = parse_args(&args).unwrap();
            let Command::Guest(command) = parsed else {
                panic!("expected guest command");
            };
            assert_eq!(command.machine().machine_name.as_deref(), Some("builder"));
        }
    }

    #[test]
    fn guest_file_transfer_commands_parse_paths() {
        let import = parse_args(&[
            "guest".to_string(),
            "import".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
            "--host-path".to_string(),
            "/tmp/in.txt".to_string(),
            "--guest-path".to_string(),
            r"C:\Temp\in.txt".to_string(),
        ])
        .unwrap();
        assert!(matches!(import, Command::Guest(GuestCommand::Import(_))));

        let export = parse_args(&[
            "guest".to_string(),
            "export".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
            "--guest-path".to_string(),
            r"C:\Temp\out.txt".to_string(),
            "--host-path".to_string(),
            "/tmp/out.txt".to_string(),
        ])
        .unwrap();
        assert!(matches!(export, Command::Guest(GuestCommand::Export(_))));
    }

    #[test]
    fn guest_node_commands_parse_paths_and_text() {
        let invoke = parse_args(&[
            "guest".to_string(),
            "invoke".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
            "--node".to_string(),
            "0/1/2".to_string(),
        ])
        .unwrap();
        assert!(matches!(invoke, Command::Guest(GuestCommand::Invoke(_))));

        let set_text = parse_args(&[
            "guest".to_string(),
            "set-text".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
            "--node".to_string(),
            "0/1/2".to_string(),
            "--text".to_string(),
            "hello".to_string(),
        ])
        .unwrap();
        assert!(matches!(set_text, Command::Guest(GuestCommand::SetText(_))));
    }

    #[test]
    fn guest_parser_rejects_duplicate_single_value_and_bad_timeout() {
        let duplicate = parse_args(&[
            "guest".to_string(),
            "invoke".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
            "--node".to_string(),
            "0".to_string(),
        ]);
        assert!(matches!(duplicate, Err(CliError::Usage(_))));

        let timeout = parse_args(&[
            "guest".to_string(),
            "run".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
            "--command".to_string(),
            r"C:\Windows\System32\cmd.exe".to_string(),
            "--timeout-ms".to_string(),
            "600001".to_string(),
        ]);
        assert!(matches!(timeout, Err(CliError::Usage(_))));
    }
    #[test]
    fn machine_clone_destroy_and_evidence_commands_parse() {
        let machine = vec![
            "machine".to_string(),
            "clone".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
            "--name".to_string(),
            "copy-1".to_string(),
        ];
        assert_eq!(
            parse_args(&machine).unwrap(),
            Command::Machine(MachineCommand::Clone(CloneArgs {
                control_socket: PathBuf::from("/tmp/control.sock"),
                machine_json: Some("{}".to_string()),
                machine_name: None,
                name: "copy-1".to_string(),
            }))
        );
        let destroy = parse_args(&[
            "machine".to_string(),
            "destroy".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
            "--machine-json".to_string(),
            "{}".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            destroy,
            Command::Machine(MachineCommand::Destroy(_))
        ));

        let tail = parse_args(&[
            "evidence".to_string(),
            "tail".to_string(),
            "--evidence-dir".to_string(),
            "/tmp/evidence".to_string(),
        ])
        .unwrap();
        assert_eq!(
            tail,
            Command::Evidence(EvidenceCommand::Tail(EvidenceTailArgs {
                evidence_dir: PathBuf::from("/tmp/evidence"),
                limit: 20,
            }))
        );
        let export = parse_args(&[
            "evidence".to_string(),
            "export".to_string(),
            "--evidence-dir".to_string(),
            "/tmp/evidence".to_string(),
            "--output".to_string(),
            "/tmp/events.jsonl".to_string(),
        ])
        .unwrap();
        assert!(matches!(
            export,
            Command::Evidence(EvidenceCommand::Export(_))
        ));
    }

    #[test]
    fn evidence_tail_limit_is_bounded() {
        for limit in ["0", "1001", "x"] {
            let result = parse_args(&[
                "evidence".to_string(),
                "tail".to_string(),
                "--evidence-dir".to_string(),
                "/tmp/evidence".to_string(),
                "--limit".to_string(),
                limit.to_string(),
            ]);
            assert!(matches!(result, Err(CliError::Usage(_))));
        }
    }
    #[test]
    fn parses_machine_list_and_machine_name_reference() {
        assert_eq!(
            parse_args(&[
                "machine".to_string(),
                "list".to_string(),
                "--control-socket".to_string(),
                "/tmp/control.sock".to_string(),
            ])
            .unwrap(),
            Command::Machine(MachineCommand::List(crate::args::ListArgs {
                control_socket: PathBuf::from("/tmp/control.sock"),
            }))
        );
        let parsed = parse_args(&[
            "machine".to_string(),
            "status".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
            "--machine".to_string(),
            "builder".to_string(),
        ])
        .unwrap();
        match parsed {
            Command::Machine(MachineCommand::Status(args)) => {
                assert_eq!(args.machine_name.as_deref(), Some("builder"));
                assert_eq!(args.machine_json, None);
            }
            other => panic!("unexpected parse result: {other:?}"),
        }
    }

    #[test]
    fn machine_reference_requires_exactly_one_form() {
        let base = [
            "machine".to_string(),
            "status".to_string(),
            "--control-socket".to_string(),
            "/tmp/control.sock".to_string(),
        ];
        assert!(parse_args(&base).is_err());
        let mut both = base.to_vec();
        both.extend([
            "--machine-json".to_string(),
            "{}".to_string(),
            "--machine".to_string(),
            "builder".to_string(),
        ]);
        assert!(parse_args(&both).is_err());
    }
}
