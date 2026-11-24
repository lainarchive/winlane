//! Windows UI Automation helper payload and blocking guest-control operations.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::time::{Duration, Instant};
use thiserror::Error;
use winlane_contracts::{FileTransferRef, UiNodeRef};

use crate::qga::{QgaClient, QgaError, QgaTransport};

pub const UIA_HELPER_GUEST_PATH: &str = r"C:\Windows\Temp\winlane-uia.ps1";
pub const POWERSHELL_GUEST_PATH: &str =
    r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";

pub const UIA_HELPER_SCRIPT: &str = r##"
param(
    [Parameter(Mandatory=$true)][string]$Mode,
    [string]$Path = "",
    [string]$TextBase64 = "",
    [int]$MaxDepth = 8,
    [int]$MaxNodes = 2000
)

trap {
    [pscustomobject]@{ ok = $false; error = $_.Exception.Message } | ConvertTo-Json -Compress
    exit 1
}

Add-Type -AssemblyName UIAutomationClient
Add-Type -AssemblyName UIAutomationTypes

$script:NodeCount = 0
$script:MaxNodeCount = $MaxNodes

function Control-TypeName($element) {
    $name = $element.Current.ControlType.ProgrammaticName
    if ($null -eq $name) { return "" }
    return $name -replace "^.*\.", ""
}

function Bounds-Array($element) {
    $rect = $element.Current.BoundingRectangle
    return @([int]$rect.X, [int]$rect.Y, [int]$rect.Width, [int]$rect.Height)
}

function Convert-Element($element, [int]$depth, [string]$path) {
    if ($script:NodeCount -ge $script:MaxNodeCount) { return $null }
    $script:NodeCount += 1
    $current = $element.Current
    $children = @()
    if ($depth -lt $MaxDepth) {
        $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
        $child = $walker.GetFirstChild($element)
        $index = 0
        while ($null -ne $child -and $script:NodeCount -lt $script:MaxNodeCount) {
            $item = Convert-Element $child ($depth + 1) "$path/$index"
            if ($null -ne $item) { $children += $item }
            $child = $walker.GetNextSibling($child)
            $index += 1
        }
    }
    $runtimeId = ""
    try { $runtimeId = ($element.GetRuntimeId() -join ".") } catch { $runtimeId = "" }
    return [pscustomobject]@{
        id = $path
        runtimeId = $runtimeId
        name = $current.Name
        automationId = $current.AutomationId
        controlType = Control-TypeName $element
        bounds = Bounds-Array $element
        isEnabled = $current.IsEnabled
        isOffscreen = $current.IsOffscreen
        processId = $current.ProcessId
        children = @($children)
    }
}

function Find-ElementByPath([string]$path) {
    if ($path -notmatch '^0(/[0-9]+)*$') { throw "invalid element path" }
    $parts = $path -split '/'
    $current = [System.Windows.Automation.AutomationElement]::RootElement
    $walker = [System.Windows.Automation.TreeWalker]::ControlViewWalker
    for ($partIndex = 1; $partIndex -lt $parts.Count; $partIndex++) {
        $wanted = [int]$parts[$partIndex]
        $child = $walker.GetFirstChild($current)
        for ($index = 0; $index -lt $wanted; $index++) {
            if ($null -eq $child) { throw "element path no longer exists" }
            $child = $walker.GetNextSibling($child)
        }
        if ($null -eq $child) { throw "element path no longer exists" }
        $current = $child
    }
    return $current
}

if ($Mode -eq "tree") {
    $root = [System.Windows.Automation.AutomationElement]::RootElement
    $tree = Convert-Element $root 0 "0"
    [pscustomobject]@{
        ok = $true
        nodeCount = $script:NodeCount
        truncated = ($script:NodeCount -ge $script:MaxNodeCount)
        root = $tree
    } | ConvertTo-Json -Depth 40 -Compress
    exit 0
}

if ($Mode -eq "invoke") {
    $element = Find-ElementByPath $Path
    $invoke = $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    if ($null -ne $invoke) {
        $invoke.Invoke()
        [pscustomobject]@{ ok = $true; pattern = "invoke" } | ConvertTo-Json -Compress
        exit 0
    }
    $toggle = $element.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
    if ($null -ne $toggle) {
        $toggle.Toggle()
        [pscustomobject]@{ ok = $true; pattern = "toggle" } | ConvertTo-Json -Compress
        exit 0
    }
    $select = $element.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern)
    if ($null -ne $select) {
        $select.Select()
        [pscustomobject]@{ ok = $true; pattern = "selection_item" } | ConvertTo-Json -Compress
        exit 0
    }
    throw "element has no invoke, toggle, or selection-item pattern"
}

