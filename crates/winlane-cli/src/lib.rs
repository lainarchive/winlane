//! Narrow command line entrypoint for attaching a terminal viewer to a machine.

pub mod args;
pub mod live;

use thiserror::Error;

pub use args::{Command, VIEW_USAGE, ViewArgs, parse_args};
pub use live::{LiveViewConfig, capture_frame, run_live_view};

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
    Ipc(#[from] winlane_ipc::IpcError),
    #[error(transparent)]
    View(#[from] winlane_view::ViewError),
}

pub async fn run(args: Vec<String>) -> CliResult<()> {
    match parse_args(&args)? {
        Command::Help => {
            println!("{VIEW_USAGE}");
            Ok(())
        }
        Command::View(view) => run_live_view(LiveViewConfig::from_args(view)?).await,
    }
}

pub fn implementation_status() -> &'static str {
    "live-view"
}
