use std::process::ExitCode;

#[tokio::main]
async fn main() -> ExitCode {
    match winlane_cli::run(std::env::args().skip(1).collect()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("winlane: {error}");
            ExitCode::from(2)
        }
    }
}
