use std::collections::HashMap;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::io::{AsyncReadExt, AsyncSeekExt};
use tokio::process::{Child, Command};
use tokio::time::{sleep, timeout};
use uuid::Uuid;
use winlane_contracts::{
    BootMode, CheckpointRef, ContractResult, ControlEvent, CpuArch, FrameRef, FrameSource,
    MachineBackend, MachineHandle, MachineSpec, MachineState, MouseButton, NetworkMode,
    ScrollDirection,
};

use crate::error::{CoreError, CoreResult};
use crate::qmp::QmpClient;

const MACHINE_FILE: &str = "machine.json";
const SPEC_FILE: &str = "spec.json";
const SCREEN_FILE: &str = "screen.json";
const FRAME_DIR: &str = "frames";
const CHECKPOINT_DIR: &str = "checkpoints";
const SYSTEM_DISK_NODE: &str = "winlane-system";
const SNAPSHOT_COMMAND_TIMEOUT: Duration = Duration::from_secs(300);
const KEY_HOLD_TIME_MS: u64 = 30;
const CLICK_HOLD_TIME: Duration = Duration::from_millis(50);
const DISK_FILE: &str = "disk.qcow2";
const QMP_SOCKET_FILE: &str = "qmp.sock";
const QGA_SOCKET_FILE: &str = "qga.sock";
const TPM_SOCKET_FILE: &str = "swtpm.sock";
const QEMU_LOG_FILE: &str = "qemu.log";
const SWTPM_LOG_FILE: &str = "swtpm.log";
const MAX_LOG_TAIL_BYTES: u64 = 64 * 1024;
const MAX_LOG_TAIL_LINES: usize = 1000;
const TPM_DIR: &str = "tpm";
const OVMF_VARS_FILE: &str = "ovmf-vars.fd";
const MAX_SOCKET_PATH_BYTES: usize = 100;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameDimensions {
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct MachineRecord {
    handle: MachineHandle,
    spec: MachineSpec,
    disk_path: PathBuf,
    qmp_socket_path: PathBuf,
    #[serde(default)]
    qga_socket_path: Option<PathBuf>,
    pid: Option<u32>,
    ovmf_vars_path: Option<PathBuf>,
    tpm_state_dir: Option<PathBuf>,
    pending_loadvm: Option<String>,
    checkpoints: Vec<CheckpointRef>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct CheckpointMetadata {
    checkpoint: CheckpointRef,
    label: String,
    backend: String,
    qemu_snapshot: String,
    disk_state_included: bool,
    vm_state_included: bool,
    firmware_vars_included: bool,
    tpm_state_included: bool,
}

struct RunningMachine {
    child: Child,
    qmp: QmpClient,
    swtpm: Option<Child>,
}

/// QEMU-backed machine control plane for the Linux x86_64 KVM path.
pub struct QemuBackend {
    root_dir: PathBuf,
    qemu_binary: PathBuf,
    qemu_img_binary: PathBuf,
    swtpm_binary: PathBuf,
    ovmf_code: Option<PathBuf>,
    ovmf_vars_template: Option<PathBuf>,
    running: HashMap<String, RunningMachine>,
}

impl QemuBackend {
    pub async fn new(root_dir: impl Into<PathBuf>) -> CoreResult<Self> {
        let root_dir = root_dir.into();
        let mut builder = tokio::fs::DirBuilder::new();
        builder.recursive(true).mode(0o700);
        builder
            .create(&root_dir)
            .await
            .map_err(CoreError::io("create machine root", &root_dir))?;
        let root_dir = tokio::fs::canonicalize(&root_dir)
            .await
            .map_err(CoreError::io("canonicalize machine root", &root_dir))?;
        Ok(Self {
            root_dir,
            qemu_binary: PathBuf::from("qemu-system-x86_64"),
            qemu_img_binary: PathBuf::from("qemu-img"),
            swtpm_binary: PathBuf::from("swtpm"),
            ovmf_code: None,
            ovmf_vars_template: None,
            running: HashMap::new(),
        })
    }

    pub fn with_qemu_binaries(
        mut self,
        qemu_binary: impl Into<PathBuf>,
        qemu_img_binary: impl Into<PathBuf>,
    ) -> Self {
        self.qemu_binary = qemu_binary.into();
        self.qemu_img_binary = qemu_img_binary.into();
        self
    }

    pub fn with_swtpm_binary(mut self, swtpm_binary: impl Into<PathBuf>) -> Self {
        self.swtpm_binary = swtpm_binary.into();
        self
    }

    pub fn with_uefi_firmware(
        mut self,
        ovmf_code: impl Into<PathBuf>,
        ovmf_vars_template: impl Into<PathBuf>,
    ) -> Self {
        self.ovmf_code = Some(ovmf_code.into());
        self.ovmf_vars_template = Some(ovmf_vars_template.into());
        self
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    pub async fn guest_agent_socket_path(
        &self,
        machine: &MachineHandle,
    ) -> CoreResult<Option<PathBuf>> {
        let record = self.load_record(machine).await?;
        if !record.spec.guest_agent {
            return Ok(None);
        }
        Ok(Some(record.qga_socket_path.unwrap_or_else(|| {
            PathBuf::from(&record.handle.state_dir).join(QGA_SOCKET_FILE)
        })))
    }

    /// Return valid machine records directly under the backend root, sorted by name.
    /// Malformed or unrelated entries are ignored so one stale artifact cannot hide machines.
    pub async fn list_machines(&mut self) -> CoreResult<Vec<MachineHandle>> {
        let mut entries = tokio::fs::read_dir(&self.root_dir)
            .await
            .map_err(CoreError::io("read machine root", &self.root_dir))?;
        let mut machines = Vec::new();
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(CoreError::io("iterate machine root", &self.root_dir))?
        {
            let path = entry.path();
            let Ok(file_type) = entry.file_type().await else {
                continue;
            };
            if !file_type.is_dir() {
                continue;
            }
            let record_path = path.join(MACHINE_FILE);
            let Ok(bytes) = tokio::fs::read(&record_path).await else {
                continue;
            };
            let Ok(record) = serde_json::from_slice::<MachineRecord>(&bytes) else {
                continue;
            };
            if path.file_name().and_then(|name| name.to_str()) != Some(record.handle.name.as_str())
                || Path::new(&record.handle.state_dir) != path
            {
                continue;
            }
            let mut handle = record.handle;
            if let Ok(state) = self.status_machine(&handle).await {
                handle.state = state;
            }
            machines.push(handle);
        }
        machines.sort_by(|left, right| left.name.cmp(&right.name).then(left.id.cmp(&right.id)));
        Ok(machines)
    }

    pub async fn create_machine(&mut self, spec: &MachineSpec) -> CoreResult<MachineHandle> {
        spec.validate()?;
        self.create_machine_inner(spec).await
    }

    async fn create_machine_inner(&mut self, spec: &MachineSpec) -> CoreResult<MachineHandle> {
        spec.validate()?;
        if spec.arch != CpuArch::X86_64 {
            return Err(CoreError::Unsupported(
                "QEMU control currently supports x86_64 only",
            ));
        }
        if let Some(iso_path) = &spec.iso_path {
            let path = PathBuf::from(iso_path);
            tokio::fs::metadata(&path)
                .await
                .map_err(CoreError::io("read install iso", path))?;
        }
        let mut stored_spec = spec.clone();
        let backing_path = if let Some(base_image_path) = &spec.base_image_path {
            let path = tokio::fs::canonicalize(PathBuf::from(base_image_path))
                .await
                .map_err(CoreError::io("canonicalize base image", base_image_path))?;
            let metadata = tokio::fs::metadata(&path)
                .await
                .map_err(CoreError::io("read base image", &path))?;
            if !metadata.is_file() {
                return Err(CoreError::InvalidHandle(
                    "base image path must be a regular file".to_string(),
                ));
            }
            let backing_disk_gb = self.qcow2_virtual_size_gb(&path).await?;
            stored_spec.disk_gb = stored_spec.disk_gb.max(backing_disk_gb);
            stored_spec.base_image_path = Some(path_to_string(&path)?);
            Some(path)
        } else {
            None
        };

        let state_dir = self.root_dir.join(&spec.name);
        if tokio::fs::try_exists(&state_dir)
            .await
            .map_err(CoreError::io("check machine state directory", &state_dir))?
        {
            return Err(CoreError::InvalidHandle(format!(
                "machine name '{}' is already in use",
                spec.name
            )));
        }
        let qmp_socket_path = state_dir.join(QMP_SOCKET_FILE);
        validate_socket_path(&qmp_socket_path)?;
        let qga_socket_path = machine_qga_socket_path(&state_dir, spec.guest_agent)?;

        create_machine_state_dir(&state_dir, &spec.name).await?;
        let result = async {
            let disk_path = state_dir.join(DISK_FILE);
            self.run_qemu_img_create(&disk_path, backing_path.as_deref(), stored_spec.disk_gb)
                .await?;
            set_private_file(&disk_path).await?;

            let tpm_state_dir = if spec.tpm {
                let dir = state_dir.join(TPM_DIR);
                create_private_dir(&dir, "create tpm state directory").await?;
                Some(dir)
            } else {
                None
            };

            let handle = MachineHandle {
                id: format!("machine-{}", Uuid::new_v4().simple()),
                name: spec.name.clone(),
                state_dir: state_dir.to_string_lossy().into_owned(),
                backend: "qemu".to_string(),
                state: MachineState::Defined,
            };
            let record = MachineRecord {
                handle: handle.clone(),
                spec: stored_spec.clone(),
                disk_path,
                qmp_socket_path,
                qga_socket_path,
                pid: None,
                ovmf_vars_path: None,
                tpm_state_dir,
                pending_loadvm: None,
                checkpoints: Vec::new(),
            };
            write_json(&state_dir.join(SPEC_FILE), &stored_spec).await?;
            self.write_record(&record).await?;
            Ok::<MachineHandle, CoreError>(handle)
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&state_dir).await;
        }
        result
    }

    pub async fn start_machine(&mut self, machine: &MachineHandle) -> CoreResult<()> {
        let mut record = self.load_record(machine).await?;
        let state = self.status_machine(&record.handle).await?;
        if matches!(
            state,
            MachineState::Running | MachineState::Paused | MachineState::Failed
        ) {
            return Err(CoreError::InvalidState {
                machine: record.handle.name,
                actual: match state {
                    MachineState::Running => "running",
                    MachineState::Paused => "paused",
                    _ => "failed",
                },
                expected: "stopped",
            });
        }

        let mut swtpm = if record.spec.tpm {
            Some(self.start_swtpm(&record).await?)
        } else {
            None
        };

        let args = self.qemu_args(&mut record).await?;
        let log_path = PathBuf::from(&record.handle.state_dir).join(QEMU_LOG_FILE);
        let log_stdout = append_log_stdio(&log_path, "open qemu log").await?;
        let log_stderr = append_log_stdio(&log_path, "open qemu log").await?;
        let mut command = Command::new(&self.qemu_binary);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(log_stdout)
            .stderr(log_stderr)
            .kill_on_drop(true);
        let mut child = command.spawn().map_err(CoreError::io(
            "spawn qemu process",
            self.qemu_binary.clone(),
        ))?;

        let qmp = match connect_qmp_with_retry(&record.qmp_socket_path).await {
            Ok(qmp) => qmp,
            Err(error) => {
                let _ = child.kill().await;
                if let Some(process) = &mut swtpm {
                    let _ = process.kill().await;
                }
                return Err(error);
            }
        };
        let permissions_result = async {
            set_private_file(&record.qmp_socket_path).await?;
            if let Some(qga_socket) = &record.qga_socket_path {
                set_private_file_if_exists(qga_socket).await?;
            }
            set_private_file_if_exists(&PathBuf::from(&record.handle.state_dir).join("qemu.pid"))
                .await
        }
        .await;
        if let Err(error) = permissions_result {
            let _ = child.kill().await;
            if let Some(process) = &mut swtpm {
                let _ = process.kill().await;
            }
            return Err(error);
        }

        if let Some(snapshot) = record.pending_loadvm.clone() {
            let load_result: CoreResult<()> = async {
                qmp.execute_with_timeout(
                    "snapshot-load",
                    Some(json!({
                        "job-id": format!("load-{snapshot}"),
                        "tag": snapshot,
                        "vmstate": SYSTEM_DISK_NODE,
                        "devices": [SYSTEM_DISK_NODE],
                    })),
                    SNAPSHOT_COMMAND_TIMEOUT,
                )
                .await?;
                wait_for_snapshot_job(&qmp, &format!("load-{snapshot}")).await?;
                qmp.execute("cont", None).await?;
                Ok(())
            }
            .await;
            if let Err(error) = load_result {
                let _ = child.kill().await;
                if let Some(process) = &mut swtpm {
                    let _ = process.kill().await;
                }
                return Err(error);
            }
        }

        record.pid = child.id();
        record.handle.state = MachineState::Running;
        record.pending_loadvm = None;
        self.write_record(&record).await?;
        self.running.insert(
            record.handle.id.clone(),
            RunningMachine { child, qmp, swtpm },
        );
        Ok(())
    }

    pub async fn machine_logs(&self, machine: &MachineHandle, lines: usize) -> CoreResult<Value> {
        let record = self.load_record(machine).await?;
        let lines = lines.clamp(1, MAX_LOG_TAIL_LINES);
        let state_dir = PathBuf::from(&record.handle.state_dir);
        let qemu = tail_log(&state_dir.join(QEMU_LOG_FILE), lines).await?;
        let swtpm = tail_log(&state_dir.join(SWTPM_LOG_FILE), lines).await?;
        Ok(json!({
            "machine": &record.handle,
            "qemu_log": qemu,
            "swtpm_log": swtpm,
        }))
    }

    pub async fn stop_machine(&mut self, machine: &MachineHandle) -> CoreResult<()> {
        let mut record = self.load_record(machine).await?;
        if let Some(mut running) = self.running.remove(&record.handle.id) {
            shutdown_running_machine(&mut running).await?;
        } else if tokio::fs::try_exists(&record.qmp_socket_path)
            .await
            .map_err(CoreError::io(
                "check qmp socket",
                record.qmp_socket_path.clone(),
            ))?
        {
            let pid = machine_process_id(&record).await;
            match QmpClient::connect(&record.qmp_socket_path).await {
                Ok(qmp) => {
                    let _ = qmp.execute("quit", None).await;
                    if !wait_for_external_qmp_exit(&record.qmp_socket_path, pid).await? {
                        return Err(CoreError::Qemu(format!(
                            "qemu process for machine '{}' did not exit after qmp quit",
                            record.handle.name
                        )));
                    }
                }
                Err(_) => {
                    if !wait_for_external_qmp_exit(&record.qmp_socket_path, pid).await? {
                        return Err(CoreError::Qemu(format!(
                            "qemu process for machine '{}' is not accepting qmp connections but has not exited",
                            record.handle.name
                        )));
                    }
                }
            }
        } else {
            let pid = machine_process_id(&record).await;
            if !wait_for_external_qmp_exit(&record.qmp_socket_path, pid).await? {
                return Err(CoreError::Qemu(format!(
                    "qemu process for machine '{}' is still running without a qmp socket",
                    record.handle.name
                )));
            }
        }

        let state_dir = PathBuf::from(&record.handle.state_dir);
        remove_stale_socket(&state_dir.join(QMP_SOCKET_FILE)).await?;
        remove_stale_socket(&state_dir.join(QGA_SOCKET_FILE)).await?;
        record.pid = None;
        record.handle.state = MachineState::Stopped;
        self.write_record(&record).await?;
        Ok(())
    }

    pub async fn shutdown_machines(&mut self) -> CoreResult<()> {
        let machines = self.list_machines().await?;
        let mut first_error = None;
        for handle in machines {
            if !matches!(
                handle.state,
                MachineState::Running | MachineState::Paused | MachineState::Failed
            ) {
                continue;
            }
            if let Err(error) = self.stop_machine(&handle).await
                && first_error.is_none()
            {
                first_error = Some(error);
            }
        }
        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }

    pub async fn status_machine(&mut self, machine: &MachineHandle) -> CoreResult<MachineState> {
        self.inspect_machine_state(machine, true).await
    }

    async fn inspect_machine_state(
        &mut self,
        machine: &MachineHandle,
        persist: bool,
    ) -> CoreResult<MachineState> {
        let mut record = self.load_record(machine).await?;
        let machine_id = record.handle.id.clone();
        let mut remove_dead_process = false;
        let state = if let Some(running) = self.running.get_mut(&machine_id) {
            if let Some(exit_status) = running.child.try_wait().map_err(CoreError::io(
                "inspect qemu process",
                record.disk_path.clone(),
            ))? {
                if persist && let Some(process) = &mut running.swtpm {
                    let _ = process.kill().await;
                }
                remove_dead_process = true;
                if exit_status.success() {
                    MachineState::Stopped
                } else {
                    MachineState::Failed
                }
            } else {
                query_machine_state(&running.qmp).await?
            }
        } else if tokio::fs::try_exists(&record.qmp_socket_path)
            .await
            .map_err(CoreError::io(
                "check qmp socket",
                record.qmp_socket_path.clone(),
            ))?
        {
            match QmpClient::connect(&record.qmp_socket_path).await {
                Ok(qmp) => query_machine_state(&qmp).await?,
                Err(_) => external_machine_state(&record).await,
            }
        } else {
            external_machine_state(&record).await
        };

        if persist {
            if remove_dead_process {
                self.running.remove(&machine_id);
            }
            record.handle.state = state.clone();
            if matches!(state, MachineState::Stopped)
                || (matches!(state, MachineState::Failed) && remove_dead_process)
            {
                record.pid = None;
            }
            self.write_record(&record).await?;
        }
        Ok(state)
    }

    pub async fn clone_machine(
        &mut self,
        machine: &MachineHandle,
        name: &str,
    ) -> CoreResult<MachineHandle> {
        self.clone_machine_inner(machine, name).await
    }

    async fn clone_machine_inner(
        &mut self,
        machine: &MachineHandle,
        name: &str,
    ) -> CoreResult<MachineHandle> {
        let source = self.load_record(machine).await?;
        let state = self.status_machine(machine).await?;
        if matches!(
            state,
            MachineState::Running | MachineState::Paused | MachineState::Failed
        ) {
            return Err(CoreError::InvalidState {
                machine: source.handle.name,
                actual: match state {
                    MachineState::Running => "running",
                    MachineState::Paused => "paused",
                    _ => "failed",
                },
                expected: "stopped",
            });
        }
        if source.pending_loadvm.is_some() {
            return Err(CoreError::InvalidState {
                machine: source.handle.name,
                actual: "stopped with a pending checkpoint load",
                expected: "stopped without a pending checkpoint load",
            });
        }

        let mut spec = source.spec.clone();
        spec.name = name.to_string();
        spec.validate()?;
        let state_dir = self.root_dir.join(name);
        if tokio::fs::try_exists(&state_dir)
            .await
            .map_err(CoreError::io("check clone state directory", &state_dir))?
        {
            return Err(CoreError::InvalidHandle(format!(
                "machine name '{name}' is already in use"
            )));
        }
        validate_socket_path(&state_dir.join(QMP_SOCKET_FILE))?;
        let qga_socket_path = machine_qga_socket_path(&state_dir, spec.guest_agent)?;
        create_machine_state_dir(&state_dir, name).await?;

        let result = async {
            let disk_path = state_dir.join(DISK_FILE);
            self.run_qemu_img_convert(&source.disk_path, &disk_path)
                .await?;
            set_private_file(&disk_path).await?;

            let tpm_state_dir = if spec.tpm {
                let dir = state_dir.join(TPM_DIR);
                create_private_dir(&dir, "create clone tpm state directory").await?;
                Some(dir)
            } else {
                None
            };
            let handle = MachineHandle {
                id: format!("machine-{}", Uuid::new_v4().simple()),
                name: name.to_string(),
                state_dir: state_dir.to_string_lossy().into_owned(),
                backend: "qemu".to_string(),
                state: MachineState::Defined,
            };
            let record = MachineRecord {
                handle: handle.clone(),
                spec: spec.clone(),
                disk_path,
                qmp_socket_path: state_dir.join(QMP_SOCKET_FILE),
                qga_socket_path,
                pid: None,
                ovmf_vars_path: None,
                tpm_state_dir,
                pending_loadvm: None,
                checkpoints: Vec::new(),
            };
            write_json(&state_dir.join(SPEC_FILE), &spec).await?;
            self.write_record(&record).await?;
            Ok::<MachineHandle, CoreError>(handle)
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&state_dir).await;
        }
        result
    }

    pub async fn destroy_machine(&mut self, machine: &MachineHandle) -> CoreResult<()> {
        let record = self.load_record(machine).await?;
        let state = self.status_machine(machine).await?;
        if matches!(
            state,
            MachineState::Running | MachineState::Paused | MachineState::Failed
        ) {
            return Err(CoreError::InvalidState {
                machine: record.handle.name,
                actual: match state {
                    MachineState::Running => "running",
                    MachineState::Paused => "paused",
                    _ => "failed",
                },
                expected: "stopped",
            });
        }
        self.running.remove(&record.handle.id);
        let state_dir = PathBuf::from(&record.handle.state_dir);
        tokio::fs::remove_dir_all(&state_dir)
            .await
            .map_err(CoreError::io("remove machine state directory", state_dir))?;
        Ok(())
    }

    pub async fn checkpoint_machine(
        &mut self,
        machine: &MachineHandle,
        label: &str,
    ) -> CoreResult<CheckpointRef> {
        validate_checkpoint_label(label)?;
        let record = self.load_record(machine).await?;
        let state_dir = PathBuf::from(&record.handle.state_dir);
        let checkpoint_id = format!("checkpoint-{}", Uuid::new_v4().simple());
        let checkpoint_dir = state_dir.join(CHECKPOINT_DIR).join(&checkpoint_id);
        create_private_dir(&checkpoint_dir, "create checkpoint directory").await?;
        let result = self
            .checkpoint_machine_inner(machine, label, &checkpoint_id, &checkpoint_dir, record)
            .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&checkpoint_dir).await;
        }
        result
    }

    async fn checkpoint_machine_inner(
        &mut self,
        machine: &MachineHandle,
        label: &str,
        checkpoint_id: &str,
        checkpoint_dir: &Path,
        mut record: MachineRecord,
    ) -> CoreResult<CheckpointRef> {
        let vm_state_included = match self.status_machine(machine).await? {
            MachineState::Running => true,
            MachineState::Paused => {
                return Err(CoreError::InvalidState {
                    machine: record.handle.name.clone(),
                    actual: "paused",
                    expected: "running or stopped",
                });
            }
            _ => false,
        };
        let (firmware_vars_included, tpm_state_included) = if vm_state_included {
            let qmp = self.qmp_for_machine(machine).await?;
            qmp.execute("stop", None).await?;
            let copy_result = async {
                qmp.execute_with_timeout(
                    "snapshot-save",
                    Some(json!({
                        "job-id": checkpoint_id,
                        "tag": checkpoint_id,
                        "vmstate": SYSTEM_DISK_NODE,
                        "devices": [SYSTEM_DISK_NODE],
                    })),
                    SNAPSHOT_COMMAND_TIMEOUT,
                )
                .await?;
                wait_for_snapshot_job(&qmp, checkpoint_id).await?;
                let firmware_vars_included = copy_optional_file(
                    record.ovmf_vars_path.as_deref(),
                    &checkpoint_dir.join(OVMF_VARS_FILE),
                )
                .await?;
                let tpm_state_included = copy_optional_dir(
                    record.tpm_state_dir.as_deref(),
                    &checkpoint_dir.join(TPM_DIR),
                )
                .await?;
                Ok::<(bool, bool), CoreError>((firmware_vars_included, tpm_state_included))
            }
            .await;
            let cont_result = qmp.execute("cont", None).await;
            match (copy_result, cont_result) {
                (Ok(copies), Ok(_)) => copies,
                (Err(copy_error), Err(cont_error)) => {
                    return Err(CoreError::Qemu(format!(
                        "checkpoint failed: {copy_error}; machine could not be resumed: {cont_error}"
                    )));
                }
                (Err(error), Ok(_)) => return Err(error),
                (Ok(_), Err(error)) => return Err(error.into()),
            }
        } else {
            self.run_qemu_img_snapshot("-c", checkpoint_id, &record.disk_path)
                .await?;
            let firmware_vars_included = copy_optional_file(
                record.ovmf_vars_path.as_deref(),
                &checkpoint_dir.join(OVMF_VARS_FILE),
            )
            .await?;
            let tpm_state_included = copy_optional_dir(
                record.tpm_state_dir.as_deref(),
                &checkpoint_dir.join(TPM_DIR),
            )
            .await?;
            (firmware_vars_included, tpm_state_included)
        };

        let checkpoint = CheckpointRef {
            id: checkpoint_id.to_string(),
            machine_id: record.handle.id.clone(),
            created_unix_ms: unix_ms(),
            state_dir: record.handle.state_dir.clone(),
            sha256: None,
        };
        let metadata = CheckpointMetadata {
            checkpoint: checkpoint.clone(),
            label: label.to_string(),
            backend: "qemu".to_string(),
            qemu_snapshot: checkpoint_id.to_string(),
            disk_state_included: true,
            vm_state_included,
            firmware_vars_included,
            tpm_state_included,
        };
        let metadata_path = checkpoint_dir.join("checkpoint.json");
        write_json(&metadata_path, &metadata).await?;
        let metadata_bytes = tokio::fs::read(&metadata_path)
            .await
            .map_err(CoreError::io("read checkpoint metadata", &metadata_path))?;
        let mut checkpoint = metadata.checkpoint.clone();
        checkpoint.sha256 = Some(sha256_hex(&metadata_bytes));
        let metadata = CheckpointMetadata {
            checkpoint: checkpoint.clone(),
            ..metadata
        };
        write_json(&metadata_path, &metadata).await?;
        record.checkpoints.push(checkpoint.clone());
        self.write_record(&record).await?;
        Ok(checkpoint)
    }

    pub async fn list_checkpoints(
        &mut self,
        machine: &MachineHandle,
    ) -> CoreResult<serde_json::Value> {
        let record = self.load_record(machine).await?;
        let machine_state = self.inspect_machine_state(machine, false).await?;
        let state_dir = PathBuf::from(&record.handle.state_dir);
        let root = state_dir.join(CHECKPOINT_DIR);
        let mut entries = Vec::new();
        let registered: std::collections::BTreeMap<String, CheckpointRef> = record
            .checkpoints
            .iter()
            .map(|checkpoint| (checkpoint.id.clone(), checkpoint.clone()))
            .collect();
        let mut dirs = Vec::new();
        if tokio::fs::try_exists(&root)
            .await
            .map_err(CoreError::io("check checkpoint root", &root))?
        {
            let mut read = tokio::fs::read_dir(&root)
                .await
                .map_err(CoreError::io("read checkpoint root", &root))?;
            while let Some(entry) = read
                .next_entry()
                .await
                .map_err(CoreError::io("iterate checkpoint root", &root))?
            {
                if entry
                    .file_type()
                    .await
                    .map(|kind| kind.is_dir())
                    .unwrap_or(false)
                {
                    dirs.push(entry.path());
                }
            }
        }
        dirs.sort();
        let mut seen = std::collections::BTreeSet::new();
        for dir in dirs {
            let id = dir
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or_default()
                .to_string();
            seen.insert(id.clone());
            entries.push(
                checkpoint_catalog_entry(
                    &dir,
                    id.clone(),
                    registered.get(&id).cloned(),
                    &machine_state,
                    &record.handle.id,
                    &record.handle.state_dir,
                )
                .await,
            );
        }
        for (id, checkpoint) in registered {
            if !seen.contains(&id) {
                entries.push(json!({
                    "id": id,
                    "machine_id": checkpoint.machine_id,
                    "state_dir": checkpoint.state_dir,
                    "sha256": checkpoint.sha256,
                    "label": null,
                    "created_unix_ms": checkpoint.created_unix_ms,
                    "kind": null,
                    "state_dir_bytes": null,
                    "registered": true,
                    "restorable": false,
                    "error": "checkpoint directory is missing",
                }));
            }
        }
        Ok(json!(entries))
    }

    pub async fn delete_checkpoint(
        &mut self,
        machine: &MachineHandle,
        checkpoint_id: &str,
    ) -> CoreResult<CheckpointRef> {
        validate_checkpoint_id(checkpoint_id)?;
        let mut record = self.load_record(machine).await?;
        let state = self.inspect_machine_state(machine, false).await?;
        if matches!(
            state,
            MachineState::Running | MachineState::Paused | MachineState::Failed
        ) {
            return Err(CoreError::InvalidState {
                machine: record.handle.name.clone(),
                actual: match state {
                    MachineState::Running => "running",
                    MachineState::Paused => "paused",
                    _ => "failed",
                },
                expected: "stopped",
            });
        }
        let Some(index) = record
            .checkpoints
            .iter()
            .position(|checkpoint| checkpoint.id == checkpoint_id)
        else {
            return Err(CoreError::InvalidHandle(
                "checkpoint is not registered on the machine".to_string(),
            ));
        };

        let state_dir = PathBuf::from(&record.handle.state_dir);
        validate_checkpoint_path(&state_dir, true).await?;
        let checkpoint_root = state_dir.join(CHECKPOINT_DIR);
        let checkpoint_dir = checkpoint_root.join(checkpoint_id);
        validate_checkpoint_path(&checkpoint_root, true).await?;
        validate_checkpoint_path(&checkpoint_dir, true).await?;
        let metadata_path = checkpoint_dir.join("checkpoint.json");
        validate_checkpoint_path(&metadata_path, false).await?;
        let metadata_bytes = tokio::fs::read(&metadata_path)
            .await
            .map_err(CoreError::io("read checkpoint metadata", &metadata_path))?;
        let metadata: CheckpointMetadata = serde_json::from_slice(&metadata_bytes)?;
        validate_checkpoint_metadata(&metadata, &record, checkpoint_id)?;
        if record.pending_loadvm.as_deref() == Some(metadata.qemu_snapshot.as_str()) {
            return Err(CoreError::InvalidHandle(
                "checkpoint is selected for the next machine start".to_string(),
            ));
        }

        self.run_qemu_img_snapshot("-d", &metadata.qemu_snapshot, &record.disk_path)
            .await?;
        let checkpoint = record.checkpoints.remove(index);
        self.write_record(&record).await?;
        tokio::fs::remove_dir_all(&checkpoint_dir)
            .await
            .map_err(CoreError::io(
                "remove checkpoint directory",
                &checkpoint_dir,
            ))?;
        Ok(checkpoint)
    }

    pub async fn restore_checkpoint(
        &mut self,
        checkpoint: &CheckpointRef,
    ) -> CoreResult<MachineHandle> {
        let state_dir = PathBuf::from(&checkpoint.state_dir);
        self.validate_state_dir_path(&state_dir, None)?;
        let record_path = state_dir.join(MACHINE_FILE);
        let bytes = tokio::fs::read(&record_path)
            .await
            .map_err(CoreError::io("read machine record", &record_path))?;
        let mut record: MachineRecord = serde_json::from_slice(&bytes)?;
        if record.handle.id != checkpoint.machine_id {
            return Err(CoreError::InvalidHandle(
                "checkpoint machine id does not match machine record".to_string(),
            ));
        }
        let state = self.status_machine(&record.handle).await?;
        if matches!(
            state,
            MachineState::Running | MachineState::Paused | MachineState::Failed
        ) {
            return Err(CoreError::InvalidState {
                machine: record.handle.name.clone(),
                actual: match state {
                    MachineState::Running => "running",
                    MachineState::Paused => "paused",
                    _ => "failed",
                },
                expected: "stopped",
            });
        }
        if !record
            .checkpoints
            .iter()
            .any(|item| item.id == checkpoint.id)
        {
            return Err(CoreError::InvalidHandle(
                "checkpoint is not registered on the machine".to_string(),
            ));
        }

        let checkpoint_dir = state_dir.join(CHECKPOINT_DIR).join(&checkpoint.id);
        let metadata_path = checkpoint_dir.join("checkpoint.json");
        let metadata_bytes = tokio::fs::read(&metadata_path)
            .await
            .map_err(CoreError::io("read checkpoint metadata", &metadata_path))?;
        let metadata: CheckpointMetadata = serde_json::from_slice(&metadata_bytes)?;

        if !metadata.vm_state_included {
            self.run_qemu_img_snapshot("-a", &metadata.qemu_snapshot, &record.disk_path)
                .await?;
            record.pending_loadvm = None;
        }
        if metadata.firmware_vars_included {
            let target = record
                .ovmf_vars_path
                .clone()
                .unwrap_or_else(|| state_dir.join(OVMF_VARS_FILE));
            replace_file_atomically(&checkpoint_dir.join(OVMF_VARS_FILE), &target).await?;
            record.ovmf_vars_path = Some(target);
        }
        if metadata.tpm_state_included {
            let target = record
                .tpm_state_dir
                .clone()
                .unwrap_or_else(|| state_dir.join(TPM_DIR));
            replace_dir_contents(&checkpoint_dir.join(TPM_DIR), &target).await?;
            record.tpm_state_dir = Some(target);
        }
        if metadata.vm_state_included {
            record.pending_loadvm = Some(metadata.qemu_snapshot);
        }
        record.handle.state = MachineState::Stopped;
        record.pid = None;
        self.write_record(&record).await?;
        Ok(record.handle)
    }

    pub async fn capture_frame(&mut self, machine: &MachineHandle) -> CoreResult<FrameRef> {
        let record = self.load_record(machine).await?;
        let qmp = self.qmp_for_machine(machine).await?;
        let frame_dir = PathBuf::from(&record.handle.state_dir).join(FRAME_DIR);
        create_private_dir(&frame_dir, "create frame directory").await?;
        let frame_path = frame_dir.join(format!("frame-{}-{}.ppm", unix_ms(), Uuid::new_v4()));
        let frame_path_string = path_to_string(&frame_path)?;
        qmp.execute("screendump", Some(json!({ "filename": frame_path_string })))
            .await?;
        set_private_file(&frame_path).await?;
        let bytes = tokio::fs::read(&frame_path)
            .await
            .map_err(CoreError::io("read frame", &frame_path))?;
        let dimensions = parse_ppm_dimensions(&bytes)?;
        write_json(
            &PathBuf::from(&record.handle.state_dir).join(SCREEN_FILE),
            &dimensions,
        )
        .await?;
        Ok(FrameRef {
            sha256: sha256_hex(&bytes),
            width: dimensions.width,
            height: dimensions.height,
            captured_unix_ms: unix_ms(),
            blob_path: frame_path.to_string_lossy().into_owned(),
        })
    }

    pub async fn send_control_event(
        &mut self,
        machine: &MachineHandle,
        event: &ControlEvent,
    ) -> CoreResult<()> {
        match event {
            ControlEvent::Click { x, y, button } => self.click(machine, *x, *y, button).await,
            ControlEvent::Move { x, y } => self.move_pointer(machine, *x, *y).await,
            ControlEvent::Type { text } => self.type_text(machine, text).await,
            ControlEvent::Key { chord } => self.send_key_chord(machine, chord).await,
            ControlEvent::Scroll { direction, steps } => {
                self.scroll(machine, direction, *steps).await
            }
            ControlEvent::Invoke { .. } | ControlEvent::SetText { .. } => Err(
                CoreError::Unsupported("semantic UI control requires the guest helper"),
            ),
        }
    }

    async fn click(
        &mut self,
        machine: &MachineHandle,
        x: u32,
        y: u32,
        button: &MouseButton,
    ) -> CoreResult<()> {
        let qmp = self.qmp_for_machine(machine).await?;
        let dimensions = self.screen_dimensions(machine).await?;
        let (x, y) = scale_pointer(x, y, &dimensions);
        let button = match button {
            MouseButton::Left => "left",
            MouseButton::Middle => "middle",
            MouseButton::Right => "right",
        };
        // QEMU syncs input only at the end of each input-send-event call.
        qmp.execute(
            "input-send-event",
            Some(json!({
                "events": [
                    {"type": "abs", "data": {"axis": "x", "value": x}},
                    {"type": "abs", "data": {"axis": "y", "value": y}}
                ]
            })),
        )
        .await?;
        qmp.execute(
            "input-send-event",
            Some(json!({
                "events": [
                    {"type": "btn", "data": {"button": button, "down": true}}
                ]
            })),
        )
        .await?;
        sleep(CLICK_HOLD_TIME).await;
        qmp.execute(
            "input-send-event",
            Some(json!({
                "events": [
                    {"type": "btn", "data": {"button": button, "down": false}}
                ]
            })),
        )
        .await?;
        Ok(())
    }

    async fn move_pointer(&mut self, machine: &MachineHandle, x: u32, y: u32) -> CoreResult<()> {
        let qmp = self.qmp_for_machine(machine).await?;
        let dimensions = self.screen_dimensions(machine).await?;
        let (x, y) = scale_pointer(x, y, &dimensions);
        qmp.execute(
            "input-send-event",
            Some(json!({
                "events": [
                    {"type": "abs", "data": {"axis": "x", "value": x}},
                    {"type": "abs", "data": {"axis": "y", "value": y}}
                ]
            })),
        )
        .await?;
        Ok(())
    }

    async fn type_text(&mut self, machine: &MachineHandle, text: &str) -> CoreResult<()> {
        let qmp = self.qmp_for_machine(machine).await?;
        let characters = text
            .chars()
            .map(ascii_qcode_keys)
            .collect::<Option<Vec<_>>>()
            .ok_or(CoreError::Unsupported(
                "virtual keyboard input supports printable ASCII plus newline and tab",
            ))?;
        for keys in characters {
            send_qmp_keys(&qmp, &keys).await?;
        }
        Ok(())
    }

    async fn send_key_chord(&mut self, machine: &MachineHandle, chord: &str) -> CoreResult<()> {
        let qmp = self.qmp_for_machine(machine).await?;
        let mut keys = Vec::new();
        for part in chord.split('+') {
            let normalized = part.trim().to_ascii_lowercase();
            if normalized.is_empty() {
                return Err(CoreError::Unsupported("empty key in chord"));
            }
            if normalized.len() == 1 {
                let character = normalized
                    .chars()
                    .next()
                    .ok_or(CoreError::Unsupported("empty key in chord"))?;
                if let Some(single_key) = ascii_qcode_keys(character) {
                    keys.extend(single_key);
                    continue;
                }
            }
            keys.push(
                qcode_key_name(&normalized)
                    .ok_or(CoreError::Unsupported("unknown key name in chord"))?,
            );
        }
        send_qmp_keys(&qmp, &keys).await
    }

    async fn scroll(
        &mut self,
        machine: &MachineHandle,
        direction: &ScrollDirection,
        steps: u16,
    ) -> CoreResult<()> {
        let qmp = self.qmp_for_machine(machine).await?;
        let button = match direction {
            ScrollDirection::Up => "wheel-up",
            ScrollDirection::Down => "wheel-down",
        };
        for _ in 0..steps {
            qmp.execute(
                "input-send-event",
                Some(json!({
                    "events": [
                        {"type": "btn", "data": {"button": button, "down": true}},
                        {"type": "btn", "data": {"button": button, "down": false}}
                    ]
                })),
            )
            .await?;
        }
        Ok(())
    }

    async fn screen_dimensions(&mut self, machine: &MachineHandle) -> CoreResult<FrameDimensions> {
        let path = PathBuf::from(&machine.state_dir).join(SCREEN_FILE);
        match tokio::fs::read(&path).await {
            Ok(bytes) => Ok(serde_json::from_slice(&bytes)?),
            Err(_) => Ok(FrameDimensions {
                width: 1024,
                height: 768,
            }),
        }
    }

    async fn qmp_for_machine(&mut self, machine: &MachineHandle) -> CoreResult<QmpClient> {
        let record = self.load_record(machine).await?;
        if let Some(running) = self.running.get(&record.handle.id) {
            return Ok(running.qmp.clone());
        }
        Ok(QmpClient::connect(&record.qmp_socket_path).await?)
    }

    async fn qemu_args(&self, record: &mut MachineRecord) -> CoreResult<Vec<String>> {
        let mut args = vec![
            "-name".to_string(),
            record.handle.name.clone(),
            "-machine".to_string(),
            if record.spec.boot == BootMode::Uefi {
                "q35,smm=on,accel=kvm".to_string()
            } else {
                "q35,accel=kvm".to_string()
            },
            "-cpu".to_string(),
            "host".to_string(),
            "-m".to_string(),
            record.spec.memory_mb.to_string(),
            "-smp".to_string(),
            record.spec.cpus.to_string(),
            "-display".to_string(),
            "none".to_string(),
            "-rtc".to_string(),
            "base=localtime".to_string(),
            "-serial".to_string(),
            "none".to_string(),
            "-monitor".to_string(),
            "none".to_string(),
            "-qmp".to_string(),
            format!(
                "unix:{},server=on,wait=off",
                qemu_option_escape(&path_to_string(&record.qmp_socket_path)?)
            ),
            "-pidfile".to_string(),
            path_to_string(&PathBuf::from(&record.handle.state_dir).join("qemu.pid"))?,
            "-device".to_string(),
            "virtio-tablet-pci".to_string(),
        ];

        if record.spec.boot == BootMode::Uefi {
            let (code, vars_template) = self.uefi_firmware().await?;
            let vars_path = PathBuf::from(&record.handle.state_dir).join(OVMF_VARS_FILE);
            if !tokio::fs::try_exists(&vars_path)
                .await
                .map_err(CoreError::io("check firmware vars", &vars_path))?
            {
                tokio::fs::copy(&vars_template, &vars_path)
                    .await
                    .map_err(CoreError::io("copy firmware vars", &vars_path))?;
                set_private_file(&vars_path).await?;
            }
            record.ovmf_vars_path = Some(vars_path.clone());
            args.extend([
                "-global".to_string(),
                "driver=cfi.pflash01,property=secure,value=on".to_string(),
                "-drive".to_string(),
                format!(
                    "if=pflash,format=raw,unit=0,readonly=on,file={}",
                    qemu_option_escape(&path_to_string(&code)?)
                ),
                "-drive".to_string(),
                format!(
                    "if=pflash,format=raw,unit=1,file={}",
                    qemu_option_escape(&path_to_string(&vars_path)?)
                ),
            ]);
        }

        args.extend([
            "-drive".to_string(),
            format!(
                "file={},format=qcow2,if=virtio,node-name=winlane-system",
                qemu_option_escape(&path_to_string(&record.disk_path)?)
            ),
        ]);
        if let Some(iso_path) = &record.spec.iso_path {
            args.extend(["-cdrom".to_string(), iso_path.clone()]);
        }
        match record.spec.network {
            NetworkMode::Isolated => args.extend(["-nic".to_string(), "none".to_string()]),
            NetworkMode::Nat => args.extend([
                "-netdev".to_string(),
                "user,id=net0".to_string(),
                "-device".to_string(),
                "virtio-net-pci,netdev=net0".to_string(),
            ]),
        }
        if record.spec.tpm {
            let tpm_socket = PathBuf::from(&record.handle.state_dir).join(TPM_SOCKET_FILE);
            args.extend([
                "-chardev".to_string(),
                format!(
                    "socket,id=chrtpm,path={}",
                    qemu_option_escape(&path_to_string(&tpm_socket)?)
                ),
                "-tpmdev".to_string(),
                "emulator,id=tpm0,chardev=chrtpm".to_string(),
                "-device".to_string(),
                "tpm-tis,tpmdev=tpm0".to_string(),
            ]);
        }
        if record.spec.guest_agent {
            let qga_socket = record
                .qga_socket_path
                .clone()
                .unwrap_or_else(|| PathBuf::from(&record.handle.state_dir).join(QGA_SOCKET_FILE));
            validate_socket_path(&qga_socket)?;
            if tokio::fs::try_exists(&qga_socket)
                .await
                .map_err(CoreError::io("check guest agent socket", &qga_socket))?
            {
                tokio::fs::remove_file(&qga_socket)
                    .await
                    .map_err(CoreError::io(
                        "remove stale guest agent socket",
                        &qga_socket,
                    ))?;
            }
            record.qga_socket_path = Some(qga_socket.clone());
            args.extend([
                "-device".to_string(),
                "virtio-serial-pci".to_string(),
                "-chardev".to_string(),
                format!(
                    "socket,id=qga0,path={},server=on,wait=off",
                    qemu_option_escape(&path_to_string(&qga_socket)?)
                ),
                "-device".to_string(),
                "virtserialport,chardev=qga0,name=org.qemu.guest_agent.0".to_string(),
            ]);
        }
        if record.pending_loadvm.is_some() {
            args.push("-S".to_string());
        }
        Ok(args)
    }

    async fn uefi_firmware(&self) -> CoreResult<(PathBuf, PathBuf)> {
        if let (Some(code), Some(vars)) = (&self.ovmf_code, &self.ovmf_vars_template) {
            return Ok((code.clone(), vars.clone()));
        }
        let code_candidates = [
            "/usr/share/OVMF/OVMF_CODE_4M.secboot.fd",
            "/usr/share/OVMF/OVMF_CODE.secboot.fd",
        ];
        let vars_candidates = [
            "/usr/share/OVMF/OVMF_VARS_4M.ms.fd",
            "/usr/share/OVMF/OVMF_VARS.ms.fd",
        ];
        let mut code = None;
        for candidate in code_candidates {
            let path = PathBuf::from(candidate);
            if tokio::fs::try_exists(&path)
                .await
                .map_err(CoreError::io("check OVMF code", &path))?
            {
                code = Some(path);
                break;
            }
        }
        let mut vars = None;
        for candidate in vars_candidates {
            let path = PathBuf::from(candidate);
            if tokio::fs::try_exists(&path)
                .await
                .map_err(CoreError::io("check OVMF vars", &path))?
            {
                vars = Some(path);
                break;
            }
        }
        match (code, vars) {
            (Some(code), Some(vars)) => Ok((code, vars)),
            _ => Err(CoreError::Unsupported(
                "UEFI boot requires OVMF firmware files on the host",
            )),
        }
    }

    async fn start_swtpm(&self, record: &MachineRecord) -> CoreResult<Child> {
        let state_dir = record
            .tpm_state_dir
            .clone()
            .unwrap_or_else(|| PathBuf::from(&record.handle.state_dir).join(TPM_DIR));
        create_private_dir(&state_dir, "create tpm state directory").await?;
        let socket = PathBuf::from(&record.handle.state_dir).join(TPM_SOCKET_FILE);
        if tokio::fs::try_exists(&socket)
            .await
            .map_err(CoreError::io("check tpm socket", &socket))?
        {
            tokio::fs::remove_file(&socket)
                .await
                .map_err(CoreError::io("remove stale tpm socket", &socket))?;
        }
        validate_socket_path(&socket)?;
        let tpm_state = path_to_string(&state_dir)?;
        let tpm_socket = path_to_string(&socket)?;
        if tpm_state.contains(',') || tpm_socket.contains(',') {
            return Err(CoreError::Unsupported(
                "TPM state paths containing commas are not supported",
            ));
        }
        let log_path = PathBuf::from(&record.handle.state_dir).join(SWTPM_LOG_FILE);
        let log_stdout = append_log_stdio(&log_path, "open swtpm log").await?;
        let log_stderr = append_log_stdio(&log_path, "open swtpm log").await?;
        let mut child = Command::new(&self.swtpm_binary)
            .arg("socket")
            .arg(format!("--tpmstate=dir={tpm_state},mode=0600"))
            .arg(format!("--ctrl=type=unixio,path={tpm_socket},mode=0600"))
            .arg("--tpm2")
            .stdin(Stdio::null())
            .stdout(log_stdout)
            .stderr(log_stderr)
            .kill_on_drop(true)
            .spawn()
            .map_err(CoreError::io("spawn swtpm", self.swtpm_binary.clone()))?;
        for _ in 0..50 {
            if tokio::fs::try_exists(&socket)
                .await
                .map_err(CoreError::io("check tpm socket", &socket))?
            {
                set_private_file(&socket).await?;
                return Ok(child);
            }
            sleep(Duration::from_millis(50)).await;
        }
        let _ = child.kill().await;
        Err(CoreError::Timeout("swtpm control socket"))
    }

    async fn qcow2_virtual_size_gb(&self, backing_path: &Path) -> CoreResult<u32> {
        let output = Command::new(&self.qemu_img_binary)
            .arg("info")
            .arg("--output=json")
            .arg(backing_path)
            .output()
            .await
            .map_err(CoreError::io(
                "run qemu-img info for base image",
                backing_path,
            ))?;
        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(CoreError::Qemu(format!(
                "qemu-img info exited with {}: {}",
                output.status,
                stderr.trim()
            )));
        }
        let info: Value = serde_json::from_slice(&output.stdout).map_err(|error| {
            CoreError::Qemu(format!("qemu-img info output was malformed JSON: {error}"))
        })?;
        if info.get("format").and_then(Value::as_str) != Some("qcow2") {
            return Err(CoreError::Qemu(
                "qemu-img info did not report a qcow2 base image".to_string(),
            ));
        }
        let virtual_size = info
            .get("virtual-size")
            .and_then(Value::as_u64)
            .filter(|size| *size > 0)
            .ok_or_else(|| {
                CoreError::Qemu("qemu-img info output missing positive virtual-size".to_string())
            })?;
        let gib = 1024_u64 * 1024 * 1024;
        let size_gb = virtual_size.div_ceil(gib);
        u32::try_from(size_gb).map_err(|_| {
            CoreError::Qemu("base image virtual size exceeds supported disk_gb range".to_string())
        })
    }

    async fn run_qemu_img_create(
        &self,
        disk_path: &Path,
        backing_path: Option<&Path>,
        disk_gb: u32,
    ) -> CoreResult<()> {
        let mut command = Command::new(&self.qemu_img_binary);
        command.arg("create").arg("-f").arg("qcow2");
        if let Some(backing_path) = backing_path {
            command.arg("-b").arg(backing_path).arg("-F").arg("qcow2");
        }
        command.arg(disk_path).arg(format!("{disk_gb}G"));
        run_quiet_command(&mut command, "qemu-img create").await
    }

    async fn run_qemu_img_convert(&self, source_path: &Path, disk_path: &Path) -> CoreResult<()> {
        let mut command = Command::new(&self.qemu_img_binary);
        command
            .arg("convert")
            .arg("-f")
            .arg("qcow2")
            .arg("-O")
            .arg("qcow2")
            .arg(source_path)
            .arg(disk_path);
        run_quiet_command(&mut command, "qemu-img convert").await
    }

    async fn run_qemu_img_snapshot(
        &self,
        action: &str,
        snapshot: &str,
        disk_path: &Path,
    ) -> CoreResult<()> {
        let mut command = Command::new(&self.qemu_img_binary);
        command
            .arg("snapshot")
            .arg(action)
            .arg(snapshot)
            .arg(disk_path);
        run_quiet_command(&mut command, "qemu-img snapshot").await
    }

    async fn load_record(&self, machine: &MachineHandle) -> CoreResult<MachineRecord> {
        let state_dir = PathBuf::from(&machine.state_dir);
        self.validate_state_dir_path(&state_dir, Some(&machine.name))?;
        let path = state_dir.join(MACHINE_FILE);
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(CoreError::io("read machine record", &path))?;
        let record: MachineRecord = serde_json::from_slice(&bytes)?;
        if record.handle.id != machine.id
            || record.handle.name != machine.name
            || record.handle.backend != machine.backend
            || record.handle.state_dir != machine.state_dir
        {
            return Err(CoreError::InvalidHandle(
                "machine id, name, backend, or state directory does not match the state directory record".to_string(),
            ));
        }
        Ok(record)
    }

    async fn write_record(&self, record: &MachineRecord) -> CoreResult<()> {
        write_json(
            &PathBuf::from(&record.handle.state_dir).join(MACHINE_FILE),
            record,
        )
        .await
    }

    fn validate_state_dir_path(
        &self,
        state_dir: &Path,
        expected_name: Option<&str>,
    ) -> CoreResult<()> {
        let parent = state_dir.parent().ok_or_else(|| {
            CoreError::InvalidHandle("machine state directory has no parent".to_string())
        })?;
        if parent != self.root_dir {
            return Err(CoreError::InvalidHandle(
                "machine state directory is outside the backend root".to_string(),
            ));
        }
        if let Some(expected_name) = expected_name {
            let actual_name = state_dir
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| {
                    CoreError::InvalidHandle("machine state directory name is invalid".to_string())
                })?;
            if actual_name != expected_name {
                return Err(CoreError::InvalidHandle(
                    "machine state directory does not match the machine name".to_string(),
                ));
            }
        }
        Ok(())
    }
}

