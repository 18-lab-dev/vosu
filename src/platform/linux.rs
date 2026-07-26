//! Linux-native platform integration.

mod capabilities;

use std::io::{self, Read};
use std::path::{Path, PathBuf};

use super::{PathResolver, nonempty_env};

const PROC_SELF_STATUS: &str = "/proc/self/status";
const CAP_SYS_PTRACE_BIT: u32 = 19;

pub(super) fn cap_sys_ptrace_effective() -> io::Result<bool> {
    read_cap_sys_ptrace_effective(std::fs::File::open(PROC_SELF_STATUS)?)
}

fn read_cap_sys_ptrace_effective(mut reader: impl Read) -> io::Result<bool> {
    let mut status = String::new();
    reader.read_to_string(&mut status)?;
    parse_cap_sys_ptrace_effective(&status)
}

fn parse_cap_sys_ptrace_effective(status: &str) -> io::Result<bool> {
    let cap_eff = status
        .lines()
        .find_map(|line| line.strip_prefix("CapEff:"))
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "CapEff is missing"))?
        .trim();
    let capabilities = u64::from_str_radix(cap_eff, 16).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("CapEff is not hexadecimal: {error}"),
        )
    })?;

    Ok(capabilities & (1_u64 << CAP_SYS_PTRACE_BIT) != 0)
}

pub(super) fn install_supported_capabilities(executable: &Path) -> io::Result<()> {
    capabilities::install(executable)
}

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

#[cfg(test)]
mod tests {
    use super::{parse_cap_sys_ptrace_effective, read_cap_sys_ptrace_effective};
    use std::io;

    struct UnavailableReader;

    impl io::Read for UnavailableReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::other("unavailable"))
        }
    }

    #[test]
    fn parser_reports_effective_cap_sys_ptrace_when_bit_19_is_set() {
        let status = "Name:\tvosu\nCapEff:\t0000000000080000\n";

        assert!(parse_cap_sys_ptrace_effective(status).expect("parse CapEff"));
    }

    #[test]
    fn parser_reports_missing_cap_sys_ptrace_when_bit_19_is_clear() {
        let status = "Name:\tvosu\nCapEff:\t0000000000000000\n";

        assert!(!parse_cap_sys_ptrace_effective(status).expect("parse CapEff"));
    }

    #[test]
    fn parser_rejects_malformed_cap_eff() {
        let error = parse_cap_sys_ptrace_effective("CapEff:\tnot-hex\n")
            .expect_err("malformed CapEff must fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn parser_rejects_status_without_cap_eff() {
        let error =
            parse_cap_sys_ptrace_effective("Name:\tvosu\n").expect_err("missing CapEff must fail");

        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn reader_propagates_unavailable_status() {
        let error = read_cap_sys_ptrace_effective(UnavailableReader)
            .expect_err("unavailable status must fail");

        assert_eq!(error.kind(), io::ErrorKind::Other);
    }
}
