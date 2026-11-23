//! Windows polling [`ProcessSource`] (`docs/v2/implementation-plan.md` M1
//! §4, phase d2b, unit c): full system enumeration every
//! [`super::RECONCILE_INTERVAL`] tick — no live event channel exists on
//! Windows, so [`super::ProcessSource`]'s defaults (`drain_events`,
//! `notifier`) already do the right thing and this module implements only
//! [`super::ProcessSource::enumerate`]/[`super::ProcessSource::set_name_prefilter`].
//!
//! **Enumeration-cost pre-filter** (this unit's own addition over the
//! first cut landed in d4e0a07): 1.x narrowed its Windows pass with an
//! osu!-specific `is_windows_candidate_name` before opening each process.
//! The generic core has no such plugin, so [`WindowsSource::enumerate`]
//! instead consults [`NamePrefilter`] — the union of every registered
//! plugin's `process_names` (`MatcherEngine::name_prefilter`, §6.1),
//! pushed down by [`super::DiscoveryService`]'s own `run_pass` through
//! [`super::ProcessSource::set_name_prefilter`] exactly like the Linux
//! comm-hint sync — and skips
//! `vosu_sys::process::{process_bitness, process_command_line}`
//! (the "open" calls) for a pid whose bare executable name the filter
//! rejects. Case rule: [`NamePrefilter::allows`] is always evaluated
//! against [`MatchPlatform::Windows`] here (§6.1's case-insensitive name
//! rule), independent of the host this file happens to be compiled on.
//!
//! **Unfiltered fallback**: a plugin whose `[match]` table matches only
//! by `process_path_patterns`/`command_line_patterns` can accept a
//! process whose executable name is nowhere in any `process_names` list,
//! so narrowing by name would silently hide it. `MatcherEngine` already
//! encodes this: the moment *any* registered plugin is not
//! `names_only`, `name_prefilter()` returns [`NamePrefilter::Unfiltered`]
//! for the whole set (not just that one plugin), and
//! [`WindowsSource::enumerate`] opens every process again, exactly as
//! before this unit.
//!
//! This also still deliberately skips 1.x's `windows_metadata_cache`
//! (`CachedWindowsMetadata`, keyed by `creation_time()`): that cache's
//! safety depends on `creation_time()` to detect pid reuse before serving
//! stale bitness/command-line data for the new occurrence, and that
//! function is outside this unit's listed surface
//! (`enumerate_processes`/`process_bitness`/`process_command_line`).
//! Re-reading every still-admitted process every tick is the simple,
//! always-correct alternative; a `creation_time`-backed cache is a future
//! optimization, not required for M1. Discovery's own
//! `discovery.rs::apply_appeared` already handles pid reuse correctly at
//! the `ProcessRecord` level (a changed name/path/command_line/bitness is
//! reported as an exit immediately followed by a fresh appearance); this
//! source only needs to make sure a reused pid whose *new* occurrence no
//! longer matches the pre-filter stops being reported at all, which
//! `tests::pid_reuse_with_a_name_change_drops_the_stale_entry`
//! below proves directly.
//!
//! The [`ProcessOps`] seam (real Win32 calls vs. a synthetic recording
//! implementation) makes the skip-on-non-match and pid-reuse behavior
//! unit-testable on any host: [`WindowsSource`]/[`ProcessOps`]/
//! [`RawEntry`] compile under `#[cfg(any(windows, test))]`, [`RealOps`]
//! (the only piece that actually calls into `vosu_sys::process`) stays
//! `#[cfg(windows)]`-only, compile-checked exclusively through
//! `cargo xwin clippy` here (`AGENTS.md`: the Windows VM is down, so
//! nothing under `#[cfg(windows)]` in this file runs natively in this
//! environment).

#[cfg(any(windows, test))]
use std::io;
#[cfg(any(windows, test))]
use std::sync::Mutex;

#[cfg(any(windows, test))]
use vosu_plugin_api::Bitness;

