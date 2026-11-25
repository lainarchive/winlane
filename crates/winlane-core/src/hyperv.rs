use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::process::Command;
use uuid::Uuid;
use winlane_contracts::{
    BootMode, CheckpointRef, ContractResult, ControlEvent, CpuArch, FrameRef, FrameSource,
    InputSink, MachineBackend, MachineHandle, MachineSpec, MachineState, NetworkMode,
};

use crate::error::{CoreError, CoreResult};

const MACHINE_FILE: &str = "machine.json";
const SPEC_FILE: &str = "spec.json";
const DISK_FILE: &str = "disk.vhdx";
const BACKEND: &str = "hyperv";
const GIB: u64 = 1024 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HyperVMachineRecord {
    handle: MachineHandle,
    spec: MachineSpec,
    vm_name: String,
    disk_path: PathBuf,
}

#[derive(Debug, Deserialize)]
struct HyperVStateOutput {
    #[serde(rename = "State")]
    state: String,
}

#[derive(Debug, Deserialize)]
struct HyperVDiskOutput {
    #[serde(rename = "Size")]
    size: u64,
}

/// Compile-safe groundwork for the native Windows Hyper-V lifecycle plane.
///
/// This backend is deliberately limited to Generation 2 lifecycle operations over
/// existing golden VHDX images. Guest control, frames, input, clone, and checkpoint
/// operations fail closed until their Hyper-V-specific semantics are implemented
/// and validated on a real Windows host.
pub struct HyperVBackend {
    root_dir: PathBuf,
}

impl HyperVBackend {
    pub async fn new(root_dir: impl Into<PathBuf>) -> CoreResult<Self> {
        let root_dir = root_dir.into();
        tokio::fs::create_dir_all(&root_dir)
            .await
            .map_err(CoreError::io("create machine root", &root_dir))?;
        let root_dir = tokio::fs::canonicalize(&root_dir)
            .await
            .map_err(CoreError::io("canonicalize machine root", &root_dir))?;
        Ok(Self { root_dir })
    }

    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

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
            let Ok(record) = serde_json::from_slice::<HyperVMachineRecord>(&bytes) else {
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
        validate_hyperv_spec(spec)?;
        require_windows_host()?;
        self.create_machine_inner(spec).await
    }

    async fn create_machine_inner(&mut self, spec: &MachineSpec) -> CoreResult<MachineHandle> {
        let base_image = spec.base_image_path.as_ref().ok_or(CoreError::Unsupported(
            "Hyper-V machines require a validated golden VHDX base image",
        ))?;
        let backing_path = tokio::fs::canonicalize(PathBuf::from(base_image))
            .await
            .map_err(CoreError::io("canonicalize base image", base_image))?;
        let metadata = tokio::fs::metadata(&backing_path)
            .await
            .map_err(CoreError::io("read base image", &backing_path))?;
        if !metadata.is_file() {
            return Err(CoreError::InvalidHandle(
                "base image path must be a regular file".to_string(),
            ));
        }
        if !is_vhdx_path(&backing_path) {
            return Err(CoreError::Unsupported(
                "Hyper-V base images must use the VHDX format",
            ));
        }

        let backing_size = vhd_size_bytes(&backing_path).await?;
        let backing_disk_gb = bytes_to_gib_ceil(backing_size)?;
        if spec.disk_gb > backing_disk_gb {
            return Err(CoreError::Unsupported(
                "expanding Hyper-V differencing disks is not implemented",
            ));
        }

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
        tokio::fs::create_dir(&state_dir)
            .await
            .map_err(CoreError::io("create machine state directory", &state_dir))?;

        let result = async {
            let mut stored_spec = spec.clone();
            stored_spec.disk_gb = backing_disk_gb;
            stored_spec.base_image_path = Some(backing_path.to_string_lossy().into_owned());
            let disk_path = state_dir.join(DISK_FILE);
            let machine_id = format!("machine-{}", Uuid::new_v4().simple());
            let vm_name = hyperv_vm_name(&spec.name, &machine_id);
            let handle = MachineHandle {
                id: machine_id,
                name: spec.name.clone(),
                state_dir: state_dir.to_string_lossy().into_owned(),
                backend: BACKEND.to_string(),
                state: MachineState::Defined,
            };

            run_powershell(&new_differencing_vhd_script(&disk_path, &backing_path)).await?;
            let define_result = run_powershell(&define_vm_script(
                &vm_name,
                &disk_path,
                stored_spec.memory_mb,
                stored_spec.cpus,
                stored_spec.tpm,
            ))
            .await;
            if let Err(error) = define_result {
                let _ = run_powershell(&remove_vm_script(&vm_name)).await;
                return Err(error);
            }

            let record = HyperVMachineRecord {
                handle: handle.clone(),
                spec: stored_spec.clone(),
                vm_name,
                disk_path,
            };
            let write_result = async {
                write_json(&state_dir.join(SPEC_FILE), &stored_spec).await?;
                write_json(&state_dir.join(MACHINE_FILE), &record).await?;
                Ok::<(), CoreError>(())
            }
            .await;
            if let Err(error) = write_result {
                let _ = run_powershell(&remove_vm_script(&record.vm_name)).await;
                return Err(error);
            }
            Ok::<MachineHandle, CoreError>(handle)
        }
        .await;
        if result.is_err() {
            let _ = tokio::fs::remove_dir_all(&state_dir).await;
        }
        result
    }

