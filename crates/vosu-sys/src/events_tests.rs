use super::*;

struct FailOnceTrackingControl(AtomicU64);

impl ProcessTrackingControl for FailOnceTrackingControl {
    fn replace_tracked(&self, _tracked: &BTreeSet<Pid>) -> io::Result<()> {
        if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
            Err(io::Error::other("synthetic map failure"))
        } else {
            Ok(())
        }
    }
}

struct NoopCommFilter;

impl ProcessCommFilter for NoopCommFilter {
    fn set_comm_hints(&self, _hints: &CommHints) -> io::Result<()> {
        Ok(())
    }
}

struct FailOnceCommFilter(AtomicU64);

impl ProcessCommFilter for FailOnceCommFilter {
    fn set_comm_hints(&self, _hints: &CommHints) -> io::Result<()> {
        if self.0.fetch_add(1, Ordering::Relaxed) == 0 {
            Err(io::Error::other("synthetic comm-filter failure"))
        } else {
            Ok(())
        }
    }
}

fn events_with_tracking_control(
    tracking_control: Arc<dyn ProcessTrackingControl>,
) -> LinuxProcessEvents {
    LinuxProcessEvents {
        mode: ProcessEventMode::Ebpf,
        notify: Arc::new(Notify::new()),
        healthy: Arc::new(AtomicBool::new(true)),
        event_count: Arc::new(AtomicU64::new(0)),
        reconciliation_request_count: Arc::new(AtomicU64::new(0)),
        queue: Arc::new(Mutex::new(EventQueue::default())),
        tracking_control: Some(tracking_control),
        comm_filter: Arc::new(NoopCommFilter),
        cancel: Arc::new(AtomicBool::new(false)),
        task: None,
    }
}

#[test]
fn tracking_control_failure_preserves_queue_and_successful_retry_commits() {
    let control = Arc::new(FailOnceTrackingControl(AtomicU64::new(0)));
    let events = events_with_tracking_control(control.clone());
    events
        .queue
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .replace_tracked([3]);

    events.replace_tracked([7]);
    assert!(!events.healthy());
    assert_eq!(events.reconciliation_request_count(), 1);
    assert_eq!(
        events
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tracked,
        BTreeSet::from([3]),
    );

    events.replace_tracked([8]);
    assert_eq!(control.0.load(Ordering::Relaxed), 2);
    assert_eq!(
        events
            .queue
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .tracked,
        BTreeSet::from([8]),
    );
    assert_eq!(events.reconciliation_request_count(), 1);
    assert!(events.drain().needs_reconcile);
}

#[test]
fn absent_mode_uses_auto_while_explicit_polling_stays_disabled() {
    assert_eq!(
        ProcessEventMode::configured(None).unwrap(),
        Some(ProcessEventMode::Auto)
    );
    for value in ["", "off", "poll"] {
        assert_eq!(ProcessEventMode::configured(Some(value)).unwrap(), None);
    }
    assert_eq!(
        ProcessEventMode::configured(Some("cn-proc")).unwrap(),
        Some(ProcessEventMode::CnProc)
    );
    assert_eq!(
        ProcessEventMode::configured(Some("ebpf")).unwrap(),
        Some(ProcessEventMode::Ebpf)
    );
    assert!(ProcessEventMode::configured(Some("unknown")).is_err());
}

#[test]
fn queue_ignores_untracked_exit_and_coalesces_same_tgid() {
    let mut queue = EventQueue::default();
    assert!(!queue.push(ProcessEvent {
        tgid: 8,
        kind: ProcessEventKind::Exit,
    }));
    assert!(queue.push(ProcessEvent {
        tgid: 8,
        kind: ProcessEventKind::Inspect,
    }));
    assert!(queue.push(ProcessEvent {
        tgid: 8,
        kind: ProcessEventKind::Exit,
    }));
    let batch = queue.drain();
    assert!(batch.inspect.is_empty());
    assert!(batch.exit.is_empty());
    assert!(queue.push(ProcessEvent {
        tgid: 7,
        kind: ProcessEventKind::Inspect,
    }));
    queue.replace_tracked([7]);
    assert!(queue.push(ProcessEvent {
        tgid: 7,
        kind: ProcessEventKind::Exit,
    }));

    let batch = queue.drain();
    assert_eq!(batch.exit, [7]);
    assert!(batch.inspect.is_empty());
    assert!(!batch.needs_reconcile);
}

#[test]
fn ebpf_queue_reconciles_untracked_exit() {
    let mut queue = EventQueue {
        reconcile_untracked_exit: true,
        ..EventQueue::default()
    };

    assert!(queue.push(ProcessEvent {
        tgid: 8,
        kind: ProcessEventKind::Exit,
    }));
    let batch = queue.drain();
    assert!(batch.inspect.is_empty());
    assert!(batch.exit.is_empty());
    assert!(batch.needs_reconcile);
}

