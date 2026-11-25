//! Windows UI Automation helper payload and blocking guest-control operations.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use thiserror::Error;
use winlane_contracts::{FileTransferRef, UiNodeRef};

use crate::qga::{QgaClient, QgaError, QgaTransport};

pub const UIA_HELPER_GUEST_PATH: &str = r"C:\Windows\Temp\winlane-uia.ps1";
pub const POWERSHELL_GUEST_PATH: &str =
    r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
const SCHTASKS_GUEST_PATH: &str = r"C:\Windows\System32\schtasks.exe";
const CMD_GUEST_PATH: &str = r"C:\Windows\System32\cmd.exe";
const PROVISIONED_INTERACTIVE_USER: &str = "winlane";
static INTERACTIVE_TASK_COUNTER: AtomicU64 = AtomicU64::new(0);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UiaView {
    Control,
    Raw,
}

impl UiaView {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Raw => "raw",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "control" => Some(Self::Control),
            "raw" => Some(Self::Raw),
            _ => None,
        }
    }
}

pub const UIA_HELPER_SCRIPT: &str = r##"
param(
    [Parameter(Mandatory=$true)][string]$Mode,
    [string]$Path = "",
    [string]$TextBase64 = "",
    [string]$RuntimeId = "",
    [string]$OutputPath = "",
    [int]$MaxDepth = 8,
    [int]$MaxNodes = 2000,
    [ValidateSet("control","raw")][string]$View = "control"
)

function Write-Result($obj) {
    $json = $obj | ConvertTo-Json -Depth 40 -Compress
    if ($OutputPath) {
        $encoding = New-Object System.Text.UTF8Encoding($true)
        $temporaryOutputPath = "$OutputPath.tmp"
        [System.IO.File]::WriteAllText($temporaryOutputPath, $json, $encoding)
        [System.IO.File]::Move($temporaryOutputPath, $OutputPath)
    } else {
        Write-Output $json
    }
}

