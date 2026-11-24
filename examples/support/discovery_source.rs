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
//! thread/`Notify`/counters shape (`crates/vosu-core/src/plugin/discovery.rs`)
//! closely enough to measure the same things, without a `PluginHost`.

use std::collections::HashSet;
use std::io;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use tokio::runtime::Handle;
use tokio::sync::{Notify, watch};
use vosu_core::plugin::{LinuxSource, Prefilter, ProcessSource};

/// Mirrors `vosu_osu::MANIFEST`'s Linux pre-filter hints (P1 lands the
/// `vosu-osu` crate in a parallel unit); switch this to parse
/// `vosu_osu::MANIFEST` once P1 integrates, per the C5 assignment.
pub(super) const OSU_COMM_HINTS: [&str; 2] = ["osu!", "osulazer.exe"];

/// How often the background thread runs a full `enumerate()` reconcile
/// when nothing else forces one sooner (mirrors `DiscoveryCadence`'s own
/// fast interval, DI): these examples only ever track a handful of
/// short-lived helper processes, so there is no "something is tracked"
/// slow interval to relax into — the fast interval alone keeps the
/// polling baseline comparable across runs.
const RECONCILE_INTERVAL: Duration = Duration::from_millis(100);


#[derive(Debug, Default)]
struct Counters {
    full_scans: AtomicU64,
    targeted_inspections: AtomicU64,
    event_wakes: AtomicU64,
    reconciliation_requests: AtomicU64,
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
/// `DiscoveryService`'s own `full_reconcile`. Returns whether the tracked
/// set changed.
fn full_reconcile(source: &LinuxSource, tracked: &mut HashSet<u32>, counters: &Counters) -> bool {
    match source.enumerate() {
        Ok(records) => {
            let observed: HashSet<u32> = records.iter().map(|record| record.pid).collect();
            counters.full_scans.fetch_add(1, Ordering::Relaxed);
            let changed = observed != *tracked;
            *tracked = observed;
            changed
        }
        Err(error) => {
            eprintln!("discovery_source: enumerate failed, keeping prior tracked set: {error}");
            false
        }
    }
}

/// Applies one `source.drain_events()` batch: every `exit` pid removed
/// outright, every `inspect` pid re-read through `ProcessSource::inspect`
/// and added/removed accordingly. Returns whether the tracked set changed.
fn apply_batch(source: &LinuxSource, tracked: &mut HashSet<u32>, counters: &Counters) -> bool {
    let batch = source.drain_events();
    let mut changed = false;
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
            }
        }
    }
    if batch.needs_reconcile {
        counters.reconciliation_requests.fetch_add(1, Ordering::Relaxed);
    }
    changed
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
    /// Opens a `LinuxSource` (`VOSU_PROCESS_EVENTS`-selected, same as
    /// 1.x's `NativeProcessProvider::open_process_events`; callers set
    /// that env var from the CLI `--mode` flag before calling this),
    /// syncs `prefilter` to it, and starts the background reconciliation
    /// thread.
    pub(super) fn start(prefilter: Prefilter, runtime: Handle) -> io::Result<Self> {
        let source = Arc::new(LinuxSource::open());
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
                let mut tracked: HashSet<u32> = HashSet::new();
                // Due immediately: the first pass is always a full
                // reconcile, regardless of what `loop_source` reports
                // before then.
                let mut next_reconcile = Instant::now();
                while !loop_cancel.load(Ordering::Acquire) {
                    let now = Instant::now();
                    let changed = if now >= next_reconcile {
                        next_reconcile = now + RECONCILE_INTERVAL;
                        full_reconcile(loop_source.as_ref(), &mut tracked, &loop_counters)
                    } else {
                        apply_batch(loop_source.as_ref(), &mut tracked, &loop_counters)
                    };
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

    pub(super) fn reconciliation_request_count(&self) -> u64 {
        self.counters.reconciliation_requests.load(Ordering::Relaxed)
            + self.source.reconciliation_request_count()
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
