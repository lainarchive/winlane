//! QEMU guest-agent command client.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use std::fs::File;
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::Duration;
use thiserror::Error;

const MAX_QGA_MESSAGE_BYTES: usize = 72 * 1024 * 1024;
const FILE_CHUNK_BYTES: usize = 1024 * 1024;

#[derive(Debug, Error)]
pub enum QgaError {
    #[error("guest agent I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("guest agent JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("guest agent returned {class}: {description}")]
    Guest { class: String, description: String },
    #[error("guest agent response did not contain 'return' or 'error'")]
    UnexpectedResponse,
    #[error("guest agent message exceeded {0} bytes")]
    MessageTooLarge(usize),
    #[error("guest agent base64 failed: {0}")]
    Base64(#[from] base64::DecodeError),
    #[error("guest process {0} is still running")]
    ProcessRunning(u64),
    #[error("sync id {0} exceeds the signed 64-bit QAPI integer range")]
    InvalidSyncId(u64),
    #[error("guest file transfer exceeded {maximum} bytes")]
    TransferTooLarge { maximum: u64 },
}

pub type QgaResult<T> = Result<T, QgaError>;

pub trait QgaTransport {
    fn write_raw(&mut self, bytes: &[u8]) -> io::Result<()>;

    fn write_message(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.write_raw(bytes)?;
        self.write_raw(b"\r\n")
    }

    fn read_message(&mut self) -> io::Result<Vec<u8>>;
}

pub struct UnixQgaTransport {
    reader: BufReader<UnixStream>,
    writer: UnixStream,
}

impl UnixQgaTransport {
    pub fn connect(path: impl AsRef<Path>, timeout: Duration) -> io::Result<Self> {
        let writer = UnixStream::connect(path)?;
        writer.set_read_timeout(Some(timeout))?;
        writer.set_write_timeout(Some(timeout))?;
        let reader = BufReader::new(writer.try_clone()?);
        Ok(Self { reader, writer })
    }

    pub fn from_stream(writer: UnixStream) -> io::Result<Self> {
        let reader = BufReader::new(writer.try_clone()?);
        Ok(Self { reader, writer })
    }
}

impl QgaTransport for UnixQgaTransport {
    fn write_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
        self.writer.write_all(bytes)?;
        self.writer.flush()
    }

    fn read_message(&mut self) -> io::Result<Vec<u8>> {
        let mut line = Vec::new();
        loop {
            let available = self.reader.fill_buf()?;
            if available.is_empty() {
                return Err(io::Error::new(
                    io::ErrorKind::UnexpectedEof,
                    "guest agent closed the channel",
                ));
            }
            if let Some(newline) = available.iter().position(|byte| *byte == b'\n') {
                if line.len() + newline > MAX_QGA_MESSAGE_BYTES {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "guest agent message exceeds size limit",
                    ));
                }
                line.extend_from_slice(&available[..newline]);
                self.reader.consume(newline + 1);
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return Ok(line);
            }
            if line.len() + available.len() > MAX_QGA_MESSAGE_BYTES {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "guest agent message exceeds size limit",
                ));
            }
            let consumed = available.len();
            line.extend_from_slice(available);
            self.reader.consume(consumed);
        }
    }
}

pub struct QgaClient<T> {
    transport: T,
}

impl<T: QgaTransport> QgaClient<T> {
    pub fn new(transport: T) -> Self {
        Self { transport }
    }

    pub fn sync(&mut self, id: u64) -> QgaResult<u64> {
        if id > i64::MAX as u64 {
            return Err(QgaError::InvalidSyncId(id));
        }
        let request = serde_json::to_vec(&json!({
            "execute": "guest-sync-delimited",
            "arguments": { "id": id },
        }))?;
        let mut payload = Vec::with_capacity(request.len() + 3);
        payload.push(0xff);
        payload.extend_from_slice(&request);
        payload.extend_from_slice(b"\r\n");
        self.transport.write_raw(&payload)?;

        for _ in 0..16 {
            let message = self.transport.read_message()?;
            let Some(sentinel) = message.iter().position(|byte| *byte == 0xff) else {
                continue;
            };
            let response_bytes = &message[sentinel + 1..];
            if response_bytes.is_empty() {
                continue;
            }
            let Ok(response) = serde_json::from_slice::<Value>(response_bytes) else {
                continue;
            };
            if let Some(value) = response.get("return").and_then(Value::as_u64)
                && value == id
            {
                return Ok(value);
            }
        }
        Err(QgaError::UnexpectedResponse)
    }

