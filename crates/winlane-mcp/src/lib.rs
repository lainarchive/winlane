//! MCP-facing machine-control tool mapping over the serialized control IPC boundary.

pub mod server;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::path::PathBuf;
use std::time::Duration;
use thiserror::Error;
use winlane_contracts::{
    BootMode, CheckpointRef, ControlEvent, CpuArch, MachineHandle, MachineSpec, MouseButton,
    NetworkMode, ScrollDirection, UiNodeRef,
};
use winlane_guest::qga::{QgaClient, QgaTransport, UnixQgaTransport};
use winlane_guest::uia::{BlockingUiaControl, UiaResult};
use winlane_ipc::{ControlRequest, IpcError, UnixControlClient};

pub type McpResult<T> = Result<T, McpError>;

#[derive(Debug, Error)]
pub enum McpError {
    #[error(transparent)]
    Ipc(#[from] IpcError),
    #[error("invalid tool input: {0}")]
    InvalidInput(String),
    #[error("unknown control tool: {0}")]
    UnknownTool(String),
    #[error("serialization error: {0}")]
    Serialization(#[from] serde_json::Error),
    #[error("stdio transport error: {0}")]
    Io(String),
    #[error("guest agent connection failed: {0}")]
    GuestConnect(String),
    #[error(transparent)]
    Evidence(#[from] winlane_evidence::EvidenceError),
    #[error(transparent)]
    Uia(#[from] winlane_guest::uia::UiaError),
    #[error(transparent)]
    Join(#[from] tokio::task::JoinError),
}

/// MCP-facing seam for serialized hardware-plane control plus direct guest semantic operations.
pub struct McpControlPlane {
    client: UnixControlClient,
}

impl McpControlPlane {
    pub fn new(control_socket: impl Into<std::path::PathBuf>) -> Self {
        Self {
            client: UnixControlClient::new(control_socket),
        }
    }

    pub async fn dispatch(&self, tool: &str, input: &Value) -> McpResult<Value> {
        match tool {
            "machine_create" => self.machine_create(input).await,
            "machine_clone" => self.machine_clone(input).await,
            "machine_destroy" => self.machine_destroy(machine_input(input)?).await,
            "machine_status" => self.machine_status(machine_input(input)?).await,
            "machine_start" => self.machine_start(machine_input(input)?).await,
            "machine_stop" => self.machine_stop(machine_input(input)?).await,
            "screenshot" => self.screenshot(machine_input(input)?).await,
            "click" => self.click(input).await,
            "move" => self.move_pointer(input).await,
            "type_text" => self.type_text(input).await,
            "key" => self.key(input).await,
            "scroll" => self.scroll(input).await,
            "checkpoint_create" => self.checkpoint_create(input).await,
            "checkpoint_restore" => self.checkpoint_restore(input).await,
            "ui_tree" => self.ui_tree(input).await,
            "ui_invoke" => self.ui_invoke(input).await,
            "ui_set_text" => self.ui_set_text(input).await,
            "process_run" => self.process_run(input).await,
            "file_import" => self.file_import(input).await,
            "file_export" => self.file_export(input).await,
            other => Err(McpError::UnknownTool(other.to_string())),
        }
    }

    pub async fn machine_create(&self, input: &Value) -> McpResult<Value> {
        let network = match optional_string(input, "network")?.as_deref() {
            None | Some("isolated") => NetworkMode::Isolated,
            Some("nat") => NetworkMode::Nat,
            Some(_) => {
                return Err(McpError::InvalidInput(
                    "network must be isolated or nat".to_string(),
                ));
            }
        };
        let boot = match optional_string(input, "boot")?.as_deref() {
            None | Some("uefi") => BootMode::Uefi,
            Some("bios") => BootMode::Bios,
            Some(_) => {
                return Err(McpError::InvalidInput(
                    "boot must be uefi or bios".to_string(),
                ));
            }
        };
        let spec = MachineSpec {
            name: required_string(input, "name")?,
            arch: CpuArch::X86_64,
            memory_mb: optional_bounded_u64(input, "memory_mb", 4096, 512, 1_048_576)? as u32,
            cpus: optional_bounded_u64(input, "cpus", 4, 1, 256)? as u16,
            disk_gb: optional_bounded_u64(input, "disk_gb", 64, 16, 4096)? as u32,
            iso_path: optional_string(input, "iso_path")?,
            base_image_path: optional_string(input, "base_image_path")?,
            guest_agent: optional_bool(input, "guest_agent")?.unwrap_or(true),
            network,
            tpm: optional_bool(input, "tpm")?.unwrap_or(true),
            boot,
        };
        spec.validate()
            .map_err(|error| McpError::InvalidInput(error.to_string()))?;
        Ok(self.client.invoke(&ControlRequest::Create { spec }).await?)
    }

    pub async fn machine_clone(&self, input: &Value) -> McpResult<Value> {
        Ok(self
            .client
            .invoke(&ControlRequest::CloneMachine {
                machine: machine_input(input)?,
                name: required_string(input, "name")?,
            })
            .await?)
    }

    pub async fn machine_destroy(&self, machine: MachineHandle) -> McpResult<Value> {
        Ok(self
            .client
            .invoke(&ControlRequest::Destroy { machine })
            .await?)
    }

    pub async fn machine_status(&self, machine: MachineHandle) -> McpResult<Value> {
        Ok(self
            .client
            .invoke(&ControlRequest::Status { machine })
            .await?)
    }

    pub async fn machine_start(&self, machine: MachineHandle) -> McpResult<Value> {
        Ok(self
            .client
            .invoke(&ControlRequest::Start { machine })
            .await?)
    }

    pub async fn machine_stop(&self, machine: MachineHandle) -> McpResult<Value> {
        Ok(self
            .client
            .invoke(&ControlRequest::Stop { machine })
            .await?)
    }

    pub async fn screenshot(&self, machine: MachineHandle) -> McpResult<Value> {
        Ok(self
            .client
            .invoke(&ControlRequest::CaptureFrame { machine })
            .await?)
    }

    pub async fn click(&self, input: &Value) -> McpResult<Value> {
        let machine = machine_input(input)?;
        let button = match optional_string(input, "button")?.as_deref() {
            None | Some("left") => MouseButton::Left,
            Some("middle") => MouseButton::Middle,
            Some("right") => MouseButton::Right,
            Some(_) => {
                return Err(McpError::InvalidInput(
                    "button must be left, middle, or right".to_string(),
                ));
            }
        };
        self.control(
            machine,
            ControlEvent::Click {
                x: required_u32(input, "x")?,
                y: required_u32(input, "y")?,
                button,
            },
        )
        .await
    }

    pub async fn move_pointer(&self, input: &Value) -> McpResult<Value> {
        self.control(
            machine_input(input)?,
            ControlEvent::Move {
                x: required_u32(input, "x")?,
                y: required_u32(input, "y")?,
            },
        )
        .await
    }

    pub async fn type_text(&self, input: &Value) -> McpResult<Value> {
        self.control(
            machine_input(input)?,
            ControlEvent::Type {
                text: required_string(input, "text")?,
            },
        )
        .await
    }

    pub async fn key(&self, input: &Value) -> McpResult<Value> {
        self.control(
            machine_input(input)?,
            ControlEvent::Key {
                chord: required_string(input, "chord")?,
            },
        )
        .await
    }

    pub async fn scroll(&self, input: &Value) -> McpResult<Value> {
        let direction = match required_string(input, "direction")?.as_str() {
            "up" => ScrollDirection::Up,
            "down" => ScrollDirection::Down,
            _ => {
                return Err(McpError::InvalidInput(
                    "direction must be up or down".to_string(),
                ));
            }
        };
        let steps = required_u32(input, "steps")?;
        let steps = u16::try_from(steps).map_err(|_| {
            McpError::InvalidInput("steps must fit in an unsigned 16-bit integer".to_string())
        })?;
        self.control(
            machine_input(input)?,
            ControlEvent::Scroll { direction, steps },
        )
        .await
    }

    pub async fn checkpoint_create(&self, input: &Value) -> McpResult<Value> {
        Ok(self
            .client
            .invoke(&ControlRequest::Checkpoint {
                machine: machine_input(input)?,
                label: required_string(input, "label")?,
            })
            .await?)
    }

    pub async fn checkpoint_restore(&self, input: &Value) -> McpResult<Value> {
        let checkpoint_value = input.get("checkpoint").ok_or_else(|| {
            McpError::InvalidInput("missing required field 'checkpoint'".to_string())
        })?;
        let checkpoint: CheckpointRef = serde_json::from_value(checkpoint_value.clone())?;
        Ok(self
            .client
            .invoke(&ControlRequest::Restore { checkpoint })
            .await?)
    }

    pub async fn ui_tree(&self, input: &Value) -> McpResult<Value> {
        let machine = machine_input(input)?;
        let max_depth = optional_bounded_u64(input, "max_depth", 8, 1, 32)? as u16;
        let max_nodes = optional_bounded_u64(input, "max_nodes", 2000, 1, 20_000)? as u32;
        self.semantic(
            machine,
            SemanticOperation::UiTree {
                max_depth,
                max_nodes,
            },
        )
        .await
    }

    pub async fn ui_invoke(&self, input: &Value) -> McpResult<Value> {
        let machine = machine_input(input)?;
        let node = required_string(input, "node")?;
        self.semantic(machine, SemanticOperation::Invoke { node })
            .await
    }

    pub async fn ui_set_text(&self, input: &Value) -> McpResult<Value> {
        let machine = machine_input(input)?;
        let node = required_string(input, "node")?;
        let text = required_string(input, "text")?;
        self.semantic(machine, SemanticOperation::SetText { node, text })
            .await
    }

    pub async fn process_run(&self, input: &Value) -> McpResult<Value> {
        let machine = machine_input(input)?;
        let command = required_string(input, "command")?;
        let args = optional_string_array(input, "args")?;
        let timeout_ms = optional_bounded_u64(input, "timeout_ms", 30_000, 1, 600_000)?;
        self.semantic(
            machine,
            SemanticOperation::Run {
                command,
                args,
                timeout_ms,
            },
        )
        .await
    }

    pub async fn file_import(&self, input: &Value) -> McpResult<Value> {
        let machine = machine_input(input)?;
        let host_path = PathBuf::from(required_string(input, "host_path")?);
        let guest_path = required_string(input, "guest_path")?;
        self.semantic(
            machine,
            SemanticOperation::Import {
                host_path,
                guest_path,
            },
        )
        .await
    }

    pub async fn file_export(&self, input: &Value) -> McpResult<Value> {
        let machine = machine_input(input)?;
        let guest_path = required_string(input, "guest_path")?;
        let host_path = PathBuf::from(required_string(input, "host_path")?);
        self.semantic(
            machine,
            SemanticOperation::Export {
                guest_path,
                host_path,
            },
        )
        .await
    }

    async fn semantic(
        &self,
        machine: MachineHandle,
        operation: SemanticOperation,
    ) -> McpResult<Value> {
        tokio::task::spawn_blocking(move || {
            let qga_socket = PathBuf::from(machine.state_dir).join("qga.sock");
            let transport = UnixQgaTransport::connect(qga_socket, Duration::from_secs(5))
                .map_err(|error| McpError::GuestConnect(error.to_string()))?;
            let mut control = BlockingUiaControl::new(QgaClient::new(transport));
            execute_semantic_operation(&mut control, operation).map_err(McpError::from)
        })
        .await?
    }

    async fn control(&self, machine: MachineHandle, event: ControlEvent) -> McpResult<Value> {
        Ok(self
            .client
            .invoke(&ControlRequest::Control { machine, event })
            .await?)
    }
}

#[derive(Debug)]
enum SemanticOperation {
    UiTree {
        max_depth: u16,
        max_nodes: u32,
    },
    Invoke {
        node: String,
    },
    SetText {
        node: String,
        text: String,
    },
    Run {
        command: String,
        args: Vec<String>,
        timeout_ms: u64,
    },
    Import {
        host_path: PathBuf,
        guest_path: String,
    },
    Export {
        guest_path: String,
        host_path: PathBuf,
    },
}

fn execute_semantic_operation<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    operation: SemanticOperation,
) -> UiaResult<Value> {
    match operation {
        SemanticOperation::UiTree {
            max_depth,
            max_nodes,
        } => control.ui_tree(max_depth, max_nodes),
        SemanticOperation::Invoke { node } => control.invoke(&node_ref(&node)),
        SemanticOperation::SetText { node, text } => control.set_text(&node_ref(&node), &text),
        SemanticOperation::Run {
            command,
            args,
            timeout_ms,
        } => {
            let capture = control.run_process_blocking(&command, &args, timeout_ms)?;
            Ok(json!({
                "exit_code": capture.exit_code,
                "stdout_base64": STANDARD.encode(&capture.stdout),
                "stderr_base64": STANDARD.encode(&capture.stderr),
            }))
        }
        SemanticOperation::Import {
            host_path,
            guest_path,
        } => Ok(serde_json::to_value(
            control.import_file_blocking(&host_path, &guest_path)?,
        )?),
        SemanticOperation::Export {
            guest_path,
            host_path,
        } => Ok(serde_json::to_value(
            control.export_file_blocking(&guest_path, &host_path)?,
        )?),
    }
}

fn node_ref(path: &str) -> UiNodeRef {
    UiNodeRef {
        id: path.to_string(),
        generation: 0,
        automation_id: None,
        control_type: None,
        bounds: None,
    }
}

pub fn tool_definitions() -> Value {
    let machine_ref = json!({
        "type": "object",
        "description": "MachineHandle returned by machine creation or restore",
        "properties": {
            "id": {"type": "string"},
            "name": {"type": "string"},
            "state_dir": {"type": "string"},
            "backend": {"type": "string"},
            "state": {"type": "string"}
        },
        "required": ["id", "name", "state_dir", "backend", "state"]
    });
    let machine_only = json!({
        "type": "object",
        "properties": {"machine": machine_ref},
        "required": ["machine"]
    });
    let pointer = json!({
        "type": "object",
        "properties": {
            "machine": machine_ref,
            "x": {"type": "integer", "minimum": 0},
            "y": {"type": "integer", "minimum": 0}
        },
        "required": ["machine", "x", "y"]
    });
    json!([
        {"name": "machine_create", "description": "Create a disposable machine from a golden image or installer ISO", "inputSchema": {
            "type": "object",
            "properties": {
                "name": {"type": "string", "minLength": 1, "maxLength": 48},
                "base_image_path": {"type": "string"},
                "iso_path": {"type": "string"},
                "memory_mb": {"type": "integer", "minimum": 512, "maximum": 1048576, "default": 4096},
                "cpus": {"type": "integer", "minimum": 1, "maximum": 256, "default": 4},
                "disk_gb": {"type": "integer", "minimum": 16, "maximum": 4096, "default": 64},
                "network": {"type": "string", "enum": ["isolated", "nat"], "default": "isolated"},
                "guest_agent": {"type": "boolean", "default": true},
                "tpm": {"type": "boolean", "default": true},
                "boot": {"type": "string", "enum": ["uefi", "bios"], "default": "uefi"}
            },
            "required": ["name"]
        }},
        {"name": "machine_clone", "description": "Clone an existing machine under a new name", "inputSchema": {
            "type": "object",
            "properties": {
                "machine": machine_ref,
                "name": {"type": "string", "minLength": 1, "maxLength": 48}
            },
            "required": ["machine", "name"]
        }},
        {"name": "machine_destroy", "description": "Destroy a machine and its disposable state", "inputSchema": machine_only},
        {"name": "machine_status", "description": "Read machine state", "inputSchema": machine_only},
        {"name": "machine_start", "description": "Start an existing machine", "inputSchema": machine_only},
        {"name": "machine_stop", "description": "Stop a running machine", "inputSchema": machine_only},
        {"name": "screenshot", "description": "Capture a full-resolution frame", "inputSchema": machine_only},
        {"name": "click", "description": "Click guest pixel coordinates", "inputSchema": {
            "type": "object",
            "properties": {
                "machine": machine_ref,
                "x": {"type": "integer", "minimum": 0},
                "y": {"type": "integer", "minimum": 0},
                "button": {"type": "string", "enum": ["left", "middle", "right"], "default": "left"}
            },
            "required": ["machine", "x", "y"]
        }},
        {"name": "move", "description": "Move the guest pointer", "inputSchema": pointer},
        {"name": "type_text", "description": "Type ASCII text through virtual hardware", "inputSchema": {
            "type": "object",
            "properties": {"machine": machine_ref, "text": {"type": "string"}},
            "required": ["machine", "text"]
        }},
        {"name": "key", "description": "Send a key or chord", "inputSchema": {
            "type": "object",
            "properties": {"machine": machine_ref, "chord": {"type": "string"}},
            "required": ["machine", "chord"]
        }},
        {"name": "scroll", "description": "Scroll up or down", "inputSchema": {
            "type": "object",
            "properties": {
                "machine": machine_ref,
                "direction": {"type": "string", "enum": ["up", "down"]},
                "steps": {"type": "integer", "minimum": 1, "maximum": 65535}
            },
            "required": ["machine", "direction", "steps"]
        }},
        {"name": "checkpoint_create", "description": "Create a restorable machine checkpoint", "inputSchema": {
            "type": "object",
            "properties": {"machine": machine_ref, "label": {"type": "string"}},
            "required": ["machine", "label"]
        }},
        {"name": "checkpoint_restore", "description": "Restore a machine checkpoint", "inputSchema": {
            "type": "object",
            "properties": {"checkpoint": {"type": "object"}},
            "required": ["checkpoint"]
        }},
        {"name": "ui_tree", "description": "Read the guest UI Automation tree", "inputSchema": {
            "type": "object",
            "properties": {
                "machine": machine_ref,
                "max_depth": {"type": "integer", "minimum": 1, "maximum": 32, "default": 8},
                "max_nodes": {"type": "integer", "minimum": 1, "maximum": 20000, "default": 2000}
            },
            "required": ["machine"]
        }},
        {"name": "ui_invoke", "description": "Invoke a UI element by tree node path", "inputSchema": {
            "type": "object",
            "properties": {
                "machine": machine_ref,
                "node": {"type": "string", "pattern": "^0(/[0-9]+)*$"}
            },
            "required": ["machine", "node"]
        }},
        {"name": "ui_set_text", "description": "Set text on a UI element by tree node path", "inputSchema": {
            "type": "object",
            "properties": {
                "machine": machine_ref,
                "node": {"type": "string", "pattern": "^0(/[0-9]+)*$"},
                "text": {"type": "string", "maxLength": 4096}
            },
            "required": ["machine", "node", "text"]
        }},
        {"name": "process_run", "description": "Run a guest process and capture exit status", "inputSchema": {
            "type": "object",
            "properties": {
                "machine": machine_ref,
                "command": {"type": "string"},
                "args": {"type": "array", "items": {"type": "string"}, "default": []},
                "timeout_ms": {"type": "integer", "minimum": 1, "maximum": 600000, "default": 30000}
            },
            "required": ["machine", "command"]
        }},
        {"name": "file_import", "description": "Copy a host file into the guest", "inputSchema": {
            "type": "object",
            "properties": {
                "machine": machine_ref,
                "host_path": {"type": "string"},
                "guest_path": {"type": "string"}
            },
            "required": ["machine", "host_path", "guest_path"]
        }},
        {"name": "file_export", "description": "Copy a guest file to the host", "inputSchema": {
            "type": "object",
            "properties": {
                "machine": machine_ref,
                "guest_path": {"type": "string"},
                "host_path": {"type": "string"}
            },
            "required": ["machine", "guest_path", "host_path"]
        }}
    ])
}

fn machine_input(input: &Value) -> McpResult<MachineHandle> {
    let machine = input
        .get("machine")
        .ok_or_else(|| McpError::InvalidInput("missing required field 'machine'".to_string()))?;
    serde_json::from_value(machine.clone()).map_err(McpError::from)
}

fn required_string(input: &Value, field: &str) -> McpResult<String> {
    input
        .get(field)
        .and_then(Value::as_str)
        .map(str::to_string)
        .ok_or_else(|| McpError::InvalidInput(format!("missing required string field '{field}'")))
}

fn optional_string(input: &Value, field: &str) -> McpResult<Option<String>> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .map(|value| Some(value.to_string()))
            .ok_or_else(|| McpError::InvalidInput(format!("field '{field}' must be a string"))),
    }
}

fn optional_string_array(input: &Value, field: &str) -> McpResult<Vec<String>> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(Vec::new()),
        Some(Value::Array(values)) => values
            .iter()
            .map(|value| {
                value.as_str().map(str::to_owned).ok_or_else(|| {
                    McpError::InvalidInput(format!("field '{field}' must contain only strings"))
                })
            })
            .collect(),
        Some(_) => Err(McpError::InvalidInput(format!(
            "field '{field}' must be an array"
        ))),
    }
}