async fn checkpoint_catalog_entry(
    dir: &Path,
    id: String,
    registered: Option<CheckpointRef>,
    machine_state: &MachineState,
    machine_id: &str,
    machine_state_dir: &str,
) -> serde_json::Value {
    let metadata_path = dir.join("checkpoint.json");
    let metadata = tokio::fs::read(&metadata_path)
        .await
        .ok()
        .and_then(|bytes| serde_json::from_slice::<CheckpointMetadata>(&bytes).ok());
    let size = checkpoint_dir_bytes(dir).await;
    let valid = metadata.as_ref().is_some_and(|metadata| {
        metadata.checkpoint.id == id
            && metadata.checkpoint.machine_id == machine_id
            && metadata.backend == "qemu"
    });
    let stopped = matches!(machine_state, MachineState::Stopped);
    json!({
        "id": id,
        "machine_id": machine_id,
        "state_dir": machine_state_dir,
        "sha256": metadata
            .as_ref()
            .and_then(|metadata| metadata.checkpoint.sha256.clone())
            .or_else(|| registered.as_ref().and_then(|checkpoint| checkpoint.sha256.clone())),
        "label": metadata.as_ref().map(|metadata| metadata.label.clone()),
        "created_unix_ms": metadata.as_ref().map(|metadata| metadata.checkpoint.created_unix_ms).or_else(|| registered.as_ref().map(|checkpoint| checkpoint.created_unix_ms)),
        "kind": metadata.as_ref().map(|metadata| if metadata.vm_state_included { "running" } else { "stopped" }),
        "state_dir_bytes": size,
        "registered": registered.is_some(),
        "restorable": registered.is_some() && valid && stopped,
        "error": if metadata.is_none() { Some("checkpoint metadata is missing or corrupt") } else if !valid { Some("checkpoint metadata does not match this machine") } else { None },
    })
}

