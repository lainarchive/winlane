use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Write};
use std::os::unix::fs::{DirBuilderExt, PermissionsExt};
use std::path::{Path, PathBuf};

use serde::Serialize;
use serde_json::json;
use sha2::{Digest, Sha256};
use winlane_contracts::EvidenceEvent;
use winlane_evidence::{EvidenceStore, validate_event};

use crate::args::{EvidenceCommand, EvidenceExportArgs, EvidenceTailArgs, EvidenceVerifyArgs};
use crate::machine::write_json_line;
use crate::{CliError, CliResult};

pub fn run_evidence_command(command: &EvidenceCommand, output: &mut impl Write) -> CliResult<()> {
    match command {
        EvidenceCommand::Export(args) => export(args, output),
        EvidenceCommand::Tail(args) => tail(args, output),
        EvidenceCommand::Verify(args) => verify(args, output),
    }
}

fn create_private_dir_all(path: &Path) -> CliResult<()> {
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)?;
    Ok(())
}

fn set_private_file(path: &Path) -> CliResult<()> {
    let mut permissions = std::fs::metadata(path)?.permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions)?;
    Ok(())
}

fn export(args: &EvidenceExportArgs, output: &mut impl Write) -> CliResult<()> {
    if args.output.as_os_str().is_empty() {
        return Err(CliError::Usage("output path must be nonempty".to_string()));
    }
    open_existing(&args.evidence_dir)?;
    if args.output.exists() {
        let metadata = std::fs::metadata(&args.output)?;
        if !metadata.is_dir() || std::fs::read_dir(&args.output)?.next().is_some() {
            return Err(CliError::Usage(format!(
                "evidence export output must be absent or an empty directory: {}",
                args.output.display()
            )));
        }
    }
    create_private_dir_all(&args.output)?;
    let events_output = args.output.join("events.jsonl");
    std::fs::copy(args.evidence_dir.join("events.jsonl"), &events_output)?;
    set_private_file(&events_output)?;
    let events_bytes = std::fs::metadata(&events_output)?.len();

    let blobs_output = args.output.join("blobs");
    create_private_dir_all(&blobs_output)?;
    let mut blob_count = 0_u64;
    let mut blob_bytes = 0_u64;
    let blobs_input = args.evidence_dir.join("blobs");
    if blobs_input.is_dir() {
        for entry in std::fs::read_dir(blobs_input)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let name = entry.file_name();
            let valid_blob_name = name.to_str().is_some_and(|name| {
                name.len() == 64
                    && name
                        .bytes()
                        .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
            });
            if file_type.is_file() && valid_blob_name {
                let target = blobs_output.join(&name);
                blob_bytes += std::fs::copy(entry.path(), &target)?;
                set_private_file(&target)?;
                blob_count += 1;
            }
        }
    }

    write_json_line(
        output,
        &json!({
            "exported": true,
            "evidence_dir": args.evidence_dir,
            "output": args.output,
            "events_bytes": events_bytes,
            "blob_count": blob_count,
            "blob_bytes": blob_bytes,
        }),
    )
}

fn tail(args: &EvidenceTailArgs, output: &mut impl Write) -> CliResult<()> {
    let store = open_existing(&args.evidence_dir)?;
    let events = store.read_events()?;
    for event in events.iter().skip(events.len().saturating_sub(args.limit)) {
        serde_json::to_writer(&mut *output, event)?;
        output.write_all(b"\n")?;
    }
    Ok(())
}

#[derive(Debug, Serialize)]
struct VerifyFailure {
    #[serde(rename = "type")]
    kind: &'static str,
    line: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    sequence: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    path: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    hash: Option<String>,
    detail: String,
}

#[derive(Debug, Serialize)]
struct VerifyReport {
    ok: bool,
    evidence_dir: PathBuf,
    events_total: u64,
    events_valid: u64,
    blob_references: u64,
    unique_blob_references: u64,
    blobs_verified: u64,
    failure_count: u64,
    failures: Vec<VerifyFailure>,
    verifies: &'static str,
    not_verified: &'static str,
}

struct BlobReference {
    line: u64,
    sequence: Option<u64>,
    path: String,
    value: Option<String>,
}

#[derive(Clone)]
enum BlobVerification {
    Verified,
    Missing,
    Mismatch(String),
    ReadError(String),
}

