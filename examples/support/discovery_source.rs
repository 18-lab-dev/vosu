//! C5: a minimal reconciliation loop built directly on
//! `vosu_core::plugin::{LinuxSource, Prefilter, ProcessSource}`, replacing
//! the 1.x `InstanceRuntime<NativeProcessProvider, _>` the process-event
//! examples used before core discovery existed
//! (`m1-remainder-plan.md` §5 unit C5).
//!
//! This intentionally does *not* reuse `vosu_core::plugin::DiscoveryService`:
//! that type drives a `PluginHost`'s plugin-matching sessions, which these
//! examples have no use for (they only need pid presence and the same
//! counters `DiscoveryStats` exposes). Per C5's own file list, the
//! examples instead exercise `ProcessSource` directly — `set_prefilter`,
//! `notifier`, `drain_events`, `inspect`, `enumerate` — and this module is
//! the small amount of glue that turns those calls into a background
//! thread plus a `watch` channel, mirroring `DiscoveryService`'s own
//! thread/`Notify`/counters/cadence shape
//! (`crates/vosu-core/src/plugin/discovery.rs`) closely enough to measure
//! the same things, without a `PluginHost`.

use std::collections::HashSet;
use std::error::Error;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::{Notify, watch};
use vosu_core::plugin::{
    DiscoveryCadence, LinuxSource, MatchPlatform, MatcherEngine, Prefilter, ProcessSource,
    cadence_interval,
};
use vosu_plugin_api::PluginManifest;

/// Builds the Linux pre-filter the real `vosu-osu` plugin would register
/// (`MatcherEngine::prefilter`, §6.1), from its own bundled manifest
/// (`m1-remainder-plan.md` §5 unit C5's own `Prefilter::Names(hints parsed
/// from vosu_osu::MANIFEST)` instruction), rather than a hand-maintained
/// hint list that could drift from the manifest.
fn osu_prefilter() -> Result<Prefilter, Box<dyn Error>> {
    let manifest = PluginManifest::parse(vosu_osu::MANIFEST)?;
    let table = manifest
        .matcher
        .clone()
        .ok_or("vosu-osu's manifest.toml has no [match] table")?;
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    engine.register(manifest.id.clone(), table)?;
    Ok(engine.prefilter())
}

