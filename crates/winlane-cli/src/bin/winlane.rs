use std::process::ExitCode;

use winlane_cli::CliError;
use winlane_guest::uia::UiaError;

#[tokio::main]
async fn main() -> ExitCode {
    match winlane_cli::run(std::env::args().skip(1).collect()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("winlane: {error}");
            ExitCode::from(match error {
                CliError::Usage(_)
                | CliError::Contract(_)
                | CliError::Uia(
                    UiaError::InvalidElementRef
                    | UiaError::InputTooLarge(_)
                    | UiaError::InvalidCommandPath
                    | UiaError::InvalidGuestPath
                    | UiaError::InvalidProcessArgument
                    | UiaError::InvalidTimeout,
                ) => 2,
                _ => 1,
            })
        }
    }
}