#[test]
fn queue_suppresses_tracked_inspects_across_drains() {
    let mut queue = EventQueue::default();
    queue.replace_tracked([7]);

    for _ in 0..2 {
        assert!(!queue.push(ProcessEvent {
            tgid: 7,
            kind: ProcessEventKind::Inspect,
        }));
        let batch = queue.drain();
        assert!(batch.inspect.is_empty());
        assert!(batch.exit.is_empty());
    }
}

#[test]
fn queue_preserves_exit_and_reconciles_inspect_conflicts() {
    let mut queue = EventQueue::default();
    queue.replace_tracked([7]);
    assert!(queue.push(ProcessEvent {
        tgid: 7,
        kind: ProcessEventKind::Exit,
    }));
    assert!(queue.push(ProcessEvent {
        tgid: 7,
        kind: ProcessEventKind::Inspect,
    }));

    let batch = queue.drain();
    assert_eq!(batch.exit, [7]);
    assert!(batch.inspect.is_empty());
    assert!(batch.needs_reconcile);

    assert!(queue.push(ProcessEvent {
        tgid: 7,
        kind: ProcessEventKind::Inspect,
    }));
    let batch = queue.drain();
    assert!(batch.inspect.is_empty());
    assert!(batch.exit.is_empty());
    assert!(batch.needs_reconcile);
}

#[test]
fn queue_keeps_untracked_inspects_for_generic_rename_discovery() {
    let mut queue = EventQueue::default();

    for _ in 0..2 {
        assert!(queue.push(ProcessEvent {
            tgid: 7,
            kind: ProcessEventKind::Inspect,
        }));
        let batch = queue.drain();
        assert_eq!(batch.inspect, [7]);
        assert!(batch.exit.is_empty());
    }
}

#[test]
fn replacing_tracked_set_discards_removed_exit_state() {
    let mut queue = EventQueue::default();
    queue.replace_tracked([7]);
    assert!(queue.push(ProcessEvent {
        tgid: 7,
        kind: ProcessEventKind::Exit,
    }));
    queue.drain();

    queue.replace_tracked([7, 8]);
    assert!(queue.exiting.contains(&7));
    queue.replace_tracked([8]);
    assert!(!queue.exiting.contains(&7));
    assert!(queue.push(ProcessEvent {
        tgid: 7,
        kind: ProcessEventKind::Inspect,
    }));
    let batch = queue.drain();
    assert_eq!(batch.inspect, [7]);
    assert!(batch.exit.is_empty());
}

#[test]
fn queue_overflow_latches_one_reconciliation() {
    let mut queue = EventQueue::default();
    for tgid in 1..=MAX_PENDING_EVENTS as Pid {
        assert!(queue.push(ProcessEvent {
            tgid,
            kind: ProcessEventKind::Inspect,
        }));
    }
    assert!(!queue.push(ProcessEvent {
        tgid: MAX_PENDING_EVENTS as Pid + 1,
        kind: ProcessEventKind::Inspect,
    }));
    assert!(queue.needs_reconcile);
    assert!(queue.drain().needs_reconcile);
    assert!(!queue.drain().needs_reconcile);
}
#[test]
fn reconciliation_counter_only_records_new_latches() {
    let counter = AtomicU64::new(0);
    let mut queue = EventQueue::default();

    record_reconciliation_request(&counter, queue.request_reconcile());
    record_reconciliation_request(&counter, queue.request_reconcile());
    assert_eq!(counter.load(Ordering::Relaxed), 1);

    assert!(queue.drain().needs_reconcile);
    record_reconciliation_request(&counter, queue.request_reconcile());
    assert_eq!(counter.load(Ordering::Relaxed), 2);
}

#[test]
fn queue_bounds_exit_tracking_and_reconciles_overflow() {
    let mut queue = EventQueue::default();
    queue.replace_tracked(1..=MAX_PENDING_EVENTS as Pid + 1);

    for tgid in 1..=MAX_PENDING_EVENTS as Pid {
        assert!(queue.push(ProcessEvent {
            tgid,
            kind: ProcessEventKind::Exit,
        }));
    }
    assert!(!queue.push(ProcessEvent {
        tgid: MAX_PENDING_EVENTS as Pid + 1,
        kind: ProcessEventKind::Exit,
    }));

    assert_eq!(queue.exiting.len(), MAX_PENDING_EVENTS);
    assert!(queue.needs_reconcile);
}

