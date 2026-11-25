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
const MAX_TRANSPORT_MANIFEST_BYTES: u64 = 16 * 1024 * 1024;

const TAR_GUEST_PATH: &str = r"C:\Windows\System32\tar.exe";
const TRANSFER_CHUNK_BYTES: usize = 1024 * 1024;
const TAR_BLOCK_BYTES: usize = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ArchiveLimits {
    pub max_members: u64,
    pub max_member_bytes: u64,
    pub max_total_bytes: u64,
    pub max_path_bytes: usize,
}

fn transport_archive_limits() -> ArchiveLimits {
    ArchiveLimits {
        max_members: MAX_WORKSPACE_MEMBERS + 2,
        max_member_bytes: MAX_WORKSPACE_MEMBER_BYTES.max(MAX_TRANSPORT_MANIFEST_BYTES),
        max_total_bytes: MAX_WORKSPACE_TOTAL_BYTES + MAX_TRANSPORT_MANIFEST_BYTES,
        max_path_bytes: MAX_WORKSPACE_PATH_BYTES,
    }
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
    pub transport_sha256: String,
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

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TransportManifest {
    format: String,
    version: u32,
    entries: Vec<TransportEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct TransportEntry {
    path: String,
    #[serde(rename = "type")]
    entry_type: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    blob: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    sha256: Option<String>,
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
    let (archive, stats, logical_sha256) = create_workspace_archive(host_dir)?;
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
        let transport_dir = format!(r"{staging_dir}\transport");
        let tree_dir = format!(r"{staging_dir}\tree");
        run_guest_tar(control, &archive_guest_path, &transport_dir, true)?;
        run_guest_materialize(control, &transport_dir, &tree_dir)?;
        if let Err(error) =
            finalize_guest_workspace(control, &tree_dir, guest_dir, &backup_dir, overwrite)
        {
            if matches!(error, UiaError::DestinationExists) {
                return Err(error);
            }
            return Err(UiaError::WorkspaceFinalizeFailed {
                backup_path: backup_dir.clone(),
                message: error.to_string(),
            });
        }
        Ok::<(u64, String, String), UiaError>((bytes, sha256, logical_sha256.clone()))
    })();
    cleanup_guest_path(control, &archive_guest_path, false);
    cleanup_guest_path(control, &staging_dir, true);
    if result.is_ok() {
        cleanup_guest_path(control, &backup_dir, true);
    }
    let (bytes, transport_sha256, sha256) = result?;
    Ok(WorkspaceTransferRef {
        sha256,
        transport_sha256,
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
    let transport_guest_path = guest_unique_path("workspace-export-transport");
    let result = (|| {
        run_guest_export_transport(control, guest_dir, &transport_guest_path)?;
        run_guest_tar(control, &archive_guest_path, &transport_guest_path, false)?;
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
        let unpack_owner = private_archive()?;
        let unpack_dir = unpack_owner.directory.join("unpack");
        fs::create_dir(&unpack_dir).map_err(|error| host_io(&unpack_dir, error))?;
        extract_workspace_archive(&archive_path, &unpack_dir, transport_archive_limits())?;
        let (probe_manifest, _) = read_transport_manifest(&unpack_dir)?;
        validate_transport_layout(&unpack_dir, &probe_manifest)?;
        let (manifest, stats_file_bytes) = materialize_transport(&unpack_dir, &staging.path)?;
        staging.persist()?;
        let logical_sha256 = canonical_manifest_hash(&manifest);
        let stats = ArchiveStats {
            members: manifest.entries.len() as u64,
            file_bytes: stats_file_bytes,
        };
        Ok::<(u64, String, ArchiveStats, String), UiaError>((bytes, sha256, stats, logical_sha256))
    })();
    cleanup_guest_path(control, &archive_guest_path, false);
    cleanup_guest_path(control, &transport_guest_path, true);
    let (bytes, transport_sha256, stats, sha256) = result?;
    Ok(WorkspaceTransferRef {
        sha256,
        transport_sha256,
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
        "$parent=Split-Path -Parent {target};if($parent -and -not(Test-Path -LiteralPath $parent)){{New-Item -ItemType Directory -Force -Path $parent|Out-Null}};exit 0"
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
        "$s={staging};$d={target};$backup=$null;{overwrite_check}try{{if(Test-Path -LiteralPath $d){{$backup=$d+'.winlane-backup-'+[System.IO.Path]::GetRandomFileName();[System.IO.File]::Replace($s,$d,$backup);Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue}}else{{[System.IO.File]::Move($s,$d)}};exit 0}}catch{{if(($null -ne $backup) -and (Test-Path -LiteralPath $backup)){{Remove-Item -LiteralPath $backup -Force -ErrorAction SilentlyContinue}};Write-Error $_.Exception.Message;exit 1}}"
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
        "$staging={staging};$target={target};if(Test-Path -LiteralPath $target){{$item=Get-Item -LiteralPath $target -Force;if(($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0){{exit 5}};if(-not $item.PSIsContainer){{exit 4}};{overwrite_check}}};if(Test-Path -LiteralPath $staging){{$item=Get-Item -LiteralPath $staging -Force;if(($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0){{exit 5}}}};New-Item -ItemType Directory -Force -Path $staging|Out-Null"
    );
    let capture = run_powershell(control, &script, 30_000)?;
    match capture.exit_code {
        Some(0) => Ok(()),
        Some(3) => Err(UiaError::DestinationExists),
        Some(4) => Err(UiaError::DestinationExists),
        Some(5) => Err(UiaError::InvalidGuestPath),
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
        "$staging={staging};$target={target};$backup={backup};$parent=Split-Path -Parent $target;if($parent -and -not(Test-Path -LiteralPath $parent)){{New-Item -ItemType Directory -Force -Path $parent|Out-Null}};$moved=$false;function Has-Reparse($dir){{foreach($child in [System.IO.Directory]::EnumerateFileSystemEntries($dir)){{$attrs=[System.IO.File]::GetAttributes($child);if(($attrs -band [System.IO.FileAttributes]::ReparsePoint) -ne 0){{return $true}};if(($attrs -band [System.IO.FileAttributes]::Directory) -ne 0){{if(Has-Reparse $child){{return $true}}}}}};return $false}};if(Test-Path -LiteralPath $backup){{exit 5}};if(Test-Path -LiteralPath $staging){{$item=Get-Item -LiteralPath $staging -Force;if(($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0){{exit 5}}}};if(Test-Path -LiteralPath $target){{$item=Get-Item -LiteralPath $target -Force;if(($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0){{exit 5}};if(-not $item.PSIsContainer){{exit 4}};{overwrite_check}if(Has-Reparse $target){{exit 5}};[System.IO.Directory]::Move($target,$backup);$moved=$true}};try{{[System.IO.Directory]::Move($staging,$target)}}catch{{if(($moved) -and (Test-Path -LiteralPath $backup) -and -not (Test-Path -LiteralPath $target)){{try{{[System.IO.Directory]::Move($backup,$target)}}catch{{}}}};Write-Error $_.Exception.Message;exit 1}};if($moved){{$item=Get-Item -LiteralPath $backup -Force;if(($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -ne 0){{exit 5}};Remove-Item -LiteralPath $backup -Recurse -Force -ErrorAction Stop;if(Test-Path -LiteralPath $backup){{Write-Error 'backup cleanup incomplete';exit 1}}}};exit 0"
    );
    let capture = run_powershell(control, &script, 60_000)?;
    match capture.exit_code {
        Some(0) => Ok(()),
        Some(3) | Some(4) => Err(UiaError::DestinationExists),
        Some(5) => Err(UiaError::InvalidGuestPath),
        exit_code => Err(UiaError::Helper {
            exit_code,
            message: process_output_summary(&capture),
        }),
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

fn run_guest_export_transport<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    source: &str,
    transport: &str,
) -> UiaResult<()> {
    let source = ps_quote(source);
    let transport = ps_quote(transport);
    let script = format!(
        "$source={source};$transport={transport};function Fail($m){{Write-Error $m;exit 1}};$script:entries=New-Object System.Collections.ArrayList;[System.IO.Directory]::CreateDirectory((Join-Path $transport 'blobs'))|Out-Null;function Walk($dir,$prefix){{foreach($item in [System.IO.Directory]::EnumerateFileSystemEntries($dir)){{$name=[System.IO.Path]::GetFileName($item);$rel=$name;if($prefix.Length -gt 0){{$rel=$prefix+'/'+$name}};if([System.Text.Encoding]::UTF8.GetByteCount($rel) -gt 255){{Fail 'path too long'}};if($script:entries.Count -ge 10000){{Fail 'too many entries'}};$attrs=[System.IO.File]::GetAttributes($item);if(($attrs -band [System.IO.FileAttributes]::ReparsePoint) -ne 0){{Fail 'reparse point'}};if(($attrs -band [System.IO.FileAttributes]::Directory) -ne 0){{[void]$script:entries.Add([pscustomobject]@{{path=$rel;type='directory'}});Walk $item $rel}}elseif(($attrs -band [System.IO.FileAttributes]::Device) -eq 0){{[void]$script:entries.Add([pscustomobject]@{{path=$rel;type='file';blob=0;size=[int64]0;sha256=''}})}}else{{Fail 'unsupported member'}}}}}};if(-not (Test-Path -LiteralPath $source -PathType Container)){{Fail 'source missing'}};Walk $source '';$cases=@{{}};foreach($e in $script:entries){{$lower=([string]$e.path).ToLowerInvariant();if($cases.ContainsKey($lower)){{Fail 'case duplicate'}};$cases[$lower]=1}};$script:entries=@($script:entries|Sort-Object {{[string]$_.path}});$blob=0;$totalBytes=[int64]0;foreach($e in @($script:entries|Where-Object{{$_.type -eq 'file'}})){{if($blob -ge 10000){{Fail 'too many files'}};$src=Join-Path $source (([string]$e.path).Replace('/','\'));$info=Get-Item -LiteralPath $src;if($info.Length -gt 268435456){{Fail 'file too large'}};$totalBytes += $info.Length;if($totalBytes -gt 536870912){{Fail 'workspace too large'}};$dest=Join-Path $transport ([System.String]::Format('blobs\\{{0:D6}}',$blob));[System.IO.File]::Copy($src,$dest,$false);$hash=(Get-FileHash -Algorithm SHA256 -LiteralPath $dest).Hash.ToLowerInvariant();$e.blob=$blob;$e.size=(Get-Item -LiteralPath $dest).Length;$e.sha256=$hash;$blob++}};$manifest=[pscustomobject]@{{format='winlane-workspace-transport';version=1;entries=$script:entries}};$json=$manifest|ConvertTo-Json -Depth 5 -Compress;[System.IO.File]::WriteAllText((Join-Path $transport 'manifest.json'),$json,[System.Text.UTF8Encoding]::new($false));exit 0"
    );
    let capture = run_powershell(control, &script, 600_000)?;
    if capture.exit_code == Some(0) {
        Ok(())
    } else {
        Err(UiaError::Helper {
            exit_code: capture.exit_code,
            message: process_output_summary(&capture),
        })
    }
}

fn run_guest_materialize<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    transport: &str,
    tree: &str,
) -> UiaResult<()> {
    let transport = ps_quote(transport);
    let tree = ps_quote(tree);
    let script = format!(
        "$transport={transport};$tree={tree};function Fail($m){{Write-Error $m;exit 1}};function Test-Part($x){{if($x.Length -eq 0 -or $x -eq '.' -or $x -eq '..'){{return $false}};if($x.EndsWith('.') -or $x.EndsWith(' ')){{return $false}};if($x -match '[\\*?\"<>|:]'){{return $false}};$stem=($x.Split('.')[0]).ToUpperInvariant();if(@('CON','PRN','AUX','NUL','COM1','COM2','COM3','COM4','COM5','COM6','COM7','COM8','COM9','LPT1','LPT2','LPT3','LPT4','LPT5','LPT6','LPT7','LPT8','LPT9') -contains $stem){{return $false}};return $true}};$json=[System.IO.File]::ReadAllText((Join-Path $transport 'manifest.json'),[System.Text.UTF8Encoding]::new($false));$m=$json|ConvertFrom-Json;if($m.format -ne 'winlane-workspace-transport' -or $m.version -ne 1){{Fail 'bad manifest format'}};$entries=@($m.entries);if($entries.Count -gt 10000){{Fail 'too many entries'}};$types=@{{}};$cases=@{{}};$blobs=@{{}};$fileCount=0;$totalBytes=[int64]0;foreach($e in $entries){{$p=[string]$e.path;if($p.Length -eq 0 -or [System.Text.Encoding]::UTF8.GetByteCount($p) -gt 255 -or $p.Contains('\\') -or $p.StartsWith('/') -or $p.StartsWith('\\')){{Fail 'bad path'}};foreach($ch in $p.ToCharArray()){{if([System.Char]::IsControl($ch)){{Fail 'control path'}}}};$parts=$p.Split('/');foreach($part in $parts){{if(-not (Test-Part $part)){{Fail 'bad component'}}}};if($types.ContainsKey($p)){{Fail 'duplicate path'}};$types[$p]=[string]$e.type;$lower=$p.ToLowerInvariant();if($cases.ContainsKey($lower)){{Fail 'case duplicate'}};$cases[$lower]=1;if($e.type -eq 'file'){{if($null -eq $e.PSObject.Properties['blob'] -or $null -eq $e.blob -or $null -eq $e.PSObject.Properties['size'] -or $null -eq $e.size -or $null -eq $e.PSObject.Properties['sha256'] -or $null -eq $e.sha256){{Fail 'missing file metadata'}};if([int64]$e.size -gt 268435456){{Fail 'file too large'}};if(([string]$e.sha256) -notmatch '^[0-9a-f]{{64}}$'){{Fail 'bad hash'}};$totalBytes += [int64]$e.size;if($totalBytes -gt 536870912){{Fail 'workspace too large'}};$b=[int]$e.blob;if($b -lt 0 -or $b -ge 10000 -or $blobs.ContainsKey($b)){{Fail 'bad blob'}};$blobs[$b]=1;$fileCount++}}elseif($e.type -ne 'directory'){{Fail 'bad type'}}}};if($blobs.Count -ne $fileCount){{Fail 'blob count'}};for($i=0;$i -lt $fileCount;$i++){{if(-not $blobs.ContainsKey($i)){{Fail 'blob indices'}}}};foreach($e in $entries){{$parts=([string]$e.path).Split('/');for($i=1;$i -lt $parts.Count;$i++){{$parent=($parts[0..($i-1)] -join '/');if(-not $types.ContainsKey($parent) -or $types[$parent] -ne 'directory'){{Fail 'bad parent'}}}}}};[System.IO.Directory]::CreateDirectory($tree)|Out-Null;$dirs=@($entries|Where-Object{{$_.type -eq 'directory'}}|Sort-Object{{([string]$_.path).Split('/').Count}},{{[string]$_.path}});foreach($e in $dirs){{$dest=Join-Path $tree ([string]$e.path);[System.IO.Directory]::CreateDirectory($dest)|Out-Null}};$files=@($entries|Where-Object{{$_.type -eq 'file'}}|Sort-Object{{[string]$_.path}});foreach($e in $files){{$rel=[string]$e.path;$b=[int]$e.blob;$src=Join-Path $transport ([System.String]::Format('blobs\\{{0:D6}}',$b));$dest=Join-Path $tree $rel;$parent=Split-Path -Parent $dest;if($parent){{[System.IO.Directory]::CreateDirectory($parent)|Out-Null}};if(Test-Path -LiteralPath $dest){{Fail 'destination conflict'}};[System.IO.File]::Move($src,$dest);$info=Get-Item -LiteralPath $dest;if($info.Length -ne [int64]$e.size){{Fail 'size mismatch'}};$hash=(Get-FileHash -Algorithm SHA256 -LiteralPath $dest).Hash.ToLowerInvariant();if($hash -ne ([string]$e.sha256).ToLowerInvariant()){{Fail 'hash mismatch'}}}};exit 0"
    );
    let capture = run_powershell(control, &script, 600_000)?;
    if capture.exit_code == Some(0) {
        Ok(())
    } else {
        Err(UiaError::Helper {
            exit_code: capture.exit_code,
            message: process_output_summary(&capture),
        })
    }
}

fn run_guest_tar<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    archive_path: &str,
    target_dir: &str,
    extract: bool,
) -> UiaResult<()> {
    let script = guest_tar_script(archive_path, target_dir, extract)?;
    let capture = run_powershell(control, &script, 600_000)?;
    if capture.exit_code == Some(0) {
        Ok(())
    } else {
        Err(UiaError::Helper {
            exit_code: capture.exit_code,
            message: process_output_summary(&capture),
        })
    }
}

fn guest_tar_script(archive_path: &str, target_dir: &str, extract: bool) -> UiaResult<String> {
    let trimmed = archive_path.trim_end_matches(['\\', '/']);
    let Some(index) = trimmed.rfind(['\\', '/']) else {
        return Err(UiaError::InvalidGuestPath);
    };
    let archive_dir = ps_quote(&trimmed[..index]);
    let archive_name = ps_quote(&trimmed[index + 1..]);
    let target_dir = ps_quote(target_dir);
    let tar = ps_quote(TAR_GUEST_PATH);
    let operation = if extract { "-x" } else { "-c" };
    let format = if extract { "" } else { "--format ustar " };
    let member = if extract { "" } else { " '.'" };
    Ok(format!(
        "$archiveDir={archive_dir};$archiveName={archive_name};$targetDir={target_dir};$code=1;New-Item -ItemType Directory -Force -Path $targetDir|Out-Null;Push-Location -LiteralPath $archiveDir;try{{& {tar} {format}{operation} -f $archiveName -C $targetDir{member};$code=$LASTEXITCODE}}finally{{Pop-Location}};exit $code"
    ))
}

fn cleanup_guest_path<T: QgaTransport>(
    control: &mut BlockingUiaControl<T>,
    path: &str,
    recurse: bool,
) {
    let path = ps_quote(path);
    let recurse = if recurse { " -Recurse" } else { "" };
    let script = format!(
        "if(Test-Path -LiteralPath {path}){{$item=Get-Item -LiteralPath {path} -Force;if(($item.Attributes -band [System.IO.FileAttributes]::ReparsePoint) -eq 0){{Remove-Item -LiteralPath {path} -Force{recurse} -ErrorAction SilentlyContinue}}}}"
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
    let windows_component_invalid =
        path.split(['\\', '/'])
            .enumerate()
            .any(|(index, component)| {
                let drive_component = index == 0
                    && component.len() == 2
                    && component.as_bytes()[0].is_ascii_alphabetic()
                    && component.as_bytes()[1] == b':';
                let stem = component
                    .split('.')
                    .next()
                    .unwrap_or(component)
                    .to_ascii_uppercase();
                component.ends_with(['.', ' '])
                    || component
                        .chars()
                        .any(|c| matches!(c, '*' | '?' | '"' | '<' | '>' | '|'))
                    || (component.contains(':') && !drive_component)
                    || matches!(
                        stem.as_str(),
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
                    )
            });
    if device
        || traversal
        || trailing_separator
        || windows_component_invalid
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
        &trimmed[..index],
        std::process::id()
    ))
}

#[derive(Debug)]
struct SourceEntry {
    relative: String,
    path: PathBuf,
    directory: bool,
    size: u64,
}

fn create_workspace_archive(source: &Path) -> UiaResult<(HostArchive, ArchiveStats, String)> {
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
    let mut manifest_entries = Vec::new();
    for (blob_index, entry) in entries.iter().filter(|entry| !entry.directory).enumerate() {
        let blob_index = blob_index as u64;
        let name = blob_name(blob_index);
        write_ustar_header(&mut writer, &name, false, entry.size, 0, b'0')?;
        let mut input = File::open(&entry.path).map_err(|error| host_io(&entry.path, error))?;
        let (bytes, sha256, grew) = copy_exact_with_hash(&mut input, &mut writer, entry.size)
            .map_err(|error| host_io(&archive.path, error))?;
        if bytes != entry.size || grew {
            return Err(UiaError::MalformedArchive(
                "source file changed while archiving".to_owned(),
            ));
        }
        write_tar_padding(&mut writer, entry.size)
            .map_err(|error| host_io(&archive.path, error))?;
        manifest_entries.push((entry.relative.clone(), blob_index, entry.size, sha256));
    }
    let mut manifest = TransportManifest {
        format: "winlane-workspace-transport".to_owned(),
        version: 1,
        entries: Vec::new(),
    };
    let mut files = std::collections::HashMap::new();
    for (path, blob, size, sha256) in manifest_entries {
        files.insert(path, (blob, size, sha256));
    }
    for entry in &entries {
        if entry.directory {
            manifest.entries.push(TransportEntry {
                path: entry.relative.clone(),
                entry_type: "directory".to_owned(),
                blob: None,
                size: None,
                sha256: None,
            });
        } else {
            let (blob, size, sha256) = files[&entry.relative].clone();
            manifest.entries.push(TransportEntry {
                path: entry.relative.clone(),
                entry_type: "file".to_owned(),
                blob: Some(blob),
                size: Some(size),
                sha256: Some(sha256),
            });
        }
    }
    validate_transport_manifest(&manifest, ArchiveLimits::default())?;
    let manifest_bytes = serde_json::to_vec(&manifest)?;
    write_ustar_header(
        &mut writer,
        "manifest.json",
        false,
        manifest_bytes.len() as u64,
        0,
        b'0',
    )?;
    writer
        .write_all(&manifest_bytes)
        .map_err(|error| host_io(&archive.path, error))?;
    write_tar_padding(&mut writer, manifest_bytes.len() as u64)
        .map_err(|error| host_io(&archive.path, error))?;
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
    let logical_sha256 = canonical_manifest_hash(&manifest);
    Ok((
        archive,
        ArchiveStats {
            members: entries.len() as u64,
            file_bytes,
        },
        logical_sha256,
    ))
}

fn valid_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_manifest_path(path: &str, max_path_bytes: usize) -> UiaResult<()> {
    if path.is_empty()
        || path.len() > max_path_bytes
        || path.starts_with(['/', '\\'])
        || path.contains('\\')
        || path.chars().any(char::is_control)
    {
        return Err(UiaError::UnsafeArchiveEntry(path.to_owned()));
    }
    let components = path.split('/').collect::<Vec<_>>();
    if components
        .iter()
        .any(|component| component.is_empty() || matches!(*component, "." | ".."))
    {
        return Err(UiaError::UnsafeArchiveEntry(path.to_owned()));
    }
    for component in components {
        validate_member_component(component)?;
    }
    Ok(())
}

fn validate_transport_manifest(
    manifest: &TransportManifest,
    limits: ArchiveLimits,
) -> UiaResult<u64> {
    if manifest.format != "winlane-workspace-transport" || manifest.version != 1 {
        return Err(malformed("unsupported workspace transport manifest"));
    }
    if manifest.entries.len() as u64 > limits.max_members {
        return Err(UiaError::TooManyMembers {
            maximum: limits.max_members,
        });
    }
    let mut exact = HashSet::new();
    let mut casefold = HashSet::new();
    let mut types = std::collections::HashMap::new();
    let mut blobs = Vec::new();
    let mut file_bytes = 0_u64;
    for entry in &manifest.entries {
        validate_manifest_path(&entry.path, limits.max_path_bytes)?;
        if !exact.insert(entry.path.clone()) || !casefold.insert(entry.path.to_lowercase()) {
            return Err(UiaError::UnsafeArchiveEntry(format!(
                "duplicate manifest path {}",
                entry.path
            )));
        }
        match entry.entry_type.as_str() {
            "directory" => {
                if entry.blob.is_some() || entry.size.is_some() || entry.sha256.is_some() {
                    return Err(malformed("directory entry carries file metadata"));
                }
            }
            "file" => {
                let blob = entry
                    .blob
                    .ok_or_else(|| malformed("file entry missing blob"))?;
                let size = entry
                    .size
                    .ok_or_else(|| malformed("file entry missing size"))?;
                let sha256 = entry
                    .sha256
                    .as_deref()
                    .ok_or_else(|| malformed("file entry missing sha256"))?;
                if size > limits.max_member_bytes {
                    return Err(UiaError::MemberTooLarge {
                        actual: size,
                        maximum: limits.max_member_bytes,
                    });
                }
                if !valid_sha256_hex(sha256) {
                    return Err(malformed("file entry sha256 is invalid"));
                }
                file_bytes = file_bytes
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
                blobs.push(blob);
            }
            _ => return Err(malformed("manifest entry type is unsupported")),
        }
        types.insert(entry.path.clone(), entry.entry_type.clone());
    }
    blobs.sort_unstable();
    if blobs
        .iter()
        .enumerate()
        .any(|(index, blob)| *blob != index as u64)
    {
        return Err(malformed(
            "manifest blob indices are not contiguous and unique",
        ));
    }
    for entry in &manifest.entries {
        let mut prefix = String::new();
        let components = entry.path.split('/').collect::<Vec<_>>();
        for component in &components[..components.len() - 1] {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(component);
            match types.get(&prefix).map(String::as_str) {
                Some("directory") => {}
                Some("file") => return Err(malformed("file entry is used as a parent path")),
                _ => return Err(malformed("manifest parent directory entry is missing")),
            }
        }
    }
    Ok(file_bytes)
}

fn blob_name(index: u64) -> String {
    format!("blobs/{index:06}")
}

fn copy_with_hash(
    reader: &mut impl Read,
    writer: &mut impl Write,
) -> std::io::Result<(u64, String)> {
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; 128 * 1024];
    loop {
        let count = reader.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        writer.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        total += count as u64;
    }
    writer.flush()?;
    Ok((total, hex_digest(hasher)))
}

