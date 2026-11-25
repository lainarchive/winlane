use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use winlane_contracts::{BootMode, CpuArch, MachineSpec, NetworkMode};
use winlane_provision::{ProvisionOptions, provision, write_human_log};

const PROVISION_USAGE: &str = "usage: winlane-provision provision --name NAME --iso WINDOWS_ISO --virtio-iso VIRTIO_ISO --artifact-root DIR [--memory-mb 8192] [--cpus 4] [--disk-gb 80] [--network isolated|nat] [--timeout-minutes 90] [--image-name NAME] [--product-key KEY] [--windows-iso-sha256 SHA256] [--virtio-iso-sha256 SHA256] [--prepare-only] [--json]";

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            eprintln!("{PROVISION_USAGE}");
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or_else(|| "missing command".to_string())?;
    match command.as_str() {
        "--help" | "-h" => {
            println!(
                "winlane-provision — Provision a Windows golden image.\n\n\
                 Options:\n  -h, --help              Print help\n  -V, --version, version  Print version\n\n\
                 {PROVISION_USAGE}"
            );
            return Ok(ExitCode::SUCCESS);
        }
        "--version" | "-V" | "version" => {
            if args.next().is_some() {
                eprintln!("error: version does not take arguments");
                return Ok(ExitCode::from(2));
            }
            println!("winlane-provision {}", env!("CARGO_PKG_VERSION"));
            return Ok(ExitCode::SUCCESS);
        }
        "provision" => {}
        _ => return Err(format!("unknown command {command}")),
    }

    let mut name = None;
    let mut iso = None;
    let mut virtio_iso = None;
    let mut artifact_root = None;
    let mut memory_mb = 8192u32;
    let mut cpus = 4u16;
    let mut disk_gb = 80u32;
    let mut network = NetworkMode::Isolated;
    let mut timeout_minutes = 90u64;
    let mut prepare_only = false;
    let mut json = false;
    let mut image_name = Some("Windows 11 Enterprise Evaluation".to_string());
    let mut product_key = "NPPR9-FWDCX-D2C8J-H872K-2YT43".to_string();
    let mut windows_iso_sha256 = None;
    let mut virtio_iso_sha256 = None;

    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--name" => name = args.next(),
            "--iso" => iso = args.next(),
            "--virtio-iso" => virtio_iso = args.next(),
            "--artifact-root" => artifact_root = args.next(),
            "--memory-mb" => {
                memory_mb = parse_value(args.next(), "--memory-mb")?;
            }
            "--cpus" => {
                cpus = parse_value(args.next(), "--cpus")?;
            }
            "--disk-gb" => {
                disk_gb = parse_value(args.next(), "--disk-gb")?;
            }
            "--network" => {
                let value = args
                    .next()
                    .ok_or_else(|| "--network needs a value".to_string())?;
                network = match value.as_str() {
                    "isolated" => NetworkMode::Isolated,
                    "nat" => NetworkMode::Nat,
                    _ => return Err(format!("unknown network mode {value}")),
                };
            }
            "--timeout-minutes" => {
                timeout_minutes = parse_value(args.next(), "--timeout-minutes")?;
            }
            "--image-name" => image_name = args.next(),
            "--product-key" => {
                product_key = args
                    .next()
                    .ok_or_else(|| "--product-key needs a value".to_string())?;
            }
            "--windows-iso-sha256" => {
                windows_iso_sha256 = Some(
                    args.next()
                        .ok_or_else(|| "--windows-iso-sha256 needs a value".to_string())?,
                )
            }
            "--virtio-iso-sha256" => {
                virtio_iso_sha256 = Some(
                    args.next()
                        .ok_or_else(|| "--virtio-iso-sha256 needs a value".to_string())?,
                )
            }
            "--prepare-only" => prepare_only = true,
            "--json" => json = true,
            "--help" | "-h" => {
                eprintln!("{PROVISION_USAGE}");
                return Ok(ExitCode::SUCCESS);
            }
            _ => return Err(format!("unknown argument {arg}")),
        }
    }

    let name = name.ok_or_else(|| "--name is required".to_string())?;
    let iso = iso.ok_or_else(|| "--iso is required".to_string())?;
    let virtio_iso = virtio_iso.ok_or_else(|| "--virtio-iso is required".to_string())?;
    let artifact_root = artifact_root.ok_or_else(|| "--artifact-root is required".to_string())?;

    let spec = MachineSpec {
        name,
        arch: CpuArch::X86_64,
        memory_mb,
        cpus,
        disk_gb,
        iso_path: Some(iso),
        base_image_path: None,
        guest_agent: true,
        network,
        tpm: true,
        boot: BootMode::Uefi,
    };
    let mut options = ProvisionOptions::new(
        spec,
        PathBuf::from(virtio_iso),
        PathBuf::from(artifact_root),
    );
    options.install_timeout = Duration::from_secs(timeout_minutes * 60);
    options.prepare_only = prepare_only;
    options.image_name = image_name;
    options.product_key = product_key;
    options.windows_iso_sha256 = windows_iso_sha256;
    options.virtio_iso_sha256 = virtio_iso_sha256;

    let report = provision(&options).map_err(|error| error.to_string())?;
    if json {
        let encoded = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;
        println!("{encoded}");
    } else {
        write_human_log(&report, std::io::stdout()).map_err(|error| error.to_string())?;
    }
    if report.success {
        Ok(ExitCode::SUCCESS)
    } else {
        Ok(ExitCode::from(1))
    }
}

fn parse_value<T>(value: Option<String>, flag: &str) -> Result<T, String>
where
    T: std::str::FromStr,
    T::Err: std::fmt::Display,
{
    value
        .ok_or_else(|| format!("{flag} needs a value"))?
        .parse::<T>()
        .map_err(|error| format!("invalid value for {flag}: {error}"))
}