    pub fn ping(&mut self) -> QgaResult<()> {
        self.execute("guest-ping", json!({}))?;
        Ok(())
    }

    pub fn exec_start(
        &mut self,
        command: &str,
        args: &[String],
        stdin: Option<&[u8]>,
    ) -> QgaResult<u64> {
        let mut arguments = json!({
            "path": command,
            "arg": args,
            "capture-output": true,
        });
        if let Some(stdin) = stdin {
            arguments["input-data"] = Value::String(STANDARD.encode(stdin));
        }
        let value = self.execute("guest-exec", arguments)?;
        value
            .get("pid")
            .and_then(Value::as_u64)
            .ok_or(QgaError::UnexpectedResponse)
    }

    pub fn exec_status(&mut self, pid: u64) -> QgaResult<GuestExecStatus> {
        let value = self.execute("guest-exec-status", json!({ "pid": pid }))?;
        Ok(serde_json::from_value(value)?)
    }

    pub fn exec_capture(&mut self, pid: u64) -> QgaResult<GuestExecCapture> {
        let status = self.exec_status(pid)?;
        if !status.exited {
            return Err(QgaError::ProcessRunning(pid));
        }
        Ok(GuestExecCapture {
            exit_code: status.exitcode,
            signal: status.signal,
            stdout: decode_optional_base64(status.out_data)?,
            stderr: decode_optional_base64(status.err_data)?,
        })
    }

    pub fn file_open(&mut self, path: &str, mode: &str) -> QgaResult<u64> {
        let value = self.execute("guest-file-open", json!({ "path": path, "mode": mode }))?;
        value.as_u64().ok_or(QgaError::UnexpectedResponse)
    }

    pub fn file_close(&mut self, handle: u64) -> QgaResult<()> {
        self.execute("guest-file-close", json!({ "handle": handle }))?;
        Ok(())
    }

    pub fn file_write(&mut self, handle: u64, bytes: &[u8]) -> QgaResult<()> {
        for chunk in bytes.chunks(FILE_CHUNK_BYTES) {
            self.execute(
                "guest-file-write",
                json!({
                    "handle": handle,
                    "buf-b64": STANDARD.encode(chunk),
                    "count": chunk.len(),
                }),
            )?;
        }
        self.execute("guest-file-flush", json!({ "handle": handle }))?;
        Ok(())
    }

    pub fn file_read(&mut self, handle: u64, count: u64) -> QgaResult<GuestFileRead> {
        let value = self.execute(
            "guest-file-read",
            json!({ "handle": handle, "count": count }),
        )?;
        let wire: GuestFileReadWire = serde_json::from_value(value)?;
        Ok(GuestFileRead {
            bytes: STANDARD.decode(wire.buf_b64.as_bytes())?,
            eof: wire.eof,
        })
    }

    pub fn write_guest_file(&mut self, path: &str, bytes: &[u8]) -> QgaResult<()> {
        let handle = self.file_open(path, "wb")?;
        let write_result = self.file_write(handle, bytes);
        let close_result = self.file_close(handle);
        write_result.and(close_result)
    }

    pub fn read_guest_file(&mut self, path: &str, maximum: u64) -> QgaResult<Vec<u8>> {
        let handle = self.file_open(path, "rb")?;
        let read_result = self.read_guest_file_open(handle, maximum);
        let close_result = self.file_close(handle);
        read_result.and_then(|bytes| close_result.map(|()| bytes))
    }

    fn read_guest_file_open(&mut self, handle: u64, maximum: u64) -> QgaResult<Vec<u8>> {
        let mut output = Vec::new();
        loop {
            let remaining = maximum.saturating_sub(output.len() as u64);
            if remaining == 0 {
                return Err(QgaError::TransferTooLarge { maximum });
            }
            let count = remaining.min(FILE_CHUNK_BYTES as u64);
            let chunk = self.file_read(handle, count)?;
            output.extend_from_slice(&chunk.bytes);
            if chunk.eof {
                return Ok(output);
            }
        }
    }