#[cfg(any(windows, test))]
use super::super::host::ProcessRecord;
#[cfg(any(windows, test))]
use super::super::matcher::{MatchPlatform, NamePrefilter};
#[cfg(any(windows, test))]
use super::ProcessSource;

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
    if trimmed.is_empty() {
        return None;
    }
    if let Some(rest) = trimmed.strip_prefix('"') {
        let path = rest.split_once('"').map_or(rest, |(path, _)| path);
        return Some(path.to_owned());
    }
    let path = trimmed.split_whitespace().next().unwrap_or(trimmed);
    Some(path.to_owned())
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

/// One raw OS process-list entry [`WindowsSource::enumerate`]'s
/// pre-filter check needs: pid and bare executable name. A local type,
/// not `vosu_sys::process::ProcessEntry` (`#[cfg(windows)]`-only), so the
/// pre-filter skip behavior below is unit-testable on any host.
#[cfg(any(windows, test))]
struct RawEntry {
    pid: u32,
    executable: String,
}

/// Seam over the raw Win32 process calls [`WindowsSource::enumerate`]
/// needs, so "a non-matching name is never opened" (no [`Self::bitness`]/
/// [`Self::command_line`] call for it) and pid-reuse-with-a-name-change
/// behavior are unit-testable on any host. [`RealOps`] is the
/// `#[cfg(windows)]` production implementation; this module's own
/// `tests` use a synthetic recording implementation instead.
#[cfg(any(windows, test))]
trait ProcessOps {
    fn enumerate(&self) -> io::Result<Vec<RawEntry>>;
    fn bitness(&self, pid: u32) -> io::Result<Bitness>;
    fn command_line(&self, pid: u32) -> io::Result<String>;
}

#[cfg(windows)]
use vosu_sys::process::{
    MemoryError, ProcessBitness, enumerate_processes, process_bitness, process_command_line,
};

#[cfg(windows)]
fn convert_bitness(bitness: ProcessBitness) -> Bitness {
    match bitness {
        ProcessBitness::X86 => Bitness::X86,
        ProcessBitness::X64 => Bitness::X64,
    }
}

#[cfg(windows)]
fn to_io_error(error: MemoryError) -> io::Error {
    io::Error::other(error)
}

/// `#[cfg(windows)]`, compile-checked only through `cargo xwin`
/// (`AGENTS.md`: no native Windows run is possible here, the Windows VM
/// being down) — unverified natively: every call into `vosu_sys::process`
/// below (`enumerate_processes`, `process_bitness`,
/// `process_command_line`), and in particular whether the
/// access-denied/exited-mid-enumeration skip in [`WindowsSource::enumerate`]
/// actually keeps the pass going on a real Windows host the way 1.x's
/// equivalent does.
#[cfg(windows)]
#[derive(Debug, Default)]
pub struct RealOps;

#[cfg(windows)]
impl ProcessOps for RealOps {
    fn enumerate(&self) -> io::Result<Vec<RawEntry>> {
        Ok(enumerate_processes()
            .map_err(to_io_error)?
            .into_iter()
            .map(|entry| RawEntry {
                pid: entry.pid,
                executable: entry.executable,
            })
            .collect())
    }

    fn bitness(&self, pid: u32) -> io::Result<Bitness> {
        process_bitness(pid)
            .map(convert_bitness)
            .map_err(to_io_error)
    }

    fn command_line(&self, pid: u32) -> io::Result<String> {
        process_command_line(pid).map_err(to_io_error)
    }
}

/// The [`ProcessSource`] itself: full enumeration through `ops` every
/// [`super::RECONCILE_INTERVAL`] tick, skipping a pid whose bare
/// executable name [`NamePrefilter::allows`] rejects before ever calling
/// [`ProcessOps::bitness`]/[`ProcessOps::command_line`] for it (module
/// doc comment's "enumeration-cost pre-filter"). A pid that *did* pass
/// the pre-filter but fails either read (exited mid-enumeration, access
/// denied) is still skipped rather than failing the whole pass — 1.x's
/// own `let Ok(..) = .. else { continue; }` handling
/// (`src/instance/discovery.rs`'s `NativeProcessProvider::enumerate`).
#[cfg(any(windows, test))]
pub struct WindowsSource<Ops> {
    ops: Ops,
    /// Starts [`NamePrefilter::Unfiltered`] (fail open): the discovery
    /// service always calls [`Self::set_name_prefilter`] (via
    /// `run_pass`'s `sync_name_prefilter`) before its very first
    /// `enumerate()`, so this default is never actually observed in
    /// production — it only avoids wrongly narrowing a source some other
    /// caller enumerates before syncing it.
    prefilter: Mutex<NamePrefilter>,
}

