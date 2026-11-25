use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::path::{Component, Path, PathBuf};

use tokio_util::sync::CancellationToken;
use vosu_plugin_api::manifest::files::portable_component;

use super::{NetworkError, PartialDirectory, ZipLimits};

/// What one end of central directory (EOCD) record says, read the way the
/// `zip` decoder reads it.
struct EndRecord {
    /// Where the record and its comment end.
    comment_end: u64,
    /// The entry count the decoder uses: the ZIP64 total when the record
    /// defers to ZIP64, otherwise the classic count for this disk.
    entries: u64,
    /// The largest of every entry count field, classic and ZIP64.
    largest: u64,
    /// A ZIP64 locator and record precede the record.
    zip64: bool,
}

/// Reads the EOCD record at `position`, or `None` when no record fits there.
/// A ZIP64 locator right before the record must point at a ZIP64 record that
/// ends exactly at the locator; anything else is refused, because the
/// decoder's own ZIP64 search is laxer and could find a different count.
fn read_end_record(
    file: &mut std::fs::File,
    position: u64,
    file_length: u64,
) -> Result<Option<EndRecord>, NetworkError> {
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
    let comment_end = position + 22 + comment_length;
    if comment_end > file_length {
        return Ok(None);
    }
    let on_disk = u16::from_le_bytes([end_record[8], end_record[9]]) as u64;
    let total = u16::from_le_bytes([end_record[10], end_record[11]]) as u64;
    let directory_size = u32::from_le_bytes(end_record[12..16].try_into().unwrap());
    let directory_offset = u32::from_le_bytes(end_record[16..20].try_into().unwrap());
    let defers_to_zip64 =
        total == u16::MAX as u64 || directory_size == u32::MAX || directory_offset == u32::MAX;
    let mut record = EndRecord {
        comment_end,
        entries: on_disk,
        largest: on_disk.max(total),
        zip64: false,
    };

    if position >= 20 {
        let locator_position = position - 20;
        file.seek(SeekFrom::Start(locator_position))?;
        let mut locator = [0u8; 20];
        file.read_exact(&mut locator)?;
        if &locator[..4] == b"PK\x06\x07" {
            let offset = u64::from_le_bytes(locator[8..16].try_into().unwrap());
            if offset
                .checked_add(56)
                .is_none_or(|end| end > locator_position)
            {
                return Err(NetworkError::UnsafeZip("invalid ZIP64 end record".into()));
            }
            file.seek(SeekFrom::Start(offset))?;
            let mut zip64 = [0u8; 56];
            file.read_exact(&mut zip64)?;
            if &zip64[..4] != b"PK\x06\x06" {
                return Err(NetworkError::UnsafeZip("invalid ZIP64 end record".into()));
            }
            let record_length = u64::from_le_bytes(zip64[4..12].try_into().unwrap());
            if record_length < 44
                || offset
                    .checked_add(12)
                    .and_then(|start| start.checked_add(record_length))
                    != Some(locator_position)
            {
                return Err(NetworkError::UnsafeZip(
                    "ambiguous ZIP64 end record offset".into(),
                ));
            }
            let on_disk64 = u64::from_le_bytes(zip64[24..32].try_into().unwrap());
            let total64 = u64::from_le_bytes(zip64[32..40].try_into().unwrap());
            if defers_to_zip64 {
                record.entries = total64;
            }
            // Classic fields saturated to 0xFFFF only defer to these counts.
            let classic = [on_disk, total]
                .into_iter()
                .filter(|count| *count != u16::MAX as u64)
                .max()
                .unwrap_or(0);
            record.largest = classic.max(on_disk64).max(total64);
            record.zip64 = true;
        }
    }
    Ok(Some(record))
}

