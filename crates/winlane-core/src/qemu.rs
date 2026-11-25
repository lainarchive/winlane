use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
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
const DISK_FILE: &str = "disk.qcow2";
const QMP_SOCKET_FILE: &str = "qmp.sock";
const QGA_SOCKET_FILE: &str = "qga.sock";
const TPM_SOCKET_FILE: &str = "swtpm.sock";
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
        tokio::fs::create_dir_all(&root_dir)
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

    pub async fn create_machine(&mut self, spec: &MachineSpec) -> CoreResult<MachineHandle> {
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

        create_private_dir(&state_dir, "create machine state directory").await?;
        let disk_path = state_dir.join(DISK_FILE);
        self.run_qemu_img_create(&disk_path, backing_path.as_deref(), spec.disk_gb)
            .await?;

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
        Ok(handle)
    }

    pub async fn start_machine(&mut self, machine: &MachineHandle) -> CoreResult<()> {
        let mut record = self.load_record(machine).await?;
        if self.running.contains_key(&record.handle.id) {
            return Err(CoreError::InvalidState {
                machine: record.handle.name,
                actual: "running",
                expected: "stopped",
            });
        }

        let mut swtpm = if record.spec.tpm {
            Some(self.start_swtpm(&record).await?)
        } else {
            None
        };

        let args = self.qemu_args(&mut record).await?;
        let mut command = Command::new(&self.qemu_binary);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
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
            match QmpClient::connect(&record.qmp_socket_path).await {
                Ok(qmp) => {
                    let _ = qmp.execute("quit", None).await;
                }
                Err(_) => {
                    record.handle.state = MachineState::Stopped;
                    self.write_record(&record).await?;
                    return Ok(());
                }
            }
        } else {
            record.handle.state = MachineState::Stopped;
            self.write_record(&record).await?;
            return Ok(());
        }

        record.pid = None;
        record.handle.state = MachineState::Stopped;
        self.write_record(&record).await?;
        Ok(())
    }

    pub async fn status_machine(&mut self, machine: &MachineHandle) -> CoreResult<MachineState> {
        let mut record = self.load_record(machine).await?;
        let machine_id = record.handle.id.clone();
        let mut remove_dead_process = false;
        let state = if let Some(running) = self.running.get_mut(&machine_id) {
            if let Some(exit_status) = running.child.try_wait().map_err(CoreError::io(
                "inspect qemu process",
                record.disk_path.clone(),
            ))? {
                if let Some(process) = &mut running.swtpm {
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
                Err(_) => record.handle.state.clone(),
            }
        } else {
            record.handle.state.clone()
        };

        if remove_dead_process {
            self.running.remove(&machine_id);
        }
        record.handle.state = state.clone();
        if matches!(state, MachineState::Stopped | MachineState::Failed) {
            record.pid = None;
        }
        self.write_record(&record).await?;
        Ok(state)
    }

    pub async fn clone_machine(
        &mut self,
        machine: &MachineHandle,
        name: &str,
    ) -> CoreResult<MachineHandle> {
        let source = self.load_record(machine).await?;
        let state = self.status_machine(machine).await?;
        if matches!(state, MachineState::Running | MachineState::Paused) {
            return Err(CoreError::InvalidState {
                machine: source.handle.name,
                actual: "running",
                expected: "stopped",
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
        create_private_dir(&state_dir, "create clone state directory").await?;

        let disk_path = state_dir.join(DISK_FILE);
        self.run_qemu_img_create(&disk_path, Some(&source.disk_path), spec.disk_gb)
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
        Ok(handle)
    }

    pub async fn destroy_machine(&mut self, machine: &MachineHandle) -> CoreResult<()> {
        let record = self.load_record(machine).await?;
        let state = self.status_machine(machine).await?;
        if matches!(state, MachineState::Running | MachineState::Paused) {
            return Err(CoreError::InvalidState {
                machine: record.handle.name,
                actual: "running",
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
        let mut record = self.load_record(machine).await?;
        let checkpoint_id = format!("checkpoint-{}", Uuid::new_v4().simple());
        let checkpoint_dir = PathBuf::from(&record.handle.state_dir)
            .join(CHECKPOINT_DIR)
            .join(&checkpoint_id);
        tokio::fs::create_dir_all(&checkpoint_dir)
            .await
            .map_err(CoreError::io(
                "create checkpoint directory",
                &checkpoint_dir,
            ))?;

        let running = matches!(self.status_machine(machine).await?, MachineState::Running);
        let vm_state_included = if running {
            let qmp = self.qmp_for_machine(machine).await?;
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
            wait_for_snapshot_job(&qmp, &checkpoint_id).await?;
            true
        } else {
            self.run_qemu_img_snapshot("-c", &checkpoint_id, &record.disk_path)
                .await?;
            false
        };

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

        let checkpoint = CheckpointRef {
            id: checkpoint_id.clone(),
            machine_id: record.handle.id.clone(),
            created_unix_ms: unix_ms(),
            state_dir: record.handle.state_dir.clone(),
            sha256: None,
        };
        let metadata = CheckpointMetadata {
            checkpoint: checkpoint.clone(),
            label: label.to_string(),
            backend: "qemu".to_string(),
            qemu_snapshot: checkpoint_id,
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
        if self.running.contains_key(&record.handle.id) {
            return Err(CoreError::InvalidState {
                machine: record.handle.name,
                actual: "running",
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

        if metadata.firmware_vars_included {
            let target = record
                .ovmf_vars_path
                .clone()
                .unwrap_or_else(|| state_dir.join(OVMF_VARS_FILE));
            tokio::fs::copy(checkpoint_dir.join(OVMF_VARS_FILE), &target)
                .await
                .map_err(CoreError::io("restore firmware state", &target))?;
            record.ovmf_vars_path = Some(target);
        }
        if metadata.tpm_state_included {
            let target = record
                .tpm_state_dir
                .clone()
                .unwrap_or_else(|| state_dir.join(TPM_DIR));
            if tokio::fs::try_exists(&target)
                .await
                .map_err(CoreError::io("check tpm state directory", &target))?
            {
                tokio::fs::remove_dir_all(&target)
                    .await
                    .map_err(CoreError::io("clear tpm state directory", &target))?;
            }
            copy_dir_all(&checkpoint_dir.join(TPM_DIR), &target).await?;
            record.tpm_state_dir = Some(target);
        }

        if metadata.vm_state_included {
            record.pending_loadvm = Some(metadata.qemu_snapshot);
        } else {
            self.run_qemu_img_snapshot("-a", &metadata.qemu_snapshot, &record.disk_path)
                .await?;
            record.pending_loadvm = None;
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
        let frame_path = frame_dir.join(format!("frame-{}.ppm", unix_ms()));
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
        qmp.execute(
            "input-send-event",
            Some(json!({
                "events": [
                    {"type": "abs", "data": {"axis": "x", "value": x}},
                    {"type": "abs", "data": {"axis": "y", "value": y}},
                    {"type": "btn", "data": {"button": button, "down": true}},
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
        for character in text.chars() {
            let keys = ascii_qcode_keys(character).ok_or(CoreError::Unsupported(
                "virtual keyboard input supports printable ASCII plus newline and tab",
            ))?;
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
            "q35,accel=kvm".to_string(),
            "-cpu".to_string(),
            "host".to_string(),
            "-m".to_string(),
            record.spec.memory_mb.to_string(),
            "-smp".to_string(),
            record.spec.cpus.to_string(),
            "-display".to_string(),
            "none".to_string(),
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
            }
            record.ovmf_vars_path = Some(vars_path.clone());
            args.extend([
                "-drive".to_string(),
                format!(
                    "if=pflash,format=raw,readonly=on,file={}",
                    qemu_option_escape(&path_to_string(&code)?)
                ),
                "-drive".to_string(),
                format!(
                    "if=pflash,format=raw,file={}",
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
            "/usr/share/OVMF/OVMF_CODE_4M.fd",
            "/usr/share/OVMF/OVMF_CODE.fd",
        ];
        let vars_candidates = [
            "/usr/share/OVMF/OVMF_VARS_4M.fd",
            "/usr/share/OVMF/OVMF_VARS.fd",
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
        let mut child = Command::new(&self.swtpm_binary)
            .arg("socket")
            .arg(format!("--tpmstate=dir={tpm_state},mode=0600"))
            .arg(format!("--ctrl=type=unixio,path={tpm_socket},mode=0600"))
            .arg("--tpm2")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
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
        if record.handle.id != machine.id || record.handle.name != machine.name {
            return Err(CoreError::InvalidHandle(
                "machine id or name does not match the state directory".to_string(),
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
        "shutdown" | "postmigrate" | "prelaunch" => MachineState::Stopped,
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
    if wait_for_child(&mut running.child, Duration::from_secs(2)).await? {
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
    }
}

async fn wait_for_child(child: &mut Child, duration: Duration) -> CoreResult<bool> {
    match timeout(duration, child.wait()).await {
        Ok(Ok(_)) => Ok(true),
        Ok(Err(error)) => Err(CoreError::io("wait for qemu process", "qemu")(error)),
        Err(_) => Ok(false),
    }
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
    tokio::fs::write(path, bytes)
        .await
        .map_err(CoreError::io("write json", path))?;
    set_private_file(path).await
}

async fn create_private_dir(path: &Path, operation: &'static str) -> CoreResult<()> {
    tokio::fs::create_dir_all(path)
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

async fn copy_dir_all(source: &Path, target: &Path) -> CoreResult<()> {
    let mut stack = vec![(source.to_path_buf(), target.to_path_buf())];
    while let Some((source_dir, target_dir)) = stack.pop() {
        tokio::fs::create_dir_all(&target_dir)
            .await
            .map_err(CoreError::io("create copied directory", &target_dir))?;
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
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ppm_dimensions() {
        let bytes = b"P6\n# comment\n320 200\n255\npixels";
        let dimensions = parse_ppm_dimensions(bytes).expect("PPM should parse");
        assert_eq!(dimensions.width, 320);
        assert_eq!(dimensions.height, 200);
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

    #[test]
    fn maps_printable_ascii() {
        assert_eq!(ascii_qcode_keys('a'), Some(vec!["a"]));
        assert_eq!(ascii_qcode_keys('A'), Some(vec!["shift", "a"]));
        assert_eq!(ascii_qcode_keys('!'), Some(vec!["shift", "1"]));
        assert_eq!(ascii_qcode_keys('é'), None);
    }
}