trap {
    Write-Result ([pscustomobject]@{ ok = $false; error = $_.Exception.Message })
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

function Uia-Walker {
    if ($View -eq "raw") { return [System.Windows.Automation.TreeWalker]::RawViewWalker }
    return [System.Windows.Automation.TreeWalker]::ControlViewWalker
}

function Convert-Element($element, [int]$depth, [string]$path) {
    if ($script:NodeCount -ge $script:MaxNodeCount) { return $null }
    $script:NodeCount += 1
    $current = $element.Current
    $children = @()
    if ($depth -lt $MaxDepth) {
        $walker = Uia-Walker
        $child = $walker.GetFirstChild($element)
        $index = 0
        while ($null -ne $child -and $script:NodeCount -lt $script:MaxNodeCount) {
            try {
                $item = Convert-Element $child ($depth + 1) "$path/$index"
                if ($null -ne $item) { $children += $item }
                $child = $walker.GetNextSibling($child)
            } catch {
                $child = $null
            }
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
    $walker = Uia-Walker
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
    Write-Result ([pscustomobject]@{
        ok = $true
        view = $View
        nodeCount = $script:NodeCount
        truncated = ($script:NodeCount -ge $script:MaxNodeCount)
        root = $tree
    })
    exit 0
}

function Assert-RuntimeId($element) {
    if (-not $RuntimeId) { return }
    if ($RuntimeId -notmatch '^-?[0-9]+(\.-?[0-9]+)*$') { throw "invalid runtime id" }
    $actual = ""
    try { $actual = ($element.GetRuntimeId() -join ".") } catch { $actual = "" }
    if (-not $actual) { throw "element runtime id unavailable for comparison" }
    if ($actual -ne $RuntimeId) { throw "element runtime id changed" }
}

if ($Mode -eq "invoke") {
    $element = Find-ElementByPath $Path
    Assert-RuntimeId $element
    $invoke = $element.GetCurrentPattern([System.Windows.Automation.InvokePattern]::Pattern)
    if ($null -ne $invoke) {
        $invoke.Invoke()
        Write-Result ([pscustomobject]@{ ok = $true; view = $View; pattern = "invoke" })
        exit 0
    }
    $toggle = $element.GetCurrentPattern([System.Windows.Automation.TogglePattern]::Pattern)
    if ($null -ne $toggle) {
        $toggle.Toggle()
        Write-Result ([pscustomobject]@{ ok = $true; view = $View; pattern = "toggle" })
        exit 0
    }
    $select = $element.GetCurrentPattern([System.Windows.Automation.SelectionItemPattern]::Pattern)
    if ($null -ne $select) {
        $select.Select()
        Write-Result ([pscustomobject]@{ ok = $true; view = $View; pattern = "selection_item" })
        exit 0
    }
    throw "element has no invoke, toggle, or selection-item pattern"
}

if ($Mode -eq "settext") {
    $element = Find-ElementByPath $Path
    Assert-RuntimeId $element
    $value = $element.GetCurrentPattern([System.Windows.Automation.ValuePattern]::Pattern)
    if ($null -eq $value) { throw "element has no value pattern" }
    $text = [System.Text.Encoding]::UTF8.GetString([System.Convert]::FromBase64String($TextBase64))
    $value.SetValue($text)
    Write-Result ([pscustomobject]@{ ok = $true; view = $View; pattern = "value" })
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
    #[error("guest command must be an absolute Windows path")]
    InvalidCommandPath,
    #[error("guest file path must be nonempty and contain no control characters")]
    InvalidGuestPath,
    #[error("process argument contains a control character or exceeds 8192 bytes")]
    InvalidProcessArgument,
    #[error("process timeout must be 1..=600000 milliseconds")]
    InvalidTimeout,
    #[error("file transfer is {actual} bytes; maximum is {maximum}")]
    TransferTooLarge { actual: u64, maximum: u64 },
    #[error("workspace member count exceeds {maximum}")]
    TooManyMembers { maximum: u64 },
    #[error("workspace member is {actual} bytes; maximum is {maximum}")]
    MemberTooLarge { actual: u64, maximum: u64 },
    #[error("archive entry is unsafe or unsupported: {0}")]
    UnsafeArchiveEntry(String),
    #[error("archive is malformed: {0}")]
    MalformedArchive(String),
    #[error("destination exists; pass overwrite to replace it")]
    DestinationExists,
    #[error("host operation failed: {0}")]
    HostCommand(String),
    #[error("guest file hash or size mismatch after transfer")]
    TransferVerificationFailed,
    #[error(
        "workspace finalization failed; inspect preserved backup path {backup_path}: {message}"
    )]
    WorkspaceFinalizeFailed {
        backup_path: String,
        message: String,
    },
}

pub type UiaResult<T> = Result<T, UiaError>;

pub struct BlockingUiaControl<T> {
    client: QgaClient<T>,
    generation: u64,
    helper_path: String,
}

pub fn is_valid_runtime_id(value: &str) -> bool {
    !value.is_empty()
        && value.split('.').all(|part| {
            let digits = part.strip_prefix('-').unwrap_or(part);
            !digits.is_empty() && digits.bytes().all(|byte| byte.is_ascii_digit())
        })
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

    pub fn ui_tree(&mut self, max_depth: u16, max_nodes: u32, view: UiaView) -> UiaResult<Value> {
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
                "-View".to_owned(),
                view.as_str().to_owned(),
            ],
            30_000,
        )?;
        let mut value = parse_helper_json(&capture.stdout)?;
        self.generation += 1;
        value["generation"] = Value::from(self.generation);
        Ok(value)
    }

    pub fn invoke(
        &mut self,
        target: &UiNodeRef,
        view: UiaView,
        timeout_ms: u64,
    ) -> UiaResult<Value> {
        validate_element_path(&target.id)?;
        validate_timeout(timeout_ms)?;
        self.install_helper()?;
        let mut args = vec![
            "-Mode".to_owned(),
            "invoke".to_owned(),
            "-Path".to_owned(),
            target.id.clone(),
            "-View".to_owned(),
            view.as_str().to_owned(),
        ];
        if let Some(runtime_id) = &target.runtime_id {
            if !is_valid_runtime_id(runtime_id) {
                return Err(UiaError::InvalidElementRef);
            }
            args.extend(["-RuntimeId".to_owned(), runtime_id.clone()]);
        }
        self.run_powershell(&args, timeout_ms)
            .and_then(|capture| parse_helper_json(&capture.stdout))
    }

    pub fn set_text(
        &mut self,
        target: &UiNodeRef,
        text: &str,
        view: UiaView,
        timeout_ms: u64,
    ) -> UiaResult<Value> {
        const MAX_TEXT_BYTES: usize = 4096;
        validate_element_path(&target.id)?;
        validate_timeout(timeout_ms)?;
        if text.len() > MAX_TEXT_BYTES {
            return Err(UiaError::InputTooLarge(MAX_TEXT_BYTES));
        }
        self.install_helper()?;
        let mut args = vec![
            "-Mode".to_owned(),
            "settext".to_owned(),
            "-Path".to_owned(),
            target.id.clone(),
            "-TextBase64".to_owned(),
            STANDARD.encode(text.as_bytes()),
            "-View".to_owned(),
            view.as_str().to_owned(),
        ];
        if let Some(runtime_id) = &target.runtime_id {
            if !is_valid_runtime_id(runtime_id) {
                return Err(UiaError::InvalidElementRef);
            }
            args.extend(["-RuntimeId".to_owned(), runtime_id.clone()]);
        }
        self.run_powershell(&args, timeout_ms)
            .and_then(|capture| parse_helper_json(&capture.stdout))
    }

    fn run_powershell(&mut self, args: &[String], timeout_ms: u64) -> UiaResult<ProcessCapture> {
        let mut full_args = vec![
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-WindowStyle".to_owned(),
            "Hidden".to_owned(),
            "-ExecutionPolicy".to_owned(),
            "Bypass".to_owned(),
            "-File".to_owned(),
            self.helper_path.clone(),
        ];
        full_args.extend(args.iter().cloned());
        self.run_interactive_powershell(&full_args, timeout_ms)
    }

    fn run_interactive_powershell(
        &mut self,
        args: &[String],
        timeout_ms: u64,
    ) -> UiaResult<ProcessCapture> {
        let counter = INTERACTIVE_TASK_COUNTER.fetch_add(1, Ordering::Relaxed);
        let unix_nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let unique = format!("{}-{}-{}", std::process::id(), unix_nanos, counter);
        let task_name = format!("WinlaneUIA{unique}");
        let xml_guest_path = format!(r"C:\Windows\Temp\winlane-uia-{unique}.xml");
        let output_guest_path = format!(r"C:\Windows\Temp\winlane-uia-{unique}.json");
        let result = self.run_interactive_task(
            &task_name,
            &xml_guest_path,
            &output_guest_path,
            args,
            timeout_ms,
        );
        let _ = self.run_process_blocking(
            SCHTASKS_GUEST_PATH,
            &[
                "/Delete".to_owned(),
                "/TN".to_owned(),
                task_name,
                "/F".to_owned(),
            ],
            5_000,
        );
        let _ = self.run_process_blocking(
            CMD_GUEST_PATH,
            &[
                "/c".to_owned(),
                "del".to_owned(),
                "/f".to_owned(),
                "/q".to_owned(),
                xml_guest_path,
                output_guest_path.clone(),
                format!("{output_guest_path}.tmp"),
            ],
            5_000,
        );
        result
    }

    fn run_interactive_task(
        &mut self,
        task_name: &str,
        xml_guest_path: &str,
        output_guest_path: &str,
        args: &[String],
        timeout_ms: u64,
    ) -> UiaResult<ProcessCapture> {
        let _ = self.run_process_blocking(
            CMD_GUEST_PATH,
            &[
                "/c".to_owned(),
                "del".to_owned(),
                "/f".to_owned(),
                "/q".to_owned(),
                output_guest_path.to_owned(),
            ],
            5_000,
        );
        let helper_arguments = format!("{} -OutputPath {}", args.join(" "), output_guest_path);
        let mut create_errors = Vec::new();
        let mut created = false;
        for principal in ["INTERACTIVE", PROVISIONED_INTERACTIVE_USER] {
            let xml = scheduled_task_xml(task_name, principal, &helper_arguments);
            self.client
                .write_guest_file(xml_guest_path, &utf16le_bytes(&xml))?;
            let create = self.run_process_blocking(
                SCHTASKS_GUEST_PATH,
                &[
                    "/Create".to_owned(),
                    "/TN".to_owned(),
                    task_name.to_owned(),
                    "/XML".to_owned(),
                    xml_guest_path.to_owned(),
                    "/F".to_owned(),
                ],
                10_000,
            )?;
            if create.exit_code == Some(0) {
                created = true;
                break;
            }
            create_errors.push(format!(
                "{}: {}",
                principal,
                process_output_summary(&create)
            ));
        }
        if !created {
            return Err(UiaError::Helper {
                exit_code: Some(1),
                message: format!(
                    "interactive task creation failed ({})",
                    create_errors.join("; ")
                ),
            });
        }
        let run = self.run_process_blocking(
            SCHTASKS_GUEST_PATH,
            &["/Run".to_owned(), "/TN".to_owned(), task_name.to_owned()],
            10_000,
        )?;
        if run.exit_code != Some(0) {
            return Err(UiaError::Helper {
                exit_code: run.exit_code,
                message: format!(
                    "interactive task start failed: {}",
                    process_output_summary(&run)
                ),
            });
        }

        let started = Instant::now();
        loop {
            match self
                .client
                .read_guest_file(output_guest_path, crate::MAX_TRANSFER_BYTES)
            {
                Ok(bytes) if !bytes.is_empty() => {
                    return Ok(ProcessCapture {
                        exit_code: Some(0),
                        stdout: bytes,
                        stderr: Vec::new(),
                    });
                }
                Ok(_) | Err(QgaError::Guest { .. }) => {}
                Err(error) => return Err(UiaError::Qga(error)),
            }
            if started.elapsed() > Duration::from_millis(timeout_ms) {
                return Err(UiaError::Timeout(timeout_ms));
            }
            std::thread::sleep(Duration::from_millis(150));
        }
    }

    pub fn run_process_blocking(
        &mut self,
        command: &str,
        args: &[String],
        timeout_ms: u64,
    ) -> UiaResult<ProcessCapture> {
        validate_process_inputs(command, args, timeout_ms)?;
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

fn scheduled_task_xml(task_name: &str, principal: &str, arguments: &str) -> String {
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>winlane interactive UIA helper {}</Description></RegistrationInfo>
  <Triggers><TimeTrigger><Enabled>false</Enabled><StartBoundary>2024-01-01T00:00:00</StartBoundary></TimeTrigger></Triggers>
  <Principals>
    <Principal id="Author"><UserId>{}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal>
  </Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand>
    <Enabled>true</Enabled>
    <ExecutionTimeLimit>PT5M</ExecutionTimeLimit>
    <Priority>4</Priority>
  </Settings>
  <Actions Context="Author">
    <Exec><Command>{}</Command><Arguments>{}</Arguments></Exec>
  </Actions>
</Task>
"#,
        xml_escape(task_name),
        xml_escape(principal),
        xml_escape(POWERSHELL_GUEST_PATH),
        xml_escape(arguments)
    )
}

