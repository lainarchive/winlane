//! Newline-delimited JSON-RPC stdio transport for MCP clients.

use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

use crate::{McpControlPlane, McpError, McpResult, tool_definitions};

const MAX_MESSAGE_BYTES: usize = 1024 * 1024;
const PROTOCOL_VERSION: &str = "2024-11-05";

pub async fn serve_stdio(control_socket: impl Into<std::path::PathBuf>) -> McpResult<()> {
    let control = McpControlPlane::new(control_socket);
    serve_stdio_with_control(&control).await
}

pub async fn serve_stdio_with_control(control: &McpControlPlane) -> McpResult<()> {
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
        if let Some(response) = handle_message(control, &value).await {
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
        "tools/call" => match tool_call(control, message.get("params")).await {
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

async fn tool_call(control: &McpControlPlane, params: Option<&Value>) -> McpResult<Value> {
    let params = params.ok_or_else(|| McpError::InvalidInput("missing params".to_string()))?;
    let name = params
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| McpError::InvalidInput("missing tool name".to_string()))?;
    let arguments = params
        .get("arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let result = control.dispatch(name, &arguments).await?;
    let text = serde_json::to_string_pretty(&result)?;
    Ok(json!({
        "content": [{"type":"text","text":text}],
        "structuredContent": {"result": result},
        "isError": false
    }))
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
