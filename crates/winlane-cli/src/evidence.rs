use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::json;
use winlane_evidence::EvidenceStore;

use crate::args::{EvidenceCommand, EvidenceExportArgs, EvidenceTailArgs};
use crate::machine::write_json_line;
use crate::{CliError, CliResult};

pub fn run_evidence_command(command: &EvidenceCommand, output: &mut impl Write) -> CliResult<()> {
    match command {
        EvidenceCommand::Export(args) => export(args, output),
        EvidenceCommand::Tail(args) => tail(args, output),
    }
}

fn export(args: &EvidenceExportArgs, output: &mut impl Write) -> CliResult<()> {
    if args.output.as_os_str().is_empty() {
        return Err(CliError::Usage("output path must be nonempty".to_string()));
    }
    let store = open_existing(&args.evidence_dir)?;
    store.export_jsonl(&args.output)?;
    let bytes = std::fs::metadata(&args.output)?.len();
    write_json_line(
        output,
        &json!({
            "exported": true,
            "evidence_dir": args.evidence_dir,
            "output": args.output,
            "bytes": bytes,
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
    use crate::args::{EvidenceExportArgs, EvidenceTailArgs};

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
        let output_path = directory.path().join("nested/events.jsonl");
        let mut store = EvidenceStore::create(directory.path()).unwrap();
        store
            .record("machine_status", None, json!({"ok": true}))
            .unwrap();
        let expected = std::fs::read(directory.path().join("events.jsonl")).unwrap();
        let mut output = Vec::new();
        export(
            &EvidenceExportArgs {
                evidence_dir: directory.path().to_path_buf(),
                output: output_path.clone(),
            },
            &mut output,
        )
        .unwrap();
        assert_eq!(std::fs::read(&output_path).unwrap(), expected);
        let value: Value = serde_json::from_slice(&output).unwrap();
        assert_eq!(value["exported"], true);
        assert_eq!(value["bytes"], expected.len() as u64);
        assert!(output.ends_with(b"\n"));
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
