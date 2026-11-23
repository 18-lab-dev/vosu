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

use super::ebpf::{comm_hint_key, length_slots};

const RAW_COMM_LEN: usize = 16;

fn raw_comm_len(comm: &[u8; RAW_COMM_LEN]) -> usize {
    comm.iter().position(|&b| b == 0).unwrap_or(RAW_COMM_LEN)
}

/// Rust model of `candidate_comm`'s length x start enumeration in
/// `bpf/process_events.bpf.c`: for every length with a registered
/// `comm_hint_len_slots` entry (as `length_slots` would produce) that does
/// not exceed `comm`'s run of non-NUL bytes, probes every start position
/// whose window stays within `comm_len`, building the exact
/// `comm_hint_key` key the real `comm_hints` map lookup would use and
/// checking it against the registered key set. `candidate_comm` skips a
/// length whose slot is unset (not merely probing and missing), so this
/// mirrors that gate explicitly rather than relying on `registered_keys`
/// alone.
fn model_candidate_comm(
    comm: &[u8; RAW_COMM_LEN],
    slots: &[u8; MAX_COMM_HINT_LEN],
    registered_keys: &BTreeSet<Vec<u8>>,
) -> bool {
    let comm_len = raw_comm_len(comm);
    if comm_len == 0 {
        return false;
    }
    for len in 1..=MAX_COMM_HINT_LEN {
        if slots[len - 1] == 0 || len > comm_len {
            continue;
        }
        let mut start = 0_usize;
        while start + len <= comm_len {
            let key = comm_hint_key(&comm[start..start + len]);
            if registered_keys.contains(&key.to_vec()) {
                return true;
            }
            start += 1;
        }
    }
    false
}

fn registered_keys_for(hints: &CommHints) -> BTreeSet<Vec<u8>> {
    hints
        .iter()
        .map(|hint| comm_hint_key(hint).to_vec())
        .collect()
}

#[test]
fn model_candidate_comm_matches_comm_hints_across_comm_and_hint_lengths() {
    fn padded(content: &[u8]) -> [u8; RAW_COMM_LEN] {
        let mut buffer = [0_u8; RAW_COMM_LEN];
        buffer[..content.len()].copy_from_slice(content);
        buffer
    }

    // Hint sets covering short, mid, and the maximum (15) allowed length,
    // including two hints that share a length to exercise length_slots'
    // bitmap-not-counter behavior.
    let hint_sets: &[&[&str]] = &[
        &["osu!", "osulazer.exe"],
        &["a"],
        &[&"z".repeat(MAX_COMM_HINT_LEN)],
        &["ab", "cd", "efgh"],
        &[],
    ];

    for hint_strs in hint_sets {
        let hints = CommHints::new(hint_strs.iter().copied()).unwrap();
        let slots = length_slots(hints.iter());
        let registered_keys = registered_keys_for(&hints);

        for comm_len in 0..=RAW_COMM_LEN {
            for fill in [b'a', b'!', b'z', b'h'] {
                let comm = padded(&vec![fill; comm_len]);
                assert_eq!(
                    model_candidate_comm(&comm, &slots, &registered_keys),
                    hints.matches(&comm),
                    "hints {hint_strs:?}, comm_len {comm_len}, fill {fill}"
                );
            }
        }

        for comm in [
            b"osu!\0\0\0\0\0\0\0\0\0\0\0\0".as_slice(),
            b"xosu!\0\0\0\0\0\0\0\0\0\0\0",
            b"prefix-osu!\0\0\0\0\0",
            b"unrelated\0\0\0\0\0\0\0",
            &[0_u8; RAW_COMM_LEN],
        ] {
            let comm: [u8; RAW_COMM_LEN] = comm.try_into().unwrap();
            assert_eq!(
                model_candidate_comm(&comm, &slots, &registered_keys),
                hints.matches(&comm),
                "hints {hint_strs:?}, comm {comm:?}"
            );
        }
    }
}

#[test]
fn comm_hint_key_pads_trailing_bytes_with_zero() {
    let key = comm_hint_key(b"ab");
    assert_eq!(key[0], 2);
    assert_eq!(&key[1..3], b"ab");
    assert!(key[3..].iter().all(|&b| b == 0));
}

#[test]
fn length_slots_marks_only_registered_lengths_once_each() {
    let hints = CommHints::new(["ab", "wxyz", "yz"]).unwrap(); // lengths 2, 4, 2
    let slots = length_slots(hints.iter());
    for len in 1..=MAX_COMM_HINT_LEN {
        let expected = hints.iter().any(|hint| hint.len() == len);
        assert_eq!(slots[len - 1] != 0, expected, "len {len}");
        if expected {
            assert_eq!(slots[len - 1] as usize, len);
        }
    }
}

#[test]
fn length_slots_of_no_hints_is_all_zero() {
    assert_eq!(length_slots(std::iter::empty()), [0_u8; MAX_COMM_HINT_LEN]);
}

#[test]
fn length_slots_widen_narrow_sequence_stays_a_superset_during_transition() {
    // Mirrors EbpfCommFilter::set_comm_hints's own sequence: widen to the
    // union of old and new lengths first, diff comm_hints, then narrow to
    // exactly the new (post-diff shadow) lengths -- so every length that
    // was, or is about to be, registered stays probed for the whole
    // transition instead of ever being narrower than either side.
    let old_hints = CommHints::new(["abcd", &"z".repeat(MAX_COMM_HINT_LEN)]).unwrap(); // lens 4, 15
    let new_hints = CommHints::new(["xy", "abcd"]).unwrap(); // lens 2, 4

    let old_slots = length_slots(old_hints.iter());
    let new_slots = length_slots(new_hints.iter());
    let union_slots = length_slots(old_hints.iter().chain(new_hints.iter()));

    for len in 1..=MAX_COMM_HINT_LEN {
        let old_present = old_slots[len - 1] != 0;
        let new_present = new_slots[len - 1] != 0;
        let union_present = union_slots[len - 1] != 0;
        assert_eq!(union_present, old_present || new_present, "len {len}");
    }

    // Length 15 is only ever registered by old_hints, so the union must
    // still carry it even though the final (narrowed) target drops it.
    assert_ne!(union_slots[MAX_COMM_HINT_LEN - 1], 0);
    assert_eq!(new_slots[MAX_COMM_HINT_LEN - 1], 0);
    // Length 2 is only ever registered by new_hints, so the union must
    // already carry it before the diff adds its comm_hints entry.
    assert_ne!(union_slots[1], 0);
    assert_eq!(old_slots[1], 0);
}