/// Finds the archive's EOCD record and checks its entry count against
/// `maximum` before any decoder sees the file. Returns the number of entries
/// the `zip` decoder will read from that record; callers compare it with
/// the opened archive so a decoder that settled on another record is refused.
///
/// The archive's record is the one whose comment ends exactly at the end of
/// the file, as in the ZIP specification's search. `PK\x05\x06` bytes that
/// happen to occur inside compressed entry data are not records, so their
/// counts no longer decide anything; the decoder only falls back to such a
/// signature when it rejects the real record, and the count comparison then
/// refuses the archive. Those fallbacks stay bounded: a classic record holds
/// at most 65,535 entries, and a signature that defers to ZIP64 behind a
/// locator is checked against `maximum` like the real record, because a
/// ZIP64 count is what could make the decoder reserve without bound.
///
/// Refused: data after the archive (no record ends at the end of the file),
/// and any other signature after the archive's record. A second record inside
/// the archive comment is ambiguous even when it also ends at the end of the
/// file and agrees on the count: the specification's backward search and the
/// `zip` decoder take the later record, other readers the earlier one, so the
/// two could list different directories. Real archives never do this.
pub fn preflight_zip_entries(
    file: &mut std::fs::File,
    maximum: usize,
    cancel: &CancellationToken,
) -> Result<u64, NetworkError> {
    let length = file.metadata()?.len();
    if length < 22 {
        return Err(NetworkError::NotZip);
    }

    const SCAN_CHUNK: usize = 1024 * 1024;
    const MAX_EOCD_CANDIDATES: usize = 100_000;
    let mut offset = 0u64;
    let mut overlap = Vec::<u8>::new();
    let mut chunk = vec![0u8; SCAN_CHUNK];
    let mut last_candidate = None;
    let mut candidate_count = 0usize;
    let mut archive_end = None;
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
            let Some(record) = read_end_record(file, position, length)? else {
                continue;
            };
            if record.comment_end == length {
                archive_end.get_or_insert((position, record));
            } else if record.zip64 && record.largest > maximum as u64 {
                return Err(NetworkError::UnsafeZip("entry limit exceeded".into()));
            }
        }
        overlap = scan[scan.len().saturating_sub(3)..].to_vec();
        offset += read as u64;
    }
    let Some(last_candidate) = last_candidate else {
        return Err(NetworkError::NotZip);
    };
    let Some((position, record)) = archive_end else {
        return Err(NetworkError::UnsafeZip(
            "no ZIP end record at the end of the file".into(),
        ));
    };
    if last_candidate != position {
        return Err(NetworkError::UnsafeZip(
            "ZIP comment contains another end record".into(),
        ));
    }
    if record.largest > maximum as u64 {
        return Err(NetworkError::UnsafeZip("entry limit exceeded".into()));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(record.entries)
}

/// Opens a preflighted archive and refuses it unless the decoder read the
/// central directory that the archive's own end record describes.
pub fn open_preflighted_zip(
    file: std::fs::File,
    entries: u64,
) -> Result<zip::ZipArchive<std::fs::File>, NetworkError> {
    let archive = zip::ZipArchive::new(file)?;
    if archive.len() as u64 != entries {
        return Err(NetworkError::UnsafeZip(
            "ZIP central directory does not match its end record".into(),
        ));
    }
    Ok(archive)
}
/// Shared counter/import entry preflight. Reject nonportable separators and
/// Windows prefixes/alternate streams even when validation runs on Unix, plus
/// Windows device names and the trailing dot/space Windows silently strips
/// (the SDK's package-path [`portable_component`] rule).
pub fn validate_zip_entry(name: &str, unix_mode: Option<u32>) -> Result<PathBuf, NetworkError> {
    let path = Path::new(name);
    if name.is_empty()
        || name.contains(['\\', ':', '\0'])
        || path.is_absolute()
        || path
            .components()
            .any(|part| !matches!(part, Component::Normal(_)))
    {
        return Err(NetworkError::UnsafeZip(name.into()));
    }
    for part in path.components() {
        if let Component::Normal(part) = part
            && !portable_component(&part.to_string_lossy())
        {
            return Err(NetworkError::UnsafeZip("nonportable entry name".into()));
        }
    }
    if unix_mode.is_some_and(|mode| {
        let kind = mode & 0o170000;
        kind != 0 && kind != 0o100000 && kind != 0o040000
    }) {
        return Err(NetworkError::UnsafeZip("non-regular archive entry".into()));
    }
    Ok(path.to_path_buf())
}

