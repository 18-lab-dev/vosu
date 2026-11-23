//! Generic core discovery service (`docs/v2/implementation-plan.md` M1 §4,
//! phase d2b): drives [`PluginHost::process_appeared`]/
//! [`PluginHost::process_exited`]/[`PluginHost::foreground_changed`] from a
//! pluggable [`ProcessSource`] seam — the real OS process list and event
//! backends in production (platform-specific modules, later units), a
//! synthetic in-memory source in this module's own tests. Owns no platform
//! code itself: only the reconciliation loop (periodic full enumeration as
//! a bounded-loss backstop, targeted [`ProcessSource::drain_events`]/
//! [`ProcessSource::inspect`] for cheap incremental updates when a source
//! provides them), the Linux comm-hint pre-filter sync
//! (`MatcherEngine::comm_hints` -> [`ProcessSource::set_comm_hints`], §6.1,
//! D18), and PID-generation correctness: a pid whose freshly observed
//! identity (name/path/command_line/bitness) differs from what this
//! service last reported for it is a reused pid, reported as an exit
//! immediately followed by a fresh appearance, never silently folded into
//! the old occurrence.
//!
//! Runs on one dedicated OS thread, not a Tokio worker (`AGENTS.md`: "No
//! blocking sleep or wait on a Tokio worker"): every [`ProcessSource`] call
//! is a blocking syscall (`/proc` reads, `EnumProcesses`, …), so the whole
//! reconciliation loop — including the one asynchronous piece, waiting on
//! a source's own event [`Notify`], this service's own wake-on-stop
//! `Notify`, and a bounded timeout together — runs on that dedicated
//! thread, driven through `Handle::block_on`, never a worker.
use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::Notify;

use super::host::{PluginHost, ProcessRecord};

/// Event hints [`ProcessSource::drain_events`] buffers since the last
/// call, generic over whatever push-capable channel a real backend uses
/// (Linux eBPF/CN_PROC's own `vosu_sys::events::ProcessEventBatch`, a
/// later unit's conversion target) and independent of any
/// platform-specific feature gate, so this module compiles the same on
/// every target (`AGENTS.md`'s Linux process-event backend paragraph:
/// `vosu_sys::events` is itself gated on
/// `target_os = "linux"`/`linux-process-events`).
#[derive(Debug, Default, Clone)]
pub struct ProcessEventBatch {
    /// Pids to re-check through [`ProcessSource::inspect`].
    pub inspect: Vec<u32>,
    /// Pids the source already knows have exited.
    pub exit: Vec<u32>,
    /// Whether the source itself lost events and a full
    /// [`ProcessSource::enumerate`] pass is needed regardless of
    /// [`RECONCILE_INTERVAL`].
    pub needs_reconcile: bool,
}

/// Full reconciliation cadence (bounded loss recovery,
/// `docs/v2/implementation-plan.md` M1 §4): every `enumerate()`-driven full
/// pass, regardless of whether the source's own event channel is healthy,
/// re-derives ground truth from scratch. A source with a healthy,
/// low-latency event channel (Linux eBPF/CN_PROC) answers almost every
/// individual change through [`ProcessSource::drain_events`]/
/// [`ProcessSource::inspect`] well before this fires; this interval only
/// bounds how long a missed or lost event
/// ([`ProcessEventBatch::needs_reconcile`], a dead event source, or a
/// pure-poll source such as Windows or the testkit default) can leave the
/// host's session set stale.
const RECONCILE_INTERVAL: Duration = Duration::from_secs(2);

/// What the discovery service needs from an OS- or test-specific process
/// source (M1 §4, phase d2b): full enumeration for startup and periodic
/// reconciliation, optional live event hints a push-capable backend
/// provides (Linux eBPF/CN_PROC), and the live comm-hint pre-filter sink.
/// A pure-poll source (Windows, the testkit default) implements only
/// [`Self::enumerate`] and relies on this service's own
/// [`RECONCILE_INTERVAL`] entirely — every other method's default already
/// behaves correctly for it.
pub trait ProcessSource: Send + Sync {
    /// Every process this source currently considers a discovery
    /// candidate (already narrowed by the live comm-hint pre-filter on
    /// Linux; unfiltered — or narrowed by the source's own equivalent —
    /// everywhere else). Full userspace `[match]` matching still runs in
    /// [`PluginHost::process_appeared`] regardless of what this narrows
    /// (`docs/v2/plugin-abi.md` §6.1).
    fn enumerate(&self) -> io::Result<Vec<ProcessRecord>>;