async fn checkpoint_dir_bytes(dir: &Path) -> Option<u64> {
    let mut total = 0_u64;
    let mut stack = vec![dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let mut read = tokio::fs::read_dir(&path).await.ok()?;
        loop {
            let entry = match read.next_entry().await {
                Ok(Some(entry)) => entry,
                Ok(None) => break,
                Err(_) => return None,
            };
            let kind = entry.file_type().await.ok()?;
            if kind.is_dir() {
                stack.push(entry.path());
            } else if kind.is_file() {
                total += entry.metadata().await.ok()?.len();
            }
        }
    }
    Some(total)
}

impl MachineBackend for QemuBackend {
    async fn create(&mut self, spec: &MachineSpec) -> ContractResult<MachineHandle> {
        self.create_machine(spec).await.map_err(Into::into)
    }

    async fn start(&mut self, machine: &MachineHandle) -> ContractResult<()> {
        self.start_machine(machine).await.map_err(Into::into)
    }

    async fn stop(&mut self, machine: &MachineHandle) -> ContractResult<()> {
        self.stop_machine(machine).await.map_err(Into::into)
    }

    async fn status(&mut self, machine: &MachineHandle) -> ContractResult<MachineState> {
        self.status_machine(machine).await.map_err(Into::into)
    }

    async fn clone_machine(
        &mut self,
        machine: &MachineHandle,
        name: &str,
    ) -> ContractResult<MachineHandle> {
        self.clone_machine(machine, name).await.map_err(Into::into)
    }

