//! Bridge B1's single process-wide hook slot. Deleted in M4a (see
//! `crate` docs).

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
        *SLOT.lock().unwrap() = None;
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
    use std::sync::Mutex as StdMutex;

    // `SLOT` is one process-wide global: nextest runs each test binary in
    // its own process, but within this binary these tests must not run
    // concurrently, or one could observe another's install/uninstall.
    static TEST_LOCK: StdMutex<()> = StdMutex::new(());

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
        let _guard = TEST_LOCK.lock().unwrap();
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
        let _guard = TEST_LOCK.lock().unwrap();
        let (sink, _rx) = RecordingSink::new(0);
        let _installed =
            install::<u32, i64>(sink as Arc<dyn LegacySink<u32, i64>>).expect("slot must be free");

        assert!(
            current::<String, bool>().is_none(),
            "a sink installed for (u32, i64) must not satisfy current::<String, bool>()"
        );
    }

    #[test]
    fn second_install_is_refused_until_the_first_is_dropped() {
        let _guard = TEST_LOCK.lock().unwrap();
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

    #[test]
    fn inputs_receiver_sees_the_latest_value_only() {
        let _guard = TEST_LOCK.lock().unwrap();
        let (sink, _rx) = RecordingSink::new(1);
        let installed = install::<u32, i64>(sink.clone() as Arc<dyn LegacySink<u32, i64>>)
            .expect("slot must be free");

        let got = current::<u32, i64>().expect("must be installed");
        let receiver = got.inputs();
        assert_eq!(*receiver.borrow(), 1);

        sink.inputs_tx.send(2).unwrap();
        sink.inputs_tx.send(3).unwrap();
        // `watch::Sender::send` writes directly into the shared cell, so a
        // receiver's `borrow()` reflects the latest value immediately with
        // no `.await` needed; it never queues the intermediate `2`.
        assert_eq!(
            *receiver.borrow(),
            3,
            "a watch receiver must observe only the latest sent value, never an intermediate one"
        );

        drop(installed);
    }
}