    /// Re-reads one still-plausibly-tracked pid without a full
    /// enumeration, for [`Self::drain_events`]'s own `inspect` hints.
    /// `Ok(None)` means the process is already gone. The default costs a
    /// full [`Self::enumerate`] and is only adequate for a source with no
    /// cheaper single-pid lookup; every real backend overrides it.
    fn inspect(&self, pid: u32) -> io::Result<Option<ProcessRecord>> {
        Ok(self
            .enumerate()?
            .into_iter()
            .find(|candidate| candidate.pid == pid))
    }

    /// Drains event hints buffered since the last call: `inspect` pids to
    /// re-check, `exit` pids the source already knows are gone, and
    /// whether the source itself lost events and this service must run a
    /// full reconcile regardless of [`RECONCILE_INTERVAL`]. The default —
    /// an always-empty, never-reconcile-forcing batch — is exactly
    /// correct for a pure-poll source: [`RECONCILE_INTERVAL`] alone
    /// already drives it.
    fn drain_events(&self) -> ProcessEventBatch {
        ProcessEventBatch::default()
    }

    /// Updates the source's own live comm-hint pre-filter (Linux
    /// production only) to the union of every enabled plugin's
    /// `comm_hints` (`docs/v2/plugin-abi.md` §6.1), already deduplicated
    /// and sorted (`MatcherEngine::comm_hints`). The default is a correct
    /// no-op for a source with nothing to narrow; a real Linux backend
    /// converts this into its own validated
    /// `vosu_sys::events::CommHints`, logging and leaving its filter
    /// unchanged on a combined set over
    /// `vosu_sys::events::MAX_COMM_HINTS`.
    fn set_comm_hints(&self, _hints: &[String]) -> io::Result<()> {
        Ok(())
    }

    /// A [`Notify`] this service awaits alongside its own
    /// [`RECONCILE_INTERVAL`] timeout, woken whenever new events are ready
    /// for [`Self::drain_events`]. `None` (the default) for a source with
    /// no event channel: the service then relies on the timeout alone,
    /// exactly as a pure-poll source needs.
    fn notifier(&self) -> Option<&Notify> {
        None
    }
}

/// Whether two observations of the same pid are the same OS process
/// occurrence (identity, not a generation counter): differing in any of
/// these fields means the pid was reused by a different process between
/// the two observations, never a genuine update to a still-running one.
fn same_occurrence(a: &ProcessRecord, b: &ProcessRecord) -> bool {
    a.name == b.name
        && a.path == b.path
        && a.command_line == b.command_line
        && a.bitness == b.bitness
}

/// Feeds one observed `record` to `host`, first reporting an exit for
/// whatever occurrence `tracked` already holds for this pid if the two
/// disagree (PID-generation correctness: a reused pid is a new process,
/// reported as an exit immediately followed by a fresh appearance, never
/// folded into the old occurrence). A repeat observation of the same
/// occurrence is **not** re-sent to `host`: [`PluginHost::process_appeared`]
/// is itself idempotent for an already-tracked pid
/// (`SessionRegistry::attach`'s own no-op rule), but skipping the call
/// here avoids needless matcher/session lock traffic on every
/// reconciliation pass for a process that never changed.
fn apply_appeared(
    host: &PluginHost,
    tracked: &mut HashMap<u32, ProcessRecord>,
    record: ProcessRecord,
) {
    match tracked.get(&record.pid) {
        Some(previous) if same_occurrence(previous, &record) => {}
        Some(_) => {
            host.process_exited(record.pid);
            host.process_appeared(record.clone());
        }
        None => host.process_appeared(record.clone()),
    }
    tracked.insert(record.pid, record);
}

/// Reports `pid` exited to `host` and stops tracking it. Safe to call for
/// a pid this service never reported as appeared (e.g. one that never
/// matched any plugin's `[match]` table): [`PluginHost::process_exited`]
/// is itself a no-op for a pid it is not tracking a session for.
fn apply_exited(host: &PluginHost, tracked: &mut HashMap<u32, ProcessRecord>, pid: u32) {
    tracked.remove(&pid);
    host.process_exited(pid);
}

