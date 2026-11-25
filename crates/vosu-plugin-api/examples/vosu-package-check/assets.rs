//! Bounded reads of the checker's own inputs, and package-file access
//! through the SDK's one package-path rule (`manifest::files`): the same
//! portable-component, no-link/no-reparse-point and bounded UTF-8 rules
//! vosu's installer applies, so a package cannot point the checker at bytes
//! outside its own tree or pass a path the host refuses.

use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use vosu_plugin_api::manifest::files;

/// Reads at most `limit` bytes of UTF-8 text from `path` (the catalog or
/// registry-context file named on the command line).
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
    files::read_file(root, relative, limit).map_err(|e| e.to_string())
}

/// Resolves `relative` inside `root` to an existing regular file.
pub fn confined(root: &Path, relative: &str) -> Result<PathBuf, String> {
    files::confined_file(root, relative).map_err(|e| e.to_string())
}

/// Resolves the extracted package directory `<root>/<components...>`; each
/// component (array name, catalog id, target) must itself be a portable
/// single path segment naming a real directory.
pub fn package_root(root: &Path, components: &[&str]) -> Result<PathBuf, String> {
    let meta = std::fs::symlink_metadata(root)
        .map_err(|e| format!("--packages-root {}: {e}", root.display()))?;
    if !meta.file_type().is_dir() {
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
    files::confined_dir(root, &components.join("/"))
        .map_err(|e| format!("extracted package missing or unsafe: {e}"))
}
