//! Bounded file and workspace transfer over the QEMU guest agent.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};
use winlane_contracts::FileTransferRef;

use crate::qga::QgaTransport;
use crate::uia::{
    BlockingUiaControl, POWERSHELL_GUEST_PATH, ProcessCapture, UiaError, UiaResult,
    process_output_summary, valid_absolute_windows_path,
};

pub const MAX_FILE_TRANSFER_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_WORKSPACE_TOTAL_BYTES: u64 = 512 * 1024 * 1024;
pub const MAX_WORKSPACE_MEMBER_BYTES: u64 = 256 * 1024 * 1024;
pub const MAX_WORKSPACE_MEMBERS: u64 = 10_000;
pub const MAX_WORKSPACE_PATH_BYTES: usize = 255;
pub const MAX_WORKSPACE_ARCHIVE_BYTES: u64 = 544 * 1024 * 1024;

const TAR_GUEST_PATH: &str = r"C:\Windows\System32\tar.exe";
const TRANSFER_CHUNK_BYTES: usize = 1024 * 1024;
const TAR_BLOCK_BYTES: usize = 512;
const MAX_PAX_HEADER_BYTES: u64 = 8192;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    pub max_members: u64,
    pub max_member_bytes: u64,
    pub max_total_bytes: u64,
    pub max_path_bytes: usize,
}

impl Default for ArchiveLimits {
    fn default() -> Self {
        Self {
            max_members: MAX_WORKSPACE_MEMBERS,
            max_member_bytes: MAX_WORKSPACE_MEMBER_BYTES,
            max_total_bytes: MAX_WORKSPACE_TOTAL_BYTES,
            max_path_bytes: MAX_WORKSPACE_PATH_BYTES,
        }
    }
}

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct WorkspaceTransferRef {
    pub sha256: String,
    pub bytes: u64,
    pub members: u64,
    pub file_bytes: u64,
    pub host_path: String,
    pub guest_path: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ArchiveStats {
    members: u64,
    file_bytes: u64,
}

pub fn import_file_blocking<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    host_path: &Path,
    guest_path: &str,
    overwrite: bool,
) -> UiaResult<FileTransferRef> {
    validate_guest_transfer_path(guest_path, false)?;
    let staging = guest_workspace_sibling_path(guest_path, "file-import.bin")?;
    prepare_guest_file_parent(control, guest_path)?;
    let result = (|| {
        let (bytes, sha256) =
            write_host_file_to_guest(control, host_path, &staging, MAX_FILE_TRANSFER_BYTES)?;
        verify_guest_file(control, &staging, bytes, &sha256)?;
        finalize_guest_file(control, &staging, guest_path, overwrite)?;
        Ok::<(u64, String), UiaError>((bytes, sha256))
    })();
    cleanup_guest_path(control, &staging, false);
    let (bytes, sha256) = result?;
    Ok(FileTransferRef {
        sha256,
        bytes,
        host_path: host_path.to_string_lossy().into_owned(),
        guest_path: guest_path.to_owned(),
    })
}

pub fn export_file_blocking<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    guest_path: &str,
    host_path: &Path,
    overwrite: bool,
) -> UiaResult<FileTransferRef> {
    validate_guest_transfer_path(guest_path, false)?;
    let (guest_size, guest_hash) = guest_file_info(control, guest_path)?;
    if guest_size > MAX_FILE_TRANSFER_BYTES {
        return Err(UiaError::TransferTooLarge {
            actual: guest_size,
            maximum: MAX_FILE_TRANSFER_BYTES,
        });
    }
    let (bytes, sha256) = read_guest_file_to_host(
        control,
        guest_path,
        host_path,
        overwrite,
        MAX_FILE_TRANSFER_BYTES,
        Some((guest_size, &guest_hash)),
    )?;
    Ok(FileTransferRef {
        sha256,
        bytes,
        host_path: host_path.to_string_lossy().into_owned(),
        guest_path: guest_path.to_owned(),
    })
}

pub fn import_workspace_blocking<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    host_dir: &Path,
    guest_dir: &str,
    overwrite: bool,
) -> UiaResult<WorkspaceTransferRef> {
    validate_guest_transfer_path(guest_dir, true)?;
    let (archive, stats) = create_workspace_archive(host_dir)?;
    let archive_path = archive.path.clone();
    let archive_guest_path = guest_unique_path("workspace-import.tar");
    let staging_dir = guest_workspace_sibling_path(guest_dir, "workspace-import")?;
    let backup_dir = guest_workspace_sibling_path(guest_dir, "workspace-backup")?;
    prepare_guest_workspace(control, &staging_dir, guest_dir, overwrite)?;
    let result = (|| {
        let (bytes, sha256) = write_host_file_to_guest(
            control,
            &archive_path,
            &archive_guest_path,
            MAX_WORKSPACE_ARCHIVE_BYTES,
        )?;
        verify_guest_file(control, &archive_guest_path, bytes, &sha256)?;
        run_checked(
            control,
            TAR_GUEST_PATH,
            &[
                "-x".to_owned(),
                "--force-local".to_owned(),
                "-f".to_owned(),
                archive_guest_path.clone(),
                "-C".to_owned(),
                staging_dir.clone(),
            ],
            600_000,
        )?;
        if let Err(error) =
            finalize_guest_workspace(control, &staging_dir, guest_dir, &backup_dir, overwrite)
        {
            if matches!(error, UiaError::DestinationExists) {
                return Err(error);
            }
            return Err(UiaError::WorkspaceFinalizeFailed {
                backup_path: backup_dir.clone(),
                message: error.to_string(),
            });
        }
        Ok::<(u64, String), UiaError>((bytes, sha256))
    })();
    cleanup_guest_path(control, &archive_guest_path, false);
    cleanup_guest_path(control, &staging_dir, true);
    if result.is_ok() {
        cleanup_guest_path(control, &backup_dir, true);
    }
    let (bytes, sha256) = result?;
    Ok(WorkspaceTransferRef {
        sha256,
        bytes,
        members: stats.members,
        file_bytes: stats.file_bytes,
        host_path: host_dir.to_string_lossy().into_owned(),
        guest_path: guest_dir.to_owned(),
    })
}

pub fn export_workspace_blocking<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    guest_dir: &str,
    host_dir: &Path,
    overwrite: bool,
) -> UiaResult<WorkspaceTransferRef> {
    validate_guest_transfer_path(guest_dir, true)?;
    check_host_directory_target(host_dir, overwrite)?;
    let archive = private_archive()?;
    let archive_path = archive.path.clone();
    let archive_guest_path = guest_unique_path("workspace-export.tar");
    let staging = StagingDirectory::create(host_dir, overwrite)?;
    let result = (|| {
        run_checked(
            control,
            TAR_GUEST_PATH,
            &[
                "-c".to_owned(),
                "--force-local".to_owned(),
                "-f".to_owned(),
                archive_guest_path.clone(),
                "-C".to_owned(),
                guest_dir.to_owned(),
                ".".to_owned(),
            ],
            600_000,
        )?;
        let (guest_size, guest_hash) = guest_file_info(control, &archive_guest_path)?;
        if guest_size > MAX_WORKSPACE_ARCHIVE_BYTES {
            return Err(UiaError::TransferTooLarge {
                actual: guest_size,
                maximum: MAX_WORKSPACE_ARCHIVE_BYTES,
            });
        }
        let (bytes, sha256) = read_guest_file_to_host(
            control,
            &archive_guest_path,
            &archive_path,
            true,
            MAX_WORKSPACE_ARCHIVE_BYTES,
            Some((guest_size, &guest_hash)),
        )?;
        let stats =
            extract_workspace_archive(&archive_path, &staging.path, ArchiveLimits::default())?;
        staging.persist()?;
        Ok::<(u64, String, ArchiveStats), UiaError>((bytes, sha256, stats))
    })();
    cleanup_guest_path(control, &archive_guest_path, false);
    let (bytes, sha256, stats) = result?;
    Ok(WorkspaceTransferRef {
        sha256,
        bytes,
        members: stats.members,
        file_bytes: stats.file_bytes,
        host_path: host_dir.to_string_lossy().into_owned(),
        guest_path: guest_dir.to_owned(),
    })
}

