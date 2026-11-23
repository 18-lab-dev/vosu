//! Windows polling [`ProcessSource`] (`docs/v2/implementation-plan.md` M1
//! §4, phase d2b, unit c): full system enumeration plus a per-pid
//! bitness/command-line read every [`super::RECONCILE_INTERVAL`] tick —
//! no live event channel exists on Windows, so [`super::ProcessSource`]'s
//! defaults (`drain_events`, `notifier`) already do the right thing and
//! this module implements only [`super::ProcessSource::enumerate`],
//! mirroring 1.x's Windows backend
//! (`src/instance/discovery.rs`'s `NativeProcessProvider::enumerate`):
//! `vosu_sys::process::enumerate_processes` for the raw OS list, then
//! `vosu_sys::process::{process_bitness, process_command_line}` per pid,
//! silently skipping (not erroring the whole pass) a pid that exited
//! mid-enumeration or whose handle this process cannot open (a protected
//! system process) — 1.x's own error handling for exactly those calls
//! (`let Ok(..) = .. else { continue; }`).
//!
//! Unlike 1.x, this backend does not pre-filter by candidate executable
//! name (`is_windows_candidate_name`, osu!-specific) before opening each
//! process: the generic [`super::DiscoveryService`] doc comment on
//! [`super::ProcessSource::enumerate`] specifies an unfiltered Windows
//! source and leaves narrowing to each plugin's own `[match]` table in
//! [`super::host::PluginHost::process_appeared`] (`docs/v2/plugin-abi.md`
//! §6.1) — there is no per-platform hint sink to narrow against yet
//! (`super::ProcessSource::set_comm_hints` is Linux-only today). This
//! also deliberately skips 1.x's `windows_metadata_cache`
//! (`CachedWindowsMetadata`, keyed by `creation_time()`): that cache's
//! safety depends on `creation_time()` to detect pid reuse before serving
//! stale bitness/command-line data for the new occurrence, and that
//! function is outside this unit's listed surface
//! (`enumerate_processes`/`process_bitness`/`process_command_line`).
//! Re-reading every live process every tick is the simple, always-correct
//! alternative; a `creation_time`-backed cache is a future optimization,
//! not required for M1.
//!
//! The OS-touching backend below is `#[cfg(windows)]`; the pure data
//! transform ([`executable_path_from_command_line`], [`build_record`]) is
//! `#[cfg(any(windows, test))]` instead, so it is also compiled and unit
//! tested on Linux (this module's own `tests`) without needing a caller
//! on every other target.

#[cfg(any(windows, test))]
use vosu_plugin_api::Bitness;

#[cfg(any(windows, test))]
use super::super::host::ProcessRecord;

/// Extracts the executable path (argv[0]) from a Windows command line —
/// `CreateProcess`'s quoting rules: a leading `"..."` run is the whole
/// path even if it contains spaces, otherwise the path is the first
/// whitespace-delimited token. Mirrors 1.x's `windows_instance_root`
/// (`src/instance/discovery.rs`), minus the final `.parent()` step: this
/// needs the full executable path, not just its containing directory.
/// Returns `None` for an empty or whitespace-only command line, so the
/// caller can fall back to the raw process-list name instead of reporting
/// an empty path.
/// Compiled for `#[cfg(windows)]` (the real backend below) and `test`
/// (this module's own `tests`, run on Linux): a non-Windows, non-test
/// build has no caller, which `-D warnings`' `dead_code` lint correctly
/// rejects.
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn executable_path_from_command_line(command_line: &str) -> Option<String> {
    let trimmed = command_line.trim_start();
    let path = if let Some(rest) = trimmed.strip_prefix('"') {
        rest.split_once('"').map_or(rest, |(path, _)| path)
    } else {
        trimmed.split_whitespace().next().unwrap_or_default()
    };
    (!path.is_empty()).then(|| path.to_owned())
}

/// Builds a [`ProcessRecord`] from one Windows process's raw parts: pure
/// (no syscalls). `entry_name` is the raw OS process-list name
/// (`vosu_sys::process::ProcessEntry::executable`) — already exactly
/// [`ProcessRecord::name`]'s own contract ("the executable file name
/// only, no directory"): `NtQuerySystemInformation`'s `ImageName` is a
/// bare file name, never a path, so this needs no further parsing (and
/// deliberately does none: extracting a file name from `path` with
/// `std::path::Path` would silently use `/`-only splitting when this
/// pure function is compiled for its `test` cfg on a non-Windows host,
/// giving a wrong answer there that a Windows build would not
/// reproduce). `entry_name` is also the fallback for
/// [`ProcessRecord::path`] when `command_line` does not parse to a
/// usable executable path (an empty command line, or one
/// `CreateProcess` never populated). Same `#[cfg(any(windows, test))]`
/// as [`executable_path_from_command_line`] and for the same reason.
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn build_record(
    pid: u32,
    entry_name: &str,
    bitness: Bitness,
    command_line: String,
) -> ProcessRecord {
    let path =
        executable_path_from_command_line(&command_line).unwrap_or_else(|| entry_name.to_owned());
    ProcessRecord {
        pid,
        name: entry_name.to_owned(),
        path,
        command_line,
        bitness,
    }
}

