//! The atomic file publish/read convention for crates above the native
//! boundary.
//!
//! vosu has one convention for a file another reader may open while it is
//! replaced: a fully written, synced same-directory temporary file published
//! by `vosu_sys::platform::atomic_replace`, read back through the retrying
//! `vosu_sys::platform::read_published` helpers that absorb the measured
//! Windows missing-name window. Plugin-side crates (`vosu-osu-reader`'s lazer
//! offset cache) must not depend on `vosu-sys` (M5 R1), so they publish and
//! read through this module, the same seam their hardened ZIP extraction
//! ([`crate::extract_zip_atomic`]) already uses. Nothing here is a second
//! implementation: the platform code stays in `vosu-sys`.
//!
//! The overlay host (A7.1-OR) performs every write under the overlay root
//! and its settings directories through this module and
//! [`crate::extract_zip_atomic`]: [`publish_file`] for same-directory
//! atomic replacement or no-clobber creation, [`create_directories`],
//! [`remove_tree`] and [`rename_directory_no_replace`]. A missing write
//! primitive is added here, never inlined in a plugin-side crate.

use std::fs;
use std::io::{self, Write};
use std::path::Path;

use tokio_util::sync::CancellationToken;

pub use crate::streaming::rename_directory_no_replace;

/// How [`publish_file`] treats an existing target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Publish {
    /// Atomically replace an existing target (`atomic_replace`).
    Replace,
    /// Create the target only if it does not exist yet; an existing
    /// complete file wins and is never replaced.
    CreateNew,
}

/// Publishes `bytes` at `target` from a synced same-directory temporary
/// file named `{temp_prefix}*.tmp`, then syncs the parent directory.
///
/// Returns `Ok(false)` only for [`Publish::CreateNew`] when `target`
/// already exists. The temporary file is removed on every failure path, and
/// `target` is never observed partially written.
pub fn publish_file(
    target: &Path,
    bytes: &[u8],
    temp_prefix: &str,
    mode: Publish,
) -> io::Result<bool> {
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "publish target has no parent")
    })?;
    fs::create_dir_all(parent)?;
    let mut temp = tempfile::Builder::new()
        .prefix(temp_prefix)
        .suffix(".tmp")
        .tempfile_in(parent)?;
    temp.write_all(bytes)?;
    temp.flush()?;
    temp.as_file().sync_all()?;
    if mode == Publish::CreateNew {
        let created = persist_no_clobber(temp, target)?;
        if created {
            sync_directory(parent)?;
        }
        return Ok(created);
    }
    // Windows refuses to replace a file this process still holds: close the
    // temporary file before the publish. The guard removes it on failure.
    let temp_path = temp.into_temp_path();
    vosu_sys::platform::atomic_replace(&temp_path, target)?;
    sync_directory(parent)?;
    Ok(true)
}

#[cfg(not(windows))]
fn persist_no_clobber(temp: tempfile::NamedTempFile, target: &Path) -> io::Result<bool> {
    match temp.persist_noclobber(target) {
        Ok(_) => Ok(true),
        Err(error) if error.error.kind() == io::ErrorKind::AlreadyExists => Ok(false),
        Err(error) => Err(error.error),
    }
}

#[cfg(windows)]
fn persist_no_clobber(temp: tempfile::NamedTempFile, target: &Path) -> io::Result<bool> {
    let mut temp_path = temp.into_temp_path();
    let moved = move_no_replace(&temp_path, target)?;
    if moved {
        // The temporary name is gone; only disarm its removal guard.
        // `TempPath::keep` would first touch the (now missing) temporary
        // name's attributes and fail with NotFound after a successful move.
        temp_path.disable_cleanup(true);
    }
    Ok(moved)
}