fn write_host_file_to_guest<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    host_path: &Path,
    guest_path: &str,
    maximum: u64,
) -> UiaResult<(u64, String)> {
    let mut input = File::open(host_path).map_err(|error| host_io(host_path, error))?;
    let metadata = input
        .metadata()
        .map_err(|error| host_io(host_path, error))?;
    if !metadata.is_file() {
        return Err(UiaError::HostCommand(
            "host import path is not a file".to_owned(),
        ));
    }
    if metadata.len() > maximum {
        return Err(UiaError::TransferTooLarge {
            actual: metadata.len(),
            maximum,
        });
    }
    let handle = control.client_mut().file_open(guest_path, "wb")?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let result = (|| {
        let mut buffer = vec![0_u8; TRANSFER_CHUNK_BYTES];
        loop {
            let count = input
                .read(&mut buffer)
                .map_err(|error| host_io(host_path, error))?;
            if count == 0 {
                break;
            }
            let next_total = total + count as u64;
            if next_total > maximum {
                return Err(UiaError::TransferTooLarge {
                    actual: next_total,
                    maximum,
                });
            }
            hasher.update(&buffer[..count]);
            control
                .client_mut()
                .file_write_chunks(handle, &buffer[..count])?;
            total = next_total;
        }
        control.client_mut().file_flush(handle)?;
        Ok::<(), UiaError>(())
    })();
    let close_result = control.client_mut().file_close(handle);
    result?;
    close_result?;
    if total != metadata.len() {
        return Err(UiaError::TransferVerificationFailed);
    }
    Ok((total, hex_digest(hasher)))
}

fn read_guest_file_to_host<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    guest_path: &str,
    host_path: &Path,
    overwrite: bool,
    maximum: u64,
    expected: Option<(u64, &str)>,
) -> UiaResult<(u64, String)> {
    if host_path.exists() && !overwrite {
        return Err(UiaError::DestinationExists);
    }
    let mut staging = StagingHostFile::create(host_path, overwrite)?;
    let handle = control.client_mut().file_open(guest_path, "rb")?;
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let result = (|| {
        loop {
            let chunk = control
                .client_mut()
                .file_read(handle, TRANSFER_CHUNK_BYTES as u64)?;
            if !chunk.bytes.is_empty() {
                total += chunk.bytes.len() as u64;
                if total > maximum {
                    return Err(UiaError::TransferTooLarge {
                        actual: total,
                        maximum,
                    });
                }
                staging
                    .file
                    .write_all(&chunk.bytes)
                    .map_err(|error| host_io(&staging.path, error))?;
                hasher.update(&chunk.bytes);
            }
            if chunk.eof {
                break;
            }
        }
        staging
            .file
            .flush()
            .map_err(|error| host_io(&staging.path, error))?;
        Ok::<(), UiaError>(())
    })();
    let close_result = control.client_mut().file_close(handle);
    result?;
    close_result?;
    let sha256 = hex_digest(hasher);
    if let Some((expected_size, expected_hash)) = expected
        && (total != expected_size || sha256 != expected_hash)
    {
        return Err(UiaError::TransferVerificationFailed);
    }
    staging.persist()?;
    Ok((total, sha256))
}

fn verify_guest_file<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    guest_path: &str,
    size: u64,
    sha256: &str,
) -> UiaResult<()> {
    let (guest_size, guest_hash) = guest_file_info(control, guest_path)?;
    if guest_size == size && guest_hash == sha256 {
        Ok(())
    } else {
        Err(UiaError::TransferVerificationFailed)
    }
}

fn prepare_guest_file_parent<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    target: &str,
) -> UiaResult<()> {
    let target = ps_quote(target);
    let script = format!(
        "$parent=Split-Path -Parent {target};if($parent){{New-Item -ItemType Directory -Force -LiteralPath $parent|Out-Null}};exit 0"
    );
    let capture = run_powershell(control, &script, 30_000)?;
    if capture.exit_code == Some(0) {
        Ok(())
    } else {
        Err(UiaError::Helper {
            exit_code: capture.exit_code,
            message: process_output_summary(&capture),
        })
    }
}

fn finalize_guest_file<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    staging: &str,
    target: &str,
    overwrite: bool,
) -> UiaResult<()> {
    let staging = ps_quote(staging);
    let target = ps_quote(target);
    let overwrite_check = if overwrite {
        ""
    } else {
        "if(Test-Path -LiteralPath $d){exit 3};"
    };
    let script = format!(
        "$s={staging};$d={target};$backup=$null;{overwrite_check}try{{if(Test-Path -LiteralPath $d){{$backup=$d+'.winlane-backup-'+[System.IO.Path]::GetRandomFileName();[System.IO.File]::Replace($s,$d,$backup);[System.IO.File]::Delete($backup)}}else{{[System.IO.File]::Move($s,$d)}};exit 0}}catch{{if(($null -ne $backup) -and (Test-Path -LiteralPath $backup)){{Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue}};Write-Error $_.Exception.Message;exit 1}}"
    );
    let capture = run_powershell(control, &script, 30_000)?;
    if capture.exit_code == Some(0) {
        Ok(())
    } else if capture.exit_code == Some(3) {
        Err(UiaError::DestinationExists)
    } else {
        Err(UiaError::Helper {
            exit_code: capture.exit_code,
            message: process_output_summary(&capture),
        })
    }
}

fn prepare_guest_workspace<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    staging: &str,
    target: &str,
    overwrite: bool,
) -> UiaResult<()> {
    let staging = ps_quote(staging);
    let target = ps_quote(target);
    let overwrite_check = if overwrite {
        ""
    } else {
        "if(Get-ChildItem -LiteralPath $target -Force|Select-Object -First 1){exit 3};"
    };
    let script = format!(
        "$staging={staging};$target={target};New-Item -ItemType Directory -Force -LiteralPath $staging|Out-Null;if(Test-Path -LiteralPath $target){{$item=Get-Item -LiteralPath $target -Force;if(-not $item.PSIsContainer){{exit 4}};{overwrite_check}}}"
    );
    let capture = run_powershell(control, &script, 30_000)?;
    match capture.exit_code {
        Some(0) => Ok(()),
        Some(3) => Err(UiaError::DestinationExists),
        Some(4) => Err(UiaError::DestinationExists),
        exit_code => Err(UiaError::Helper {
            exit_code,
            message: process_output_summary(&capture),
        }),
    }
}

fn finalize_guest_workspace<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    staging: &str,
    target: &str,
    backup: &str,
    overwrite: bool,
) -> UiaResult<()> {
    let staging = ps_quote(staging);
    let target = ps_quote(target);
    let backup = ps_quote(backup);
    let overwrite_check = if overwrite {
        ""
    } else {
        "if(Get-ChildItem -LiteralPath $target -Force|Select-Object -First 1){exit 3};"
    };
    let script = format!(
        "$staging={staging};$target={target};$backup={backup};$parent=Split-Path -Parent $target;if($parent){{New-Item -ItemType Directory -Force -LiteralPath $parent|Out-Null}};$moved=$false;if(Test-Path -LiteralPath $target){{$item=Get-Item -LiteralPath $target -Force;if(-not $item.PSIsContainer){{exit 4}};{overwrite_check}[System.IO.Directory]::Move($target,$backup);$moved=$true}};try{{[System.IO.Directory]::Move($staging,$target)}}catch{{if(($moved) -and (Test-Path -LiteralPath $backup) -and -not (Test-Path -LiteralPath $target)){{try{{[System.IO.Directory]::Move($backup,$target)}}catch{{}}}};Write-Error $_.Exception.Message;exit 1}};if($moved){{Remove-Item -LiteralPath $backup -Recurse -Force -ErrorAction SilentlyContinue}};exit 0"
    );
    let capture = run_powershell(control, &script, 60_000)?;
    match capture.exit_code {
        Some(0) => Ok(()),
        Some(3) | Some(4) => Err(UiaError::DestinationExists),
        exit_code => Err(UiaError::Helper {
            exit_code,
            message: process_output_summary(&capture),
        }),
    }
}

