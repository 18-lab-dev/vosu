//! Package files in an extracted package tree: the portable path-component
//! rule, confined resolution of a package-relative path and bounded UTF-8
//! reads.
//!
//! These are the one copy of each rule. vosu's archive extractor names an
//! entry portable through [`portable_component`], its catalog installer
//! reads every manifest-declared asset through [`read_file`], and
//! `vosu-package-check` resolves the same paths the same way, so the
//! checker cannot pass a package path the host refuses.

use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Upper bound on a package's root `manifest.toml`.
pub const MAX_MANIFEST_BYTES: usize = 256 * 1024;

/// Why a package file could not be resolved or read. Every variant names
/// only the package-relative path, never file contents.
#[derive(Debug, thiserror::Error)]
pub enum PackageFileError {
    /// Not a portable relative path, or some component is missing, a
    /// link/reparse point, or the wrong kind of entry.
    #[error("{0}: not a confined package path")]
    Escapes(String),
    /// The confined file exists but could not be opened.
    #[error("{0}: {1}")]
    Open(String, #[source] std::io::Error),
    /// Larger than the limit, unreadable or not UTF-8.
    #[error("{0}: not UTF-8 text of at most {1} bytes")]
    Invalid(String, usize),
}

/// Windows reserves device names with any extension (`CON.txt`, `com1.json`),
/// including the superscript-digit COM/LPT forms, and strips a trailing dot or
/// space, so `a.` and `a` name one file there but two on Linux. Callers also
/// reject empty, `.` and `..` components and the `\`, `:` and NUL bytes.
pub fn portable_component(component: &str) -> bool {
    if component.ends_with(['.', ' ']) {
        return false;
    }
    let stem = component
        .split('.')
        .next()
        .unwrap_or_default()
        .trim_end_matches(' ')
        .to_ascii_uppercase();
    let device = matches!(
        stem.as_str(),
        "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
    ) || ["COM", "LPT"].iter().any(|prefix| {
        stem.strip_prefix(prefix).is_some_and(|digit| {
            matches!(
                digit,
                "1" | "2"
                    | "3"
                    | "4"
                    | "5"
                    | "6"
                    | "7"
                    | "8"
                    | "9"
                    | "\u{b9}"
                    | "\u{b2}"
                    | "\u{b3}"
            )
        })
    });
    !device
}

/// Resolves the package-relative `relative` inside `root` to an existing
/// regular file whose every component is a real directory (no links,
/// reparse points or `..`).
pub fn confined_file(root: &Path, relative: &str) -> Result<PathBuf, PackageFileError> {
    walk(root, relative, true)
}

/// Resolves the package-relative `relative` inside `root` to an existing
/// real directory, under the same rules as [`confined_file`].
pub fn confined_dir(root: &Path, relative: &str) -> Result<PathBuf, PackageFileError> {
    walk(root, relative, false)
}

/// Reads the confined package file `relative` as UTF-8 text of at most
/// `limit` bytes, never buffering past the limit.
pub fn read_file(root: &Path, relative: &str, limit: usize) -> Result<String, PackageFileError> {
    let path = confined_file(root, relative)?;
    let mut text = String::new();
    File::open(path)
        .map_err(|e| PackageFileError::Open(relative.to_owned(), e))?
        .take(limit as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|_| PackageFileError::Invalid(relative.to_owned(), limit))?;
    if text.len() > limit {
        return Err(PackageFileError::Invalid(relative.to_owned(), limit));
    }
    Ok(text)
}

fn walk(root: &Path, relative: &str, file: bool) -> Result<PathBuf, PackageFileError> {
    let escape = || PackageFileError::Escapes(relative.to_owned());
    if relative.is_empty()
        || relative.contains(['\\', ':', '\0'])
        || relative.starts_with('/')
        || relative
            .split('/')
            .any(|part| matches!(part, "" | "." | "..") || !portable_component(part))
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
            || (last && file && !kind.is_file())
            || ((!last || !file) && !kind.is_dir())
        {
            return Err(escape());
        }
    }
    Ok(path)
}

#[cfg(windows)]
fn reparse_point(meta: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn reparse_point(_: &Metadata) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn portable_components_exclude_windows_device_names_and_stripped_suffixes() {
        for ok in [
            "a",
            "a.json",
            "COM0",
            "LPT10",
            "console",
            "con_x.json",
            "é🦀",
        ] {
            assert!(portable_component(ok), "{ok}");
        }
        for bad in [
            "con",
            "CON.txt",
            "con .json",
            "nul",
            "aux.x",
            "prn",
            "conin$",
            "CONOUT$.log",
            "com1",
            "LPT9.json",
            "COM\u{b9}",
            "lpt\u{b3}",
            "a.",
            "a ",
        ] {
            assert!(!portable_component(bad), "{bad}");
        }
    }

    #[test]
    fn confinement_rejects_non_portable_paths_before_touching_the_tree() {
        let root = Path::new("/nonexistent-vosu-package-root");
        for path in [
            "",
            "/abs",
            "../a.json",
            "a/../b",
            "./a",
            "a//b",
            "a\\b",
            "c:a",
            "a\0b",
            "con.json",
            "dir/aux",
        ] {
            assert!(
                matches!(confined_file(root, path), Err(PackageFileError::Escapes(_))),
                "{path:?}"
            );
        }
    }
}
