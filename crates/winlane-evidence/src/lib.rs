//! Append-only evidence records, content-addressed blobs, validation and export.

use serde_json::Value;
use sha2::{Digest, Sha256};
use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Read, Write};
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};
use thiserror::Error;
use winlane_contracts::{ContractError, EvidenceEvent, EvidenceWriter};

const EVENTS_FILE: &str = "events.jsonl";
const BLOBS_DIR: &str = "blobs";
static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Error)]
pub enum EvidenceError {
    #[error("evidence I/O failed at {path}: {source}")]
    Io { path: PathBuf, source: io::Error },
    #[error("invalid evidence JSON at {path}: {source}")]
    Json {
        path: PathBuf,
        source: serde_json::Error,
    },
    #[error("invalid event kind '{0}'")]
    InvalidKind(String),
    #[error("event payload must be a JSON object")]
    InvalidPayload,
    #[error("event sequence {actual} does not match expected sequence {expected}")]
    SequenceConflict { expected: u64, actual: u64 },
    #[error("blob is missing: {0}")]
    MissingBlob(String),
    #[error("sha256 must be 64 lowercase hex characters")]
    InvalidSha256(String),
}

impl EvidenceError {
    fn io(path: impl Into<PathBuf>) -> impl FnOnce(io::Error) -> Self {
        let path = path.into();
        move |source| Self::Io { path, source }
    }

    fn json(path: impl Into<PathBuf>) -> impl FnOnce(serde_json::Error) -> Self {
        let path = path.into();
        move |source| Self::Json { path, source }
    }
}

pub type EvidenceResult<T> = Result<T, EvidenceError>;

fn create_private_dir_all(path: &Path) -> EvidenceResult<()> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(EvidenceError::io(path))
}

fn create_private_file(path: &Path) -> EvidenceResult<File> {
    OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(path)
        .map_err(EvidenceError::io(path))
}

fn set_private_file_if_new(path: &Path, existed: bool) -> EvidenceResult<()> {
    if existed {
        return Ok(());
    }
    let mut permissions = fs::metadata(path)
        .map_err(EvidenceError::io(path))?
        .permissions();
    permissions.set_mode(0o600);
    fs::set_permissions(path, permissions).map_err(EvidenceError::io(path))
}

#[derive(Debug)]
pub struct EvidenceStore {
    root: PathBuf,
    events_path: PathBuf,
    blobs_dir: PathBuf,
    next_sequence: u64,
}

impl EvidenceStore {
    pub fn create(root: impl AsRef<Path>) -> EvidenceResult<Self> {
        let root = root.as_ref().to_path_buf();
        create_private_dir_all(&root.join(BLOBS_DIR))?;
        let events_path = root.join(EVENTS_FILE);
        if events_path.exists() {
            return Self::open(root);
        }
        create_private_file(&events_path)?;
        Ok(Self {
            events_path,
            blobs_dir: root.join(BLOBS_DIR),
            root,
            next_sequence: 0,
        })
    }