fn run_checked<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    command: &str,
    args: &[String],
    timeout_ms: u64,
) -> UiaResult<()> {
    let capture = control.run_process_blocking(command, args, timeout_ms)?;
    if capture.exit_code == Some(0) {
        Ok(())
    } else {
        Err(UiaError::Helper {
            exit_code: capture.exit_code,
            message: process_output_summary(&capture),
        })
    }
}

fn run_powershell<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    script: &str,
    timeout_ms: u64,
) -> UiaResult<ProcessCapture> {
    let script = format!(
        "$ErrorActionPreference='Stop';trap{{[Console]::Error.WriteLine($_.Exception.Message);exit 1}};{script}"
    );
    control.run_process_blocking(
        POWERSHELL_GUEST_PATH,
        &[
            "-NoProfile".to_owned(),
            "-NonInteractive".to_owned(),
            "-ExecutionPolicy".to_owned(),
            "Bypass".to_owned(),
            "-Command".to_owned(),
            script,
        ],
        timeout_ms,
    )
}

fn cleanup_guest_path<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    path: &str,
    recurse: bool,
) {
    let path = ps_quote(path);
    let recurse = if recurse { " -Recurse" } else { "" };
    let script = format!(
        "if(Test-Path -LiteralPath {path}){{Remove-Item -LiteralPath {path} -Force{recurse} -ErrorAction SilentlyContinue}}"
    );
    let _ = run_powershell(control, &script, 10_000);
}

#[derive(Debug, Deserialize)]
struct GuestFileInfo {
    size: u64,
    sha256: String,
}

fn guest_file_info<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    path: &str,
) -> UiaResult<(u64, String)> {
    let quoted = ps_quote(path);
    let script = format!(
        "$p={quoted};if(-not(Test-Path -LiteralPath $p -PathType Leaf)){{exit 3}};$item=Get-Item -LiteralPath $p;$hash=Get-FileHash -Algorithm SHA256 -LiteralPath $p;[pscustomobject]@{{size=$item.Length;sha256=$hash.Hash.ToLowerInvariant()}}|ConvertTo-Json -Compress"
    );
    let capture = run_powershell(control, &script, 60_000)?;
    if capture.exit_code != Some(0) {
        return Err(UiaError::Helper {
            exit_code: capture.exit_code,
            message: process_output_summary(&capture),
        });
    }
    let text = std::str::from_utf8(&capture.stdout).map_err(|_| UiaError::InvalidUtf8)?;
    let info: GuestFileInfo = serde_json::from_str(text.trim().trim_start_matches('\u{feff}'))?;
    Ok((info.size, info.sha256))
}

fn validate_guest_transfer_path(path: &str, workspace: bool) -> UiaResult<()> {
    let device = path.starts_with(r"\\.\")
        || path.starts_with(r"\\?\")
        || path.starts_with("//./")
        || path.starts_with("//?/");
    let traversal = path
        .split(['\\', '/'])
        .any(|component| matches!(component, ".." | "."));
    let trailing_separator = path.len() > 3 && path.ends_with(['\\', '/']);
    if device
        || traversal
        || trailing_separator
        || path.len() > 1024
        || !valid_absolute_windows_path(path)
    {
        return Err(UiaError::InvalidGuestPath);
    }
    if workspace {
        let unc = path.starts_with(r"\\") || path.starts_with("//");
        let bytes = path.as_bytes();
        let drive_root = bytes.len() == 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'\\' | b'/');
        let trailing_separator =
            bytes.len() > 3 && matches!(bytes.last(), Some(b'\\') | Some(b'/'));
        if unc || drive_root || trailing_separator {
            return Err(UiaError::InvalidGuestPath);
        }
    }
    Ok(())
}

fn ps_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

fn host_io(path: &Path, source: std::io::Error) -> UiaError {
    UiaError::HostIo {
        path: path.to_string_lossy().into_owned(),
        source,
    }
}

struct StagingHostFile {
    target: PathBuf,
    path: PathBuf,
    file: File,
    overwrite: bool,
    persisted: bool,
}

impl StagingHostFile {
    fn create(target: &Path, overwrite: bool) -> UiaResult<Self> {
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|error| host_io(parent, error))?;
        for counter in 0..100 {
            let path = parent.join(unique_leaf_name(".winlane-file-", counter));
            match OpenOptions::new().write(true).create_new(true).open(&path) {
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
                Err(error) => return Err(host_io(&path, error)),
            }
        }
        Err(UiaError::HostCommand(
            "could not create staging file".to_owned(),
        ))
    }

    fn persist(mut self) -> UiaResult<()> {
        self.file
            .sync_all()
            .map_err(|error| host_io(&self.path, error))?;
        let result = if self.overwrite {
            fs::rename(&self.path, &self.target)
        } else {
            fs::hard_link(&self.path, &self.target)
        };
        match result {
            Ok(()) => {
                self.persisted = true;
                if !self.overwrite {
                    let _ = fs::remove_file(&self.path);
                }
                Ok(())
            }
            Err(_) if !self.overwrite && self.target.exists() => Err(UiaError::DestinationExists),
            Err(error) => Err(host_io(&self.target, error)),
        }
    }
}

impl Drop for StagingHostFile {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = fs::remove_file(&self.path);
        }
    }
}

struct HostArchive {
    directory: PathBuf,
    path: PathBuf,
}

impl Drop for HostArchive {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.directory);
    }
}

fn create_private_dir(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        fs::DirBuilder::new().mode(0o700).create(path)?;
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o700))
    }
    #[cfg(not(unix))]
    {
        fs::create_dir(path)
    }
}

