use crate::answer::{firstboot_cmd, unattend_xml};
use crate::qemu::{GuestExecStatus, QemuProtocolError, QemuProtocolResult, QgaClient, QmpClient};
use serde::{Deserialize, Serialize};
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use winlane_contracts::{BootMode, ContractError, CpuArch, MachineSpec, NetworkMode};

#[derive(Debug, Error)]
pub enum ProvisionError {
    #[error("{0}")]
    Contract(#[from] ContractError),
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),
    #[error("{0}")]
    InvalidRequest(String),
    #[error("missing host requirement: {0}")]
    MissingRequirement(String),
    #[error("command {program} failed with {status}: {stderr}")]
    Command {
        program: String,
        status: String,
        stderr: String,
    },
    #[error("QEMU protocol error: {0}")]
    QemuProtocol(#[from] QemuProtocolError),
}

pub type ProvisionResult<T> = Result<T, ProvisionError>;

#[derive(Debug, Clone)]
pub struct ProvisionOptions {
    pub spec: MachineSpec,
    pub virtio_iso_path: PathBuf,
    pub artifact_root: PathBuf,
    pub install_timeout: Duration,
    pub prepare_only: bool,
    pub qemu_binary: PathBuf,
    pub qemu_img_binary: PathBuf,
    pub swtpm_binary: PathBuf,
    pub ovmf_code_path: Option<PathBuf>,
    pub ovmf_vars_path: Option<PathBuf>,
    pub image_name: Option<String>,
    pub product_key: String,
}

impl ProvisionOptions {
    pub fn new(spec: MachineSpec, virtio_iso_path: PathBuf, artifact_root: PathBuf) -> Self {
        Self {
            spec,
            virtio_iso_path,
            artifact_root,
            install_timeout: Duration::from_secs(90 * 60),
            prepare_only: false,
            qemu_binary: PathBuf::from("qemu-system-x86_64"),
            qemu_img_binary: PathBuf::from("qemu-img"),
            swtpm_binary: PathBuf::from("swtpm"),
            ovmf_code_path: None,
            ovmf_vars_path: None,
            image_name: Some("Windows 11 Enterprise Evaluation".to_string()),
            product_key: "NPPR9-FWDCX-D2C8J-H872K-2YT43".to_string(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ProvisionStage {
    Validating,
    Preflight,
    PreparingAnswerMedia,
    CreatingDisk,
    PreparingFirmware,
    StartingTpm,
    StartingQemu,
    InstallingWindows,
    WaitingForGuestAgent,
    CheckingDesktop,
    ShuttingDown,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProbeResult {
    pub name: String,
    pub ok: bool,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct StageTiming {
    pub stage: ProvisionStage,
    pub started_unix_ms: u64,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct ReadinessReport {
    pub guest_agent_ping: bool,
    pub guest_exec: bool,
    pub firstboot_sentinel: bool,
    pub explorer_running: bool,
    pub whoami: Option<String>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ShutdownReport {
    pub powerdown_sent: bool,
    pub forced_kill: bool,
    pub exit_status: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProvisionReport {
    pub schema_version: u8,
    pub machine_name: String,
    pub success: bool,
    pub stage: ProvisionStage,
    pub error: Option<String>,
    pub state_dir: PathBuf,
    pub disk_path: PathBuf,
    pub windows_iso_path: PathBuf,
    pub virtio_iso_path: PathBuf,
    pub unattended_iso_path: PathBuf,
    pub qemu_log_path: PathBuf,
    pub swtpm_log_path: PathBuf,
    pub report_path: PathBuf,
    pub final_screenshot_path: Option<PathBuf>,
    pub qemu_command: Vec<String>,
    pub probes: Vec<ProbeResult>,
    pub timings: Vec<StageTiming>,
    pub readiness: ReadinessReport,
    pub shutdown: Option<ShutdownReport>,
    pub started_unix_ms: u64,
    pub finished_unix_ms: u64,
    pub total_duration_ms: u64,
}

#[derive(Debug)]
struct PipelinePaths {
    state_dir: PathBuf,
    unattend_dir: PathBuf,
    unattended_iso: PathBuf,
    disk: PathBuf,
    ovmf_vars: PathBuf,
    swtpm_socket: PathBuf,
    swtpm_log: PathBuf,
    qmp_socket: PathBuf,
    qga_socket: PathBuf,
    qemu_log: PathBuf,
    screenshot: PathBuf,
    report: PathBuf,
}

#[derive(Debug)]
struct InstallOutcome {
    readiness: ReadinessReport,
    shutdown: Option<ShutdownReport>,
    timings: Vec<StageTiming>,
    qemu_command: Vec<String>,
    final_screenshot_path: Option<PathBuf>,
    error: Option<String>,
}

pub fn provision(options: &ProvisionOptions) -> ProvisionResult<ProvisionReport> {
    let started = Instant::now();
    let started_unix_ms = unix_ms();
    let paths = pipeline_paths(options);
    let mut timings = Vec::new();
    let validation_started = Instant::now();
    let validation = validate_request(options);
    push_timing(&mut timings, ProvisionStage::Validating, validation_started);

    let mut report = ProvisionReport {
        schema_version: 1,
        machine_name: options.spec.name.clone(),
        success: false,
        stage: ProvisionStage::Validating,
        error: None,
        state_dir: paths.state_dir.clone(),
        disk_path: paths.disk.clone(),
        windows_iso_path: options
            .spec
            .iso_path
            .as_ref()
            .map(PathBuf::from)
            .unwrap_or_default(),
        virtio_iso_path: options.virtio_iso_path.clone(),
        unattended_iso_path: paths.unattended_iso.clone(),
        qemu_log_path: paths.qemu_log.clone(),
        swtpm_log_path: paths.swtpm_log.clone(),
        report_path: paths.report.clone(),
        final_screenshot_path: None,
        qemu_command: Vec::new(),
        probes: Vec::new(),
        timings: timings.clone(),
        readiness: ReadinessReport::default(),
        shutdown: None,
        started_unix_ms,
        finished_unix_ms: started_unix_ms,
        total_duration_ms: 0,
    };

    if let Err(error) = validation {
        return finish_failed(report, started, ProvisionStage::Failed, error.to_string());
    }

    fs::create_dir_all(&paths.state_dir)?;
    fs::create_dir_all(&paths.unattend_dir)?;

    let preflight_started = Instant::now();
    report.stage = ProvisionStage::Preflight;
    let probes = preflight(options);
    push_timing(&mut timings, ProvisionStage::Preflight, preflight_started);
    report.probes = probes.clone();
    report.timings = timings.clone();
    if let Some(probe) = probes.iter().find(|probe| !probe.ok) {
        return finish_failed(
            report,
            started,
            ProvisionStage::Failed,
            format!("{}: {}", probe.name, probe.detail),
        );
    }

    let media_started = Instant::now();
    report.stage = ProvisionStage::PreparingAnswerMedia;
    if let Err(error) = prepare_answer_media(options, &paths) {
        return finish_failed(report, started, ProvisionStage::Failed, error.to_string());
    }
    push_timing(
        &mut timings,
        ProvisionStage::PreparingAnswerMedia,
        media_started,
    );
    report.timings = timings.clone();

    if options.prepare_only {
        report.success = true;
        report.stage = ProvisionStage::Completed;
        return finish_report(report, started);
    }

    let outcome = execute_install(options, &paths);
    timings.extend(outcome.timings);
    report.timings = timings;
    report.qemu_command = outcome.qemu_command;
    report.readiness = outcome.readiness;
    report.shutdown = outcome.shutdown;
    report.final_screenshot_path = outcome.final_screenshot_path;

    if let Some(error) = outcome.error {
        return finish_failed(report, started, ProvisionStage::Failed, error);
    }

    report.success = true;
    report.stage = ProvisionStage::Completed;
    finish_report(report, started)
}

fn pipeline_paths(options: &ProvisionOptions) -> PipelinePaths {
    let state_dir = options.artifact_root.join(&options.spec.name);
    PipelinePaths {
        unattend_dir: state_dir.join("unattend"),
        unattended_iso: state_dir.join("unattended.iso"),
        disk: state_dir.join("golden.qcow2"),
        ovmf_vars: state_dir.join("OVMF_VARS.fd"),
        swtpm_socket: state_dir.join("swtpm.sock"),
        swtpm_log: state_dir.join("swtpm.log"),
        qmp_socket: state_dir.join("qmp.sock"),
        qga_socket: state_dir.join("qga.sock"),
        qemu_log: state_dir.join("qemu.log"),
        screenshot: state_dir.join("screen-final.ppm"),
        report: state_dir.join("provisioning-report.json"),
        state_dir,
    }
}

fn validate_request(options: &ProvisionOptions) -> ProvisionResult<()> {
    options.spec.validate()?;
    if options.spec.arch != CpuArch::X86_64 {
        return Err(ProvisionError::InvalidRequest(
            "the first provisioning path supports only x86_64 guests".to_string(),
        ));
    }
    if options.spec.boot != BootMode::Uefi {
        return Err(ProvisionError::InvalidRequest(
            "Windows 11 provisioning requires UEFI boot".to_string(),
        ));
    }
    if !options.spec.tpm {
        return Err(ProvisionError::InvalidRequest(
            "Windows 11 provisioning requires TPM emulation".to_string(),
        ));
    }
    if options.spec.memory_mb < 4096 {
        return Err(ProvisionError::InvalidRequest(
            "Windows 11 provisioning requires at least 4096 MiB of memory".to_string(),
        ));
    }
    if options.spec.cpus < 2 {
        return Err(ProvisionError::InvalidRequest(
            "Windows 11 provisioning requires at least 2 CPUs".to_string(),
        ));
    }
    if options.spec.disk_gb < 64 {
        return Err(ProvisionError::InvalidRequest(
            "Windows 11 provisioning requires at least 64 GiB of disk".to_string(),
        ));
    }
    if options.spec.iso_path.is_none() {
        return Err(ProvisionError::InvalidRequest(
            "MachineSpec.iso_path must point to an official Windows ISO".to_string(),
        ));
    }
    if options.install_timeout < Duration::from_secs(60) {
        return Err(ProvisionError::InvalidRequest(
            "install_timeout must be at least 60 seconds".to_string(),
        ));
    }
    Ok(())
}

fn preflight(options: &ProvisionOptions) -> Vec<ProbeResult> {
    let (code, vars) = ovmf_paths(options);
    vec![
        binary_probe("qemu-system-x86_64", &options.qemu_binary, "--version"),
        binary_probe("qemu-img", &options.qemu_img_binary, "--version"),
        binary_probe("swtpm", &options.swtpm_binary, "--version"),
        iso_builder_probe(),
        kvm_probe(),
        file_probe(
            "windows iso",
            options
                .spec
                .iso_path
                .as_ref()
                .map(Path::new)
                .unwrap_or_else(|| Path::new("")),
            1_000_000_000,
        ),
        file_probe("virtio driver iso", &options.virtio_iso_path, 10_000_000),
        file_probe("OVMF code", &code, 100_000),
        file_probe("OVMF vars template", &vars, 100_000),
    ]
}

fn binary_probe(name: &str, binary: &Path, arg: &str) -> ProbeResult {
    match Command::new(binary).arg(arg).output() {
        Ok(output) if output.status.success() => {
            let first = String::from_utf8_lossy(&output.stdout)
                .lines()
                .next()
                .unwrap_or("available")
                .to_string();
            ProbeResult {
                name: name.to_string(),
                ok: true,
                detail: first,
            }
        }
        Ok(output) => ProbeResult {
            name: name.to_string(),
            ok: false,
            detail: String::from_utf8_lossy(&output.stderr).trim().to_string(),
        },
        Err(error) => ProbeResult {
            name: name.to_string(),
            ok: false,
            detail: error.to_string(),
        },
    }
}

fn iso_builder_probe() -> ProbeResult {
    if let Ok(output) = Command::new("xorriso").arg("--version").output()
        && output.status.success()
    {
        return ProbeResult {
            name: "iso builder".to_string(),
            ok: true,
            detail: "xorriso".to_string(),
        };
    }
    if let Ok(output) = Command::new("genisoimage").arg("--version").output()
        && output.status.success()
    {
        return ProbeResult {
            name: "iso builder".to_string(),
            ok: true,
            detail: "genisoimage".to_string(),
        };
    }
    ProbeResult {
        name: "iso builder".to_string(),
        ok: false,
        detail: "neither xorriso nor genisoimage is available".to_string(),
    }
}

fn kvm_probe() -> ProbeResult {
    match OpenOptions::new().read(true).write(true).open("/dev/kvm") {
        Ok(_) => ProbeResult {
            name: "KVM acceleration".to_string(),
            ok: true,
            detail: "/dev/kvm is readable and writable".to_string(),
        },
        Err(error) => ProbeResult {
            name: "KVM acceleration".to_string(),
            ok: false,
            detail: format!("/dev/kvm: {error}"),
        },
    }
}

fn file_probe(name: &str, path: &Path, minimum_bytes: u64) -> ProbeResult {
    match fs::metadata(path) {
        Ok(metadata) if metadata.is_file() && metadata.len() >= minimum_bytes => ProbeResult {
            name: name.to_string(),
            ok: true,
            detail: format!("{} bytes at {}", metadata.len(), path.display()),
        },
        Ok(metadata) => ProbeResult {
            name: name.to_string(),
            ok: false,
            detail: format!(
                "expected a file of at least {minimum_bytes} bytes at {}, got {} bytes",
                path.display(),
                metadata.len()
            ),
        },
        Err(error) => ProbeResult {
            name: name.to_string(),
            ok: false,
            detail: format!("{}: {error}", path.display()),
        },
    }
}

fn prepare_answer_media(options: &ProvisionOptions, paths: &PipelinePaths) -> ProvisionResult<()> {
    let password = random_password()?;
    let username = "winlane";
    let computer_name = computer_name(&options.spec.name);
    let xml = unattend_xml(
        &computer_name,
        username,
        &password,
        &options.spec.network,
        options.image_name.as_deref(),
        &options.product_key,
    );
    fs::write(paths.unattend_dir.join("autounattend.xml"), xml)?;
    fs::write(
        paths.unattend_dir.join("winlane-firstboot.cmd"),
        firstboot_cmd(),
    )?;

    let output = if command_available("xorriso") {
        Command::new("xorriso")
            .args([
                "-as",
                "mkisofs",
                "-quiet",
                "-J",
                "-r",
                "-V",
                "WINLANE_UNATTEND",
                "-o",
            ])
            .arg(&paths.unattended_iso)
            .arg(&paths.unattend_dir)
            .output()?
    } else {
        Command::new("genisoimage")
            .args(["-quiet", "-J", "-r", "-V", "WINLANE_UNATTEND", "-o"])
            .arg(&paths.unattended_iso)
            .arg(&paths.unattend_dir)
            .output()?
    };
    if !output.status.success() {
        return Err(command_error("iso builder", output.status, &output.stderr));
    }
    Ok(())
}

fn execute_install(options: &ProvisionOptions, paths: &PipelinePaths) -> InstallOutcome {
    let mut timings = Vec::new();
    let mut outcome = InstallOutcome {
        readiness: ReadinessReport::default(),
        shutdown: None,
        timings: Vec::new(),
        qemu_command: Vec::new(),
        final_screenshot_path: None,
        error: None,
    };

    if let Err(error) = create_disk(options, paths, &mut timings) {
        outcome.error = Some(error.to_string());
        outcome.timings = timings;
        return outcome;
    }
    if let Err(error) = prepare_firmware(options, paths, &mut timings) {
        outcome.error = Some(error.to_string());
        outcome.timings = timings;
        return outcome;
    }

    let tpm_started = Instant::now();
    let mut swtpm = match start_swtpm(options, paths) {
        Ok(child) => child,
        Err(error) => {
            outcome.error = Some(error.to_string());
            outcome.timings = timings;
            return outcome;
        }
    };
    push_timing(&mut timings, ProvisionStage::StartingTpm, tpm_started);

    let qemu_started = Instant::now();
    let qemu_args = qemu_args(options, paths);
    outcome.qemu_command = qemu_args
        .iter()
        .map(|arg| arg.to_string_lossy().into_owned())
        .collect();
    let mut qemu = match spawn_qemu(options, paths, &qemu_args) {
        Ok(child) => child,
        Err(error) => {
            stop_child(&mut swtpm);
            outcome.error = Some(error.to_string());
            outcome.timings = timings;
            return outcome;
        }
    };
    push_timing(&mut timings, ProvisionStage::StartingQemu, qemu_started);
    nudge_boot_key(paths);

    let install_started = Instant::now();
    let readiness_result = wait_for_readiness(options, paths, &mut qemu);
    push_timing(
        &mut timings,
        ProvisionStage::InstallingWindows,
        install_started,
    );

    match readiness_result {
        Ok(readiness) => {
            outcome.readiness = readiness;
            let screenshot_started = Instant::now();
            if let Err(error) = screenshot(paths) {
                outcome.readiness.detail = Some(format!(
                    "readiness reached; final screenshot failed: {error}"
                ));
            } else {
                outcome.final_screenshot_path = Some(paths.screenshot.clone());
            }
            push_timing(
                &mut timings,
                ProvisionStage::CheckingDesktop,
                screenshot_started,
            );
        }
        Err(error) => {
            let _ = screenshot(paths);
            outcome.error = Some(error);
        }
    }

    let shutdown_started = Instant::now();
    let shutdown = shutdown_qemu(paths, &mut qemu);
    push_timing(&mut timings, ProvisionStage::ShuttingDown, shutdown_started);
    outcome.shutdown = Some(shutdown);
    stop_child(&mut swtpm);
    outcome.timings = timings;
    outcome
}

fn create_disk(
    options: &ProvisionOptions,
    paths: &PipelinePaths,
    timings: &mut Vec<StageTiming>,
) -> ProvisionResult<()> {
    let started = Instant::now();
    if paths.disk.exists() {
        return Err(ProvisionError::InvalidRequest(format!(
            "disk already exists at {}; use a fresh machine name or remove it deliberately",
            qemu_path(&paths.disk)
        )));
    }
    let output = Command::new(&options.qemu_img_binary)
        .args(["create", "-f", "qcow2"])
        .arg(&paths.disk)
        .arg(format!("{}G", options.spec.disk_gb))
        .output()?;
    if !output.status.success() {
        return Err(command_error("qemu-img", output.status, &output.stderr));
    }
    push_timing(timings, ProvisionStage::CreatingDisk, started);
    Ok(())
}

fn prepare_firmware(
    options: &ProvisionOptions,
    paths: &PipelinePaths,
    timings: &mut Vec<StageTiming>,
) -> ProvisionResult<()> {
    let started = Instant::now();
    let (_code, vars) = ovmf_paths(options);
    fs::copy(vars, &paths.ovmf_vars)?;
    push_timing(timings, ProvisionStage::PreparingFirmware, started);
    Ok(())
}

fn start_swtpm(options: &ProvisionOptions, paths: &PipelinePaths) -> ProvisionResult<Child> {
    let log = File::create(&paths.swtpm_log)?;
    let stderr = log.try_clone()?;
    let child = Command::new(&options.swtpm_binary)
        .arg("socket")
        .arg("--ctrl")
        .arg(format!("type=unixio,path={}", paths.swtpm_socket.display()))
        .arg("--terminate")
        .arg("--tpmstate")
        .arg(format!("dir={}", paths.state_dir.display()))
        .arg("--tpm2")
        .stdout(Stdio::from(log))
        .stderr(Stdio::from(stderr))
        .spawn()?;
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        if paths.swtpm_socket.exists() {
            return Ok(child);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(ProvisionError::MissingRequirement(format!(
        "swtpm socket did not appear at {}; see {}",
        paths.swtpm_socket.display(),
        paths.swtpm_log.display()
    )))
}

fn qemu_path(path: &Path) -> String {
    path.display().to_string().replace(',', ",,")
}

fn qemu_args(options: &ProvisionOptions, paths: &PipelinePaths) -> Vec<std::ffi::OsString> {
    let (ovmf_code, _vars) = ovmf_paths(options);
    let network = match options.spec.network {
        NetworkMode::Isolated => "user,id=nic,restrict=on".to_string(),
        NetworkMode::Nat => "user,id=nic".to_string(),
    };
    let mut args: Vec<std::ffi::OsString> = vec![
        "-machine".into(),
        "q35,smm=on,accel=kvm".into(),
        "-cpu".into(),
        "host".into(),
        "-m".into(),
        options.spec.memory_mb.to_string().into(),
        "-smp".into(),
        options.spec.cpus.to_string().into(),
        "-global".into(),
        "driver=cfi.pflash01,property=secure,value=on".into(),
        "-drive".into(),
        format!(
            "if=pflash,format=raw,unit=0,file={},readonly=on",
            qemu_path(&ovmf_code)
        )
        .into(),
        "-drive".into(),
        format!(
            "if=pflash,format=raw,unit=1,file={}",
            qemu_path(&paths.ovmf_vars)
        )
        .into(),
        "-drive".into(),
        format!(
            "id=SystemDisk,if=none,format=qcow2,file={},discard=unmap,detect-zeroes=unmap,cache=writeback,aio=threads",
            qemu_path(&paths.disk)
        )
        .into(),
        "-device".into(),
        "virtio-blk-pci,drive=SystemDisk,bootindex=1".into(),
        "-device".into(),
        "ich9-ahci,id=sata".into(),
        "-drive".into(),
        format!(
            "if=none,id=InstallMedia,format=raw,media=cdrom,readonly=on,file={}",
            qemu_path(Path::new(&options.spec.iso_path.clone().unwrap_or_default()))
        )
        .into(),
        "-device".into(),
        "ide-cd,drive=InstallMedia,bus=sata.1,bootindex=2".into(),
        "-drive".into(),
        format!(
            "if=none,id=VirtioMedia,format=raw,media=cdrom,readonly=on,file={}",
            qemu_path(&options.virtio_iso_path)
        )
        .into(),
        "-device".into(),
        "ide-cd,drive=VirtioMedia,bus=sata.2".into(),
        "-drive".into(),
        format!(
            "if=none,id=AnswerMedia,format=raw,media=cdrom,readonly=on,file={}",
            qemu_path(&paths.unattended_iso)
        )
        .into(),
        "-device".into(),
        "ide-cd,drive=AnswerMedia,bus=sata.3".into(),
        "-netdev".into(),
        network.into(),
        "-device".into(),
        "virtio-net-pci,netdev=nic".into(),
        "-device".into(),
        "virtio-balloon-pci".into(),
        "-device".into(),
        "qemu-xhci".into(),
        "-device".into(),
        "usb-tablet".into(),
        "-vga".into(),
        "virtio".into(),
        "-display".into(),
        "none".into(),
        "-monitor".into(),
        "none".into(),
        "-serial".into(),
        "none".into(),
        "-rtc".into(),
        "base=localtime".into(),
        "-qmp".into(),
        format!("unix:{},server=on,wait=off", qemu_path(&paths.qmp_socket)).into(),
        "-chardev".into(),
        format!(
            "socket,id=chrtpm,path={}",
            qemu_path(&paths.swtpm_socket)
        )
        .into(),
        "-tpmdev".into(),
        "emulator,id=tpm0,chardev=chrtpm".into(),
        "-device".into(),
        "tpm-tis,tpmdev=tpm0".into(),
        "-device".into(),
        "virtio-serial-pci".into(),
        "-chardev".into(),
        format!(
            "socket,id=qga0,path={},server=on,wait=off",
            qemu_path(&paths.qga_socket)
        )
        .into(),
        "-device".into(),
        "virtserialport,chardev=qga0,name=org.qemu.guest_agent.0".into(),
    ];
    args.insert(0, options.qemu_binary.clone().into_os_string());
    args
}

fn spawn_qemu(
    _options: &ProvisionOptions,
    paths: &PipelinePaths,
    args: &[std::ffi::OsString],
) -> ProvisionResult<Child> {
    let program = args
        .first()
        .ok_or_else(|| ProvisionError::InvalidRequest("empty QEMU command".to_string()))?;
    let stdout = File::create(&paths.qemu_log)?;
    let stderr = stdout.try_clone()?;
    Command::new(program)
        .args(&args[1..])
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr))
        .spawn()
        .map_err(ProvisionError::Io)
}

fn nudge_boot_key(paths: &PipelinePaths) {
    let deadline = Instant::now() + Duration::from_secs(12);
    while Instant::now() < deadline {
        if paths.qmp_socket.exists()
            && let Ok(mut client) =
                QmpClient::connect(&paths.qmp_socket, Duration::from_millis(750))
        {
            let _ = client.send_key("ret");
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn wait_for_readiness(
    options: &ProvisionOptions,
    paths: &PipelinePaths,
    qemu: &mut Child,
) -> Result<ReadinessReport, String> {
    let deadline = Instant::now() + options.install_timeout;
    let mut last_detail = "guest agent has not answered".to_string();
    while Instant::now() < deadline {
        if let Some(status) = qemu
            .try_wait()
            .map_err(|error| format!("failed to inspect QEMU process: {error}"))?
        {
            let tail = read_tail(&paths.qemu_log, 4096).unwrap_or_default();
            return Err(format!(
                "QEMU exited before Windows readiness: {status}; log {}; tail: {}",
                paths.qemu_log.display(),
                tail.trim()
            ));
        }

        match probe_guest(&paths.qga_socket) {
            Ok(report) if report.firstboot_sentinel && report.explorer_running => {
                return Ok(report);
            }
            Ok(report) => {
                last_detail = report.detail.unwrap_or_else(|| {
                    format!(
                        "ping={} exec={} sentinel={} explorer={}",
                        report.guest_agent_ping,
                        report.guest_exec,
                        report.firstboot_sentinel,
                        report.explorer_running
                    )
                });
            }
            Err(error) => {
                last_detail = error;
            }
        }
        std::thread::sleep(Duration::from_secs(5));
    }
    Err(format!(
        "timed out after {} seconds waiting for Windows desktop readiness: {}",
        options.install_timeout.as_secs(),
        last_detail
    ))
}

fn probe_guest(path: &Path) -> Result<ReadinessReport, String> {
    let mut report = ReadinessReport::default();
    let mut client = QgaClient::connect(path, Duration::from_secs(2))
        .map_err(|error| format!("guest agent socket unavailable: {error}"))?;
    if !client.ping() {
        report.detail = Some("guest agent ping failed".to_string());
        return Ok(report);
    }
    report.guest_agent_ping = true;

    match run_guest_command(
        &mut client,
        "C:\\Windows\\System32\\cmd.exe",
        &["/c", "whoami"],
    ) {
        Ok(status) if status.exit_code == Some(0) => {
            report.guest_exec = true;
            report.whoami = status.stdout.map(|value| value.trim().to_string());
        }
        Ok(status) => {
            report.detail = Some(format!(
                "whoami failed: code {:?}, stderr {:?}",
                status.exit_code, status.stderr
            ));
            return Ok(report);
        }
        Err(error) => {
            report.detail = Some(format!("whoami execution failed: {error}"));
            return Ok(report);
        }
    }

    match run_guest_command(
        &mut client,
        "C:\\Windows\\System32\\cmd.exe",
        &[
            "/c",
            "if exist C:\\winlane-ready.txt (exit /b 0) else (exit /b 1)",
        ],
    ) {
        Ok(status) => report.firstboot_sentinel = status.exit_code == Some(0),
        Err(error) => report.detail = Some(format!("firstboot probe failed: {error}")),
    }

    match run_guest_command(
        &mut client,
        "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe",
        &[
            "-NoProfile",
            "-Command",
            "if (Get-Process explorer -ErrorAction SilentlyContinue) { exit 0 } else { exit 1 }",
        ],
    ) {
        Ok(status) => report.explorer_running = status.exit_code == Some(0),
        Err(error) => report.detail = Some(format!("explorer probe failed: {error}")),
    }
    Ok(report)
}

fn run_guest_command(
    client: &mut QgaClient,
    path: &str,
    args: &[&str],
) -> Result<GuestExecStatus, String> {
    let pid = client
        .exec(path, args)
        .map_err(|error| format!("guest-exec failed: {error}"))?;
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        let status = client
            .exec_status(pid)
            .map_err(|error| format!("guest-exec-status failed: {error}"))?;
        if status.exited {
            return Ok(status);
        }
        std::thread::sleep(Duration::from_millis(250));
    }
    Err("guest command timed out".to_string())
}

fn screenshot(paths: &PipelinePaths) -> QemuProtocolResult<()> {
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if paths.qmp_socket.exists() {
            let mut client = QmpClient::connect(&paths.qmp_socket, Duration::from_secs(2))?;
            return client.screendump(&paths.screenshot);
        }
        std::thread::sleep(Duration::from_millis(100));
    }
    Err(QemuProtocolError::MissingField("qmp socket"))
}

fn shutdown_qemu(paths: &PipelinePaths, qemu: &mut Child) -> ShutdownReport {
    let mut powerdown_sent = false;
    if paths.qmp_socket.exists()
        && let Ok(mut client) = QmpClient::connect(&paths.qmp_socket, Duration::from_secs(2))
    {
        powerdown_sent = client.system_powerdown().is_ok();
    }

    let deadline = Instant::now() + Duration::from_secs(90);
    let mut exit_status = None;
    while Instant::now() < deadline {
        match qemu.try_wait() {
            Ok(Some(status)) => {
                exit_status = Some(status.to_string());
                break;
            }
            Ok(None) => std::thread::sleep(Duration::from_secs(1)),
            Err(_) => break,
        }
    }

    let mut forced_kill = false;
    if exit_status.is_none() {
        forced_kill = qemu.kill().is_ok();
        exit_status = qemu.wait().ok().map(|status| status.to_string());
    }

    ShutdownReport {
        powerdown_sent,
        forced_kill,
        exit_status,
    }
}

fn stop_child(child: &mut Child) {
    if child.try_wait().ok().flatten().is_none() {
        let _ = child.kill();
    }
    let _ = child.wait();
}

fn finish_failed(
    mut report: ProvisionReport,
    started: Instant,
    stage: ProvisionStage,
    error: String,
) -> ProvisionResult<ProvisionReport> {
    report.success = false;
    report.stage = stage;
    report.error = Some(error);
    finish_report(report, started)
}

fn finish_report(
    mut report: ProvisionReport,
    started: Instant,
) -> ProvisionResult<ProvisionReport> {
    report.finished_unix_ms = unix_ms();
    report.total_duration_ms = started.elapsed().as_millis() as u64;
    if let Some(parent) = report.report_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let encoded = serde_json::to_string_pretty(&report)?;
    fs::write(&report.report_path, encoded)?;
    Ok(report)
}

fn push_timing(timings: &mut Vec<StageTiming>, stage: ProvisionStage, started: Instant) {
    let duration_ms = started.elapsed().as_millis() as u64;
    timings.push(StageTiming {
        stage,
        started_unix_ms: unix_ms().saturating_sub(duration_ms),
        duration_ms,
    });
}

fn ovmf_paths(options: &ProvisionOptions) -> (PathBuf, PathBuf) {
    (
        options
            .ovmf_code_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("/usr/share/OVMF/OVMF_CODE_4M.secboot.fd")),
        options
            .ovmf_vars_path
            .clone()
            .unwrap_or_else(|| PathBuf::from("/usr/share/OVMF/OVMF_VARS_4M.ms.fd")),
    )
}

fn command_available(binary: &str) -> bool {
    Command::new(binary)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
}

fn command_error(program: &str, status: ExitStatus, stderr: &[u8]) -> ProvisionError {
    ProvisionError::Command {
        program: program.to_string(),
        status: status.to_string(),
        stderr: String::from_utf8_lossy(stderr).trim().to_string(),
    }
}

fn random_password() -> ProvisionResult<String> {
    let mut bytes = [0u8; 18];
    File::open("/dev/urandom")?.read_exact(&mut bytes)?;
    Ok(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}

fn computer_name(machine_name: &str) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in machine_name.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    let mut stem: String = machine_name
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .take(6)
        .collect();
    if stem.is_empty() {
        stem = "vm".to_string();
    }
    format!("wl-{stem}-{:04x}", hash & 0xffff)
}

fn read_tail(path: &Path, bytes: usize) -> std::io::Result<String> {
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
    let offset = length.saturating_sub(bytes as u64);
    use std::io::Seek;
    file.seek(std::io::SeekFrom::Start(offset))?;
    let mut buffer = String::new();
    file.read_to_string(&mut buffer)?;
    Ok(buffer)
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

pub fn write_human_log(report: &ProvisionReport, mut output: impl Write) -> std::io::Result<()> {
    writeln!(output, "machine: {}", report.machine_name)?;
    writeln!(output, "success: {}", report.success)?;
    writeln!(output, "stage: {:?}", report.stage)?;
    if let Some(error) = &report.error {
        writeln!(output, "error: {error}")?;
    }
    writeln!(output, "report: {}", report.report_path.display())?;
    writeln!(output, "disk: {}", report.disk_path.display())
}

#[cfg(test)]
mod tests {
    use super::*;
    use winlane_contracts::{BootMode, CpuArch, NetworkMode};

    fn spec() -> MachineSpec {
        MachineSpec {
            name: "proof".to_string(),
            arch: CpuArch::X86_64,
            memory_mb: 8192,
            cpus: 4,
            disk_gb: 80,
            iso_path: Some("/tmp/windows.iso".to_string()),
            network: NetworkMode::Isolated,
            tpm: true,
            boot: BootMode::Uefi,
        }
    }

    #[test]
    fn validates_windows_11_floor() {
        let mut options = ProvisionOptions::new(
            spec(),
            PathBuf::from("/tmp/virtio.iso"),
            PathBuf::from("/tmp/winlane-test"),
        );
        options.spec.memory_mb = 2048;
        assert!(validate_request(&options).is_err());
    }

    #[test]
    fn isolated_network_uses_restricted_user_network() {
        let options = ProvisionOptions::new(
            spec(),
            PathBuf::from("/tmp/virtio.iso"),
            PathBuf::from("/tmp/winlane-test"),
        );
        let paths = pipeline_paths(&options);
        let args = qemu_args(&options, &paths)
            .iter()
            .map(|arg| arg.to_string_lossy().into_owned())
            .collect::<Vec<_>>();
        assert!(args.iter().any(|arg| arg == "user,id=nic,restrict=on"));
        assert!(args.iter().any(|arg| arg.contains("tpm-tis")));
        assert!(
            args.iter()
                .any(|arg| arg.contains("org.qemu.guest_agent.0"))
        );
        assert!(
            args.iter()
                .any(|arg| arg == "ide-cd,drive=AnswerMedia,bus=sata.3")
        );
        assert!(
            args.iter()
                .any(|arg| arg == "ide-cd,drive=InstallMedia,bus=sata.1,bootindex=2")
        );
        assert!(
            args.iter()
                .any(|arg| arg == "virtio-blk-pci,drive=SystemDisk,bootindex=1")
        );
    }

    #[test]
    fn computer_names_fit_netbios_limit() {
        let name = computer_name("averyveryverylong_machine_name");
        assert!(name.len() <= 15);
        assert!(!name.contains('_'));
    }
}
