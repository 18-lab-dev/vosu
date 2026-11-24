//! Windows polling [`ProcessSource`] (`docs/v2/implementation-plan.md` M1
//! §4, phase d2b, unit c): full system enumeration every
//! [`super::DiscoveryCadence`] tick — no live event channel exists on
//! Windows, so [`super::ProcessSource`]'s defaults (`drain_events`,
//! `notifier`) already do the right thing and this module implements
//! [`super::ProcessSource::enumerate`]/[`super::ProcessSource::set_prefilter`].
//!
//! **Enumeration-cost pre-filter**: 1.x narrowed its Windows pass with an
//! osu!-specific `is_windows_candidate_name` before opening each process.
//! The generic core has no such plugin, so [`WindowsSource::enumerate`]
//! instead consults [`Prefilter`] — the union of every registered
//! plugin's hint names (`MatcherEngine::prefilter`, §6.1), pushed down by
//! [`super::DiscoveryService`]'s own `run_pass` through
//! [`super::ProcessSource::set_prefilter`] exactly like Linux's own comm
//! pre-filter sync — and skips [`ProcessOps::bitness`]/
//! [`ProcessOps::command_line`]/[`ProcessOps::image_path`] (the "open"
//! calls) for a pid whose bare executable name the filter rejects. Case
//! rule: [`Prefilter::allows`] is always evaluated against
//! [`MatchPlatform::Windows`] here (§6.1's case-insensitive name rule),
//! independent of the host this file happens to be compiled on.
//!
//! **Unfiltered fallback**: a plugin whose `[match]` table matches only
//! by `process_path_patterns`/`command_line_patterns` (with no usable
//! hint, Windows has none, `MatcherEngine::prefilter`'s own doc comment)
//! can accept a process whose executable name is nowhere in any
//! `process_names` list, so narrowing by name would silently hide it.
//! `MatcherEngine` already encodes this: the moment *any* registered
//! plugin is not `names_only`, `prefilter()` returns
//! [`Prefilter::Unfiltered`] for the whole set (not just that one
//! plugin), and [`WindowsSource::enumerate`] opens every process again.
//!
//! **Metadata cache** (P1 "Windows cost"): keyed by `(pid, create_time)`,
//! mirroring 1.x's own `windows_metadata_cache`/`CachedWindowsMetadata`
//! (`src/instance/discovery.rs`) — `create_time` detects pid reuse before
//! serving stale bitness/command-line/path data for a new occurrence, so
//! even when the pre-filter is [`Prefilter::Unfiltered`] (every process
//! admitted; `vosu-osu`'s own matcher will declare path patterns) each
//! tick opens only pids the cache has not already seen at their current
//! `create_time` — [`tests::cache_hits_do_not_reopen_an_unchanged_pid`]
//! proves this through [`ProcessOps`]'s own open-count log.
//!
//! **Identity and pid reuse**: `ProcessOps::enumerate`'s own
//! `create_time` (`vosu_sys::process::ProcessEntry::create_time`, read
//! directly from `SYSTEM_PROCESS_INFORMATION` without opening the
//! process) is `ProcessRecord::start_time`'s source on this platform —
//! `discovery.rs::same_occurrence` (P1) compares it, not name/path/
//! command_line/bitness, so a still-running process that rewrites its
//! own argv is never churned, and a reused pid with identical metadata
//! but a new `create_time` is still reported as an exit immediately
//! followed by a fresh appearance. This source only needs to make sure a
//! reused pid whose *new* occurrence no longer matches the pre-filter
//! stops being reported at all, which
//! `tests::pid_reuse_with_a_name_change_drops_the_stale_entry` below
//! proves directly.
//!
//! **Executable path**: [`ProcessOps::image_path`]
//! (`vosu_sys::process::process_image_path`, `QueryFullProcessImageNameW`)
//! is the preferred [`ProcessRecord::path`] source — a raw command line's
//! argv[0] can be relative, bare, or whatever an unrelated
//! `lpApplicationName` happened to be, and `process_path_patterns`
//! matches the full executable path (§6.1). `executable_path_from_command_line`
//! is only the fallback for a pid whose image path read fails.
//!
//! The [`ProcessOps`] seam (real Win32 calls vs. a synthetic recording
//! implementation) makes the skip-on-non-match, cache, and pid-reuse
//! behavior unit-testable on any host: [`WindowsSource`]/[`ProcessOps`]/
//! [`RawEntry`] compile under `#[cfg(any(windows, test))]`, [`RealOps`]
//! (the only piece that actually calls into `vosu_sys::process`) stays
//! `#[cfg(windows)]`-only, compile-checked exclusively through
//! `cargo xwin clippy` here (`AGENTS.md`: the Windows VM is down, so
//! nothing under `#[cfg(windows)]` in this file runs natively in this
//! environment).