fn private_archive() -> UiaResult<HostArchive> {
    for counter in 0..100 {
        let directory = std::env::temp_dir().join(unique_leaf_name("winlane-archive-", counter));
        match create_private_dir(&directory) {
            Ok(()) => {
                return Ok(HostArchive {
                    path: directory.join("workspace.tar"),
                    directory,
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(host_io(&directory, error)),
        }
    }
    Err(UiaError::HostCommand(
        "could not create private archive directory".to_owned(),
    ))
}

fn unique_leaf_name(prefix: &str, counter: u32) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!("{prefix}{}-{nanos}-{counter}", std::process::id())
}

fn guest_unique_path(name: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    format!(
        r"C:\Windows\Temp\winlane-{}-{nanos}-{name}",
        std::process::id()
    )
}

fn guest_workspace_sibling_path(target: &str, kind: &str) -> UiaResult<String> {
    let trimmed = target.trim_end_matches(['\\', '/']);
    let Some(index) = trimmed.rfind(['\\', '/']) else {
        return Err(UiaError::InvalidGuestPath);
    };
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    Ok(format!(
        "{}\\.winlane-{}-{nanos}-{kind}",
        &trimmed[..=index],
        std::process::id()
    ))
}

#[derive(Debug)]
struct SourceEntry {
    relative: String,
    path: PathBuf,
    directory: bool,
    size: u64,
    mtime: u64,
}

fn create_workspace_archive(source: &Path) -> UiaResult<(HostArchive, ArchiveStats)> {
    if !source.is_dir() {
        return Err(UiaError::HostCommand(
            "workspace host path is not a directory".to_owned(),
        ));
    }
    let mut entries = Vec::new();
    let mut file_bytes = 0_u64;
    let mut case_keys = HashSet::new();
    collect_source_entries(
        source,
        "",
        &mut entries,
        &mut file_bytes,
        &mut case_keys,
        ArchiveLimits::default(),
    )?;
    entries.sort_by(|left, right| left.relative.cmp(&right.relative));
    let archive = private_archive()?;
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&archive.path)
        .map_err(|error| host_io(&archive.path, error))?;
    let mut writer = std::io::BufWriter::new(file);
    for entry in &entries {
        write_tar_entry(&mut writer, entry)?;
    }
    writer
        .write_all(&[0_u8; TAR_BLOCK_BYTES * 2])
        .map_err(|error| host_io(&archive.path, error))?;
    writer
        .flush()
        .map_err(|error| host_io(&archive.path, error))?;
    let archive_len = fs::metadata(&archive.path)
        .map_err(|error| host_io(&archive.path, error))?
        .len();
    if archive_len > MAX_WORKSPACE_ARCHIVE_BYTES {
        return Err(UiaError::TransferTooLarge {
            actual: archive_len,
            maximum: MAX_WORKSPACE_ARCHIVE_BYTES,
        });
    }
    Ok((
        archive,
        ArchiveStats {
            members: entries.len() as u64,
            file_bytes,
        },
    ))
}

fn collect_source_entries(
    directory: &Path,
    prefix: &str,
    entries: &mut Vec<SourceEntry>,
    file_bytes: &mut u64,
    case_keys: &mut HashSet<String>,
    limits: ArchiveLimits,
) -> UiaResult<()> {
    let mut children = Vec::new();
    for entry in fs::read_dir(directory).map_err(|error| host_io(directory, error))? {
        children.push(entry.map_err(|error| host_io(directory, error))?);
    }
    children.sort_by_key(|entry| entry.file_name());
    for child in children {
        let path = child.path();
        let metadata = fs::symlink_metadata(&path).map_err(|error| host_io(&path, error))?;
        if metadata.file_type().is_symlink() || (!metadata.is_dir() && !metadata.is_file()) {
            return Err(UiaError::UnsafeArchiveEntry(
                path.to_string_lossy().into_owned(),
            ));
        }
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .ok_or_else(|| UiaError::UnsafeArchiveEntry(path.to_string_lossy().into_owned()))?;
        validate_member_component(name)?;
        let relative = if prefix.is_empty() {
            name.to_owned()
        } else {
            format!("{prefix}/{name}")
        };
        if relative.len() > limits.max_path_bytes {
            return Err(UiaError::UnsafeArchiveEntry(relative));
        }
        if entries.len() as u64 >= limits.max_members {
            return Err(UiaError::TooManyMembers {
                maximum: limits.max_members,
            });
        }
        let directory_entry = metadata.is_dir();
        let size = if directory_entry { 0 } else { metadata.len() };
        if !directory_entry && size > limits.max_member_bytes {
            return Err(UiaError::MemberTooLarge {
                actual: size,
                maximum: limits.max_member_bytes,
            });
        }
        *file_bytes = file_bytes
            .checked_add(size)
            .ok_or(UiaError::TransferTooLarge {
                actual: u64::MAX,
                maximum: limits.max_total_bytes,
            })?;
        if *file_bytes > limits.max_total_bytes {
            return Err(UiaError::TransferTooLarge {
                actual: *file_bytes,
                maximum: limits.max_total_bytes,
            });
        }
        let mtime = metadata
            .modified()
            .ok()
            .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        // Conservative Windows collision key: Unicode lowercase over the normalized `/` path.
        if !case_keys.insert(relative.to_lowercase()) {
            return Err(UiaError::UnsafeArchiveEntry(format!(
                "case-insensitive duplicate archive member {relative}"
            )));
        }
        entries.push(SourceEntry {
            relative: relative.clone(),
            path: path.clone(),
            directory: directory_entry,
            size,
            mtime,
        });
        if directory_entry {
            collect_source_entries(&path, &relative, entries, file_bytes, case_keys, limits)?;
        }
    }
    Ok(())
}

fn validate_member_component(name: &str) -> UiaResult<()> {
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
    let invalid = name.is_empty()
        || name.len() > 240
        || reserved
        || name
            .chars()
            .any(|c| c.is_control() || matches!(c, '<' | '>' | ':' | '"' | '|' | '?' | '*' | '\\'))
        || name.ends_with(['.', ' ']);
    if invalid {
        Err(UiaError::UnsafeArchiveEntry(name.to_owned()))
    } else {
        Ok(())
    }
}

fn write_tar_entry(writer: &mut impl Write, entry: &SourceEntry) -> UiaResult<()> {
    write_tar_header(
        writer,
        &entry.relative,
        entry.directory,
        entry.size,
        entry.mtime,
    )?;
    if entry.directory {
        return Ok(());
    }
    let mut input = File::open(&entry.path).map_err(|error| host_io(&entry.path, error))?;
    let mut remaining = entry.size;
    let mut buffer = vec![0_u8; 128 * 1024];
    while remaining > 0 {
        let wanted = remaining.min(buffer.len() as u64) as usize;
        let count = input
            .read(&mut buffer[..wanted])
            .map_err(|error| host_io(&entry.path, error))?;
        if count == 0 {
            return Err(UiaError::MalformedArchive(
                "source file changed while archiving".to_owned(),
            ));
        }
        writer
            .write_all(&buffer[..count])
            .map_err(|error| host_io(&entry.path, error))?;
        remaining -= count as u64;
    }
    let mut extra = [0_u8; 1];
    if input
        .read(&mut extra)
        .map_err(|error| host_io(&entry.path, error))?
        != 0
    {
        return Err(UiaError::MalformedArchive(
            "source file changed while archiving".to_owned(),
        ));
    }
    write_tar_padding(writer, entry.size).map_err(|error| host_io(&entry.path, error))?;
    Ok(())
}

fn write_tar_header(
    writer: &mut impl Write,
    path: &str,
    directory: bool,
    size: u64,
    mtime: u64,
) -> UiaResult<()> {
    let path = if directory {
        format!("{path}/")
    } else {
        path.to_owned()
    };
    let needs_pax = !path.is_ascii() || split_ustar_path(&path).is_none();
    let ustar_path = if needs_pax {
        write_pax_path_header(writer, &path, mtime)?;
        if split_ustar_path(&path).is_some() {
            path.clone()
        } else {
            fallback_ustar_path(&path, directory)
        }
    } else {
        path.clone()
    };
    write_ustar_header(
        writer,
        &ustar_path,
        directory,
        size,
        mtime,
        if directory { b'5' } else { b'0' },
    )
}

fn write_pax_path_header(writer: &mut impl Write, path: &str, mtime: u64) -> UiaResult<()> {
    let record = pax_record("path", path);
    write_ustar_header(
        writer,
        &pax_header_name(path),
        false,
        record.len() as u64,
        mtime,
        b'x',
    )?;
    writer
        .write_all(&record)
        .map_err(|error| UiaError::HostIo {
            path: "workspace archive".to_owned(),
            source: error,
        })?;
    write_tar_padding(writer, record.len() as u64).map_err(|error| UiaError::HostIo {
        path: "workspace archive".to_owned(),
        source: error,
    })
}

fn pax_record(key: &str, value: &str) -> Vec<u8> {
    let mut digits = 1_usize;
    loop {
        let length = digits + 1 + key.len() + 1 + value.len() + 1;
        let actual_digits = length.to_string().len();
        if actual_digits == digits {
            return format!("{length} {key}={value}\n").into_bytes();
        }
        digits = actual_digits;
    }
}

fn archive_path_digest(path: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(path.as_bytes());
    hex_digest(hasher)[..16].to_owned()
}

fn pax_header_name(path: &str) -> String {
    format!("PaxHeader/winlane-{}", archive_path_digest(path))
}

fn fallback_ustar_path(path: &str, directory: bool) -> String {
    let mut fallback = format!("winlane-path-{}", archive_path_digest(path));
    if directory {
        fallback.push('/');
    }
    fallback
}

fn write_ustar_header(
    writer: &mut impl Write,
    path: &str,
    directory: bool,
    size: u64,
    mtime: u64,
    typeflag: u8,
) -> UiaResult<()> {
    let mut header = [0_u8; TAR_BLOCK_BYTES];
    let (prefix, name) =
        split_ustar_path(path).ok_or_else(|| UiaError::UnsafeArchiveEntry(path.to_owned()))?;
    write_bytes(&mut header[0..100], name.as_bytes());
    if let Some(prefix) = prefix {
        write_bytes(&mut header[345..500], prefix.as_bytes());
    }
    write_octal(&mut header[100..108], if directory { 0o755 } else { 0o644 });
    write_octal(&mut header[108..116], 0);
    write_octal(&mut header[116..124], 0);
    write_octal(&mut header[124..136], size);
    write_octal(&mut header[136..148], mtime);
    for byte in &mut header[148..156] {
        *byte = b' ';
    }
    header[156] = typeflag;
    write_bytes(&mut header[257..263], b"ustar\0");
    write_bytes(&mut header[263..265], b"00");
    let checksum = header.iter().map(|byte| u64::from(*byte)).sum::<u64>();
    let encoded = format!("{checksum:06o}\0 ");
    write_bytes(&mut header[148..156], encoded.as_bytes());
    writer
        .write_all(&header)
        .map_err(|error| UiaError::HostIo {
            path: "workspace archive".to_owned(),
            source: error,
        })?;
    Ok(())
}

fn write_bytes(field: &mut [u8], bytes: &[u8]) {
    field[..bytes.len()].copy_from_slice(bytes);
}

fn write_octal(field: &mut [u8], value: u64) {
    let width = field.len() - 1;
    let encoded = format!("{value:0width$o}");
    write_bytes(field, encoded.as_bytes());
}

fn split_ustar_path(path: &str) -> Option<(Option<&str>, &str)> {
    if path.len() <= 100 {
        return Some((None, path));
    }
    if path.len() > 255 {
        return None;
    }
    let mut selected = None;
    for (index, _) in path.match_indices('/') {
        let prefix = &path[..index];
        let name = &path[index + 1..];
        if !prefix.is_empty() && prefix.len() <= 155 && !name.is_empty() && name.len() <= 100 {
            selected = Some((Some(prefix), name));
        }
    }
    selected
}

fn write_tar_padding(writer: &mut impl Write, size: u64) -> std::io::Result<()> {
    let padding =
        (TAR_BLOCK_BYTES as u64 - (size % TAR_BLOCK_BYTES as u64)) % TAR_BLOCK_BYTES as u64;
    if padding > 0 {
        writer.write_all(&vec![0_u8; padding as usize])?;
    }
    Ok(())
}

fn extract_workspace_archive(
    archive: &Path,
    target: &Path,
    limits: ArchiveLimits,
) -> UiaResult<ArchiveStats> {
    let file = File::open(archive).map_err(|error| host_io(archive, error))?;
    extract_tar(file, target, limits)
}

fn extract_tar(
    mut reader: impl Read,
    target: &Path,
    limits: ArchiveLimits,
) -> UiaResult<ArchiveStats> {
    let mut members = 0_u64;
    let mut file_bytes = 0_u64;
    let mut seen = HashSet::new();
    let mut pending_path = None;
    let mut pending_size = None;
    loop {
        let mut block = [0_u8; TAR_BLOCK_BYTES];
        read_exact_archive(&mut reader, &mut block)?;
        if block.iter().all(|byte| *byte == 0) {
            let mut second = [0_u8; TAR_BLOCK_BYTES];
            read_exact_archive(&mut reader, &mut second)?;
            if !second.iter().all(|byte| *byte == 0) {
                return Err(malformed("archive end marker is incomplete"));
            }
            reject_nonzero_trailing_data(&mut reader)?;
            return Ok(ArchiveStats {
                members,
                file_bytes,
            });
        }
        let header = parse_tar_header(&block)?;
        match header.typeflag {
            b'x' => {
                if header.size > MAX_PAX_HEADER_BYTES {
                    return Err(malformed("pax header exceeds size limit"));
                }
                let data = read_tar_data_vec(&mut reader, header.size)?;
                let (path, size) = parse_pax_header(&data)?;
                if path.is_some() {
                    pending_path = path;
                }
                if size.is_some() {
                    pending_size = size;
                }
            }
            b'L' => {
                if header.size as usize > limits.max_path_bytes + 1 {
                    return Err(malformed("long path entry exceeds size limit"));
                }
                let data = read_tar_data_vec(&mut reader, header.size)?;
                let end = data
                    .iter()
                    .position(|byte| *byte == 0)
                    .unwrap_or(data.len());
                let path = std::str::from_utf8(&data[..end])
                    .map_err(|_| malformed("long path is not UTF-8"))?
                    .trim_end_matches('\n')
                    .to_owned();
                pending_path = Some(path);
            }
            b'0' | 0 | b'5' => {
                let path = pending_path.take().unwrap_or(header.path);
                let size = pending_size.take().unwrap_or(header.size);
                let Some(relative) = normalize_archive_path(&path, limits.max_path_bytes)? else {
                    skip_tar_data(&mut reader, size)?;
                    continue;
                };
                // Conservative Windows collision key: Unicode lowercase over the normalized `/` path.
                if !seen.insert(relative.to_lowercase()) {
                    return Err(UiaError::UnsafeArchiveEntry(format!(
                        "duplicate archive member {relative}"
                    )));
                }
                if members >= limits.max_members {
                    return Err(UiaError::TooManyMembers {
                        maximum: limits.max_members,
                    });
                }
                members += 1;
                let output_path = target.join(PathBuf::from(
                    relative.replace('/', std::path::MAIN_SEPARATOR_STR),
                ));
                let mut extracted_file = None;
                if header.typeflag == b'5' {
                    if size != 0 {
                        return Err(malformed("directory entry has file data"));
                    }
                    fs::create_dir_all(&output_path)
                        .map_err(|error| host_io(&output_path, error))?;
                } else {
                    if size > limits.max_member_bytes {
                        return Err(UiaError::MemberTooLarge {
                            actual: size,
                            maximum: limits.max_member_bytes,
                        });
                    }
                    file_bytes =
                        file_bytes
                            .checked_add(size)
                            .ok_or(UiaError::TransferTooLarge {
                                actual: u64::MAX,
                                maximum: limits.max_total_bytes,
                            })?;
                    if file_bytes > limits.max_total_bytes {
                        return Err(UiaError::TransferTooLarge {
                            actual: file_bytes,
                            maximum: limits.max_total_bytes,
                        });
                    }
                    if let Some(parent) = output_path.parent() {
                        fs::create_dir_all(parent).map_err(|error| host_io(parent, error))?;
                    }
                    let mut output = OpenOptions::new()
                        .write(true)
                        .create_new(true)
                        .open(&output_path)
                        .map_err(|error| host_io(&output_path, error))?;
                    if let Err(error) =
                        copy_exact_archive(&mut reader, &mut output, size, &output_path).and_then(
                            |()| output.flush().map_err(|error| host_io(&output_path, error)),
                        )
                    {
                        let _ = fs::remove_file(&output_path);
                        return Err(error);
                    }
                    extracted_file = Some(output_path);
                }
                if let Err(error) = skip_tar_padding(&mut reader, size) {
                    if let Some(path) = extracted_file {
                        let _ = fs::remove_file(path);
                    }
                    return Err(error);
                }
            }
            _ => {
                return Err(UiaError::UnsafeArchiveEntry(format!(
                    "unsupported tar entry type {}",
                    header.typeflag
                )));
            }
        }
    }
}

fn normalize_archive_path(path: &str, max_path_bytes: usize) -> UiaResult<Option<String>> {
    let mut value = path;
    while let Some(rest) = value.strip_prefix("./") {
        value = rest;
    }
    let value = value.trim_end_matches('/');
    if value.is_empty() || value == "." {
        return Ok(None);
    }
    if value.len() > max_path_bytes
        || value.starts_with('/')
        || value.starts_with('\\')
        || value.contains('\\')
    {
        return Err(UiaError::UnsafeArchiveEntry(path.to_owned()));
    }
    let components = value.split('/').collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| component.is_empty() || matches!(*component, "." | ".."))
    {
        return Err(UiaError::UnsafeArchiveEntry(path.to_owned()));
    }
    for component in components {
        validate_member_component(component)?;
    }
    Ok(Some(value.to_owned()))
}