#[derive(Debug, Default)]
struct Counters {
    full_scans: AtomicU64,
    targeted_inspections: AtomicU64,
    event_wakes: AtomicU64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WakeReason {
    Timeout,
    Stopped,
    SourceEvent,
}

/// Blocks the calling (dedicated, non-worker) thread until `source`'s own
/// event `Notify` fires, `wake` fires (stopping), or `timeout` elapses.
/// Driven through `runtime.block_on`, never a Tokio worker (mirrors
/// `DiscoveryService`'s own `wait_next`, `crates/vosu-core/src/plugin/discovery.rs`).
fn wait_next(runtime: &Handle, source: &LinuxSource, wake: &Notify, timeout: Duration) -> WakeReason {
    runtime.block_on(async {
        let source_event = async {
            match ProcessSource::notifier(source) {
                Some(notifier) => notifier.notified().await,
                None => std::future::pending::<()>().await,
            }
        };
        tokio::select! {
            () = wake.notified() => WakeReason::Stopped,
            () = source_event => WakeReason::SourceEvent,
            () = tokio::time::sleep(timeout) => WakeReason::Timeout,
        }
    })
}

/// Full ground-truth pass: replaces `tracked` with exactly what
/// `source.enumerate()` reports (already narrowed by the pre-filter).
/// Leaves `tracked` unchanged on a failed enumeration, same as
/// `DiscoveryService`'s own `full_reconcile`. `None` on a failed
/// enumeration (the caller falls back to `cadence.empty` for the retry,
/// mirroring core's own `run_pass`); `Some(changed)` on success.
fn full_reconcile(source: &LinuxSource, tracked: &mut HashSet<u32>, counters: &Counters) -> Option<bool> {
    match source.enumerate() {
        Ok(records) => {
            let observed: HashSet<u32> = records.iter().map(|record| record.pid).collect();
            counters.full_scans.fetch_add(1, Ordering::Relaxed);
            let changed = observed != *tracked;
            *tracked = observed;
            Some(changed)
        }
        Err(error) => {
            eprintln!("discovery_source: enumerate failed, keeping prior tracked set: {error}");
            None
        }
    }
}

/// Applies one `source.drain_events()` batch: every `exit` pid removed
/// outright, every `inspect` pid re-read through `ProcessSource::inspect`
/// and added/removed accordingly. Returns `(changed, needs_reconcile)`:
/// `needs_reconcile` is the source's own
/// `ProcessEventBatch::needs_reconcile` flag, or `true` when an `inspect`
/// itself failed (mirrors core's `BatchOutcome`, `discovery.rs:515-530` —
/// this harness has no pending-exit-confirmation eviction path to also
/// fold in, since it applies an `exit` hint outright rather than
/// reconfirming it).
fn apply_batch(source: &LinuxSource, tracked: &mut HashSet<u32>, counters: &Counters) -> (bool, bool) {
    let batch = source.drain_events();
    let mut changed = false;
    let mut needs_reconcile = batch.needs_reconcile;
    for pid in batch.exit {
        changed |= tracked.remove(&pid);
    }
    for pid in batch.inspect {
        match source.inspect(pid) {
            Ok(Some(record)) => {
                counters.targeted_inspections.fetch_add(1, Ordering::Relaxed);
                changed |= tracked.insert(record.pid);
            }
            Ok(None) => {
                counters.targeted_inspections.fetch_add(1, Ordering::Relaxed);
                changed |= tracked.remove(&pid);
            }
            Err(error) => {
                eprintln!("discovery_source: inspect({pid}) failed: {error}");
                needs_reconcile = true;
            }
        }
    }
    (changed, needs_reconcile)
}

/// Owns the background reconciliation thread and the `LinuxSource` it
/// drives. Replaces `InstanceRuntime<NativeProcessProvider, _>` for the
/// process-event examples (C5): the examples only need pid presence plus
/// discovery counters, not plugin-matched sessions.
pub(super) struct DiscoverySource {
    cancel: Arc<AtomicBool>,
    wake: Arc<Notify>,
    thread: Option<JoinHandle<()>>,
    source: Arc<LinuxSource>,
    counters: Arc<Counters>,
    current: watch::Receiver<Arc<HashSet<u32>>>,
}

impl DiscoverySource {
    /// Opens a `LinuxSource` (`VOSU_PROCESS_EVENTS`-selected; callers set
    /// that env var from the CLI `--mode` flag before calling this — 1.x
    /// instead passed the backend choice directly to
    /// `NativeProcessProvider::open_process_events`, since it predates
    /// `LinuxSource`'s env-var selection), syncs the osu! pre-filter
    /// (`osu_prefilter`) to it, and starts the background reconciliation
    /// thread.
    pub(super) fn start(runtime: Handle) -> Result<Self, Box<dyn Error>> {
        let source = Arc::new(LinuxSource::open());
        let prefilter = osu_prefilter()?;
        source.set_prefilter(&prefilter)?;

        let (tx, rx) = watch::channel(Arc::new(HashSet::new()));
        let cancel = Arc::new(AtomicBool::new(false));
        let wake = Arc::new(Notify::new());
        let counters = Arc::new(Counters::default());

        let loop_cancel = Arc::clone(&cancel);
        let loop_wake = Arc::clone(&wake);
        let loop_source = Arc::clone(&source);
        let loop_counters = Arc::clone(&counters);
        let thread = thread::Builder::new()
            .name("bench-discovery".to_owned())
            .spawn(move || {
                let cadence = DiscoveryCadence::default();
                let mut tracked: HashSet<u32> = HashSet::new();
                // Due immediately: the first pass is always a full
                // reconcile, regardless of what `loop_source` reports
                // before then.
                let mut next_reconcile = Instant::now();
                while !loop_cancel.load(Ordering::Acquire) {
                    let now = Instant::now();
                    let periodic_due = now >= next_reconcile;
                    let (changed, interval) = if periodic_due {
                        match full_reconcile(loop_source.as_ref(), &mut tracked, &loop_counters) {
                            Some(changed) => (
                                changed,
                                cadence_interval(
                                    &cadence,
                                    tracked.is_empty(),
                                    loop_source.events_healthy(),
                                    false,
                                ),
                            ),
                            // Enumeration itself failed: retry promptly,
                            // mirroring core's own `run_pass` (DI).
                            None => (false, cadence.empty),
                        }
                    } else {
                        let (batch_changed, needs_reconcile) =
                            apply_batch(loop_source.as_ref(), &mut tracked, &loop_counters);
                        if needs_reconcile {
                            match full_reconcile(loop_source.as_ref(), &mut tracked, &loop_counters)
                            {
                                Some(reconciled_changed) => (
                                    batch_changed || reconciled_changed,
                                    cadence_interval(
                                        &cadence,
                                        tracked.is_empty(),
                                        loop_source.events_healthy(),
                                        true,
                                    ),
                                ),
                                None => (batch_changed, cadence.empty),
                            }
                        } else {
                            // No out-of-cadence reconcile forced: the
                            // already-scheduled periodic deadline stands
                            // (mirrors core's own `run_pass`, which
                            // returns `next_reconcile` unchanged here).
                            (batch_changed, next_reconcile.saturating_duration_since(now))
                        }
                    };
                    next_reconcile = now + interval;
                    if changed {
                        let _ = tx.send(Arc::new(tracked.clone()));
                    }
                    if loop_cancel.load(Ordering::Acquire) {
                        break;
                    }
                    let timeout = next_reconcile.saturating_duration_since(Instant::now());
                    if wait_next(&runtime, loop_source.as_ref(), &loop_wake, timeout)
                        == WakeReason::SourceEvent
                    {
                        loop_counters.event_wakes.fetch_add(1, Ordering::Relaxed);
                    }
                }
            })
            .expect("spawning the example discovery thread must not fail");

        Ok(Self {
            cancel,
            wake,
            thread: Some(thread),
            source,
            counters,
            current: rx,
        })
    }