fn events_with_comm_filter(comm_filter: Arc<dyn ProcessCommFilter>) -> LinuxProcessEvents {
    LinuxProcessEvents {
        mode: ProcessEventMode::Ebpf,
        notify: Arc::new(Notify::new()),
        healthy: Arc::new(AtomicBool::new(true)),
        event_count: Arc::new(AtomicU64::new(0)),
        reconciliation_request_count: Arc::new(AtomicU64::new(0)),
        queue: Arc::new(Mutex::new(EventQueue::default())),
        tracking_control: None,
        comm_filter,
        cancel: Arc::new(AtomicBool::new(false)),
        task: None,
    }
}

#[test]
fn comm_filter_failure_preserves_health_loss_and_reconcile_request_through_successful_retry() {
    let filter = Arc::new(FailOnceCommFilter(AtomicU64::new(0)));
    let events = events_with_comm_filter(filter.clone());
    let hints = CommHints::new(["osu!"]).unwrap();

    assert!(events.set_comm_hints(&hints).is_err());
    assert!(!events.healthy());
    assert_eq!(events.reconciliation_request_count(), 1);

    // A successful retry applies the hints but, like the analogous
    // tracking-control retry, neither resets `healthy` nor clears the
    // pending reconciliation it already latched: both stay as the
    // failure left them until an actual reconcile runs.
    assert!(events.set_comm_hints(&hints).is_ok());
    assert_eq!(filter.0.load(Ordering::Relaxed), 2);
    assert!(!events.healthy());
    assert_eq!(events.reconciliation_request_count(), 1);
    assert!(events.drain().needs_reconcile);
}

#[test]
fn comm_hints_reject_empty_overlong_and_oversized_sets() {
    assert_eq!(
        CommHints::new([""]).unwrap_err(),
        CommHintsError::Empty { index: 0 }
    );
    assert_eq!(
        CommHints::new(["osu!", ""]).unwrap_err(),
        CommHintsError::Empty { index: 1 }
    );

    let max_len_hint = vec![b'a'; MAX_COMM_HINT_LEN];
    assert_eq!(CommHints::new([max_len_hint]).unwrap().len(), 1);

    let over_len_hint = vec![b'a'; MAX_COMM_HINT_LEN + 1];
    assert_eq!(
        CommHints::new([over_len_hint]).unwrap_err(),
        CommHintsError::TooLong {
            index: 0,
            len: MAX_COMM_HINT_LEN + 1,
        }
    );

    let too_many: Vec<String> = (0..=MAX_COMM_HINTS)
        .map(|index| format!("h{index}"))
        .collect();
    assert_eq!(
        CommHints::new(too_many).unwrap_err(),
        CommHintsError::TooMany {
            count: MAX_COMM_HINTS + 1,
        }
    );

    let exactly_bounded: Vec<String> = (0..MAX_COMM_HINTS)
        .map(|index| format!("h{index}"))
        .collect();
    assert_eq!(
        CommHints::new(exactly_bounded).unwrap().len(),
        MAX_COMM_HINTS
    );
}

#[test]
fn comm_hints_dedupe_and_match_substrings_like_the_former_hardcoded_filter() {
    let hints = CommHints::new(["osu!", "osulazer.exe", "osu!"]).unwrap();
    assert_eq!(hints.len(), 2);

    assert!(hints.matches(b"bench-osu!-away"));
    assert!(hints.matches(b"osulazer.exe"));
    assert!(!hints.matches(b"unrelated"));
    assert!(!hints.matches(b""));
    assert!(!CommHints::default().matches(b"osu!"));
}

#[test]
fn comm_hints_reject_nul_byte() {
    assert_eq!(
        CommHints::new(["a\0"]).unwrap_err(),
        CommHintsError::ContainsNul { index: 0 }
    );
    assert_eq!(
        CommHints::new(["osu!", "\0"]).unwrap_err(),
        CommHintsError::ContainsNul { index: 1 }
    );
    assert_eq!(
        CommHints::new(["x\0"]).unwrap_err(),
        CommHintsError::ContainsNul { index: 0 }
    );
}

/// Mirrored by `COMM_HINT_MAX_LEN`/`MAX_COMM_HINTS` in
/// `bpf/process_events.bpf.c`; guards the two sides from drifting apart.
#[test]
fn comm_hint_bounds_match_bpf_mirror_constants() {
    assert_eq!(MAX_COMM_HINT_LEN, 15);
    assert_eq!(MAX_COMM_HINTS, 256);
}

struct FailFirstInsertOps {
    fail_hint: &'static [u8],
    failed_once: AtomicBool,
    applied: Mutex<BTreeSet<Vec<u8>>>,
}