#[derive(Debug)]
struct TarHeader {
    path: String,
    size: u64,
    typeflag: u8,
}

fn parse_tar_header(block: &[u8; TAR_BLOCK_BYTES]) -> UiaResult<TarHeader> {
    let stored_checksum =
        parse_octal(&block[148..156]).ok_or_else(|| malformed("tar checksum is invalid"))?;
    let mut checksum_input = *block;
    for byte in &mut checksum_input[148..156] {
        *byte = b' ';
    }
    let actual_checksum = checksum_input
        .iter()
        .map(|byte| u64::from(*byte))
        .sum::<u64>();
    if stored_checksum != actual_checksum {
        return Err(malformed("tar checksum mismatch"));
    }
    if &block[257..262] != b"ustar" {
        return Err(malformed("tar magic is missing"));
    }
    let name = parse_tar_string(&block[0..100])?;
    let prefix = parse_tar_string(&block[345..500])?;
    let path = if prefix.is_empty() {
        name
    } else {
        format!("{prefix}/{name}")
    };
    let size = parse_octal(&block[124..136]).ok_or_else(|| malformed("tar size is invalid"))?;
    Ok(TarHeader {
        path,
        size,
        typeflag: block[156],
    })
}

fn parse_tar_string(field: &[u8]) -> UiaResult<String> {
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    std::str::from_utf8(&field[..end])
        .map(str::to_owned)
        .map_err(|_| malformed("tar path is not UTF-8"))
}

