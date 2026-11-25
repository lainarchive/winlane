//! Newline-delimited JSON-RPC stdio transport for MCP clients.

use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::Arc;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::Mutex;
use winlane_evidence::{EvidenceStore, sha256_hex};

use crate::{McpControlPlane, McpError, McpResult, tool_definitions};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const PROTOCOL_VERSION: &str = "2024-11-05";

pub async fn serve_stdio(
    control_socket: impl Into<std::path::PathBuf>,
    evidence_root: Option<PathBuf>,
) -> McpResult<()> {
    let control = McpControlPlane::new(control_socket);
    let evidence = evidence_root
        .map(|root| EvidenceStore::create(root).map(|store| Arc::new(Mutex::new(store))))
        .transpose()?;
    serve_stdio_with_control(&control, evidence).await
}

pub async fn serve_stdio_with_control(
    control: &McpControlPlane,
    evidence: Option<Arc<Mutex<EvidenceStore>>>,
) -> McpResult<()> {
    let stdin = tokio::io::stdin();
    let mut lines = BufReader::new(stdin).lines();
    let stdout = tokio::io::stdout();
    let mut output = tokio::io::BufWriter::new(stdout);

    while let Some(line) = lines
        .next_line()
        .await
        .map_err(|error| McpError::Io(error.to_string()))?
    {
        if line.len() > MAX_MESSAGE_BYTES {
            write_response(
                &mut output,
                error_response(Value::Null, -32600, "message exceeded one MiB"),
            )
            .await?;
            continue;
        }
        let value = match serde_json::from_str::<Value>(&line) {
            Ok(value) => value,
            Err(_) => {
                write_response(
                    &mut output,
                    error_response(Value::Null, -32700, "invalid JSON"),
                )
                .await?;
                continue;
            }
        };
        if let Some(response) =
            handle_message_with_evidence(control, &value, evidence.clone()).await
        {
            write_response(&mut output, response).await?;
        }
    }
    output
        .flush()
        .await
        .map_err(|error| McpError::Io(error.to_string()))?;
    Ok(())
}

pub async fn handle_message(control: &McpControlPlane, message: &Value) -> Option<Value> {
    handle_message_with_evidence(control, message, None).await
}

pub async fn handle_message_with_evidence(
    control: &McpControlPlane,
    message: &Value,
    evidence: Option<Arc<Mutex<EvidenceStore>>>,
) -> Option<Value> {
    let id = message.get("id").cloned();
    let method = message.get("method").and_then(Value::as_str);
    let Some(method) = method else {
        return id.map(|id| error_response(id, -32600, "missing method"));
    };
    let notification = id.is_none();

    let result = match method {
        "initialize" => Ok(json!({
            "protocolVersion": message
                .get("params")
                .and_then(|params| params.get("protocolVersion"))
                .and_then(Value::as_str)
                .unwrap_or(PROTOCOL_VERSION),
            "capabilities": {
                "tools": { "listChanged": false }
            },
            "serverInfo": {
                "name": "winlane",
                "version": env!("CARGO_PKG_VERSION")
            }
        })),
        "notifications/initialized" | "notifications/cancelled" => Ok(Value::Null),
        "ping" => Ok(json!({})),
        "tools/list" => Ok(json!({ "tools": tool_definitions() })),
        "tools/call" => match tool_call(control, message.get("params"), evidence).await {
            Ok(value) => Ok(value),
            Err(error) => Ok(tool_error_result(error.to_string())),
        },
        _ => Err((-32601, format!("unknown method '{method}'"))),
    };

    if notification {
        return None;
    }
    let id = id.unwrap_or(Value::Null);
    Some(match result {
        Ok(result) => json!({"jsonrpc":"2.0","id":id,"result":result}),
        Err((code, message)) => error_response(id, code, &message),
    })
}

async fn tool_call(
    control: &McpControlPlane,
    params: Option<&Value>,
    evidence: Option<Arc<Mutex<EvidenceStore>>>,
) -> McpResult<Value> {
    let params = params.ok_or_else(|| McpError::InvalidInput("missing params".to_string()))?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::InvalidInput("missing tool name".to_string()))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let outcome = control.dispatch(name, &arguments).await;
    if let Some(store) = evidence {
        record_tool_evidence(store, name, &arguments, &outcome).await?;
    }
    let result = outcome?;
    let text = serde_json::to_string_pretty(&result)?;
    Ok(json!({
        "content": [{"type":"text","text":text}],
        "structuredContent": {"result": result},
        "isError": false
    }))
}

