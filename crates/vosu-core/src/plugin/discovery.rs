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
//!
//! Cadence and exit confirmation (DI, `docs/v2/implementation-plan.md` M1
//! §4): a flat reconciliation interval regressed Windows detection
//! latency from 1.x's 100 ms to 2 s, so [`DiscoveryCadence`] instead
//! mirrors 1.x's own cadence exactly (`src/instance.rs:33-42`,
//! `src/instance/runtime.rs:262-266`) — fast (100 ms) while nothing is
//! tracked, 1 s while something is, relaxing to 5 s only while
//! [`ProcessSource::events_healthy`] reports a healthy push-event channel
//! and nothing just failed or overflowed [`MAX_PENDING_EXIT_CONFIRMATIONS`]
//! (a bare pre-filter/matcher-generation change or the source's own
//! loss flag never does, matching 1.x's own `retry_reconcile`, which
//! only a real failure or an eviction sets). An `exit` hint is not
//! trusted outright either: [`ProcessSource::is_alive`] re-confirms it,
//! and a pid that still reports alive is re-checked with exponential
//! backoff (10 ms up to 250 ms, at most
//! [`MAX_PENDING_EXIT_CONFIRMATIONS`] pending at once) instead of being
//! detached — a push event can race a process that is dying but has not
//! yet been reaped.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::Notify;
use vosu_sys::process::is_process_alive;

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
    /// cadence.
    pub needs_reconcile: bool,
}

/// Full reconciliation cadence (module doc comment, DI): every
/// `enumerate()`-driven full pass re-derives ground truth from scratch,
/// no less often than this. [`DiscoveryService::start`] always uses
/// [`DiscoveryCadence::default`]; no production caller configures
/// another value today, but tests drive `run_pass` directly with their
/// own instance to pin deadlines without a wall-clock sleep.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryCadence {
    /// Cadence while no process is tracked at all: 1.x polled this fast
    /// specifically so a cold-started osu! is still detected quickly
    /// (`src/instance.rs:33` `EMPTY_DISCOVERY_INTERVAL`).
    pub empty: Duration,
    /// Cadence while at least one process is tracked and either the
    /// source's own event channel is unhealthy/absent or this pass was
    /// `degraded` -- a real enumerate/inspect failure or a
    /// `MAX_PENDING_EXIT_CONFIRMATIONS` eviction, never a bare loss
    /// flag alone (`cadence_interval`'s own doc comment, which keeps
    /// [`Self::events_healthy`] for that case instead): a real failure
    /// or an eviction needs confirming soon, not after a long
    /// health-gated wait
    /// (`src/instance.rs:34` `ACTIVE_DISCOVERY_INTERVAL`).
    pub active: Duration,
    /// Cadence while at least one process is tracked and
    /// [`ProcessSource::events_healthy`] reports the event channel is
    /// healthy: the backstop only needs to catch a missed event, which a
    /// healthy channel already makes rare
    /// (`src/instance.rs:36` `EVENT_RECONCILIATION_INTERVAL`).
    pub events_healthy: Duration,
}

impl Default for DiscoveryCadence {
    fn default() -> Self {
        Self {
            empty: Duration::from_millis(100),
            active: Duration::from_secs(1),
            events_healthy: Duration::from_secs(5),
        }
    }
}

/// Exit-confirmation backoff bounds (module doc comment; mirrors
/// `src/instance.rs:37-42`).
const MAX_PENDING_EXIT_CONFIRMATIONS: usize = 128;
const EXIT_CONFIRMATION_INITIAL_BACKOFF: Duration = Duration::from_millis(10);
const EXIT_CONFIRMATION_MAX_BACKOFF: Duration = Duration::from_millis(250);

/// A still-pending exit hint awaiting liveness reconfirmation (module doc
/// comment, exit confirmation). Keyed by pid in [`run_pass`]'s own
/// state; `start_time` guards an entry against resolving the wrong
/// occurrence once something else (a reused pid, or the same pid simply
/// reconfirmed alive through another path) has superseded it.
#[derive(Debug, Clone, Copy)]
struct PendingExit {
    start_time: u64,
    due: Instant,
    backoff: Duration,
}

