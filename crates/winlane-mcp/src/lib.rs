//! MCP-facing machine-control tool mapping over the serialized control IPC boundary.

use serde_json::{Value, json};
use thiserror::Error;
use winlane_contracts::{CheckpointRef, ControlEvent, MachineHandle, MouseButton, ScrollDirection};
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
}

/// Narrow MCP-facing seam. It exposes only the hardware-plane operations in
/// the contract; guest semantic tools remain unavailable until the guest helper exists.
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
            other => Err(McpError::UnknownTool(other.to_string())),
        }
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

    async fn control(&self, machine: MachineHandle, event: ControlEvent) -> McpResult<Value> {
        Ok(self
            .client
            .invoke(&ControlRequest::Control { machine, event })
            .await?)
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

fn required_u32(input: &Value, field: &str) -> McpResult<u32> {
    let value = input.get(field).and_then(Value::as_u64).ok_or_else(|| {
        McpError::InvalidInput(format!("missing required integer field '{field}'"))
    })?;
    u32::try_from(value)
        .map_err(|_| McpError::InvalidInput(format!("field '{field}' exceeds u32 range")))
}

pub fn implementation_status() -> &'static str {
    "hardware-control-tools"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn exposes_only_hardware_plane_contract_tools() {
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
                "checkpoint_restore"
            ]
        );
    }
}