#[cfg(any(windows, test))]
impl<Ops> WindowsSource<Ops> {
    fn new(ops: Ops) -> Self {
        Self {
            ops,
            prefilter: Mutex::new(NamePrefilter::Unfiltered),
        }
    }
}

#[cfg(any(windows, test))]
impl<Ops: ProcessOps + Send + Sync> ProcessSource for WindowsSource<Ops> {
    fn enumerate(&self) -> io::Result<Vec<ProcessRecord>> {
        let filter = self
            .prefilter
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .clone();
        let entries = self.ops.enumerate()?;
        let mut records = Vec::with_capacity(entries.len());
        for entry in entries {
            if !filter.allows(MatchPlatform::Windows, &entry.executable) {
                continue;
            }
            let Ok(bitness) = self.ops.bitness(entry.pid) else {
                continue;
            };
            let Ok(command_line) = self.ops.command_line(entry.pid) else {
                continue;
            };
            records.push(build_record(
                entry.pid,
                &entry.executable,
                bitness,
                command_line,
            ));
        }
        Ok(records)
    }

    fn set_name_prefilter(&self, filter: &NamePrefilter) {
        *self
            .prefilter
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = filter.clone();
    }
}

/// Production Windows [`ProcessSource`] (M1 §4 phase d2b, units b/c):
/// [`WindowsSource`] over [`RealOps`]. An opaque alias — callers
/// construct it with [`Default`] and use it only as `Arc<dyn
/// ProcessSource>` (`src/runtime/lifecycle.rs`'s own wiring, unit d).
#[cfg(windows)]
pub type WindowsProcessSource = WindowsSource<RealOps>;

#[cfg(windows)]
impl Default for WindowsProcessSource {
    fn default() -> Self {
        WindowsSource::new(RealOps)
    }
}

#[cfg(test)]
mod tests {
    use std::io;
    use std::sync::Mutex;

    use vosu_plugin_api::Bitness;

    use super::{
        NamePrefilter, ProcessOps, ProcessSource, RawEntry, WindowsSource, build_record,
        executable_path_from_command_line,
    };

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

    /// Synthetic [`ProcessOps`]: a mutable in-memory process list plus a
    /// log of every pid [`Self::bitness`]/[`Self::command_line`] was
    /// called for — "opened", in the enumeration-cost sense this unit's
    /// pre-filter exists to avoid.
    #[derive(Clone)]
    struct FakeEntry {
        pid: u32,
        name: &'static str,
        bitness: Bitness,
        command_line: &'static str,
    }

    #[derive(Default)]
    struct FakeOps {
        entries: Mutex<Vec<FakeEntry>>,
        opened: Mutex<Vec<u32>>,
    }

