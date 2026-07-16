//! Strict osu!lazer version discovery from the target installation.

use std::fs::File;
use std::io::{self, Read};
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
    let mut file = File::open(path)?;
    let length = file.metadata()?.len();
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

pub fn process_executable_root(pid: u32) -> Result<PathBuf, LazerVersionError> {
    #[cfg(target_os = "linux")]
    {
        crate::process::process_root(pid)
            .map_err(|error| io::Error::other(error.to_string()).into())
    }
    #[cfg(windows)]
    {
        let module = crate::process::enumerate_modules(pid)
            .map_err(|error| io::Error::other(error.to_string()))?
            .into_iter()
            .next()
            .ok_or(LazerVersionError::MissingProcessRoot)?;
        PathBuf::from(module.path)
            .parent()
            .map(Path::to_path_buf)
            .ok_or(LazerVersionError::MissingProcessRoot)
    }
}

pub fn discover_for_process(pid: u32) -> Result<String, LazerVersionError> {
    discover_from_root(&process_executable_root(pid)?)
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
}
