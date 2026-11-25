use serde_json::{Value, json};
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

#[derive(Debug, Error)]
pub enum QemuProtocolError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),
    #[error("invalid JSON response: {0}")]
    Json(#[from] serde_json::Error),
    #[error("QMP error response: {0}")]
    Qmp(String),
    #[error("guest agent error response: {0}")]
    GuestAgent(String),
    #[error("missing field {0}")]
    MissingField(&'static str),
    #[error("invalid base64 output")]
    InvalidBase64,
}

pub type QemuProtocolResult<T> = Result<T, QemuProtocolError>;

struct JsonUnixSocket {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl JsonUnixSocket {
    fn connect(path: &Path, timeout: Duration) -> QemuProtocolResult<Self> {
        let stream = UnixStream::connect(path)?;
        stream.set_read_timeout(Some(timeout))?;
        stream.set_write_timeout(Some(timeout))?;
        let writer = stream.try_clone()?;
        Ok(Self {
            reader: BufReader::new(stream),
            writer,
        })
    }

    fn send(&mut self, value: &Value) -> QemuProtocolResult<()> {
        let mut encoded = serde_json::to_vec(value)?;
        encoded.push(b'\n');
        self.writer.write_all(&encoded)?;
        self.writer.flush()?;
        Ok(())
    }

    fn read_json_line(&mut self) -> QemuProtocolResult<Value> {
        for _ in 0..16 {
            let mut line = Vec::new();
            let bytes = self.reader.read_until(b'\n', &mut line)?;
            if bytes == 0 {
                return Err(QemuProtocolError::Io(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "socket closed",
                )));
            }
            let text = String::from_utf8_lossy(&line);
            let trimmed = text.trim_start_matches(|c: char| c.is_control() && c != '\n');
            if let Ok(value) = serde_json::from_str(trimmed) {
                return Ok(value);
            }
        }
        Err(QemuProtocolError::Json(serde_json::Error::io(
            std::io::Error::new(std::io::ErrorKind::InvalidData, "no JSON response"),
        )))
    }
}

pub struct QmpClient {
    socket: JsonUnixSocket,
}

impl QmpClient {
    pub fn connect(path: &Path, timeout: Duration) -> QemuProtocolResult<Self> {
        let mut socket = JsonUnixSocket::connect(path, timeout)?;
        let _greeting = socket.read_json_line()?;
        socket.send(&json!({"execute": "qmp_capabilities"}))?;
        let response = socket.read_json_line()?;
        if response.get("error").is_some() {
            return Err(QemuProtocolError::Qmp(response.to_string()));
        }
        Ok(Self { socket })
    }

    pub fn command(&mut self, execute: &str, arguments: Value) -> QemuProtocolResult<Value> {
        let request = if arguments.is_null() {
            json!({"execute": execute})
        } else {
            json!({"execute": execute, "arguments": arguments})
        };
        self.socket.send(&request)?;
        for _ in 0..32 {
            let response = self.socket.read_json_line()?;
            if response.get("event").is_some() {
                continue;
            }
            if response.get("error").is_some() {
                return Err(QemuProtocolError::Qmp(response.to_string()));
            }
            return Ok(response);
        }
        Err(QemuProtocolError::Qmp(format!(
            "no command response for {execute}"
        )))
    }

    pub fn screendump(&mut self, output: &Path) -> QemuProtocolResult<()> {
        self.command(
            "screendump",
            json!({"filename": output.to_string_lossy().to_string()}),
        )?;
        Ok(())
    }

    pub fn send_key(&mut self, key: &str) -> QemuProtocolResult<()> {
        self.command(
            "send-key",
            json!({"keys": [{"type": "qcode", "data": key}]}),
        )?;
        Ok(())
    }

    pub fn system_powerdown(&mut self) -> QemuProtocolResult<()> {
        self.command("system_powerdown", Value::Null)?;
        Ok(())
    }
}

pub struct QgaClient {
    socket: JsonUnixSocket,
}

impl QgaClient {
    pub fn connect(path: &Path, timeout: Duration) -> QemuProtocolResult<Self> {
        Ok(Self {
            socket: JsonUnixSocket::connect(path, timeout)?,
        })
    }

    fn execute(&mut self, execute: &str, arguments: Value) -> QemuProtocolResult<Value> {
        let request = if arguments.is_null() {
            json!({"execute": execute})
        } else {
            json!({"execute": execute, "arguments": arguments})
        };
        self.socket.send(&request)?;
        let response = self.socket.read_json_line()?;
        if let Some(error) = response.get("error") {
            return Err(QemuProtocolError::GuestAgent(error.to_string()));
        }
        Ok(response)
    }

    pub fn ping(&mut self) -> bool {
        self.execute("guest-ping", Value::Null).is_ok()
    }

    pub fn exec(&mut self, path: &str, args: &[&str]) -> QemuProtocolResult<i64> {
        let response = self.execute(
            "guest-exec",
            json!({
                "path": path,
                "arg": args,
                "capture-output": true,
            }),
        )?;
        response
            .get("return")
            .and_then(|value| value.get("pid"))
            .and_then(Value::as_i64)
            .ok_or(QemuProtocolError::MissingField("return.pid"))
    }

    pub fn exec_status(&mut self, pid: i64) -> QemuProtocolResult<GuestExecStatus> {
        let response = self.execute("guest-exec-status", json!({"pid": pid}))?;
        let returned = response
            .get("return")
            .ok_or(QemuProtocolError::MissingField("return"))?;
        let exited = returned
            .get("exited")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let exit_code = returned.get("exitcode").and_then(Value::as_i64);
        let stdout = returned
            .get("out-data")
            .and_then(Value::as_str)
            .map(base64_decode)
            .transpose()?
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
        let stderr = returned
            .get("err-data")
            .and_then(Value::as_str)
            .map(base64_decode)
            .transpose()?
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
        Ok(GuestExecStatus {
            exited,
            exit_code,
            stdout,
            stderr,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestExecStatus {
    pub exited: bool,
    pub exit_code: Option<i64>,
    pub stdout: Option<String>,
    pub stderr: Option<String>,
}

fn base64_decode(input: &str) -> QemuProtocolResult<Vec<u8>> {
    let mut output = Vec::new();
    let mut buffer = 0u32;
    let mut bits = 0u32;
    for byte in input.bytes() {
        let value = match byte {
            b'A'..=b'Z' => byte - b'A',
            b'a'..=b'z' => byte - b'a' + 26,
            b'0'..=b'9' => byte - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            b'=' => break,
            b'\r' | b'\n' | b' ' | b'\t' => continue,
            _ => return Err(QemuProtocolError::InvalidBase64),
        };
        buffer = (buffer << 6) | u32::from(value);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            output.push(((buffer >> bits) & 0xff) as u8);
        }
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_base64() {
        assert_eq!(base64_decode("aGVsbG8=").unwrap(), b"hello");
        assert_eq!(base64_decode("aGVsbG8").unwrap(), b"hello");
    }

    #[test]
    fn rejects_invalid_base64() {
        assert!(base64_decode("not***base64").is_err());
    }
}