if ($Mode -eq "settext") {
    $element = Find-ElementByPath $Path
    $value = $element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    if ($null -eq $value) { throw "element has no value pattern" }
    $text = [System.Text.Encoding]::UTF8.GetString([System.Convert]::FromBase64String($TextBase64))
    $value.SetValue($text)
    [pscustomobject]@{ ok = $true; pattern = "value" } | ConvertTo-Json -Compress
    exit 0
}

throw "unknown mode '$Mode'"
"##;

#[derive(Debug, Error)]
pub enum UiaError {
    #[error(transparent)]
    Qga(#[from] QgaError),
    #[error("host file operation failed at {path}: {source}")]
    HostIo {
        path: String,
        source: std::io::Error,
    },
    #[error("guest JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("guest process timed out after {0} ms")]
    Timeout(u64),
    #[error("guest helper exited with {exit_code:?}: {message}")]
    Helper {
        exit_code: Option<i32>,
        message: String,
    },
    #[error("guest helper response did not decode as UTF-8 JSON")]
    InvalidUtf8,
    #[error("element reference is invalid")]
    InvalidElementRef,
    #[error("text input exceeds {0} bytes")]
    InputTooLarge(usize),
}

pub type UiaResult<T> = Result<T, UiaError>;

pub struct BlockingUiaControl<T> {
    client: QgaClient<T>,
    generation: u64,
    helper_path: String,
}

impl<T: QgaTransport> BlockingUiaControl<T> {
    pub fn new(client: QgaClient<T>) -> Self {
        Self {
            client,
            generation: 0,
            helper_path: UIA_HELPER_GUEST_PATH.to_owned(),
        }
    }

    pub fn client_mut(&mut self) -> &mut QgaClient<T> {
        &mut self.client
    }

    pub fn install_helper(&mut self) -> UiaResult<FileTransferRef> {
        let bytes = UIA_HELPER_SCRIPT.as_bytes();
        self.client.write_guest_file(&self.helper_path, bytes)?;
        Ok(FileTransferRef {
            sha256: sha256_hex(bytes),
            bytes: bytes.len() as u64,
            host_path: "<embedded>".to_owned(),
            guest_path: self.helper_path.clone(),
        })
    }

    pub fn ui_tree(&mut self, max_depth: u16, max_nodes: u32) -> UiaResult<Value> {
        self.install_helper()?;
        let depth = max_depth.to_string();
        let nodes = max_nodes.to_string();
        let capture = self.run_powershell(
            &[
                "-Mode".to_owned(),
                "tree".to_owned(),
                "-MaxDepth".to_owned(),
                depth,
                "-MaxNodes".to_owned(),
                nodes,
            ],
            30_000,
        )?;
        let mut value = parse_helper_json(&capture.stdout)?;
        self.generation += 1;
        value["generation"] = Value::from(self.generation);
        Ok(value)
    }

    pub fn import_file_blocking(
        &mut self,
        host_path: &std::path::Path,
        guest_path: &str,
    ) -> UiaResult<FileTransferRef> {
        let bytes = std::fs::read(host_path).map_err(|error| UiaError::HostIo {
            path: host_path.to_string_lossy().into_owned(),
            source: error,
        })?;
        self.client.write_guest_file(guest_path, &bytes)?;
        Ok(FileTransferRef {
            sha256: sha256_hex(&bytes),
            bytes: bytes.len() as u64,
            host_path: host_path.to_string_lossy().into_owned(),
            guest_path: guest_path.to_owned(),
        })
    }