async fn record_tool_evidence(
    store: Arc<Mutex<EvidenceStore>>,
    tool: &str,
    arguments: &Value,
    outcome: &McpResult<Value>,
) -> McpResult<()> {
    let payload = json!({
        "tool": tool,
        "input": sanitize_tool_input(tool, arguments),
        "ok": outcome.is_ok(),
        "result": outcome.as_ref().ok().cloned().unwrap_or(Value::Null),
        "error": outcome.as_ref().err().map(ToString::to_string),
    });
    let machine_id = arguments
        .pointer("/machine/id")
        .or_else(|| arguments.pointer("/checkpoint/machine_id"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    let mut store = store.lock().await;
    store
        .record(evidence_kind_for_tool(tool), machine_id, payload)
        .map_err(McpError::from)?;
    Ok(())
}

fn evidence_kind_for_tool(tool: &str) -> &'static str {
    match tool {
        "machine_create" => "machine_created",
        "machine_clone" => "machine_cloned",
        "machine_destroy" => "machine_destroyed",
        "machine_start" => "machine_started",
        "machine_stop" => "machine_stopped",
        "screenshot" => "frame_captured",
        "click" | "move" | "type_text" | "key" | "scroll" | "ui_invoke" | "ui_set_text" => {
            "input_sent"
        }
        "ui_tree" => "ui_tree_read",
        "process_run" => "process_exited",
        "file_import" => "file_imported",
        "file_export" => "file_exported",
        "checkpoint_create" => "checkpoint_created",
        "checkpoint_restore" => "checkpoint_restored",
        _ => "machine_status",
    }
}

fn sanitize_tool_input(tool: &str, input: &Value) -> Value {
    let mut sanitized = input.clone();
    if matches!(tool, "type_text" | "ui_set_text")
        && let Some(text) = input.get("text").and_then(Value::as_str)
    {
        sanitized["text"] = json!({
            "sha256": sha256_hex(text.as_bytes()),
            "bytes": text.len(),
        });
    }
    if tool == "process_run"
        && let Some(args) = input.get("args").and_then(Value::as_array)
    {
        sanitized["args"] = json!(
            args.iter()
                .map(|argument| {
                    argument.as_str().map_or(argument.clone(), |argument| {
                        json!({
                            "sha256": sha256_hex(argument.as_bytes()),
                            "bytes": argument.len(),
                        })
                    })
                })
                .collect::<Vec<_>>()
        );
    }
    sanitized
}

fn tool_error_result(message: String) -> Value {
    json!({
        "content": [{"type":"text","text":message}],
        "isError": true
    })
}

fn error_response(id: Value, code: i64, message: &str) -> Value {
    json!({
        "jsonrpc":"2.0",
        "id":id,
        "error":{"code":code,"message":message}
    })
}

async fn write_response(
    output: &mut tokio::io::BufWriter<tokio::io::Stdout>,
    response: Value,
) -> McpResult<()> {
    let encoded = serde_json::to_vec(&response)?;
    output
        .write_all(&encoded)
        .await
        .map_err(|error| McpError::Io(error.to_string()))?;
    output
        .write_all(b"\n")
        .await
        .map_err(|error| McpError::Io(error.to_string()))?;
    output
        .flush()
        .await
        .map_err(|error| McpError::Io(error.to_string()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn initialize_response_uses_mcp_shape() {
        let control = McpControlPlane::new("/tmp/winlane-test.sock");
        let response = handle_message(
            &control,
            &json!({"jsonrpc":"2.0","id":1,"method":"initialize","params":{}}),
        )
        .await
        .unwrap();
        assert_eq!(response["result"]["serverInfo"]["name"], "winlane");
        assert!(response["result"]["capabilities"]["tools"].is_object());
    }

    #[tokio::test]
    async fn lists_contract_tools() {
        let control = McpControlPlane::new("/tmp/winlane-test.sock");
        let response = handle_message(
            &control,
            &json!({"jsonrpc":"2.0","id":"tools","method":"tools/list"}),
        )
        .await
        .unwrap();
        assert!(response["result"]["tools"].as_array().unwrap().len() >= 11);
    }

    #[test]
    fn redacts_typed_text_in_evidence_input() {
        let input = json!({"machine":{"id":"m"},"text":"secret"});
        let sanitized = sanitize_tool_input("type_text", &input);
        assert!(sanitized["text"]["sha256"].is_string());
        assert_eq!(sanitized["text"]["bytes"], 6);
        assert!(sanitized["text"].get("sha256").is_some());
        assert_ne!(sanitized["text"], "secret");
    }

    #[test]
    fn redacts_semantic_text_and_process_args_in_evidence_input() {
        let input = json!({
            "machine":{"id":"m"},
            "text":"secret",
            "args":["visible?", "--password=secret"]
        });
        let text_sanitized = sanitize_tool_input("ui_set_text", &input);
        assert_eq!(text_sanitized["text"]["bytes"], 6);
        assert_ne!(text_sanitized["text"], "secret");
        let process_sanitized = sanitize_tool_input("process_run", &input);
        assert_eq!(process_sanitized["args"][0]["bytes"], 8);
        assert_eq!(process_sanitized["args"][1]["bytes"], 17);
        assert!(!process_sanitized["args"].to_string().contains("secret"));
    }

    #[test]
    fn maps_control_tools_to_evidence_kinds() {
        assert_eq!(evidence_kind_for_tool("screenshot"), "frame_captured");
        assert_eq!(evidence_kind_for_tool("click"), "input_sent");
        assert_eq!(evidence_kind_for_tool("ui_set_text"), "input_sent");
        assert_eq!(evidence_kind_for_tool("ui_tree"), "ui_tree_read");
        assert_eq!(evidence_kind_for_tool("process_run"), "process_exited");
        assert_eq!(evidence_kind_for_tool("file_import"), "file_imported");
        assert_eq!(evidence_kind_for_tool("file_export"), "file_exported");
        assert_eq!(
            evidence_kind_for_tool("checkpoint_restore"),
            "checkpoint_restored"
        );
    }

    #[tokio::test]
    async fn notification_has_no_response() {
        let control = McpControlPlane::new("/tmp/winlane-test.sock");
        assert!(
            handle_message(
                &control,
                &json!({"jsonrpc":"2.0","method":"notifications/initialized"})
            )
            .await
            .is_none()
        );
    }
}
