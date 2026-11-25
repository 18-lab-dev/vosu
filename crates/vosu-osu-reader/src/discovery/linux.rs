//! Resolves the osu! client's data directory on Linux without invoking
//! another process.
//!
//! Duplicates a small private `/proc` text-file reader also used by
//! `vosu_sys::process::linux` (`backend`, bounded reads, NUL-separated
//! argv) rather than sharing it: the pointer-width process-memory boundary
//! and this osu!-specific discovery input are separate crates, and both
//! read the same `/proc` files independently.

use std::ffi::{OsStr, OsString};
use std::fs;
use std::io;
use std::os::unix::ffi::{OsStrExt, OsStringExt};
use std::path::{Path, PathBuf};

use vosu_plugin_api::memory::MemoryError;

const MAX_PROC_FILE: u64 = 1024 * 1024;

/// Resolves the target's osu! data directory without invoking another process.
pub fn resolve_osu_path(pid: u32) -> Result<PathBuf, MemoryError> {
    let environment: Vec<_> = std::env::vars_os().collect();
    resolve_osu_path_at(Path::new("/proc"), pid, &environment)
}

fn resolve_osu_path_at(
    proc_root: &Path,
    pid: u32,
    env: &[(OsString, OsString)],
) -> Result<PathBuf, MemoryError> {
    let env_value = |key: &str| {
        env.iter()
            .find(|(name, _)| name == OsStr::new(key))
            .map(|(_, value)| value)
    };
    if let Some(path) = env_value("VOSU_OSU_PATH").filter(|value| !value.is_empty()) {
        return Ok(PathBuf::from(path));
    }
    let dir = proc_root.join(pid.to_string());
    let executable =
        fs::read_link(dir.join("exe")).map_err(|error| backend("read /proc exe", error))?;
    if executable
        .as_os_str()
        .as_bytes()
        .windows(b"wine-preloader".len())
        .any(|part| part == b"wine-preloader")
    {
        let data_home = env_value("XDG_DATA_HOME")
            .map(PathBuf::from)
            .or_else(|| env_value("HOME").map(|home| PathBuf::from(home).join(".local/share")));
        if let Some(config) = data_home.map(|path| path.join("osuconfig/osupath"))
            && config.is_file()
        {
            let bytes =
                read_bounded(&config).map_err(|error| backend("read osuconfig/osupath", error))?;
            return Ok(PathBuf::from(OsString::from_vec(
                trim_ascii(&bytes).to_vec(),
            )));
        }
        for argument in command_line_at(&dir)?.into_iter().skip(1) {
            let bytes = argument.as_bytes();
            if bytes.to_ascii_lowercase().ends_with(b"osu!.exe") {
                let converted: Vec<u8> = bytes
                    .iter()
                    .map(|byte| if *byte == b'\\' { b'/' } else { *byte })
                    .collect();
                let without_drive = if converted.get(1) == Some(&b':') {
                    &converted[2..]
                } else {
                    &converted
                };
                let path = PathBuf::from(OsString::from_vec(without_drive.to_vec()));
                return Ok(path.parent().unwrap_or(Path::new("/")).to_path_buf());
            }
        }
    }
    fs::read_link(dir.join("cwd")).map_err(|error| backend("read /proc cwd", error))
}

fn command_line_at(dir: &Path) -> Result<Vec<OsString>, MemoryError> {
    let bytes =
        read_bounded(&dir.join("cmdline")).map_err(|error| backend("read /proc cmdline", error))?;
    Ok(bytes
        .split(|byte| *byte == 0)
        .filter(|part| !part.is_empty())
        .map(|part| OsString::from_vec(part.to_vec()))
        .collect())
}

fn read_bounded(path: &Path) -> io::Result<Vec<u8>> {
    let file = fs::File::open(path)?;
    let length = file.metadata()?.len();
    if length > MAX_PROC_FILE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "proc file exceeds 1 MiB",
        ));
    }
    use std::io::Read;
    let mut bytes = Vec::with_capacity(length as usize);
    file.take(MAX_PROC_FILE + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_PROC_FILE {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "proc file exceeds 1 MiB",
        ));
    }
    Ok(bytes)
}

fn trim_ascii(bytes: &[u8]) -> &[u8] {
    let start = bytes
        .iter()
        .position(|byte| !byte.is_ascii_whitespace())
        .unwrap_or(bytes.len());
    let end = bytes
        .iter()
        .rposition(|byte| !byte.is_ascii_whitespace())
        .map_or(start, |index| index + 1);
    &bytes[start..end]
}

fn backend(operation: &'static str, error: io::Error) -> MemoryError {
    MemoryError::Backend {
        operation,
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests;
