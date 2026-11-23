//! Generic core discovery service (`docs/v2/implementation-plan.md` M1 §4,
//! phase d2b): drives [`PluginHost::process_appeared`]/
//! [`PluginHost::process_exited`]/[`PluginHost::foreground_changed`] from a
//! pluggable [`ProcessSource`] seam — the real OS process list and event
//! backends in production (platform-specific modules, later units), a
//! synthetic in-memory source in this module's own tests. Owns no platform
//! code itself: only the reconciliation loop (periodic full enumeration as
//! a bounded-loss backstop, targeted [`ProcessSource::drain_events`]/
//! [`ProcessSource::inspect`] for cheap incremental updates when a source
//! provides them), the unified enumeration-cost pre-filter sync
//! (`MatcherEngine::prefilter` -> [`ProcessSource::set_prefilter`], §6.1),
//! the matcher-set generation sync (`MatcherEngine::generation`, P2: a
//! plugin installed or torn down after discovery already tracked a
//! process re-offers every tracked record), and PID-generation
//! correctness: a pid whose freshly observed occurrence (`start_time`)
//! differs from what this service last reported for it is a reused pid,
//! reported as an exit immediately followed by a fresh appearance, never
//! silently folded into the old occurrence. Identity is `start_time`
//! alone (not name/path/command_line/bitness, P1): a still-running
//! process that rewrites its own argv (a setproctitle-style
//! `/proc/<pid>/cmdline` change) keeps the same `start_time` and must not
//! churn the session.
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
use super::matcher::Prefilter;

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

    /// Updates the source's own enumeration-cost pre-filter (§6.1,
    /// [`Prefilter`]'s own doc comment) to `filter`, derived from every
    /// registered plugin's hint names (`MatcherEngine::prefilter`). A
    /// real backend skips opening/reading anything else about a process
    /// [`Prefilter::allows`] rejects; on Linux this also fills the
    /// bounded in-kernel/CN_PROC pre-filter, which can fail independently
    /// of the userspace side (logged and returned, never panicking). The
    /// default is a correct no-op for a source with nothing to narrow.
    fn set_prefilter(&self, _filter: &Prefilter) -> io::Result<()> {
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
/// occurrence (identity, not a generation counter): a different
/// `start_time` means the pid was reused by a different process between
/// the two observations, never a genuine update to a still-running one
/// (P1: 1.x's own Windows `creation_time` cache, `src/instance/discovery.rs`,
/// generalized to both targets through `ProcessRecord::start_time`).
/// Every other field (name/path/command_line/bitness) may legitimately
/// change without the process ever exiting — in particular a
/// setproctitle-style argv rewrite, which must not churn the session.
fn same_occurrence(a: &ProcessRecord, b: &ProcessRecord) -> bool {
    a.start_time == b.start_time
}

/// Feeds one observed `record` to `host`, first reporting an exit for
/// whatever occurrence `tracked` already holds for this pid if the two
/// disagree (PID-generation correctness: a reused pid is a new process,
/// reported as an exit immediately followed by a fresh appearance, never
/// folded into the old occurrence). A repeat observation of the same
/// occurrence with an unchanged name/path is **not** re-sent to `host`:
/// [`PluginHost::process_appeared`] is itself idempotent for an
/// already-tracked pid (`SessionRegistry::attach`'s own no-op rule), but
/// skipping the call here avoids needless matcher/session lock traffic on
/// every reconciliation pass for a process that never changed.
///
/// A same-occurrence observation whose name or path *did* change (P2: an
/// in-place rename, e.g. Wine's `set_process_name` calling
/// `prctl(PR_SET_NAME)` right after `execve` keeps the same pid and
/// `start_time`) is still never reported as an exit — `same_occurrence`
/// already established this is the same running process, and the host
/// has no detach reason for "renamed" (`docs/v2/plugin-abi.md` §6.2 lists
/// only `"exited"|"lost"|"superseded"|"declined"` for
/// `VOSU_OP_SESSION_DETACHED`). Instead this re-offers the updated record
/// to [`PluginHost::process_appeared`], which is idempotent either way: a
/// no-op for a pid `SessionRegistry` already attached (unaffected by a
/// rename that stops matching — §6.2's detach-reason list has no "no
/// longer matches" entry, so the already-attached session simply keeps
/// running, exactly 1.x's own pid-identified session lifecycle), and a
/// fresh attach for one whose new name/path a matcher now accepts that
/// did not accept the old one.
fn apply_appeared(
    host: &PluginHost,
    tracked: &mut HashMap<u32, ProcessRecord>,
    record: ProcessRecord,
) {
    match tracked.get(&record.pid) {
        Some(previous) if same_occurrence(previous, &record) => {
            if previous.name != record.name || previous.path != record.path {
                host.process_appeared(record.clone());
            }
        }
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

/// Syncs the unified enumeration-cost pre-filter (`MatcherEngine::prefilter`,
/// §6.1) to `source` whenever it has changed since the last sync, and
/// reports whether it did: the caller forces a full reconcile on a
/// change, since a widening can newly admit a process the pre-filter
/// previously hid from `source.enumerate()`/the kernel event stream
/// entirely, so only a full pass is guaranteed to pick it up.
///
/// Always advances `last_filter` to `filter`, success or failure (P2):
/// retrying the exact same already-failed update on every pass — e.g. a
/// kernel comm-hint push [`ProcessSource::set_prefilter`] cannot apply —
/// would otherwise hot-loop this service (repeated `/proc` scans and log
/// spam) without ever making progress, since the failure is
/// deterministic for an unchanged input. A backend that implements
/// `set_prefilter` is itself responsible for applying what it safely can
/// even when it returns `Err` (`ProcessSource::set_prefilter`'s own doc
/// comment) — this function never second-guesses a partial success.
fn sync_prefilter(
    host: &PluginHost,
    source: &dyn ProcessSource,
    last_filter: &mut Option<Prefilter>,
) -> bool {
    let filter = host.prefilter();
    if last_filter.as_ref() == Some(&filter) {
        return false;
    }
    let applied = source.set_prefilter(&filter).is_ok();
    if !applied {
        tracing::warn!(
            "enumeration-cost pre-filter update failed; retrying only on the next change"
        );
    }
    *last_filter = Some(filter);
    applied
}

/// Syncs this host's own matcher-set generation (`MatcherEngine::generation`,
/// P2) and reports whether it changed since the last sync. The caller
/// forces a full reconcile on a change and then re-offers every tracked
/// record (`reoffer_tracked`): a plugin installed (or re-installed) after
/// discovery already tracked a non-matching process must still get a
/// chance to match it, which `apply_appeared`'s own same-occurrence skip
/// would otherwise suppress forever for an unchanged process.
fn sync_generation(host: &PluginHost, last_generation: &mut Option<u64>) -> bool {
    let generation = host.matcher_generation();
    if *last_generation == Some(generation) {
        return false;
    }
    *last_generation = Some(generation);
    true
}

/// Re-offers every currently tracked record to `host` (P2,
/// `sync_generation`'s own doc comment): `PluginHost::process_appeared`
/// is idempotent for a pid it already attached a session to
/// (`SessionRegistry::attach`'s own no-op rule), so this only ever has an
/// effect for a record no plugin matched the last time it was offered.
fn reoffer_tracked(host: &PluginHost, tracked: &HashMap<u32, ProcessRecord>) {
    for record in tracked.values() {
        host.process_appeared(record.clone());
    }
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

/// One discovery reconciliation pass: syncs the pre-filter and matcher
/// generation, then either a full reconcile (startup, due tick, a
/// pre-filter/generation change, or a source-requested reconcile) or a
/// targeted event batch. A generation change additionally re-offers
/// every tracked record once the full reconcile's own enumerate() has
/// run. Returns the deadline for the next full reconcile.
fn run_pass(
    host: &PluginHost,
    source: &dyn ProcessSource,
    tracked: &mut HashMap<u32, ProcessRecord>,
    last_filter: &mut Option<Prefilter>,
    last_generation: &mut Option<u64>,
    next_reconcile: Instant,
) -> Instant {
    let filter_changed = sync_prefilter(host, source, last_filter);
    let generation_changed = sync_generation(host, last_generation);
    let now = Instant::now();
    if filter_changed || generation_changed || now >= next_reconcile {
        full_reconcile(host, source, tracked);
        if generation_changed {
            reoffer_tracked(host, tracked);
        }
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
                let mut last_filter: Option<Prefilter> = None;
                let mut last_generation: Option<u64> = None;
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
                        &mut last_filter,
                        &mut last_generation,
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
        // `notify_one` (not `notify_waiters`): a wake between this
        // store and the loop's own next `wait_next` call stores a
        // permit that call consumes immediately, instead of being lost
        // the way `notify_waiters` would lose it for a `Notify` nobody
        // was already awaiting at the moment it fired (P3: `stop` must
        // not stall until `RECONCILE_INTERVAL`'s own timeout).
        self.wake.notify_one();
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

/// Linux [`ProcessSource`] backend (M1 phase d2b, unit b):
/// `vosu_sys::process::{enumerate_processes_matching, process_metadata_matching}`
/// for reads, `vosu_sys::events::LinuxProcessEvents` for push events when
/// available. Gated on the `linux-process-events` feature because
/// `vosu_sys::events` itself is (module doc comment).
#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
mod linux_source;
#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
pub use linux_source::LinuxSource;
mod foreground;
/// `ProcessRecord` mapping shared by [`linux_source`] and
/// `src/runtime/discovery.rs::PollingLinuxSource` (P3): gated only on
/// `target_os = "linux"`, so a `--no-default-features` build without the
/// `linux-process-events` feature still reaches it
/// (`linux_common`'s own module doc comment).
#[cfg(target_os = "linux")]
pub mod linux_common;
pub use foreground::FOREGROUND_POLL_INTERVAL;
#[cfg(windows)]
pub use foreground::ForegroundPoller;

mod windows_source;
#[cfg(windows)]
pub use windows_source::WindowsProcessSource;

#[cfg(test)]
mod tests;
