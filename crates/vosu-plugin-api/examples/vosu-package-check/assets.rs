//! Confined, bounded reads from an already-extracted package tree. Every
//! package-relative path is a portable `/`-separated relative path whose
//! components are real directories (and a real final file): no `..`,
//! absolute or drive paths, symlinks or Windows reparse points, so a
//! package cannot point the checker at bytes outside its own tree.

use std::fs::{File, Metadata};
use std::io::Read;
use std::path::{Path, PathBuf};

/// Reads at most `limit` bytes of UTF-8 text from `path`.
pub fn read_bounded(path: &Path, limit: usize) -> Result<String, String> {
    let mut text = String::new();
    File::open(path)
        .map_err(|e| e.to_string())?
        .take(limit as u64 + 1)
        .read_to_string(&mut text)
        .map_err(|e| e.to_string())?;
    if text.len() > limit {
        return Err(format!("larger than {limit} bytes"));
    }
    Ok(text)
}

/// Reads the confined package file `relative` (at most `limit` bytes).
pub fn read_asset(root: &Path, relative: &str, limit: usize) -> Result<String, String> {
    let path = confined(root, relative)?;
    read_bounded(&path, limit).map_err(|e| format!("{relative}: {e}"))
}

/// Resolves `relative` inside `root` to an existing regular file.
pub fn confined(root: &Path, relative: &str) -> Result<PathBuf, String> {
    walk(root, relative, true)
}

/// Resolves the extracted package directory `<root>/<components...>`; each
/// component (array name, catalog id, target) must itself be a portable
/// single path segment naming a real directory.
pub fn package_root(root: &Path, components: &[&str]) -> Result<PathBuf, String> {
    let meta = std::fs::symlink_metadata(root)
        .map_err(|e| format!("--packages-root {}: {e}", root.display()))?;
    if !plain_dir(&meta) {
        return Err(format!(
            "--packages-root {} is not a real directory",
            root.display()
        ));
    }
    if components.iter().any(|c| c.contains('/')) {
        return Err(format!(
            "unsafe package directory {:?}",
            components.join("/")
        ));
    }
    walk(root, &components.join("/"), false)
        .map_err(|e| format!("extracted package missing or unsafe: {e}"))
}

fn walk(root: &Path, relative: &str, file: bool) -> Result<PathBuf, String> {
    let escape = || format!("{relative}: not a confined package path");
    if relative.is_empty()
        || relative.contains(['\\', ':', '\0'])
        || relative.starts_with('/')
        || relative.split('/').any(|part| !portable_component(part))
    {
        return Err(escape());
    }
    let mut path = root.to_path_buf();
    let mut parts = relative.split('/').peekable();
    while let Some(part) = parts.next() {
        path.push(part);
        let meta = std::fs::symlink_metadata(&path).map_err(|_| escape())?;
        let last = parts.peek().is_none();
        let wanted = if last && file {
            meta.file_type().is_file() && !reparse_point(&meta)
        } else {
            plain_dir(&meta)
        };
        if !wanted {
            return Err(escape());
        }
    }
    Ok(path)
}

fn plain_dir(meta: &Metadata) -> bool {
    meta.file_type().is_dir() && !reparse_point(meta)
}

/// A component every supported OS stores and names the same way.
fn portable_component(part: &str) -> bool {
    if matches!(part, "" | "." | "..") || part.ends_with(['.', ' ']) {
        return false;
    }
    if part
        .chars()
        .any(|c| c.is_control() || matches!(c, '<' | '>' | '"' | '|' | '?' | '*'))
    {
        return false;
    }
    let stem = part.split('.').next().unwrap_or(part).to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || ((stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.len() == 4
            && stem.as_bytes()[3].is_ascii_digit());
    !reserved
}

#[cfg(windows)]
fn reparse_point(meta: &Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
    meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0
}

#[cfg(not(windows))]
fn reparse_point(meta: &Metadata) -> bool {
    meta.file_type().is_symlink()
}
