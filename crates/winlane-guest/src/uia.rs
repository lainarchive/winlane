//! Windows UI Automation helper payload and blocking guest-control operations.

use base64::{Engine as _, engine::general_purpose::STANDARD};
use serde_json::Value;
use std::io::{Read, Write};
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
const TAR_GUEST_PATH: &str = r"C:\Windows\System32\tar.exe";
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
    #[error("destination exists; pass overwrite explicitly to replace it")]
    DestinationExists,
    #[error("host archive command failed: {0}")]
    HostCommand(String),
    #[error("archive entry is unsafe or unsupported: {0}")]
    UnsafeArchiveEntry(String),
    #[error("guest file hash or size mismatch after transfer")]
    TransferVerificationFailed,
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

    fn guest_file_info(&mut self, path: &str) -> UiaResult<(u64, String)> {
        let quoted = path.replace('\'', "''");
        let script = format!(
            "$p='{quoted}'; if (!(Test-Path -LiteralPath $p -PathType Leaf)) {{ exit 3 }}; $i=Get-Item -LiteralPath $p; $h=Get-FileHash -LiteralPath $p -Algorithm SHA256; [Console]::WriteLine(('' + $i.Length + ':' + $h.Hash.ToLower()))"
        );
        let capture = self.run_process_blocking(
            POWERSHELL_GUEST_PATH,
            &[
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script,
            ],
            600_000,
        )?;
        if capture.exit_code != Some(0) {
            return Err(UiaError::Helper {
                exit_code: capture.exit_code,
                message: process_output_summary(&capture),
            });
        }
        let text = String::from_utf8_lossy(&capture.stdout);
        let (size, hash) = text.trim().split_once(':').ok_or(UiaError::InvalidUtf8)?;
        Ok((
            size.parse().map_err(|_| UiaError::InvalidUtf8)?,
            hash.to_string(),
        ))
    }

    fn write_host_file_to_guest(
        &mut self,
        host_path: &std::path::Path,
        guest_path: &str,
        overwrite: bool,
    ) -> UiaResult<(u64, String)> {
        use sha2::{Digest, Sha256};
        let mut input = std::fs::File::open(host_path).map_err(|error| UiaError::HostIo {
            path: host_path.to_string_lossy().into_owned(),
            source: error,
        })?;
        let mode = if overwrite { "wb" } else { "wbx" };
        let handle = self.client.file_open(guest_path, mode)?;
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let result = (|| {
            loop {
                let mut chunk = vec![0_u8; 1024 * 1024];
                let count = input.read(&mut chunk).map_err(|error| UiaError::HostIo {
                    path: host_path.to_string_lossy().into_owned(),
                    source: error,
                })?;
                if count == 0 {
                    break;
                }
                chunk.truncate(count);
                hasher.update(&chunk);
                self.client.file_write_chunks(handle, &chunk)?;
                total += count as u64;
            }
            self.client.file_flush(handle)?;
            Ok::<(), UiaError>(())
        })();
        let close = self.client.file_close(handle);
        result.and(close.map_err(UiaError::Qga))?;
        Ok((total, hex_digest(hasher)))
    }

    fn read_guest_file_to_host(
        &mut self,
        guest_path: &str,
        host_path: &std::path::Path,
        overwrite: bool,
        expected: Option<(u64, &str)>,
    ) -> UiaResult<(u64, String)> {
        use sha2::{Digest, Sha256};
        if host_path.exists() && !overwrite {
            return Err(UiaError::DestinationExists);
        }
        let handle = self.client.file_open(guest_path, "rb")?;
        let mut staging = StagingHostFile::create(host_path, overwrite)?;
        let mut hasher = Sha256::new();
        let mut total = 0_u64;
        let result = (|| {
            loop {
                let chunk = self.client.file_read(handle, 1024 * 1024)?;
                if !chunk.bytes.is_empty() {
                    staging
                        .file
                        .write_all(&chunk.bytes)
                        .map_err(|error| UiaError::HostIo {
                            path: staging.path.to_string_lossy().into_owned(),
                            source: error,
                        })?;
                    hasher.update(&chunk.bytes);
                    total += chunk.bytes.len() as u64;
                }
                if chunk.eof {
                    break;
                }
            }
            staging.file.flush().map_err(|error| UiaError::HostIo {
                path: staging.path.to_string_lossy().into_owned(),
                source: error,
            })?;
            Ok::<(), UiaError>(())
        })();
        let close = self.client.file_close(handle);
        result.and(close.map_err(UiaError::Qga))?;
        let sha256 = hex_digest(hasher);
        if let Some((expected_size, expected_hash)) = expected
            && (total != expected_size || sha256 != expected_hash)
        {
            return Err(UiaError::TransferVerificationFailed);
        }
        staging.persist()?;
        Ok((total, sha256))
    }

    fn verify_guest_file(&mut self, guest_path: &str, size: u64, sha256: &str) -> UiaResult<()> {
        let (guest_size, guest_hash) = self.guest_file_info(guest_path)?;
        if guest_size == size && guest_hash == sha256 {
            Ok(())
        } else {
            Err(UiaError::TransferVerificationFailed)
        }
    }

    pub fn import_file_blocking(
        &mut self,
        host_path: &std::path::Path,
        guest_path: &str,
        overwrite: bool,
    ) -> UiaResult<FileTransferRef> {
        validate_guest_transfer_path(guest_path)?;
        let metadata = std::fs::metadata(host_path).map_err(|error| UiaError::HostIo {
            path: host_path.to_string_lossy().into_owned(),
            source: error,
        })?;
        if !metadata.is_file() {
            return Err(UiaError::HostCommand(
                "host import path is not a file".into(),
            ));
        }
        let (bytes, sha256) = self.write_host_file_to_guest(host_path, guest_path, overwrite)?;
        self.verify_guest_file(guest_path, bytes, &sha256)?;
        Ok(FileTransferRef {
            sha256,
            bytes,
            host_path: host_path.to_string_lossy().into_owned(),
            guest_path: guest_path.to_owned(),
        })
    }

    pub fn export_file_blocking(
        &mut self,
        guest_path: &str,
        host_path: &std::path::Path,
        overwrite: bool,
    ) -> UiaResult<FileTransferRef> {
        validate_guest_transfer_path(guest_path)?;
        let (guest_size, guest_hash) = self.guest_file_info(guest_path)?;
        let (bytes, sha256) = self.read_guest_file_to_host(
            guest_path,
            host_path,
            overwrite,
            Some((guest_size, &guest_hash)),
        )?;
        Ok(FileTransferRef {
            sha256,
            bytes,
            host_path: host_path.to_string_lossy().into_owned(),
            guest_path: guest_path.to_owned(),
        })
    }

    fn prepare_guest_directory(&mut self, path: &str, overwrite: bool) -> UiaResult<()> {
        let quoted = path.replace('\'', "''");
        let script = if overwrite {
            format!(
                "$p='{quoted}'; New-Item -ItemType Directory -Force -Path $p | Out-Null; if ((Get-Item -LiteralPath $p) -isnot [System.IO.DirectoryInfo]) {{ exit 2 }}"
            )
        } else {
            format!(
                "$p='{quoted}'; if ((Test-Path -LiteralPath $p) -and (Get-ChildItem -LiteralPath $p -Force | Select-Object -First 1)) {{ exit 3 }}; New-Item -ItemType Directory -Force -Path $p | Out-Null"
            )
        };
        let capture = self.run_process_blocking(
            POWERSHELL_GUEST_PATH,
            &[
                "-NoProfile".into(),
                "-NonInteractive".into(),
                "-Command".into(),
                script,
            ],
            30_000,
        )?;
        if capture.exit_code == Some(3) {
            return Err(UiaError::DestinationExists);
        }
        if capture.exit_code != Some(0) {
            return Err(UiaError::Helper {
                exit_code: capture.exit_code,
                message: process_output_summary(&capture),
            });
        }
        Ok(())
    }

    pub fn import_workspace_blocking(
        &mut self,
        host_dir: &std::path::Path,
        guest_dir: &str,
        overwrite: bool,
    ) -> UiaResult<FileTransferRef> {
        validate_guest_workspace_path(guest_dir)?;
        validate_host_tree(host_dir)?;
        self.prepare_guest_directory(guest_dir, overwrite)?;
        let archive = temporary_host_archive(host_dir, true)?;
        validate_host_archive(&archive.path)?;
        let archive_path = archive.path.clone();
        let unique = guest_unique_path("workspace-import.tar");
        let result: UiaResult<(u64, String)> = (|| {
            let (bytes, sha256) = self.write_host_file_to_guest(&archive_path, &unique, true)?;
            self.verify_guest_file(&unique, bytes, &sha256)?;
            self.run_checked(
                TAR_GUEST_PATH,
                &["-xf".into(), unique.clone(), "-C".into(), guest_dir.into()],
                600_000,
            )?;
            Ok((bytes, sha256))
        })();
        let _ = self.run_process_blocking(
            CMD_GUEST_PATH,
            &["/c".into(), "del".into(), "/f".into(), "/q".into(), unique],
            10_000,
        );
        let (bytes, sha256) = result?;
        Ok(FileTransferRef {
            sha256,
            bytes,
            host_path: host_dir.to_string_lossy().into_owned(),
            guest_path: guest_dir.into(),
        })
    }

    pub fn export_workspace_blocking(
        &mut self,
        guest_dir: &str,
        host_dir: &std::path::Path,
        overwrite: bool,
    ) -> UiaResult<FileTransferRef> {
        validate_guest_workspace_path(guest_dir)?;
        prepare_host_directory(host_dir, overwrite)?;
        let archive = temporary_host_archive(host_dir, false)?;
        let archive_path = archive.path.clone();
        let unique = guest_unique_path("workspace-export.tar");
        let result: UiaResult<(u64, String)> = (|| {
            self.run_checked(
                TAR_GUEST_PATH,
                &[
                    "-cf".into(),
                    unique.clone(),
                    "-C".into(),
                    guest_dir.into(),
                    ".".into(),
                ],
                600_000,
            )?;
            let (guest_size, guest_hash) = self.guest_file_info(&unique)?;
            let (bytes, sha256) = self.read_guest_file_to_host(
                &unique,
                &archive_path,
                true,
                Some((guest_size, &guest_hash)),
            )?;
            validate_host_archive(&archive_path)?;
            extract_host_archive(&archive_path, host_dir)?;
            Ok((bytes, sha256))
        })();
        let _ = self.run_process_blocking(
            CMD_GUEST_PATH,
            &["/c".into(), "del".into(), "/f".into(), "/q".into(), unique],
            10_000,
        );
        let (bytes, sha256) = result?;
        Ok(FileTransferRef {
            sha256,
            bytes,
            host_path: host_dir.to_string_lossy().into_owned(),
            guest_path: guest_dir.into(),
        })
    }

    fn run_checked(&mut self, command: &str, args: &[String], timeout_ms: u64) -> UiaResult<()> {
        let capture = self.run_process_blocking(command, args, timeout_ms)?;
        if capture.exit_code == Some(0) {
            Ok(())
        } else {
            Err(UiaError::Helper {
                exit_code: capture.exit_code,
                message: process_output_summary(&capture),
            })
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

fn process_output_summary(capture: &ProcessCapture) -> String {
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

fn validate_guest_file_path(path: &str) -> UiaResult<()> {
    if valid_absolute_windows_path(path) {
        Ok(())
    } else {
        Err(UiaError::InvalidGuestPath)
    }
}

fn valid_absolute_windows_path(path: &str) -> bool {
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

struct StagingHostFile {
    target: std::path::PathBuf,
    path: std::path::PathBuf,
    file: std::fs::File,
    overwrite: bool,
    persisted: bool,
}

impl StagingHostFile {
    fn create(target: &std::path::Path, overwrite: bool) -> UiaResult<Self> {
        let parent = target.parent().unwrap_or_else(|| std::path::Path::new("."));
        std::fs::create_dir_all(parent).map_err(|error| UiaError::HostIo {
            path: parent.to_string_lossy().into_owned(),
            source: error,
        })?;
        for counter in 0..100 {
            let nanos = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or_default();
            let path = parent.join(format!(
                ".winlane-transfer-{}-{nanos}-{counter}",
                std::process::id()
            ));
            match std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => {
                    return Ok(Self {
                        target: target.to_path_buf(),
                        path,
                        file,
                        overwrite,
                        persisted: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(UiaError::HostIo {
                        path: path.to_string_lossy().into_owned(),
                        source: error,
                    });
                }
            }
        }
        Err(UiaError::HostCommand(
            "could not create staging file".into(),
        ))
    }

    fn persist(mut self) -> UiaResult<()> {
        let result = if self.overwrite {
            std::fs::rename(&self.path, &self.target)
        } else {
            std::fs::hard_link(&self.path, &self.target)
        };
        match result {
            Ok(()) => {
                if self.overwrite {
                    self.persisted = true;
                } else {
                    self.persisted = std::fs::remove_file(&self.path).is_ok();
                }
                Ok(())
            }
            Err(_) if !self.overwrite && self.target.exists() => Err(UiaError::DestinationExists),
            Err(error) => Err(UiaError::HostIo {
                path: self.target.to_string_lossy().into_owned(),
                source: error,
            }),
        }
    }
}

impl Drop for StagingHostFile {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

fn hex_digest(hasher: sha2::Sha256) -> String {
    use sha2::Digest;
    hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn guest_unique_path(name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!(
        r"C:\Windows\Temp\winlane-{}-{nanos}-{name}",
        std::process::id()
    )
}

struct HostArchive {
    directory: std::path::PathBuf,
    path: std::path::PathBuf,
}
impl Drop for HostArchive {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

fn private_archive() -> UiaResult<HostArchive> {
    for counter in 0..100 {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default();
        let directory = std::env::temp_dir().join(format!(
            "winlane-transfer-{}-{nanos}-{counter}",
            std::process::id()
        ));
        match std::fs::create_dir(&directory) {
            Ok(()) => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ = std::fs::set_permissions(
                        &directory,
                        std::fs::Permissions::from_mode(0o700),
                    );
                }
                let path = directory.join("workspace.tar");
                return Ok(HostArchive { directory, path });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(UiaError::HostIo {
                    path: directory.to_string_lossy().into_owned(),
                    source: error,
                });
            }
        }
    }
    Err(UiaError::HostCommand(
        "could not create private archive directory".into(),
    ))
}

fn validate_host_tree(path: &std::path::Path) -> UiaResult<()> {
    fn walk(path: &std::path::Path, validate_name: bool) -> UiaResult<()> {
        let metadata = std::fs::symlink_metadata(path).map_err(|error| UiaError::HostIo {
            path: path.to_string_lossy().into_owned(),
            source: error,
        })?;
        if metadata.file_type().is_symlink() {
            return Err(UiaError::UnsafeArchiveEntry(
                path.to_string_lossy().into_owned(),
            ));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or_default();
        let stem = name.split('.').next().unwrap_or(name);
        let reserved = matches!(
            stem.to_ascii_uppercase().as_str(),
            "CON"
                | "PRN"
                | "AUX"
                | "NUL"
                | "COM1"
                | "COM2"
                | "COM3"
                | "COM4"
                | "COM5"
                | "COM6"
                | "COM7"
                | "COM8"
                | "COM9"
                | "LPT1"
                | "LPT2"
                | "LPT3"
                | "LPT4"
                | "LPT5"
                | "LPT6"
                | "LPT7"
                | "LPT8"
                | "LPT9"
        );
        if validate_name
            && (name.is_empty()
                || name.len() > 240
                || reserved
                || name.chars().any(|c| {
                    c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\')
                })
                || name.ends_with(['.', ' ']))
        {
            return Err(UiaError::UnsafeArchiveEntry(
                path.to_string_lossy().into_owned(),
            ));
        }
        if metadata.is_dir() {
            for entry in std::fs::read_dir(path).map_err(|error| UiaError::HostIo {
                path: path.to_string_lossy().into_owned(),
                source: error,
            })? {
                let entry = entry.map_err(|error| UiaError::HostIo {
                    path: path.to_string_lossy().into_owned(),
                    source: error,
                })?;
                walk(&entry.path(), true)?;
            }
        } else if !metadata.is_file() {
            return Err(UiaError::UnsafeArchiveEntry(
                path.to_string_lossy().into_owned(),
            ));
        }
        Ok(())
    }
    if !path.is_dir() {
        return Err(UiaError::HostCommand(
            "workspace host path is not a directory".into(),
        ));
    }
    walk(path, false)
}

fn temporary_host_archive(source: &std::path::Path, create: bool) -> UiaResult<HostArchive> {
    let archive = private_archive()?;
    if create {
        let status = std::process::Command::new("tar")
            .arg("-cf")
            .arg(&archive.path)
            .arg("-C")
            .arg(source)
            .arg(".")
            .status()
            .map_err(|error| UiaError::HostIo {
                path: archive.path.to_string_lossy().into_owned(),
                source: error,
            })?;
        if !status.success() {
            return Err(UiaError::HostCommand("tar archive creation failed".into()));
        }
    }
    Ok(archive)
}

fn prepare_host_directory(path: &std::path::Path, overwrite: bool) -> UiaResult<()> {
    if path.exists() {
        if !path.is_dir() {
            return Err(UiaError::DestinationExists);
        }
        if !overwrite
            && std::fs::read_dir(path)
                .map_err(|error| UiaError::HostIo {
                    path: path.to_string_lossy().into_owned(),
                    source: error,
                })?
                .next()
                .is_some()
        {
            return Err(UiaError::DestinationExists);
        }
    } else {
        std::fs::create_dir_all(path).map_err(|error| UiaError::HostIo {
            path: path.to_string_lossy().into_owned(),
            source: error,
        })?;
    }
    Ok(())
}

fn validate_host_archive(path: &std::path::Path) -> UiaResult<()> {
    let list = std::process::Command::new("tar")
        .arg("-tf")
        .arg(path)
        .output()
        .map_err(|error| UiaError::HostIo {
            path: path.to_string_lossy().into_owned(),
            source: error,
        })?;
    if !list.status.success() {
        return Err(UiaError::HostCommand("tar archive listing failed".into()));
    }
    for line in String::from_utf8_lossy(&list.stdout).lines() {
        let normalized = line
            .strip_prefix("./")
            .unwrap_or(line)
            .trim_end_matches('/');
        if normalized.is_empty() || normalized == "." {
            continue;
        }
        let bad = normalized.starts_with('/')
            || normalized.contains('\\')
            || normalized.as_bytes().get(1) == Some(&b':')
            || normalized
                .split('/')
                .any(|part| part == ".." || part.is_empty());
        if bad {
            return Err(UiaError::UnsafeArchiveEntry(line.into()));
        }
    }
    let verbose = std::process::Command::new("tar")
        .arg("-tvf")
        .arg(path)
        .output()
        .map_err(|error| UiaError::HostIo {
            path: path.to_string_lossy().into_owned(),
            source: error,
        })?;
    for line in verbose.stdout.split(|byte| *byte == b'\n') {
        if line.is_empty() {
            continue;
        }
        if !matches!(line.first(), Some(b'-') | Some(b'd')) {
            return Err(UiaError::UnsafeArchiveEntry(
                "non-file/non-directory entry".into(),
            ));
        }
    }
    Ok(())
}

fn extract_host_archive(archive: &std::path::Path, target: &std::path::Path) -> UiaResult<()> {
    let status = std::process::Command::new("tar")
        .arg("-xf")
        .arg(archive)
        .arg("-C")
        .arg(target)
        .arg("--no-same-owner")
        .arg("--no-same-permissions")
        .status()
        .map_err(|error| UiaError::HostIo {
            path: target.to_string_lossy().into_owned(),
            source: error,
        })?;
    if status.success() {
        Ok(())
    } else {
        Err(UiaError::HostCommand("tar extraction failed".into()))
    }
}

fn validate_guest_workspace_path(path: &str) -> UiaResult<()> {
    if path.starts_with(r"\\") || path.starts_with("//") {
        return Err(UiaError::InvalidGuestPath);
    }
    validate_guest_transfer_path(path)
}

fn validate_guest_transfer_path(path: &str) -> UiaResult<()> {
    let device = path.starts_with(r"\\.\")
        || path.starts_with(r"\\?\")
        || path.starts_with("//./")
        || path.starts_with("//?/");
    let traversal = path.split(['\\', '/']).any(|component| component == "..");
    if device || traversal {
        return Err(UiaError::InvalidGuestPath);
    }
    validate_guest_file_path(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    struct TestTempDir {
        path: std::path::PathBuf,
    }

    impl TestTempDir {
        fn new() -> Self {
            for counter in 0..100 {
                let nanos = SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .map(|duration| duration.as_nanos())
                    .unwrap_or_default();
                let path = std::env::temp_dir().join(format!(
                    "winlane-test-{}-{nanos}-{counter}",
                    std::process::id()
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => return Self { path },
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create test temp dir: {error}"),
                }
            }
            panic!("could not create test temp dir")
        }

        fn path(&self) -> &std::path::Path {
            &self.path
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

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

    struct MemoryQgaTransport {
        incoming: std::collections::VecDeque<Vec<u8>>,
        requests: Vec<Value>,
        written_file: Vec<u8>,
        read_file: Vec<u8>,
        read_position: usize,
    }

    impl MemoryQgaTransport {
        fn new(read_file: Vec<u8>) -> Self {
            Self {
                incoming: std::collections::VecDeque::new(),
                requests: Vec::new(),
                written_file: Vec::new(),
                read_file,
                read_position: 0,
            }
        }
    }

    impl crate::qga::QgaTransport for MemoryQgaTransport {
        fn write_raw(&mut self, _bytes: &[u8]) -> std::io::Result<()> {
            Ok(())
        }

        fn write_message(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            let request: Value = serde_json::from_slice(bytes).unwrap();
            self.requests.push(request.clone());
            let execute = request["execute"].as_str().unwrap();
            let result = match execute {
                "guest-file-open" => json!(1),
                "guest-file-write" => {
                    let encoded = request["arguments"]["buf-b64"].as_str().unwrap();
                    self.written_file.extend(STANDARD.decode(encoded).unwrap());
                    json!({})
                }
                "guest-file-flush" | "guest-file-close" => json!({}),
                "guest-file-read" => {
                    let count = request["arguments"]["count"].as_u64().unwrap() as usize;
                    let end = (self.read_position + count).min(self.read_file.len());
                    let bytes = self.read_file[self.read_position..end].to_vec();
                    let read_count = bytes.len();
                    self.read_position = end;
                    json!({
                        "buf-b64": STANDARD.encode(bytes),
                        "count": read_count,
                        "eof": self.read_position == self.read_file.len(),
                    })
                }
                "guest-exec" => json!({ "pid": 7 }),
                "guest-exec-status" => {
                    let bytes = if self.read_file.is_empty() {
                        &self.written_file
                    } else {
                        &self.read_file
                    };
                    json!({
                        "exited": true,
                        "exitcode": 0,
                        "out-data": STANDARD.encode(format!("{}:{}\n", bytes.len(), sha256_hex(bytes))),
                    })
                }
                other => panic!("unexpected qga request {other}"),
            };
            self.incoming
                .push_back(json!({"return": result}).to_string().into_bytes());
            Ok(())
        }

        fn read_message(&mut self) -> std::io::Result<Vec<u8>> {
            self.incoming.pop_front().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "missing response")
            })
        }
    }

    #[test]
    fn streamed_import_round_trips_chunk_boundary_bytes_in_bounded_requests() {
        let bytes: Vec<u8> = (0..=255).cycle().take(1024 * 1024 + 1).collect();
        let temp = TestTempDir::new();
        let host_path = temp.path().join("input.bin");
        std::fs::write(&host_path, &bytes).unwrap();
        let transport = MemoryQgaTransport::new(Vec::new());
        let mut control = BlockingUiaControl::new(crate::qga::QgaClient::new(transport));
        let (size, hash) = control
            .write_host_file_to_guest(&host_path, r"C:\Temp\output.bin", false)
            .unwrap();
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(hash, sha256_hex(&bytes));
        let transport = control.client.transport;
        assert_eq!(transport.written_file, bytes);
        let write_counts: Vec<u64> = transport
            .requests
            .iter()
            .filter(|request| request["execute"] == "guest-file-write")
            .map(|request| request["arguments"]["count"].as_u64().unwrap())
            .collect();
        assert_eq!(write_counts, vec![1024 * 1024, 1]);
        let open = transport
            .requests
            .iter()
            .find(|request| request["execute"] == "guest-file-open")
            .unwrap();
        assert_eq!(open["arguments"]["mode"], "wbx");
    }

    #[test]
    fn streamed_export_round_trips_chunk_boundary_bytes() {
        let bytes: Vec<u8> = (0..=255).cycle().take(1024 * 1024 + 1).collect();
        let transport = MemoryQgaTransport::new(bytes.clone());
        let mut control = BlockingUiaControl::new(crate::qga::QgaClient::new(transport));
        let temp = TestTempDir::new();
        let host_path = temp.path().join("output.bin");
        let (size, hash) = control
            .read_guest_file_to_host(r"C:\Temp\input.bin", &host_path, false, None)
            .unwrap();
        assert_eq!(size, bytes.len() as u64);
        assert_eq!(hash, sha256_hex(&bytes));
        assert_eq!(std::fs::read(&host_path).unwrap(), bytes);
        let read_counts: Vec<u64> = control
            .client
            .transport
            .requests
            .iter()
            .filter(|request| request["execute"] == "guest-file-read")
            .map(|request| request["arguments"]["count"].as_u64().unwrap())
            .collect();
        assert_eq!(read_counts, vec![1024 * 1024, 1024 * 1024]);
        assert!(matches!(
            control.read_guest_file_to_host(r"C:\Temp\input.bin", &host_path, false, None),
            Err(UiaError::DestinationExists)
        ));
    }

    #[test]
    fn public_file_import_and_export_verify_exact_round_trip_hashes() {
        let bytes: Vec<u8> = (0..=255).cycle().take(1024 * 1024 + 1).collect();
        let temp = TestTempDir::new();
        let import_host = temp.path().join("import.bin");
        std::fs::write(&import_host, &bytes).unwrap();
        let transport = MemoryQgaTransport::new(Vec::new());
        let mut control = BlockingUiaControl::new(crate::qga::QgaClient::new(transport));
        let imported = control
            .import_file_blocking(&import_host, r"C:\Temp\roundtrip.bin", false)
            .unwrap();
        assert_eq!(imported.bytes, bytes.len() as u64);
        assert_eq!(imported.sha256, sha256_hex(&bytes));

        let transport = MemoryQgaTransport::new(bytes.clone());
        let mut control = BlockingUiaControl::new(crate::qga::QgaClient::new(transport));
        let export_host = temp.path().join("export.bin");
        let exported = control
            .export_file_blocking(r"C:\Temp\roundtrip.bin", &export_host, false)
            .unwrap();
        assert_eq!(exported.bytes, bytes.len() as u64);
        assert_eq!(exported.sha256, sha256_hex(&bytes));
        assert_eq!(std::fs::read(&export_host).unwrap(), bytes);
        let mut names: Vec<String> = std::fs::read_dir(temp.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        names.sort();
        assert_eq!(names, vec!["export.bin", "import.bin"]);
    }

    #[test]
    fn transfer_paths_reject_device_and_traversal_paths() {
        assert!(validate_guest_transfer_path(r"C:\Temp\file.txt").is_ok());
        assert!(validate_guest_transfer_path(r"\\host\share\file.txt").is_ok());
        for path in [
            r"\\.\PhysicalDrive0",
            r"\\?\C:\Temp\file.txt",
            r"C:\workspace\..\evil.txt",
        ] {
            assert!(matches!(
                validate_guest_transfer_path(path),
                Err(UiaError::InvalidGuestPath)
            ));
        }
    }

    #[cfg(unix)]
    #[test]
    fn host_tree_rejects_windows_device_and_separator_names() {
        let temp = TestTempDir::new();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        for name in ["NUL", "bad\\name.txt"] {
            std::fs::write(source.join(name), b"data").unwrap();
            assert!(matches!(
                validate_host_tree(&source),
                Err(UiaError::UnsafeArchiveEntry(_))
            ));
            std::fs::remove_file(source.join(name)).unwrap();
        }
    }

    #[test]
    fn workspace_archive_round_trip_preserves_ordinary_tree_bytes() {
        let temp = TestTempDir::new();
        let source = temp.path().join("source");
        let nested = source.join("alpha").join("nested");
        let duplicate = source.join("beta").join("nested");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::create_dir_all(&duplicate).unwrap();
        std::fs::create_dir_all(source.join("empty-dir")).unwrap();
        let binary: Vec<u8> = (0..=255).cycle().take(8193).collect();
        std::fs::write(nested.join("same.bin"), &binary).unwrap();
        std::fs::write(duplicate.join("same.bin"), b"different bytes").unwrap();
        std::fs::write(source.join("empty.txt"), b"").unwrap();
        let archive = temporary_host_archive(&source, true).unwrap();
        validate_host_archive(&archive.path).unwrap();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        extract_host_archive(&archive.path, &target).unwrap();
        assert_eq!(
            sha256_hex(&std::fs::read(target.join("alpha/nested/same.bin")).unwrap()),
            sha256_hex(&binary)
        );
        assert_eq!(
            std::fs::read(target.join("beta/nested/same.bin")).unwrap(),
            b"different bytes"
        );
        assert_eq!(std::fs::read(target.join("empty.txt")).unwrap(), b"");
        assert!(target.join("empty-dir").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn host_tree_rejects_symlinks() {
        let temp = TestTempDir::new();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("target.txt"), b"data").unwrap();
        std::os::unix::fs::symlink(source.join("target.txt"), source.join("link.txt")).unwrap();
        assert!(matches!(
            validate_host_tree(&source),
            Err(UiaError::UnsafeArchiveEntry(_))
        ));
    }

    #[cfg(unix)]
    #[test]
    fn archive_validation_rejects_links() {
        let temp = TestTempDir::new();
        std::fs::write(temp.path().join("target.txt"), b"data").unwrap();
        std::os::unix::fs::symlink(temp.path().join("target.txt"), temp.path().join("link.txt"))
            .unwrap();
        let archive_path = temp.path().join("links.tar");
        let status = std::process::Command::new("tar")
            .arg("-cf")
            .arg(&archive_path)
            .arg("-C")
            .arg(temp.path())
            .arg("target.txt")
            .arg("link.txt")
            .status()
            .unwrap();
        assert!(status.success());
        assert!(matches!(
            validate_host_archive(&archive_path),
            Err(UiaError::UnsafeArchiveEntry(_))
        ));
    }

    #[test]
    fn archive_validation_rejects_path_traversal() {
        let temp = TestTempDir::new();
        let source = temp.path().join("source");
        std::fs::create_dir(&source).unwrap();
        std::fs::write(source.join("evil.txt"), b"data").unwrap();
        let archive_path = temp.path().join("traversal.tar");
        let status = std::process::Command::new("tar")
            .arg("-cf")
            .arg(&archive_path)
            .arg("--transform=s,evil,../evil,")
            .arg("-C")
            .arg(&source)
            .arg("evil.txt")
            .status()
            .unwrap();
        assert!(status.success());
        assert!(matches!(
            validate_host_archive(&archive_path),
            Err(UiaError::UnsafeArchiveEntry(_))
        ));
    }

    #[test]
    fn host_workspace_export_refuses_non_empty_destination_without_overwrite() {
        let temp = TestTempDir::new();
        let target = temp.path().join("target");
        std::fs::create_dir(&target).unwrap();
        std::fs::write(target.join("existing.txt"), b"data").unwrap();
        assert!(matches!(
            prepare_host_directory(&target, false),
            Err(UiaError::DestinationExists)
        ));
        assert!(prepare_host_directory(&target, true).is_ok());
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