fn parse_octal(field: &[u8]) -> Option<u64> {
    let mut value = 0_u64;
    let mut digits = false;
    for byte in field {
        match byte {
            b' ' | 0 if !digits => continue,
            b' ' | 0 => break,
            b'0'..=b'7' => {
                digits = true;
                value = value.checked_mul(8)?.checked_add(u64::from(byte - b'0'))?;
            }
            _ => return None,
        }
    }
    Some(value)
}

fn parse_pax_header(data: &[u8]) -> UiaResult<(Option<String>, Option<u64>)> {
    let mut position = 0_usize;
    let mut path = None;
    let mut size = None;
    while position < data.len() {
        let space_offset = data[position..]
            .iter()
            .position(|byte| *byte == b' ')
            .ok_or_else(|| malformed("pax record missing length separator"))?;
        let length_text = std::str::from_utf8(&data[position..position + space_offset])
            .map_err(|_| malformed("pax length is not ASCII"))?;
        let length = length_text
            .parse::<usize>()
            .map_err(|_| malformed("pax length is invalid"))?;
        if length == 0 || position + length > data.len() || length <= space_offset + 2 {
            return Err(malformed("pax record length is invalid"));
        }
        let end = position + length;
        if data[end - 1] != b'\n' {
            return Err(malformed("pax record is not newline terminated"));
        }
        let record = &data[position + space_offset + 1..end - 1];
        let equals = record
            .iter()
            .position(|byte| *byte == b'=')
            .ok_or_else(|| malformed("pax record missing key separator"))?;
        let key = &record[..equals];
        let value = &record[equals + 1..];
        if key == b"path" {
            path = Some(
                std::str::from_utf8(value)
                    .map_err(|_| malformed("pax path is not UTF-8"))?
                    .to_owned(),
            );
        } else if key == b"size" {
            let text =
                std::str::from_utf8(value).map_err(|_| malformed("pax size is not ASCII"))?;
            size = Some(
                text.parse::<u64>()
                    .map_err(|_| malformed("pax size is invalid"))?,
            );
        }
        position = end;
    }
    Ok((path, size))
}

fn read_exact_archive(reader: &mut impl Read, buffer: &mut [u8]) -> UiaResult<()> {
    reader
        .read_exact(buffer)
        .map_err(|_| malformed("archive is truncated"))
}

fn read_tar_data_vec(reader: &mut impl Read, size: u64) -> UiaResult<Vec<u8>> {
    let mut data = vec![0_u8; size as usize];
    read_exact_archive(reader, &mut data)?;
    skip_tar_padding(reader, size)?;
    Ok(data)
}

fn copy_exact_archive(
    reader: &mut impl Read,
    output: &mut File,
    size: u64,
    output_path: &Path,
) -> UiaResult<()> {
    let mut remaining = size;
    let mut buffer = vec![0_u8; 128 * 1024];
    while remaining > 0 {
        let wanted = remaining.min(buffer.len() as u64) as usize;
        read_exact_archive(reader, &mut buffer[..wanted])?;
        output
            .write_all(&buffer[..wanted])
            .map_err(|error| host_io(output_path, error))?;
        remaining -= wanted as u64;
    }
    Ok(())
}

fn skip_tar_data(reader: &mut impl Read, size: u64) -> UiaResult<()> {
    let mut remaining = size;
    let mut buffer = vec![0_u8; 128 * 1024];
    while remaining > 0 {
        let wanted = remaining.min(buffer.len() as u64) as usize;
        read_exact_archive(reader, &mut buffer[..wanted])?;
        remaining -= wanted as u64;
    }
    skip_tar_padding(reader, size)
}

fn skip_tar_padding(reader: &mut impl Read, size: u64) -> UiaResult<()> {
    let padding =
        (TAR_BLOCK_BYTES as u64 - (size % TAR_BLOCK_BYTES as u64)) % TAR_BLOCK_BYTES as u64;
    let mut buffer = vec![0_u8; padding as usize];
    read_exact_archive(reader, &mut buffer)
}

fn reject_nonzero_trailing_data(reader: &mut impl Read) -> UiaResult<()> {
    let mut buffer = [0_u8; 8192];
    loop {
        let count = reader
            .read(&mut buffer)
            .map_err(|_| malformed("archive trailing data is unreadable"))?;
        if count == 0 {
            return Ok(());
        }
        if buffer[..count].iter().any(|byte| *byte != 0) {
            return Err(malformed("archive has nonzero trailing data"));
        }
    }
}

fn malformed(message: &str) -> UiaError {
    UiaError::MalformedArchive(message.to_owned())
}

fn check_host_directory_target(path: &Path, overwrite: bool) -> UiaResult<()> {
    let Ok(metadata) = fs::symlink_metadata(path) else {
        return Ok(());
    };
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(UiaError::DestinationExists);
    }
    if !overwrite
        && fs::read_dir(path)
            .map_err(|error| host_io(path, error))?
            .next()
            .is_some()
    {
        return Err(UiaError::DestinationExists);
    }
    Ok(())
}

struct StagingDirectory {
    target: PathBuf,
    path: PathBuf,
    overwrite: bool,
    persisted: bool,
}