#[cfg(windows)]
mod backend {
    use std::io;

    use vosu_plugin_api::Bitness;
    use vosu_sys::process::{
        MemoryError, ProcessBitness, enumerate_processes, process_bitness, process_command_line,
    };

    use super::super::super::host::ProcessRecord;
    use super::super::ProcessSource;
    use super::build_record;

    fn convert_bitness(bitness: ProcessBitness) -> Bitness {
        match bitness {
            ProcessBitness::X86 => Bitness::X86,
            ProcessBitness::X64 => Bitness::X64,
        }
    }

    fn to_io_error(error: MemoryError) -> io::Error {
        io::Error::other(error)
    }

    /// `#[cfg(windows)]`, compile-checked only through `cargo xwin`
    /// (`AGENTS.md`: no native Windows run is possible here, the Windows
    /// VM being down) — unverified natively: every call into
    /// `vosu_sys::process` below (`enumerate_processes`,
    /// `process_bitness`, `process_command_line`), and in particular
    /// whether the access-denied/exited-mid-enumeration `continue` below
    /// actually keeps the pass going on a real Windows host the way 1.x's
    /// equivalent does.
    #[derive(Debug, Default)]
    pub struct WindowsProcessSource;

    impl ProcessSource for WindowsProcessSource {
        fn enumerate(&self) -> io::Result<Vec<ProcessRecord>> {
            let entries = enumerate_processes().map_err(to_io_error)?;
            let mut records = Vec::with_capacity(entries.len());
            for entry in entries {
                // A process that exited between `enumerate_processes` and
                // these per-pid reads, or one this process has no
                // permission to open (a protected system process), is
                // skipped rather than failing the whole pass — 1.x's same
                // `let Ok(..) = .. else { continue; }` error handling
                // (`src/instance/discovery.rs`'s
                // `NativeProcessProvider::enumerate`).
                let Ok(bitness) = process_bitness(entry.pid) else {
                    continue;
                };
                let Ok(command_line) = process_command_line(entry.pid) else {
                    continue;
                };
                records.push(build_record(
                    entry.pid,
                    &entry.executable,
                    convert_bitness(bitness),
                    command_line,
                ));
            }
            Ok(records)
        }
    }
}

#[cfg(windows)]
pub use backend::WindowsProcessSource;

#[cfg(test)]
mod tests {
    use vosu_plugin_api::Bitness;

    use super::{build_record, executable_path_from_command_line};

    #[test]
    fn parses_quoted_path_with_spaces() {
        let command_line = r#""C:\Games\osu!\osu!.exe" -nosplash"#;
        assert_eq!(
            executable_path_from_command_line(command_line).as_deref(),
            Some(r"C:\Games\osu!\osu!.exe")
        );
    }

    #[test]
    fn parses_unquoted_path_without_arguments() {
        assert_eq!(
            executable_path_from_command_line(r"C:\Games\osu!\osu!.exe").as_deref(),
            Some(r"C:\Games\osu!\osu!.exe")
        );
    }

    #[test]
    fn parses_unquoted_path_with_arguments() {
        assert_eq!(
            executable_path_from_command_line(r"C:\Games\osu!\osu!.exe -devserver x").as_deref(),
            Some(r"C:\Games\osu!\osu!.exe")
        );
    }

    #[test]
    fn empty_command_line_has_no_path() {
        assert_eq!(executable_path_from_command_line(""), None);
        assert_eq!(executable_path_from_command_line("   "), None);
    }

    #[test]
    fn unterminated_quote_falls_back_to_the_rest_of_the_line() {
        // `CreateProcess` never produces this, but a hostile or corrupt
        // `GetCommandLine` read must not panic: `split_once` returns
        // `None`, so the whole remainder after the opening quote is used.
        assert_eq!(
            executable_path_from_command_line(r#""C:\Games\osu!\osu!.exe"#).as_deref(),
            Some(r"C:\Games\osu!\osu!.exe")
        );
    }

    #[test]
    fn build_record_uses_parsed_path_and_entry_name() {
        let record = build_record(
            4242,
            "osu!.exe",
            Bitness::X64,
            r#""C:\Games\osu!\osu!.exe" -nosplash"#.to_owned(),
        );
        assert_eq!(record.pid, 4242);
        assert_eq!(record.path, r"C:\Games\osu!\osu!.exe");
        assert_eq!(record.name, "osu!.exe");
        assert_eq!(record.bitness, Bitness::X64);
        assert_eq!(record.command_line, r#""C:\Games\osu!\osu!.exe" -nosplash"#);
    }

    #[test]
    fn build_record_falls_back_to_entry_name_without_a_parseable_command_line() {
        let record = build_record(7, "svchost.exe", Bitness::X86, String::new());
        assert_eq!(record.path, "svchost.exe");
        assert_eq!(record.name, "svchost.exe");
    }
}
