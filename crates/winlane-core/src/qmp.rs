use std::collections::VecDeque;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::UnixStream;
use tokio::net::unix::OwnedWriteHalf;
use tokio::sync::{Mutex, broadcast, mpsc};
use tokio::task::JoinHandle;
use tokio::time::{Instant, timeout, timeout_at};

const MAX_QMP_MESSAGE_BYTES: usize = 1024 * 1024;
const COMMAND_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Error)]
pub enum QmpError {
    #[error("qmp io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("qmp protocol error: {0}")]
    Protocol(String),
    #[error("qmp command failed: {class}: {description}")]
    Command { class: String, description: String },
    #[error("qmp connection closed")]
    Closed,
    #[error("qmp command timed out")]
    Timeout,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct QmpEvent {
    pub event: String,
    #[serde(default)]
    pub data: Value,
    #[serde(default)]
    pub timestamp: Option<Value>,
}

type ReaderMessage = Result<Value, String>;

struct QmpClientInner {
    writer: Mutex<OwnedWriteHalf>,
    responses: Mutex<mpsc::Receiver<ReaderMessage>>,
    events: broadcast::Sender<QmpEvent>,
    command_lock: Mutex<()>,
    next_id: AtomicU64,
    poisoned: AtomicBool,
    reader_task: JoinHandle<()>,
}

impl Drop for QmpClientInner {
    fn drop(&mut self) {
        self.reader_task.abort();
    }
}

/// Async QMP client with serialized commands and a background event stream.
#[derive(Clone)]
pub struct QmpClient {
    inner: Arc<QmpClientInner>,
}

impl QmpClient {
    pub async fn connect(path: impl AsRef<Path>) -> Result<Self, QmpError> {
        let stream = UnixStream::connect(path).await?;
        let (read_half, write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);
        let greeting = timeout(COMMAND_TIMEOUT, read_message(&mut reader))
            .await
            .map_err(|_| QmpError::Timeout)??;
        if greeting.get("QMP").is_none() {
            return Err(QmpError::Protocol(
                "first socket message was not a QMP greeting".to_string(),
            ));
        }

        let (response_tx, response_rx) = mpsc::channel(64);
        let (event_tx, _) = broadcast::channel(256);
        let reader_events = event_tx.clone();
        let reader_task = tokio::spawn(async move {
            loop {
                match read_message(&mut reader).await {
                    Ok(message) => {
                        if let Some(event) = parse_event(&message) {
                            let _ = reader_events.send(event);
                        } else if response_tx.send(Ok(message)).await.is_err() {
                            break;
                        }
                    }
                    Err(error) => {
                        let _ = response_tx.send(Err(error.to_string())).await;
                        break;
                    }
                }
            }
        });

        let client = Self {
            inner: Arc::new(QmpClientInner {
                writer: Mutex::new(write_half),
                responses: Mutex::new(response_rx),
                events: event_tx,
                command_lock: Mutex::new(()),
                next_id: AtomicU64::new(1),
                poisoned: AtomicBool::new(false),
                reader_task,
            }),
        };
        client.execute("qmp_capabilities", None).await?;
        Ok(client)
    }

    pub async fn execute(
        &self,
        command: &str,
        arguments: Option<Value>,
    ) -> Result<Value, QmpError> {
        self.execute_with_timeout(command, arguments, COMMAND_TIMEOUT)
            .await
    }

    pub async fn execute_with_timeout(
        &self,
        command: &str,
        arguments: Option<Value>,
        command_timeout: Duration,
    ) -> Result<Value, QmpError> {
        if self.inner.poisoned.load(Ordering::SeqCst) {
            return Err(QmpError::Closed);
        }

        let _serialization = self.inner.command_lock.lock().await;
        let id = self.inner.next_id.fetch_add(1, Ordering::SeqCst);
        let mut message = serde_json::Map::new();
        message.insert("execute".to_string(), Value::from(command));
        if let Some(arguments) = arguments {
            message.insert("arguments".to_string(), arguments);
        }
        message.insert("id".to_string(), Value::from(id));
        let mut encoded = serde_json::to_string(&Value::Object(message))
            .map_err(|error| QmpError::Protocol(error.to_string()))?;
        encoded.push('\n');

        {
            let mut writer = self.inner.writer.lock().await;
            writer.write_all(encoded.as_bytes()).await?;
            writer.flush().await?;
        }

        let deadline = Instant::now() + command_timeout;
        let received = timeout_at(deadline, async {
            let mut responses = self.inner.responses.lock().await;
            responses.recv().await
        })
        .await;

        let message = match received {
            Ok(Some(Ok(message))) => message,
            Ok(Some(Err(error))) => {
                self.inner.poisoned.store(true, Ordering::SeqCst);
                return Err(QmpError::Protocol(error));
            }
            Ok(None) => {
                self.inner.poisoned.store(true, Ordering::SeqCst);
                return Err(QmpError::Closed);
            }
            Err(_) => {
                self.inner.poisoned.store(true, Ordering::SeqCst);
                return Err(QmpError::Timeout);
            }
        };

        if message.get("id") != Some(&Value::from(id)) {
            self.inner.poisoned.store(true, Ordering::SeqCst);
            return Err(QmpError::Protocol(
                "response id did not match outstanding command".to_string(),
            ));
        }

        if let Some(error) = message.get("error") {
            let class = error
                .get("class")
                .and_then(Value::as_str)
                .unwrap_or("unknown")
                .to_string();
            let description = error
                .get("desc")
                .or_else(|| error.get("description"))
                .and_then(Value::as_str)
                .unwrap_or("command failed")
                .to_string();
            return Err(QmpError::Command { class, description });
        }

        Ok(message.get("return").cloned().unwrap_or(Value::Null))
    }

    pub fn subscribe_events(&self) -> broadcast::Receiver<QmpEvent> {
        self.inner.events.subscribe()
    }
}

fn parse_event(message: &Value) -> Option<QmpEvent> {
    let event = message.get("event")?.as_str()?.to_string();
    Some(QmpEvent {
        event,
        data: message.get("data").cloned().unwrap_or(Value::Null),
        timestamp: message.get("timestamp").cloned(),
    })
}

async fn read_message(
    reader: &mut BufReader<tokio::net::unix::OwnedReadHalf>,
) -> Result<Value, QmpError> {
    let mut bytes = VecDeque::new();
    loop {
        let byte = reader.read_u8().await.map_err(|error| {
            if error.kind() == std::io::ErrorKind::UnexpectedEof {
                QmpError::Closed
            } else {
                QmpError::Io(error)
            }
        })?;
        if byte == b'\n' {
            break;
        }
        if bytes.len() >= MAX_QMP_MESSAGE_BYTES {
            return Err(QmpError::Protocol(
                "QMP message exceeded one MiB".to_string(),
            ));
        }
        bytes.push_back(byte);
    }
    let bytes = Vec::from(bytes);
    serde_json::from_slice(&bytes).map_err(|error| QmpError::Protocol(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_qmp_event() {
        let message = serde_json::json!({
            "event": "STOP",
            "data": {"reason": "test"},
            "timestamp": {"seconds": 1, "microseconds": 2}
        });
        let event = parse_event(&message).expect("event should parse");
        assert_eq!(event.event, "STOP");
        assert_eq!(event.data["reason"], "test");
    }
}
