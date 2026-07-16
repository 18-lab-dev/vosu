use std::collections::HashSet;
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use tokio_util::sync::CancellationToken;

use super::{NetworkError, PartialDirectory, ZipLimits};

fn zip_candidate_entries(
    file: &mut std::fs::File,
    position: u64,
    file_length: u64,
) -> Result<Option<u64>, NetworkError> {
    if position.checked_add(22).is_none_or(|end| end > file_length) {
        return Ok(None);
    }
    file.seek(SeekFrom::Start(position))?;
    let mut end_record = [0u8; 22];
    file.read_exact(&mut end_record)?;
    if &end_record[..4] != b"PK\x05\x06" {
        return Ok(None);
    }
    let comment_length = u16::from_le_bytes([end_record[20], end_record[21]]) as u64;
    if position
        .checked_add(22 + comment_length)
        .is_none_or(|end| end > file_length)
    {
        return Ok(None);
    }

    if position >= 20 {
        file.seek(SeekFrom::Start(position - 20))?;
        let mut locator = [0u8; 20];
        file.read_exact(&mut locator)?;
        if &locator[..4] == b"PK\x06\x07" {
            let offset = u64::from_le_bytes(locator[8..16].try_into().unwrap());
            if offset.checked_add(56).is_none_or(|end| end > file_length) {
                return Err(NetworkError::UnsafeZip("invalid ZIP64 end record".into()));
            }
            file.seek(SeekFrom::Start(offset))?;
            let mut record = [0u8; 56];
            file.read_exact(&mut record)?;
            if &record[..4] != b"PK\x06\x06" {
                return Err(NetworkError::UnsafeZip("invalid ZIP64 end record".into()));
            }
            return Ok(Some(u64::from_le_bytes(record[32..40].try_into().unwrap())));
        }
    }

    Ok(Some(
        u16::from_le_bytes([end_record[10], end_record[11]]) as u64
    ))
}

