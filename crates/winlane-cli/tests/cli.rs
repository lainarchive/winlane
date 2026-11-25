use std::process::Command;

#[test]
fn version_commands_print_only_the_package_version() {
    for alias in ["--version", "-V", "version"] {
        let output = Command::new(env!("CARGO_BIN_EXE_winlane"))
            .arg(alias)
            .output()
            .unwrap();

        assert!(output.status.success(), "{alias}");
        assert!(output.stderr.is_empty(), "{alias}");
        assert_eq!(
            output.stdout,
            concat!("winlane ", env!("CARGO_PKG_VERSION"), "\n").as_bytes(),
            "{alias}"
        );
    }
}

#[test]
fn help_lists_version_and_preserves_command_usage() {
    let expected = format!(
        "winlane — Run and control disposable Windows machines.\n\n\
         Options:\n  -h, --help              Print help\n  -V, --version, version  Print version\n\n\
         {}\n",
        winlane_cli::USAGE
    );
    for args in [vec![], vec!["--help"], vec!["-h"], vec!["doctor", "--help"]] {
        let output = Command::new(env!("CARGO_BIN_EXE_winlane"))
            .args(&args)
            .output()
            .unwrap();

        assert!(output.status.success(), "{args:?}");
        assert!(output.stderr.is_empty(), "{args:?}");
        assert_eq!(output.stdout, expected.as_bytes(), "{args:?}");
    }
}