    pub fn export_file_blocking(
        &mut self,
        guest_path: &str,
        host_path: &std::path::Path,
    ) -> UiaResult<FileTransferRef> {
        let bytes = self
            .client
            .read_guest_file(guest_path, crate::MAX_TRANSFER_BYTES)?;
        if let Some(parent) = host_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| UiaError::HostIo {
                path: parent.to_string_lossy().into_owned(),
                source: error,
            })?;
        }
        std::fs::write(host_path, &bytes).map_err(|error| UiaError::HostIo {
            path: host_path.to_string_lossy().into_owned(),
            source: error,
        })?;
        Ok(FileTransferRef {
            sha256: sha256_hex(&bytes),
            bytes: bytes.len() as u64,
            host_path: host_path.to_string_lossy().into_owned(),
            guest_path: guest_path.to_owned(),
        })
    }

    pub fn invoke(&mut self, target: &UiNodeRef) -> UiaResult<Value> {
        validate_element_path(&target.id)?;
        self.install_helper()?;
        self.run_powershell(
            &[
                "-Mode".to_owned(),
                "invoke".to_owned(),
                "-Path".to_owned(),
                target.id.clone(),
            ],
            15_000,
        )
        .and_then(|capture| parse_helper_json(&capture.stdout))
    }

    pub fn set_text(&mut self, target: &UiNodeRef, text: &str) -> UiaResult<Value> {
        const MAX_TEXT_BYTES: usize = 4096;
        validate_element_path(&target.id)?;
        if text.len() > MAX_TEXT_BYTES {
            return Err(UiaError::InputTooLarge(MAX_TEXT_BYTES));
        }
        self.install_helper()?;
        self.run_powershell(
            &[
                "-Mode".to_owned(),
                "settext".to_owned(),
                "-Path".to_owned(),
                target.id.clone(),
                "-TextBase64".to_owned(),
                STANDARD.encode(text.as_bytes()),
            ],
            15_000,
        )
        .and_then(|capture| parse_helper_json(&capture.stdout))
    }

    fn run_powershell(&mut self, args: &[String], timeout_ms: u64) -> UiaResult<ProcessCapture> {
        let mut full_args = vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-ExecutionPolicy".to_owned(),
            "Bypass".to_owned(),
            "-File".to_owned(),
            self.helper_path.clone(),
        ];
        full_args.extend(args.iter().cloned());
        let capture = self.run_process_blocking(POWERSHELL_GUEST_PATH, &full_args, timeout_ms)?;
        if capture.exit_code != Some(0) {
            return Err(UiaError::Helper {
                exit_code: capture.exit_code,
                message: String::from_utf8_lossy(&capture.stderr).into_owned(),
            });
        }
        Ok(capture)
    }

    pub fn run_process_blocking(
        &mut self,
        command: &str,
        args: &[String],
        timeout_ms: u64,
    ) -> UiaResult<ProcessCapture> {
        let pid = self.client.exec_start(command, args, None)?;
        let started = Instant::now();
        loop {
            match self.client.exec_capture(pid) {
                Ok(capture) => {
                    return Ok(ProcessCapture {
                        exit_code: capture.exit_code,
                        stdout: capture.stdout.unwrap_or_default(),
                        stderr: capture.stderr.unwrap_or_default(),
                    });
                }
                Err(QgaError::ProcessRunning(_)) => {
                    if started.elapsed() > Duration::from_millis(timeout_ms) {
                        return Err(UiaError::Timeout(timeout_ms));
                    }
                    std::thread::sleep(Duration::from_millis(100));
                }
                Err(error) => return Err(UiaError::Qga(error)),
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCapture {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

fn parse_helper_json(stdout: &[u8]) -> UiaResult<Value> {
    let text = std::str::from_utf8(stdout).map_err(|_| UiaError::InvalidUtf8)?;
    let value: Value = serde_json::from_str(text.trim())?;
    if value.get("ok").and_then(Value::as_bool) == Some(true) {
        Ok(value)
    } else {
        let message = value
            .get("error")
            .and_then(Value::as_str)
            .unwrap_or("guest helper failed")
            .to_owned();
        Err(UiaError::Helper {
            exit_code: Some(1),
            message,
        })
    }
}

fn validate_element_path(path: &str) -> UiaResult<()> {
    let valid = path.len() <= 512
        && (path == "0"
            || path.strip_prefix("0/").is_some_and(|rest| {
                rest.split('/')
                    .all(|part| !part.is_empty() && part.chars().all(|c| c.is_ascii_digit()))
            }));
    if valid {
        Ok(())
    } else {
        Err(UiaError::InvalidElementRef)
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    use sha2::{Digest, Sha256};
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push(HEX[(byte >> 4) as usize] as char);
        output.push(HEX[(byte & 0x0f) as usize] as char);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn helper_contains_uia_operations() {
        assert!(UIA_HELPER_SCRIPT.contains("UIAutomationClient"));
        assert!(UIA_HELPER_SCRIPT.contains("InvokePattern"));
        assert!(UIA_HELPER_SCRIPT.contains("ValuePattern"));
        assert!(UIA_HELPER_SCRIPT.contains("ConvertTo-Json"));
    }

    #[test]
    fn validates_element_paths() {
        assert!(validate_element_path("0").is_ok());
        assert!(validate_element_path("0/2/14").is_ok());
        assert!(validate_element_path("../0").is_err());
        assert!(validate_element_path("0/abc").is_err());
        assert!(validate_element_path("00").is_err());
    }

    #[test]
    fn parses_successful_helper_json() {
        let value = parse_helper_json(br#"{"ok":true,"generation":0}"#).unwrap();
        assert_eq!(value["ok"], true);
    }
}