#[cfg(windows)]
fn move_no_replace(source: &Path, target: &Path) -> io::Result<bool> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_NORMAL, MOVEFILE_WRITE_THROUGH, MoveFileExW, SetFileAttributesW,
    };

    let source_wide = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let target_wide = target
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    // tempfile creates the file FILE_ATTRIBUTE_TEMPORARY; clear that before
    // it becomes the published file, as tempfile's own persist does.
    // SAFETY: `source_wide` is a NUL-terminated UTF-16 string held alive for
    // the call, the only precondition `SetFileAttributesW` documents for it.
    if unsafe { SetFileAttributesW(source_wide.as_ptr(), FILE_ATTRIBUTE_NORMAL) } == 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: both buffers are NUL-terminated UTF-16 strings held alive for
    // the call, the only precondition `MoveFileExW` documents for its pointer
    // arguments. Without MOVEFILE_REPLACE_EXISTING the move never replaces.
    let moved = unsafe {
        MoveFileExW(
            source_wide.as_ptr(),
            target_wide.as_ptr(),
            MOVEFILE_WRITE_THROUGH,
        )
    };
    if moved != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::AlreadyExists {
        Ok(false)
    } else {
        Err(error)
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> io::Result<()> {
    fs::File::open(path)?.sync_all()
}
#[cfg(not(unix))]
fn sync_directory(_: &Path) -> io::Result<()> {
    Ok(())
}

/// Creates `path` and its missing parents.
pub fn create_directories(path: &Path) -> io::Result<()> {
    fs::create_dir_all(path)
}

/// Removes `path` without following a link: a symbolic link (or Windows
/// directory junction/symlink) is removed itself, a directory with its whole
/// tree, anything else as a file.
pub fn remove_tree(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    let file_type = metadata.file_type();
    if file_type.is_symlink() {
        return remove_link(path, &file_type);
    }
    if file_type.is_dir() {
        fs::remove_dir_all(path)
    } else {
        fs::remove_file(path)
    }
}

#[cfg(not(windows))]
fn remove_link(path: &Path, _: &fs::FileType) -> io::Result<()> {
    fs::remove_file(path)
}

#[cfg(windows)]
fn remove_link(path: &Path, file_type: &fs::FileType) -> io::Result<()> {
    use std::os::windows::fs::FileTypeExt;
    if file_type.is_symlink_dir() {
        fs::remove_dir(path)
    } else {
        fs::remove_file(path)
    }
}

/// Reads a file published through [`publish_file`] or [`publish_bytes`]
/// with `vosu_sys::platform::read_published`'s bounded Windows retry.
/// Blocking: async callers use [`read_published_async`] or
/// `spawn_blocking`.
pub fn read_published(path: &Path) -> io::Result<Vec<u8>> {
    vosu_sys::platform::read_published(path)
}

/// Atomically replaces `target` with `bytes`.
///
/// Writes and syncs a temporary file named `{temp_prefix}*.tmp` in
/// `target`'s directory (created if missing), then publishes it with
/// `vosu_sys::platform::atomic_replace`. `target` is untouched when any
/// step fails or `cancel` fires before the publish; the temporary file is
/// removed on every failure path.
pub fn publish_bytes(
    target: &Path,
    bytes: &[u8],
    temp_prefix: &str,
    cancel: &CancellationToken,
) -> io::Result<()> {
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    let parent = target.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "publish target has no parent")
    })?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::Builder::new()
        .prefix(temp_prefix)
        .suffix(".tmp")
        .tempfile_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file_mut().sync_all()?;
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    let temp_path = temp.into_temp_path();
    vosu_sys::platform::atomic_replace(&temp_path, target)
}

/// Reads a file published through [`publish_bytes`] (or any other
/// `atomic_replace` publish) with `vosu_sys::platform::read_published_async`'s
/// bounded Windows retry, without blocking the calling Tokio worker.
pub async fn read_published_async(path: &Path) -> io::Result<Vec<u8>> {
    vosu_sys::platform::read_published_async(path).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn publishes_replaces_and_reads_back() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("nested").join("cache.json");
        let cancel = CancellationToken::new();
        publish_bytes(&target, b"first", ".publish-", &cancel).unwrap();
        assert_eq!(read_published_async(&target).await.unwrap(), b"first");
        publish_bytes(&target, b"second", ".publish-", &cancel).unwrap();
        assert_eq!(read_published_async(&target).await.unwrap(), b"second");
        let leftovers: Vec<_> = std::fs::read_dir(target.parent().unwrap())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name != "cache.json")
            .collect();
        assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
    }

    #[tokio::test]
    async fn cancelled_publish_leaves_the_target_untouched() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("cache.json");
        publish_bytes(&target, b"kept", ".publish-", &CancellationToken::new()).unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        let error = publish_bytes(&target, b"lost", ".publish-", &cancel).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted);
        assert_eq!(read_published_async(&target).await.unwrap(), b"kept");
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[tokio::test]
    async fn absent_published_file_is_not_found() {
        let directory = tempfile::tempdir().unwrap();
        let error = read_published_async(&directory.path().join("missing"))
            .await
            .unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::NotFound);
    }

    #[test]
    fn create_new_never_replaces_and_leaves_no_temporary_file() {
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("nested").join("values.toml");
        assert!(publish_file(&target, b"first", ".settings-", Publish::CreateNew).unwrap());
        assert!(!publish_file(&target, b"second", ".settings-", Publish::CreateNew).unwrap());
        assert_eq!(read_published(&target).unwrap(), b"first");
        #[cfg(windows)]
        {
            // The published file is an ordinary file, not tempfile's
            // FILE_ATTRIBUTE_TEMPORARY (0x100) scratch file.
            use std::os::windows::fs::MetadataExt as _;
            let attributes = std::fs::metadata(&target).unwrap().file_attributes();
            assert_eq!(attributes & 0x100, 0);
        }
        assert!(publish_file(&target, b"third", ".settings-", Publish::Replace).unwrap());
        assert_eq!(read_published(&target).unwrap(), b"third");
        assert_eq!(
            std::fs::read_dir(target.parent().unwrap()).unwrap().count(),
            1
        );
    }

    #[cfg(unix)]
    #[test]
    fn remove_tree_removes_a_link_without_following_it() {
        let directory = tempfile::tempdir().unwrap();
        let outside = directory.path().join("outside");
        std::fs::create_dir_all(outside.join("kept")).unwrap();
        let root = directory.path().join("root");
        create_directories(&root.join("folder/inner")).unwrap();
        std::os::unix::fs::symlink(&outside, root.join("link")).unwrap();
        remove_tree(&root.join("link")).unwrap();
        assert!(outside.join("kept").is_dir());
        remove_tree(&root.join("folder")).unwrap();
        assert_eq!(std::fs::read_dir(&root).unwrap().count(), 0);
    }
}
