use std::io::Write;
use std::path::PathBuf;
use std::time::Duration;

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use winlane_contracts::{MachineHandle, UiNodeRef};
use winlane_guest::qga::{QgaClient, QgaTransport, UnixQgaTransport};
use winlane_guest::uia::{BlockingUiaControl, UiaView};

use crate::CliError;
use crate::CliResult;
use crate::args::GuestCommand;
use crate::machine::{resolve_machine, write_json_line};

pub async fn run_guest_command(command: &GuestCommand, output: &mut impl Write) -> CliResult<()> {
    let operation = guest_operation(command)?;
    let machine_json = guest_machine_json(command.machine()).await?;
    let result =
        tokio::task::spawn_blocking(move || execute_guest_operation(&machine_json, operation))
            .await??;
    write_json_line(output, &result)
}

async fn guest_machine_json(machine: &crate::args::GuestMachineArgs) -> CliResult<String> {
    if let Some(machine_json) = &machine.machine_json {
        return Ok(machine_json.clone());
    }
    let control_socket = machine.control_socket.as_deref().ok_or_else(|| {
        CliError::Usage("--control-socket is required with --machine".to_string())
    })?;
    let resolved =
        resolve_machine(control_socket, &machine.machine_json, &machine.machine_name).await?;
    Ok(serde_json::to_string(&resolved)?)
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GuestOperation {
    UiTree {
        max_depth: u16,
        max_nodes: u32,
        view: UiaView,
    },
    Invoke {
        node: String,
        view: UiaView,
        runtime_id: Option<String>,
    },
    SetText {
        node: String,
        text: String,
        view: UiaView,
        runtime_id: Option<String>,
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

fn guest_operation(command: &GuestCommand) -> CliResult<GuestOperation> {
    match command {
        GuestCommand::UiTree(args) => Ok(GuestOperation::UiTree {
            max_depth: args.max_depth,
            max_nodes: args.max_nodes,
            view: args.view,
        }),
        GuestCommand::Invoke(args) => Ok(GuestOperation::Invoke {
            node: args.node.clone(),
            view: args.view,
            runtime_id: args.runtime_id.clone(),
        }),
        GuestCommand::SetText(args) => {
            if args
                .text
                .chars()
                .any(|c| c.is_control() && !matches!(c, '\t' | '\n' | '\r'))
            {
                return Err(CliError::Usage(
                    "set-text contains unsupported control characters".to_string(),
                ));
            }
            Ok(GuestOperation::SetText {
                node: args.node.clone(),
                text: args.text.clone(),
                view: args.view,
                runtime_id: args.runtime_id.clone(),
            })
        }
        GuestCommand::Run(args) => Ok(GuestOperation::Run {
            command: args.command.clone(),
            args: args.args.clone(),
            timeout_ms: args.timeout_ms,
        }),
        GuestCommand::Import(args) => Ok(GuestOperation::Import {
            host_path: args.host_path.clone(),
            guest_path: args.guest_path.clone(),
        }),
        GuestCommand::Export(args) => Ok(GuestOperation::Export {
            guest_path: args.guest_path.clone(),
            host_path: args.host_path.clone(),
        }),
    }
}

fn execute_guest_operation(machine_json: &str, operation: GuestOperation) -> CliResult<Value> {
    let machine: MachineHandle = serde_json::from_str(machine_json)?;
    let qga_socket = PathBuf::from(machine.state_dir).join("qga.sock");
    let transport = UnixQgaTransport::connect(qga_socket, Duration::from_secs(5))?;
    let mut control = BlockingUiaControl::new(QgaClient::new(transport));
    execute_operation(&mut control, &operation)
}

fn execute_operation<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    operation: &GuestOperation,
) -> CliResult<Value> {
    match operation {
        GuestOperation::UiTree {
            max_depth,
            max_nodes,
            view,
        } => Ok(control.ui_tree(*max_depth, *max_nodes, *view)?),
        GuestOperation::Invoke {
            node,
            view,
            runtime_id,
        } => Ok(control.invoke(&node_ref(node, runtime_id.clone()), *view, 30_000)?),
        GuestOperation::SetText {
            node,
            text,
            view,
            runtime_id,
        } => Ok(control.set_text(&node_ref(node, runtime_id.clone()), text, *view, 30_000)?),
        GuestOperation::Run {
            command,
            args,
            timeout_ms,
        } => {
            let capture = control.run_process_blocking(command, args, *timeout_ms)?;
            Ok(json!({
                "exit_code": capture.exit_code,
                "stdout_base64": STANDARD.encode(&capture.stdout),
                "stderr_base64": STANDARD.encode(&capture.stderr),
            }))
        }
        GuestOperation::Import {
            host_path,
            guest_path,
        } => Ok(serde_json::to_value(
            control.import_file_blocking(host_path, guest_path)?,
        )?),
        GuestOperation::Export {
            guest_path,
            host_path,
        } => Ok(serde_json::to_value(
            control.export_file_blocking(guest_path, host_path)?,
        )?),
    }
}

fn node_ref(path: &str, runtime_id: Option<String>) -> UiNodeRef {
    UiNodeRef {
        id: path.to_string(),
        generation: 0,
        runtime_id,
        automation_id: None,
        control_type: None,
        bounds: None,
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::io;
    use std::rc::Rc;

    use winlane_guest::uia::{POWERSHELL_GUEST_PATH, UIA_HELPER_GUEST_PATH, UiaError};

    use super::*;

    type WrittenRequests = Rc<RefCell<Vec<Value>>>;

    struct MockQgaTransport {
        incoming: VecDeque<Vec<u8>>,
        exec_outputs: VecDeque<(i32, Vec<u8>, Vec<u8>)>,
        file_read_bytes: Vec<u8>,
        written: WrittenRequests,
    }

    impl MockQgaTransport {
        fn new(exec_outputs: Vec<(i32, Vec<u8>, Vec<u8>)>, written: WrittenRequests) -> Self {
            let file_read_bytes = exec_outputs
                .first()
                .map(|(_, stdout, _)| stdout.clone())
                .unwrap_or_default();
            Self {
                incoming: VecDeque::new(),
                exec_outputs: exec_outputs.into(),
                file_read_bytes,
                written,
            }
        }
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
                "guest-file-open" => json!(1),
                "guest-file-write" | "guest-file-flush" | "guest-file-close" => json!({}),
                "guest-file-read" => json!({
                    "buf-b64": STANDARD.encode(&self.file_read_bytes),
                    "count": self.file_read_bytes.len(),
                    "eof": true,
                }),
                "guest-exec" => json!({ "pid": 99 }),
                "guest-exec-status" => {
                    let (exit_code, stdout, stderr) =
                        self.exec_outputs
                            .pop_front()
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

    fn control(
        exec_outputs: Vec<(i32, Vec<u8>, Vec<u8>)>,
    ) -> (BlockingUiaControl<MockQgaTransport>, WrittenRequests) {
        let written = Rc::new(RefCell::new(Vec::new()));
        let transport = MockQgaTransport::new(exec_outputs, Rc::clone(&written));
        (BlockingUiaControl::new(QgaClient::new(transport)), written)
    }

    fn task_xml_payloads(requests: &[Value]) -> Vec<String> {
        requests
            .iter()
            .filter(|request| request["execute"] == "guest-file-write")
            .filter_map(|request| request["arguments"]["buf-b64"].as_str().map(str::to_owned))
            .filter_map(|encoded| STANDARD.decode(encoded.as_bytes()).ok())
            .filter(|bytes| bytes.starts_with(&[0xff, 0xfe]))
            .filter_map(|bytes| {
                let units: Vec<u16> = bytes[2..]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|chunk| u16::from_le_bytes(*chunk))
                    .collect();
                String::from_utf16(&units).ok()
            })
            .collect()
    }

    #[test]
    fn run_uses_guest_exec_and_outputs_base64_schema() {
        let (mut control, written) = control(vec![(0, b"out".to_vec(), b"err".to_vec())]);
        let output = execute_operation(
            &mut control,
            &GuestOperation::Run {
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
                "stderr_base64": "ZXJy",
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
        assert_eq!(requests[1]["execute"], "guest-exec-status");
        assert_eq!(requests[1]["arguments"]["pid"], 99);
    }

    #[test]
    fn bare_windows_command_is_rejected_before_transport_writes() {
        let (mut control, written) = control(vec![]);
        let result = execute_operation(
            &mut control,
            &GuestOperation::Run {
                command: "powershell.exe".to_string(),
                args: vec![],
                timeout_ms: 5_000,
            },
        );
        assert!(matches!(
            result,
            Err(CliError::Uia(UiaError::InvalidCommandPath))
        ));
        assert!(written.borrow().is_empty());
    }

    #[test]
    fn ui_tree_installs_helper_and_passes_bounds_to_interactive_task() {
        let helper_stdout = br#"{"ok":true,"root":{"id":"0"},"nodeCount":1}"#.to_vec();
        let (mut control, written) = control(vec![(0, helper_stdout, Vec::new())]);
        let output = execute_operation(
            &mut control,
            &GuestOperation::UiTree {
                max_depth: 4,
                max_nodes: 50,
                view: UiaView::Raw,
            },
        )
        .unwrap();
        assert_eq!(output["ok"], true);
        assert_eq!(output["generation"], 1);
        assert_eq!(output["root"]["id"], "0");

        let requests = written.borrow();
        assert_eq!(requests[0]["execute"], "guest-file-open");
        assert_eq!(requests[0]["arguments"]["path"], UIA_HELPER_GUEST_PATH);
        assert!(
            requests
                .iter()
                .any(|request| request["execute"] == "guest-file-read")
        );
        assert!(requests.iter().any(|request| {
            request["arguments"]["path"] == r"C:\Windows\System32\schtasks.exe"
                && request["arguments"]["arg"]
                    .as_array()
                    .is_some_and(|args| args.iter().any(|arg| arg == "/Create"))
        }));
        let xml = task_xml_payloads(&requests).join(
            "
",
        );
        assert!(xml.contains("InteractiveToken"));
        assert!(xml.contains("-Mode tree -MaxDepth 4 -MaxNodes 50 -View raw -OutputPath"));
        assert!(
            !requests
                .iter()
                .any(|request| request["arguments"]["path"] == POWERSHELL_GUEST_PATH)
        );
    }

    #[test]
    fn invoke_passes_node_path_to_interactive_task() {
        let helper_stdout = br#"{"ok":true,"pattern":"invoke"}"#.to_vec();
        let (mut control, written) = control(vec![(0, helper_stdout, Vec::new())]);
        let output = execute_operation(
            &mut control,
            &GuestOperation::Invoke {
                node: "0/1/2".to_string(),
                view: UiaView::Raw,
                runtime_id: Some("7.42".to_string()),
            },
        )
        .unwrap();
        assert_eq!(output["pattern"], "invoke");
        let xml = task_xml_payloads(&written.borrow()).join(
            "
",
        );
        assert!(xml.contains("-Mode invoke -Path 0/1/2 -View raw -RuntimeId 7.42 -OutputPath"));
    }

    #[test]
    fn set_text_preserves_empty_text_as_empty_base64_argument() {
        let helper_stdout = br#"{"ok":true,"pattern":"value"}"#.to_vec();
        let (mut control, written) = control(vec![(0, helper_stdout, Vec::new())]);
        let output = execute_operation(
            &mut control,
            &GuestOperation::SetText {
                node: "0/1/2".to_string(),
                text: String::new(),
                view: UiaView::Control,
                runtime_id: None,
            },
        )
        .unwrap();
        assert_eq!(output["pattern"], "value");
        let xml = task_xml_payloads(&written.borrow()).join(
            "
",
        );
        assert!(xml.contains("-Mode settext -Path 0/1/2 -TextBase64  -View control -OutputPath"));
    }
}
