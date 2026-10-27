use super::*;

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn regular_ack_is_after_publish_then_sleep_and_never_catches_up() {
    let regular_captures = Arc::new(Mutex::new(Vec::new()));
    let precise_captures = Arc::new(Mutex::new(Vec::new()));
    let regular_transitions = Arc::new(AtomicUsize::new(0));
    let precise_transitions = Arc::new(Mutex::new(Vec::new()));
    let publications = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let clock = Arc::new(ManualClock::default());
    let (interval_tx, interval_rx) = watch::channel(PollIntervals {
        regular: Duration::from_millis(100),
        precise: Duration::from_millis(10),
        subscription: None,
    });
    let worker = spawn_state_worker_with_clock(
        FakeRegular {
            captures: regular_captures.clone(),
        },
        FakePrecise {
            captures: precise_captures.clone(),
            corrupt_first_mode: false,
        },
        RecordingTransition {
            context: context(ClientType::Stable),
            regular: regular_transitions.clone(),
            precise_sequences: precise_transitions,
        },
        Arc::new(GatedPublisher {
            publications,
            gate_first_regular: AtomicBool::new(true),
            gate: gate.clone(),
        }),
        context(ClientType::Stable),
        RegularCapturePlan::default(),
        interval_rx.into(),
        Arc::new(Mutex::new(ReaderSynchronization::default())),
        clock.clone(),
    );
    let _open_gate_on_unwind = OpenGateOnDrop(gate.clone());

    wait_until(|| regular_transitions.load(Ordering::Acquire) == 1).await;
    assert_eq!(lock(&regular_captures).as_slice(), &[1]);
    assert!(clock.observed_count(Duration::from_millis(100)) == 0);
    wait_until(|| !lock(&precise_captures).is_empty()).await;
    assert!(
        !lock(&precise_captures).is_empty(),
        "precise reader remains independent"
    );

    *lock(&gate.0) = true;
    gate.1.notify_all();
    wait_until(|| clock.observed_count(Duration::from_millis(100)) == 1).await;
    assert_eq!(lock(&regular_captures).as_slice(), &[1]);

    interval_tx.send_replace(PollIntervals {
        regular: Duration::from_millis(250),
        precise: Duration::from_millis(10),
        subscription: None,
    });
    clock.advance(Duration::from_millis(100));
    wait_until(|| lock(&regular_captures).len() == 2).await;
    wait_until(|| regular_transitions.load(Ordering::Acquire) == 2).await;
    assert_eq!(lock(&regular_captures).as_slice(), &[1, 2]);
    assert_eq!(regular_transitions.load(Ordering::Acquire), 2);
    wait_until(|| clock.observed_count(Duration::from_millis(250)) == 1).await;
    assert_eq!(clock.observed_count(Duration::from_millis(100)), 1);

    worker.shutdown().await.expect("clean shutdown");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn precise_slot_overwrites_and_context_mismatch_advances_nothing() {
    let clock = Arc::new(ManualClock::default());
    let captures = Arc::new(Mutex::new(Vec::new()));
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let publications = Arc::new(Mutex::new(Vec::new()));
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (_interval_tx, interval_rx) = watch::channel(PollIntervals {
        regular: Duration::from_secs(1),
        precise: Duration::from_millis(10),
        subscription: None,
    });
    let worker = spawn_state_worker_with_clock(
        FakeRegular {
            captures: Arc::new(Mutex::new(Vec::new())),
        },
        FakePrecise {
            captures: captures.clone(),
            corrupt_first_mode: true,
        },
        RecordingTransition {
            context: context(ClientType::Lazer),
            regular: Arc::new(AtomicUsize::new(0)),
            precise_sequences: accepted.clone(),
        },
        Arc::new(GatedPublisher {
            publications: publications.clone(),
            gate_first_regular: AtomicBool::new(true),
            gate: gate.clone(),
        }),
        context(ClientType::Lazer),
        RegularCapturePlan::default(),
        interval_rx.into(),
        Arc::new(Mutex::new(ReaderSynchronization::default())),
        clock.clone(),
    );

    wait_until(|| lock(&captures).len() == 1).await;
    wait_until(|| lock(&publications).iter().any(|value| value == "regular")).await;
    clock.advance(Duration::from_millis(10));
    wait_until(|| lock(&captures).len() == 2).await;
    clock.advance(Duration::from_millis(10));
    wait_until(|| lock(&captures).len() == 3).await;
    assert!(
        lock(&accepted).is_empty(),
        "reducer is parked in regular publication"
    );

    *lock(&gate.0) = true;
    gate.1.notify_all();
    wait_until(|| !lock(&accepted).is_empty()).await;
    assert_eq!(
        lock(&accepted).as_slice(),
        &[3],
        "latest slot coalesces older samples"
    );
    {
        let captured = lock(&captures);
        assert_eq!(captured[1].start_index(), 0);
        assert_eq!(captured[1].base_accepted_precise_sequence(), 0);
        assert_eq!(captured[2].start_index(), 0);
        assert_eq!(captured[2].base_accepted_precise_sequence(), 0);
    }

    worker
        .shutdown()
        .await
        .expect("both blocking readers and reducer joined");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stale_regular_epoch_is_recaptured_instead_of_terminating_worker() {
    let gate = Arc::new((Mutex::new(false), Condvar::new()));
    let captures = Arc::new(Mutex::new(Vec::new()));
    let regular = Arc::new(AtomicUsize::new(0));
    let publications = Arc::new(Mutex::new(Vec::new()));
    let precise_captures = Arc::new(Mutex::new(Vec::new()));
    let clock: Arc<dyn WorkerClock> = Arc::new(SystemWorkerClock::default());
    let (_interval_tx, interval_rx) = watch::channel(PollIntervals {
        regular: Duration::from_secs(1),
        precise: Duration::from_millis(10),
        subscription: None,
    });
    let worker = spawn_state_worker_with_clock(
        GatedRecordingRegular {
            gate: gate.clone(),
            captures: captures.clone(),
        },
        FakePrecise {
            captures: precise_captures.clone(),
            corrupt_first_mode: false,
        },
        EpochAdvancingTransition {
            context: context(ClientType::Lazer),
            regular: regular.clone(),
        },
        Arc::new(GatedPublisher {
            publications: publications.clone(),
            gate_first_regular: AtomicBool::new(false),
            gate: Arc::new((Mutex::new(true), Condvar::new())),
        }),
        context(ClientType::Lazer),
        RegularCapturePlan::default(),
        interval_rx.into(),
        Arc::new(Mutex::new(ReaderSynchronization::default())),
        clock.clone(),
    );

    timeout(Duration::from_secs(2), async {
        while lock(&captures).len() != 1 {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("initial regular capture");
    timeout(Duration::from_secs(2), async {
        while !lock(&publications).iter().any(|value| value == "precise") {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("precise epoch publication");
    timeout(Duration::from_secs(2), async {
        while lock(&precise_captures)
            .last()
            .is_none_or(|capture| capture.epoch().reset_epoch != 4)
        {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("precise reader observed committed epoch");
    *lock(&gate.0) = true;
    gate.1.notify_all();
    timeout(Duration::from_secs(2), async {
        while lock(&captures).len() != 2 {
            sleep(Duration::from_millis(1)).await;
        }
    })
    .await
    .expect("replacement regular capture");
    wait_until(|| regular.load(Ordering::Acquire) == 1).await;
    {
        let captures = lock(&captures);
        assert_eq!(captures.len(), 2);
        assert_eq!(captures[0].epoch.reset_epoch, 3);
        assert_eq!(captures[1].epoch.reset_epoch, 4);
        assert_eq!(captures[0].produced_sequence, captures[1].produced_sequence);
    }

    worker
        .shutdown()
        .await
        .expect("stale capture replacement remains healthy");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rejected_precise_context_does_not_advance_index_or_sequence() {
    let clock = Arc::new(ManualClock::default());
    let captures = Arc::new(Mutex::new(Vec::new()));
    let accepted = Arc::new(Mutex::new(Vec::new()));
    let regular_gate = Arc::new((Mutex::new(false), Condvar::new()));
    let (_interval_tx, interval_rx) = watch::channel(PollIntervals {
        regular: Duration::from_secs(1),
        precise: Duration::from_millis(10),
        subscription: None,
    });
    let worker = spawn_state_worker_with_clock(
        BlockedRegular {
            gate: regular_gate.clone(),
        },
        FakePrecise {
            captures: captures.clone(),
            corrupt_first_mode: true,
        },
        RecordingTransition {
            context: context(ClientType::Lazer),
            regular: Arc::new(AtomicUsize::new(0)),
            precise_sequences: accepted.clone(),
        },
        Arc::new(GatedPublisher {
            publications: Arc::new(Mutex::new(Vec::new())),
            gate_first_regular: AtomicBool::new(false),
            gate: Arc::new((Mutex::new(true), Condvar::new())),
        }),
        context(ClientType::Lazer),
        RegularCapturePlan::default(),
        interval_rx.into(),
        Arc::new(Mutex::new(ReaderSynchronization::default())),
        clock.clone(),
    );

    wait_until(|| lock(&captures).len() == 1).await;
    sleep(Duration::from_millis(5)).await;
    assert!(lock(&accepted).is_empty(), "mismatched mode is rejected");
    clock.advance(Duration::from_millis(10));
    wait_until(|| lock(&captures).len() == 2).await;
    wait_until(|| lock(&accepted).as_slice() == [2]).await;
    {
        let captures = lock(&captures);
        assert_eq!(captures[1].start_index(), 0);
        assert_eq!(captures[1].base_accepted_precise_sequence(), 0);
    }

    *lock(&regular_gate.0) = true;
    regular_gate.1.notify_all();
    worker
        .shutdown()
        .await
        .expect("cancel wakes clock and joins all workers");
}

struct ParkedTransition {
    entered: Arc<AtomicBool>,
}

impl SnapshotTransition for ParkedTransition {
    type Publication = String;

    fn apply_regular<'a>(
        &'a mut self,
        _snapshot: Arc<RawRegularSnapshot>,
    ) -> TransitionFuture<'a, RegularTransition<String>> {
        self.entered.store(true, Ordering::Release);
        Box::pin(std::future::pending())
    }

    fn apply_precise<'a>(
        &'a mut self,
        _snapshot: Arc<RawPreciseSnapshot>,
    ) -> TransitionFuture<'a, PreciseTransition<String>> {
        Box::pin(async {
            Ok(PreciseTransition {
                publication: "precise".into(),
                context: None,
            })
        })
    }
}
struct ReleasedAfterCancellationTransition {
    entered: Arc<AtomicBool>,
    release: Arc<Notify>,
    context: ReadContext,
}

impl SnapshotTransition for ReleasedAfterCancellationTransition {
    type Publication = String;

    fn apply_regular<'a>(
        &'a mut self,
        _snapshot: Arc<RawRegularSnapshot>,
    ) -> TransitionFuture<'a, RegularTransition<String>> {
        self.entered.store(true, Ordering::Release);
        let release = self.release.clone();
        let context = self.context.clone();
        Box::pin(async move {
            release.notified().await;
            Ok(RegularTransition {
                publication: "must-not-publish".into(),
                context,
                next_capture: RegularCapturePlan::default(),
            })
        })
    }

    fn apply_precise<'a>(
        &'a mut self,
        _snapshot: Arc<RawPreciseSnapshot>,
    ) -> TransitionFuture<'a, PreciseTransition<String>> {
        Box::pin(std::future::pending())
    }
}

#[test]
fn poll_duration_saturates_without_panicking() {
    assert_eq!(
        duration_from_millis_saturating(f64::INFINITY),
        Duration::MAX
    );
    assert_eq!(duration_from_millis_saturating(1.0e100), Duration::MAX);
    assert_eq!(duration_from_millis_saturating(-1.0), Duration::ZERO);
    assert_eq!(duration_from_millis_saturating(0.0), Duration::ZERO);
    assert_eq!(
        duration_from_millis_saturating(0.125),
        Duration::from_nanos(125_000)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_aborts_a_parked_transition_and_joins_readers() {
    let entered = Arc::new(AtomicBool::new(false));
    let clock = Arc::new(ManualClock::default());
    let (_interval_tx, interval_rx) = watch::channel(PollIntervals {
        regular: Duration::from_secs(1),
        precise: Duration::from_secs(1),
        subscription: None,
    });
    let worker = spawn_state_worker_with_clock(
        FakeRegular {
            captures: Arc::new(Mutex::new(Vec::new())),
        },
        FakePrecise {
            captures: Arc::new(Mutex::new(Vec::new())),
            corrupt_first_mode: false,
        },
        ParkedTransition {
            entered: entered.clone(),
        },
        Arc::new(GatedPublisher {
            publications: Arc::new(Mutex::new(Vec::new())),
            gate_first_regular: AtomicBool::new(false),
            gate: Arc::new((Mutex::new(true), Condvar::new())),
        }),
        context(ClientType::Stable),
        RegularCapturePlan::default(),
        interval_rx.into(),
        Arc::new(Mutex::new(ReaderSynchronization::default())),
        clock,
    );

    wait_until(|| entered.load(Ordering::Acquire)).await;
    timeout(Duration::from_secs(2), worker.shutdown())
        .await
        .expect("shutdown must abort parked transition")
        .expect("shutdown joins every worker");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_joins_precise_even_when_regular_panics() {
    let regular_entered = Arc::new(AtomicBool::new(false));
    let precise_entered = Arc::new(AtomicBool::new(false));
    let precise_dropped = Arc::new(AtomicBool::new(false));
    let precise_gate = Arc::new((Mutex::new(false), Condvar::new()));
    let clock = Arc::new(ManualClock::default());
    let (_interval_tx, interval_rx) = watch::channel(PollIntervals {
        regular: Duration::from_secs(1),
        precise: Duration::from_secs(1),
        subscription: None,
    });
    let worker = spawn_state_worker_with_clock(
        PanickingRegular {
            entered: regular_entered.clone(),
        },
        BlockingPrecise {
            entered: precise_entered.clone(),
            gate: precise_gate.clone(),
            dropped: precise_dropped.clone(),
        },
        RecordingTransition {
            context: context(ClientType::Stable),
            regular: Arc::new(AtomicUsize::new(0)),
            precise_sequences: Arc::new(Mutex::new(Vec::new())),
        },
        Arc::new(GatedPublisher {
            publications: Arc::new(Mutex::new(Vec::new())),
            gate_first_regular: AtomicBool::new(false),
            gate: Arc::new((Mutex::new(true), Condvar::new())),
        }),
        context(ClientType::Stable),
        RegularCapturePlan::default(),
        interval_rx.into(),
        Arc::new(Mutex::new(ReaderSynchronization::default())),
        clock,
    );

    wait_until(|| precise_entered.load(Ordering::Acquire)).await;
    wait_until(|| regular_entered.load(Ordering::Acquire)).await;
    let shutdown = tokio::spawn(worker.shutdown());
    tokio::time::sleep(Duration::from_millis(20)).await;
    let returned_before_precise_finished = shutdown.is_finished();
    let precise_dropped_before_release = precise_dropped.load(Ordering::Acquire);

    {
        let (open, changed) = &*precise_gate;
        *lock(open) = true;
        changed.notify_all();
    }
    let result = timeout(Duration::from_secs(2), shutdown)
        .await
        .expect("shutdown must finish after precise reader is released")
        .expect("shutdown task must not panic");

    assert!(
        !returned_before_precise_finished,
        "shutdown must wait for precise after the regular reader panics"
    );
    assert!(
        !precise_dropped_before_release,
        "the blocked precise reader must not be detached"
    );
    assert!(precise_dropped.load(Ordering::Acquire));
    assert!(matches!(
        result,
        Err(StateWorkerError::ThreadPanicked("regular"))
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancelled_transition_cannot_publish_when_its_result_becomes_ready() {
    let entered = Arc::new(AtomicBool::new(false));
    let release = Arc::new(Notify::new());
    let publications = Arc::new(Mutex::new(Vec::new()));
    let clock = Arc::new(ManualClock::default());
    let (_interval_tx, interval_rx) = watch::channel(PollIntervals {
        regular: Duration::from_secs(1),
        precise: Duration::from_secs(1),
        subscription: None,
    });
    let worker = spawn_state_worker_with_clock(
        FakeRegular {
            captures: Arc::new(Mutex::new(Vec::new())),
        },
        FakePrecise {
            captures: Arc::new(Mutex::new(Vec::new())),
            corrupt_first_mode: true,
        },
        ReleasedAfterCancellationTransition {
            entered: entered.clone(),
            release: release.clone(),
            context: context(ClientType::Stable),
        },
        Arc::new(GatedPublisher {
            publications: publications.clone(),
            gate_first_regular: AtomicBool::new(false),
            gate: Arc::new((Mutex::new(true), Condvar::new())),
        }),
        context(ClientType::Stable),
        RegularCapturePlan::default(),
        interval_rx.into(),
        Arc::new(Mutex::new(ReaderSynchronization::default())),
        clock.clone(),
    );

    wait_until(|| entered.load(Ordering::Acquire)).await;
    clock.cancel();
    release.notify_waiters();
    sleep(Duration::from_millis(10)).await;
    worker
        .shutdown()
        .await
        .expect("cancelled transition exits cleanly");
    assert!(lock(&publications).is_empty());
}

struct TimedRegular {
    captures: Arc<Mutex<Vec<(std::time::Instant, bool)>>>,
}

impl RegularSnapshotReader for TimedRegular {
    fn capture_regular(
        &mut self,
        request: &RegularCaptureRequest,
    ) -> Result<Arc<RawRegularSnapshot>, StateWorkerError> {
        lock(&self.captures).push((std::time::Instant::now(), request.context.api_cadence));
        Ok(
            RawRegularSnapshotBuilder::new(request.context, Ok(ReadStatus::Ready(raw_global())))
                .publish(),
        )
    }
}

struct DeliveryRecorder(Arc<Mutex<Vec<(String, Delivery)>>>);

impl SnapshotPublisher<String> for DeliveryRecorder {
    fn publish(&self, publication: String, delivery: Delivery) -> Result<(), StateWorkerError> {
        lock(&self.0).push((publication, delivery));
        Ok(())
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscriber_demand_samples_faster_but_keeps_api_cadence() {
    let regular_interval = Duration::from_millis(40);
    let captures = Arc::new(Mutex::new(Vec::new()));
    let publications = Arc::new(Mutex::new(Vec::new()));
    let (interval_tx, interval_rx) = watch::channel(PollIntervals {
        regular: regular_interval,
        precise: Duration::from_secs(30),
        subscription: Some(SubscriptionPolling {
            interval: Duration::from_millis(1),
            demand: SubscriptionDemand {
                regular: true,
                precise: true,
                ..Default::default()
            },
        }),
    });
    let worker = spawn_state_worker_with_clock(
        TimedRegular {
            captures: captures.clone(),
        },
        FakePrecise {
            captures: Arc::new(Mutex::new(Vec::new())),
            corrupt_first_mode: false,
        },
        RecordingTransition {
            context: context(ClientType::Stable),
            regular: Arc::new(AtomicUsize::new(0)),
            precise_sequences: Arc::new(Mutex::new(Vec::new())),
        },
        Arc::new(DeliveryRecorder(publications.clone())),
        context(ClientType::Stable),
        RegularCapturePlan::default(),
        interval_rx.into(),
        Arc::new(Mutex::new(ReaderSynchronization::default())),
        Arc::new(SystemWorkerClock::default()),
    );

    wait_until(|| lock(&captures).iter().filter(|(_, api)| *api).count() >= 3).await;
    // Everything recorded before the switch was evaluated with demand active.
    let subscribed = lock(&captures).len();
    let subscribed_publications = lock(&publications).len();
    interval_tx.send_replace(PollIntervals {
        regular: regular_interval,
        precise: Duration::from_secs(30),
        subscription: None,
    });
    wait_until(|| lock(&captures).len() >= subscribed + 3).await;
    worker.shutdown().await.expect("clean shutdown");

    let captures = lock(&captures).clone();
    let (cadence, fast): (Vec<&(std::time::Instant, bool)>, Vec<_>) =
        captures[..subscribed].iter().partition(|(_, api)| *api);
    assert!(
        fast.len() > cadence.len(),
        "subscriber samples run between API cadence captures"
    );
    assert!(
        cadence
            .windows(2)
            .all(|pair| pair[1].0 - pair[0].0 >= regular_interval),
        "API cadence captures stay POLL_RATE apart"
    );
    // One capture may already have been in flight when demand ended.
    assert!(
        captures[subscribed + 1..].iter().all(|(_, api)| *api),
        "without demand every capture is an API capture"
    );

    let publications = lock(&publications)[..subscribed_publications].to_vec();
    let regular = publications
        .iter()
        .filter(|(kind, _)| kind == "regular")
        .map(|(_, delivery)| *delivery)
        .collect::<Vec<_>>();
    assert!(regular.contains(&Delivery::All) && regular.contains(&Delivery::Live));
    let precise = publications
        .iter()
        .filter(|(kind, _)| kind == "precise")
        .map(|(_, delivery)| *delivery)
        .collect::<Vec<_>>();
    assert_eq!(precise.first(), Some(&Delivery::Precise));
    assert!(
        precise[1..]
            .iter()
            .all(|delivery| *delivery == Delivery::Live),
        "precise samples inside one PRECISE_DATA_POLL_RATE interval are live-only"
    );
}

#[test]
fn subscription_polling_tightens_only_requested_active_lanes() {
    let regular = Duration::from_millis(150);
    let precise = Duration::from_millis(10);
    let fast = Duration::from_millis(1);
    for playing in [false, true] {
        for regular_requested in [false, true] {
            for precise_requested in [false, true] {
                let intervals = PollIntervals {
                    regular,
                    precise,
                    subscription: Some(SubscriptionPolling {
                        interval: fast,
                        demand: SubscriptionDemand {
                            gameplay_regular: regular_requested,
                            gameplay_precise: precise_requested,
                            ..Default::default()
                        },
                    }),
                };
                assert_eq!(
                    intervals.regular_delay(playing),
                    if playing && regular_requested {
                        fast
                    } else {
                        regular
                    }
                );
                assert_eq!(
                    intervals.precise_delay(playing),
                    if playing && precise_requested {
                        fast
                    } else {
                        precise
                    }
                );
            }
        }
    }
    let menu_clock = PollIntervals {
        regular,
        precise,
        subscription: Some(SubscriptionPolling {
            interval: fast,
            demand: SubscriptionDemand {
                precise: true,
                ..Default::default()
            },
        }),
    };
    assert_eq!(menu_clock.precise_delay(false), fast);
    assert_eq!(menu_clock.regular_delay(false), regular);
    let slower = PollIntervals {
        subscription: Some(SubscriptionPolling {
            interval: Duration::from_secs(1),
            demand: SubscriptionDemand {
                regular: true,
                precise: true,
                ..Default::default()
            },
        }),
        ..menu_clock
    };
    assert_eq!(slower.precise_delay(true), precise);
    assert_eq!(slower.regular_delay(true), regular);
}

#[test]
fn gameplay_demand_survives_menu_play_menu_transitions() {
    let intervals = PollIntervals {
        regular: Duration::from_millis(150),
        precise: Duration::from_millis(10),
        subscription: Some(SubscriptionPolling {
            interval: Duration::from_millis(1),
            demand: SubscriptionDemand {
                gameplay_regular: true,
                gameplay_precise: true,
                ..Default::default()
            },
        }),
    };
    for playing in [false, true, false] {
        assert_eq!(
            intervals.regular_delay(playing),
            if playing {
                Duration::from_millis(1)
            } else {
                intervals.regular
            }
        );
        assert_eq!(
            intervals.precise_delay(playing),
            if playing {
                Duration::from_millis(1)
            } else {
                intervals.precise
            }
        );
        let demand = intervals.subscription.unwrap().demand;
        assert!(demand.gameplay_regular && demand.gameplay_precise);
        assert!(!demand.regular && !demand.precise);
    }
}

/// Real worker-thread scheduling with the same offline reader/transition harness
/// as the cadence tests. CPU is this process's consumed CPU, not wall-clock wait.
#[cfg(target_os = "linux")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "five-second CPU/wakeup samples; run alone in a release test binary"]
async fn subscription_cadence_cpu_and_wakeups() {
    use crate::server::v2_subscription::{
        V2SubscriptionHub, V2SubscriptionProvider, V2SubscriptionSnapshot,
    };
    #[derive(Clone)]
    struct FixtureDocument(watch::Receiver<u64>);
    impl V2SubscriptionSnapshot for FixtureDocument {
        fn revision(&self) -> u64 {
            *self.0.borrow()
        }
        fn document(
            &self,
            _pointers: &[&[String]],
        ) -> Result<Option<serde_json::Value>, crate::api::ApiError> {
            Ok(Some(serde_json::json!({
                "client": "lazer", "settings": {"mode": {"number": 0}},
                "folders": {"skin": null}, "beatmap": {"checksum": "fixture"}, "keys": {"all": []}
            })))
        }
    }
    impl V2SubscriptionProvider for FixtureDocument {
        fn subscribe(&self) -> watch::Receiver<u64> {
            self.0.clone()
        }
        fn capture(&self) -> Arc<dyn V2SubscriptionSnapshot> {
            Arc::new(self.clone())
        }
    }
    fn cpu_seconds() -> f64 {
        let mut time = libc::timespec {
            tv_sec: 0,
            tv_nsec: 0,
        };
        // SAFETY: time is valid writable storage and the process CPU clock is
        // independent of any user's application or device.
        assert_eq!(
            unsafe { libc::clock_gettime(libc::CLOCK_PROCESS_CPUTIME_ID, &mut time) },
            0
        );
        time.tv_sec as f64 + time.tv_nsec as f64 / 1_000_000_000.0
    }
    for (name, paths) in [
        ("none", vec![]),
        (
            "slow",
            vec![
                "/settings/mode",
                "/client",
                "/folders/skin",
                "/beatmap/checksum",
            ],
        ),
        ("precise-play", vec!["/keys"]),
        ("whole-document", vec![""]),
    ] {
        let captures = Arc::new(Mutex::new(Vec::new()));
        let precise = Arc::new(Mutex::new(Vec::new()));
        let (_events, events) = watch::channel(1);
        let hub =
            V2SubscriptionHub::new(Arc::new(FixtureDocument(events)), Duration::from_millis(1));
        let runner = tokio::spawn({
            let hub = hub.clone();
            async move { hub.run().await }
        });
        let connection = if paths.is_empty() {
            None
        } else {
            let (connection, mut receiver) = hub.connect();
            connection
                .subscribe(paths.into_iter().map(str::to_owned).collect())
                .unwrap();
            receiver.recv().await.expect("initial subscription frame");
            connection.write_completed();
            Some((connection, receiver))
        };
        let (_config, config) = watch::channel(Arc::new(crate::config::Config::default()));
        let intervals = PollIntervalUpdates::Config {
            config,
            demand: hub.demand(),
        };
        let worker = spawn_state_worker_with_clock(
            TimedRegular {
                captures: Arc::clone(&captures),
            },
            FakePrecise {
                captures: Arc::clone(&precise),
                corrupt_first_mode: false,
            },
            RecordingTransition {
                context: context(ClientType::Lazer),
                regular: Arc::new(AtomicUsize::new(0)),
                precise_sequences: Arc::new(Mutex::new(Vec::new())),
            },
            Arc::new(DeliveryRecorder(Arc::new(Mutex::new(Vec::new())))),
            context(ClientType::Lazer),
            RegularCapturePlan::default(),
            intervals,
            Arc::new(Mutex::new(ReaderSynchronization::default())),
            Arc::new(SystemWorkerClock::default()),
        );
        sleep(Duration::from_millis(250)).await;
        let before = (lock(&captures).len(), lock(&precise).len());
        let cpu_start = cpu_seconds();
        let start = std::time::Instant::now();
        sleep(Duration::from_secs(5)).await;
        let elapsed = start.elapsed().as_secs_f64();
        let cpu = cpu_seconds() - cpu_start;
        let after = (lock(&captures).len(), lock(&precise).len());
        worker.shutdown().await.unwrap();
        drop(connection);
        hub.shutdown();
        runner.await.unwrap();
        println!(
            "cadence={name} seconds={elapsed:.6} regular_hz={:.3} precise_hz={:.3} cpu_seconds={cpu:.6} cpu_percent={:.3}",
            (after.0 - before.0) as f64 / elapsed,
            (after.1 - before.1) as f64 / elapsed,
            cpu / elapsed * 100.0
        );
    }
}
