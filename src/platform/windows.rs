//! Windows-native platform integration.

use super::PathResolver;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::os::windows::ffi::OsStrExt;
use std::os::windows::fs::OpenOptionsExt;
use std::os::windows::io::AsRawHandle;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};
use windows_sys::Win32::Foundation::{
    ERROR_ACCESS_DENIED, ERROR_CALL_NOT_IMPLEMENTED, ERROR_FILE_NOT_FOUND, ERROR_INVALID_FUNCTION,
    ERROR_INVALID_PARAMETER, ERROR_NOT_SUPPORTED, ERROR_PATH_NOT_FOUND, ERROR_SHARING_VIOLATION,
    GENERIC_WRITE,
};
use windows_sys::Win32::Storage::FileSystem::{
    DELETE, FILE_FLAG_WRITE_THROUGH, FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, FileRenameInfoEx, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    MoveFileExW, ReplaceFileW, SetFileInformationByHandle,
};

const STAGE_PREFIX: &str = ".vosu-atomic-";

pub(super) fn platform_roots(
    _resolver: &impl PathResolver,
    program: &Path,
) -> io::Result<(PathBuf, PathBuf, PathBuf)> {
    Ok((
        program.to_path_buf(),
        program.to_path_buf(),
        program.join(".cache"),
    ))
}

struct Stage {
    // Fields drop in declaration order: the exclusive lease must close before
    // TempDir removes its directory, including on an early return or unwind.
    lease: File,
    directory: tempfile::TempDir,
}
impl Stage {
    fn new(parent: &Path) -> io::Result<Self> {
        let directory = tempfile::Builder::new()
            .prefix(STAGE_PREFIX)
            .tempdir_in(parent)?;
        let lease = OpenOptions::new()
            .write(true)
            .create_new(true)
            .share_mode(0)
            .open(directory.path().join("lease"))?;
        Ok(Self { lease, directory })
    }
    fn replacement(&self) -> PathBuf {
        self.directory.path().join("replacement")
    }
    fn finish(self, result: io::Result<()>) -> io::Result<()> {
        drop(self.lease);
        self.directory.close().map_err(|error| {
            io::Error::new(
                error.kind(),
                format!(
                    "atomic replacement staging cleanup failed: {error}; operation: {result:?}"
                ),
            )
        })?;
        result
    }
}

pub(super) fn atomic_replace(temp: &Path, target: &Path) -> io::Result<()> {
    atomic_replace_with_link(temp, target, |from, to| fs::hard_link(from, to))
}

fn atomic_replace_with_link(
    temp: &Path,
    target: &Path,
    mut link: impl FnMut(&Path, &Path) -> io::Result<()>,
) -> io::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(2);
    let target_wide = wide(target);
    let temp_wide = wide(temp);
    let metadata = match fs::metadata(target) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return legacy_replace(&temp_wide, &target_wide, deadline);
        }
        Err(error) => return Err(error),
    };
    // A read-only hard link could not be removed during error cleanup. The
    // previous ReplaceFileW implementation refused read-only targets as well.
    if metadata.permissions().readonly() {
        return Err(io::Error::from_raw_os_error(ERROR_ACCESS_DENIED as i32));
    }
    // Acquire delete access before creating a second link. Existing readers
    // without share-delete must drain first; new ones cannot open until this
    // guard closes. Thus even a failed merge can unlink its private alias.
    let mut delete_guard = Some(
        match retry(deadline, || {
            OpenOptions::new()
                .read(true)
                .access_mode(DELETE)
                .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
                .open(target)
        }) {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return legacy_replace(&temp_wide, &target_wide, deadline);
            }
            Err(error) => return Err(error),
        },
    );
    let stage = Stage::new(
        target
            .parent()
            .filter(|path| !path.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new(".")),
    )?;
    let replacement = stage.replacement();
    let result = (|| {
        if let Err(error) = retry(deadline, || link(target, &replacement)) {
            if error.kind() == io::ErrorKind::NotFound || unsupported_links(&error) {
                // FAT/exFAT and some remote filesystems have no hard links.
                // Retain their existing ReplaceFileW semantics, including its
                // documented weaker concurrent-reader visibility guarantee.
                drop(delete_guard.take());
                return legacy_replace(&temp_wide, &target_wide, deadline);
            }
            return Err(error);
        }
        let replacement_wide = wide(&replacement);
        // ReplaceFileW merges DACLs, creation time, ADS, encryption/compression
        // and other metadata, but can temporarily remove its replaced name.
        // Merge on a private hard link, not the public config/settings name.
        retry(deadline, || replace_file(&temp_wide, &replacement_wide))?;
        // POSIX rename keeps old open handles usable and directs new opens to
        // the replacement. Legacy MoveFileExW can starve on continuous readers.
        match publish_file(&replacement, target, &target_wide, deadline) {
            Err(error)
                if unsupported_links(&error)
                    || error.raw_os_error() == Some(ERROR_INVALID_PARAMETER as i32) =>
            {
                // Downlevel/remote filesystems may support hard links but not
                // FileRenameInfoEx. Keep the existing bounded native rename.
                drop(delete_guard.take());
                retry(deadline, || move_file(&replacement_wide, &target_wide))
            }
            result => result,
        }
    })();
    stage.finish(result)
}