    fn execute(&mut self, command: &str, arguments: Value) -> QgaResult<Value> {
        let request = json!({
            "execute": command,
            "arguments": arguments,
        });
        self.transport
            .write_message(serde_json::to_string(&request)?.as_bytes())?;
        let response_bytes = self.transport.read_message()?;
        if response_bytes.len() > MAX_QGA_MESSAGE_BYTES {
            return Err(QgaError::MessageTooLarge(response_bytes.len()));
        }
        let response: Value = serde_json::from_slice(&response_bytes)?;
        if let Some(error) = response.get("error") {
            let class = match error.get("class").and_then(Value::as_str) {
                Some(value) => value.to_owned(),
                None => "Unknown".to_owned(),
            };
            let description = match error.get("desc").and_then(Value::as_str) {
                Some(value) => value.to_owned(),
                None => "guest agent command failed".to_owned(),
            };
            return Err(QgaError::Guest { class, description });
        }
        response
            .get("return")
            .cloned()
            .ok_or(QgaError::UnexpectedResponse)
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct GuestExecStatus {
    pub exited: bool,
    pub exitcode: Option<i32>,
    pub signal: Option<i32>,
    #[serde(rename = "out-data")]
    pub out_data: Option<String>,
    #[serde(rename = "err-data")]
    pub err_data: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestExecCapture {
    pub exit_code: Option<i32>,
    pub signal: Option<i32>,
    pub stdout: Option<Vec<u8>>,
    pub stderr: Option<Vec<u8>>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GuestFileRead {
    pub bytes: Vec<u8>,
    pub eof: bool,
}

#[derive(Debug, Deserialize)]
struct GuestFileReadWire {
    #[serde(rename = "buf-b64")]
    buf_b64: String,
    eof: bool,
}

fn decode_optional_base64(value: Option<String>) -> QgaResult<Option<Vec<u8>>> {
    value
        .map(|encoded| STANDARD.decode(encoded.as_bytes()))
        .transpose()
        .map_err(QgaError::Base64)
}

pub fn read_host_file(path: impl AsRef<Path>) -> io::Result<Vec<u8>> {
    File::open(path).and_then(|mut file| {
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes).map(|_| bytes)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    struct ScriptedTransport {
        incoming: VecDeque<Vec<u8>>,
        written: Vec<Vec<u8>>,
    }

    impl QgaTransport for ScriptedTransport {
        fn write_raw(&mut self, bytes: &[u8]) -> io::Result<()> {
            self.written.push(bytes.to_vec());
            Ok(())
        }

        fn read_message(&mut self) -> io::Result<Vec<u8>> {
            self.incoming.pop_front().ok_or_else(|| {
                io::Error::new(io::ErrorKind::UnexpectedEof, "scripted response missing")
            })
        }
    }

    #[test]
    fn parses_guest_error() {
        let transport = ScriptedTransport {
            incoming: VecDeque::from([
                br#"{"error":{"class":"CommandNotFound","desc":"missing"}}"#.to_vec(),
            ]),
            written: vec![],
        };
        let mut client = QgaClient::new(transport);
        assert!(matches!(client.ping(), Err(QgaError::Guest { .. })));
    }

    #[test]
    fn exec_capture_decodes_output() {
        let response = json!({
            "return": {
                "exited": true,
                "exitcode": 0,
                "out-data": STANDARD.encode(b"ok"),
                "err-data": STANDARD.encode(b"warn")
            }
        });
        let transport = ScriptedTransport {
            incoming: VecDeque::from([response.to_string().into_bytes()]),
            written: vec![],
        };
        let mut client = QgaClient::new(transport);
        let capture = client.exec_capture(7).unwrap();
        assert_eq!(capture.exit_code, Some(0));
        assert_eq!(capture.stdout, Some(b"ok".to_vec()));
        assert_eq!(capture.stderr, Some(b"warn".to_vec()));
    }
}
