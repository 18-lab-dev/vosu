//! Bridge B1: the interim part of `vosu-surface` (see `crate` docs).
//! This module and the `vosu-osu -> vosu-surface` edge are deleted in M4a;
//! the rest of the crate is permanent.

use std::any::Any;
use std::fmt;
use std::sync::{Arc, LazyLock, Mutex};

use tokio::sync::watch;

/// A legacy adapter's handle into bridge B1: it receives events of type `E`
/// (the osu! plugin's `StateBundleSnapshot`s in production) and publishes
/// control inputs of type `I` that the plugin reads back as the latest
/// value only.
pub trait LegacySink<E, I>: Send + Sync + 'static {
    /// Hand the sink one event. Must never block: the plugin calls this on
    /// its own dispatch thread, and a blocking `send` would stall every
    /// other plugin callback behind it.
    fn send(&self, event: E);

    /// A watch receiver over the sink's latest input value. `watch` is a
    /// deliberate latest-only choice (ADR 0002): a slow or absent reader
    /// only ever sees the newest value, never an unbounded backlog.
    fn inputs(&self) -> watch::Receiver<I>;
}

/// One process-wide slot, keyed at `install`/`current` call sites by the
/// concrete `(E, I)` pair: `Any::downcast_ref` fails (returning `None`
/// rather than panicking) when a caller's types don't match what is
/// currently installed. No other lock is taken while this one is held, so
/// there is no lock-ordering hazard to document beyond "never call back
/// into plugin code while holding it" (nothing here does).
static SLOT: LazyLock<Mutex<Option<Box<dyn Any + Send + Sync>>>> =
    LazyLock::new(|| Mutex::new(None));

/// Returned by [`install`] when the slot is already occupied.
#[derive(Debug)]
pub struct AlreadyInstalled(());

impl fmt::Display for AlreadyInstalled {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("a LegacySink is already installed")
    }
}

impl std::error::Error for AlreadyInstalled {}

/// Holds bridge B1's slot occupied; dropping it uninstalls the sink. Never
/// constructed except by a successful [`install`], so dropping it always
/// clears exactly the slot that call filled (no other `install` can have
/// succeeded in between, since a second one is refused while this guard is
/// alive).
#[derive(Debug)]
#[must_use = "dropping this immediately uninstalls the sink"]
pub struct Installed(());

impl Drop for Installed {
    fn drop(&mut self) {
        // Take the sink out first so its own `Drop` runs without the slot
        // lock held; a sink whose drop called `current()` would deadlock.
        let previous = SLOT.lock().unwrap().take();
        drop(previous);
    }
}

/// Install `sink` as the current bridge B1 handle for events `E` and inputs
/// `I`. Refused with [`AlreadyInstalled`] while another sink (of any type)
/// already occupies the slot; drop the returned [`Installed`] guard first.
pub fn install<E, I>(sink: Arc<dyn LegacySink<E, I>>) -> Result<Installed, AlreadyInstalled>
where
    E: 'static,
    I: 'static,
{
    let mut slot = SLOT.lock().unwrap();
    if slot.is_some() {
        return Err(AlreadyInstalled(()));
    }
    *slot = Some(Box::new(sink));
    Ok(Installed(()))
}