    async fn destroy(&mut self, machine: &MachineHandle) -> ContractResult<()> {
        self.destroy_machine(machine).await.map_err(Into::into)
    }

    async fn checkpoint(
        &mut self,
        machine: &MachineHandle,
        label: &str,
    ) -> ContractResult<CheckpointRef> {
        self.checkpoint_machine(machine, label)
            .await
            .map_err(Into::into)
    }

    async fn restore(&mut self, checkpoint: &CheckpointRef) -> ContractResult<MachineHandle> {
        self.restore_checkpoint(checkpoint)
            .await
            .map_err(Into::into)
    }

    async fn delete_checkpoint(
        &mut self,
        machine: &MachineHandle,
        checkpoint_id: &str,
    ) -> ContractResult<CheckpointRef> {
        self.delete_checkpoint(machine, checkpoint_id)
            .await
            .map_err(Into::into)
    }
}

impl FrameSource for QemuBackend {
    async fn capture_frame(&mut self, machine: &MachineHandle) -> ContractResult<FrameRef> {
        self.capture_frame(machine).await.map_err(Into::into)
    }
}

async fn query_machine_state(qmp: &QmpClient) -> CoreResult<MachineState> {
    let value = qmp.execute("query-status", None).await?;
    let status = value
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or("unknown");
    Ok(match status {
        "running" => MachineState::Running,
        "paused" | "suspended" => MachineState::Paused,
        "internal-error" | "io-error" | "guest-panicked" => MachineState::Failed,
        _ => MachineState::Running,
    })
}

