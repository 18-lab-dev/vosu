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
    path::{Path, PathBuf},
};
use tokio_util::sync::CancellationToken;
use vosu_net::{
    NetworkError, ZipLimits,
    streaming::{StagedTree, extract_zip_staged},
};
use vosu_plugin_api::manifest::files::{self, PackageFileError};

/// The 1.x archive entry limit carries over unchanged (catalog-v2.md).
pub const MAX_PACKAGE_ENTRIES: usize = 10_000;

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
/// reparse points or `..`): the SDK's one package-path rule, independent of
/// the SDK's manifest path grammar check.
pub(super) fn confined(root: &Path, relative: &str) -> Result<PathBuf, InstallError> {
    files::confined_file(root, relative).map_err(|e| asset_error(relative, e))
}

/// Bounded UTF-8 read of one confined asset.
pub(super) fn read_asset(
    root: &Path,
    relative: &str,
    limit: usize,
) -> Result<String, InstallError> {
    files::read_file(root, relative, limit).map_err(|e| asset_error(relative, e))
}

fn asset_error(relative: &str, error: PackageFileError) -> InstallError {
    match error {
        PackageFileError::Escapes(_) => InstallError::AssetEscapes(relative.to_owned()),
        PackageFileError::Open(_, error) => InstallError::io(error),
        PackageFileError::Invalid(..) => InstallError::InvalidAsset(relative.to_owned()),
    }
}