fn verify(args: &EvidenceVerifyArgs, output: &mut impl Write) -> CliResult<()> {
    if args.evidence_dir.as_os_str().is_empty() || !args.evidence_dir.is_dir() {
        return Err(CliError::Usage(format!(
            "evidence directory does not exist: {}",
            args.evidence_dir.display()
        )));
    }
    let events_path = args.evidence_dir.join("events.jsonl");
    if !events_path.is_file() {
        return Err(CliError::Usage(format!(
            "evidence events file does not exist: {}",
            events_path.display()
        )));
    }
    let report = verify_report(&args.evidence_dir, &events_path)?;
    let failed = !report.ok;
    let report_json = serde_json::to_value(&report)?;
    write_json_line(output, &report_json)?;
    if failed {
        Err(CliError::Operation(format!(
            "evidence verification failed with {} failure(s)",
            report.failure_count
        )))
    } else {
        Ok(())
    }
}

fn verify_report(evidence_dir: &Path, events_path: &Path) -> CliResult<VerifyReport> {
    let contents = std::fs::read_to_string(events_path)?;
    let mut events_total = 0_u64;
    let mut events_valid = 0_u64;
    let mut expected_sequence = 0_u64;
    let mut failures = Vec::new();
    let mut references = Vec::new();
    for (index, line) in contents.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let line_number = index as u64 + 1;
        events_total += 1;
        let event: EvidenceEvent = match serde_json::from_str(line) {
            Ok(event) => event,
            Err(error) => {
                failures.push(VerifyFailure {
                    kind: "invalid_json",
                    line: line_number,
                    sequence: None,
                    path: None,
                    hash: None,
                    detail: format!("event line is not valid evidence JSON: {error}"),
                });
                continue;
            }
        };
        if event.sequence != expected_sequence {
            failures.push(VerifyFailure {
                kind: "sequence_mismatch",
                line: line_number,
                sequence: Some(event.sequence),
                path: None,
                hash: None,
                detail: format!(
                    "event sequence {} does not match expected sequence {expected_sequence}",
                    event.sequence
                ),
            });
        }
        expected_sequence = event.sequence.saturating_add(1);
        if let Err(error) = validate_event(&event) {
            failures.push(VerifyFailure {
                kind: "invalid_event",
                line: line_number,
                sequence: Some(event.sequence),
                path: None,
                hash: None,
                detail: error.to_string(),
            });
            continue;
        }
        events_valid += 1;
        collect_blob_references(
            &event.payload,
            "payload",
            line_number,
            Some(event.sequence),
            &mut references,
        );
    }

    let blobs_dir = evidence_dir.join("blobs");
    let mut cache: HashMap<String, BlobVerification> = HashMap::new();
    let mut unique_hashes = HashSet::new();
    let mut blobs_verified = 0_u64;
    for reference in references.iter() {
        let Some(hash) = reference.value.as_deref() else {
            failures.push(blob_failure(
                "malformed_reference",
                reference,
                None,
                "blob reference is not a string".to_string(),
            ));
            continue;
        };
        if !is_lower_hex_sha256(hash) {
            failures.push(blob_failure(
                "malformed_hash",
                reference,
                Some(hash),
                "blob reference is not 64 lowercase hexadecimal characters".to_string(),
            ));
            continue;
        }
        unique_hashes.insert(hash.to_string());
        if !cache.contains_key(hash) {
            let path = blobs_dir.join(hash);
            let result = verify_blob(&path, hash);
            if matches!(result, BlobVerification::Verified) {
                blobs_verified += 1;
            }
            cache.insert(hash.to_string(), result);
        }
        if let Some(result) = cache.get(hash) {
            match result {
                BlobVerification::Verified => {}
                BlobVerification::Missing => failures.push(blob_failure(
                    "missing_blob",
                    reference,
                    Some(hash),
                    "referenced blob does not exist".to_string(),
                )),
                BlobVerification::Mismatch(actual) => failures.push(blob_failure(
                    "hash_mismatch",
                    reference,
                    Some(hash),
                    format!("blob content hashes to {actual}"),
                )),
                BlobVerification::ReadError(error) => failures.push(blob_failure(
                    "blob_read_error",
                    reference,
                    Some(hash),
                    format!("could not read blob: {error}"),
                )),
            }
        }
    }
    let failure_count = failures.len() as u64;
    Ok(VerifyReport {
        ok: failure_count == 0,
        evidence_dir: evidence_dir.to_path_buf(),
        events_total,
        events_valid,
        blob_references: references.len() as u64,
        unique_blob_references: unique_hashes.len() as u64,
        blobs_verified,
        failure_count,
        failures,
        verifies: "event JSON shape/kind/sequence rules and recursive string-valued payload fields named 'blob'",
        not_verified: "hashes stored under other field names, non-string blob fields beyond reporting them malformed, and unreferenced files in blobs/",
    })
}

fn blob_failure(
    kind: &'static str,
    reference: &BlobReference,
    hash: Option<&str>,
    detail: String,
) -> VerifyFailure {
    VerifyFailure {
        kind,
        line: reference.line,
        sequence: reference.sequence,
        path: Some(reference.path.clone()),
        hash: hash.map(str::to_string),
        detail,
    }
}