    pub async fn start_machine(&mut self, machine: &MachineHandle) -> CoreResult<()> {
        require_windows_host()?;
        let record = self.load_record(machine).await?;
        let state = self.status_machine(machine).await?;
        if matches!(
            state,
            MachineState::Running | MachineState::Paused | MachineState::Failed
        ) {
            return Err(invalid_state(&record.handle.name, &state, "stopped"));
        }
        run_powershell(&start_vm_script(&record.vm_name)).await?;
        self.write_state(&record, MachineState::Running).await
    }

    pub async fn stop_machine(&mut self, machine: &MachineHandle) -> CoreResult<()> {
        require_windows_host()?;
        let record = self.load_record(machine).await?;
        let state = self.status_machine(machine).await?;
        if matches!(state, MachineState::Stopped | MachineState::Defined) {
            return Err(invalid_state(&record.handle.name, &state, "running"));
        }
        run_powershell(&stop_vm_script(&record.vm_name)).await?;
        self.write_state(&record, MachineState::Stopped).await
    }

    pub async fn status_machine(&mut self, machine: &MachineHandle) -> CoreResult<MachineState> {
        require_windows_host()?;
        let record = self.load_record(machine).await?;
        let stdout = run_powershell(&status_vm_script(&record.vm_name)).await?;
        let output: HyperVStateOutput = serde_json::from_str(stdout.trim()).map_err(|error| {
            CoreError::HyperV(format!("Get-VM state output was malformed JSON: {error}"))
        })?;
        let state = hyperv_machine_state(&output.state);
        self.write_state(&record, state.clone()).await?;
        Ok(state)
    }

    pub async fn destroy_machine(&mut self, machine: &MachineHandle) -> CoreResult<()> {
        require_windows_host()?;
        let record = self.load_record(machine).await?;
        let state = self.status_machine(machine).await?;
        if matches!(
            state,
            MachineState::Running | MachineState::Paused | MachineState::Failed
        ) {
            return Err(invalid_state(&record.handle.name, &state, "stopped"));
        }
        run_powershell(&remove_vm_script(&record.vm_name)).await?;
        let state_dir = PathBuf::from(&record.handle.state_dir);
        tokio::fs::remove_dir_all(&state_dir)
            .await
            .map_err(CoreError::io("remove machine state directory", state_dir))?;
        Ok(())
    }

    async fn write_state(
        &self,
        record: &HyperVMachineRecord,
        state: MachineState,
    ) -> CoreResult<()> {
        let mut updated = record.clone();
        updated.handle.state = state;
        write_json(
            &PathBuf::from(&updated.handle.state_dir).join(MACHINE_FILE),
            &updated,
        )
        .await
    }

