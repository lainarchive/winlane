//! Read-only host preflight diagnostics for first-use onboarding.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command as ProcessCommand;

use serde::Serialize;

use crate::args::DoctorArgs;
use crate::{CliError, CliResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorCheck {
    pub name: String,
    pub status: String,
    pub detail: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DoctorReport {
    pub ok: bool,
    pub root: String,
    pub hard_failures: usize,
    pub warnings: usize,
    pub checks: Vec<DoctorCheck>,
}

pub fn run_doctor(args: &DoctorArgs, output: &mut impl Write) -> CliResult<()> {
    run_doctor_with_environment(
        args,
        output,
        std::env::var_os("PATH"),
        Path::new("/dev/kvm"),
        &default_ovmf_pairs(),
    )
}

fn run_doctor_with_environment(
    args: &DoctorArgs,
    output: &mut impl Write,
    path_value: Option<std::ffi::OsString>,
    kvm_path: &Path,
    ovmf_pairs: &[(PathBuf, PathBuf)],
) -> CliResult<()> {
    let root = args.root.clone().unwrap_or_else(default_machine_root);
    let report = collect_report(&root, path_value, kvm_path, ovmf_pairs);
    serde_json::to_writer(&mut *output, &report)?;
    writeln!(output)?;
    if report.hard_failures == 0 {
        Ok(())
    } else {
        Err(CliError::Operation(
            "host preflight has hard failures".to_string(),
        ))
    }
}

fn default_machine_root() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".winlane")
        .join("machines")
}

fn default_ovmf_pairs() -> Vec<(PathBuf, PathBuf)> {
    vec![
        (
            PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.fd"),
            PathBuf::from("/usr/share/OVMF/OVMF_VARS_4M.fd"),
        ),
        (
            PathBuf::from("/usr/share/OVMF/OVMF_CODE.fd"),
            PathBuf::from("/usr/share/OVMF/OVMF_VARS.fd"),
        ),
    ]
}

fn pass(name: &str, detail: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        status: "pass".to_string(),
        detail: detail.into(),
        remediation: None,
    }
}

fn fail(name: &str, detail: impl Into<String>, remediation: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        status: "fail".to_string(),
        detail: detail.into(),
        remediation: Some(remediation.into()),
    }
}

fn warn(name: &str, detail: impl Into<String>, remediation: impl Into<String>) -> DoctorCheck {
    DoctorCheck {
        name: name.to_string(),
        status: "warn".to_string(),
        detail: detail.into(),
        remediation: Some(remediation.into()),
    }
}

fn collect_report(
    root: &Path,
    path_value: Option<std::ffi::OsString>,
    kvm_path: &Path,
    ovmf_pairs: &[(PathBuf, PathBuf)],
) -> DoctorReport {
    let checks = vec![
        platform_check(),
        kvm_check(kvm_path),
        executable_check(
            "qemu-system-x86_64",
            path_value.as_deref(),
            "install QEMU system emulation, for example: sudo apt install qemu-system-x86",
        ),
        executable_check(
            "qemu-img",
            path_value.as_deref(),
            "install QEMU image tools, for example: sudo apt install qemu-utils",
        ),
        executable_check(
            "swtpm",
            path_value.as_deref(),
            "install the software TPM emulator, for example: sudo apt install swtpm swtpm-tools",
        ),
        ovmf_check(ovmf_pairs),
        disk_check(root),
    ];
    let hard_failures = checks.iter().filter(|check| check.status == "fail").count();
    let warnings = checks.iter().filter(|check| check.status == "warn").count();
    DoctorReport {
        ok: hard_failures == 0,
        root: root.display().to_string(),
        hard_failures,
        warnings,
        checks,
    }
}

fn platform_check() -> DoctorCheck {
    let os = std::env::consts::OS;
    let arch = std::env::consts::ARCH;
    if os == "linux" && arch == "x86_64" {
        pass("platform", format!("{os} {arch}"))
    } else {
        fail(
            "platform",
            format!("{os} {arch}"),
            "use a Linux x86_64 host with KVM support",
        )
    }
}

fn kvm_check(path: &Path) -> DoctorCheck {
    match std::fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(path)
    {
        Ok(_) => pass(
            "kvm",
            format!("{} is readable and writable", path.display()),
        ),
        Err(error) => fail(
            "kvm",
            format!("cannot access {}: {error}", path.display()),
            "enable hardware virtualization and grant this user access to /dev/kvm, commonly through the kvm group",
        ),
    }
}

fn executable_check(
    name: &str,
    path_value: Option<&std::ffi::OsStr>,
    remediation: &str,
) -> DoctorCheck {
    let Some(binary) = find_on_path(name, path_value) else {
        return fail(
            name,
            "not found in PATH",
            format!("{remediation}; then ensure {name} is in PATH"),
        );
    };
    match ProcessCommand::new(&binary).arg("--version").output() {
        Ok(output) if output.status.success() => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let detail = stdout
                .lines()
                .chain(stderr.lines())
                .find(|line| !line.trim().is_empty())
                .unwrap_or("available")
                .trim()
                .to_string();
            pass(name, format!("{}: {detail}", binary.display()))
        }
        Ok(output) => fail(
            name,
            format!(
                "{} --version exited with {}",
                binary.display(),
                output.status
            ),
            format!("repair or reinstall {name}: {remediation}"),
        ),
        Err(error) => fail(
            name,
            format!("cannot execute {}: {error}", binary.display()),
            format!("repair or reinstall {name}: {remediation}"),
        ),
    }
}