impl CommHintMapOps for FailFirstInsertOps {
    fn insert(&self, hint: &[u8]) -> io::Result<()> {
        if hint == self.fail_hint && !self.failed_once.swap(true, Ordering::SeqCst) {
            return Err(io::Error::other("synthetic insert failure"));
        }
        self.applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .insert(hint.to_vec());
        Ok(())
    }

    fn remove(&self, hint: &[u8]) -> io::Result<()> {
        self.applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .remove(hint);
        Ok(())
    }
}

#[test]
fn comm_hint_diff_partial_failure_leaves_shadow_matching_applied_set() {
    let ops = FailFirstInsertOps {
        fail_hint: b"B",
        failed_once: AtomicBool::new(false),
        applied: Mutex::new(BTreeSet::new()),
    };
    let mut shadow: BTreeSet<Vec<u8>> = BTreeSet::from([b"X".to_vec()]);
    let hints = CommHints::new(["A", "B"]).unwrap();

    // X is removed (not in the target set), A inserts, B's insert fails:
    // the shadow must stop exactly where the map actually stopped.
    let error = diff_comm_hints(&mut shadow, &hints, &ops).unwrap_err();
    assert_eq!(error.to_string(), "synthetic insert failure");
    assert_eq!(
        shadow,
        ops.applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    );
    assert_eq!(shadow, BTreeSet::from([b"A".to_vec()]));

    // A later retry resumes from that point and completes the diff.
    diff_comm_hints(&mut shadow, &hints, &ops).unwrap();
    assert_eq!(
        shadow,
        ops.applied
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    );
    assert_eq!(shadow, BTreeSet::from([b"A".to_vec(), b"B".to_vec()]));
}

#[test]
fn comm_matcher_parity_with_former_hardcoded_filter_across_lengths_and_boundaries() {
    // Verbatim copy of the `candidate_comm` function this replaced
    // (hardcoded in this file before the configurable `CommHints` API),
    // kept only to prove the new matcher is byte-for-byte equivalent
    // for the hints it used to hardcode.
    fn former_candidate_comm(comm: &[u8]) -> bool {
        comm.windows(b"osu!".len()).any(|name| name == b"osu!")
            || comm
                .windows(b"osulazer.exe".len())
                .any(|name| name == b"osulazer.exe")
    }

    fn padded(content: &[u8]) -> [u8; 16] {
        let mut buffer = [0_u8; 16];
        buffer[..content.len()].copy_from_slice(content);
        buffer
    }

    let hints = CommHints::new(["osu!", "osulazer.exe"]).unwrap();

    for len in 0..=15_usize {
        for fill in [b'a', b'!', b's'] {
            let content = vec![fill; len];
            let comm = padded(&content);
            assert_eq!(
                hints.matches(&comm[..len]),
                former_candidate_comm(&comm[..len]),
                "length {len}, fill {fill}, unpadded"
            );
            assert_eq!(
                hints.matches(&comm),
                former_candidate_comm(&comm),
                "length {len}, fill {fill}, NUL-padded"
            );
        }
    }

    let boundary_cases: &[&[u8]] = &[
        b"osu!",
        b"xosu!",
        b"osulazer.exe",
        b"prefix-osu!",
        b"-osulazer.exe",
        b"osulazer.ex",
        b"xosulazer.exey",
        b"unrelated",
        b"",
    ];
    for comm in boundary_cases {
        assert_eq!(
            hints.matches(comm),
            former_candidate_comm(comm),
            "boundary case {comm:?}, unpadded"
        );
        assert_eq!(
            hints.matches(&padded(comm)),
            former_candidate_comm(&padded(comm)),
            "boundary case {comm:?}, NUL-padded"
        );
    }

    // Hint at the very end of a full, non-NUL-padded 16-byte comm.
    let mut hint_at_end = [b'x'; 16];
    hint_at_end[12..].copy_from_slice(b"osu!");
    assert_eq!(
        hints.matches(&hint_at_end),
        former_candidate_comm(&hint_at_end)
    );
    assert!(hints.matches(&hint_at_end));

    // A length-15 hint (MAX_COMM_HINT_LEN, the longest allowed) filling
    // every content byte up to the guaranteed-NUL byte 15.
    let long_hint = vec![b'z'; MAX_COMM_HINT_LEN];
    let long_hints = CommHints::new([long_hint.clone()]).unwrap();
    let mut comm15 = [0_u8; 16];
    comm15[..MAX_COMM_HINT_LEN].copy_from_slice(&long_hint);
    assert!(long_hints.matches(&comm15));
    assert!(long_hints.matches(&comm15[..MAX_COMM_HINT_LEN]));
    assert!(!long_hints.matches(&comm15[..MAX_COMM_HINT_LEN - 1]));
}