/// The currently installed sink for `(E, I)`, or `None` if nothing is
/// installed, or if something is installed for a different `(E, I)` pair.
pub fn current<E, I>() -> Option<Arc<dyn LegacySink<E, I>>>
where
    E: 'static,
    I: 'static,
{
    let slot = SLOT.lock().unwrap();
    slot.as_ref()?
        .downcast_ref::<Arc<dyn LegacySink<E, I>>>()
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Mutex as StdMutex, MutexGuard, PoisonError};
    use std::time::Duration;

    // `SLOT` is one process-wide global: nextest runs each test binary in
    // its own process, but within this binary these tests must not run
    // concurrently, or one could observe another's install/uninstall.
    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

    fn test_lock() -> MutexGuard<'static, ()> {
        TEST_LOCK.lock().unwrap_or_else(PoisonError::into_inner)
    }

    struct RecordingSink {
        events: StdMutex<Vec<u32>>,
        inputs_tx: watch::Sender<i64>,
    }

    impl RecordingSink {
        fn new(initial_input: i64) -> (Arc<Self>, watch::Receiver<i64>) {
            let (inputs_tx, rx) = watch::channel(initial_input);
            (
                Arc::new(Self {
                    events: StdMutex::new(Vec::new()),
                    inputs_tx,
                }),
                rx,
            )
        }
    }

    impl LegacySink<u32, i64> for RecordingSink {
        fn send(&self, event: u32) {
            self.events.lock().unwrap().push(event);
        }

        fn inputs(&self) -> watch::Receiver<i64> {
            self.inputs_tx.subscribe()
        }
    }

    struct OtherSink;

    impl LegacySink<String, bool> for OtherSink {
        fn send(&self, _event: String) {}

        fn inputs(&self) -> watch::Receiver<bool> {
            watch::channel(false).1
        }
    }

    #[test]
    fn install_then_current_returns_the_same_sink() {
        let _guard = test_lock();
        assert!(current::<u32, i64>().is_none());

        let (sink, _rx) = RecordingSink::new(0);
        let installed = install::<u32, i64>(sink.clone() as Arc<dyn LegacySink<u32, i64>>)
            .expect("slot must be free");

        let got = current::<u32, i64>().expect("install must make current() return Some");
        got.send(7);
        assert_eq!(*sink.events.lock().unwrap(), vec![7]);

        drop(installed);
        assert!(
            current::<u32, i64>().is_none(),
            "dropping the guard must uninstall the sink"
        );
    }

    #[test]
    fn current_is_none_for_a_mismatched_type_pair() {
        let _guard = test_lock();
        let (sink, _rx) = RecordingSink::new(0);
        let _installed =
            install::<u32, i64>(sink as Arc<dyn LegacySink<u32, i64>>).expect("slot must be free");

        assert!(
            current::<u32, i64>().is_some(),
            "the install above must still be in effect, so the None below isn't vacuous"
        );
        assert!(
            current::<String, bool>().is_none(),
            "a sink installed for (u32, i64) must not satisfy current::<String, bool>()"
        );
    }

    #[test]
    fn second_install_is_refused_until_the_first_is_dropped() {
        let _guard = test_lock();
        let (first, _rx1) = RecordingSink::new(0);
        let first_installed =
            install::<u32, i64>(first as Arc<dyn LegacySink<u32, i64>>).expect("slot must be free");

        let second_err =
            install::<String, bool>(Arc::new(OtherSink) as Arc<dyn LegacySink<String, bool>>)
                .expect_err("a second install must be refused while the first is still installed");
        assert_eq!(second_err.to_string(), "a LegacySink is already installed");

        drop(first_installed);
        let second_installed =
            install::<String, bool>(Arc::new(OtherSink) as Arc<dyn LegacySink<String, bool>>)
                .expect("the slot must be free again after the first guard is dropped");
        drop(second_installed);
    }

    /// A sink whose own `Drop` calls `current::<E, I>()`. Regression for
    /// 935a95a: before that fix, `Installed::drop` held `SLOT`'s lock while
    /// dropping the previous sink, so this would deadlock on itself.
    struct DropQueriesCurrent {
        current_during_drop: Arc<StdMutex<Option<bool>>>,
    }

    impl LegacySink<u32, i64> for DropQueriesCurrent {
        fn send(&self, _event: u32) {}

        fn inputs(&self) -> watch::Receiver<i64> {
            watch::channel(0).1
        }
    }

    impl Drop for DropQueriesCurrent {
        fn drop(&mut self) {
            let is_some = current::<u32, i64>().is_some();
            *self.current_during_drop.lock().unwrap() = Some(is_some);
        }
    }

    #[test]
    fn uninstalling_a_sink_whose_drop_calls_current_does_not_deadlock() {
        let _guard = test_lock();
        let current_during_drop = Arc::new(StdMutex::new(None));
        let sink = Arc::new(DropQueriesCurrent {
            current_during_drop: current_during_drop.clone(),
        });
        let installed = install::<u32, i64>(sink.clone() as Arc<dyn LegacySink<u32, i64>>)
            .expect("slot must be free");
        // Drop our extra reference so the slot's own `Arc` is the sink's
        // last owner: dropping `installed` below then runs `DropQueriesCurrent::drop`.
        drop(sink);

        // Run the uninstall on its own thread with a bounded wait: a
        // deadlocked `Installed::drop` hangs that thread forever, and
        // `recv_timeout` turns that into a test failure instead of a hang.
        let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
        let handle = std::thread::spawn(move || {
            drop(installed);
            let _ = done_tx.send(());
        });
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("Installed::drop must not deadlock when the sink's own Drop calls current()");
        handle.join().expect("uninstall thread must not panic");

        assert_eq!(
            *current_during_drop.lock().unwrap(),
            Some(false),
            "current() must see the slot already cleared while this sink's own Drop is running"
        );
        assert!(
            current::<u32, i64>().is_none(),
            "the slot must be empty once the uninstall completes"
        );
    }
}