async fn connect_qmp_with_retry(path: &Path) -> CoreResult<QmpClient> {
    let mut last_error = None;
    for _ in 0..100 {
        if tokio::fs::try_exists(path)
            .await
            .map_err(CoreError::io("check qmp socket", path))?
        {
            set_private_file(path).await?;
        }
        match QmpClient::connect(path).await {
            Ok(client) => return Ok(client),
            Err(error) => {
                last_error = Some(error);
                sleep(Duration::from_millis(50)).await;
            }
        }
    }
    match last_error {
        Some(error) => Err(error.into()),
        None => Err(CoreError::Timeout("QMP socket")),
    }
}

async fn wait_for_snapshot_job(qmp: &QmpClient, job_id: &str) -> CoreResult<()> {
    let started = std::time::Instant::now();
    loop {
        let jobs = qmp.execute("query-jobs", None).await?;
        let job = jobs.as_array().and_then(|jobs| {
            jobs.iter()
                .find(|job| job.get("id").and_then(Value::as_str) == Some(job_id))
        });
        if let Some(job) = job {
            if let Some(error) = job.get("error").and_then(Value::as_str) {
                return Err(CoreError::Qemu(format!("snapshot job failed: {error}")));
            }
            if job.get("status").and_then(Value::as_str) == Some("concluded") {
                let _ = qmp
                    .execute("job-dismiss", Some(json!({ "id": job_id })))
                    .await;
                return Ok(());
            }
        }
        if started.elapsed() > Duration::from_secs(300) {
            return Err(CoreError::Timeout("snapshot job completion"));
        }
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn shutdown_running_machine(running: &mut RunningMachine) -> CoreResult<()> {
    let _ = running.qmp.execute("system_powerdown", None).await;
    if wait_for_child(&mut running.child, Duration::from_secs(45)).await? {
        shutdown_swtpm(&mut running.swtpm).await;
        return Ok(());
    }
    let _ = running.qmp.execute("quit", None).await;
    if !wait_for_child(&mut running.child, Duration::from_secs(2)).await? {
        running
            .child
            .kill()
            .await
            .map_err(CoreError::io("kill qemu process", "qemu-system-x86_64"))?;
    }
    shutdown_swtpm(&mut running.swtpm).await;
    Ok(())
}

async fn shutdown_swtpm(child: &mut Option<Child>) {
    if let Some(process) = child {
        let _ = process.kill().await;
        let _ = timeout(Duration::from_secs(2), process.wait()).await;
    }
}

async fn qemu_process_alive(pid: u32, qmp_socket_path: &Path) -> bool {
    let stat_path = format!("/proc/{pid}/stat");
    let Ok(stat) = tokio::fs::read_to_string(&stat_path).await else {
        return false;
    };
    if stat
        .rsplit_once(')')
        .and_then(|(_, rest)| rest.trim_start().chars().next())
        == Some('Z')
    {
        return false;
    }
    let cmdline_path = format!("/proc/{pid}/cmdline");
    let Ok(cmdline) = tokio::fs::read(&cmdline_path).await else {
        return false;
    };
    if cmdline.is_empty() {
        return false;
    }
    let socket_path = qmp_socket_path.to_string_lossy();
    let expected_arg = format!(
        "unix:{},server=on,wait=off",
        qemu_option_escape(&socket_path)
    );
    cmdline
        .split(|byte| *byte == 0)
        .any(|arg| arg == expected_arg.as_bytes())
}

async fn external_machine_state(record: &MachineRecord) -> MachineState {
    if let Some(pid) = machine_process_id(record).await
        && qemu_process_alive(pid, &record.qmp_socket_path).await
    {
        MachineState::Running
    } else if matches!(
        record.handle.state,
        MachineState::Running | MachineState::Paused
    ) {
        MachineState::Failed
    } else {
        record.handle.state.clone()
    }
}

async fn machine_process_id(record: &MachineRecord) -> Option<u32> {
    let pid_path = PathBuf::from(&record.handle.state_dir).join("qemu.pid");
    if let Some(pid) = tokio::fs::read_to_string(pid_path)
        .await
        .ok()
        .and_then(|pid| pid.trim().parse().ok())
    {
        return Some(pid);
    }
    record.pid
}

async fn remove_stale_socket(path: &Path) -> CoreResult<()> {
    let metadata = match tokio::fs::symlink_metadata(path).await {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(CoreError::io("inspect stale socket", path)(error)),
    };
    if metadata.file_type().is_socket() {
        tokio::fs::remove_file(path)
            .await
            .map_err(CoreError::io("remove stale socket", path))?;
    }
    Ok(())
}

async fn external_qmp_is_gone(path: &Path) -> CoreResult<bool> {
    if !tokio::fs::try_exists(path)
        .await
        .map_err(CoreError::io("check external qmp socket", path))?
    {
        return Ok(true);
    }
    Ok(
        match timeout(Duration::from_millis(250), QmpClient::connect(path)).await {
            Err(_) | Ok(Err(_)) => true,
            Ok(Ok(_)) => false,
        },
    )
}

async fn external_machine_is_gone(path: &Path, pid: Option<u32>) -> CoreResult<bool> {
    if !external_qmp_is_gone(path).await? {
        return Ok(false);
    }
    if let Some(pid) = pid
        && qemu_process_alive(pid, path).await
    {
        return Ok(false);
    }
    Ok(true)
}

async fn wait_for_external_qmp_exit(path: &Path, pid: Option<u32>) -> CoreResult<bool> {
    for _ in 0..50 {
        if external_machine_is_gone(path, pid).await? {
            return Ok(true);
        }
        sleep(Duration::from_millis(100)).await;
    }
    external_machine_is_gone(path, pid).await
}

async fn wait_for_child(child: &mut Child, duration: Duration) -> CoreResult<bool> {
    match timeout(duration, child.wait()).await {
        Ok(Ok(_)) => Ok(true),
        Ok(Err(error)) => Err(CoreError::io("wait for qemu process", "qemu")(error)),
        Err(_) => Ok(false),
    }
}

async fn append_log_stdio(path: &Path, operation: &'static str) -> CoreResult<Stdio> {
    let file = tokio::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        .open(path)
        .await
        .map_err(CoreError::io(operation, path))?;
    Ok(Stdio::from(file.into_std().await))
}

async fn tail_log(path: &Path, max_lines: usize) -> CoreResult<Value> {
    if !tokio::fs::try_exists(path)
        .await
        .map_err(CoreError::io("check machine log", path))?
    {
        return Ok(json!({
            "path": path,
            "present": false,
            "lines": [],
            "truncated": false,
        }));
    }
    let mut file = tokio::fs::File::open(path)
        .await
        .map_err(CoreError::io("open machine log", path))?;
    let len = file
        .metadata()
        .await
        .map_err(CoreError::io("read machine log metadata", path))?
        .len();
    let start = len.saturating_sub(MAX_LOG_TAIL_BYTES);
    file.seek(std::io::SeekFrom::Start(start))
        .await
        .map_err(CoreError::io("seek machine log", path))?;
    let mut bytes = Vec::new();
    file.take(len - start)
        .read_to_end(&mut bytes)
        .await
        .map_err(CoreError::io("read machine log", path))?;
    let text = String::from_utf8_lossy(&bytes);
    let mut lines = text.lines().collect::<Vec<_>>();
    if start > 0 && !lines.is_empty() {
        lines.remove(0);
    }
    let omitted_lines = lines.len().saturating_sub(max_lines);
    let lines = lines
        .into_iter()
        .skip(omitted_lines)
        .map(str::to_string)
        .collect::<Vec<_>>();
    Ok(json!({
        "path": path,
        "present": true,
        "lines": lines,
        "truncated": start > 0 || omitted_lines > 0,
    }))
}

async fn run_quiet_command(command: &mut Command, operation: &'static str) -> CoreResult<()> {
    let program = command
        .as_std()
        .get_program()
        .to_string_lossy()
        .into_owned();
    let output = command
        .output()
        .await
        .map_err(CoreError::io(operation, program.clone()))?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr);
    Err(CoreError::Qemu(format!(
        "{operation} exited with {}: {}",
        output.status,
        stderr.trim()
    )))
}

async fn write_json(path: &Path, value: &impl Serialize) -> CoreResult<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let temp_path = sibling_path(path, &format!(".tmp-{}", Uuid::new_v4().simple()));
    let result = async {
        tokio::fs::write(&temp_path, bytes)
            .await
            .map_err(CoreError::io("write json", &temp_path))?;
        set_private_file(&temp_path).await?;
        tokio::fs::rename(&temp_path, path)
            .await
            .map_err(CoreError::io("replace json", path))
    }
    .await;
    if result.is_err() {
        let _ = tokio::fs::remove_file(&temp_path).await;
    }
    result
}

async fn create_machine_state_dir(path: &Path, name: &str) -> CoreResult<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.mode(0o700);
    match builder.create(path).await {
        Ok(()) => tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
            .await
            .map_err(CoreError::io("create machine state directory", path)),
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => Err(
            CoreError::InvalidHandle(format!("machine name '{name}' is already in use")),
        ),
        Err(error) => Err(CoreError::io("create machine state directory", path)(error)),
    }
}

async fn create_private_dir(path: &Path, operation: &'static str) -> CoreResult<()> {
    let mut builder = tokio::fs::DirBuilder::new();
    builder.recursive(true).mode(0o700);
    builder
        .create(path)
        .await
        .map_err(CoreError::io(operation, path))?;
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
        .await
        .map_err(CoreError::io(operation, path))
}

async fn set_private_file(path: &Path) -> CoreResult<()> {
    tokio::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))
        .await
        .map_err(CoreError::io("set private file permissions", path))
}

async fn set_private_file_if_exists(path: &Path) -> CoreResult<()> {
    if tokio::fs::try_exists(path)
        .await
        .map_err(CoreError::io("check private file", path))?
    {
        set_private_file(path).await?;
    }
    Ok(())
}

fn qemu_option_escape(value: &str) -> String {
    value.replace(',', ",,")
}

fn machine_qga_socket_path(state_dir: &Path, enabled: bool) -> CoreResult<Option<PathBuf>> {
    if !enabled {
        return Ok(None);
    }
    let path = state_dir.join(QGA_SOCKET_FILE);
    validate_socket_path(&path)?;
    Ok(Some(path))
}

fn validate_socket_path(path: &Path) -> CoreResult<()> {
    let bytes = path.as_os_str().as_encoded_bytes().len();
    if bytes >= MAX_SOCKET_PATH_BYTES {
        return Err(CoreError::InvalidHandle(format!(
            "socket path is {bytes} bytes; Unix sockets must stay below {MAX_SOCKET_PATH_BYTES}"
        )));
    }
    Ok(())
}

fn validate_checkpoint_label(label: &str) -> CoreResult<()> {
    if label.is_empty() || label.len() > 64 {
        return Err(CoreError::InvalidHandle(
            "checkpoint label must be 1-64 characters".to_string(),
        ));
    }
    if !label
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return Err(CoreError::InvalidHandle(
            "checkpoint label may contain only ASCII letters, digits, '-' and '_'".to_string(),
        ));
    }
    Ok(())
}

fn validate_checkpoint_id(checkpoint_id: &str) -> CoreResult<()> {
    if checkpoint_id.is_empty() || checkpoint_id.len() > 128 {
        return Err(CoreError::InvalidHandle(
            "checkpoint id must be 1-128 characters".to_string(),
        ));
    }
    if !checkpoint_id
        .chars()
        .all(|character| character.is_ascii_alphanumeric() || character == '-' || character == '_')
    {
        return Err(CoreError::InvalidHandle(
            "checkpoint id may contain only ASCII letters, digits, '-' and '_'".to_string(),
        ));
    }
    Ok(())
}

async fn validate_checkpoint_path(path: &Path, directory: bool) -> CoreResult<()> {
    let metadata = tokio::fs::symlink_metadata(path)
        .await
        .map_err(CoreError::io("inspect checkpoint path", path))?;
    let file_type = metadata.file_type();
    let valid = if directory {
        file_type.is_dir()
    } else {
        file_type.is_file()
    };
    if !valid {
        return Err(CoreError::InvalidHandle(format!(
            "checkpoint path is not a regular {}: {}",
            if directory { "directory" } else { "file" },
            path.display()
        )));
    }
    Ok(())
}

