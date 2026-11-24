use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::watch;

struct Sink {
    latest: Mutex<Option<B1Event>>,
    count: AtomicUsize,
    publisher: Mutex<Option<std::sync::Weak<B1Publisher>>>,
}
impl vosu_surface::LegacySink<B1Event, B1Inputs> for Sink {
    fn send(&self, event: B1Event) {
        if let B1Event::Publish { live, .. } = &event {
            let publisher = self.publisher.lock().unwrap().clone();
            if let Some(publisher) = publisher.and_then(|publisher| publisher.upgrade()) {
                // Equivalent re-entry must be suppressed, and must not deadlock.
                publisher.publish(Arc::clone(live), Delivery::All).unwrap();
            }
        }
        self.count.fetch_add(1, Ordering::SeqCst);
        *self.latest.lock().unwrap() = Some(event);
    }
    fn inputs(&self) -> watch::Receiver<B1Inputs> {
        watch::channel(B1Inputs::default()).1
    }
}
fn snapshot(epoch: u64) -> Arc<StateBundleSnapshot> {
    let mut snapshot = (*vosu_osu_state::StateBundle::new_with_root(
        ClientType::Stable,
        9,
        std::path::PathBuf::new(),
        Arc::new(vosu_osu_state::state_backend::LiveStateCalculationBackend::new()),
    )
    .snapshot())
    .clone();
    snapshot.reset_epoch = epoch;
    Arc::new(snapshot)
}
#[test]
fn publisher_preserves_key_suppresses_duplicates_and_retains_only_latest() {
    let sink = Arc::new(Sink {
        latest: Mutex::new(None),
        count: AtomicUsize::new(0),
        publisher: Mutex::new(None),
    });
    let target: Arc<B1Sink> = sink.clone();
    let publisher = B1Publisher::new(
        B1SessionKey {
            pid: 61054,
            generation: 9,
        },
        target,
    );
    let first = snapshot(0);
    publisher
        .publish(Arc::clone(&first), Delivery::All)
        .unwrap();
    publisher
        .publish(Arc::new((*first).clone()), Delivery::All)
        .unwrap();
    assert_eq!(sink.count.load(Ordering::SeqCst), 2);
    let latest = snapshot(2);
    publisher
        .publish(Arc::clone(&latest), Delivery::Live)
        .unwrap();
    assert_eq!(sink.count.load(Ordering::SeqCst), 3);
    match sink.latest.lock().unwrap().take().unwrap() {
        B1Event::Publish { key, api, live } => {
            assert_eq!(
                key,
                B1SessionKey {
                    pid: 61054,
                    generation: 9
                }
            );
            assert!(Arc::ptr_eq(&api, &latest));
            assert!(Arc::ptr_eq(&live, &latest));
        }
        _ => panic!("subsequent publications emit Publish without another phase"),
    }
}
#[test]
fn publisher_drops_its_lock_before_calling_a_reentrant_sink() {
    let (tx, rx) = std::sync::mpsc::sync_channel(1);
    std::thread::spawn(move || {
        let sink = Arc::new(Sink {
            latest: Mutex::new(None),
            count: AtomicUsize::new(0),
            publisher: Mutex::new(None),
        });
        let target: Arc<B1Sink> = sink.clone();
        let publisher = Arc::new(B1Publisher::new(
            B1SessionKey {
                pid: 61055,
                generation: 9,
            },
            target,
        ));
        *sink.publisher.lock().unwrap() = Some(Arc::downgrade(&publisher));
        publisher.publish(snapshot(0), Delivery::All).unwrap();
        tx.send(sink.count.load(Ordering::SeqCst)).unwrap();
    });
    assert_eq!(
        rx.recv_timeout(std::time::Duration::from_secs(5)).unwrap(),
        2
    );
}
#[test]
fn reader_settings_combine_non_default_b2_and_b1_fields() {
    let osu = OsuSettings {
        calculate_pp: false,
        poll_rate: 543.0,
        precise_data_poll_rate: 21.0,
        show_mp_commands: true,
        read_mania_scroll_speed: false,
    };
    let inputs = B1Inputs {
        demand: SubscriptionDemand {
            regular: true,
            precise: true,
            gameplay_regular: false,
            gameplay_precise: true,
        },
    };
    assert_eq!(
        inputs.reader_settings(osu),
        ReaderSettings {
            calculate_pp: false,
            poll_rate: 543.0,
            precise_data_poll_rate: 21.0,
            show_mp_commands: true,
            read_mania_scroll_speed: false,
            collect_hit_errors: false,
        }
    );
}

