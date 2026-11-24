use std::path::PathBuf;
use std::process::ExitCode;
use std::time::Duration;
use winlane_contracts::{BootMode, CpuArch, MachineSpec, NetworkMode};
use winlane_provision::{ProvisionOptions, provision, write_human_log};

fn main() -> ExitCode {
    match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!("error: {error}");
            usage();
            ExitCode::from(2)
        }
    }
}

fn run() -> Result<ExitCode, String> {
    let mut args = std::env::args().skip(1);
    let command = args.next().ok_or_else(|| "missing command".to_string())?;
    if command != "provision" {
        return Err(format!("unknown command {command}"));
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
            "--prepare-only" => prepare_only = true,
            "--json" => json = true,
            "--help" | "-h" => {
                usage();
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

fn usage() {
    eprintln!(
        "usage: winlane-provision provision --name NAME --iso WINDOWS_ISO --virtio-iso VIRTIO_ISO --artifact-root DIR [--memory-mb 8192] [--cpus 4] [--disk-gb 80] [--network isolated|nat] [--timeout-minutes 90] [--image-name NAME] [--product-key KEY] [--prepare-only] [--json]"
    );
}
