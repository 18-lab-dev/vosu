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
    let precise_entered = Arc::new(AtomicBool::new(false));
    let precise_dropped = Arc::new(AtomicBool::new(false));
    let precise_gate = Arc::new((Mutex::new(false), Condvar::new()));
    let clock = Arc::new(ManualClock::default());
    let (_interval_tx, interval_rx) = watch::channel(PollIntervals {
        regular: Duration::from_secs(1),
        precise: Duration::from_secs(1),
    });
    let worker = spawn_state_worker_with_clock(
        PanickingRegular,
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
