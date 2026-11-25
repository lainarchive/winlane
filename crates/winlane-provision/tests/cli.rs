use std::process::Command;

const PROVISION_USAGE: &str = "usage: winlane-provision provision --name NAME --iso WINDOWS_ISO --virtio-iso VIRTIO_ISO --artifact-root DIR [--memory-mb 8192] [--cpus 4] [--disk-gb 80] [--network isolated|nat] [--timeout-minutes 90] [--image-name NAME] [--product-key KEY] [--windows-iso-sha256 SHA256] [--virtio-iso-sha256 SHA256] [--prepare-only] [--json]\n";

fn assert_cli(args: &[&str], code: i32, stdout: &str, stderr: &str) {
    let output = Command::new(env!("CARGO_BIN_EXE_winlane-provision"))
        .args(args)
        .output()
        .unwrap();

    assert_eq!(output.status.code(), Some(code), "{args:?}");
    assert_eq!(output.stdout, stdout.as_bytes(), "{args:?}");
    assert_eq!(output.stderr, stderr.as_bytes(), "{args:?}");
}

#[test]
fn version_aliases_print_the_package_version() {
    let expected = concat!("winlane-provision ", env!("CARGO_PKG_VERSION"), "\n");
    for alias in ["--version", "-V", "version"] {
        assert_cli(&[alias], 0, expected, "");
        assert_cli(
            &[alias, "--help"],
            2,
            "",
            "error: version does not take arguments\n",
        );
    }
}

#[test]
fn global_help_works_without_or_before_provision() {
    let expected = format!(
        "winlane-provision — Provision a Windows golden image.\n\n\
         Options:\n  -h, --help              Print help\n  -V, --version, version  Print version\n\n\
         {PROVISION_USAGE}"
    );
    for flag in ["--help", "-h"] {
        assert_cli(&[flag], 0, &expected, "");
        assert_cli(&[flag, "provision"], 0, &expected, "");
    }
}

#[test]
fn provision_help_and_missing_command_keep_existing_output() {
    for flag in ["--help", "-h"] {
        assert_cli(&["provision", flag], 0, "", PROVISION_USAGE);
    }
    assert_cli(
        &[],
        2,
        "",
        &format!("error: missing command\n{PROVISION_USAGE}"),
    );
}

#[test]
fn iso_hash_flags_require_values() {
    for flag in ["--windows-iso-sha256", "--virtio-iso-sha256"] {
        assert_cli(
            &["provision", flag],
            2,
            "",
            &format!("error: {flag} needs a value\n{PROVISION_USAGE}"),
        );
    }
}