impl StagingDirectory {
    fn create(target: &Path, overwrite: bool) -> UiaResult<Self> {
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent).map_err(|error| host_io(parent, error))?;
        for counter in 0..100 {
            let path = parent.join(unique_leaf_name(".winlane-workspace-", counter));
            match create_private_dir(&path) {
                Ok(()) => {
                    return Ok(Self {
                        target: target.to_path_buf(),
                        path,
                        overwrite,
                        persisted: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(host_io(&path, error)),
            }
        }
        Err(UiaError::HostCommand(
            "could not create workspace staging directory".to_owned(),
        ))
    }

    fn persist(mut self) -> UiaResult<()> {
        let target_metadata = fs::symlink_metadata(&self.target).ok();
        if let Some(metadata) = &target_metadata {
            if metadata.file_type().is_symlink() || !metadata.is_dir() {
                return Err(UiaError::DestinationExists);
            }
            if !self.overwrite
                && fs::read_dir(&self.target)
                    .map_err(|error| host_io(&self.target, error))?
                    .next()
                    .is_some()
            {
                return Err(UiaError::DestinationExists);
            }
        }
        if target_metadata.is_some() {
            let parent = self.target.parent().unwrap_or_else(|| Path::new("."));
            let backup = parent.join(unique_leaf_name(".winlane-backup-", 0));
            fs::rename(&self.target, &backup).map_err(|error| host_io(&self.target, error))?;
            match fs::rename(&self.path, &self.target) {
                Ok(()) => {
                    let _ = fs::remove_dir_all(&backup);
                    self.persisted = true;
                    Ok(())
                }
                Err(error) => {
                    let _ = fs::rename(&backup, &self.target);
                    Err(host_io(&self.target, error))
                }
            }
        } else {
            match fs::rename(&self.path, &self.target) {
                Ok(()) => {
                    self.persisted = true;
                    Ok(())
                }
                Err(_) if !self.overwrite && self.target.exists() => {
                    Err(UiaError::DestinationExists)
                }
                Err(error) => Err(host_io(&self.target, error)),
            }
        }
    }
}

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        if !self.persisted {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn hex_digest(hasher: Sha256) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = hasher.finalize();
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

    struct TestTempDir {
        path: PathBuf,
    }

    impl TestTempDir {
        fn new() -> Self {
            for counter in 0..100 {
                let path = std::env::temp_dir().join(unique_leaf_name("winlane-test-", counter));
                match fs::create_dir(&path) {
                    Ok(()) => return Self { path },
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("create test tempdir: {error}"),
                }
            }
            panic!("could not create test tempdir");
        }
    }

    impl Drop for TestTempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    fn extract_bytes(bytes: &[u8], target: &Path) -> UiaResult<ArchiveStats> {
        extract_tar(
            std::io::Cursor::new(bytes),
            target,
            ArchiveLimits::default(),
        )
    }

    fn make_archive_entry(path: &str, typeflag: u8, data: &[u8]) -> Vec<u8> {
        let mut output = Vec::new();
        let mut header = [0_u8; TAR_BLOCK_BYTES];
        write_bytes(&mut header[0..100], path.as_bytes());
        write_octal(&mut header[100..108], 0o644);
        write_octal(&mut header[124..136], data.len() as u64);
        for byte in &mut header[148..156] {
            *byte = b' ';
        }
        header[156] = typeflag;
        write_bytes(&mut header[257..263], b"ustar\0");
        write_bytes(&mut header[263..265], b"00");
        let checksum = header.iter().map(|byte| u64::from(*byte)).sum::<u64>();
        write_bytes(
            &mut header[148..156],
            format!("{checksum:06o}\0 ").as_bytes(),
        );
        output.extend_from_slice(&header);
        output.extend_from_slice(data);
        let padding = (TAR_BLOCK_BYTES - (data.len() % TAR_BLOCK_BYTES)) % TAR_BLOCK_BYTES;
        output.extend_from_slice(&vec![0_u8; padding]);
        output
    }

    fn finish_archive(mut bytes: Vec<u8>) -> Vec<u8> {
        bytes.extend_from_slice(&[0_u8; TAR_BLOCK_BYTES * 2]);
        bytes
    }

    struct RecordingTransport {
        incoming: std::collections::VecDeque<Vec<u8>>,
        written: std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>>,
    }

    impl QgaTransport for RecordingTransport {
        fn write_raw(&mut self, _bytes: &[u8]) -> std::io::Result<()> {
            Ok(())
        }

        fn write_message(&mut self, bytes: &[u8]) -> std::io::Result<()> {
            let request: serde_json::Value = serde_json::from_slice(bytes)
                .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
            self.written.borrow_mut().push(request.clone());
            let execute = request["execute"].as_str().unwrap_or_default();
            let result = match execute {
                "guest-file-open" => serde_json::json!(1),
                "guest-file-write" | "guest-file-flush" | "guest-file-close" => {
                    serde_json::json!({})
                }
                _ => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        format!("unexpected request {execute}"),
                    ));
                }
            };
            self.incoming.push_back(
                serde_json::json!({ "return": result })
                    .to_string()
                    .into_bytes(),
            );
            Ok(())
        }

