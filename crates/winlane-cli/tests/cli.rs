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

#[test]
fn invalid_arguments_exit_two_without_stdout() {
    for (args, error) in [
        (
            &["version", "--json"][..],
            "winlane: usage: winlane --version | -V | version\n",
        ),
        (
            &[
                "machine",
                "status",
                "--control-socket",
                "unused.sock",
                "--machine-json",
                "{",
            ],
            "winlane: EOF while parsing an object at line 1 column 1\n",
        ),
        (
            &["guest", "ui-tree", "--machine-json", "{"],
            "winlane: EOF while parsing an object at line 1 column 1\n",
        ),
        (
            &[
                "machine",
                "create",
                "--control-socket",
                "unused.sock",
                "--name",
                "invalid!",
            ],
            "winlane: machine name may contain only ASCII letters, digits, '-' and '_'\n",
        ),
    ] {
        let output = Command::new(env!("CARGO_BIN_EXE_winlane"))
            .args(args)
            .output()
            .unwrap();

        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(output.stdout.is_empty(), "{args:?}");
        assert_eq!(output.stderr, error.as_bytes(), "{args:?}");
    }
}

#[test]
fn missing_control_socket_exits_one_without_stdout() {
    let root = tempfile::tempdir().unwrap();
    let socket = root.path().join("missing.sock");
    let connect_error = std::os::unix::net::UnixStream::connect(&socket).unwrap_err();
    let output = Command::new(env!("CARGO_BIN_EXE_winlane"))
        .args(["machine", "list", "--control-socket"])
        .arg(socket)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert!(output.stdout.is_empty());
    assert_eq!(
        output.stderr,
        format!("winlane: ipc io error: {connect_error}\n").as_bytes()
    );
}

#[test]
fn failed_doctor_prints_json_and_exits_one() {
    let root = tempfile::tempdir().unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_winlane"))
        .args(["doctor", "--root"])
        .arg(root.path())
        .env("PATH", root.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        output.stderr,
        b"winlane: host preflight has hard failures\n"
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert!(report["hard_failures"].as_u64().unwrap() >= 3);
}

#[test]
fn failed_evidence_verification_prints_json_and_exits_one() {
    let root = tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("events.jsonl"), "{\n").unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_winlane"))
        .args(["evidence", "verify", "--evidence-dir"])
        .arg(root.path())
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(1));
    assert_eq!(
        output.stderr,
        b"winlane: evidence verification failed with 1 failure(s)\n"
    );
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report["ok"], false);
    assert_eq!(report["failure_count"], 1);
    assert_eq!(report["failures"][0]["type"], "invalid_json");
}
