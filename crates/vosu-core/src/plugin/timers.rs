//! Host-side timers (`docs/v2/plugin-abi.md` §6.11): `VOSU_OP_TIMER_SET`/
//! `VOSU_OP_TIMER_CANCEL` routing and `VOSU_OP_TIMER_FIRED` delivery,
//! built on `tokio::time` per `AGENTS.md` (no `std::thread::sleep`, no
//! unbounded per-plugin timer threads).
//!
//! §6.11/§5 cap outstanding timers per instance at 64, matching read
//! plans, scans, and cross-plugin action calls: [`MAX_TIMERS_PER_INSTANCE`].
//! A fired one-shot's slot is freed immediately (it no longer counts
//! against this cap), and `TIMER_CANCEL`/destroy's `cancel_all` free a
//! slot right away too.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::runtime::Handle;
use tokio::task::JoinHandle;
use vosu_plugin_api::vosu_status;

use super::host_services::PluginInstanceId;

/// §6.11: "a one-shot fire after the host's minimum granularity", and the
/// floor a periodic `interval_ms` is clamped up to, so a plugin cannot
/// make the host wake its dispatch path faster than this regardless of
/// what it requests.
pub const MIN_TIMER_GRANULARITY: Duration = Duration::from_millis(10);

/// This host's own defensive cap on outstanding timers per instance (see
/// the module doc comment): a `TIMER_SET` past this is refused with
/// `VOSU_ERR_INVALID_ARGUMENT` rather than accepted unbounded. §6.11
/// records this cap directly now, matching §5's read plans/scans/action
/// calls (64 each).
pub const MAX_TIMERS_PER_INSTANCE: usize = 64;

/// Delivers a fired or routes a cancelled timer back to its owning
/// instance: the host-service modules in `crate::plugin` have no instance
/// registry of their own (instances are created and owned by whoever
/// embeds this host, e.g. `vosu`'s `src/runtime/dev_plugins.rs`), so
/// timer delivery is this one narrow callback instead.
pub trait TimerDelivery: Send + Sync {
    /// `VOSU_OP_TIMER_FIRED` (host -> plugin, no reply): delivered through
    /// `instance`'s own dispatch path exactly like any other host ->
    /// plugin message (§4). **Always called while holding this
    /// registry's internal table lock** (that is how the cancel race
    /// below is made race-free): an implementation must not block, and
    /// must never re-enter this registry (`set`/`cancel`/`cancel_all`)
    /// from inside this call, or it deadlocks against the very lock it
    /// is running under. See `cancel`'s own doc comment for a race this
    /// call's own correctness does not close: once this returns, the
    /// message may still be sitting in the owning instance's own
    /// dispatch queue, not yet delivered to the plugin.
    fn timer_fired(&self, instance: PluginInstanceId, timer: u64);
}

/// A live timer's registration. `generation` is bumped on every `set` for
/// a given `(instance, timer)` key, including one replacing an id that
/// was already live: the spawned task captures the generation it was
/// created with and, right before it may act (deliver a fire, or free a
/// fired one-shot's own slot), re-checks it against the table under the
/// same lock `cancel`/`cancel_all` use to remove entries. A stale
/// generation (the slot was cancelled, destroyed, or replaced by a later
/// `set` since this task started) means the task does nothing further:
/// a replaced timer's old task can never remove its successor's slot,
/// and a cancelled or destroyed timer can never deliver after `cancel`/
/// `cancel_all` has returned, because both sides serialize on this same
/// lock.
struct LiveTimer {
    task: JoinHandle<()>,
    generation: u64,
}

impl Drop for LiveTimer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

type TimerTable = Arc<Mutex<HashMap<(PluginInstanceId, u64), LiveTimer>>>;

/// One process-wide registry of every loaded instance's live timers.
/// Cloning shares the same underlying table (cheap `Arc` clone), so one
/// registry instance serves every plugin instance.
#[derive(Clone)]
pub struct TimerRegistry {
    runtime: Handle,
    timers: TimerTable,
    next_generation: Arc<AtomicU64>,
}