fn copy_exact_with_hash(
    reader: &mut impl Read,
    writer: &mut impl Write,
    expected: u64,
) -> std::io::Result<(u64, String, bool)> {
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let mut buffer = vec![0_u8; 128 * 1024];
    while total < expected {
        let wanted = (expected - total).min(buffer.len() as u64) as usize;
        let count = reader.read(&mut buffer[..wanted])?;
        if count == 0 {
            break;
        }
        writer.write_all(&buffer[..count])?;
        hasher.update(&buffer[..count]);
        total += count as u64;
    }
    writer.flush()?;
    let mut extra = [0_u8; 1];
    let grew = total == expected && reader.read(&mut extra)? != 0;
    Ok((total, hex_digest(hasher), grew))
}

fn canonical_manifest_hash(manifest: &TransportManifest) -> String {
    let mut entries = manifest.entries.clone();
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    let mut hasher = Sha256::new();
    hasher.update(b"winlane-workspace-transport-v1\0");
    for entry in entries {
        let size;
        let fields = [
            entry.entry_type.as_str(),
            entry.path.as_str(),
            if entry.entry_type == "file" {
                size = entry.size.unwrap_or(0).to_string();
                size.as_str()
            } else {
                ""
            },
            entry.sha256.as_deref().unwrap_or(""),
        ];
        for field in fields {
            hasher.update((field.len() as u64).to_le_bytes());
            hasher.update(field.as_bytes());
        }
    }
    hex_digest(hasher)
}