/// Case-insensitive key matching Windows' per-character simple upcase table.
fn fold_path(path: &Path) -> String {
    path.to_string_lossy()
        .chars()
        .map(|c| {
            let mut upper = c.to_uppercase();
            match (upper.next(), upper.next()) {
                (Some(single), None) => single,
                _ => c,
            }
        })
        .collect()
}

/// Every exact spelling of one portable name must agree; a repeated file entry
/// or two spellings that one OS would merge make the tree platform-dependent.
fn claim_portable(
    claimed: &mut HashMap<String, (PathBuf, bool)>,
    relative: &Path,
    is_directory: bool,
) -> Result<(), NetworkError> {
    let mut prefix = PathBuf::new();
    let mut components = relative.components().peekable();
    while let Some(component) = components.next() {
        prefix.push(component);
        let directory = is_directory || components.peek().is_some();
        match claimed.entry(fold_path(&prefix)) {
            std::collections::hash_map::Entry::Vacant(slot) => {
                slot.insert((prefix.clone(), directory));
            }
            std::collections::hash_map::Entry::Occupied(slot) => {
                let (exact, was_directory) = slot.get();
                if *exact != prefix {
                    return Err(NetworkError::UnsafeZip("portable name collision".into()));
                }
                if !(directory && *was_directory) {
                    return Err(NetworkError::UnsafeZip("duplicate archive entry".into()));
                }
            }
        }
    }
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
    // SAFETY: `source` and `destination` are NUL-terminated UTF-16 strings
    // held alive for the call, the only precondition `MoveFileExW`
    // documents for its pointer arguments.
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
    // SAFETY: `renameat2` is a direct syscall; `staging_c` and
    // `destination_c` are NUL-terminated C strings held alive for the call,
    // and `AT_FDCWD` is a documented sentinel rather than a real descriptor.
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
    extract_zip_atomic_validated(archive_path, destination, limits, cancel, |_| Ok(()))
}