pub(super) fn preflight_zip_entries(
    file: &mut std::fs::File,
    maximum: usize,
    cancel: &CancellationToken,
) -> Result<(), NetworkError> {
    let length = file.metadata()?.len();
    if length < 22 {
        return Err(NetworkError::UnsafeZip("missing ZIP end record".into()));
    }

    // `zip` deliberately falls back through every EOCD signature in the file,
    // not only the conventional 65,557-byte tail. Mirror that search without
    // loading an attacker-controlled archive into memory so no fallback record
    // can make the decoder reserve beyond our entry limit.
    const SCAN_CHUNK: usize = 1024 * 1024;
    const MAX_EOCD_CANDIDATES: usize = 100_000;
    let mut offset = 0u64;
    let mut overlap = Vec::<u8>::new();
    let mut chunk = vec![0u8; SCAN_CHUNK];
    let mut last_candidate = None;
    let mut found = false;
    let mut candidate_count = 0usize;
    while offset < length {
        if cancel.is_cancelled() {
            return Err(NetworkError::Cancelled);
        }
        file.seek(SeekFrom::Start(offset))?;
        let read = file.read(&mut chunk)?;
        if read == 0 {
            break;
        }
        let overlap_length = overlap.len();
        let mut scan = overlap;
        scan.extend_from_slice(&chunk[..read]);
        let base = offset.saturating_sub(overlap_length as u64);
        for index in 0..scan.len().saturating_sub(3) {
            if &scan[index..index + 4] != b"PK\x05\x06" {
                continue;
            }
            if cancel.is_cancelled() {
                return Err(NetworkError::Cancelled);
            }
            let position = base + index as u64;
            if last_candidate == Some(position) {
                continue;
            }
            last_candidate = Some(position);
            candidate_count += 1;
            if candidate_count > MAX_EOCD_CANDIDATES {
                return Err(NetworkError::UnsafeZip("too many ZIP end records".into()));
            }
            if let Some(entries) = zip_candidate_entries(file, position, length)? {
                found = true;
                if entries > maximum as u64 {
                    return Err(NetworkError::UnsafeZip("entry limit exceeded".into()));
                }
            }
        }
        overlap = scan[scan.len().saturating_sub(3)..].to_vec();
        offset += read as u64;
    }
    if !found {
        return Err(NetworkError::UnsafeZip("missing ZIP end record".into()));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    std::fs::File::open(path)?.sync_all()
}

#[cfg(windows)]
fn sync_directory(_: &Path) -> io::Result<()> {
    // Windows does not provide a supported directory-flush operation.
    // Extracted files are individually flushed and installation uses
    // MoveFileExW(MOVEFILE_WRITE_THROUGH) as the durable namespace boundary.
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn sync_directory(_: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(windows)]
fn move_directory(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn move_directory(source: &Path, destination: &Path) -> io::Result<()> {
    std::fs::rename(source, destination)
}

fn remember_paths(
    directories: &mut HashSet<PathBuf>,
    extracted_paths: &mut HashSet<PathBuf>,
    root: &Path,
    output: &Path,
    output_is_directory: bool,
    maximum: usize,
) -> Result<(), NetworkError> {
    let mut current = Some(output);
    let mut is_directory = output_is_directory;
    while let Some(path) = current {
        if !path.starts_with(root) {
            break;
        }
        if path != root {
            extracted_paths.insert(path.to_owned());
            if extracted_paths.len() > maximum {
                return Err(NetworkError::UnsafeZip(
                    "extracted path limit exceeded".into(),
                ));
            }
        }
        if is_directory || path == root {
            directories.insert(path.to_owned());
        }
        if path == root {
            break;
        }
        current = path.parent();
        is_directory = true;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn install_staging(staging: &Path, destination: &Path, parent: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    if !destination.exists() {
        std::fs::rename(staging, destination)?;
        return sync_directory(parent);
    }
    let staging_c = CString::new(staging.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "staging path contains NUL"))?;
    let destination_c = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination path contains NUL")
    })?;
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            staging_c.as_ptr(),
            libc::AT_FDCWD,
            destination_c.as_ptr(),
            2u32,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    sync_directory(parent)?;
    std::fs::remove_dir_all(staging)?;
    sync_directory(parent)
}

#[cfg(not(target_os = "linux"))]
fn install_staging(staging: &Path, destination: &Path, parent: &Path) -> io::Result<()> {
    if !destination.exists() {
        move_directory(staging, destination)?;
        return sync_directory(parent);
    }
    let backup = backup_path(destination, parent)?;
    if backup.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "install recovery backup already exists",
        ));
    }
    move_directory(destination, &backup)?;
    sync_directory(parent)?;
    if let Err(install_error) = move_directory(staging, destination) {
        return match move_directory(&backup, destination) {
            Ok(()) => {
                sync_directory(parent)?;
                Err(install_error)
            }
            Err(rollback_error) => Err(io::Error::other(format!(
                "install failed ({install_error}); rollback failed ({rollback_error}); backup retained at {}",
                backup.display()
            ))),
        };
    }
    sync_directory(parent)?;
    std::fs::remove_dir_all(backup)?;
    sync_directory(parent)
}

#[cfg(target_os = "linux")]
fn recover_install(_: &Path, _: &Path) -> io::Result<()> {
    Ok(())
}

#[cfg(not(target_os = "linux"))]
fn backup_path(destination: &Path, parent: &Path) -> io::Result<PathBuf> {
    let name = destination.file_name().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination has no file name")
    })?;
    let mut backup = std::ffi::OsString::from(".");
    backup.push(name);
    backup.push(".install-backup");
    Ok(parent.join(backup))
}

#[cfg(not(target_os = "linux"))]
fn recover_install(destination: &Path, parent: &Path) -> io::Result<()> {
    let backup = backup_path(destination, parent)?;
    if !backup.exists() {
        return Ok(());
    }
    if destination.exists() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "both install destination and recovery backup exist: {}",
                backup.display()
            ),
        ));
    }
    move_directory(&backup, destination)?;
    sync_directory(parent)
}