/// [`run_pass`]'s own mutable discovery state, bundled into one
/// parameter (P3 review: these four fields always travel together, so a
/// small struct removes the need for a `too_many_arguments` exception
/// rather than only excusing it): the tracked occurrence per pid, every
/// still-pending exit confirmation, and the last pre-filter/matcher
/// generation this service synced.
#[derive(Default)]
struct DiscoveryState {
    tracked: HashMap<u32, ProcessRecord>,
    pending_exits: HashMap<u32, PendingExit>,
    last_filter: Option<Prefilter>,
    last_generation: Option<u64>,
}

/// Snapshot of [`DiscoveryService`]'s own lifetime counters
/// (`DiscoveryService::stats`), useful for diagnostics and tests.
/// `full_scans`/`targeted_inspections` count only a *successful* call —
/// a failed [`ProcessSource::enumerate`]/[`ProcessSource::inspect`] is
/// logged and forces a retry/reconcile but is never counted as work
/// done. `reconciliation_requests` counts only an out-of-cadence full
/// reconcile this service itself forced (a pre-filter/matcher-generation
/// change, or [`apply_batch`] asking for one), never the plain periodic
/// tick. `process_events` is a live read of
/// [`ProcessSource::event_count`]: this service never duplicates a
/// source backend's own internal bookkeeping.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct DiscoveryStats {
    pub full_scans: u64,
    pub targeted_inspections: u64,
    pub event_wakes: u64,
    pub reconciliation_requests: u64,
    pub process_events: u64,
}

/// [`DiscoveryService`]'s own lifetime atomics backing [`DiscoveryStats`]
/// (`DiscoveryStats`'s own doc comment explains which fields these
/// back).
#[derive(Debug, Default)]
struct DiscoveryCounters {
    last_scan_error: std::sync::Mutex<Option<String>>,
    full_scans: AtomicU64,
    targeted_inspections: AtomicU64,
    event_wakes: AtomicU64,
    reconciliation_requests: AtomicU64,
}