/// The one archive extractor: preflight the retained handle, then write every
/// entry into `staging` under the path, link, count, portable-name and
/// expanded-size limits. Returns the directories to flush.
fn extract_entries(
    mut file: std::fs::File,
    staging: &Path,
    limits: ZipLimits,
    cancel: &CancellationToken,
) -> Result<HashSet<PathBuf>, NetworkError> {
    file.seek(SeekFrom::Start(0))?;
    let entries = preflight_zip_entries(&mut file, limits.max_entries, cancel)?;
    if cancel.is_cancelled() {
        return Err(NetworkError::Cancelled);
    }
    let mut archive = open_preflighted_zip(file, entries)?;
    if archive.len() > limits.max_entries {
        return Err(NetworkError::UnsafeZip("entry limit exceeded".into()));
    }
    let mut expanded = 0u64;
    let mut directories = HashSet::from([staging.to_owned()]);
    let mut extracted_paths = HashSet::new();
    let mut claimed = HashMap::new();
    let mut buffer = [0u8; 64 * 1024];
    for index in 0..archive.len() {
        if cancel.is_cancelled() {
            return Err(NetworkError::Cancelled);
        }
        let mut entry = archive.by_index(index)?;
        let enclosed = validate_zip_entry(entry.name(), entry.unix_mode())?;
        claim_portable(&mut claimed, &enclosed, entry.is_dir())?;
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
        let output = staging.join(enclosed);
        if entry.is_dir() {
            remember_paths(
                &mut directories,
                &mut extracted_paths,
                staging,
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
            staging,
            &output,
            false,
            limits.max_entries,
        )?;
        if let Some(directory) = output.parent() {
            std::fs::create_dir_all(directory)?;
        }
        // create_new: a name the portable claim missed can never truncate an
        // earlier entry's bytes after they were counted.
        let mut target = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&output)?;
        let mut copied = 0u64;
        loop {
            if cancel.is_cancelled() {
                return Err(NetworkError::Cancelled);
            }
            let allowance = remaining.saturating_sub(copied);
            let read_length = buffer.len().min(allowance.saturating_add(1) as usize);
            // The decoder verifies each entry's CRC-32 at its end; a late
            // mismatch fails here, before validation or publication.
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
    Ok(directories)
}

fn sync_tree(
    directories: HashSet<PathBuf>,
    parent: &Path,
    cancel: &CancellationToken,
) -> Result<(), NetworkError> {
    let mut directories: Vec<_> = directories.into_iter().collect();
    directories.sort_by_key(|path| std::cmp::Reverse(path.components().count()));
    for directory in directories {
        if cancel.is_cancelled() {
            return Err(NetworkError::Cancelled);
        }
        sync_directory(&directory)?;
    }
    sync_directory(parent)?;
    Ok(())
}

/// Validate and prepare the complete staging tree before replacing an installation.
pub fn extract_zip_atomic_validated(
    archive_path: &Path,
    destination: &Path,
    limits: ZipLimits,
    cancel: &CancellationToken,
    validate: impl FnOnce(&Path) -> Result<(), NetworkError>,
) -> Result<(), NetworkError> {
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    std::fs::create_dir_all(parent)?;
    recover_install(destination, parent)?;
    let staging = tempfile::Builder::new()
        .prefix(".extract-")
        .tempdir_in(parent)?;
    let file = std::fs::File::open(archive_path)?;
    let directories = extract_entries(file, staging.path(), limits, cancel)?;
    validate(staging.path())?;
    if cancel.is_cancelled() {
        return Err(NetworkError::Cancelled);
    }
    sync_tree(directories, parent, cancel)?;
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

/// A completely extracted, flushed and unpublished package tree in a private
/// directory under the caller's staging parent. Dropping it removes the tree.
#[derive(Debug)]
pub struct StagedTree {
    directory: tempfile::TempDir,
}
impl StagedTree {
    pub fn path(&self) -> &Path {
        self.directory.path()
    }
    /// Transfer ownership of the staged directory to a recovery journal.
    pub fn keep(self) -> PathBuf {
        self.directory.keep()
    }
}

/// Extract from the caller's retained, already verified handle (never by
/// reopening a path) into `parent`, which must be on the destination's
/// filesystem. Nothing is published; the caller owns validation and commit.
pub fn extract_zip_staged(
    file: std::fs::File,
    parent: &Path,
    limits: ZipLimits,
    cancel: &CancellationToken,
) -> Result<StagedTree, NetworkError> {
    std::fs::create_dir_all(parent)?;
    let directory = tempfile::Builder::new()
        .prefix(".stage-")
        .tempdir_in(parent)?;
    let directories = extract_entries(file, directory.path(), limits, cancel)?;
    sync_tree(directories, parent, cancel)?;
    Ok(StagedTree { directory })
}

/// Durable no-replace directory rename within one filesystem: fails if
/// `destination` exists, then flushes both parents' namespace changes.
pub fn rename_directory_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    let source_parent = source
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "source has no parent"))?;
    let parent = destination
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent"))?;
    rename_no_replace(source, destination)?;
    sync_directory(parent)?;
    if source_parent != parent {
        sync_directory(source_parent)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;
    let source = CString::new(source.as_os_str().as_bytes())
        .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "source path contains NUL"))?;
    let destination = CString::new(destination.as_os_str().as_bytes()).map_err(|_| {
        io::Error::new(io::ErrorKind::InvalidInput, "destination path contains NUL")
    })?;
    // SAFETY: `renameat2` is a direct syscall; both arguments are
    // NUL-terminated C strings held alive for the call, `AT_FDCWD` is a
    // documented sentinel and flag 1 is RENAME_NOREPLACE.
    let result = unsafe {
        libc::syscall(
            libc::SYS_renameat2,
            libc::AT_FDCWD,
            source.as_ptr(),
            libc::AT_FDCWD,
            destination.as_ptr(),
            1u32,
        )
    };
    if result != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(windows)]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    let source: Vec<u16> = source.as_os_str().encode_wide().chain(Some(0)).collect();
    let destination: Vec<u16> = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect();
    // SAFETY: both arguments are NUL-terminated UTF-16 strings held alive for
    // the call. Without MOVEFILE_REPLACE_EXISTING an existing name fails.
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

#[cfg(not(any(windows, target_os = "linux")))]
fn rename_no_replace(source: &Path, destination: &Path) -> io::Result<()> {
    if destination.exists() {
        return Err(io::ErrorKind::AlreadyExists.into());
    }
    std::fs::rename(source, destination)
}
