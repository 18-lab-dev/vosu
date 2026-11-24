//! ZIP preflight into a retained staging tree and confined package asset reads.
//!
//! The shared `vosu-net` extractor is the only archive extractor: it refuses
//! traversal, links/non-regular entries, duplicate and Windows case/device-name
//! collisions, entry-count and expanded-size overruns and CRC/truncation errors
//! before returning. Every later read names a package-relative path inside the
//! staged tree this operation created and owns; no path is reopened outside it.
use super::InstallError;
use std::{
    fs::File,
    io::Read,
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;
use vosu_net::{
    NetworkError, ZipLimits,
    streaming::{StagedTree, extract_zip_staged},
};

/// The 1.x archive entry limit carries over unchanged (catalog-v2.md).
pub const MAX_PACKAGE_ENTRIES: usize = 10_000;
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;

pub(super) fn stage(
    file: File,
    parent: &Path,
    expanded_limit: u64,
    cancel: &CancellationToken,
) -> Result<StagedTree, InstallError> {
    extract_zip_staged(
        file,
        parent,
        ZipLimits {
            max_entries: MAX_PACKAGE_ENTRIES,
            max_expanded_bytes: expanded_limit,
        },
        cancel,
    )
    .map_err(|error| match error {
        NetworkError::Cancelled => InstallError::Cancelled,
        NetworkError::UnsafeZip(message) => InstallError::UnsafeArchive(message),
        _ => InstallError::UnsafeArchive("malformed or corrupt archive".into()),
    })
}

/// Resolve a manifest-declared relative path to an existing regular file whose
/// every component is a real directory/file in the staged tree (no links,
/// reparse points or `..`), independent of the SDK's own path grammar check.
pub(super) fn confined(root: &Path, relative: &str) -> Result<PathBuf, InstallError> {
    let escape = || InstallError::AssetEscapes(relative.to_owned());
    if relative.is_empty()
        || relative.contains(['\\', ':', '\0'])
        || relative.starts_with('/')
        || relative.split('/').any(|part| {
            matches!(part, "" | "." | "..") || !vosu_net::streaming::portable_component(part)
        })
    {
        return Err(escape());
    }
    let mut path = root.to_path_buf();
    let mut parts = relative.split('/').peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        let meta = std::fs::symlink_metadata(&path).map_err(|_| escape())?;
        let last = parts.peek().is_none();
        let kind = meta.file_type();
        if kind.is_symlink()
            || reparse_point(&meta)
            || (last && !kind.is_file())
            || (!last && !kind.is_dir())
        {
            return Err(escape());
        }
    }
    Ok(path)
}

#[cfg(windows)]
fn reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn reparse_point(_: &std::fs::Metadata) -> bool {
    false
}

/// Bounded UTF-8 read of one confined asset.
pub(super) fn read_asset(
    root: &Path,
    relative: &str,
    limit: usize,
) -> Result<String, InstallError> {
    let path = confined(root, relative)?;
    let mut text = String::new();
    File::open(path)
        .map_err(InstallError::io)?
        .take(limit as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|_| InstallError::InvalidAsset(relative.to_owned()))?;
    if text.len() > limit {
        return Err(InstallError::InvalidAsset(relative.to_owned()));
    }
    Ok(text)
}