/// Full ground-truth pass (bounded loss recovery): every process
/// `source.enumerate()` reports is applied through [`apply_appeared`];
/// every previously tracked pid absent from that list is applied through
/// [`apply_exited`]. Logs and leaves `tracked` unchanged on an
/// enumeration failure, so a transient failure neither drops every
/// tracked session nor wedges the service — the next [`RECONCILE_INTERVAL`]
/// tick (or targeted event) retries.
fn full_reconcile(
    host: &PluginHost,
    source: &dyn ProcessSource,
    tracked: &mut HashMap<u32, ProcessRecord>,
) {
    let current = match source.enumerate() {
        Ok(current) => current,
        Err(error) => {
            tracing::warn!(%error, "discovery full reconcile failed; retrying next cycle");
            return;
        }
    };
    let mut seen = HashSet::with_capacity(current.len());
    for record in current {
        seen.insert(record.pid);
        apply_appeared(host, tracked, record);
    }
    let gone: Vec<u32> = tracked
        .keys()
        .copied()
        .filter(|pid| !seen.contains(pid))
        .collect();
    for pid in gone {
        apply_exited(host, tracked, pid);
    }
}

/// Applies one `source.drain_events()` batch: every `exit` pid through
/// [`apply_exited`], every `inspect` pid re-read through
/// [`ProcessSource::inspect`] and applied through [`apply_appeared`] (or
/// [`apply_exited`] if it is already gone). Returns whether a full
/// reconcile is now needed — either because the source itself asked for
/// one ([`ProcessEventBatch::needs_reconcile`]) or because a targeted
/// `inspect` failed, which the caller cannot otherwise tell from a plain
/// `()` return.
fn apply_batch(
    host: &PluginHost,
    source: &dyn ProcessSource,
    tracked: &mut HashMap<u32, ProcessRecord>,
    batch: ProcessEventBatch,
) -> bool {
    for pid in batch.exit {
        apply_exited(host, tracked, pid);
    }
    let mut needs_reconcile = batch.needs_reconcile;
    for pid in batch.inspect {
        match source.inspect(pid) {
            Ok(Some(record)) => apply_appeared(host, tracked, record),
            Ok(None) => apply_exited(host, tracked, pid),
            Err(error) => {
                tracing::warn!(%error, pid, "discovery targeted inspect failed; forcing a full reconcile");
                needs_reconcile = true;
            }
        }
    }
    needs_reconcile
}

/// Syncs the Linux comm-hint pre-filter (`MatcherEngine::comm_hints`,
/// §6.1) to `source` whenever the enabled plugin set's union has changed
/// since the last sync, and reports whether it did: the caller forces a
/// full reconcile on a change, since a hint widening can newly admit a
/// process the pre-filter previously hid from `source.enumerate()`/the
/// kernel event stream entirely, so only a full pass is guaranteed to
/// pick it up. Leaves `last_hints` unchanged on a
/// [`ProcessSource::set_comm_hints`] failure (already logged by the
/// backend itself, e.g. a combined set over
/// `vosu_sys::events::MAX_COMM_HINTS`), so this never silently narrows
/// the filter to something wrong.
fn sync_comm_hints(
    host: &PluginHost,
    source: &dyn ProcessSource,
    last_hints: &mut Vec<String>,
) -> bool {
    let hints = host.comm_hints();
    if hints == *last_hints {
        return false;
    }
    if let Err(error) = source.set_comm_hints(&hints) {
        tracing::warn!(%error, "comm-hint pre-filter update failed; left unchanged");
        return false;
    }
    *last_hints = hints;
    true
}

/// Blocks the calling (dedicated, non-worker) thread until `source`'s own
/// event [`Notify`] fires, `wake` fires (service stopping, or a hint-set
/// change worth checking promptly), or `timeout` elapses — whichever comes
/// first. Driven through `runtime.block_on`, never a worker (module doc
/// comment).
fn wait_next(runtime: &Handle, source: &dyn ProcessSource, wake: &Notify, timeout: Duration) {
    runtime.block_on(async {
        match source.notifier() {
            Some(notifier) => {
                tokio::select! {
                    () = notifier.notified() => {}
                    () = wake.notified() => {}
                    () = tokio::time::sleep(timeout) => {}
                }
            }
            None => {
                tokio::select! {
                    () = wake.notified() => {}
                    () = tokio::time::sleep(timeout) => {}
                }
            }
        }
    });
}

