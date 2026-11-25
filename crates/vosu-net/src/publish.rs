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

use std::io::{self, Write};
use std::path::Path;

use tokio_util::sync::CancellationToken;

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
}