pub fn extract_zip_atomic(
    archive_path: &Path,
    destination: &Path,
    limits: ZipLimits,
    cancel: &CancellationToken,
) -> Result<(), NetworkError> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    std::fs::create_dir_all(parent)?;
    recover_install(destination, parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".extract-")
        .tempdir_in(parent)?;
    let mut file = std::fs::File::open(archive_path)?;
    preflight_zip_entries(&mut file, limits.max_entries, cancel)?;
    if cancel.is_cancelled() {
        return Err(NetworkError::Cancelled);
    }
    let mut archive = zip::ZipArchive::new(file)?;
    if archive.len() > limits.max_entries {
        return Err(NetworkError::UnsafeZip("entry limit exceeded".into()));
    }
    let mut expanded = 0u64;
    let mut directories = HashSet::from([staging.path().to_owned()]);
    let mut extracted_paths = HashSet::new();
    let mut buffer = [0u8; 64 * 1024];
    for index in 0..archive.len() {
        if cancel.is_cancelled() {
            return Err(NetworkError::Cancelled);
        }
        let mut entry = archive.by_index(index)?;
        let name = entry.name().to_owned();
        let raw_path = Path::new(&name);
        if raw_path.is_absolute()
            || raw_path.components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(NetworkError::UnsafeZip(name));
        }
        let enclosed = entry
            .enclosed_name()
            .ok_or_else(|| NetworkError::UnsafeZip(name.clone()))?;
        if enclosed.is_absolute()
            || enclosed
                .components()
                .any(|part| !matches!(part, Component::Normal(_)))
        {
            return Err(NetworkError::UnsafeZip(name));
        }
        if entry
            .unix_mode()
            .is_some_and(|mode| mode & 0o170000 == 0o120000)
        {
            return Err(NetworkError::UnsafeZip("symbolic link".into()));
        }
        let entry_size = entry.size();
        let remaining = limits
            .max_expanded_bytes
            .checked_sub(expanded)
            .ok_or_else(|| NetworkError::UnsafeZip("expanded size limit exceeded".into()))?;
        if entry_size > remaining {
            return Err(NetworkError::UnsafeZip(
                "expanded size limit exceeded".into(),
            ));
        }
        let output = staging.path().join(enclosed);
        if entry.is_dir() {
            remember_paths(
                &mut directories,
                &mut extracted_paths,
                staging.path(),
                &output,
                true,
                limits.max_entries,
            )?;
            std::fs::create_dir_all(&output)?;
            continue;
        }
        remember_paths(
            &mut directories,
            &mut extracted_paths,
            staging.path(),
            &output,
            false,
            limits.max_entries,
        )?;
        if let Some(directory) = output.parent() {
            std::fs::create_dir_all(directory)?;
        }
        let mut target = std::fs::File::create(&output)?;
        let mut copied = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(NetworkError::Cancelled);
            }
            let allowance = remaining.saturating_sub(copied);
            let read_length = buffer.len().min(allowance.saturating_add(1) as usize);
            let read = entry.read(&mut buffer[..read_length])?;
            if read == 0 {
                break;
            }
            copied += read as u64;
            if copied > remaining {
                return Err(NetworkError::UnsafeZip(
                    "expanded size limit exceeded".into(),
                ));
            }
            target.write_all(&buffer[..read])?;
        }
        if copied != entry_size {
            return Err(NetworkError::UnsafeZip("truncated entry".into()));
        }
        expanded += copied;
        target.sync_all()?;
    }
    if cancel.is_cancelled() {
        return Err(NetworkError::Cancelled);
    }
    let mut directories: Vec<_> = directories.into_iter().collect();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    if cancel.is_cancelled() {
        return Err(NetworkError::Cancelled);
    }
    for directory in directories {
        if cancel.is_cancelled() {
            return Err(NetworkError::Cancelled);
        }
        sync_directory(&directory)?;
    }
    sync_directory(parent)?;
    if cancel.is_cancelled() {
        return Err(NetworkError::Cancelled);
    }
    let staging_path = staging.keep();
    let staging_guard = PartialDirectory(staging_path.clone());
    if cancel.is_cancelled() {
        return Err(NetworkError::Cancelled);
    }
    install_staging(&staging_path, destination, parent)?;
    sync_directory(parent)?;
    drop(staging_guard);
    Ok(())
}
