//! Linux [`ProcessSource`] backend (`docs/v2/implementation-plan.md` M1
//! §4, phase d2b, unit b): wires [`DiscoveryService`](super::DiscoveryService)
//! to `vosu_sys::process::{enumerate_processes_matching,
//! process_metadata_matching}` for every full/targeted process read, and,
//! capability permitting, `vosu_sys::events::LinuxProcessEvents`'s push
//! event channel (CN_PROC/eBPF) for low-latency appear/exit hints.
//! `LinuxProcessEvents::open`'s own `Auto` selection (eBPF first, CN_PROC
//! fallback) is unchanged by this module; [`LinuxSource::open`] only
//! decides whether to ask for an event backend at all, via
//! `LinuxProcessEvents::from_environment`.
//!
//! When no event backend is available (no CAP_NET_ADMIN/CAP_BPF, a kernel
//! too old for either, or `VOSU_PROCESS_EVENTS=off`),
//! `LinuxProcessEvents::from_environment` returns `None` (already logging
//! why) and this source degrades to polling exactly as 1.x's
//! `NativeProcessProvider::default` did: [`ProcessSource::notifier`]
//! returns `None` and [`ProcessSource::drain_events`]'s default
//! (always-empty, never reconcile-forcing) applies, so
//! [`DiscoveryService`](super::DiscoveryService)'s own
//! `RECONCILE_INTERVAL` alone drives discovery.
//!
//! Comm-hint handling (§6.1, D18): [`ProcessSource::set_comm_hints`]
//! validates the host's `Vec<String>` into `vosu_sys::events::CommHints`,
//! logs and leaves the previous (already-validated) filter in place on a
//! bounds violation (never panics), and only on success both updates this
//! source's own userspace pre-filter (used by [`Self::enumerate`]/
//! [`Self::inspect`] regardless of whether an event backend is open) and,
//! if one is open, the kernel/CN_PROC pre-filter.
//!
//! This source also mirrors every pid it has itself most recently
//! confirmed live and comm-matched into the event backend's own "tracked"
//! set (`LinuxProcessEvents::replace_tracked`, `vosu-sys/src/events.rs`'s
//! `EventQueue::tracked`), so a later exit event for a confirmed pid is
//! recognized precisely instead of being silently dropped (CN_PROC) or
//! forcing an unconditional full reconcile (eBPF) — `EventQueue::push`'s
//! own untracked-exit handling. [`ProcessSource`] has no callback for "this
//! pid is no longer tracked", so this bookkeeping is entirely internal:
//! [`Self::enumerate`] replaces the whole tracked set with what it just
//! observed, [`Self::inspect`] adds/removes one pid, and
//! [`Self::drain_events`] removes every pid its own `events.drain()` just
//! reported exited.

use std::collections::HashSet;
use std::ffi::{OsStr, OsString};
use std::io;
use std::sync::Mutex;

use tokio::sync::Notify;
use vosu_plugin_api::Bitness;
use vosu_sys::events::{CommHints, LinuxProcessEvents};
use vosu_sys::process::{
    LinuxProcessMetadata, ProcessBitness, enumerate_processes_matching, process_metadata_matching,
};

use crate::plugin::host::ProcessRecord;

use super::{ProcessEventBatch, ProcessSource};

fn map_bitness(bitness: ProcessBitness) -> Bitness {
    match bitness {
        ProcessBitness::X86 => Bitness::X86,
        ProcessBitness::X64 => Bitness::X64,
    }
}

/// `ProcessRecord::command_line`'s own "joined argv" contract
/// (`crate::plugin::host::ProcessRecord`), space-joined to match
/// `MatcherEngine`'s `command_line_patterns` full-match semantics against
/// a single string (matcher.rs's own doc comment).
fn join_command_line(parts: &[OsString]) -> String {
    parts
        .iter()
        .map(|part| part.to_string_lossy())
        .collect::<Vec<_>>()
        .join(" ")
}

fn map_metadata(metadata: LinuxProcessMetadata) -> ProcessRecord {
    ProcessRecord {
        pid: metadata.pid,
        name: metadata.name.to_string_lossy().into_owned(),
        path: metadata.executable.to_string_lossy().into_owned(),
        command_line: join_command_line(&metadata.command_line),
        bitness: map_bitness(metadata.bitness),
    }
}