#[cfg(any(windows, test))]
use std::collections::HashMap;
#[cfg(any(windows, test))]
use std::io;
#[cfg(any(windows, test))]
use std::sync::Mutex;

#[cfg(any(windows, test))]
use vosu_plugin_api::Bitness;

#[cfg(any(windows, test))]
use super::super::host::ProcessRecord;
#[cfg(any(windows, test))]
use super::super::matcher::{MatchPlatform, Prefilter};
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
/// an empty path. Only a fallback itself now (module doc comment
/// "Executable path"): [`ProcessOps::image_path`] is preferred whenever
/// it succeeds.
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
/// reproduce). `image_path` (§6.1's `process_path_patterns` source,
/// module doc comment) is preferred for [`ProcessRecord::path`];
/// `entry_name` is the fallback when it is `None` (an image-path read
/// failure) *and* `command_line` does not parse to a usable executable
/// path either. Same `#[cfg(any(windows, test))]` as
/// [`executable_path_from_command_line`] and for the same reason.
#[cfg(any(windows, test))]
#[must_use]
pub(crate) fn build_record(
    pid: u32,
    entry_name: &str,
    bitness: Bitness,
    command_line: String,
    image_path: Option<String>,
    start_time: u64,
) -> ProcessRecord {
    let path = image_path
        .or_else(|| executable_path_from_command_line(&command_line))
        .unwrap_or_else(|| entry_name.to_owned());
    ProcessRecord {
        pid,
        name: entry_name.to_owned(),
        path,
        command_line,
        bitness,
        start_time,
        argv: None,
    }
}

/// One raw OS process-list entry [`WindowsSource::enumerate`]'s
/// pre-filter check needs: pid, bare executable name, and `create_time`
/// (identity/cache key, module doc comment). A local type, not
/// `vosu_sys::process::ProcessEntry` (`#[cfg(windows)]`-only), so the
/// pre-filter skip, cache, and pid-reuse behavior below are unit-testable
/// on any host.
#[cfg(any(windows, test))]
struct RawEntry {
    pid: u32,
    executable: String,
    create_time: u64,
}

/// Seam over the raw Win32 process calls [`WindowsSource::enumerate`]
/// needs, so "a non-matching name is never opened" (no [`Self::bitness`]/
/// [`Self::command_line`]/[`Self::image_path`] call for it), the
/// `(pid, create_time)` cache, and pid-reuse-with-a-name-change behavior
/// are unit-testable on any host. [`RealOps`] is the `#[cfg(windows)]`
/// production implementation; this module's own `tests` use a synthetic
/// recording implementation instead.
#[cfg(any(windows, test))]
trait ProcessOps {
    fn enumerate(&self) -> io::Result<Vec<RawEntry>>;
    fn bitness(&self, pid: u32) -> io::Result<Bitness>;
    fn command_line(&self, pid: u32) -> io::Result<String>;
    /// The full executable image path (module doc comment "Executable
    /// path"). `Ok` with no success path makes this indistinguishable
    /// from "not available" to callers, which already fall back to
    /// `command_line`'s own argv[0]: `Err` here never fails the whole
    /// pid the way [`Self::bitness`]/[`Self::command_line`] failing does.
    fn image_path(&self, pid: u32) -> io::Result<String>;
}

#[cfg(windows)]
use vosu_sys::process::{
    MemoryError, ProcessBitness, enumerate_processes, process_bitness, process_command_line,
    process_image_path,
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
/// below (`enumerate_processes`, `process_bitness`, `process_command_line`,
/// `process_image_path`), and in particular whether the
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
                create_time: entry.create_time,
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

    fn image_path(&self, pid: u32) -> io::Result<String> {
        process_image_path(pid).map_err(to_io_error)
    }
}

/// Cached per-pid metadata (module doc comment "Metadata cache"), reused
/// across ticks while `create_time` stays the same.
#[cfg(any(windows, test))]
#[derive(Clone)]
struct CachedWindowsMetadata {
    create_time: u64,
    bitness: Bitness,
    command_line: String,
    image_path: Option<String>,
}

