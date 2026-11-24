//! Narrow command line entrypoint for machine control and terminal viewing.

pub mod args;
pub mod live;
pub mod machine;

use std::io::Write as _;

use thiserror::Error;

pub use args::{
    Command, CreateArgs, DaemonArgs, MachineArgs, MachineCommand, McpArgs, USAGE, VIEW_USAGE,
    ViewArgs, parse_args,
};
pub use live::{LiveViewConfig, capture_frame, run_live_view};
pub use machine::{run_daemon, run_machine_command, run_mcp};

pub type CliResult<T> = Result<T, CliError>;

#[derive(Debug, Error)]
pub enum CliError {
    #[error("{0}")]
    Usage(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Contract(#[from] winlane_contracts::ContractError),
    #[error(transparent)]
    Core(#[from] winlane_core::CoreError),
    #[error(transparent)]
    Ipc(#[from] winlane_ipc::IpcError),
    #[error(transparent)]
    Mcp(#[from] winlane_mcp::McpError),
    #[error(transparent)]
    View(#[from] winlane_view::ViewError),
}

pub async fn run(args: Vec<String>) -> CliResult<()> {
    match parse_args(&args)? {
        Command::Help => {
            let stdout = std::io::stdout();
            let mut output = stdout.lock();
            writeln!(output, "{USAGE}")?;
            Ok(())
        }
        Command::View(view) => run_live_view(LiveViewConfig::from_args(view)?).await,
        Command::Daemon(daemon) => run_daemon(daemon).await,
        Command::Machine(machine) => {
            let stdout = std::io::stdout();
            let mut output = stdout.lock();
            run_machine_command(&machine, &mut output).await
        }
        Command::Mcp(mcp) => run_mcp(mcp).await,
    }
}

pub fn implementation_status() -> &'static str {
    "machine-lifecycle"
}