        fn read_message(&mut self) -> std::io::Result<Vec<u8>> {
            self.incoming.pop_front().ok_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::UnexpectedEof, "missing response")
            })
        }
    }

    fn recording_control() -> (
        BlockingUiaControl<RecordingTransport>,
        std::rc::Rc<std::cell::RefCell<Vec<serde_json::Value>>>,
    ) {
        let written = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let transport = RecordingTransport {
            incoming: std::collections::VecDeque::new(),
            written: std::rc::Rc::clone(&written),
        };
        (
            BlockingUiaControl::new(crate::qga::QgaClient::new(transport)),
            written,
        )
    }

    #[test]
    fn export_staging_failure_does_not_open_guest_file() {
        let temp = TestTempDir::new();
        let parent_file = temp.path.join("parent-file");
        fs::write(&parent_file, b"not a directory").unwrap();
        let target = parent_file.join("payload.bin");
        let (mut control, written) = recording_control();
        let result = read_guest_file_to_host(
            &mut control,
            r"C:\winlane-validation\payload.bin",
            &target,
            false,
            1024,
            None,
        );
        assert!(result.is_err());
        assert!(written.borrow().is_empty());
    }

    #[test]
    fn growing_host_file_is_rejected_before_writing_beyond_cap() {
        let (mut control, written) = recording_control();
        let result = write_host_file_to_guest(
            &mut control,
            Path::new("/proc/version"),
            r"C:\winlane-validation\payload.bin",
            4,
        );
        assert!(matches!(result, Err(UiaError::TransferTooLarge { .. })));
        let requests = written.borrow();
        assert!(
            !requests
                .iter()
                .any(|request| request["execute"] == "guest-file-write")
        );
        assert!(
            requests
                .iter()
                .any(|request| request["execute"] == "guest-file-close")
        );
    }

    #[test]
    fn workspace_archive_round_trip_preserves_files_and_directories() {
        let temp = TestTempDir::new();
        let source = temp.path.join("source");
        fs::create_dir_all(source.join("alpha/nested")).unwrap();
        fs::create_dir_all(source.join("empty-dir")).unwrap();
        fs::write(source.join("alpha/nested/data.bin"), b"data").unwrap();
        fs::write(source.join("empty.txt"), b"").unwrap();
        let (archive, stats) = create_workspace_archive(&source).unwrap();
        assert_eq!(stats.members, 5);
        assert_eq!(stats.file_bytes, 4);
        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let extracted =
            extract_workspace_archive(&archive.path, &target, ArchiveLimits::default()).unwrap();
        assert_eq!(extracted.members, stats.members);
        assert_eq!(
            fs::read(target.join("alpha/nested/data.bin")).unwrap(),
            b"data"
        );
        assert_eq!(fs::read(target.join("empty.txt")).unwrap(), b"");
        assert!(target.join("empty-dir").is_dir());
    }

    #[test]
    fn workspace_archive_unicode_names_use_pax_and_round_trip() {
        let temp = TestTempDir::new();
        let source = temp.path.join("source");
        let unicode_dir = source.join("unicode-é");
        fs::create_dir_all(&unicode_dir).unwrap();
        fs::write(unicode_dir.join("日程🙂.txt"), "unicode content".as_bytes()).unwrap();
        let (archive, _) = create_workspace_archive(&source).unwrap();
        let bytes = fs::read(&archive.path).unwrap();
        assert_eq!(bytes[156], b'x');
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("path=unicode-é/"));
        assert!(text.contains("path=unicode-é/日程🙂.txt"));

        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        extract_workspace_archive(&archive.path, &target, ArchiveLimits::default()).unwrap();
        assert_eq!(
            fs::read(target.join("unicode-é/日程🙂.txt")).unwrap(),
            b"unicode content"
        );
    }

    #[test]
    fn workspace_archive_unrepresentable_long_name_uses_pax_and_round_trips() {
        let temp = TestTempDir::new();
        let source = temp.path.join("source");
        fs::create_dir(&source).unwrap();
        let name = "a".repeat(120);
        fs::write(source.join(&name), b"long").unwrap();
        let (archive, _) = create_workspace_archive(&source).unwrap();
        let bytes = fs::read(&archive.path).unwrap();
        assert_eq!(bytes[156], b'x');
        assert!(String::from_utf8_lossy(&bytes).contains("winlane-path-"));

        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        extract_workspace_archive(&archive.path, &target, ArchiveLimits::default()).unwrap();
        assert_eq!(fs::read(target.join(name)).unwrap(), b"long");
    }

    #[test]
    fn workspace_archive_ascii_representable_name_stays_plain_ustar() {
        let temp = TestTempDir::new();
        let source = temp.path.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("ascii.txt"), b"ascii").unwrap();
        let (archive, _) = create_workspace_archive(&source).unwrap();
        let bytes = fs::read(&archive.path).unwrap();
        assert_eq!(bytes[156], b'0');
        assert!(
            !bytes
                .windows(b"PaxHeader".len())
                .any(|window| window == b"PaxHeader")
        );
    }

    #[test]
    fn archive_extraction_uses_utf8_pax_path() {
        let temp = TestTempDir::new();
        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let pax = pax_record("path", "unicode-é/雪.txt");
        let mut archive = make_archive_entry("PaxHeader/test", b'x', &pax);
        archive.extend(make_archive_entry("winlane-path-fallback", b'0', b"data"));
        let archive = finish_archive(archive);
        extract_bytes(&archive, &target).unwrap();
        assert_eq!(fs::read(target.join("unicode-é/雪.txt")).unwrap(), b"data");
    }

    #[cfg(unix)]
    #[test]
    fn workspace_archive_rejects_symlink_source_entries() {
        let temp = TestTempDir::new();
        let source = temp.path.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("target.txt"), b"data").unwrap();
        std::os::unix::fs::symlink(source.join("target.txt"), source.join("link.txt")).unwrap();
        let result = create_workspace_archive(&source);
        assert!(matches!(result, Err(UiaError::UnsafeArchiveEntry(_))));
    }

    #[test]
    fn archive_extraction_rejects_path_traversal() {
        let temp = TestTempDir::new();
        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let archive = finish_archive(make_archive_entry("../evil.txt", b'0', b"bad"));
        let result = extract_bytes(&archive, &target);
        assert!(matches!(result, Err(UiaError::UnsafeArchiveEntry(_))));
        assert!(!temp.path.join("evil.txt").exists());
    }

    #[test]
    fn archive_extraction_rejects_links_and_unknown_types() {
        let temp = TestTempDir::new();
        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let archive = finish_archive(make_archive_entry("link", b'2', b"target"));
        let result = extract_bytes(&archive, &target);
        assert!(matches!(result, Err(UiaError::UnsafeArchiveEntry(_))));
    }

    #[test]
    fn archive_extraction_rejects_truncated_input_without_creating_target_file() {
        let temp = TestTempDir::new();
        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let mut archive = make_archive_entry("file.txt", b'0', b"abcdef");
        archive.truncate(archive.len() - 2);
        let result = extract_bytes(&archive, &target);
        assert!(matches!(result, Err(UiaError::MalformedArchive(_))));
        assert!(!target.join("file.txt").exists());
    }

    #[test]
    fn archive_extraction_enforces_member_and_total_caps() {
        let temp = TestTempDir::new();
        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let archive = finish_archive(make_archive_entry("file.txt", b'0', b"abcdef"));
        let limited = ArchiveLimits {
            max_member_bytes: 2,
            ..ArchiveLimits::default()
        };
        let result = extract_tar(std::io::Cursor::new(&archive), &target, limited);
        assert!(matches!(result, Err(UiaError::MemberTooLarge { .. })));
        let limited = ArchiveLimits {
            max_total_bytes: 2,
            ..ArchiveLimits::default()
        };
        let result = extract_tar(std::io::Cursor::new(&archive), &target, limited);
        assert!(matches!(result, Err(UiaError::TransferTooLarge { .. })));
    }

    #[test]
    fn archive_extraction_enforces_member_count_cap() {
        let temp = TestTempDir::new();
        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let archive = finish_archive(make_archive_entry("file.txt", b'0', b"a"));
        let limited = ArchiveLimits {
            max_members: 0,
            ..ArchiveLimits::default()
        };
        let result = extract_tar(std::io::Cursor::new(&archive), &target, limited);
        assert!(matches!(result, Err(UiaError::TooManyMembers { .. })));
    }

    #[test]
    fn archive_extraction_rejects_duplicate_members() {
        let temp = TestTempDir::new();
        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let mut archive = make_archive_entry("file.txt", b'0', b"a");
        archive.extend(make_archive_entry("file.txt", b'0', b"b"));
        let archive = finish_archive(archive);
        let result = extract_bytes(&archive, &target);
        assert!(matches!(result, Err(UiaError::UnsafeArchiveEntry(_))));
    }

    #[test]
    fn archive_extraction_rejects_bad_checksum() {
        let temp = TestTempDir::new();
        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let mut archive = finish_archive(make_archive_entry("file.txt", b'0', b"a"));
        archive[0] = b'z';
        let result = extract_bytes(&archive, &target);
        assert!(matches!(result, Err(UiaError::MalformedArchive(_))));
    }

    #[test]
    fn staging_directory_no_overwrite_preserves_nonempty_target() {
        let temp = TestTempDir::new();
        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("existing.txt"), b"old").unwrap();
        let staging = StagingDirectory::create(&target, false).unwrap();
        fs::write(staging.path.join("new.txt"), b"new").unwrap();
        let result = staging.persist();
        assert!(matches!(result, Err(UiaError::DestinationExists)));
        assert_eq!(fs::read(target.join("existing.txt")).unwrap(), b"old");
        assert!(!target.join("new.txt").exists());
    }

    #[test]
    fn staging_directory_overwrite_replaces_target_tree() {
        let temp = TestTempDir::new();
        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        fs::write(target.join("old.txt"), b"old").unwrap();
        let staging = StagingDirectory::create(&target, true).unwrap();
        fs::write(staging.path.join("new.txt"), b"new").unwrap();
        staging.persist().unwrap();
        assert_eq!(fs::read(target.join("new.txt")).unwrap(), b"new");
        assert!(!target.join("old.txt").exists());
    }

    #[test]
    fn workspace_archive_rejects_case_insensitive_duplicate_paths() {
        let temp = TestTempDir::new();
        let source = temp.path.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("Foo"), b"upper").unwrap();
        fs::write(source.join("foo"), b"lower").unwrap();
        let result = create_workspace_archive(&source);
        assert!(matches!(result, Err(UiaError::UnsafeArchiveEntry(_))));
    }

    #[test]
    fn archive_extraction_rejects_case_insensitive_duplicate_paths() {
        let temp = TestTempDir::new();
        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let mut archive = make_archive_entry("dir/Foo", b'0', b"a");
        archive.extend(make_archive_entry("dir/foo", b'0', b"b"));
        let archive = finish_archive(archive);
        let result = extract_bytes(&archive, &target);
        assert!(matches!(result, Err(UiaError::UnsafeArchiveEntry(_))));
    }

    #[cfg(unix)]
    #[test]
    fn private_archive_and_workspace_staging_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let archive = private_archive().unwrap();
        assert_eq!(
            fs::metadata(&archive.directory)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        let temp = TestTempDir::new();
        let target = temp.path.join("target");
        let staging = StagingDirectory::create(&target, true).unwrap();
        assert_eq!(
            fs::metadata(&staging.path).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn powershell_literal_path_is_used_for_directory_creation() {
        let source = include_str!("transfer.rs");
        assert!(source.contains("New-Item -ItemType Directory -Force -LiteralPath $parent"));
        assert!(source.contains("New-Item -ItemType Directory -Force -LiteralPath $staging"));
    }

    #[test]
    fn guest_transfer_paths_reject_device_traversal_unc_workspace_and_roots() {
        assert!(validate_guest_transfer_path(r"C:\Temp\file.txt", false).is_ok());
        assert!(validate_guest_transfer_path(r"\\host\share\file.txt", false).is_ok());
        for path in [
            r"\\?\C:\Temp\file.txt",
            r"C:\workspace\..\evil.txt",
            r"C:\workspace\.\evil.txt",
        ] {
            assert!(validate_guest_transfer_path(path, false).is_err());
        }
        assert!(validate_guest_transfer_path(r"\\host\share\workspace", true).is_err());
        assert!(validate_guest_transfer_path(r"C:\", true).is_err());
    }
}