fn collect_blob_references(
    value: &serde_json::Value,
    path: &str,
    line: u64,
    sequence: Option<u64>,
    references: &mut Vec<BlobReference>,
) {
    match value {
        serde_json::Value::Object(object) => {
            for (key, child) in object {
                let child_path = format!("{path}.{key}");
                if key == "blob" {
                    references.push(BlobReference {
                        line,
                        sequence,
                        path: child_path,
                        value: child.as_str().map(str::to_string),
                    });
                } else {
                    collect_blob_references(child, &child_path, line, sequence, references);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                collect_blob_references(
                    child,
                    &format!("{path}[{index}]"),
                    line,
                    sequence,
                    references,
                );
            }
        }
        _ => {}
    }
}

fn is_lower_hex_sha256(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn verify_blob(path: &Path, expected: &str) -> BlobVerification {
    if !path.is_file() {
        return BlobVerification::Missing;
    }
    match stream_file_sha256(path) {
        Ok(actual) if actual == expected => BlobVerification::Verified,
        Ok(actual) => BlobVerification::Mismatch(actual),
        Err(error) => BlobVerification::ReadError(error.to_string()),
    }
}

fn stream_file_sha256(path: &Path) -> io::Result<String> {
    let mut file = std::fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hasher.update(&buffer[..count]);
    }
    Ok(digest_hex(hasher.finalize()))
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

fn open_existing(evidence_dir: &Path) -> CliResult<EvidenceStore> {
    if evidence_dir.as_os_str().is_empty() || !evidence_dir.is_dir() {
        return Err(CliError::Usage(format!(
            "evidence directory does not exist: {}",
            evidence_dir.display()
        )));
    }
    let events_path: PathBuf = evidence_dir.join("events.jsonl");
    if !events_path.is_file() {
        return Err(CliError::Usage(format!(
            "evidence events file does not exist: {}",
            events_path.display()
        )));
    }
    Ok(EvidenceStore::open(evidence_dir)?)
}

#[cfg(test)]
mod tests {
    use serde_json::Value;

    use super::*;
    use crate::args::{EvidenceExportArgs, EvidenceTailArgs, EvidenceVerifyArgs};

    #[test]
    fn tail_prints_recent_events_in_original_order() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = EvidenceStore::create(directory.path()).unwrap();
        for index in 0..4 {
            store
                .record("machine_status", None, json!({"index": index}))
                .unwrap();
        }
        let mut output = Vec::new();
        tail(
            &EvidenceTailArgs {
                evidence_dir: directory.path().to_path_buf(),
                limit: 2,
            },
            &mut output,
        )
        .unwrap();
        let lines = output
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice::<Value>(line).unwrap())
            .collect::<Vec<_>>();
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0]["sequence"], 2);
        assert_eq!(lines[1]["sequence"], 3);
        assert_eq!(lines[0]["payload"]["index"], 2);
    }

    #[test]
    fn export_copies_events_and_reports_output_shape() {
        let directory = tempfile::tempdir().unwrap();
        let output_path = directory.path().join("nested/export");
        let mut store = EvidenceStore::create(directory.path()).unwrap();
        store
            .record("machine_status", None, json!({"ok": true}))
            .unwrap();
        let expected = std::fs::read(directory.path().join("events.jsonl")).unwrap();
        let blob_name = "a".repeat(64);
        std::fs::write(directory.path().join("blobs").join(&blob_name), b"blob").unwrap();
        std::fs::write(directory.path().join("blobs").join(".tmp-ignore"), b"tmp").unwrap();
        let mut output = Vec::new();
        export(
            &EvidenceExportArgs {
                evidence_dir: directory.path().to_path_buf(),
                output: output_path.clone(),
            },
            &mut output,
        )
        .unwrap();
        assert_eq!(
            std::fs::read(output_path.join("events.jsonl")).unwrap(),
            expected
        );
        let value: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["exported"], true);
        assert_eq!(value["events_bytes"], expected.len() as u64);
        assert_eq!(value["blob_count"], 1);
        assert_eq!(value["blob_bytes"], 4);
        assert_eq!(
            std::fs::read(output_path.join("blobs").join(&blob_name)).unwrap(),
            b"blob"
        );
        assert!(!output_path.join("blobs").join(".tmp-ignore").exists());
        assert!(output.ends_with(b"\n"));
        let mode = |path: &Path| std::fs::metadata(path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode(&output_path), 0o700);
        assert_eq!(mode(&output_path.join("events.jsonl")), 0o600);
        assert_eq!(mode(&output_path.join("blobs")), 0o700);
        assert_eq!(mode(&output_path.join("blobs").join(&blob_name)), 0o600);
    }

    fn verify_output(evidence_dir: &Path) -> (CliResult<()>, Value) {
        let mut output = Vec::new();
        let result = verify(
            &EvidenceVerifyArgs {
                evidence_dir: evidence_dir.to_path_buf(),
            },
            &mut output,
        );
        let value = serde_json::from_slice(&output).expect("verification JSON");
        (result, value)
    }

    #[test]
    fn verify_accepts_valid_events_and_stream_hashed_blob_reference() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = EvidenceStore::create(directory.path()).unwrap();
        let blob = store.put_blob(b"blob").unwrap();
        store
            .record(
                "process_exited",
                None,
                json!({"result":{"stdout":{"blob":blob,"bytes":4}}}),
            )
            .unwrap();
        let (result, value) = verify_output(directory.path());
        assert!(result.is_ok());
        assert_eq!(value["ok"], true);
        assert_eq!(value["events_total"], 1);
        assert_eq!(value["events_valid"], 1);
        assert_eq!(value["blob_references"], 1);
        assert_eq!(value["unique_blob_references"], 1);
        assert_eq!(value["blobs_verified"], 1);
        assert_eq!(value["failure_count"], 0);
    }

    #[test]
    fn verify_reports_missing_blob_after_printing_json() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = EvidenceStore::create(directory.path()).unwrap();
        store
            .record(
                "process_exited",
                None,
                json!({"result":{"stdout":{"blob":"a".repeat(64)}}}),
            )
            .unwrap();
        let (result, value) = verify_output(directory.path());
        assert!(matches!(result, Err(CliError::Operation(_))));
        assert_eq!(value["ok"], false);
        assert_eq!(value["failures"][0]["type"], "missing_blob");
        assert_eq!(value["failures"][0]["path"], "payload.result.stdout.blob");
    }

    #[test]
    fn verify_reports_blob_content_hash_mismatch() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = EvidenceStore::create(directory.path()).unwrap();
        let hash = winlane_evidence::sha256_hex(b"correct");
        std::fs::write(directory.path().join("blobs").join(&hash), b"wrong").unwrap();
        store
            .record("ui_tree_read", None, json!({"tree":{"blob":hash}}))
            .unwrap();
        let (result, value) = verify_output(directory.path());
        assert!(result.is_err());
        assert_eq!(value["failures"][0]["type"], "hash_mismatch");
    }

    #[test]
    fn verify_reports_malformed_hash_and_non_string_reference() {
        let directory = tempfile::tempdir().unwrap();
        let mut store = EvidenceStore::create(directory.path()).unwrap();
        store
            .record("process_exited", None, json!({"bad":{"blob":"abc"}}))
            .unwrap();
        store
            .record(
                "process_exited",
                None,
                json!({"bad":{"blob":{"nested":true}}}),
            )
            .unwrap();
        let (result, value) = verify_output(directory.path());
        assert!(result.is_err());
        let types = value["failures"]
            .as_array()
            .unwrap()
            .iter()
            .map(|failure| failure["type"].as_str().unwrap())
            .collect::<Vec<_>>();
        assert_eq!(types, ["malformed_hash", "malformed_reference"]);
        assert_eq!(value["blob_references"], 2);
        assert_eq!(value["unique_blob_references"], 0);
    }

    #[test]
    fn verify_reports_invalid_event_json() {
        let directory = tempfile::tempdir().unwrap();
        EvidenceStore::create(directory.path()).unwrap();
        std::fs::write(directory.path().join("events.jsonl"), "not json\n").unwrap();
        let (result, value) = verify_output(directory.path());
        assert!(result.is_err());
        assert_eq!(value["failures"][0]["type"], "invalid_json");
        assert_eq!(value["events_valid"], 0);
    }

    #[test]
    fn verify_reports_sequence_corruption() {
        let directory = tempfile::tempdir().unwrap();
        EvidenceStore::create(directory.path()).unwrap();
        std::fs::write(
            directory.path().join("events.jsonl"),
            "{\"sequence\":1,\"unix_ms\":10,\"machine_id\":null,\"kind\":\"machine_status\",\"payload\":{}}\n",
        )
        .unwrap();
        let (result, value) = verify_output(directory.path());
        assert!(result.is_err());
        assert_eq!(value["failures"][0]["type"], "sequence_mismatch");
        assert_eq!(value["events_valid"], 1);
    }

    #[test]
    fn missing_evidence_is_rejected_before_export_write() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("missing");
        let output_path = directory.path().join("out.jsonl");
        let mut output = Vec::new();
        let result = export(
            &EvidenceExportArgs {
                evidence_dir: missing,
                output: output_path.clone(),
            },
            &mut output,
        );
        assert!(result.is_err());
        assert!(!output_path.exists());
    }
}