    impl FakeOps {
        fn set_entries(&self, entries: Vec<FakeEntry>) {
            *self
                .entries
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()) = entries;
        }
    }

    impl ProcessOps for FakeOps {
        fn enumerate(&self) -> io::Result<Vec<RawEntry>> {
            Ok(self
                .entries
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .iter()
                .map(|entry| RawEntry {
                    pid: entry.pid,
                    executable: entry.name.to_owned(),
                })
                .collect())
        }

        fn bitness(&self, pid: u32) -> io::Result<Bitness> {
            self.opened
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .push(pid);
            self.entries
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .iter()
                .find(|entry| entry.pid == pid)
                .map(|entry| entry.bitness)
                .ok_or_else(|| io::Error::other("pid gone"))
        }

        fn command_line(&self, pid: u32) -> io::Result<String> {
            self.entries
                .lock()
                .unwrap_or_else(|poison| poison.into_inner())
                .iter()
                .find(|entry| entry.pid == pid)
                .map(|entry| entry.command_line.to_owned())
                .ok_or_else(|| io::Error::other("pid gone"))
        }
    }

    #[test]
    fn non_matching_names_are_never_opened() {
        let ops = FakeOps::default();
        ops.set_entries(vec![
            FakeEntry {
                pid: 1,
                name: "match.exe",
                bitness: Bitness::X64,
                command_line: "match.exe",
            },
            FakeEntry {
                pid: 2,
                name: "nomatch.exe",
                bitness: Bitness::X64,
                command_line: "nomatch.exe",
            },
        ]);
        let source = WindowsSource::new(ops);
        source.set_name_prefilter(&NamePrefilter::Names(vec!["match.exe".to_owned()]));

        let records = source.enumerate().expect("enumerate succeeds");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].pid, 1);
        assert_eq!(
            *source
                .ops
                .opened
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
            vec![1],
            "pid 2's name never matched the pre-filter, so it was never opened"
        );
    }

    #[test]
    fn windows_name_matching_is_case_insensitive() {
        let ops = FakeOps::default();
        ops.set_entries(vec![FakeEntry {
            pid: 1,
            name: "Match.EXE",
            bitness: Bitness::X64,
            command_line: "Match.EXE",
        }]);
        let source = WindowsSource::new(ops);
        source.set_name_prefilter(&NamePrefilter::Names(vec!["match.exe".to_owned()]));

        let records = source.enumerate().expect("enumerate succeeds");

        assert_eq!(
            records.len(),
            1,
            "§6.1: Windows name matching is case-insensitive"
        );
    }

    #[test]
    fn unfiltered_prefilter_opens_every_name() {
        let ops = FakeOps::default();
        ops.set_entries(vec![FakeEntry {
            pid: 9,
            name: "anything.exe",
            bitness: Bitness::X64,
            command_line: "anything.exe",
        }]);
        let source = WindowsSource::new(ops);
        source.set_name_prefilter(&NamePrefilter::Unfiltered);

        let records = source.enumerate().expect("enumerate succeeds");

        assert_eq!(records.len(), 1);
        assert_eq!(
            *source
                .ops
                .opened
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
            vec![9]
        );
    }

    #[test]
    fn pid_reuse_with_a_name_change_drops_the_stale_entry() {
        let ops = FakeOps::default();
        ops.set_entries(vec![FakeEntry {
            pid: 7,
            name: "match.exe",
            bitness: Bitness::X64,
            command_line: "match.exe",
        }]);
        let source = WindowsSource::new(ops);
        source.set_name_prefilter(&NamePrefilter::Names(vec!["match.exe".to_owned()]));

        let first = source.enumerate().expect("enumerate succeeds");
        assert_eq!(first.len(), 1, "the original occurrence matches");

        // The OS reused pid 7 for an unrelated, non-matching process
        // between reconciliation passes.
        source.ops.set_entries(vec![FakeEntry {
            pid: 7,
            name: "unrelated.exe",
            bitness: Bitness::X64,
            command_line: "unrelated.exe",
        }]);
        let second = source.enumerate().expect("enumerate succeeds");

        assert!(
            second.is_empty(),
            "the new occurrence under the same pid no longer matches the pre-filter"
        );
    }

    #[test]
    fn pid_reuse_with_a_name_change_can_newly_match() {
        let ops = FakeOps::default();
        ops.set_entries(vec![FakeEntry {
            pid: 3,
            name: "other.exe",
            bitness: Bitness::X64,
            command_line: "other.exe",
        }]);
        let source = WindowsSource::new(ops);
        source.set_name_prefilter(&NamePrefilter::Names(vec!["match.exe".to_owned()]));

        assert!(source.enumerate().expect("enumerate succeeds").is_empty());

        // The OS reused pid 3 for a now-matching process.
        source.ops.set_entries(vec![FakeEntry {
            pid: 3,
            name: "match.exe",
            bitness: Bitness::X64,
            command_line: "match.exe",
        }]);
        let records = source.enumerate().expect("enumerate succeeds");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].pid, 3);
    }
}