fn unsupported_links(error: &io::Error) -> bool {
    matches!(
        error.raw_os_error().map(|code| code as u32),
        Some(ERROR_INVALID_FUNCTION | ERROR_NOT_SUPPORTED | ERROR_CALL_NOT_IMPLEMENTED)
    )
}

fn replace_file(temp: &[u16], target: &[u16]) -> io::Result<()> {
    // SAFETY: both slices contain live NUL-terminated UTF-16 paths. Optional
    // pointers are null. REPLACEFILE_WRITE_THROUGH is unsupported by Windows;
    // the final handle-based rename flushes the merged file before and after publication.
    let result = unsafe {
        ReplaceFileW(
            target.as_ptr(),
            temp.as_ptr(),
            std::ptr::null(),
            0,
            std::ptr::null(),
            std::ptr::null(),
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn move_file(temp: &[u16], target: &[u16]) -> io::Result<()> {
    // SAFETY: both slices contain live NUL-terminated UTF-16 paths.
    let result = unsafe {
        MoveFileExW(
            temp.as_ptr(),
            target.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn publish_file(
    candidate: &Path,
    target: &Path,
    target_wide: &[u16],
    deadline: Instant,
) -> io::Result<()> {
    let file = retry(deadline, || {
        OpenOptions::new()
            .write(true)
            .access_mode(DELETE | GENERIC_WRITE)
            .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE)
            .custom_flags(FILE_FLAG_WRITE_THROUGH)
            .open(candidate)
    })?;
    // Metadata and streams were merged after the caller synced its contents.
    file.sync_all()?;
    let absolute;
    let name = if target.is_absolute() {
        target_wide
    } else {
        absolute = wide(&std::path::absolute(target)?);
        &absolute
    };
    let name_bytes = (name.len() - 1) * std::mem::size_of::<u16>();
    let size = std::mem::size_of::<FILE_RENAME_INFO>() + name_bytes;
    let size_u32 = u32::try_from(size).map_err(|_| io::Error::other("rename path is too long"))?;
    // FILE_RENAME_INFO has a variable-length trailing UTF-16 name. u64 storage
    // supplies the native alignment without an unaligned struct reference.
    let mut buffer = vec![0_u64; size.div_ceil(std::mem::size_of::<u64>())];
    let information = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // FILE_RENAME_REPLACE_IF_EXISTS | FILE_RENAME_POSIX_SEMANTICS, documented at
    // https://learn.microsoft.com/windows-hardware/drivers/ddi/ntifs/ns-ntifs-_file_rename_information
    // SAFETY: the aligned allocation covers the header plus the complete name;
    // `name` includes its terminator, but FileNameLength excludes that terminator.
    unsafe {
        (*information).Anonymous.Flags = 0x1 | 0x2;
        (*information).RootDirectory = std::ptr::null_mut();
        (*information).FileNameLength = name_bytes as u32;
        std::ptr::copy_nonoverlapping(
            name.as_ptr(),
            std::ptr::addr_of_mut!((*information).FileName).cast::<u16>(),
            name.len() - 1,
        );
    }
    retry(deadline, || {
        // SAFETY: the live file has delete access and the buffer is initialized
        // for FileRenameInfoEx. Windows does not retain either pointer.
        let renamed = unsafe {
            SetFileInformationByHandle(
                file.as_raw_handle(),
                FileRenameInfoEx,
                information.cast(),
                size_u32,
            )
        };
        if renamed == 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    })?;
    file.sync_all()
}

fn legacy_replace(temp: &[u16], target: &[u16], deadline: Instant) -> io::Result<()> {
    retry(deadline, || match replace_file(temp, target) {
        Err(error)
            if matches!(
                error.raw_os_error().map(|code| code as u32),
                Some(ERROR_FILE_NOT_FOUND | ERROR_PATH_NOT_FOUND)
            ) =>
        {
            move_file(temp, target)
        }
        result => result,
    })
}

#[cfg_attr(test, track_caller)]
fn retry<T>(deadline: Instant, mut operation: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    let mut delay = Duration::from_millis(10);
    let mut cooperative_attempts = 0;
    loop {
        match operation() {
            Err(error)
                if matches!(
                    error.raw_os_error().map(|code| code as u32),
                    Some(ERROR_ACCESS_DENIED | ERROR_SHARING_VIOLATION)
                ) && Instant::now() < deadline =>
            {
                // Brief readers can reopen on the same coarse timer boundary
                // as this writer. A bounded cooperative burst lets them drain
                // without extending the deadline or busy-waiting on long locks.
                if cooperative_attempts < 32 {
                    cooperative_attempts += 1;
                    thread::yield_now();
                } else {
                    cooperative_attempts = 0;
                    thread::sleep(delay.min(deadline.saturating_duration_since(Instant::now())));
                    delay = (delay * 2).min(Duration::from_millis(250));
                }
            }
            result => {
                #[cfg(test)]
                if let Err(error) = &result {
                    eprintln!(
                        "atomic replacement operation at {}: {error}",
                        std::panic::Location::caller()
                    );
                }
                return result;
            }
        }
    }
}

/// Bounded startup recovery, confined to our own regular-file staging layout.
/// A live writer holds `lease` without sharing; another process cannot sweep it.
/// Windows config/data/cache roots all live below the program directory.
pub(super) fn cleanup_stale_replacements(root: &Path) {
    fn visit(root: &Path, depth: usize, remaining: &mut usize) {
        let Ok(entries) = fs::read_dir(root) else {
            return;
        };
        for entry in entries {
            if *remaining == 0 {
                break;
            }
            *remaining -= 1;
            let Ok(entry) = entry else {
                continue;
            };
            if !entry
                .file_type()
                .is_ok_and(|kind| kind.is_dir() && !kind.is_symlink())
            {
                continue;
            }
            if entry
                .file_name()
                .to_str()
                .is_some_and(|name| name.starts_with(STAGE_PREFIX))
            {
                cleanup_stage(&entry.path());
            } else if depth > 0 {
                visit(&entry.path(), depth - 1, remaining);
            }
        }
    }
    visit(root, 4, &mut 2048);
}

fn cleanup_stage(path: &Path) {
    let Ok(entries) = fs::read_dir(path) else {
        return;
    };
    let mut count = 0;
    for entry in entries {
        count += 1;
        let Ok(entry) = entry else {
            return;
        };
        if count > 2
            || !matches!(entry.file_name().to_str(), Some("lease" | "replacement"))
            || !entry.file_type().is_ok_and(|kind| kind.is_file())
        {
            return;
        }
    }
    match OpenOptions::new()
        .write(true)
        .share_mode(0)
        .open(path.join("lease"))
    {
        Ok(lease) => drop(lease),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return,
    }
    if let Err(error) = fs::remove_dir_all(path) {
        tracing::warn!(%error, path = %path.display(), "cannot clean stale atomic replacement staging");
    }
}

fn wide(path: &Path) -> Vec<u16> {
    path.as_os_str().encode_wide().chain(Some(0)).collect()
}

#[cfg(test)]
#[path = "windows_tests.rs"]
mod tests;