fn read_transport_manifest(transport: &Path) -> UiaResult<(TransportManifest, u64)> {
    let path = transport.join("manifest.json");
    let bytes = fs::read(&path).map_err(|error| host_io(&path, error))?;
    if bytes.len() as u64 > MAX_TRANSPORT_MANIFEST_BYTES {
        return Err(UiaError::MalformedArchive(
            "workspace manifest exceeds size limit".to_owned(),
        ));
    }
    let manifest: TransportManifest = serde_json::from_slice(&bytes)?;
    let file_bytes = validate_transport_manifest(&manifest, ArchiveLimits::default())?;
    Ok((manifest, file_bytes))
}

fn materialize_transport(transport: &Path, target: &Path) -> UiaResult<(TransportManifest, u64)> {
    let (manifest, file_bytes) = read_transport_manifest(transport)?;
    let mut entries = manifest.entries.clone();
    entries.sort_by(|left, right| left.path.cmp(&right.path));
    for entry in entries
        .iter()
        .filter(|entry| entry.entry_type == "directory")
    {
        let path = target.join(entry.path.replace('/', std::path::MAIN_SEPARATOR_STR));
        fs::create_dir(&path).map_err(|error| host_io(&path, error))?;
    }
    for entry in entries.iter().filter(|entry| entry.entry_type == "file") {
        let relative = entry.path.replace('/', std::path::MAIN_SEPARATOR_STR);
        let output_path = target.join(&relative);
        let input_path = transport.join(blob_name(entry.blob.unwrap_or(0)));
        let mut input = File::open(&input_path).map_err(|error| host_io(&input_path, error))?;
        let mut output = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output_path)
            .map_err(|error| host_io(&output_path, error))?;
        let copy_result =
            copy_with_hash(&mut input, &mut output).map_err(|error| host_io(&output_path, error));
        let (bytes, sha256) = match copy_result {
            Ok(value) => value,
            Err(error) => {
                let _ = fs::remove_file(&output_path);
                return Err(error);
            }
        };
        if bytes != entry.size.unwrap_or(u64::MAX)
            || sha256 != entry.sha256.as_deref().unwrap_or("")
        {
            let _ = fs::remove_file(&output_path);
            return Err(UiaError::TransferVerificationFailed);
        }
    }
    Ok((manifest, file_bytes))
}