fn utf16le_bytes(text: &str) -> Vec<u8> {
    let mut bytes = vec![0xff, 0xfe];
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn xml_escape(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('\"', "&quot;")
        .replace('\'', "&apos;")
}

pub(crate) fn process_output_summary(capture: &ProcessCapture) -> String {
    let stdout = String::from_utf8_lossy(&capture.stdout);
    let stderr = String::from_utf8_lossy(&capture.stderr);
    format!(
        "exit {:?}, stdout '{}', stderr '{}'",
        capture.exit_code,
        stdout.trim(),
        stderr.trim()
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCapture {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

fn validate_process_inputs(command: &str, args: &[String], timeout_ms: u64) -> UiaResult<()> {
    validate_absolute_windows_command(command)?;
    for argument in args {
        if argument.len() > 8192 || argument.chars().any(char::is_control) {
            return Err(UiaError::InvalidProcessArgument);
        }
    }
    validate_timeout(timeout_ms)
}

fn validate_timeout(timeout_ms: u64) -> UiaResult<()> {
    if !(1..=600_000).contains(&timeout_ms) {
        return Err(UiaError::InvalidTimeout);
    }
    Ok(())
}

fn validate_absolute_windows_command(command: &str) -> UiaResult<()> {
    if valid_absolute_windows_path(command) {
        Ok(())
    } else {
        Err(UiaError::InvalidCommandPath)
    }
}

#[cfg(test)]
fn validate_guest_file_path(path: &str) -> UiaResult<()> {
    if valid_absolute_windows_path(path) {
        Ok(())
    } else {
        Err(UiaError::InvalidGuestPath)
    }
}

pub(crate) fn valid_absolute_windows_path(path: &str) -> bool {
    if path.is_empty() || path.len() > 32_767 || path.chars().any(char::is_control) {
        return false;
    }
    let bytes = path.as_bytes();
    let drive_absolute = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/');
    let unc_absolute = bytes.len() > 2
        && ((bytes[0] == b'\\' && bytes[1] == b'\\') || (bytes[0] == b'/' && bytes[1] == b'/'));
    drive_absolute || unc_absolute
}

fn parse_helper_json(stdout: &[u8]) -> UiaResult<Value> {
    let text = std::str::from_utf8(stdout).map_err(|_| UiaError::InvalidUtf8)?;
    let value: Value = serde_json::from_str(text.trim().trim_start_matches('\u{feff}'))?;
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
        assert!(UIA_HELPER_SCRIPT.contains("RawViewWalker"));
        assert!(UIA_HELPER_SCRIPT.contains(r#"[ValidateSet("control","raw")]"#));
        assert!(UIA_HELPER_SCRIPT.contains("view = $View"));
        assert!(UIA_HELPER_SCRIPT.contains("Assert-RuntimeId"));
        assert!(UIA_HELPER_SCRIPT.contains("element runtime id changed"));
        assert!(UIA_HELPER_SCRIPT.contains("element runtime id unavailable for comparison"));
    }

    #[test]
    fn validates_runtime_id_shape() {
        assert!(is_valid_runtime_id("7.42.-3"));
        assert!(!is_valid_runtime_id(""));
        assert!(!is_valid_runtime_id("7..42"));
        assert!(!is_valid_runtime_id("7/42"));
        assert!(!is_valid_runtime_id("7.42;calc"));
    }

    #[test]
    fn uia_view_parse_is_explicit_and_defaults_are_external() {
        assert_eq!(UiaView::parse("control"), Some(UiaView::Control));
        assert_eq!(UiaView::parse("raw"), Some(UiaView::Raw));
        assert_eq!(UiaView::parse("content"), None);
    }

    #[test]
    fn helper_writes_result_to_output_file() {
        assert!(UIA_HELPER_SCRIPT.contains("OutputPath"));
        assert!(UIA_HELPER_SCRIPT.contains("WriteAllText"));
    }

    #[test]
    fn scheduled_task_xml_escapes_principal_and_arguments() {
        let xml = scheduled_task_xml(
            "WinlaneUIAProbe",
            "winlane",
            "-File helper.ps1 -OutputPath out.json & <test>",
        );
        assert!(xml.contains("<UserId>winlane</UserId>"));
        assert!(xml.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(xml.contains("&amp; &lt;test&gt;"));
        let bytes = utf16le_bytes(&xml);
        assert_eq!(&bytes[..2], &[0xff, 0xfe]);
    }

    #[test]
    fn parses_utf8_bom_helper_json() {
        let value = parse_helper_json("\u{feff}{\"ok\":true}".as_bytes()).unwrap();
        assert_eq!(value["ok"], true);
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

    #[test]
    fn validates_absolute_windows_command_paths() {
        assert!(validate_absolute_windows_command(r"C:\Windows\System32\cmd.exe").is_ok());
        assert!(validate_absolute_windows_command("c:/windows/system32/cmd.exe").is_ok());
        assert!(validate_absolute_windows_command(r"\\server\share\tool.exe").is_ok());
        assert!(matches!(
            validate_absolute_windows_command("powershell.exe"),
            Err(UiaError::InvalidCommandPath)
        ));
        assert!(matches!(
            validate_absolute_windows_command(r"C:Windows\System32\cmd.exe"),
            Err(UiaError::InvalidCommandPath)
        ));
        assert!(matches!(
            validate_absolute_windows_command(r"\Windows\System32\cmd.exe"),
            Err(UiaError::InvalidCommandPath)
        ));
    }

    #[test]
    fn validates_process_inputs_before_guest_execution() {
        assert!(
            validate_process_inputs(
                r"C:\Windows\System32\cmd.exe",
                &["/c".to_string(), String::new()],
                1000,
            )
            .is_ok()
        );
        assert!(matches!(
            validate_process_inputs(r"C:\Windows\System32\cmd.exe", &["bad\0".to_string()], 1000),
            Err(UiaError::InvalidProcessArgument)
        ));
        assert!(matches!(
            validate_process_inputs(r"C:\Windows\System32\cmd.exe", &[], 0),
            Err(UiaError::InvalidTimeout)
        ));
    }

    #[test]
    fn validates_absolute_guest_file_paths() {
        assert!(validate_guest_file_path(r"C:\Temp\file.txt").is_ok());
        assert!(validate_guest_file_path(r"\\host\share\file.txt").is_ok());
        assert!(matches!(
            validate_guest_file_path(r"Temp\file.txt"),
            Err(UiaError::InvalidGuestPath)
        ));
        assert!(matches!(
            validate_guest_file_path("bad\0path"),
            Err(UiaError::InvalidGuestPath)
        ));
    }
}