/// The [`ProcessSource`] itself: full enumeration through `ops` every
/// [`super::DiscoveryCadence`] tick, skipping a pid whose bare
/// executable name [`Prefilter::allows`] rejects before ever calling
/// [`ProcessOps::bitness`]/[`ProcessOps::command_line`]/
/// [`ProcessOps::image_path`] for it (module doc comment "Enumeration-cost
/// pre-filter"), and serving a pid already in `cache` at its current
/// `create_time` from there instead of reopening it (module doc comment
/// "Metadata cache"). A pid that passed the pre-filter but fails either
/// read (exited mid-enumeration, access denied) is still skipped rather
/// than failing the whole pass — 1.x's own `let Ok(..) = .. else {
/// continue; }` handling (`src/instance/discovery.rs`'s
/// `NativeProcessProvider::enumerate`).
#[cfg(any(windows, test))]
pub struct WindowsSource<Ops> {
    ops: Ops,
    /// Starts [`Prefilter::Unfiltered`] (fail open): the discovery
    /// service always calls [`Self::set_prefilter`] (via `run_pass`'s
    /// `sync_prefilter`) before its very first `enumerate()`, so this
    /// default is never actually observed in production — it only avoids
    /// wrongly narrowing a source some other caller enumerates before
    /// syncing it.
    prefilter: Mutex<Prefilter>,
    cache: Mutex<HashMap<u32, CachedWindowsMetadata>>,
}

