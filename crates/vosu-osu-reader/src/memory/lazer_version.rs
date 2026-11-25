//! Strict osu!lazer version discovery from the target installation.

use std::fs::OpenOptions;
use std::io::{self, Read};
#[cfg(target_os = "linux")]
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};

use indexmap::IndexMap;
use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;

use super::lazer_offsets::{LazerOffsetsError, validate_version};

const MAX_DEPS_BYTES: u64 = 4 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum LazerVersionError {
    #[error("can't read osu dependencies: {0}")]
    Io(#[from] io::Error),
    #[error("can't read osu dependencies: {0}")]
    Json(#[from] serde_json::Error),
    #[error("osu dependencies do not contain a canonical osu! version")]
    Missing,
    #[error(transparent)]
    Invalid(#[from] LazerOffsetsError),
    #[error("target process has no executable root")]
    MissingProcessRoot,
}

#[derive(Deserialize)]
struct Dependencies {
    libraries: IndexMap<String, Value>,
}

/// Extracts the first insertion-ordered `osu!/<x.y.z>-...` dependency.
pub fn version_from_deps(bytes: &[u8]) -> Result<String, LazerVersionError> {
    let dependencies: Dependencies = serde_json::from_slice(bytes)?;
    for key in dependencies.libraries.keys() {
        let Some(rest) = key.strip_prefix("osu!/") else {
            continue;
        };
        let Some((version, suffix)) = rest.split_once('-') else {
            continue;
        };
        if suffix.is_empty() {
            continue;
        }
        validate_version(version)?;
        return Ok(version.to_owned());
    }
    Err(LazerVersionError::Missing)
}

pub fn deps_path(executable_root: &Path) -> PathBuf {
    executable_root.join("osu!.deps.json")
}

pub fn discover_from_root(executable_root: &Path) -> Result<String, LazerVersionError> {
    let path = deps_path(executable_root);
    let mut options = OpenOptions::new();
    options.read(true);
    #[cfg(target_os = "linux")]
    options.custom_flags(libc::O_NONBLOCK | libc::O_CLOEXEC);
    let mut file = options.open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "osu!.deps.json is not a regular file",
        )
        .into());
    }
    let length = metadata.len();
    if length > MAX_DEPS_BYTES {
        return Err(
            io::Error::new(io::ErrorKind::InvalidData, "osu!.deps.json exceeds 4 MiB").into(),
        );
    }
    let mut bytes = Vec::with_capacity(length as usize);
    file.by_ref()
        .take(MAX_DEPS_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_DEPS_BYTES {
        return Err(
            io::Error::new(io::ErrorKind::InvalidData, "osu!.deps.json exceeds 4 MiB").into(),
        );
    }
    version_from_deps(&bytes)
}

/// The target's executable directory as the target itself sees it, derived
/// only from core's authorized attach facts (plugin-abi.md §6.2): the
/// session's `pid` and the directory of its `SESSION_ATTACHED` `path`.
/// No process handle, module enumeration or `/proc/<pid>/exe` query runs
/// here (M5 R1); the plugin reads ordinary files under the returned path.
///
/// On Linux this is that directory below `/proc/<pid>/root`, so a
/// sandboxed (Flatpak/AppImage) lazer's own filesystem view is read, as the
/// 1.x `process_root` did. On Windows it is the directory itself, which is
/// the main module's directory the 1.x loader-table lookup returned.
/// An empty or relative directory (an absent `path` fact) is
/// [`LazerVersionError::MissingProcessRoot`], never a cwd-relative read.
pub fn process_executable_root(
    pid: u32,
    executable_dir: &Path,
) -> Result<PathBuf, LazerVersionError> {
    if !executable_dir.is_absolute() {
        return Err(LazerVersionError::MissingProcessRoot);
    }
    #[cfg(target_os = "linux")]
    {
        let relative = executable_dir
            .strip_prefix("/")
            .map_err(|_| LazerVersionError::MissingProcessRoot)?;
        Ok(Path::new("/proc")
            .join(pid.to_string())
            .join("root")
            .join(relative))
    }
    #[cfg(not(target_os = "linux"))]
    {
        let _ = pid;
        Ok(executable_dir.to_path_buf())
    }
}

pub fn discover_for_process(pid: u32, executable_dir: &Path) -> Result<String, LazerVersionError> {
    discover_from_root(&process_executable_root(pid, executable_dir)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_first_matching_dependency_in_insertion_order() {
        let deps = br#"{"libraries":{"other/1.0.0":{},"osu!/2026.711.0-lazer":{},"osu!/2026.518.0-tachyon":{}}}"#;
        assert_eq!(version_from_deps(deps).unwrap(), "2026.711.0");
    }

    #[test]
    fn rejects_malformed_dependency_shapes_and_versions() {
        for deps in [
            br#"{}"#.as_slice(),
            br#"{"libraries":null}"#,
            br#"{"libraries":[]}"#,
            br#"{"libraries":{"Osu!/2026.711.0-lazer":{}}}"#,
            br#"{"libraries":{"osu!/2026.711-lazer":{}}}"#,
            br#"{"libraries":{"osu!/2026.0711.0-lazer":{}}}"#,
            br#"{"libraries":{"osu!/2026.711.0":{}}}"#,
            br#"{"libraries":{"osu!/2026.711.0-":{}}}"#,
            br#"{"libraries":{"osu!/../../x-lazer":{}}}"#,
        ] {
            assert!(
                version_from_deps(deps).is_err(),
                "fixture should fail: {}",
                String::from_utf8_lossy(deps)
            );
        }
    }

    #[test]
    fn discovers_from_explicit_executable_root() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("osu!.deps.json"),
            br#"{"libraries":{"osu!/2026.518.0-tachyon":{}}}"#,
        )
        .unwrap();
        assert_eq!(discover_from_root(root.path()).unwrap(), "2026.518.0");
    }

    /// The executable root comes from the attach `path` directory and the
    /// pid alone; on Linux it reads through the target's own root view.
    #[test]
    fn discovers_for_process_through_the_attach_path_fact() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(
            root.path().join("osu!.deps.json"),
            br#"{"libraries":{"osu!/2026.711.0-lazer":{}}}"#,
        )
        .unwrap();
        let pid = std::process::id();
        let resolved = process_executable_root(pid, root.path()).unwrap();
        #[cfg(target_os = "linux")]
        assert_eq!(
            resolved,
            Path::new("/proc")
                .join(pid.to_string())
                .join("root")
                .join(root.path().strip_prefix("/").unwrap())
        );
        #[cfg(not(target_os = "linux"))]
        assert_eq!(resolved, root.path());
        assert_eq!(
            discover_for_process(pid, root.path()).unwrap(),
            "2026.711.0"
        );
    }

    /// Moved from `vosu-sys`'s deleted `root_for` test (M5 R1): an AppImage
    /// mount directory resolves relative to the target's own `/proc` root.
    #[cfg(target_os = "linux")]
    #[test]
    fn resolves_an_appimage_mount_relative_to_the_target_proc_root() {
        assert_eq!(
            process_executable_root(30, Path::new("/tmp/.mount_osu/usr/bin")).unwrap(),
            Path::new("/proc/30/root/tmp/.mount_osu/usr/bin"),
        );
    }

    /// Counterexample "executable-root fallback selects another
    /// installation": an absent (empty) or relative `path` fact never
    /// becomes a cwd-relative read.
    #[test]
    fn missing_or_relative_attach_path_is_missing_process_root() {
        for dir in [Path::new(""), Path::new("osu"), Path::new("./lazer")] {
            assert!(matches!(
                process_executable_root(std::process::id(), dir),
                Err(LazerVersionError::MissingProcessRoot)
            ));
            assert!(matches!(
                discover_for_process(std::process::id(), dir),
                Err(LazerVersionError::MissingProcessRoot)
            ));
        }
    }

    #[test]
    fn rejects_non_regular_dependency_file() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("osu!.deps.json")).unwrap();
        assert!(discover_from_root(root.path()).is_err());
    }

    #[cfg(target_os = "linux")]
    #[test]
    fn rejects_dependency_fifo_without_waiting_for_a_writer() {
        use std::os::unix::ffi::OsStrExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("osu!.deps.json");
        let name = std::ffi::CString::new(path.as_os_str().as_bytes()).unwrap();
        // SAFETY: name is a valid NUL-terminated pathname.
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o600) }, 0);
        assert!(discover_from_root(root.path()).is_err());
    }
}