struct Publication {
    api: Arc<StateBundleSnapshot>,
    live: Arc<StateBundleSnapshot>,
}
#[derive(Default)]
struct TestQueue(Mutex<std::collections::BTreeMap<u32, Publication>>);
impl TestQueue {
    fn drain(&self) -> std::collections::BTreeMap<u32, Publication> {
        std::mem::take(&mut *self.0.lock().unwrap())
    }
}
impl vosu_surface::LegacySink<B1Event, B1Inputs> for TestQueue {
    fn send(&self, event: B1Event) {
        if let B1Event::Publish { key, api, live } = event {
            self.0
                .lock()
                .unwrap()
                .insert(key.pid, Publication { api, live });
        }
    }
    fn inputs(&self) -> watch::Receiver<B1Inputs> {
        watch::channel(B1Inputs::default()).1
    }
}
fn state_bundle(generation: u64, reset_epoch: u64) -> Arc<StateBundleSnapshot> {
    let mut snapshot = (*snapshot(reset_epoch)).clone();
    snapshot.generation = generation;
    Arc::new(snapshot)
}

#[test]
fn publication_sink_suppresses_equivalent_snapshots() {
    let queue = Arc::new(TestQueue::default());
    let sink = B1Publisher::new(
        B1SessionKey {
            pid: 7,
            generation: 1,
        },
        queue.clone(),
    );
    let first = state_bundle(1, 10);
    let equivalent = Arc::new(StateBundleSnapshot::clone(&first));

    sink.publish(first, Delivery::All).unwrap();
    assert_eq!(queue.drain().len(), 1);
    sink.publish(equivalent, Delivery::All).unwrap();
    assert!(queue.drain().is_empty());

    sink.publish(state_bundle(1, 10), Delivery::All).unwrap();
    assert_eq!(queue.drain().len(), 1);
}

#[test]
fn subscriber_samples_reach_live_state_before_api_cadence() {
    let queue = Arc::new(TestQueue::default());
    let sink = B1Publisher::new(
        B1SessionKey {
            pid: 7,
            generation: 1,
        },
        queue.clone(),
    );
    let cadence = state_bundle(1, 10);
    sink.publish(Arc::clone(&cadence), Delivery::All).unwrap();
    queue.drain();

    // A subscriber-driven regular sample is live-only.
    let fast = Arc::new(StateBundleSnapshot {
        regular: Arc::clone(&state_bundle(1, 10).regular),
        ..StateBundleSnapshot::clone(&cadence)
    });
    sink.publish(Arc::clone(&fast), Delivery::Live).unwrap();
    let drained = queue.drain();
    assert!(Arc::ptr_eq(&drained[&7].live, &fast));
    assert!(Arc::ptr_eq(&drained[&7].api, &cadence));

    // A due precise sample advances only the API's precise component.
    let precise = Arc::new(StateBundleSnapshot {
        precise: Arc::clone(&state_bundle(1, 10).precise),
        ..StateBundleSnapshot::clone(&fast)
    });
    sink.publish(Arc::clone(&precise), Delivery::Precise)
        .unwrap();
    let api = Arc::clone(&queue.drain()[&7].api);
    assert!(Arc::ptr_eq(&api.regular, &cadence.regular));
    assert!(Arc::ptr_eq(&api.precise, &precise.precise));

    // A reset epoch reaches API readers at once, even between cadence points.
    let reset = state_bundle(1, 11);
    sink.publish(Arc::clone(&reset), Delivery::Live).unwrap();
    let drained = queue.drain();
    assert!(Arc::ptr_eq(&drained[&7].api, &reset));
    assert!(Arc::ptr_eq(&drained[&7].live, &reset));

    // The next API cadence capture catches API readers up.
    let caught_up = state_bundle(1, 11);
    sink.publish(Arc::clone(&caught_up), Delivery::All).unwrap();
    assert!(Arc::ptr_eq(&queue.drain()[&7].api, &caught_up));
}

#[test]
fn reading_phase_follows_only_the_first_successful_publication() {
    #[derive(Default)]
    struct Events(Mutex<Vec<&'static str>>);
    impl vosu_surface::LegacySink<B1Event, B1Inputs> for Events {
        fn send(&self, event: B1Event) {
            self.0.lock().unwrap().push(match event {
                B1Event::Publish { .. } => "publish",
                B1Event::Phase {
                    phase: B1Phase::Reading,
                    ..
                } => "reading",
                _ => panic!("unexpected publisher event"),
            });
        }
        fn inputs(&self) -> watch::Receiver<B1Inputs> {
            watch::channel(B1Inputs::default()).1
        }
    }
    let events = Arc::new(Events::default());
    let publisher = B1Publisher::new(
        B1SessionKey {
            pid: 61059,
            generation: 9,
        },
        events.clone(),
    );
    assert!(
        events.0.lock().unwrap().is_empty(),
        "creating a reader publisher must not mark the session reading"
    );
    let first = snapshot(0);
    publisher.publish(first.clone(), Delivery::All).unwrap();
    assert_eq!(*events.0.lock().unwrap(), ["publish", "reading"]);
    publisher.publish(first, Delivery::All).unwrap();
    assert_eq!(
        *events.0.lock().unwrap(),
        ["publish", "reading"],
        "an equivalent publication emits no second phase"
    );
    publisher.publish(snapshot(1), Delivery::All).unwrap();
    assert_eq!(*events.0.lock().unwrap(), ["publish", "reading", "publish"]);
}
