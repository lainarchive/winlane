use serde::{Deserialize, Serialize};
use thiserror::Error;

pub type ContractResult<T> = Result<T, ContractError>;

#[derive(Debug, Clone, Error, Serialize, Deserialize, PartialEq, Eq)]
#[error("{message}")]
pub struct ContractError {
    pub message: String,
}

impl ContractError {
    pub fn new(message: impl Into<String>) -> Self {
        Self {
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum CpuArch {
    X86_64,
    Aarch64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum NetworkMode {
    Isolated,
    Nat,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum BootMode {
    Uefi,
    Bios,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MachineSpec {
    pub name: String,
    pub arch: CpuArch,
    pub memory_mb: u32,
    pub cpus: u16,
    pub disk_gb: u32,
    pub iso_path: Option<String>,
    pub network: NetworkMode,
    pub tpm: bool,
    pub boot: BootMode,
}

impl MachineSpec {
    pub fn validate(&self) -> ContractResult<()> {
        if self.name.is_empty() || self.name.len() > 48 {
            return Err(ContractError::new("machine name must be 1-48 characters"));
        }
        if !self
            .name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
        {
            return Err(ContractError::new(
                "machine name may contain only ASCII letters, digits, '-' and '_'",
            ));
        }
        if self.memory_mb < 512 {
            return Err(ContractError::new("memory_mb must be at least 512"));
        }
        if self.cpus == 0 {
            return Err(ContractError::new("cpus must be nonzero"));
        }
        if self.disk_gb < 16 {
            return Err(ContractError::new("disk_gb must be at least 16"));
        }
        if self.boot == BootMode::Bios && self.arch != CpuArch::X86_64 {
            return Err(ContractError::new("legacy BIOS boot is x86_64-only"));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MachineState {
    Defined,
    Provisioning,
    Stopped,
    Running,
    Paused,
    Failed,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MachineHandle {
    pub id: String,
    pub name: String,
    pub state_dir: String,
    pub backend: String,
    pub state: MachineState,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct CheckpointRef {
    pub id: String,
    pub machine_id: String,
    pub created_unix_ms: u64,
    pub state_dir: String,
    pub sha256: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FrameRef {
    pub sha256: String,
    pub width: u32,
    pub height: u32,
    pub captured_unix_ms: u64,
    pub blob_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct UiNodeRef {
    pub id: String,
    pub generation: u64,
    pub automation_id: Option<String>,
    pub control_type: Option<String>,
    pub bounds: Option<[i32; 4]>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ControlEvent {
    Click {
        x: u32,
        y: u32,
        button: MouseButton,
    },
    Move {
        x: u32,
        y: u32,
    },
    Type {
        text: String,
    },
    Key {
        chord: String,
    },
    Scroll {
        direction: ScrollDirection,
        steps: u16,
    },
    Invoke {
        target: UiNodeRef,
    },
    SetText {
        target: UiNodeRef,
        text: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MouseButton {
    Left,
    Middle,
    Right,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ScrollDirection {
    Up,
    Down,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct EvidenceEvent {
    pub sequence: u64,
    pub unix_ms: u64,
    pub machine_id: Option<String>,
    pub kind: String,
    pub payload: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessResult {
    pub exit_code: Option<i32>,
    pub stdout_sha256: Option<String>,
    pub stderr_sha256: Option<String>,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct FileTransferRef {
    pub sha256: String,
    pub bytes: u64,
    pub host_path: String,
    pub guest_path: String,
}

pub trait MachineBackend {
    fn create(
        &mut self,
        spec: &MachineSpec,
    ) -> impl Future<Output = ContractResult<MachineHandle>> + Send;
    fn start(&mut self, machine: &MachineHandle)
    -> impl Future<Output = ContractResult<()>> + Send;
    fn stop(&mut self, machine: &MachineHandle) -> impl Future<Output = ContractResult<()>> + Send;
    fn status(
        &mut self,
        machine: &MachineHandle,
    ) -> impl Future<Output = ContractResult<MachineState>> + Send;
    fn clone_machine(
        &mut self,
        machine: &MachineHandle,
        name: &str,
    ) -> impl Future<Output = ContractResult<MachineHandle>> + Send;
    fn destroy(
        &mut self,
        machine: &MachineHandle,
    ) -> impl Future<Output = ContractResult<()>> + Send;
    fn checkpoint(
        &mut self,
        machine: &MachineHandle,
        label: &str,
    ) -> impl Future<Output = ContractResult<CheckpointRef>> + Send;
    fn restore(
        &mut self,
        checkpoint: &CheckpointRef,
    ) -> impl Future<Output = ContractResult<MachineHandle>> + Send;
}

pub trait FrameSource {
    fn capture_frame(
        &mut self,
        machine: &MachineHandle,
    ) -> impl Future<Output = ContractResult<FrameRef>> + Send;
}

pub trait InputSink {
    fn send_control(
        &mut self,
        machine: &MachineHandle,
        event: ControlEvent,
    ) -> impl Future<Output = ContractResult<()>> + Send;
}

pub trait GuestControlPlane {
    fn ui_tree(
        &mut self,
        machine: &MachineHandle,
    ) -> impl Future<Output = ContractResult<serde_json::Value>> + Send;
    fn invoke(
        &mut self,
        machine: &MachineHandle,
        target: &UiNodeRef,
    ) -> impl Future<Output = ContractResult<()>> + Send;
    fn run_process(
        &mut self,
        machine: &MachineHandle,
        command: &str,
        args: &[String],
    ) -> impl Future<Output = ContractResult<ProcessResult>> + Send;
    fn import_file(
        &mut self,
        machine: &MachineHandle,
        host_path: &str,
        guest_path: &str,
    ) -> impl Future<Output = ContractResult<FileTransferRef>> + Send;
    fn export_file(
        &mut self,
        machine: &MachineHandle,
        guest_path: &str,
        host_path: &str,
    ) -> impl Future<Output = ContractResult<FileTransferRef>> + Send;
}

pub trait EvidenceWriter {
    fn append(&mut self, event: EvidenceEvent) -> impl Future<Output = ContractResult<()>> + Send;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> MachineSpec {
        MachineSpec {
            name: "proof".into(),
            arch: CpuArch::X86_64,
            memory_mb: 4096,
            cpus: 4,
            disk_gb: 64,
            iso_path: None,
            network: NetworkMode::Isolated,
            tpm: true,
            boot: BootMode::Uefi,
        }
    }

    #[test]
    fn machine_spec_round_trips() {
        let spec = spec();
        let encoded = serde_json::to_string(&spec).unwrap();
        let decoded: MachineSpec = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, spec);
    }

    #[test]
    fn rejects_invalid_names() {
        let mut spec = spec();
        spec.name = "bad/name".into();
        assert!(spec.validate().is_err());
    }

    #[test]
    fn rejects_bios_on_arm() {
        let mut spec = spec();
        spec.arch = CpuArch::Aarch64;
        spec.boot = BootMode::Bios;
        assert!(spec.validate().is_err());
    }
}
