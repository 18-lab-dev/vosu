//! Linux-native platform integration.

use super::{PathResolver, nonempty_env};
use std::io;
use std::path::{Path, PathBuf};

pub(super) fn platform_roots(
    resolver: &impl PathResolver,
    _program: &Path,
) -> io::Result<(PathBuf, PathBuf, PathBuf)> {
    let home = || {
        nonempty_env(resolver, "HOME").ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::NotFound,
                "HOME is required when an XDG root is unset",
            )
        })
    };

    let config = match nonempty_env(resolver, "XDG_CONFIG_HOME") {
        Some(root) => root,
        None => home()?.join(".config"),
    }
    .join("vosu");
    let data = match nonempty_env(resolver, "XDG_DATA_HOME") {
        Some(root) => root,
        None => home()?.join(".local").join("share"),
    }
    .join("vosu");
    let cache = match nonempty_env(resolver, "XDG_CACHE_HOME") {
        Some(root) => root,
        None => home()?.join(".cache"),
    }
    .join("vosu");

    Ok((config, data, cache))
}

pub(super) fn atomic_replace(temp: &Path, target: &Path) -> io::Result<()> {
    std::fs::rename(temp, target)?;
    if let Some(parent) = target.parent() {
        std::fs::File::open(parent)?.sync_all()?;
    }
    Ok(())
}
