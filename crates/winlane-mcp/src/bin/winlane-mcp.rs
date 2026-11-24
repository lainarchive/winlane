use std::path::PathBuf;
use std::process::ExitCode;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let mut socket = None;
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--control-socket" => socket = args.next(),
            "--help" | "-h" => {
                eprintln!("usage: winlane-mcp --control-socket PATH");
                return ExitCode::SUCCESS;
            }
            _ => {
                eprintln!("unknown argument {arg}");
                return ExitCode::from(2);
            }
        }
    }
    let Some(socket) = socket else {
        eprintln!("usage: winlane-mcp --control-socket PATH");
        return ExitCode::from(2);
    };
    let runtime = match tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("failed to start runtime: {error}");
            return ExitCode::from(1);
        }
    };
    match runtime.block_on(winlane_mcp::server::serve_stdio(PathBuf::from(socket))) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("mcp server failed: {error}");
            ExitCode::from(1)
        }
    }
}