/// One discovery reconciliation pass: syncs comm hints, then either a full
/// reconcile (startup, due tick, a hint-set change, or a source-requested
/// reconcile) or a targeted event batch. Returns the deadline for the next
/// full reconcile.
fn run_pass(
    host: &PluginHost,
    source: &dyn ProcessSource,
    tracked: &mut HashMap<u32, ProcessRecord>,
    last_hints: &mut Vec<String>,
    next_reconcile: Instant,
) -> Instant {
    let hints_changed = sync_comm_hints(host, source, last_hints);
    let now = Instant::now();
    if hints_changed || now >= next_reconcile {
        full_reconcile(host, source, tracked);
        return Instant::now() + RECONCILE_INTERVAL;
    }
    if apply_batch(host, source, tracked, source.drain_events()) {
        full_reconcile(host, source, tracked);
        return Instant::now() + RECONCILE_INTERVAL;
    }
    next_reconcile
}

/// Owns the discovery background thread
/// (`docs/v2/implementation-plan.md` M1 §4, phase d2b). [`Self::start`]
/// unconditionally starts the thread; whether to call it at all (zero cost
/// when unused: `PluginHost::has_enabled_matchers`) is the caller's own
/// decision (production runtime wiring, a later unit), not this type's.
pub struct DiscoveryService {
    cancel: Arc<AtomicBool>,
    wake: Arc<Notify>,
    thread: Option<JoinHandle<()>>,
}

impl DiscoveryService {
    /// Starts the background reconciliation thread, feeding `host` from
    /// `source` until [`Self::stop`] is called (or this value drops, which
    /// stops it the same way, `Drop`'s own doc comment). Holds only a
    /// [`std::sync::Weak`] reference to `host` (`AGENTS.md`: background
    /// tasks hold `Weak`), so this service can never keep the host alive
    /// past its own owner dropping it — the loop exits as soon as the
    /// upgrade fails.
    #[must_use]
    pub fn start(host: &Arc<PluginHost>, source: Arc<dyn ProcessSource>, runtime: Handle) -> Self {
        let cancel = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());
        let loop_host = Arc::downgrade(host);
        let loop_cancel = Arc::clone(&cancel);
        let loop_wake = Arc::clone(&wake);
        let thread = thread::Builder::new()
            .name("vosu-discovery".to_owned())
            .spawn(move || {
                let mut tracked: HashMap<u32, ProcessRecord> = HashMap::new();
                let mut last_hints: Vec<String> = Vec::new();
                // Due immediately: the first pass is always a full
                // reconcile, regardless of what `source` reports before
                // then.
                let mut next_reconcile = Instant::now();
                while !loop_cancel.load(Ordering::Acquire) {
                    let Some(host) = loop_host.upgrade() else {
                        break;
                    };
                    next_reconcile = run_pass(
                        &host,
                        source.as_ref(),
                        &mut tracked,
                        &mut last_hints,
                        next_reconcile,
                    );
                    drop(host);
                    if loop_cancel.load(Ordering::Acquire) {
                        break;
                    }
                    let timeout = next_reconcile.saturating_duration_since(Instant::now());
                    wait_next(&runtime, source.as_ref(), &loop_wake, timeout);
                }
            })
            .expect("spawning the discovery thread must not fail");
        Self {
            cancel,
            wake,
            thread: Some(thread),
        }
    }

    /// Stops the background thread and joins it (bounded: the loop body
    /// never blocks longer than one [`wait_next`] timeout/notify, so this
    /// returns promptly). Equivalent to dropping this value; both exist so
    /// a caller that wants to observe the join (tests; orderly runtime
    /// shutdown — discovery stops before plugins are destroyed,
    /// `src/runtime/lifecycle.rs`'s own wiring) can call it explicitly.
    pub fn stop(mut self) {
        self.stop_inner();
    }

    fn stop_inner(&mut self) {
        self.cancel.store(true, Ordering::Release);
        self.wake.notify_waiters();
        if let Some(thread) = self.thread.take()
            && thread.join().is_err()
        {
            tracing::warn!("discovery thread panicked during shutdown");
        }
    }
}

impl Drop for DiscoveryService {
    fn drop(&mut self) {
        self.stop_inner();
    }
}

#[cfg(test)]
mod tests;