    async fn load_record(&self, machine: &MachineHandle) -> CoreResult<HyperVMachineRecord> {
        if machine.backend != BACKEND {
            return Err(CoreError::InvalidHandle(
                "machine handle does not belong to the Hyper-V backend".to_string(),
            ));
        }
        let state_dir = PathBuf::from(&machine.state_dir);
        self.validate_state_dir_path(&state_dir, Some(&machine.name))?;
        let path = state_dir.join(MACHINE_FILE);
        let bytes = tokio::fs::read(&path)
            .await
            .map_err(CoreError::io("read machine record", &path))?;
        let record: HyperVMachineRecord = serde_json::from_slice(&bytes)?;
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

impl MachineBackend for HyperVBackend {
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
        _machine: &MachineHandle,
        _name: &str,
    ) -> ContractResult<MachineHandle> {
        Err(CoreError::Unsupported("Hyper-V clone is not implemented").into())
    }

    async fn destroy(&mut self, machine: &MachineHandle) -> ContractResult<()> {
        self.destroy_machine(machine).await.map_err(Into::into)
    }

    async fn checkpoint(
        &mut self,
        _machine: &MachineHandle,
        _label: &str,
    ) -> ContractResult<CheckpointRef> {
        Err(CoreError::Unsupported("Hyper-V checkpoints are not implemented").into())
    }

    async fn restore(&mut self, _checkpoint: &CheckpointRef) -> ContractResult<MachineHandle> {
        Err(CoreError::Unsupported("Hyper-V checkpoint restore is not implemented").into())
    }

    async fn delete_checkpoint(
        &mut self,
        _machine: &MachineHandle,
        _checkpoint_id: &str,
    ) -> ContractResult<CheckpointRef> {
        Err(CoreError::Unsupported("Hyper-V checkpoint deletion is not implemented").into())
    }
}

impl FrameSource for HyperVBackend {
    async fn capture_frame(&mut self, _machine: &MachineHandle) -> ContractResult<FrameRef> {
        Err(CoreError::Unsupported("Hyper-V frame capture is not implemented").into())
    }
}

impl InputSink for HyperVBackend {
    async fn send_control(
        &mut self,
        _machine: &MachineHandle,
        _event: ControlEvent,
    ) -> ContractResult<()> {
        Err(CoreError::Unsupported("Hyper-V hardware input is not implemented").into())
    }
}

fn validate_hyperv_spec(spec: &MachineSpec) -> CoreResult<()> {
    if spec.arch != CpuArch::X86_64 {
        return Err(CoreError::Unsupported(
            "Hyper-V control currently supports x86_64 only",
        ));
    }
    if spec.boot != BootMode::Uefi {
        return Err(CoreError::Unsupported(
            "Hyper-V machines require Generation 2 UEFI boot",
        ));
    }
    if spec.iso_path.is_some() {
        return Err(CoreError::Unsupported(
            "Hyper-V unattended provisioning is not implemented",
        ));
    }
    if spec.base_image_path.is_none() {
        return Err(CoreError::Unsupported(
            "Hyper-V machines require a validated golden VHDX base image",
        ));
    }
    if spec.network != NetworkMode::Isolated {
        return Err(CoreError::Unsupported(
            "Hyper-V network automation is not implemented; only isolated machines are supported",
        ));
    }
    Ok(())
}

fn require_windows_host() -> CoreResult<()> {
    if cfg!(windows) {
        Ok(())
    } else {
        Err(CoreError::Unsupported(
            "Hyper-V lifecycle requires a Windows host",
        ))
    }
}

async fn vhd_size_bytes(path: &Path) -> CoreResult<u64> {
    let stdout = run_powershell(&vhd_size_script(path)).await?;
    let output: HyperVDiskOutput = serde_json::from_str(stdout.trim()).map_err(|error| {
        CoreError::HyperV(format!("Get-VHD output was malformed JSON: {error}"))
    })?;
    if output.size == 0 {
        return Err(CoreError::HyperV(
            "Get-VHD reported a zero-length disk".to_string(),
        ));
    }
    Ok(output.size)
}

async fn run_powershell(script: &str) -> CoreResult<String> {
    require_windows_host()?;
    let output = Command::new("powershell.exe")
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-ExecutionPolicy",
            "Bypass",
            "-Command",
            script,
        ])
        .output()
        .await
        .map_err(CoreError::io("run powershell", "powershell.exe"))?;
    if output.status.success() {
        String::from_utf8(output.stdout)
            .map_err(|error| CoreError::HyperV(format!("PowerShell output was not UTF-8: {error}")))
    } else {
        Err(CoreError::HyperV(format!(
            "powershell.exe exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        )))
    }
}

fn new_differencing_vhd_script(disk_path: &Path, backing_path: &Path) -> String {
    format!(
        "New-VHD -Path {} -ParentPath {} -Differencing -ErrorAction Stop | Out-Null",
        ps_quote(&disk_path.to_string_lossy()),
        ps_quote(&backing_path.to_string_lossy())
    )
}

fn define_vm_script(
    vm_name: &str,
    disk_path: &Path,
    memory_mb: u32,
    cpus: u16,
    tpm: bool,
) -> String {
    let memory_bytes = u64::from(memory_mb) * 1024 * 1024;
    let vm = ps_quote(vm_name);
    let disk = ps_quote(&disk_path.to_string_lossy());
    let mut script = format!(
        "New-VM -Name {vm} -Generation 2 -MemoryStartupBytes {memory_bytes} -VHDPath {disk} -ErrorAction Stop | Out-Null;\
Set-VM -Name {vm} -ProcessorCount {cpus} -ErrorAction Stop | Out-Null;\
Set-VMFirmware -VMName {vm} -EnableSecureBoot On -ErrorAction Stop | Out-Null;\
Get-VMNetworkAdapter -VMName {vm} -ErrorAction Stop | Remove-VMNetworkAdapter -ErrorAction Stop"
    );
    if tpm {
        script.push_str(&format!(
            ";Set-VMKeyProtector -VMName {vm} -NewLocalKeyProtector -ErrorAction Stop | Out-Null;Enable-VMTPM -VMName {vm} -ErrorAction Stop | Out-Null"
        ));
    }
    script
}

fn start_vm_script(vm_name: &str) -> String {
    format!(
        "Start-VM -Name {} -ErrorAction Stop | Out-Null",
        ps_quote(vm_name)
    )
}

fn stop_vm_script(vm_name: &str) -> String {
    format!(
        "Stop-VM -Name {} -Force -TurnOff -ErrorAction Stop | Out-Null",
        ps_quote(vm_name)
    )
}

fn status_vm_script(vm_name: &str) -> String {
    format!(
        "Get-VM -Name {} -ErrorAction Stop | Select-Object -Property State | ConvertTo-Json -Compress",
        ps_quote(vm_name)
    )
}

fn remove_vm_script(vm_name: &str) -> String {
    format!(
        "$vm=Get-VM -Name {} -ErrorAction SilentlyContinue;if($null -ne $vm){{Remove-VM -Name {} -Force -ErrorAction Stop}}",
        ps_quote(vm_name),
        ps_quote(vm_name)
    )
}

fn vhd_size_script(path: &Path) -> String {
    format!(
        "Get-VHD -Path {} -ErrorAction Stop | Select-Object -Property Size | ConvertTo-Json -Compress",
        ps_quote(&path.to_string_lossy())
    )
}

fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn hyperv_vm_name(machine_name: &str, machine_id: &str) -> String {
    format!("winlane-{machine_name}-{}", machine_id.replace('-', ""))
}

fn hyperv_machine_state(state: &str) -> MachineState {
    match state {
        "Running" => MachineState::Running,
        "Paused" => MachineState::Paused,
        "Off" | "Saved" => MachineState::Stopped,
        _ => MachineState::Failed,
    }
}

fn bytes_to_gib_ceil(bytes: u64) -> CoreResult<u32> {
    let gib = bytes.div_ceil(GIB);
    u32::try_from(gib).map_err(|_| {
        CoreError::HyperV("base image virtual size exceeds supported disk_gb range".to_string())
    })
}

fn is_vhdx_path(path: &Path) -> bool {
    path.extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("vhdx"))
}

fn invalid_state(machine: &str, state: &MachineState, expected: &'static str) -> CoreError {
    CoreError::InvalidState {
        machine: machine.to_string(),
        actual: match state {
            MachineState::Defined => "defined",
            MachineState::Provisioning => "provisioning",
            MachineState::Stopped => "stopped",
            MachineState::Running => "running",
            MachineState::Paused => "paused",
            MachineState::Failed => "failed",
        },
        expected,
    }
}

async fn write_json(path: &Path, value: &impl Serialize) -> CoreResult<()> {
    let bytes = serde_json::to_vec_pretty(value)?;
    let temp_path = path.with_file_name(format!(
        ".tmp-{}-{}",
        Uuid::new_v4().simple(),
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("json")
    ));
    let result = async {
        tokio::fs::write(&temp_path, bytes)
            .await
            .map_err(CoreError::io("write json", &temp_path))?;
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
            iso_path: None,
            base_image_path: Some("C:\\goldens\\golden.vhdx".to_string()),
            guest_agent: true,
            network: NetworkMode::Isolated,
            tpm: true,
            boot: BootMode::Uefi,
        }
    }

    #[test]
    fn powershell_single_quotes_are_escaped() {
        assert_eq!(ps_quote("a'b"), "'a''b'");
    }

    #[test]
    fn vm_names_include_machine_identity() {
        let first = hyperv_vm_name("proof", "machine-abc");
        let second = hyperv_vm_name("proof", "machine-def");
        assert!(first.starts_with("winlane-proof-"));
        assert_ne!(first, second);
    }

    #[test]
    fn definition_script_is_generation_two_isolated_and_secure_boot() {
        let script = define_vm_script(
            "winlane-proof-abc",
            Path::new("C:\\vm\\disk.vhdx"),
            8192,
            4,
            true,
        );
        assert!(script.contains("New-VM -Name 'winlane-proof-abc' -Generation 2"));
        assert!(script.contains("-MemoryStartupBytes 8589934592"));
        assert!(script.contains("Set-VM -Name 'winlane-proof-abc' -ProcessorCount 4"));
        assert!(script.contains("Set-VMFirmware -VMName 'winlane-proof-abc' -EnableSecureBoot On"));
        assert!(script.contains("Remove-VMNetworkAdapter"));
        assert!(
            script.contains("Set-VMKeyProtector -VMName 'winlane-proof-abc' -NewLocalKeyProtector")
        );
        assert!(script.contains("Enable-VMTPM -VMName 'winlane-proof-abc'"));
        assert!(!script.contains("-SwitchName"));
    }

    #[test]
    fn lifecycle_scripts_are_exact_and_fail_closed() {
        assert_eq!(
            start_vm_script("vm'one"),
            "Start-VM -Name 'vm''one' -ErrorAction Stop | Out-Null"
        );
        assert_eq!(
            stop_vm_script("vm'one"),
            "Stop-VM -Name 'vm''one' -Force -TurnOff -ErrorAction Stop | Out-Null"
        );
        assert!(status_vm_script("vm").contains("ConvertTo-Json"));
        assert!(remove_vm_script("vm").contains("Remove-VM"));
    }

    #[test]
    fn hyperv_states_map_conservatively() {
        assert_eq!(hyperv_machine_state("Running"), MachineState::Running);
        assert_eq!(hyperv_machine_state("Paused"), MachineState::Paused);
        assert_eq!(hyperv_machine_state("Off"), MachineState::Stopped);
        assert_eq!(hyperv_machine_state("Saved"), MachineState::Stopped);
        assert_eq!(
            hyperv_machine_state("RunningCritical"),
            MachineState::Failed
        );
    }

    #[test]
    fn validation_rejects_unsupported_first_slice_semantics() {
        let mut value = spec();
        value.network = NetworkMode::Nat;
        assert!(validate_hyperv_spec(&value).is_err());
        let mut value = spec();
        value.iso_path = Some("C:\\windows.iso".to_string());
        assert!(validate_hyperv_spec(&value).is_err());
        let mut value = spec();
        value.base_image_path = None;
        assert!(validate_hyperv_spec(&value).is_err());
    }

    #[test]
    fn vhdx_detection_is_case_insensitive() {
        assert!(is_vhdx_path(Path::new("C:\\image.VHDX")));
        assert!(!is_vhdx_path(Path::new("image.qcow2")));
    }

    #[test]
    fn disk_size_rounds_up_to_whole_gib() {
        assert_eq!(bytes_to_gib_ceil(GIB + 1).expect("disk size"), 2);
    }

    #[cfg(not(windows))]
    #[tokio::test]
    async fn lifecycle_fails_closed_before_touching_disk_on_non_windows() {
        let root = tempfile::tempdir().expect("temporary machine root");
        let mut backend = HyperVBackend::new(root.path())
            .await
            .expect("create backend");
        let error = backend
            .create_machine(&spec())
            .await
            .expect_err("non-windows lifecycle must fail closed");
        assert!(error.to_string().contains("requires a Windows host"));
        assert!(
            tokio::fs::read_dir(root.path())
                .await
                .expect("read root")
                .next_entry()
                .await
                .expect("inspect root")
                .is_none()
        );
    }
}