    pub fn open(root: impl AsRef<Path>) -> EvidenceResult<Self> {
        let root = root.as_ref().to_path_buf();
        let events_path = root.join(EVENTS_FILE);
        let blobs_dir = root.join(BLOBS_DIR);
        create_private_dir_all(&blobs_dir)?;
        if !events_path.exists() {
            create_private_file(&events_path)?;
        }

        let file = File::open(&events_path).map_err(EvidenceError::io(&events_path))?;
        let mut expected = 0;
        for line in BufReader::new(file).lines() {
            let line = line.map_err(EvidenceError::io(&events_path))?;
            if line.trim().is_empty() {
                continue;
            }
            let event: EvidenceEvent =
                serde_json::from_str(&line).map_err(EvidenceError::json(&events_path))?;
            validate_event(&event)?;
            if event.sequence != expected {
                return Err(EvidenceError::SequenceConflict {
                    expected,
                    actual: event.sequence,
                });
            }
            expected += 1;
        }

        Ok(Self {
            root,
            events_path,
            blobs_dir,
            next_sequence: expected,
        })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn next_sequence(&self) -> u64 {
        self.next_sequence
    }

    pub fn record(
        &mut self,
        kind: impl Into<String>,
        machine_id: Option<String>,
        payload: Value,
    ) -> EvidenceResult<EvidenceEvent> {
        self.record_at(kind, machine_id, payload, unix_ms_now())
    }

    pub fn record_at(
        &mut self,
        kind: impl Into<String>,
        machine_id: Option<String>,
        payload: Value,
        unix_ms: u64,
    ) -> EvidenceResult<EvidenceEvent> {
        let event = EvidenceEvent {
            sequence: self.next_sequence,
            unix_ms,
            machine_id,
            kind: kind.into(),
            payload,
        };
        self.append_event(event.clone())?;
        Ok(event)
    }

    pub fn append_event(&mut self, event: EvidenceEvent) -> EvidenceResult<()> {
        validate_event(&event)?;
        if event.sequence != self.next_sequence {
            return Err(EvidenceError::SequenceConflict {
                expected: self.next_sequence,
                actual: event.sequence,
            });
        }
        let encoded =
            serde_json::to_string(&event).map_err(EvidenceError::json(&self.events_path))?;
        let mut file = OpenOptions::new()
            .append(true)
            .open(&self.events_path)
            .map_err(EvidenceError::io(&self.events_path))?;
        file.write_all(encoded.as_bytes())
            .and_then(|()| file.write_all(b"\n"))
            .and_then(|()| file.sync_data())
            .map_err(EvidenceError::io(&self.events_path))?;
        self.next_sequence += 1;
        Ok(())
    }

    pub fn read_events(&self) -> EvidenceResult<Vec<EvidenceEvent>> {
        let file = File::open(&self.events_path).map_err(EvidenceError::io(&self.events_path))?;
        let mut events = Vec::new();
        for line in BufReader::new(file).lines() {
            let line = line.map_err(EvidenceError::io(&self.events_path))?;
            if line.trim().is_empty() {
                continue;
            }
            let event: EvidenceEvent =
                serde_json::from_str(&line).map_err(EvidenceError::json(&self.events_path))?;
            validate_event(&event)?;
            events.push(event);
        }
        Ok(events)
    }

    pub fn put_blob(&self, bytes: &[u8]) -> EvidenceResult<String> {
        let sha256 = sha256_hex(bytes);
        validate_sha256(&sha256)?;
        let final_path = self.blobs_dir.join(&sha256);
        if final_path.exists() {
            return Ok(sha256);
        }

        let tmp_path = self.blobs_dir.join(format!(
            ".tmp-{}-{}-{}",
            std::process::id(),
            unix_ms_now(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        create_private_file(&tmp_path)?
            .write_all(bytes)
            .map_err(EvidenceError::io(&tmp_path))?;
        fs::rename(&tmp_path, &final_path).map_err(EvidenceError::io(&final_path))?;
        Ok(sha256)
    }

    pub fn put_blob_reader(&self, mut reader: impl Read) -> EvidenceResult<String> {
        let tmp_path = self.blobs_dir.join(format!(
            ".tmp-{}-{}-{}",
            std::process::id(),
            unix_ms_now(),
            TEMP_COUNTER.fetch_add(1, Ordering::Relaxed)
        ));
        let result = (|| {
            let mut file = create_private_file(&tmp_path)?;
            let mut hasher = Sha256::new();
            let mut buffer = [0_u8; 64 * 1024];
            loop {
                let count = reader
                    .read(&mut buffer)
                    .map_err(EvidenceError::io(&tmp_path))?;
                if count == 0 {
                    break;
                }
                hasher.update(&buffer[..count]);
                file.write_all(&buffer[..count])
                    .map_err(EvidenceError::io(&tmp_path))?;
            }
            file.sync_data().map_err(EvidenceError::io(&tmp_path))?;
            let sha256 = digest_hex(hasher.finalize());
            validate_sha256(&sha256)?;
            let final_path = self.blobs_dir.join(&sha256);
            if final_path.exists() {
                fs::remove_file(&tmp_path).map_err(EvidenceError::io(&tmp_path))?;
            } else {
                fs::rename(&tmp_path, &final_path).map_err(EvidenceError::io(&final_path))?;
            }
            Ok(sha256)
        })();
        if result.is_err() {
            let _ = fs::remove_file(&tmp_path);
        }
        result
    }

    pub fn blob_bytes(&self, sha256: &str) -> EvidenceResult<Vec<u8>> {
        let path = self.blob_path(sha256)?;
        fs::read(&path).map_err(EvidenceError::io(path))
    }

    pub fn blob_path(&self, sha256: &str) -> EvidenceResult<PathBuf> {
        validate_sha256(sha256)?;
        let path = self.blobs_dir.join(sha256);
        if !path.exists() {
            return Err(EvidenceError::MissingBlob(sha256.to_owned()));
        }
        Ok(path)
    }

    pub fn expected_blob_path(&self, sha256: &str) -> EvidenceResult<PathBuf> {
        validate_sha256(sha256)?;
        Ok(self.blobs_dir.join(sha256))
    }

    pub fn export_jsonl(&self, destination: impl AsRef<Path>) -> EvidenceResult<()> {
        let destination = destination.as_ref();
        if let Some(parent) = destination.parent() {
            create_private_dir_all(parent)?;
        }
        let existed = destination.exists();
        fs::copy(&self.events_path, destination).map_err(EvidenceError::io(destination))?;
        set_private_file_if_new(destination, existed)?;
        Ok(())
    }
}

impl EvidenceWriter for EvidenceStore {
    fn append(
        &mut self,
        event: EvidenceEvent,
    ) -> impl Future<Output = winlane_contracts::ContractResult<()>> + Send {
        let result = self
            .append_event(event)
            .map_err(|error| ContractError::new(error.to_string()));
        std::future::ready(result)
    }
}

pub fn validate_event(event: &EvidenceEvent) -> EvidenceResult<()> {
    validate_kind(&event.kind)?;
    if !event.payload.is_object() {
        return Err(EvidenceError::InvalidPayload);
    }
    Ok(())
}

pub fn validate_kind(kind: &str) -> EvidenceResult<()> {
    let valid = !kind.is_empty()
        && kind.len() <= 80
        && kind
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_');
    if valid {
        Ok(())
    } else {
        Err(EvidenceError::InvalidKind(kind.to_owned()))
    }
}

pub fn required_first_pass_kinds() -> &'static [&'static str] {
    &[
        "machine_created",
        "machine_started",
        "frame_captured",
        "input_sent",
        "checkpoint_created",
        "checkpoint_restored",
        "file_imported",
        "file_exported",
        "process_started",
        "process_exited",
        "network_policy_changed",
        "error",
    ]
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    digest_hex(Sha256::digest(bytes))
}

fn digest_hex(digest: impl AsRef<[u8]>) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = digest.as_ref();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

fn validate_sha256(sha256: &str) -> EvidenceResult<()> {
    if sha256.len() == 64
        && sha256
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        Ok(())
    } else {
        Err(EvidenceError::InvalidSha256(sha256.to_owned()))
    }
}

fn unix_ms_now() -> u64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => duration.as_millis() as u64,
        Err(_) => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn temp_root(name: &str) -> PathBuf {
        let root = std::env::temp_dir().join(format!(
            "winlane-evidence-test-{}-{}",
            name,
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        root
    }

    #[test]
    fn records_and_reopens_ordered_events() {
        let root = temp_root("ordered");
        let mut store = EvidenceStore::create(&root).unwrap();
        store
            .record_at("machine_created", None, json!({"name":"proof"}), 10)
            .unwrap();
        store
            .record_at(
                "input_sent",
                Some("machine-1".into()),
                json!({"kind":"click"}),
                11,
            )
            .unwrap();
        drop(store);

        let reopened = EvidenceStore::open(&root).unwrap();
        assert_eq!(reopened.next_sequence(), 2);
        let events = reopened.read_events().unwrap();
        assert_eq!(events[0].kind, "machine_created");
        assert_eq!(events[1].machine_id.as_deref(), Some("machine-1"));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_sequence_gaps() {
        let root = temp_root("gap");
        let mut store = EvidenceStore::create(&root).unwrap();
        let event = EvidenceEvent {
            sequence: 1,
            unix_ms: 10,
            machine_id: None,
            kind: "error".into(),
            payload: json!({}),
        };
        assert!(matches!(
            store.append_event(event),
            Err(EvidenceError::SequenceConflict { .. })
        ));
        let _ = fs::remove_dir_all(root);
    }

    fn owner_mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn store_and_blob_artifacts_use_owner_only_permissions() {
        let root = temp_root("permissions");
        fs::remove_dir_all(&root).unwrap();
        let store = EvidenceStore::create(&root).unwrap();
        assert_eq!(owner_mode(&root), 0o700);
        assert_eq!(owner_mode(&root.join(EVENTS_FILE)), 0o600);
        assert_eq!(owner_mode(&root.join(BLOBS_DIR)), 0o700);
        let sha = store.put_blob(b"screen").unwrap();
        assert_eq!(owner_mode(&store.blob_path(&sha).unwrap()), 0o600);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stores_blobs_by_content_hash() {
        let root = temp_root("blob");
        let store = EvidenceStore::create(&root).unwrap();
        let sha = store.put_blob(b"screen").unwrap();
        assert_eq!(sha, sha256_hex(b"screen"));
        assert_eq!(store.blob_bytes(&sha).unwrap(), b"screen");
        assert!(store.blob_path("not-a-hash").is_err());
        let _ = fs::remove_dir_all(root);
    }

    struct PatternReader {
        remaining: usize,
        position: usize,
    }

    impl Read for PatternReader {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            if self.remaining == 0 {
                return Ok(0);
            }
            let count = self.remaining.min(output.len());
            for byte in &mut output[..count] {
                *byte = (self.position % 251) as u8;
                self.position += 1;
            }
            self.remaining -= count;
            Ok(count)
        }
    }

    #[test]
    fn streamed_blob_hashes_and_places_large_input_incrementally() {
        let root = temp_root("streamed-blob");
        let store = EvidenceStore::create(&root).unwrap();
        let len: usize = 8 * 1024 * 1024 + 17;
        let mut expected = Sha256::new();
        let mut position = 0_usize;
        let mut chunk = [0_u8; 8192];
        for _ in 0..len.div_ceil(chunk.len()) {
            let count = (len - position).min(chunk.len());
            for byte in &mut chunk[..count] {
                *byte = (position % 251) as u8;
                position += 1;
            }
            expected.update(&chunk[..count]);
        }
        let sha = store
            .put_blob_reader(PatternReader {
                remaining: len,
                position: 0,
            })
            .unwrap();
        assert_eq!(sha, digest_hex(expected.finalize()));
        assert_eq!(
            fs::metadata(store.blob_path(&sha).unwrap()).unwrap().len(),
            len as u64
        );
        let _ = fs::remove_dir_all(root);
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            output[0] = b'x';
            Err(io::Error::new(io::ErrorKind::InvalidData, "reader failed"))
        }
    }

    #[test]
    fn failed_streamed_blob_removes_temporary_artifacts() {
        let root = temp_root("failed-streamed-blob");
        let store = EvidenceStore::create(&root).unwrap();
        assert!(store.put_blob_reader(FailingReader).is_err());
        let entries = fs::read_dir(root.join(BLOBS_DIR))
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert!(entries.is_empty());
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn rejects_non_object_payloads() {
        let event = EvidenceEvent {
            sequence: 0,
            unix_ms: 10,
            machine_id: None,
            kind: "error".into(),
            payload: json!([]),
        };
        assert!(matches!(
            validate_event(&event),
            Err(EvidenceError::InvalidPayload)
        ));
    }
}