    /// A `watch::Receiver` over the currently tracked pid set, for a
    /// caller that wants to await a specific pid's presence/absence
    /// (`wait_for_pid`/`wait_for_new_pid` in the examples).
    pub(super) fn subscribe_current(&self) -> watch::Receiver<Arc<HashSet<u32>>> {
        self.current.clone()
    }

    pub(super) fn discovery_scan_count(&self) -> u64 {
        self.counters.full_scans.load(Ordering::Relaxed)
    }

    pub(super) fn targeted_inspection_count(&self) -> u64 {
        self.counters.targeted_inspections.load(Ordering::Relaxed)
    }

    pub(super) fn process_event_count(&self) -> u64 {
        self.source.event_count()
    }

    pub(super) fn event_wake_count(&self) -> u64 {
        self.counters.event_wakes.load(Ordering::Relaxed)
    }

    /// `DiscoveryStats::reconciliation_requests`'s own baseline (1.x:
    /// `src/instance/discovery.rs:263-267`; core: `discovery.rs:906-909`):
    /// the source backend's own newly latched request count, counted
    /// once by `LinuxProcessEvents` itself
    /// (`crates/vosu-sys/src/events.rs:146-150, 527-537`) and never
    /// re-counted here — this harness forces the reconcile that count
    /// implies (`apply_batch`'s own `needs_reconcile`) but does not keep
    /// a second tally of it.
    pub(super) fn reconciliation_request_count(&self) -> u64 {
        self.source.reconciliation_request_count()
    }

    pub(super) fn process_events_healthy(&self) -> bool {
        self.source.events_healthy()
    }

    /// Stops the background thread and joins it off the calling Tokio
    /// worker (`AGENTS.md`: no blocking wait on a worker), mirroring
    /// `DiscoveryService::stop`.
    pub(super) async fn shutdown(mut self) {
        self.cancel.store(true, Ordering::Release);
        self.wake.notify_one();
        if let Some(thread) = self.thread.take() {
            let _ = tokio::task::spawn_blocking(move || thread.join()).await;
        }
    }
}

impl Drop for DiscoverySource {
    fn drop(&mut self) {
        // Best-effort only (mirrors `DiscoveryService`'s `Drop`, but never
        // blocks here): a caller that wants the join to complete uses
        // `shutdown().await` instead. Letting the thread notice `cancel`
        // on its own next wake is enough for an example process that is
        // about to exit anyway.
        self.cancel.store(true, Ordering::Release);
        self.wake.notify_one();
    }
}