fn find_on_path(name: &str, path_value: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let path_value = path_value?;
    for directory in std::env::split_paths(path_value) {
        let candidate = directory.join(name);
        if candidate.is_file() {
            return Some(candidate);
        }
    }
    None
}

fn ovmf_check(pairs: &[(PathBuf, PathBuf)]) -> DoctorCheck {
    for (code, vars) in pairs {
        if code.is_file() && vars.is_file() {
            return pass(
                "ovmf",
                format!("found {} and {}", code.display(), vars.display()),
            );
        }
    }
    fail(
        "ovmf",
        "no complete OVMF code/vars firmware pair found",
        "install OVMF UEFI firmware, for example: sudo apt install ovmf",
    )
}

fn disk_check(root: &Path) -> DoctorCheck {
    let mut probe = root;
    while !probe.exists() {
        match probe.parent() {
            Some(parent) => probe = parent,
            None => {
                return warn(
                    "disk_space",
                    format!("no existing ancestor for {}", root.display()),
                    "choose an existing machine root filesystem",
                );
            }
        }
    }
    let output = ProcessCommand::new("df")
        .arg("-P")
        .arg("-k")
        .arg(probe)
        .output();
    let available_bytes = output.ok().and_then(|output| {
        if !output.status.success() {
            return None;
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.lines().nth(1).and_then(|line| {
            line.split_whitespace()
                .nth(3)
                .and_then(|value| value.parse::<u64>().ok())
                .map(|kibibytes| kibibytes.saturating_mul(1024))
        })
    });
    match available_bytes {
        Some(bytes) if bytes >= 20 * 1024 * 1024 * 1024 => pass(
            "disk_space",
            format!(
                "{} bytes free on filesystem containing {}",
                bytes,
                probe.display()
            ),
        ),
        Some(bytes) => warn(
            "disk_space",
            format!(
                "only {bytes} bytes free on filesystem containing {}",
                probe.display()
            ),
            "free at least 20 GiB for machine disks and checkpoints, or choose another machine root",
        ),
        None => warn(
            "disk_space",
            format!("could not measure free space for {}", probe.display()),
            "verify that df is available and the machine root filesystem is accessible",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;

    use crate::Command;
    use crate::args::parse_args;

    fn fake_bin(root: &Path, name: &str) {
        let path = root.join(name);
        std::fs::write(&path, "#!/bin/sh\necho fake version 1\n").expect("write fake binary");
        let mut permissions = std::fs::metadata(&path).expect("metadata").permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).expect("chmod fake binary");
    }

    #[test]
    fn doctor_parses_optional_root() {
        let parsed = parse_args(&["doctor".to_string()]).expect("parse doctor");
        assert_eq!(parsed, Command::Doctor(DoctorArgs { root: None }));
        let parsed = parse_args(&[
            "doctor".to_string(),
            "--root".to_string(),
            "/tmp/machines".to_string(),
        ])
        .expect("parse doctor root");
        assert_eq!(
            parsed,
            Command::Doctor(DoctorArgs {
                root: Some(PathBuf::from("/tmp/machines"))
            })
        );
    }

    #[test]
    fn empty_path_reports_missing_required_tools_as_hard_failures() {
        let root = tempfile::tempdir().expect("temporary root");
        let report = collect_report(
            root.path(),
            Some(std::ffi::OsString::new()),
            Path::new("/dev/kvm"),
            &[],
        );
        assert!(!report.ok);
        assert!(report.hard_failures >= 4);
        for name in ["qemu-system-x86_64", "qemu-img", "swtpm", "ovmf"] {
            let check = report
                .checks
                .iter()
                .find(|check| check.name == name)
                .expect("check present");
            assert_eq!(check.status, "fail");
            assert!(check.remediation.is_some());
        }
    }

    #[test]
    fn fake_tools_and_firmware_can_satisfy_doctor_checks() {
        let root = tempfile::tempdir().expect("temporary root");
        let bin = root.path().join("bin");
        std::fs::create_dir(&bin).expect("create bin");
        for name in ["qemu-system-x86_64", "qemu-img", "swtpm"] {
            fake_bin(&bin, name);
        }
        let code = root.path().join("OVMF_CODE.fd");
        let vars = root.path().join("OVMF_VARS.fd");
        std::fs::write(&code, b"code").expect("write code");
        std::fs::write(&vars, b"vars").expect("write vars");
        let report = collect_report(
            root.path(),
            std::env::join_paths([&bin]).ok(),
            Path::new("/dev/kvm"),
            &[(code, vars)],
        );
        for name in ["qemu-system-x86_64", "qemu-img", "swtpm", "ovmf"] {
            assert_eq!(
                report
                    .checks
                    .iter()
                    .find(|check| check.name == name)
                    .expect("check present")
                    .status,
                "pass"
            );
        }
    }

    #[test]
    fn doctor_writes_json_before_reporting_hard_failure() {
        let root = tempfile::tempdir().expect("temporary root");
        let mut output = Vec::new();
        let result = run_doctor_with_environment(
            &DoctorArgs {
                root: Some(root.path().to_path_buf()),
            },
            &mut output,
            Some(std::ffi::OsString::new()),
            Path::new("/definitely/missing/kvm"),
            &[],
        );
        let value: serde_json::Value = serde_json::from_slice(&output).expect("doctor json");
        assert!(matches!(result, Err(CliError::Operation(_))));
        assert!(value["checks"].is_array());
        assert!(value["hard_failures"].as_u64().expect("failure count") >= 5);
        assert_eq!(
            value["root"].as_str(),
            Some(root.path().to_string_lossy().as_ref())
        );
    }
}