fn validate_transport_layout(transport: &Path, manifest: &TransportManifest) -> UiaResult<()> {
    let mut expected = HashSet::from(["manifest.json".to_owned()]);
    for entry in &manifest.entries {
        if entry.entry_type == "file" {
            expected.insert(blob_name(entry.blob.unwrap_or(0)));
        }
    }
    let mut actual_files = Vec::new();
    let mut actual_dirs = Vec::new();
    fn collect(
        directory: &Path,
        prefix: &str,
        files: &mut Vec<String>,
        dirs: &mut Vec<String>,
    ) -> UiaResult<()> {
        for child in fs::read_dir(directory).map_err(|error| host_io(directory, error))? {
            let child = child.map_err(|error| host_io(directory, error))?;
            let name = child
                .file_name()
                .to_str()
                .ok_or_else(|| malformed("transport member is not UTF-8"))?
                .to_owned();
            let relative = if prefix.is_empty() {
                name
            } else {
                format!("{prefix}/{name}")
            };
            let metadata = fs::symlink_metadata(child.path())
                .map_err(|error| host_io(&child.path(), error))?;
            if metadata.is_dir() {
                dirs.push(relative.clone());
                collect(&child.path(), &relative, files, dirs)?;
            } else if metadata.is_file() {
                files.push(relative);
            } else {
                return Err(UiaError::UnsafeArchiveEntry(relative));
            }
        }
        Ok(())
    }
    collect(transport, "", &mut actual_files, &mut actual_dirs)?;
    for directory in actual_dirs {
        if directory != "blobs" {
            return Err(UiaError::UnsafeArchiveEntry(format!(
                "unexpected transport directory {directory}"
            )));
        }
    }
    for path in actual_files {
        if !expected.remove(&path) {
            return Err(UiaError::UnsafeArchiveEntry(format!(
                "unexpected transport member {path}"
            )));
        }
    }
    if !expected.is_empty() {
        return Err(malformed("transport members do not match manifest blobs"));
    }
    Ok(())
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
            b'0' | 0 | b'5' => {
                let path = header.path;
                let size = header.size;
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

fn read_exact_archive(reader: &mut impl Read, buffer: &mut [u8]) -> UiaResult<()> {
    reader
        .read_exact(buffer)
        .map_err(|_| malformed("archive is truncated"))
}

fn copy_exact_archive(
    reader: &mut impl Read,
    writer: &mut impl Write,
    size: u64,
    path: &Path,
) -> UiaResult<()> {
    let mut remaining = size;
    let mut buffer = vec![0_u8; 128 * 1024];
    while remaining > 0 {
        let wanted = remaining.min(buffer.len() as u64) as usize;
        let count = reader
            .read(&mut buffer[..wanted])
            .map_err(|error| host_io(path, error))?;
        if count == 0 {
            return Err(malformed("archive is truncated"));
        }
        writer
            .write_all(&buffer[..count])
            .map_err(|error| host_io(path, error))?;
        remaining -= count as u64;
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

    fn tar_header_names(bytes: &[u8]) -> Vec<(String, u8)> {
        let mut output = Vec::new();
        let mut offset = 0_usize;
        loop {
            let end = offset + TAR_BLOCK_BYTES;
            assert!(end <= bytes.len());
            let block: &[u8; TAR_BLOCK_BYTES] = bytes[offset..end].try_into().unwrap();
            if block.iter().all(|byte| *byte == 0) {
                break;
            }
            let header = parse_tar_header(block).unwrap();
            output.push((header.path.clone(), header.typeflag));
            offset = end + (header.size as usize).div_ceil(TAR_BLOCK_BYTES) * TAR_BLOCK_BYTES;
        }
        output
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

    fn transport_file(path: &str, blob: u64, size: u64, sha256: &str) -> TransportEntry {
        TransportEntry {
            path: path.to_owned(),
            entry_type: "file".to_owned(),
            blob: Some(blob),
            size: Some(size),
            sha256: Some(sha256.to_owned()),
        }
    }

    fn transport_dir(path: &str) -> TransportEntry {
        TransportEntry {
            path: path.to_owned(),
            entry_type: "directory".to_owned(),
            blob: None,
            size: None,
            sha256: None,
        }
    }

    fn transport_manifest(entries: Vec<TransportEntry>) -> TransportManifest {
        TransportManifest {
            format: "winlane-workspace-transport".to_owned(),
            version: 1,
            entries,
        }
    }

    #[test]
    fn manifest_validation_rejects_malformed_paths_and_conflicts() {
        let digest = "a".repeat(64);
        for path in [
            "../evil",
            "/absolute",
            "a//b",
            "a/./b",
            "a/../b",
            "a\\b",
            "control\u{1}",
            "a/CON",
            "a/com1.txt",
            "a/trailing.",
            "a/trailing ",
            "a/file.txt:stream",
            "a/bad*name",
            "a/bad?name",
            "a/bad<name",
            "a/bad>name",
            "a/bad|name",
        ] {
            let manifest = transport_manifest(vec![transport_file(path, 0, 1, &digest)]);
            assert!(
                validate_transport_manifest(&manifest, ArchiveLimits::default()).is_err(),
                "{path}"
            );
        }

        let duplicate = transport_manifest(vec![
            transport_file("a.txt", 0, 1, &digest),
            transport_file("a.txt", 1, 1, &digest),
        ]);
        assert!(validate_transport_manifest(&duplicate, ArchiveLimits::default()).is_err());
        let case_duplicate = transport_manifest(vec![
            transport_file("A.txt", 0, 1, &digest),
            transport_file("a.TXT", 1, 1, &digest),
        ]);
        assert!(validate_transport_manifest(&case_duplicate, ArchiveLimits::default()).is_err());
        let file_parent = transport_manifest(vec![
            transport_file("a", 0, 1, &digest),
            transport_file("a/b", 1, 1, &digest),
        ]);
        assert!(validate_transport_manifest(&file_parent, ArchiveLimits::default()).is_err());
        let missing_parent = transport_manifest(vec![transport_file("a/b", 0, 1, &digest)]);
        assert!(validate_transport_manifest(&missing_parent, ArchiveLimits::default()).is_err());
        let valid = transport_manifest(vec![
            transport_dir("a"),
            transport_file("a/b", 0, 1, &digest),
        ]);
        assert!(validate_transport_manifest(&valid, ArchiveLimits::default()).is_ok());
    }

    #[test]
    fn manifest_validation_rejects_bad_file_metadata_and_blob_indices() {
        let digest = "a".repeat(64);
        for file in [
            transport_file("a", 1, 1, &digest),
            transport_file("a", 0, 1, "bad"),
        ] {
            let manifest = transport_manifest(vec![file]);
            assert!(validate_transport_manifest(&manifest, ArchiveLimits::default()).is_err());
        }
        let duplicate_blob = transport_manifest(vec![
            transport_file("a", 0, 1, &digest),
            transport_file("b", 0, 1, &digest),
        ]);
        assert!(validate_transport_manifest(&duplicate_blob, ArchiveLimits::default()).is_err());
        let missing_blob_zero = transport_manifest(vec![
            transport_file("a", 1, 1, &digest),
            transport_file("b", 2, 1, &digest),
        ]);
        assert!(validate_transport_manifest(&missing_blob_zero, ArchiveLimits::default()).is_err());
        let mut directory_metadata = transport_dir("a");
        directory_metadata.size = Some(0);
        assert!(
            validate_transport_manifest(
                &transport_manifest(vec![directory_metadata]),
                ArchiveLimits::default()
            )
            .is_err()
        );
    }

    #[test]
    fn canonical_manifest_hash_ignores_order_and_uses_logical_semantics() {
        let digest = "a".repeat(64);
        let first = transport_manifest(vec![
            transport_dir("a"),
            transport_file("a/b", 0, 1, &digest),
        ]);
        let second = transport_manifest(vec![
            transport_file("a/b", 7, 1, &digest),
            transport_dir("a"),
        ]);
        assert_eq!(
            canonical_manifest_hash(&first),
            canonical_manifest_hash(&second)
        );
        let changed_size = transport_manifest(vec![
            transport_dir("a"),
            transport_file("a/b", 0, 2, &digest),
        ]);
        assert_ne!(
            canonical_manifest_hash(&first),
            canonical_manifest_hash(&changed_size)
        );
    }

    #[test]
    fn materialize_transport_rejects_hash_size_mismatch() {
        let digest = "a".repeat(64);
        for (content, size, hash) in [
            (b"abc".as_slice(), 2, digest.as_str()),
            (b"abc".as_slice(), 3, digest.as_str()),
        ] {
            let temp = TestTempDir::new();
            let transport = temp.path.join("transport");
            let target = temp.path.join("target");
            fs::create_dir_all(transport.join("blobs")).unwrap();
            fs::create_dir(&target).unwrap();
            fs::write(transport.join("blobs/000000"), content).unwrap();
            let manifest = transport_manifest(vec![transport_file("file", 0, size, hash)]);
            fs::write(
                transport.join("manifest.json"),
                serde_json::to_vec(&manifest).unwrap(),
            )
            .unwrap();
            assert!(materialize_transport(&transport, &target).is_err());
            assert!(!target.join("file").exists());
        }
    }

    #[test]
    fn powershell_transport_scripts_are_ascii_and_utf8_manifest_based() {
        let source = include_str!("transfer.rs");
        let export_start = source.find("fn run_guest_export_transport").unwrap();
        let materialize_start = source.find("fn run_guest_materialize").unwrap();
        let materialize_end = source[materialize_start..]
            .find("fn run_guest_tar")
            .unwrap()
            + materialize_start;
        assert!(source[export_start..materialize_start].is_ascii());
        assert!(source[materialize_start..materialize_end].is_ascii());
        assert!(source[export_start..materialize_start].contains("UTF8Encoding]::new($false)"));
        assert!(source[materialize_start..materialize_end].contains("UTF8Encoding]::new($false)"));
        assert!(source[materialize_start..materialize_end].contains("bad parent"));
        assert!(source[materialize_start..materialize_end].contains("hash mismatch"));
        let script = guest_tar_script(
            r"C:\Windows\Temp\a.tar",
            r"C:\Windows\Temp\transport",
            false,
        )
        .unwrap();
        assert!(script.contains("--format ustar"));
        let script =
            guest_tar_script(r"C:\Windows\Temp\a.tar", r"C:\Windows\Temp\transport", true).unwrap();
        assert!(!script.contains("--format ustar"));
    }

    #[test]
    fn workspace_archive_transport_round_trip_preserves_empty_dirs() {
        let temp = TestTempDir::new();
        let source = temp.path.join("source");
        fs::create_dir_all(source.join("alpha/nested")).unwrap();
        fs::create_dir_all(source.join("empty-dir")).unwrap();
        fs::write(source.join("alpha/nested/data.bin"), b"data").unwrap();
        fs::write(source.join("empty.txt"), b"").unwrap();
        let (archive, stats, logical_sha256) = create_workspace_archive(&source).unwrap();
        assert_eq!(stats.members, 5);
        assert_eq!(stats.file_bytes, 4);
        assert_eq!(logical_sha256.len(), 64);
        let bytes = fs::read(&archive.path).unwrap();
        assert_eq!(&bytes[0..12], b"blobs/000000");
        assert_eq!(bytes[156], b'0');
        let names = tar_header_names(&bytes);
        assert!(names.iter().all(|(name, _)| name.is_ascii()));
        assert!(!names.iter().any(|(name, _)| name.contains("alpha")));

        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let transport = temp.path.join("transport");
        fs::create_dir(&transport).unwrap();
        extract_workspace_archive(&archive.path, &transport, ArchiveLimits::default()).unwrap();
        let (probe, _) = read_transport_manifest(&transport).unwrap();
        validate_transport_layout(&transport, &probe).unwrap();
        let (manifest, file_bytes) = materialize_transport(&transport, &target).unwrap();
        assert_eq!(file_bytes, stats.file_bytes);
        assert_eq!(manifest.entries.len() as u64, stats.members);
        assert_eq!(canonical_manifest_hash(&manifest), logical_sha256);
        assert_eq!(
            fs::read(target.join("alpha/nested/data.bin")).unwrap(),
            b"data"
        );
        assert_eq!(fs::read(target.join("empty.txt")).unwrap(), b"");
        assert!(target.join("empty-dir").is_dir());
    }

    #[test]
    fn workspace_archive_unicode_non_bmp_names_round_trip_as_manifest_data() {
        let temp = TestTempDir::new();
        let source = temp.path.join("source");
        let unicode_dir = source.join("unicode-é");
        fs::create_dir_all(&unicode_dir).unwrap();
        fs::write(unicode_dir.join("日程🙂.txt"), "unicode content".as_bytes()).unwrap();
        let (archive, stats, logical_sha256) = create_workspace_archive(&source).unwrap();
        assert_eq!(stats.members, 2);
        let bytes = fs::read(&archive.path).unwrap();
        let names = tar_header_names(&bytes);
        assert!(names.iter().all(|(name, _)| name.is_ascii()));
        assert!(names.iter().all(|(_, typeflag)| *typeflag == b'0'));
        assert!(String::from_utf8_lossy(&bytes).contains("unicode-é/日程🙂.txt"));

        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let transport = temp.path.join("transport");
        fs::create_dir(&transport).unwrap();
        extract_workspace_archive(&archive.path, &transport, ArchiveLimits::default()).unwrap();
        let (probe, _) = read_transport_manifest(&transport).unwrap();
        validate_transport_layout(&transport, &probe).unwrap();
        let (manifest, _) = materialize_transport(&transport, &target).unwrap();
        assert_eq!(canonical_manifest_hash(&manifest), logical_sha256);
        assert_eq!(
            fs::read(target.join("unicode-é/日程🙂.txt")).unwrap(),
            b"unicode content"
        );
    }

    #[test]
    fn workspace_archive_long_name_round_trips_without_long_tar_names() {
        let temp = TestTempDir::new();
        let source = temp.path.join("source");
        fs::create_dir(&source).unwrap();
        let name = "a".repeat(120);
        fs::write(source.join(&name), b"long").unwrap();
        let (archive, _, _) = create_workspace_archive(&source).unwrap();
        let bytes = fs::read(&archive.path).unwrap();
        let names = tar_header_names(&bytes);
        assert!(names.iter().all(|(name, _)| name.len() <= 32));
        assert!(!names.iter().any(|(name, _)| name.contains("aaaa")));

        let target = temp.path.join("target");
        fs::create_dir(&target).unwrap();
        let transport = temp.path.join("transport");
        fs::create_dir(&transport).unwrap();
        extract_workspace_archive(&archive.path, &transport, ArchiveLimits::default()).unwrap();
        let (probe, _) = read_transport_manifest(&transport).unwrap();
        validate_transport_layout(&transport, &probe).unwrap();
        materialize_transport(&transport, &target).unwrap();
        assert_eq!(fs::read(target.join(name)).unwrap(), b"long");
    }

    #[test]
    fn workspace_archive_transport_members_are_ascii_only() {
        let temp = TestTempDir::new();
        let source = temp.path.join("source");
        fs::create_dir(&source).unwrap();
        fs::write(source.join("ascii.txt"), b"ascii").unwrap();
        let (archive, _, _) = create_workspace_archive(&source).unwrap();
        let bytes = fs::read(&archive.path).unwrap();
        let names = tar_header_names(&bytes);
        assert!(names.iter().all(|(name, _)| name.is_ascii()));
        assert!(!names.iter().any(|(name, _)| name.contains("ascii.txt")));
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
        assert!(source.contains(
            "if($parent -and -not(Test-Path -LiteralPath $parent)){New-Item -ItemType Directory -Force -Path $parent"
        ));
        assert!(source.contains(
            "if(Test-Path -LiteralPath $target){$item=Get-Item -LiteralPath $target -Force"
        ));
        assert!(source.contains("New-Item -ItemType Directory -Force -Path $staging"));
        assert!(source.contains("[System.IO.FileAttributes]::ReparsePoint"));
        assert!(source.contains("function Has-Reparse($dir)"));
        assert!(source.contains("if(Has-Reparse $target){exit 5}"));
        assert!(
            source.contains("Remove-Item -LiteralPath $backup -Recurse -Force -ErrorAction Stop")
        );
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
        for path in [
            r"C:\workspace\name.",
            r"C:\workspace\name ",
            r"C:\workspace\CON",
            r"C:\workspace\com1.txt",
            r"C:\workspace\file.txt:stream",
            r"C:\workspace\bad*name",
            r"C:\workspace\bad?name",
            r"C:\workspace\bad<name",
            r"C:\workspace\bad>name",
            r"C:\workspace\bad|name",
        ] {
            assert!(validate_guest_transfer_path(path, false).is_err());
        }
        assert!(validate_guest_transfer_path(r"C:\workspace\unicodé-[x]'", false).is_ok());
    }

    #[test]
    fn guest_workspace_sibling_path_uses_single_separator() {
        let path =
            guest_workspace_sibling_path(r"C:\winlane-workspace", "workspace-import").unwrap();
        assert!(path.starts_with(r"C:\.winlane-"));
        assert!(!path.starts_with(r"C:\\.winlane-"));
        let nested = guest_workspace_sibling_path(r"C:\parent\child", "workspace-import").unwrap();
        assert!(nested.starts_with(r"C:\parent\.winlane-"));
        assert!(!nested.contains(r"\\.winlane-"));
    }

    #[test]
    fn guest_tar_script_uses_relative_archive_name() {
        let script = guest_tar_script(
            r"C:\Windows\Temp\winlane-workspace.tar",
            r"C:\winlane-workspace",
            false,
        )
        .unwrap();
        assert!(!script.contains("--force-local"));
        assert!(script.contains("$archiveName='winlane-workspace.tar'"));
        assert!(script.contains(r"$archiveDir='C:\Windows\Temp'"));
        assert!(script.contains(r"$targetDir='C:\winlane-workspace'"));
    }
}