impl TimerRegistry {
    /// `runtime` is captured once (typically `Handle::current()` at
    /// startup) so `set` can spawn a timer task regardless of which
    /// thread calls it — a plugin instance's dispatch thread is a plain
    /// OS thread (§4), not a Tokio runtime, so the ambient `tokio::spawn`
    /// is not available there.
    pub fn new(runtime: Handle) -> Self {
        Self {
            runtime,
            timers: Arc::new(Mutex::new(HashMap::new())),
            next_generation: Arc::new(AtomicU64::new(0)),
        }
    }

    /// `VOSU_OP_TIMER_SET`. Setting an id this instance already has live
    /// replaces it (cancels the old task first), matching
    /// `VOSU_OP_STATE_SUBSCRIBE`'s precedent of a later message wholesale
    /// replacing an earlier one addressed by the same key, since §6.11
    /// does not say a second `TIMER_SET` for a live id is refused either.
    pub fn set(
        &self,
        instance: PluginInstanceId,
        timer: u64,
        interval_ms: Option<u32>,
        delivery: Arc<dyn TimerDelivery>,
    ) -> vosu_status {
        let key = (instance, timer);
        let mut guard = self
            .timers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        if !guard.contains_key(&key) {
            let live = guard.keys().filter(|(owner, _)| *owner == instance).count();
            if live >= MAX_TIMERS_PER_INSTANCE {
                return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
            }
        }
        let generation = self.next_generation.fetch_add(1, Ordering::Relaxed);
        let interval =
            interval_ms.map(|ms| Duration::from_millis(u64::from(ms)).max(MIN_TIMER_GRANULARITY));
        let task = self.runtime.spawn(run_timer(
            instance,
            timer,
            generation,
            interval,
            delivery,
            Arc::clone(&self.timers),
        ));
        // Replacing a still-live entry drops its old `LiveTimer` here,
        // which aborts the old task; the generation check in `run_timer`
        // covers the narrow window before that abort takes effect.
        guard.insert(key, LiveTimer { task, generation });
        vosu_status::VOSU_OK
    }

    /// `VOSU_OP_TIMER_CANCEL`: stops a periodic timer; cancelling a
    /// one-shot that already fired (or an id this instance never set) is
    /// a no-op, per §6.11, so this always answers `VOSU_OK`. Removing the
    /// entry under this same lock is what makes `run_timer`'s generation
    /// check race-free: once this returns, this registry will not start
    /// delivering another `TIMER_FIRED` for `timer`. That is not the same
    /// as saying the plugin can never observe one for it again: a fire
    /// `timer_fired` already decided to deliver, and already handed off
    /// to the instance's own dispatch queue, before this call started,
    /// can still be sitting in that queue and reach the plugin after this
    /// call has returned — this registry has no way to retract a message
    /// already queued elsewhere, nor any per-message tag left to filter
    /// it by once it has left for the instance's own queue.
    /// `docs/v2/plugin-abi.md` §6.11 tells plugins to tolerate one stray
    /// `TIMER_FIRED` for a timer they just cancelled.
    pub fn cancel(&self, instance: PluginInstanceId, timer: u64) -> vosu_status {
        let mut guard = self
            .timers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        guard.remove(&(instance, timer));
        vosu_status::VOSU_OK
    }

    /// Cancels every timer this instance still has live: `destroy` must
    /// cancel all of an instance's timers (M1 exit list), called before
    /// or alongside tearing the instance itself down. Same race-free
    /// guarantee as `cancel`: no delivery for this instance can follow.
    pub fn cancel_all(&self, instance: PluginInstanceId) {
        let mut guard = self
            .timers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        guard.retain(|(owner, _), _| *owner != instance);
    }

    /// The number of timers currently live for `instance` (test/diagnostic
    /// use).
    pub fn live_count(&self, instance: PluginInstanceId) -> usize {
        let guard = self
            .timers
            .lock()
            .unwrap_or_else(|poison| poison.into_inner());
        guard.keys().filter(|(owner, _)| *owner == instance).count()
    }
}