fn validate_checkpoint_metadata(
    metadata: &CheckpointMetadata,
    record: &MachineRecord,
    checkpoint_id: &str,
) -> CoreResult<()> {
    validate_checkpoint_id(&metadata.qemu_snapshot)?;
    if metadata.backend != "qemu"
        || metadata.checkpoint.id != checkpoint_id
        || metadata.checkpoint.machine_id != record.handle.id
        || metadata.checkpoint.state_dir != record.handle.state_dir
        || metadata.qemu_snapshot != checkpoint_id
        || !metadata.disk_state_included
    {
        return Err(CoreError::InvalidHandle(
            "checkpoint metadata does not match this machine".to_string(),
        ));
    }
    Ok(())
}

fn path_to_string(path: &Path) -> CoreResult<String> {
    path.to_str()
        .map(str::to_string)
        .ok_or_else(|| CoreError::InvalidHandle("path is not valid UTF-8".to_string()))
}

fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn parse_ppm_dimensions(bytes: &[u8]) -> CoreResult<FrameDimensions> {
    let mut position = 0;
    let magic = next_ppm_token(bytes, &mut position)
        .ok_or_else(|| CoreError::Qemu("screendump did not contain a PPM header".to_string()))?;
    if magic != b"P6" {
        return Err(CoreError::Qemu(
            "screendump was not a binary PPM frame".to_string(),
        ));
    }
    let width = next_ppm_token(bytes, &mut position)
        .and_then(|token| parse_ascii_u32(&token))
        .ok_or_else(|| CoreError::Qemu("PPM width was invalid".to_string()))?;
    let height = next_ppm_token(bytes, &mut position)
        .and_then(|token| parse_ascii_u32(&token))
        .ok_or_else(|| CoreError::Qemu("PPM height was invalid".to_string()))?;
    Ok(FrameDimensions { width, height })
}

fn next_ppm_token(bytes: &[u8], position: &mut usize) -> Option<Vec<u8>> {
    let limit = bytes.len().min(256);
    while *position < limit {
        let byte = bytes[*position];
        if byte.is_ascii_whitespace() {
            *position += 1;
            continue;
        }
        if byte == b'#' {
            while *position < limit && bytes[*position] != b'\n' {
                *position += 1;
            }
            continue;
        }
        break;
    }
    let start = *position;
    while *position < limit && !bytes[*position].is_ascii_whitespace() {
        *position += 1;
    }
    if start == *position {
        None
    } else {
        Some(bytes[start..*position].to_vec())
    }
}

fn parse_ascii_u32(bytes: &[u8]) -> Option<u32> {
    let mut value = 0u32;
    if bytes.is_empty() {
        return None;
    }
    for byte in bytes {
        if !byte.is_ascii_digit() {
            return None;
        }
        value = value.checked_mul(10)?.checked_add(u32::from(byte - b'0'))?;
    }
    Some(value)
}

fn scale_pointer(x: u32, y: u32, dimensions: &FrameDimensions) -> (i64, i64) {
    let scale_axis = |value: u32, size: u32| -> i64 {
        if size <= 1 {
            return 0;
        }
        let clamped = i64::from(value.min(size - 1));
        clamped * 32767 / i64::from(size - 1)
    };
    (
        scale_axis(x, dimensions.width),
        scale_axis(y, dimensions.height),
    )
}

async fn send_qmp_keys(qmp: &QmpClient, keys: &[&'static str]) -> CoreResult<()> {
    let keys = keys
        .iter()
        .map(|key| json!({"type": "qcode", "data": key}))
        .collect::<Vec<_>>();
    qmp.execute(
        "send-key",
        Some(json!({ "keys": keys, "hold-time": KEY_HOLD_TIME_MS })),
    )
    .await?;
    sleep(Duration::from_millis(KEY_HOLD_TIME_MS + 10)).await;
    Ok(())
}

fn qcode_key_name(name: &str) -> Option<&'static str> {
    Some(match name {
        "ctrl" | "control" => "ctrl",
        "alt" => "alt",
        "shift" => "shift",
        "meta" | "super" | "win" => "meta_l",
        "enter" | "return" => "ret",
        "escape" | "esc" => "esc",
        "space" => "spc",
        "tab" => "tab",
        "backspace" => "backspace",
        "delete" | "del" => "delete",
        "insert" | "ins" => "insert",
        "home" => "home",
        "end" => "end",
        "pageup" | "pgup" => "pgup",
        "pagedown" | "pgdn" => "pgdn",
        "up" => "up",
        "down" => "down",
        "left" => "left",
        "right" => "right",
        "f1" => "f1",
        "f2" => "f2",
        "f3" => "f3",
        "f4" => "f4",
        "f5" => "f5",
        "f6" => "f6",
        "f7" => "f7",
        "f8" => "f8",
        "f9" => "f9",
        "f10" => "f10",
        "f11" => "f11",
        "f12" => "f12",
        _ => return None,
    })
}

fn ascii_qcode_keys(character: char) -> Option<Vec<&'static str>> {
    if character.is_ascii_lowercase() {
        return Some(vec![match character {
            'a' => "a",
            'b' => "b",
            'c' => "c",
            'd' => "d",
            'e' => "e",
            'f' => "f",
            'g' => "g",
            'h' => "h",
            'i' => "i",
            'j' => "j",
            'k' => "k",
            'l' => "l",
            'm' => "m",
            'n' => "n",
            'o' => "o",
            'p' => "p",
            'q' => "q",
            'r' => "r",
            's' => "s",
            't' => "t",
            'u' => "u",
            'v' => "v",
            'w' => "w",
            'x' => "x",
            'y' => "y",
            'z' => "z",
            _ => return None,
        }]);
    }
    if character.is_ascii_uppercase() {
        let lowercase = character.to_ascii_lowercase();
        let mut keys = vec!["shift"];
        keys.extend(ascii_qcode_keys(lowercase)?);
        return Some(keys);
    }
    Some(match character {
        '0' => vec!["0"],
        '1' => vec!["1"],
        '2' => vec!["2"],
        '3' => vec!["3"],
        '4' => vec!["4"],
        '5' => vec!["5"],
        '6' => vec!["6"],
        '7' => vec!["7"],
        '8' => vec!["8"],
        '9' => vec!["9"],
        ' ' => vec!["spc"],
        '\n' | '\r' => vec!["ret"],
        '\t' => vec!["tab"],
        '-' => vec!["minus"],
        '_' => vec!["shift", "minus"],
        '=' => vec!["equal"],
        '+' => vec!["shift", "equal"],
        '[' => vec!["bracket_left"],
        '{' => vec!["shift", "bracket_left"],
        ']' => vec!["bracket_right"],
        '}' => vec!["shift", "bracket_right"],
        ';' => vec!["semicolon"],
        ':' => vec!["shift", "semicolon"],
        '\'' => vec!["apostrophe"],
        '"' => vec!["shift", "apostrophe"],
        '`' => vec!["grave_accent"],
        '~' => vec!["shift", "grave_accent"],
        '\\' => vec!["backslash"],
        '|' => vec!["shift", "backslash"],
        ',' => vec!["comma"],
        '<' => vec!["shift", "comma"],
        '.' => vec!["dot"],
        '>' => vec!["shift", "dot"],
        '/' => vec!["slash"],
        '?' => vec!["shift", "slash"],
        '!' => vec!["shift", "1"],
        '@' => vec!["shift", "2"],
        '#' => vec!["shift", "3"],
        '$' => vec!["shift", "4"],
        '%' => vec!["shift", "5"],
        '^' => vec!["shift", "6"],
        '&' => vec!["shift", "7"],
        '*' => vec!["shift", "8"],
        '(' => vec!["shift", "9"],
        ')' => vec!["shift", "0"],
        _ => return None,
    })
}

async fn copy_optional_file(source: Option<&Path>, target: &Path) -> CoreResult<bool> {
    let Some(source) = source else {
        return Ok(false);
    };
    if !tokio::fs::try_exists(source)
        .await
        .map_err(CoreError::io("check firmware state", source))?
    {
        return Ok(false);
    }
    tokio::fs::copy(source, target)
        .await
        .map_err(CoreError::io("copy firmware state", target))?;
    set_private_file(target).await?;
    Ok(true)
}

async fn copy_optional_dir(source: Option<&Path>, target: &Path) -> CoreResult<bool> {
    let Some(source) = source else {
        return Ok(false);
    };
    if !tokio::fs::try_exists(source)
        .await
        .map_err(CoreError::io("check tpm state", source))?
    {
        return Ok(false);
    }
    copy_dir_all(source, target).await?;
    Ok(true)
}

fn sibling_path(path: &Path, suffix: &str) -> PathBuf {
    let mut value = path.as_os_str().to_os_string();
    value.push(suffix);
    PathBuf::from(value)
}

async fn remove_dir_if_exists(path: &Path) -> CoreResult<()> {
    if tokio::fs::try_exists(path)
        .await
        .map_err(CoreError::io("check temporary restore directory", path))?
    {
        tokio::fs::remove_dir_all(path)
            .await
            .map_err(CoreError::io("remove temporary restore directory", path))?;
    }
    Ok(())
}

async fn replace_file_atomically(source: &Path, target: &Path) -> CoreResult<()> {
    let staging = sibling_path(target, ".winlane-new");
    if tokio::fs::try_exists(&staging)
        .await
        .map_err(CoreError::io("check staged restore file", &staging))?
    {
        tokio::fs::remove_file(&staging)
            .await
            .map_err(CoreError::io("remove stale staged restore file", &staging))?;
    }
    tokio::fs::copy(source, &staging)
        .await
        .map_err(CoreError::io("stage restored file", &staging))?;
    set_private_file(&staging).await?;
    if let Err(error) = tokio::fs::rename(&staging, target).await {
        let _ = tokio::fs::remove_file(&staging).await;
        return Err(CoreError::io("replace restored file", target)(error));
    }
    Ok(())
}

async fn replace_dir_contents(source: &Path, target: &Path) -> CoreResult<()> {
    let staging = sibling_path(target, ".winlane-new");
    let backup = sibling_path(target, ".winlane-old");
    remove_dir_if_exists(&staging).await?;
    remove_dir_if_exists(&backup).await?;
    if let Err(error) = copy_dir_all(source, &staging).await {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(error);
    }
    let target_exists = tokio::fs::try_exists(target)
        .await
        .map_err(CoreError::io("check restore target directory", target))?;
    if target_exists && let Err(error) = tokio::fs::rename(target, &backup).await {
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(CoreError::io("move aside restore target directory", target)(error));
    }
    if let Err(error) = tokio::fs::rename(&staging, target).await {
        if target_exists {
            let _ = tokio::fs::rename(&backup, target).await;
        }
        let _ = tokio::fs::remove_dir_all(&staging).await;
        return Err(CoreError::io("replace restore target directory", target)(
            error,
        ));
    }
    let _ = tokio::fs::remove_dir_all(&backup).await;
    Ok(())
}