#[cfg(any(windows, test))]
impl<Ops> WindowsSource<Ops> {
    fn new(ops: Ops) -> Self {
        Self {
            ops,
            prefilter: Mutex::new(Prefilter::Unfiltered),
            cache: Mutex::new(HashMap::new()),
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
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        cache.retain(|pid, _| entries.iter().any(|entry| entry.pid == *pid));
        let mut records = Vec::with_capacity(entries.len());
        for entry in entries {
            if !filter.allows(MatchPlatform::Windows, &entry.executable) {
                continue;
            }
            let cached = cache
                .get(&entry.pid)
                .filter(|cached| cached.create_time == entry.create_time)
                .cloned();
            let metadata = if let Some(cached) = cached {
                cached
            } else {
                let Ok(bitness) = self.ops.bitness(entry.pid) else {
                    continue;
                };
                let Ok(command_line) = self.ops.command_line(entry.pid) else {
                    continue;
                };
                let image_path = self.ops.image_path(entry.pid).ok();
                let metadata = CachedWindowsMetadata {
                    create_time: entry.create_time,
                    bitness,
                    command_line,
                    image_path,
                };
                cache.insert(entry.pid, metadata.clone());
                metadata
            };
            records.push(build_record(
                entry.pid,
                &entry.executable,
                metadata.bitness,
                metadata.command_line,
                metadata.image_path,
                metadata.create_time,
            ));
        }
        Ok(records)
    }

    fn set_prefilter(&self, filter: &Prefilter) -> io::Result<()> {
        *self
            .prefilter
            .lock()
            .unwrap_or_else(|poison| poison.into_inner()) = filter.clone();
        Ok(())
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
        Prefilter, ProcessOps, ProcessSource, RawEntry, WindowsSource, build_record,
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
    fn build_record_prefers_the_image_path_over_the_command_line() {
        let record = build_record(
            4242,
            "osu!.exe",
            Bitness::X64,
            r#""C:\Games\osu!\osu!.exe" -nosplash"#.to_owned(),
            Some(r"C:\Games\osu-lazer\osu!.exe".to_owned()),
            7,
        );
        assert_eq!(record.pid, 4242);
        assert_eq!(record.path, r"C:\Games\osu-lazer\osu!.exe");
        assert_eq!(record.name, "osu!.exe");
        assert_eq!(record.bitness, Bitness::X64);
        assert_eq!(record.command_line, r#""C:\Games\osu!\osu!.exe" -nosplash"#);
        assert_eq!(record.start_time, 7);
    }

    #[test]
    fn build_record_falls_back_to_the_command_line_without_an_image_path() {
        let record = build_record(
            4242,
            "osu!.exe",
            Bitness::X64,
            r#""C:\Games\osu!\osu!.exe" -nosplash"#.to_owned(),
            None,
            0,
        );
        assert_eq!(record.path, r"C:\Games\osu!\osu!.exe");
    }

    #[test]
    fn build_record_falls_back_to_entry_name_without_an_image_path_or_command_line() {
        let record = build_record(7, "svchost.exe", Bitness::X86, String::new(), None, 0);
        assert_eq!(record.path, "svchost.exe");
        assert_eq!(record.name, "svchost.exe");
    }

    /// Synthetic [`ProcessOps`]: a mutable in-memory process list plus a
    /// log of every pid [`Self::bitness`] was called for — "opened", in
    /// the enumeration-cost sense the pre-filter and cache both exist to
    /// avoid (`enumerate`'s own implementation always calls
    /// [`Self::bitness`] exactly once per real open, before
    /// [`Self::command_line`]/[`Self::image_path`]).
    #[derive(Clone)]
    struct FakeEntry {
        pid: u32,
        name: &'static str,
        bitness: Bitness,
        command_line: &'static str,
        create_time: u64,
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
                    create_time: entry.create_time,
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

        fn image_path(&self, _pid: u32) -> io::Result<String> {
            Err(io::Error::other("no image path in this fixture"))
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
                create_time: 1,
            },
            FakeEntry {
                pid: 2,
                name: "nomatch.exe",
                bitness: Bitness::X64,
                command_line: "nomatch.exe",
                create_time: 1,
            },
        ]);
        let source = WindowsSource::new(ops);
        source
            .set_prefilter(&Prefilter::Names(vec!["match.exe".to_owned()]))
            .expect("set_prefilter never fails on Windows");

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
            create_time: 1,
        }]);
        let source = WindowsSource::new(ops);
        source
            .set_prefilter(&Prefilter::Names(vec!["match.exe".to_owned()]))
            .expect("set_prefilter never fails on Windows");

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
            create_time: 1,
        }]);
        let source = WindowsSource::new(ops);
        source
            .set_prefilter(&Prefilter::Unfiltered)
            .expect("set_prefilter never fails on Windows");

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
            create_time: 1,
        }]);
        let source = WindowsSource::new(ops);
        source
            .set_prefilter(&Prefilter::Names(vec!["match.exe".to_owned()]))
            .expect("set_prefilter never fails on Windows");

        let first = source.enumerate().expect("enumerate succeeds");
        assert_eq!(first.len(), 1, "the original occurrence matches");

        // The OS reused pid 7 for an unrelated, non-matching process
        // between reconciliation passes.
        source.ops.set_entries(vec![FakeEntry {
            pid: 7,
            name: "unrelated.exe",
            bitness: Bitness::X64,
            command_line: "unrelated.exe",
            create_time: 2,
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
            create_time: 1,
        }]);
        let source = WindowsSource::new(ops);
        source
            .set_prefilter(&Prefilter::Names(vec!["match.exe".to_owned()]))
            .expect("set_prefilter never fails on Windows");

        assert!(source.enumerate().expect("enumerate succeeds").is_empty());

        // The OS reused pid 3 for a now-matching process.
        source.ops.set_entries(vec![FakeEntry {
            pid: 3,
            name: "match.exe",
            bitness: Bitness::X64,
            command_line: "match.exe",
            create_time: 2,
        }]);
        let records = source.enumerate().expect("enumerate succeeds");

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].pid, 3);
    }

    #[test]
    fn cache_hits_do_not_reopen_an_unchanged_pid() {
        // P1 "Windows cost": even `Prefilter::Unfiltered` (every process
        // admitted) must open a still-live, unchanged process only once.
        let ops = FakeOps::default();
        ops.set_entries(vec![FakeEntry {
            pid: 11,
            name: "osu!.exe",
            bitness: Bitness::X64,
            command_line: "osu!.exe",
            create_time: 42,
        }]);
        let source = WindowsSource::new(ops);
        source
            .set_prefilter(&Prefilter::Unfiltered)
            .expect("set_prefilter never fails on Windows");

        let first = source.enumerate().expect("enumerate succeeds");
        assert_eq!(first.len(), 1);
        let second = source.enumerate().expect("enumerate succeeds");
        assert_eq!(second.len(), 1);

        assert_eq!(
            *source
                .ops
                .opened
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
            vec![11],
            "the second pass must serve pid 11 from the cache instead of reopening it"
        );
    }

    #[test]
    fn a_new_start_time_invalidates_the_cache() {
        let ops = FakeOps::default();
        ops.set_entries(vec![FakeEntry {
            pid: 11,
            name: "osu!.exe",
            bitness: Bitness::X64,
            command_line: "osu!.exe",
            create_time: 42,
        }]);
        let source = WindowsSource::new(ops);
        source
            .set_prefilter(&Prefilter::Unfiltered)
            .expect("set_prefilter never fails on Windows");
        source.enumerate().expect("enumerate succeeds");

        // The OS reused pid 11 for a new occurrence with the same name,
        // bitness and command line but a different `create_time`.
        source.ops.set_entries(vec![FakeEntry {
            pid: 11,
            name: "osu!.exe",
            bitness: Bitness::X64,
            command_line: "osu!.exe",
            create_time: 43,
        }]);
        source.enumerate().expect("enumerate succeeds");

        assert_eq!(
            *source
                .ops
                .opened
                .lock()
                .unwrap_or_else(|poison| poison.into_inner()),
            vec![11, 11],
            "a new create_time must reopen the pid rather than serve the stale cache entry"
        );
    }

    /// Native-only coverage (`#[cfg(target_os = "windows")]`): everything
    /// above this point uses [`FakeOps`] and runs on any host. These
    /// tests drive [`WindowsSource<RealOps>`] against the real Win32
    /// process list, proving the DiscReview2 Windows checklist items the
    /// synthetic tests above cannot: real enumeration finds a freshly
    /// spawned, uniquely renamed child and stops reporting it once it
    /// exits, and §6.1's case-insensitive name rule holds against a real
    /// on-disk image name, not a hand-written [`FakeEntry`].
    #[cfg(target_os = "windows")]
    mod native_tests {
        use std::path::{Path, PathBuf};
        use std::process::{Child, Command, Stdio};
        use std::sync::atomic::{AtomicU32, Ordering};
        use std::time::{Duration, Instant};

        use super::super::RealOps;
        use super::{Prefilter, ProcessSource, WindowsSource};

        fn unique_name(extension: &str) -> String {
            static COUNTER: AtomicU32 = AtomicU32::new(0);
            let n = COUNTER.fetch_add(1, Ordering::Relaxed);
            format!("vtest{}-{n}{extension}", std::process::id() % 1_000_000)
        }

        fn cmd_source() -> PathBuf {
            std::env::var_os("ComSpec")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows\System32\cmd.exe"))
        }

        /// Copies `cmd.exe` to a unique name under `dir` and spawns it
        /// running a ~20s `ping`, so the real process list carries this
        /// test's own chosen image name instead of a shared `cmd.exe`.
        fn renamed_child(dir: &Path, name: &str) -> Child {
            let dest = dir.join(name);
            std::fs::copy(cmd_source(), &dest).expect("copying cmd.exe for a renamed test child");
            Command::new(&dest)
                .args(["/C", "ping", "-n", "20", "127.0.0.1"])
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .expect("spawning the renamed test child")
        }

        fn poll_until<T>(mut probe: impl FnMut() -> Option<T>) -> T {
            let deadline = Instant::now() + Duration::from_secs(10);
            loop {
                if let Some(value) = probe() {
                    return value;
                }
                assert!(
                    Instant::now() < deadline,
                    "timed out waiting for the real Windows process list to settle"
                );
                std::thread::sleep(Duration::from_millis(50));
            }
        }

        #[test]
        fn real_os_enumerate_finds_a_renamed_child_and_stops_after_it_exits() {
            let dir = tempfile::tempdir().unwrap();
            let name = unique_name(".exe");
            let mut child = renamed_child(dir.path(), &name);
            let pid = child.id();

            let source = WindowsSource::new(RealOps);
            source
                .set_prefilter(&Prefilter::Names(vec![name.clone()]))
                .expect("set_prefilter never fails on Windows");

            let found = poll_until(|| {
                source
                    .enumerate()
                    .expect("enumerate succeeds")
                    .into_iter()
                    .find(|record| record.pid == pid)
            });
            assert_eq!(found.name.to_ascii_lowercase(), name.to_ascii_lowercase());

            child.kill().expect("killing the test child");
            child.wait().expect("reaping the test child");

            poll_until(|| {
                let still_present = source
                    .enumerate()
                    .expect("enumerate succeeds")
                    .iter()
                    .any(|record| record.pid == pid);
                (!still_present).then_some(())
            });
        }

        #[test]
        fn prefilter_matching_is_case_insensitive_against_a_real_image_name() {
            let dir = tempfile::tempdir().unwrap();
            // The real on-disk/process-list image name is upper-case;
            // the configured hint below is lower-case.
            let name = unique_name(".EXE");
            let mut child = renamed_child(dir.path(), &name);
            let pid = child.id();

            let source = WindowsSource::new(RealOps);
            source
                .set_prefilter(&Prefilter::Names(vec![name.to_ascii_lowercase()]))
                .expect("set_prefilter never fails on Windows");

            let found = poll_until(|| {
                source
                    .enumerate()
                    .expect("enumerate succeeds")
                    .into_iter()
                    .find(|record| record.pid == pid)
            });
            assert_eq!(
                found.pid, pid,
                "§6.1: Windows name matching must be case-insensitive against a real image name"
            );

            child.kill().expect("killing the test child");
            child.wait().expect("reaping the test child");
        }
    }
}