/// Checks `generation` against the table's current entry for `key` and,
/// if it still matches, runs `on_match` while still holding the lock,
/// returning its result. A stale generation (removed by `cancel`/
/// `cancel_all`, or replaced by a later `set`) means `on_match` never
/// runs. Holding the lock across `on_match` (a synchronous delivery call
/// or a removal) is what makes this race-free against `cancel`/
/// `cancel_all`, which take the same lock.
fn with_current_generation(
    timers: &TimerTable,
    key: (PluginInstanceId, u64),
    generation: u64,
    on_match: impl FnOnce(&mut HashMap<(PluginInstanceId, u64), LiveTimer>),
) -> bool {
    let mut guard = timers.lock().unwrap_or_else(|poison| poison.into_inner());
    let current = matches!(guard.get(&key), Some(slot) if slot.generation == generation);
    if current {
        on_match(&mut guard);
    }
    current
}

async fn run_timer(
    instance: PluginInstanceId,
    timer: u64,
    generation: u64,
    interval: Option<Duration>,
    delivery: Arc<dyn TimerDelivery>,
    timers: TimerTable,
) {
    let key = (instance, timer);
    match interval {
        None => {
            tokio::time::sleep(MIN_TIMER_GRANULARITY).await;
            // Free the fired one-shot's own slot (so it stops counting
            // against the per-instance cap) and delivers, both only if
            // this is still the live registration for `key`: a cancel,
            // destroy, or replacement since this task started leaves
            // nothing to free and nothing to deliver.
            with_current_generation(&timers, key, generation, |guard| {
                guard.remove(&key);
                delivery.timer_fired(instance, timer);
            });
        }
        Some(interval) => {
            let mut ticker = tokio::time::interval(interval);
            // The first tick fires immediately; a timer's first callback
            // should wait one interval, not fire at registration time.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                let delivered = with_current_generation(&timers, key, generation, |_guard| {
                    delivery.timer_fired(instance, timer);
                });
                if !delivered {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex as StdMutex;
    use std::sync::mpsc::{Receiver, Sender, channel};

    use super::*;

    struct RecordingDelivery {
        tx: StdMutex<Sender<(PluginInstanceId, u64)>>,
    }

    impl TimerDelivery for RecordingDelivery {
        fn timer_fired(&self, instance: PluginInstanceId, timer: u64) {
            let _ = self.tx.lock().unwrap().send((instance, timer));
        }
    }

    fn recording() -> (Arc<RecordingDelivery>, Receiver<(PluginInstanceId, u64)>) {
        let (tx, rx) = channel();
        (
            Arc::new(RecordingDelivery {
                tx: StdMutex::new(tx),
            }),
            rx,
        )
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn one_shot_timer_fires_exactly_once_and_frees_its_slot() {
        let registry = TimerRegistry::new(Handle::current());
        let (delivery, rx) = recording();
        let instance = PluginInstanceId::new(1);
        assert_eq!(
            registry.set(instance, 1, None, delivery),
            vosu_status::VOSU_OK
        );

        let (fired_instance, fired_timer) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(fired_instance, instance);
        assert_eq!(fired_timer, 1);
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        // The fired one-shot's slot is freed, not merely inert: it stops
        // counting against `MAX_TIMERS_PER_INSTANCE`.
        assert_eq!(registry.live_count(instance), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn fired_one_shots_free_slots_so_the_cap_never_starves_an_instance() {
        let registry = TimerRegistry::new(Handle::current());
        let (delivery, rx) = recording();
        let instance = PluginInstanceId::new(10);
        // Twice the cap, one at a time, each only set once the previous
        // one-shot has fired: if a fired one-shot's slot were never
        // freed (the P1 finding), the cap would refuse every id past
        // `MAX_TIMERS_PER_INSTANCE`.
        for id in 0..(MAX_TIMERS_PER_INSTANCE as u64 * 2) {
            assert_eq!(
                registry.set(instance, id, None, delivery.clone()),
                vosu_status::VOSU_OK,
                "timer {id} should not be refused: earlier fired one-shots must have freed their slots"
            );
            rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        assert_eq!(registry.live_count(instance), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn replacing_a_live_one_shot_keeps_exactly_the_successor_live() {
        let registry = TimerRegistry::new(Handle::current());
        let (delivery, rx) = recording();
        let instance = PluginInstanceId::new(11);
        registry.set(instance, 1, None, delivery.clone());
        // Replace it with a fresh registration under the same id before
        // it can have fired: `set` bumps the generation for this key, so
        // even if the replaced task's own `tokio::time::sleep` had
        // already elapsed by the time it next checks the table, it would
        // find a newer generation and do nothing — it can never remove
        // or fire for its successor.
        assert_eq!(
            registry.set(instance, 1, None, delivery),
            vosu_status::VOSU_OK
        );

        let (fired_instance, fired_timer) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(fired_instance, instance);
        assert_eq!(fired_timer, 1);
        // Exactly one delivery, from the successor; its slot was freed
        // when it fired.
        assert!(rx.recv_timeout(Duration::from_millis(100)).is_err());
        assert_eq!(registry.live_count(instance), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn periodic_timer_fires_repeatedly() {
        let registry = TimerRegistry::new(Handle::current());
        let (delivery, rx) = recording();
        let instance = PluginInstanceId::new(2);
        registry.set(instance, 7, Some(10), delivery);

        for _ in 0..3 {
            let (fired_instance, fired_timer) = rx.recv_timeout(Duration::from_secs(2)).unwrap();
            assert_eq!(fired_instance, instance);
            assert_eq!(fired_timer, 7);
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancel_stops_further_firing() {
        let registry = TimerRegistry::new(Handle::current());
        let (delivery, rx) = recording();
        let instance = PluginInstanceId::new(3);
        registry.set(instance, 9, Some(10), delivery);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();

        assert_eq!(registry.cancel(instance, 9), vosu_status::VOSU_OK);
        // `cancel` removes the entry under the same lock `run_timer`
        // checks before delivering, so nothing can fire after this
        // returns; drain the one delivery that may have raced it (not
        // followed it — see
        // `cancel_serializes_with_an_in_flight_delivery_so_nothing_fires_after_it_returns`).
        while rx.recv_timeout(Duration::from_millis(5)).is_ok() {}
        assert!(rx.recv_timeout(Duration::from_millis(200)).is_err());
        assert_eq!(registry.live_count(instance), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn cancelling_an_already_fired_one_shot_is_a_no_op() {
        let registry = TimerRegistry::new(Handle::current());
        let (delivery, rx) = recording();
        let instance = PluginInstanceId::new(4);
        registry.set(instance, 5, None, delivery);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();

        assert_eq!(registry.cancel(instance, 5), vosu_status::VOSU_OK);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn destroy_cancels_every_timer_of_an_instance_and_none_fire_again() {
        let registry = TimerRegistry::new(Handle::current());
        let (delivery, rx) = recording();
        let instance = PluginInstanceId::new(6);
        let other = PluginInstanceId::new(60);
        registry.set(instance, 1, Some(10), delivery.clone());
        registry.set(instance, 2, Some(10), delivery.clone());
        registry.set(other, 1, Some(10), delivery);
        rx.recv_timeout(Duration::from_secs(2)).unwrap();
        // Drains whatever had already fired for either instance before
        // cancellation.
        while rx.recv_timeout(Duration::from_millis(5)).is_ok() {}

        registry.cancel_all(instance);
        assert_eq!(registry.live_count(instance), 0);
        assert_eq!(registry.live_count(other), 1);

        // `cancel_all` removes every entry of `instance` under the same
        // lock `run_timer` checks before delivering, so no `instance`
        // timer can fire after this call has returned: every delivery
        // seen from here on must be `other`'s, deterministically (no
        // "wait and hope nothing was in flight" sleep needed).
        for _ in 0..5 {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok((fired_instance, _)) => assert_eq!(fired_instance, other),
                Err(_) => break,
            }
        }
    }

    #[test]
    fn cancel_serializes_with_an_in_flight_delivery_so_nothing_fires_after_it_returns() {
        // A plain `#[test]` with a hand-built runtime, not `#[tokio::test]`:
        // this test blocks real OS threads on real synchronization
        // primitives (never a `sleep`-sized guess) to prove the ordering
        // invariant itself, not just its usual-case outcome.
        let rt = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .unwrap();
        let instance = PluginInstanceId::new(99);

        // `timer_fired` rendezvous-blocks only on its first call: it only
        // returns, and only lets `run_timer` release the registry lock it
        // is called under, once the test sends on `release`. The timer is
        // periodic (`Some(10)` below) so its next tick can race `cancel`
        // for that same lock before `cancel`'s thread gets scheduled —
        // likely under Windows's coarser timer granularity and full
        // test-thread contention. A second rendezvous-block would then
        // hold the lock on a `release` nobody sends again (the test sends
        // on it exactly once), deadlocking both that delivery and
        // `cancel` forever. Every delivery after the first is therefore
        // non-blocking: it still proves nothing fires once `cancel` has
        // returned (the drain below), without this test being able to
        // hang itself.
        struct BlockingDelivery {
            entered: Sender<()>,
            release: StdMutex<Receiver<()>>,
            first: std::sync::atomic::AtomicBool,
            fired: Sender<(PluginInstanceId, u64)>,
        }
        impl TimerDelivery for BlockingDelivery {
            fn timer_fired(&self, instance: PluginInstanceId, timer: u64) {
                if self
                    .first
                    .compare_exchange(true, false, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
                {
                    self.entered.send(()).unwrap();
                    self.release.lock().unwrap().recv().unwrap();
                }
                self.fired.send((instance, timer)).unwrap();
            }
        }

        let (entered_tx, entered_rx) = channel();
        let (release_tx, release_rx) = channel();
        let (fire_tx, fire_rx) = channel();
        let delivery = Arc::new(BlockingDelivery {
            entered: entered_tx,
            release: StdMutex::new(release_rx),
            first: std::sync::atomic::AtomicBool::new(true),
            fired: fire_tx,
        });

        let registry = TimerRegistry::new(rt.handle().clone());
        registry.set(instance, 1, Some(10), delivery);

        // Block until the spawned timer task is inside `timer_fired`,
        // which it only enters while holding the registry's lock.
        entered_rx.recv_timeout(Duration::from_secs(2)).unwrap();

        // `cancel` needs that same lock to remove the entry, so this
        // thread cannot return from it until the in-flight delivery
        // above releases the lock.
        let registry2 = registry.clone();
        let cancel = std::thread::spawn(move || registry2.cancel(instance, 1));

        release_tx.send(()).unwrap();
        assert_eq!(cancel.join().unwrap(), vosu_status::VOSU_OK);
        // The delivery that was already in flight when `cancel` was
        // called still completes: it raced `cancel`, it did not follow
        // it. The periodic timer's next tick can race `cancel` for the
        // same lock too (see `BlockingDelivery`'s doc comment above) and
        // fire again, non-blocking, before `cancel` wins. Every such
        // delivery sends on `fired` while holding the registry lock,
        // before `cancel` acquires it, so all of them are already queued
        // once `join` returns: drain them without waiting.
        fire_rx.recv_timeout(Duration::from_secs(2)).unwrap();
        while fire_rx.try_recv().is_ok() {}

        // Nothing fires once `cancel` has returned: the next tick finds
        // no entry (removed under the same lock) and the task stops.
        assert!(fire_rx.recv_timeout(Duration::from_millis(300)).is_err());
        assert_eq!(registry.live_count(instance), 0);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn exceeding_the_per_instance_cap_is_refused() {
        let registry = TimerRegistry::new(Handle::current());
        let (delivery, _rx) = recording();
        let instance = PluginInstanceId::new(9);
        for id in 0..MAX_TIMERS_PER_INSTANCE as u64 {
            assert_eq!(
                registry.set(instance, id, Some(60_000), delivery.clone()),
                vosu_status::VOSU_OK
            );
        }
        assert_eq!(
            registry.set(
                instance,
                MAX_TIMERS_PER_INSTANCE as u64,
                Some(60_000),
                delivery
            ),
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        );
        registry.cancel_all(instance);
    }
}
