//! Host-side timers (`docs/v2/plugin-abi.md` §6.11): `VOSU_OP_TIMER_SET`/
//! `VOSU_OP_TIMER_CANCEL` routing and `VOSU_OP_TIMER_FIRED` delivery,
//! built on `tokio::time` per `AGENTS.md` (no `std::thread::sleep`, no
//! unbounded per-plugin timer threads).
//!
//! §6.11 states one limit directly — a one-shot timer (`interval_ms:
//! null`) "fires after the host's minimum granularity" — but, unlike read
//! plans, scans, and cross-plugin action calls (§5: 64 outstanding each),
//! states no numeric cap on how many timers one instance may have live at
//! once. [`MAX_TIMERS_PER_INSTANCE`] is this host's own defensive bound
//! (`AGENTS.md`: bounded resource use only), chosen to match those other
//! opcodes' cap for consistency; recorded as a spec gap in this phase's
//! handoff notes, not a value §6.11 itself states.

use std::collections::HashMap;
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
/// `VOSU_ERR_INVALID_ARGUMENT` rather than accepted unbounded.
pub const MAX_TIMERS_PER_INSTANCE: usize = 64;

/// Delivers a fired or routes a cancelled timer back to its owning
/// instance: the host-service modules in `crate::plugin` have no instance
/// registry of their own (instances are created and owned by whoever
/// embeds this host, e.g. `vosu`'s `src/runtime/dev_plugins.rs`), so
/// timer delivery is this one narrow callback instead.
pub trait TimerDelivery: Send + Sync {
    /// `VOSU_OP_TIMER_FIRED` (host -> plugin, no reply): delivered through
    /// `instance`'s own dispatch path exactly like any other host ->
    /// plugin message (§4), never called directly from this registry's
    /// internal lock.
    fn timer_fired(&self, instance: PluginInstanceId, timer: u64);
}

struct LiveTimer {
    task: JoinHandle<()>,
}

impl Drop for LiveTimer {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// One process-wide registry of every loaded instance's live timers.
/// Cloning shares the same underlying table (cheap `Arc` clone), so one
/// registry instance serves every plugin instance.
#[derive(Clone)]
pub struct TimerRegistry {
    runtime: Handle,
    timers: Arc<Mutex<HashMap<(PluginInstanceId, u64), LiveTimer>>>,
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
        let interval =
            interval_ms.map(|ms| Duration::from_millis(u64::from(ms)).max(MIN_TIMER_GRANULARITY));
        let task = self
            .runtime
            .spawn(run_timer(instance, timer, interval, delivery));
        guard.insert(key, LiveTimer { task });
        vosu_status::VOSU_OK
    }

    /// `VOSU_OP_TIMER_CANCEL`: stops a periodic timer; cancelling a
    /// one-shot that already fired (or an id this instance never set) is
    /// a no-op, per §6.11, so this always answers `VOSU_OK`.
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
    /// or alongside tearing the instance itself down.
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

async fn run_timer(
    instance: PluginInstanceId,
    timer: u64,
    interval: Option<Duration>,
    delivery: Arc<dyn TimerDelivery>,
) {
    match interval {
        None => {
            tokio::time::sleep(MIN_TIMER_GRANULARITY).await;
            delivery.timer_fired(instance, timer);
        }
        Some(interval) => {
            let mut ticker = tokio::time::interval(interval);
            // The first tick fires immediately; a timer's first callback
            // should wait one interval, not fire at registration time.
            ticker.tick().await;
            loop {
                ticker.tick().await;
                delivery.timer_fired(instance, timer);
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
    async fn one_shot_timer_fires_exactly_once() {
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
        // Draining whatever was already in flight the instant cancel ran:
        // at most one more tick may have been mid-delivery.
        while rx.recv_timeout(Duration::from_millis(50)).is_ok() {}
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
        // cancellation, so the post-cancel drain below only ever sees
        // ticks that raced with (or followed) `cancel_all` itself.
        while rx.recv_timeout(Duration::from_millis(5)).is_ok() {}

        registry.cancel_all(instance);
        assert_eq!(registry.live_count(instance), 0);
        assert_eq!(registry.live_count(other), 1);

        // `abort()` cancels a task at its next suspension point: a tick
        // already past `ticker.tick().await` and about to call `delivery`
        // synchronously still completes that one call. Give any such
        // in-flight delivery time to land, unexamined, before asserting
        // only `other` fires from here on.
        std::thread::sleep(Duration::from_millis(50));
        while rx.recv_timeout(Duration::from_millis(5)).is_ok() {}
        for _ in 0..5 {
            match rx.recv_timeout(Duration::from_millis(50)) {
                Ok((fired_instance, _)) => assert_eq!(fired_instance, other),
                Err(_) => break,
            }
        }
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