async fn copy_dir_all(source: &Path, target: &Path) -> CoreResult<()> {
    let mut stack = vec![(source.to_path_buf(), target.to_path_buf())];
    while let Some((source_dir, target_dir)) = stack.pop() {
        create_private_dir(&target_dir, "create copied directory").await?;
        let mut entries = tokio::fs::read_dir(&source_dir)
            .await
            .map_err(CoreError::io("read copied directory", &source_dir))?;
        while let Some(entry) = entries
            .next_entry()
            .await
            .map_err(CoreError::io("iterate copied directory", &source_dir))?
        {
            let entry_path = entry.path();
            let target_path = target_dir.join(entry.file_name());
            let file_type = entry
                .file_type()
                .await
                .map_err(CoreError::io("inspect copied file", &entry_path))?;
            if file_type.is_symlink() {
                return Err(CoreError::Unsupported(
                    "symbolic links are not copied into checkpoint state",
                ));
            }
            if file_type.is_dir() {
                stack.push((entry_path, target_path));
            } else if file_type.is_file() {
                tokio::fs::copy(&entry_path, &target_path)
                    .await
                    .map_err(CoreError::io("copy checkpoint file", &target_path))?;
                set_private_file(&target_path).await?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn checkpoint_delete_spec(name: &str) -> MachineSpec {
        MachineSpec {
            name: name.into(),
            arch: CpuArch::X86_64,
            memory_mb: 512,
            cpus: 1,
            disk_gb: 16,
            iso_path: None,
            base_image_path: None,
            guest_agent: false,
            network: NetworkMode::Isolated,
            tpm: false,
            boot: BootMode::Bios,
        }
    }

    async fn backend_with_fake_qemu_img(root: &Path) -> QemuBackend {
        let fake_qemu_img = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tests/fixtures/control/fake_qemu_img.py");
        QemuBackend::new(root)
            .await
            .unwrap()
            .with_qemu_binaries("qemu-system-x86_64", fake_qemu_img)
    }

    async fn write_fake_base_image(path: &Path, virtual_size: u64) {
        tokio::fs::write(
            path,
            serde_json::to_vec(&json!({
                "format": "qcow2",
                "virtual-size": virtual_size,
            }))
            .unwrap(),
        )
        .await
        .unwrap();
    }

    fn checkpoint_delete_ref(handle: &MachineHandle, id: &str) -> CheckpointRef {
        CheckpointRef {
            id: id.into(),
            machine_id: handle.id.clone(),
            created_unix_ms: 10,
            state_dir: handle.state_dir.clone(),
            sha256: Some("x".repeat(64)),
        }
    }

    fn checkpoint_delete_record(
        handle: MachineHandle,
        state_dir: &Path,
        checkpoints: Vec<CheckpointRef>,
    ) -> MachineRecord {
        MachineRecord {
            handle: handle.clone(),
            spec: checkpoint_delete_spec(&handle.name),
            disk_path: state_dir.join(DISK_FILE),
            qmp_socket_path: state_dir.join(QMP_SOCKET_FILE),
            qga_socket_path: None,
            pid: None,
            ovmf_vars_path: None,
            tpm_state_dir: None,
            pending_loadvm: None,
            checkpoints,
        }
    }

    async fn write_checkpoint_metadata(
        state_dir: &Path,
        checkpoint: &CheckpointRef,
        vm_state_included: bool,
    ) -> PathBuf {
        let dir = state_dir.join(CHECKPOINT_DIR).join(&checkpoint.id);
        tokio::fs::create_dir_all(&dir).await.unwrap();
        let metadata = CheckpointMetadata {
            checkpoint: checkpoint.clone(),
            label: "delete".into(),
            backend: "qemu".into(),
            qemu_snapshot: checkpoint.id.clone(),
            disk_state_included: true,
            vm_state_included,
            firmware_vars_included: false,
            tpm_state_included: false,
        };
        tokio::fs::write(
            dir.join("checkpoint.json"),
            serde_json::to_vec(&metadata).unwrap(),
        )
        .await
        .unwrap();
        dir
    }

    async fn checkpoint_delete_fixture(
        root: &Path,
        state: MachineState,
        ids: &[(&str, bool)],
    ) -> (QemuBackend, MachineHandle, PathBuf) {
        let fake_qemu_img = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../..")
            .join("tests/fixtures/control/fake_qemu_img.py");
        let backend = QemuBackend::new(root)
            .await
            .unwrap()
            .with_qemu_binaries("qemu-system-x86_64", fake_qemu_img);
        let state_dir = backend.root_dir().join("builder");
        tokio::fs::create_dir(&state_dir).await.unwrap();
        tokio::fs::write(state_dir.join(DISK_FILE), b"qcow2")
            .await
            .unwrap();
        let handle = MachineHandle {
            id: "machine-1".into(),
            name: "builder".into(),
            state_dir: state_dir.to_string_lossy().into_owned(),
            backend: "qemu".into(),
            state,
        };
        let checkpoints = ids
            .iter()
            .map(|(id, _)| checkpoint_delete_ref(&handle, id))
            .collect::<Vec<_>>();
        let record = checkpoint_delete_record(handle.clone(), &state_dir, checkpoints.clone());
        tokio::fs::write(
            state_dir.join(MACHINE_FILE),
            serde_json::to_vec(&record).unwrap(),
        )
        .await
        .unwrap();
        for (checkpoint, (_, vm_state_included)) in checkpoints.iter().zip(ids.iter()) {
            write_checkpoint_metadata(&state_dir, checkpoint, *vm_state_included).await;
        }
        (backend, handle, state_dir)
    }

    fn snapshot_log(state_dir: &Path) -> String {
        std::fs::read_to_string(state_dir.join("disk.qcow2.snapshots")).unwrap_or_default()
    }

    #[tokio::test]
    async fn checkpoint_delete_removes_stopped_and_running_metadata_after_qcow2_delete() {
        let root = tempfile::tempdir().unwrap();
        let stopped_id = "checkpoint-stopped-delete";
        let running_id = "checkpoint-running-delete";
        let (mut backend, handle, state_dir) = checkpoint_delete_fixture(
            root.path(),
            MachineState::Stopped,
            &[(stopped_id, false), (running_id, true)],
        )
        .await;

        let stopped = backend
            .delete_checkpoint(&handle, stopped_id)
            .await
            .expect("delete stopped checkpoint");
        assert_eq!(stopped.id, stopped_id);
        assert!(!state_dir.join(CHECKPOINT_DIR).join(stopped_id).exists());
        assert!(state_dir.join(CHECKPOINT_DIR).join(running_id).exists());

        let running = backend
            .delete_checkpoint(&handle, running_id)
            .await
            .expect("delete running checkpoint");
        assert_eq!(running.id, running_id);
        assert!(!state_dir.join(CHECKPOINT_DIR).join(running_id).exists());
        let record_bytes = tokio::fs::read(state_dir.join(MACHINE_FILE)).await.unwrap();
        let record: MachineRecord = serde_json::from_slice(&record_bytes).unwrap();
        assert!(record.checkpoints.is_empty());

        let actions = snapshot_log(&state_dir)
            .lines()
            .map(|line| serde_json::from_str::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(actions.len(), 2);
        assert_eq!(actions[0]["action"], "-d");
        assert_eq!(actions[0]["snapshot"], stopped_id);
        assert_eq!(actions[1]["action"], "-d");
        assert_eq!(actions[1]["snapshot"], running_id);
    }

    #[tokio::test]
    async fn checkpoint_delete_missing_checkpoint_does_not_call_qemu_img() {
        let root = tempfile::tempdir().unwrap();
        let (mut backend, handle, state_dir) =
            checkpoint_delete_fixture(root.path(), MachineState::Stopped, &[]).await;
        let error = backend
            .delete_checkpoint(&handle, "checkpoint-missing")
            .await
            .expect_err("missing checkpoint should fail");
        assert!(error.to_string().contains("not registered"), "{error}");
        assert!(snapshot_log(&state_dir).is_empty());
    }

    #[tokio::test]
    async fn checkpoint_delete_rejects_running_machine_before_qemu_img() {
        let root = tempfile::tempdir().unwrap();
        let id = "checkpoint-running-machine";
        let (mut backend, handle, state_dir) =
            checkpoint_delete_fixture(root.path(), MachineState::Running, &[(id, true)]).await;
        let error = backend
            .delete_checkpoint(&handle, id)
            .await
            .expect_err("running machine should reject deletion");
        assert!(error.to_string().contains("expected stopped"), "{error}");
        assert!(snapshot_log(&state_dir).is_empty());
        assert!(state_dir.join(CHECKPOINT_DIR).join(id).exists());
    }

    #[tokio::test]
    async fn checkpoint_delete_malformed_metadata_does_not_call_qemu_img() {
        let root = tempfile::tempdir().unwrap();
        let id = "checkpoint-malformed";
        let (mut backend, handle, state_dir) =
            checkpoint_delete_fixture(root.path(), MachineState::Stopped, &[(id, false)]).await;
        tokio::fs::write(
            state_dir
                .join(CHECKPOINT_DIR)
                .join(id)
                .join("checkpoint.json"),
            b"not json",
        )
        .await
        .unwrap();
        let error = backend
            .delete_checkpoint(&handle, id)
            .await
            .expect_err("malformed metadata should fail");
        assert!(matches!(error, CoreError::Serialization(_)), "{error}");
        assert!(snapshot_log(&state_dir).is_empty());
        assert!(state_dir.join(CHECKPOINT_DIR).join(id).exists());
    }

    #[tokio::test]
    async fn checkpoint_delete_qemu_failure_preserves_registry_and_directory() {
        let root = tempfile::tempdir().unwrap();
        let id = "checkpoint-faildelete";
        let (mut backend, handle, state_dir) =
            checkpoint_delete_fixture(root.path(), MachineState::Stopped, &[(id, false)]).await;
        let error = backend
            .delete_checkpoint(&handle, id)
            .await
            .expect_err("forced qemu-img failure");
        assert!(error.to_string().contains("qemu-img snapshot"), "{error}");
        assert!(state_dir.join(CHECKPOINT_DIR).join(id).exists());
        let record_bytes = tokio::fs::read(state_dir.join(MACHINE_FILE)).await.unwrap();
        let record: MachineRecord = serde_json::from_slice(&record_bytes).unwrap();
        assert_eq!(record.checkpoints.len(), 1);
        assert_eq!(record.checkpoints[0].id, id);
    }

    #[tokio::test]
    async fn checkpoint_delete_rejects_traversal_ids_and_symlinked_metadata() {
        let root = tempfile::tempdir().unwrap();
        let id = "checkpoint-symlink";
        let (mut backend, handle, state_dir) =
            checkpoint_delete_fixture(root.path(), MachineState::Stopped, &[(id, false)]).await;
        assert!(
            backend
                .delete_checkpoint(&handle, "../checkpoint-outside")
                .await
                .is_err()
        );
        assert!(snapshot_log(&state_dir).is_empty());

        let metadata_path = state_dir
            .join(CHECKPOINT_DIR)
            .join(id)
            .join("checkpoint.json");
        let outside = root.path().join("outside-metadata.json");
        tokio::fs::rename(&metadata_path, &outside).await.unwrap();
        std::os::unix::fs::symlink(&outside, &metadata_path).unwrap();
        let error = backend
            .delete_checkpoint(&handle, id)
            .await
            .expect_err("symlinked metadata should fail");
        assert!(error.to_string().contains("not a regular file"), "{error}");
        assert!(outside.exists());
        assert!(snapshot_log(&state_dir).is_empty());
    }

    #[tokio::test]
    async fn checkpoint_delete_rejects_checkpoint_selected_for_next_start() {
        let root = tempfile::tempdir().unwrap();
        let id = "checkpoint-pending-load";
        let (mut backend, handle, state_dir) =
            checkpoint_delete_fixture(root.path(), MachineState::Stopped, &[(id, true)]).await;
        let record_path = state_dir.join(MACHINE_FILE);
        let mut record: MachineRecord =
            serde_json::from_slice(&tokio::fs::read(&record_path).await.unwrap()).unwrap();
        record.pending_loadvm = Some(id.into());
        tokio::fs::write(&record_path, serde_json::to_vec(&record).unwrap())
            .await
            .unwrap();
        let error = backend
            .delete_checkpoint(&handle, id)
            .await
            .expect_err("pending load checkpoint should fail");
        assert!(error.to_string().contains("next machine start"), "{error}");
        assert!(state_dir.join(CHECKPOINT_DIR).join(id).exists());
        assert!(snapshot_log(&state_dir).is_empty());
    }

    #[test]
    fn parses_ppm_dimensions() {
        let bytes = b"P6\n# comment\n320 200\n255\npixels";
        let dimensions = parse_ppm_dimensions(bytes).expect("PPM should parse");
        assert_eq!(dimensions.width, 320);
        assert_eq!(dimensions.height, 200);
    }

    #[tokio::test]
    async fn restore_replacement_helpers_replace_file_and_directory_contents() {
        let root = tempfile::tempdir().expect("temporary restore root");
        let source_file = root.path().join("source-file");
        let target_file = root.path().join("target-file");
        std::fs::write(&source_file, b"new").expect("write source file");
        std::fs::write(&target_file, b"old").expect("write target file");
        replace_file_atomically(&source_file, &target_file)
            .await
            .expect("replace file");
        assert_eq!(
            std::fs::read(&target_file).expect("read target file"),
            b"new"
        );
        assert!(!sibling_path(&target_file, ".winlane-new").exists());

        let source_dir = root.path().join("source-dir");
        let target_dir = root.path().join("target-dir");
        std::fs::create_dir_all(source_dir.join("nested")).expect("create source dir");
        std::fs::create_dir_all(&target_dir).expect("create target dir");
        std::fs::write(source_dir.join("nested/value"), b"new").expect("write source value");
        std::fs::write(target_dir.join("obsolete"), b"old").expect("write obsolete value");
        replace_dir_contents(&source_dir, &target_dir)
            .await
            .expect("replace directory");
        assert_eq!(
            std::fs::read(target_dir.join("nested/value")).expect("read replacement value"),
            b"new"
        );
        assert!(!target_dir.join("obsolete").exists());
        assert!(!sibling_path(&target_dir, ".winlane-new").exists());
        assert!(!sibling_path(&target_dir, ".winlane-old").exists());
    }

    #[tokio::test]
    async fn json_writes_replace_contents_without_leaving_staging_files() {
        let root = tempfile::tempdir().expect("temporary json root");
        let path = root.path().join("state.json");
        write_json(&path, &json!({"version": 1}))
            .await
            .expect("write initial json");
        write_json(&path, &json!({"version": 2}))
            .await
            .expect("replace json");
        let value: Value =
            serde_json::from_str(&std::fs::read_to_string(&path).expect("read replaced json"))
                .expect("parse replaced json");
        assert_eq!(value["version"], 2);
        assert_eq!(
            std::fs::read_dir(root.path())
                .expect("read json directory")
                .count(),
            1
        );
    }

    #[test]
    fn scales_pointer_to_qemu_absolute_range() {
        let scaled = scale_pointer(
            1023,
            767,
            &FrameDimensions {
                width: 1024,
                height: 768,
            },
        );
        assert_eq!(scaled, (32767, 32767));
    }

    #[test]
    fn escapes_commas_for_qemu_socket_options() {
        assert_eq!(
            qemu_option_escape("/tmp/machine,a/qga.sock"),
            "/tmp/machine,,a/qga.sock"
        );
    }

    #[test]
    fn rejects_overlong_socket_paths() {
        let path = PathBuf::from("/").join("a".repeat(MAX_SOCKET_PATH_BYTES));
        assert!(validate_socket_path(&path).is_err());
    }

    #[tokio::test]
    async fn base_image_backing_size_raises_default_effective_disk_size() {
        let root = tempfile::tempdir().expect("temporary root");
        let mut backend = backend_with_fake_qemu_img(root.path()).await;
        let base = root.path().join("base.qcow2");
        write_fake_base_image(&base, 80_u64 * 1024 * 1024 * 1024).await;
        let mut spec = checkpoint_delete_spec("builder");
        spec.disk_gb = 64;
        spec.base_image_path = Some(base.to_string_lossy().into_owned());

        backend.create_machine(&spec).await.expect("create machine");

        let state_dir = root.path().join("builder");
        let stored_spec: Value =
            serde_json::from_slice(&tokio::fs::read(state_dir.join(SPEC_FILE)).await.unwrap())
                .unwrap();
        assert_eq!(stored_spec["disk_gb"], 80);
        let disk: Value =
            serde_json::from_slice(&tokio::fs::read(state_dir.join(DISK_FILE)).await.unwrap())
                .unwrap();
        assert_eq!(disk["size"], "80G");
    }

    #[tokio::test]
    async fn larger_explicit_disk_size_survives_smaller_backing_image() {
        let root = tempfile::tempdir().expect("temporary root");
        let mut backend = backend_with_fake_qemu_img(root.path()).await;
        let base = root.path().join("base.qcow2");
        write_fake_base_image(&base, 80_u64 * 1024 * 1024 * 1024).await;
        let mut spec = checkpoint_delete_spec("builder");
        spec.disk_gb = 128;
        spec.base_image_path = Some(base.to_string_lossy().into_owned());

        backend.create_machine(&spec).await.expect("create machine");

        let state_dir = root.path().join("builder");
        let stored_spec: Value =
            serde_json::from_slice(&tokio::fs::read(state_dir.join(SPEC_FILE)).await.unwrap())
                .unwrap();
        assert_eq!(stored_spec["disk_gb"], 128);
        let disk: Value =
            serde_json::from_slice(&tokio::fs::read(state_dir.join(DISK_FILE)).await.unwrap())
                .unwrap();
        assert_eq!(disk["size"], "128G");
    }

    #[tokio::test]
    async fn malformed_or_failed_base_image_info_fails_closed() {
        for (name, expected) in [
            ("badinfo-base.qcow2", "malformed JSON"),
            ("failinfo-base.qcow2", "qemu-img info exited"),
        ] {
            let root = tempfile::tempdir().expect("temporary root");
            let mut backend = backend_with_fake_qemu_img(root.path()).await;
            let base = root.path().join(name);
            write_fake_base_image(&base, 80_u64 * 1024 * 1024 * 1024).await;
            let mut spec = checkpoint_delete_spec("builder");
            spec.disk_gb = 64;
            spec.base_image_path = Some(base.to_string_lossy().into_owned());

            let error = backend
                .create_machine(&spec)
                .await
                .expect_err("base image info failure must reject creation");
            assert!(error.to_string().contains(expected), "{error}");
            assert!(!root.path().join("builder").exists());
        }
    }

    #[tokio::test]
    async fn list_machines_returns_valid_records_and_ignores_malformed_entries() {
        let root = tempfile::tempdir().expect("temporary root");
        let mut backend = QemuBackend::new(root.path()).await.expect("create backend");
        let state_dir = backend.root_dir().join("builder");
        tokio::fs::create_dir(&state_dir)
            .await
            .expect("create state dir");
        let handle = MachineHandle {
            id: "machine-1".to_string(),
            name: "builder".to_string(),
            state_dir: state_dir.to_string_lossy().into_owned(),
            backend: "qemu".to_string(),
            state: MachineState::Defined,
        };
        let record = MachineRecord {
            handle: handle.clone(),
            spec: MachineSpec {
                name: "builder".to_string(),
                arch: CpuArch::X86_64,
                memory_mb: 4096,
                cpus: 4,
                disk_gb: 64,
                iso_path: None,
                base_image_path: None,
                guest_agent: true,
                network: NetworkMode::Isolated,
                tpm: true,
                boot: BootMode::Uefi,
            },
            disk_path: state_dir.join(DISK_FILE),
            qmp_socket_path: state_dir.join(QMP_SOCKET_FILE),
            qga_socket_path: Some(state_dir.join(QGA_SOCKET_FILE)),
            pid: None,
            ovmf_vars_path: None,
            tpm_state_dir: None,
            pending_loadvm: None,
            checkpoints: Vec::new(),
        };
        tokio::fs::write(
            state_dir.join(MACHINE_FILE),
            serde_json::to_vec(&record).expect("encode record"),
        )
        .await
        .expect("write record");
        tokio::fs::create_dir(backend.root_dir().join("bad"))
            .await
            .expect("create malformed dir");
        tokio::fs::write(
            backend.root_dir().join("bad").join(MACHINE_FILE),
            b"not json",
        )
        .await
        .expect("write malformed record");
        tokio::fs::write(backend.root_dir().join("stray"), b"{}")
            .await
            .expect("write stray file");

        assert_eq!(
            backend.list_machines().await.expect("list machines"),
            vec![handle]
        );
    }

    #[tokio::test]
    async fn checkpoint_catalog_reports_registered_orphaned_corrupt_and_missing_entries() {
        let root = tempfile::tempdir().expect("temporary root");
        let mut backend = QemuBackend::new(root.path()).await.expect("backend");
        let state_dir = root.path().join("builder");
        tokio::fs::create_dir(&state_dir).await.expect("state dir");
        let handle = MachineHandle {
            id: "machine-1".into(),
            name: "builder".into(),
            state_dir: state_dir.to_string_lossy().into_owned(),
            backend: "qemu".into(),
            state: MachineState::Stopped,
        };
        let checkpoint = |id: &str, created: u64| CheckpointRef {
            id: id.into(),
            machine_id: handle.id.clone(),
            created_unix_ms: created,
            state_dir: handle.state_dir.clone(),
            sha256: Some("x".repeat(64)),
        };
        let record = MachineRecord {
            handle: handle.clone(),
            spec: MachineSpec {
                name: "builder".into(),
                arch: CpuArch::X86_64,
                memory_mb: 4096,
                cpus: 4,
                disk_gb: 64,
                iso_path: None,
                base_image_path: None,
                guest_agent: true,
                network: NetworkMode::Isolated,
                tpm: true,
                boot: BootMode::Uefi,
            },
            disk_path: state_dir.join(DISK_FILE),
            qmp_socket_path: state_dir.join(QMP_SOCKET_FILE),
            qga_socket_path: None,
            pid: None,
            ovmf_vars_path: None,
            tpm_state_dir: None,
            pending_loadvm: None,
            checkpoints: vec![
                checkpoint("checkpoint-a", 10),
                checkpoint("checkpoint-b", 20),
                checkpoint("checkpoint-missing", 30),
            ],
        };
        let machine_record_bytes = serde_json::to_vec(&record).unwrap();
        tokio::fs::write(state_dir.join(MACHINE_FILE), &machine_record_bytes)
            .await
            .unwrap();
        let root_dir = state_dir.join(CHECKPOINT_DIR);
        for (id, label, vm) in [
            ("checkpoint-a", "before", true),
            ("checkpoint-b", "after", false),
            ("checkpoint-orphan", "orphan", false),
        ] {
            let dir = root_dir.join(id);
            tokio::fs::create_dir_all(&dir).await.unwrap();
            let checkpoint_ref = checkpoint(id, if id.ends_with('a') { 10 } else { 20 });
            let metadata = CheckpointMetadata {
                checkpoint: checkpoint_ref,
                label: label.into(),
                backend: "qemu".into(),
                qemu_snapshot: id.into(),
                disk_state_included: true,
                vm_state_included: vm,
                firmware_vars_included: false,
                tpm_state_included: false,
            };
            tokio::fs::write(
                dir.join("checkpoint.json"),
                serde_json::to_vec(&metadata).unwrap(),
            )
            .await
            .unwrap();
        }
        tokio::fs::write(
            root_dir.join("checkpoint-b").join("checkpoint.json"),
            b"corrupt",
        )
        .await
        .unwrap();
        let foreign_dir = root_dir.join("checkpoint-foreign");
        tokio::fs::create_dir_all(&foreign_dir).await.unwrap();
        let mut foreign_checkpoint = checkpoint("checkpoint-foreign", 40);
        foreign_checkpoint.machine_id = "other-machine".into();
        let foreign_metadata = CheckpointMetadata {
            checkpoint: foreign_checkpoint,
            label: "foreign".into(),
            backend: "qemu".into(),
            qemu_snapshot: "checkpoint-foreign".into(),
            disk_state_included: true,
            vm_state_included: false,
            firmware_vars_included: false,
            tpm_state_included: false,
        };
        tokio::fs::write(
            foreign_dir.join("checkpoint.json"),
            serde_json::to_vec(&foreign_metadata).unwrap(),
        )
        .await
        .unwrap();
        let result = backend.list_checkpoints(&handle).await.expect("catalog");
        let entries = result.as_array().expect("array");
        assert_eq!(entries.len(), 5);
        let entry = |id: &str| {
            entries
                .iter()
                .find(|entry| entry["id"] == id)
                .unwrap_or_else(|| panic!("missing catalog entry {id}"))
        };
        assert_eq!(entry("checkpoint-a")["label"], "before");
        assert_eq!(entry("checkpoint-a")["kind"], "running");
        assert_eq!(entry("checkpoint-a")["registered"], true);
        assert_eq!(entry("checkpoint-a")["restorable"], true);
        assert_eq!(entry("checkpoint-b")["label"], serde_json::Value::Null);
        assert_eq!(entry("checkpoint-b")["restorable"], false);
        assert_eq!(entry("checkpoint-missing")["registered"], true);
        assert_eq!(
            entry("checkpoint-missing")["error"],
            "checkpoint directory is missing"
        );
        assert_eq!(entry("checkpoint-orphan")["label"], "orphan");
        assert_eq!(entry("checkpoint-orphan")["registered"], false);
        assert_eq!(entry("checkpoint-orphan")["restorable"], false);
        assert_eq!(
            entry("checkpoint-foreign")["error"],
            "checkpoint metadata does not match this machine"
        );
        assert_eq!(entry("checkpoint-foreign")["restorable"], false);
        assert_eq!(
            tokio::fs::read(root_dir.join("checkpoint-b").join("checkpoint.json"))
                .await
                .unwrap(),
            b"corrupt"
        );
        assert!(root_dir.join("checkpoint-orphan").exists());
        assert_eq!(
            tokio::fs::read(state_dir.join(MACHINE_FILE)).await.unwrap(),
            machine_record_bytes
        );

        let mut running_record: MachineRecord =
            serde_json::from_slice(&machine_record_bytes).expect("decode record");
        running_record.handle.state = MachineState::Running;
        tokio::fs::write(
            state_dir.join(MACHINE_FILE),
            serde_json::to_vec(&running_record).unwrap(),
        )
        .await
        .unwrap();
        let result = backend.list_checkpoints(&handle).await.expect("catalog");
        let entries = result.as_array().expect("array");
        let running_entry = entries
            .iter()
            .find(|entry| entry["id"] == "checkpoint-a")
            .expect("checkpoint entry");
        assert_eq!(running_entry["restorable"], false);
    }

    #[tokio::test]
    async fn checkpoint_catalog_empty_registry_is_empty_array() {
        let root = tempfile::tempdir().expect("temporary root");
        let mut backend = QemuBackend::new(root.path()).await.expect("backend");
        let state_dir = root.path().join("builder");
        tokio::fs::create_dir(&state_dir).await.unwrap();
        let handle = MachineHandle {
            id: "machine-1".into(),
            name: "builder".into(),
            state_dir: state_dir.to_string_lossy().into_owned(),
            backend: "qemu".into(),
            state: MachineState::Defined,
        };
        let record = MachineRecord {
            handle: handle.clone(),
            spec: MachineSpec {
                name: "builder".into(),
                arch: CpuArch::X86_64,
                memory_mb: 4096,
                cpus: 4,
                disk_gb: 64,
                iso_path: None,
                base_image_path: None,
                guest_agent: true,
                network: NetworkMode::Isolated,
                tpm: true,
                boot: BootMode::Uefi,
            },
            disk_path: state_dir.join(DISK_FILE),
            qmp_socket_path: state_dir.join(QMP_SOCKET_FILE),
            qga_socket_path: None,
            pid: None,
            ovmf_vars_path: None,
            tpm_state_dir: None,
            pending_loadvm: None,
            checkpoints: vec![],
        };
        tokio::fs::write(
            state_dir.join(MACHINE_FILE),
            serde_json::to_vec(&record).unwrap(),
        )
        .await
        .unwrap();
        assert_eq!(backend.list_checkpoints(&handle).await.unwrap(), json!([]));
    }

    #[tokio::test]
    async fn machine_logs_rejects_state_dir_outside_backend_root() {
        let root = tempfile::tempdir().expect("temporary root");
        let backend = QemuBackend::new(root.path()).await.expect("backend");
        let outside = tempfile::tempdir().expect("outside root");
        let handle = MachineHandle {
            id: "machine-1".into(),
            name: "builder".into(),
            state_dir: outside.path().to_string_lossy().into_owned(),
            backend: "qemu".into(),
            state: MachineState::Defined,
        };
        let error = backend
            .machine_logs(&handle, 10)
            .await
            .expect_err("outside state dir must be rejected");
        assert!(matches!(error, CoreError::InvalidHandle(_)));
    }

    #[tokio::test]
    async fn machine_log_tail_is_bounded_and_reports_missing_logs() {
        let root = tempfile::tempdir().expect("temporary root");
        let path = root.path().join("qemu.log");
        let contents = (0..20)
            .map(|index| format!("line-{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        tokio::fs::write(&path, format!("{contents}\n"))
            .await
            .unwrap();
        let tail = tail_log(&path, 3).await.expect("tail log");
        assert_eq!(tail["present"], true);
        assert_eq!(tail["truncated"], true);
        assert_eq!(
            tail["lines"].as_array().unwrap(),
            &vec![json!("line-17"), json!("line-18"), json!("line-19")]
        );
        let missing = tail_log(&root.path().join("missing.log"), 3)
            .await
            .expect("missing log");
        assert_eq!(missing["present"], false);
        assert_eq!(missing["lines"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn machine_log_tail_caps_long_files_by_bytes() {
        let root = tempfile::tempdir().expect("temporary root");
        let path = root.path().join("qemu.log");
        let mut contents = "old-start\n".to_string();
        contents.push_str(&"x".repeat((MAX_LOG_TAIL_BYTES + 128) as usize));
        contents.push_str("\nlast\n");
        tokio::fs::write(&path, contents).await.unwrap();
        let tail = tail_log(&path, 10).await.expect("tail log");
        assert_eq!(tail["truncated"], true);
        let lines = tail["lines"].as_array().unwrap();
        assert_eq!(lines.last(), Some(&json!("last")));
        assert!(lines.iter().all(|line| line != "old-start"));
    }

    #[test]
    fn maps_printable_ascii() {
        assert_eq!(ascii_qcode_keys('a'), Some(vec!["a"]));
        assert_eq!(ascii_qcode_keys('A'), Some(vec!["shift", "a"]));
        assert_eq!(ascii_qcode_keys('!'), Some(vec!["shift", "1"]));
        assert_eq!(ascii_qcode_keys('é'), None);
        assert_eq!(
            "aé"
                .chars()
                .map(ascii_qcode_keys)
                .collect::<Option<Vec<_>>>(),
            None
        );
    }
}