fn optional_bounded_u64(
    input: &Value,
    field: &str,
    default: u64,
    minimum: u64,
    maximum: u64,
) -> McpResult<u64> {
    let value = match input.get(field) {
        None | Some(Value::Null) => default,
        Some(value) => value
            .as_u64()
            .ok_or_else(|| McpError::InvalidInput(format!("field '{field}' must be an integer")))?,
    };
    if (minimum..=maximum).contains(&value) {
        Ok(value)
    } else {
        Err(McpError::InvalidInput(format!(
            "field '{field}' must be {minimum}..={maximum}"
        )))
    }
}

fn optional_bool(input: &Value, field: &str) -> McpResult<Option<bool>> {
    match input.get(field) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| McpError::InvalidInput(format!("field '{field}' must be a boolean"))),
    }
}

fn required_u32(input: &Value, field: &str) -> McpResult<u32> {
    let value = input.get(field).and_then(Value::as_u64).ok_or_else(|| {
        McpError::InvalidInput(format!("missing required integer field '{field}'"))
    })?;
    u32::try_from(value)
        .map_err(|_| McpError::InvalidInput(format!("field '{field}' exceeds u32 range")))
}

pub fn implementation_status() -> &'static str {
    "semantic-guest-tools"
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::rc::Rc;

    type WrittenRequests = Rc<RefCell<Vec<Value>>>;

    struct MockQgaTransport {
        incoming: VecDeque<Vec<u8>>,
        exec_output: Option<(i32, Vec<u8>, Vec<u8>)>,
        written: WrittenRequests,
    }

    impl QgaTransport for MockQgaTransport {
        fn write_raw(&mut self, _bytes: &[u8]) -> io::Result<()> {
            Ok(())
        }

        fn write_message(&mut self, bytes: &[u8]) -> io::Result<()> {
            let request: Value = serde_json::from_slice(bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
            self.written.borrow_mut().push(request.clone());
            let execute = request
                .get("execute")
                .and_then(Value::as_str)
                .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "missing execute"))?;
            let result = match execute {
                "guest-exec" => json!({ "pid": 99 }),
                "guest-exec-status" => {
                    let (exit_code, stdout, stderr) =
                        self.exec_output
                            .take()
                            .unwrap_or((0, Vec::new(), Vec::new()));
                    json!({
                        "exited": true,
                        "exitcode": exit_code,
                        "out-data": STANDARD.encode(stdout),
                        "err-data": STANDARD.encode(stderr),
                    })
                }
                _ => return Err(io::Error::new(io::ErrorKind::InvalidData, execute)),
            };
            self.incoming
                .push_back(json!({ "return": result }).to_string().into_bytes());
            Ok(())
        }

        fn read_message(&mut self) -> io::Result<Vec<u8>> {
            self.incoming.pop_front().ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "scripted response missing")
            })
        }
    }

    fn machine_json() -> Value {
        json!({
            "id": "m1",
            "name": "machine",
            "state_dir": "/tmp/machine",
            "backend": "qemu",
            "state": "running"
        })
    }

    #[test]
    fn exposes_hardware_and_semantic_contract_tools() {
        let tools = tool_definitions();
        let names = tools
            .as_array()
            .expect("tool definitions are an array")
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            [
                "machine_create",
                "machine_clone",
                "machine_destroy",
                "machine_status",
                "machine_start",
                "machine_stop",
                "screenshot",
                "click",
                "move",
                "type_text",
                "key",
                "scroll",
                "checkpoint_create",
                "checkpoint_restore",
                "ui_tree",
                "ui_invoke",
                "ui_set_text",
                "process_run",
                "file_import",
                "file_export"
            ]
        );
    }

    #[test]
    fn semantic_tool_schemas_match_snapshot() {
        let expected: Value = serde_json::from_str(include_str!("../tests/semantic-schema.json"))
            .expect("semantic schema snapshot is valid JSON");
        let semantic_names = [
            "ui_tree",
            "ui_invoke",
            "ui_set_text",
            "process_run",
            "file_import",
            "file_export",
        ];
        let actual = tool_definitions()
            .as_array()
            .expect("tool definitions are an array")
            .iter()
            .filter(|tool| {
                tool.get("name")
                    .and_then(Value::as_str)
                    .is_some_and(|name| semantic_names.contains(&name))
            })
            .cloned()
            .collect::<Vec<_>>();
        assert_eq!(json!(actual), expected);
    }

    #[tokio::test]
    async fn machine_create_input_is_validated_before_control_connect() {
        let mcp = McpControlPlane::new("/tmp/winlane-test.sock");
        let bad_name = mcp
            .dispatch("machine_create", &json!({"name": "bad/name"}))
            .await;
        assert!(matches!(bad_name, Err(McpError::InvalidInput(_))));
        let bad_network = mcp
            .dispatch(
                "machine_create",
                &json!({"name": "valid-name", "network": "bridged"}),
            )
            .await;
        assert!(matches!(bad_network, Err(McpError::InvalidInput(_))));
    }

    #[tokio::test]
    async fn semantic_bounds_and_array_types_are_validated_before_guest_connect() {
        let mcp = McpControlPlane::new("/tmp/winlane-test.sock");
        let bad_depth = mcp
            .dispatch(
                "ui_tree",
                &json!({"machine": machine_json(), "max_depth": 0}),
            )
            .await;
        assert!(matches!(bad_depth, Err(McpError::InvalidInput(_))));
        let bad_args = mcp
            .dispatch(
                "process_run",
                &json!({
                    "machine": machine_json(),
                    "command": r"C:\Windows\System32\cmd.exe",
                    "args": ["ok", 2]
                }),
            )
            .await;
        assert!(matches!(bad_args, Err(McpError::InvalidInput(_))));
    }

    #[test]
    fn process_run_uses_guest_exec_and_base64_schema() {
        let written = Rc::new(RefCell::new(Vec::new()));
        let transport = MockQgaTransport {
            incoming: VecDeque::new(),
            exec_output: Some((0, b"out".to_vec(), b"err".to_vec())),
            written: Rc::clone(&written),
        };
        let mut control = BlockingUiaControl::new(QgaClient::new(transport));
        let output = execute_semantic_operation(
            &mut control,
            SemanticOperation::Run {
                command: r"C:\Windows\System32\cmd.exe".to_string(),
                args: vec!["/c".to_string(), "echo ok".to_string()],
                timeout_ms: 5_000,
            },
        )
        .unwrap();
        assert_eq!(
            output,
            json!({
                "exit_code": 0,
                "stdout_base64": "b3V0",
                "stderr_base64": "ZXJy"
            })
        );
        let requests = written.borrow();
        assert_eq!(requests.len(), 2);
        assert_eq!(requests[0]["execute"], "guest-exec");
        assert_eq!(
            requests[0]["arguments"]["path"],
            r"C:\Windows\System32\cmd.exe"
        );
        assert_eq!(requests[0]["arguments"]["arg"], json!(["/c", "echo ok"]));
    }

    #[test]
    fn bare_process_command_is_rejected_before_transport_writes() {
        let written = Rc::new(RefCell::new(Vec::new()));
        let transport = MockQgaTransport {
            incoming: VecDeque::new(),
            exec_output: None,
            written: Rc::clone(&written),
        };
        let mut control = BlockingUiaControl::new(QgaClient::new(transport));
        let result = execute_semantic_operation(
            &mut control,
            SemanticOperation::Run {
                command: "powershell.exe".to_string(),
                args: Vec::new(),
                timeout_ms: 5_000,
            },
        );
        assert!(matches!(
            result,
            Err(winlane_guest::uia::UiaError::InvalidCommandPath)
        ));
        assert!(written.borrow().is_empty());
    }
}