/// What the discovery service needs from an OS- or test-specific process
/// source (M1 §4, phase d2b): full enumeration for startup and periodic
/// reconciliation, optional live event hints a push-capable backend
/// provides (Linux eBPF/CN_PROC), and the live comm-hint pre-filter sink.
/// A pure-poll source (Windows, the testkit default) implements only
/// [`Self::enumerate`] and relies on this service's own
/// [`DiscoveryCadence`] entirely — every other method's default already
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
    /// full reconcile regardless of cadence. The default — an
    /// always-empty, never-reconcile-forcing batch — is exactly correct
    /// for a pure-poll source: [`DiscoveryCadence`] alone already drives
    /// it.
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

    /// A [`Notify`] this service awaits alongside its own cadence
    /// timeout, woken whenever new events are ready for
    /// [`Self::drain_events`]. `None` (the default) for a source with no
    /// event channel: the service then relies on the timeout alone,
    /// exactly as a pure-poll source needs.
    fn notifier(&self) -> Option<&Notify> {
        None
    }

    /// Human-readable push-event backend name for diagnostics/logging
    /// (`src/runtime/discovery.rs::start_discovery` logs `"Process
    /// events backend: {name}"`, mirroring 1.x's own startup log). The
    /// default, `"polling"`, is exactly right for a pure-poll source.
    fn backend_name(&self) -> &'static str {
        "polling"
    }

    /// Whether this source's own push-event channel is currently healthy
    /// (DI: 1.x's `discovery_events_healthy`/`process_events_healthy`).
    /// [`DiscoveryCadence::events_healthy`] only applies while this is
    /// `true`; the default, `false`, is exactly right for a pure-poll
    /// source (no event channel is ever "healthy").
    fn events_healthy(&self) -> bool {
        false
    }

    /// Total process-level events this source's own backend has ever
    /// observed, for `DiscoveryStats::process_events`. The default, 0,
    /// is exactly right for a pure-poll source.
    fn event_count(&self) -> u64 {
        0
    }

    /// Total number of times this source's own backend requested a full
    /// reconcile (a bounded internal queue overflowing, for instance),
    /// independent of whether [`DiscoveryService`] actually acted on it.
    /// The default, 0, is exactly right for a pure-poll source.
    fn reconciliation_request_count(&self) -> u64 {
        0
    }

    /// Whether `pid` is still running, for exit confirmation (module doc
    /// comment): an `exit` hint this source itself reported is
    /// re-verified through this method, with backoff, instead of being
    /// trusted outright — mirrors 1.x's own confirmation
    /// (`src/instance.rs:37-42`). The default asks the OS directly
    /// (`vosu_sys::process::is_process_alive`); a synthetic test source
    /// overrides this to answer from its own in-memory state instead of
    /// the real OS.
    fn is_alive(&self, pid: u32) -> bool {
        is_process_alive(pid)
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
///
/// A reused-pid supersede additionally drops any pending exit
/// confirmation for the old occurrence (exit confirmation, module doc
/// comment): `start_time` already guards [`resolve_due_pending_exits`]
/// against resolving the wrong occurrence, but the old entry is now
/// certainly stale (a new process owns this pid) and would otherwise
/// sit pending until its own backoff deadline for nothing. A
/// same-occurrence observation — including a full reconcile's own
/// repeat sighting — leaves a pending entry untouched (P2 review): the
/// `exit` hint that created it has already been consumed, and
/// `start_time` alone cannot tell "still running, exactly as the
/// pending entry expects" apart from "a loss event's full reconcile
/// re-derived ground truth while the real exit is still only
/// unconfirmed" — only [`resolve_due_pending_exits`]'s own
/// `ProcessSource::is_alive` recheck may resolve it, exactly 1.x's own
/// `pending_exits.retain` keyed by generation, never cleared by a mere
/// re-sighting.
fn apply_appeared(
    host: &PluginHost,
    tracked: &mut HashMap<u32, ProcessRecord>,
    pending_exits: &mut HashMap<u32, PendingExit>,
    record: ProcessRecord,
) {
    match tracked.get(&record.pid) {
        Some(previous) if same_occurrence(previous, &record) => {
            if previous.name != record.name || previous.path != record.path {
                host.process_appeared(record.clone());
            }
        }
        Some(_) => {
            pending_exits.remove(&record.pid);
            host.process_exited(record.pid);
            host.process_appeared(record.clone());
        }
        None => host.process_appeared(record.clone()),
    }
    tracked.insert(record.pid, record);
}

/// Reports `pid` exited to `host`, stops tracking it, and drops any
/// pending exit confirmation for it. Safe to call for a pid this service
/// never reported as appeared (e.g. one that never matched any plugin's
/// `[match]` table): [`PluginHost::process_exited`] is itself a no-op for
/// a pid it is not tracking a session for.
fn apply_exited(
    host: &PluginHost,
    tracked: &mut HashMap<u32, ProcessRecord>,
    pending_exits: &mut HashMap<u32, PendingExit>,
    pid: u32,
) {
    tracked.remove(&pid);
    pending_exits.remove(&pid);
    host.process_exited(pid);
}

/// Full ground-truth pass (bounded loss recovery): every process
/// `source.enumerate()` reports is applied through [`apply_appeared`];
/// every previously tracked pid absent from that list is applied through
/// [`apply_exited`]. Logs and leaves `tracked` unchanged on an
/// enumeration failure, so a transient failure neither drops every
/// tracked session nor wedges the service — the next cadence tick (or
/// targeted event) retries. Returns whether the pass succeeded; only a
/// success counts toward [`DiscoveryStats::full_scans`].
fn full_reconcile(
    host: &PluginHost,
    source: &dyn ProcessSource,
    tracked: &mut HashMap<u32, ProcessRecord>,
    pending_exits: &mut HashMap<u32, PendingExit>,
    counters: &DiscoveryCounters,
) -> bool {
    let current = match source.enumerate() {
        Ok(current) => current,
        Err(error) => {
            let message = error.to_string();
            let changed = {
                let mut previous = counters
                    .last_scan_error
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                if previous.as_ref() == Some(&message) {
                    false
                } else {
                    *previous = Some(message.clone());
                    true
                }
            };
            if changed {
                tracing::error!("Process discovery scan failed: {message}");
            }
            return false;
        }
    };
    counters
        .last_scan_error
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .take();
    let mut seen = HashSet::with_capacity(current.len());
    for record in current {
        seen.insert(record.pid);
        apply_appeared(host, tracked, pending_exits, record);
    }
    let gone: Vec<u32> = tracked
        .keys()
        .copied()
        .filter(|pid| !seen.contains(pid))
        .collect();
    for pid in gone {
        apply_exited(host, tracked, pending_exits, pid);
    }
    counters.full_scans.fetch_add(1, Ordering::Relaxed);
    true
}

/// Applies one `exit` hint (exit confirmation, module doc comment):
/// detaches outright only once [`ProcessSource::is_alive`] agrees the
/// pid is actually gone; while it still reports alive, schedules (or
/// keeps, backoff unchanged) a pending re-check instead — mirrors
/// `src/instance.rs:37-42`. A pid this service is not tracking at all
/// needs no confirmation (nothing to detach). Returns whether the
/// [`MAX_PENDING_EXIT_CONFIRMATIONS`] bound had to evict another
/// still-pending entry to make room: the caller treats that the same as
/// an inspect failure, forcing a reconcile, since the evicted pid's own
/// eventual exit can now only be caught by a full reconcile.
fn handle_exit_hint(
    host: &PluginHost,
    source: &dyn ProcessSource,
    tracked: &mut HashMap<u32, ProcessRecord>,
    pending_exits: &mut HashMap<u32, PendingExit>,
    now: Instant,
    pid: u32,
) -> bool {
    let Some(record) = tracked.get(&pid) else {
        pending_exits.remove(&pid);
        return false;
    };
    if source.is_alive(pid) {
        let mut evicted_another = false;
        if !pending_exits.contains_key(&pid)
            && pending_exits.len() >= MAX_PENDING_EXIT_CONFIRMATIONS
            && let Some(oldest) = pending_exits
                .iter()
                .max_by_key(|(_, pending)| pending.due)
                .map(|(&pid, _)| pid)
        {
            pending_exits.remove(&oldest);
            evicted_another = true;
        }
        pending_exits.entry(pid).or_insert(PendingExit {
            start_time: record.start_time,
            due: now + EXIT_CONFIRMATION_INITIAL_BACKOFF,
            backoff: EXIT_CONFIRMATION_INITIAL_BACKOFF,
        });
        evicted_another
    } else {
        pending_exits.remove(&pid);
        apply_exited(host, tracked, pending_exits, pid);
        false
    }
}

/// Re-checks every pending exit confirmation whose backoff deadline is
/// due as of `now` (exit confirmation, module doc comment): confirmed
/// dead detaches it; still alive doubles the backoff (capped at
/// [`EXIT_CONFIRMATION_MAX_BACKOFF`]); an entry whose pid no longer
/// tracks the same occurrence it was scheduled for (superseded
/// elsewhere, e.g. [`apply_appeared`]'s own reused-pid clear) is simply
/// dropped, since whatever superseded it already resolved the session
/// correctly.
fn resolve_due_pending_exits(
    host: &PluginHost,
    source: &dyn ProcessSource,
    tracked: &mut HashMap<u32, ProcessRecord>,
    pending_exits: &mut HashMap<u32, PendingExit>,
    now: Instant,
) {
    let due: Vec<u32> = pending_exits
        .iter()
        .filter(|(_, pending)| pending.due <= now)
        .map(|(&pid, _)| pid)
        .collect();
    for pid in due {
        let Some(pending) = pending_exits.get(&pid).copied() else {
            continue;
        };
        let still_current = tracked
            .get(&pid)
            .is_some_and(|record| record.start_time == pending.start_time);
        if !still_current {
            pending_exits.remove(&pid);
        } else if source.is_alive(pid) {
            let backoff = (pending.backoff * 2).min(EXIT_CONFIRMATION_MAX_BACKOFF);
            pending_exits.insert(
                pid,
                PendingExit {
                    start_time: pending.start_time,
                    due: now + backoff,
                    backoff,
                },
            );
        } else {
            pending_exits.remove(&pid);
            apply_exited(host, tracked, pending_exits, pid);
        }
    }
}

/// What [`apply_batch`] learned from one event batch.
struct BatchOutcome {
    /// Whether a full reconcile is now needed — the source itself asked
    /// for one ([`ProcessEventBatch::needs_reconcile`]), an exit
    /// confirmation evicted another pending entry under
    /// [`MAX_PENDING_EXIT_CONFIRMATIONS`], or a targeted `inspect`
    /// failed.
    needs_reconcile: bool,
    /// Whether the need came from a real failure or eviction, never a
    /// bare source loss flag alone (P3 review, [`cadence_interval`]'s
    /// own doc comment): 1.x's own `retry_reconcile` is set only by an
    /// inspect failure or an eviction (`src/instance/runtime.rs:262-266`),
    /// never by `lost` on its own, so only this forces the fast cadence
    /// — a bare loss flag still keeps the healthy-events interval.
    degraded: bool,
}

/// Applies one `source.drain_events()` batch: every `exit` pid through
/// [`handle_exit_hint`], every `inspect` pid re-read through
/// [`ProcessSource::inspect`] and applied through [`apply_appeared`] (or
/// [`apply_exited`] if it is already gone) — a successful `inspect`
/// (either outcome) counts toward [`DiscoveryStats::targeted_inspections`].
/// A failed inspect is never counted.
fn apply_batch(
    host: &PluginHost,
    source: &dyn ProcessSource,
    tracked: &mut HashMap<u32, ProcessRecord>,
    pending_exits: &mut HashMap<u32, PendingExit>,
    counters: &DiscoveryCounters,
    now: Instant,
    batch: ProcessEventBatch,
) -> BatchOutcome {
    let mut needs_reconcile = batch.needs_reconcile;
    let mut degraded = false;
    for pid in batch.exit {
        if handle_exit_hint(host, source, tracked, pending_exits, now, pid) {
            needs_reconcile = true;
            degraded = true;
        }
    }
    for pid in batch.inspect {
        match source.inspect(pid) {
            Ok(Some(record)) => {
                apply_appeared(host, tracked, pending_exits, record);
                counters
                    .targeted_inspections
                    .fetch_add(1, Ordering::Relaxed);
            }
            Ok(None) => {
                apply_exited(host, tracked, pending_exits, pid);
                counters
                    .targeted_inspections
                    .fetch_add(1, Ordering::Relaxed);
            }
            Err(error) => {
                tracing::error!("Targeted process inspection failed for PID {pid}: {error}");
                needs_reconcile = true;
                degraded = true;
            }
        }
    }
    BatchOutcome {
        needs_reconcile,
        degraded,
    }
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

/// The next full-reconcile deadline's interval from `now` (DI,
/// [`DiscoveryCadence`]'s own doc comment). `degraded` is `true` only
/// for a real failure or a `MAX_PENDING_EXIT_CONFIRMATIONS` eviction
/// (`BatchOutcome::degraded`'s own doc comment) — never for a bare
/// pre-filter/matcher-generation change or the source's own loss flag
/// alone (P3 review): 1.x's own `retry_reconcile` is set only by an
/// enumeration/inspect failure or an eviction, never by `lost` alone
/// (`src/instance/runtime.rs:262-266`), so only a real `degraded` pass
/// drops to the fast cadence regardless of event health.
pub fn cadence_interval(
    cadence: &DiscoveryCadence,
    tracked_empty: bool,
    events_healthy: bool,
    degraded: bool,
) -> Duration {
    if events_healthy && !degraded {
        cadence.events_healthy
    } else if tracked_empty {
        cadence.empty
    } else {
        cadence.active
    }
}

/// Blocks the calling (dedicated, non-worker) thread until `source`'s own
/// event [`Notify`] fires, `wake` fires (service stopping, or a hint-set
/// change worth checking promptly), or `timeout` elapses — whichever comes
/// first. Driven through `runtime.block_on`, never a worker (module doc
/// comment). Reports which of the three actually woke it, so the caller
/// can count [`DiscoveryStats::event_wakes`] only for a genuine source
/// event, never a timeout or the service's own stop signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WakeReason {
    SourceEvent,
    ServiceWake,
    Timeout,
}

fn wait_next(
    runtime: &Handle,
    source: &dyn ProcessSource,
    wake: &Notify,
    timeout: Duration,
) -> WakeReason {
    runtime.block_on(async {
        match source.notifier() {
            Some(notifier) => {
                tokio::select! {
                    () = notifier.notified() => WakeReason::SourceEvent,
                    () = wake.notified() => WakeReason::ServiceWake,
                    () = tokio::time::sleep(timeout) => WakeReason::Timeout,
                }
            }
            None => {
                tokio::select! {
                    () = wake.notified() => WakeReason::ServiceWake,
                    () = tokio::time::sleep(timeout) => WakeReason::Timeout,
                }
            }
        }
    })
}

/// One discovery reconciliation pass: syncs the pre-filter and matcher
/// generation, then either a full reconcile (startup, due tick, a
/// pre-filter/generation change, or a source-requested reconcile) or a
/// targeted event batch (resolving any due exit confirmations first). A
/// generation change additionally re-offers every tracked record once
/// the full reconcile's own `enumerate()` has run. Takes `now` explicitly
/// (rather than reading [`Instant::now`] itself) so a test can pin
/// deadlines and backoff windows deterministically, without a wall-clock
/// sleep for ordering (`discovery/tests/mod.rs`). Returns the deadline
/// for the next full reconcile.
///
/// Only an out-of-cadence reconcile counts toward
/// [`DiscoveryStats::reconciliation_requests`] (that field's own doc
/// comment): a pre-filter/matcher-generation change that merely
/// coincides with an already-due periodic tick does not add a second
/// one — in particular the very first pass, whose `next_reconcile`
/// starts already due, is never counted even though `last_filter`/
/// `last_generation` always "change" from `None` on it.
fn run_pass(
    host: &PluginHost,
    source: &dyn ProcessSource,
    state: &mut DiscoveryState,
    cadence: &DiscoveryCadence,
    counters: &DiscoveryCounters,
    now: Instant,
    next_reconcile: Instant,
) -> Instant {
    let filter_changed = sync_prefilter(host, source, &mut state.last_filter);
    let generation_changed = sync_generation(host, &mut state.last_generation);
    let periodic_due = now >= next_reconcile;
    if filter_changed || generation_changed || periodic_due {
        if (filter_changed || generation_changed) && !periodic_due {
            counters
                .reconciliation_requests
                .fetch_add(1, Ordering::Relaxed);
        }
        if !full_reconcile(
            host,
            source,
            &mut state.tracked,
            &mut state.pending_exits,
            counters,
        ) {
            // Enumeration itself failed: retry promptly regardless of
            // tracked-set size or event health, instead of waiting out
            // whatever interval the formula below would otherwise pick
            // (`failed_full_scan_retries_early_and_counts_only_success`).
            return now + cadence.empty;
        }
        if generation_changed {
            reoffer_tracked(host, &state.tracked);
        }
        // Never `degraded`: a bare pre-filter/matcher-generation change
        // (or simply being due) is not a failure or an eviction
        // (`cadence_interval`'s own doc comment).
        return now
            + cadence_interval(
                cadence,
                state.tracked.is_empty(),
                source.events_healthy(),
                false,
            );
    }
    resolve_due_pending_exits(
        host,
        source,
        &mut state.tracked,
        &mut state.pending_exits,
        now,
    );
    let batch = source.drain_events();
    let outcome = apply_batch(
        host,
        source,
        &mut state.tracked,
        &mut state.pending_exits,
        counters,
        now,
        batch,
    );
    if outcome.needs_reconcile {
        counters
            .reconciliation_requests
            .fetch_add(1, Ordering::Relaxed);
        if !full_reconcile(
            host,
            source,
            &mut state.tracked,
            &mut state.pending_exits,
            counters,
        ) {
            return now + cadence.empty;
        }
        return now
            + cadence_interval(
                cadence,
                state.tracked.is_empty(),
                source.events_healthy(),
                outcome.degraded,
            );
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
    source: Arc<dyn ProcessSource>,
    counters: Arc<DiscoveryCounters>,
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
        let counters = Arc::new(DiscoveryCounters::default());
        let cadence = DiscoveryCadence::default();
        let loop_host = Arc::downgrade(host);
        let loop_cancel = Arc::clone(&cancel);
        let loop_wake = Arc::clone(&wake);
        let loop_source = Arc::clone(&source);
        let loop_counters = Arc::clone(&counters);
        let thread = thread::Builder::new()
            .name("vosu-discovery".to_owned())
            .spawn(move || {
                let mut state = DiscoveryState::default();
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
                        loop_source.as_ref(),
                        &mut state,
                        &cadence,
                        &loop_counters,
                        Instant::now(),
                        next_reconcile,
                    );
                    drop(host);
                    if loop_cancel.load(Ordering::Acquire) {
                        break;
                    }
                    let now = Instant::now();
                    // Wakes early for whichever is sooner: the next full
                    // reconcile, or the next due exit-confirmation
                    // backoff retry (module doc comment) — neither
                    // `wait_next`'s own source notifier nor the periodic
                    // timeout alone would otherwise retry a pending exit
                    // on its own fast schedule.
                    let deadline = state
                        .pending_exits
                        .values()
                        .map(|pending| pending.due)
                        .fold(next_reconcile, Instant::min);
                    let timeout = deadline.saturating_duration_since(now);
                    if wait_next(&runtime, loop_source.as_ref(), &loop_wake, timeout)
                        == WakeReason::SourceEvent
                    {
                        loop_counters.event_wakes.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
            .expect("spawning the discovery thread must not fail");
        Self {
            cancel,
            wake,
            thread: Some(thread),
            source,
            counters,
        }
    }

    /// A snapshot of this service's own lifetime counters
    /// (`DiscoveryStats`'s own doc comment).
    #[must_use]
    pub fn stats(&self) -> DiscoveryStats {
        DiscoveryStats {
            full_scans: self.counters.full_scans.load(Ordering::Relaxed),
            targeted_inspections: self.counters.targeted_inspections.load(Ordering::Relaxed),
            event_wakes: self.counters.event_wakes.load(Ordering::Relaxed),
            reconciliation_requests: self
                .counters
                .reconciliation_requests
                .load(Ordering::Relaxed),
            process_events: self.source.event_count(),
        }
    }

    /// Stops the background thread and joins it (bounded: the loop body
    /// never blocks longer than one `wait_next` timeout/notify, so this
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
        // not stall until the cadence's own timeout).
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
/// `ProcessRecord` mapping shared by `linux_source` and
/// `src/runtime/discovery.rs::PollingLinuxSource` (P3): gated only on
/// `target_os = "linux"`, so a `--no-default-features` build without the
/// `linux-process-events` feature still reaches it
/// (`linux_common`'s own module doc comment).
#[cfg(target_os = "linux")]
pub mod linux_common;
pub use foreground::FOREGROUND_POLL_INTERVAL;
#[cfg(windows)]
pub use foreground::ForegroundPoller;
#[cfg(any(windows, test))]
pub(crate) use foreground::foreground_owner_changed;

mod windows_source;
#[cfg(windows)]
pub use windows_source::WindowsProcessSource;

#[cfg(test)]
mod tests;