fn comm_matches(filter: &CommHints, name: &OsStr) -> bool {
    use std::os::unix::ffi::OsStrExt;
    filter.matches(name.as_bytes())
}

/// Linux [`ProcessSource`]; see the module doc comment for the full
/// design.
pub struct LinuxSource {
    events: Option<LinuxProcessEvents>,
    filter: Mutex<CommHints>,
    tracked: Mutex<HashSet<u32>>,
}

impl LinuxSource {
    /// Opens the fastest available event backend
    /// (`VOSU_PROCESS_EVENTS`-selected; unset defaults to `Auto`, eBPF
    /// first, CN_PROC fallback) and degrades to polling when none is
    /// available (module doc comment). Starts with an empty comm-hint
    /// filter: [`DiscoveryService`](super::DiscoveryService) always syncs
    /// the real set through [`ProcessSource::set_comm_hints`] before its
    /// first full reconcile (`run_pass`'s own ordering), so this never
    /// observes a stale filter.
    #[must_use]
    pub fn open() -> Self {
        let filter = CommHints::default();
        Self {
            events: LinuxProcessEvents::from_environment(&filter),
            filter: Mutex::new(filter),
            tracked: Mutex::new(HashSet::new()),
        }
    }

    #[cfg(test)]
    #[must_use]
    pub(crate) fn backend_name(&self) -> &'static str {
        self.events
            .as_ref()
            .map_or("polling", LinuxProcessEvents::backend_name)
    }

    fn current_filter(&self) -> CommHints {
        self.filter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    fn sync_tracked(&self, tracked: &HashSet<u32>) {
        if let Some(events) = self.events.as_ref() {
            events.replace_tracked(tracked.iter().copied());
        }
    }

    fn mark_tracked(&self, pid: u32) {
        let mut tracked = self
            .tracked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if tracked.insert(pid) {
            self.sync_tracked(&tracked);
        }
    }

    fn forget_tracked(&self, pids: impl IntoIterator<Item = u32>) {
        let mut tracked = self
            .tracked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let mut changed = false;
        for pid in pids {
            changed |= tracked.remove(&pid);
        }
        if changed {
            self.sync_tracked(&tracked);
        }
    }

    fn replace_all_tracked(&self, pids: impl Iterator<Item = u32>) {
        let mut tracked = self
            .tracked
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        tracked.clear();
        tracked.extend(pids);
        self.sync_tracked(&tracked);
    }
}

impl ProcessSource for LinuxSource {
    fn enumerate(&self) -> io::Result<Vec<ProcessRecord>> {
        let filter = self.current_filter();
        let records: Vec<ProcessRecord> =
            enumerate_processes_matching(|name| comm_matches(&filter, name))
                .map_err(io::Error::other)?
                .into_iter()
                .map(map_metadata)
                .collect();
        self.replace_all_tracked(records.iter().map(|record| record.pid));
        Ok(records)
    }

    fn inspect(&self, pid: u32) -> io::Result<Option<ProcessRecord>> {
        let filter = self.current_filter();
        let found = process_metadata_matching(pid, |name| comm_matches(&filter, name))
            .map_err(io::Error::other)?;
        match found {
            Some(metadata) => {
                self.mark_tracked(pid);
                Ok(Some(map_metadata(metadata)))
            }
            None => {
                self.forget_tracked([pid]);
                Ok(None)
            }
        }
    }

    fn drain_events(&self) -> ProcessEventBatch {
        let Some(events) = self.events.as_ref() else {
            return ProcessEventBatch::default();
        };
        let batch = events.drain();
        if !batch.exit.is_empty() {
            self.forget_tracked(batch.exit.iter().copied());
        }
        ProcessEventBatch {
            inspect: batch.inspect,
            exit: batch.exit,
            needs_reconcile: batch.needs_reconcile,
        }
    }

    fn set_comm_hints(&self, hints: &[String]) -> io::Result<()> {
        let hints = CommHints::new(hints).map_err(|error| {
            tracing::warn!(%error, "comm hints rejected; keeping the previous filter");
            io::Error::new(io::ErrorKind::InvalidInput, error)
        })?;
        if let Some(events) = self.events.as_ref() {
            events.set_comm_hints(&hints)?;
        }
        *self
            .filter
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = hints;
        Ok(())
    }

    fn notifier(&self) -> Option<&Notify> {
        self.events.as_ref().map(LinuxProcessEvents::notifier)
    }
}

#[cfg(test)]
mod tests;
