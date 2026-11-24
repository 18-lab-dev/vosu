use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use vosu_osu_reader::memory::StablePatterns;
use vosu_osu_reader::process::{
    MemoryError, Pattern, ProcessBitness, ProcessHandle, ProcessMemory, ReadRequest, ReadStatus,
    ScanBackend,
};

use super::{MAX_OUTSTANDING_SCANS, PlanScanBackend, ScanWaiters};
use crate::memory::PlanProcessMemory;
use crate::test_support::{FakeHost, SharedFakeHost};

fn deliverer(waiters: &Arc<ScanWaiters>) -> impl Fn(u64, vosu_plugin_api::ScanResult) + 'static {
    let waiters = Arc::clone(waiters);
    move |id, result| waiters.deliver(id, result)
}

fn ok_result() -> vosu_plugin_api::ScanResult {
    vosu_plugin_api::ScanResult {
        addresses: Vec::new(),
        error: None,
    }
}

// ---------------------------------------------------------------------
// ScanWaiters: bounded capacity, semaphore, timeout, detach, shutdown.
// ---------------------------------------------------------------------

#[test]
fn register_and_deliver_round_trips_addresses() {
    let waiters = ScanWaiters::new();
    let slot = waiters.register(1).expect("register succeeds");
    let id = slot.id();
    waiters.deliver(
        id,
        vosu_plugin_api::ScanResult {
            addresses: vec![0x1000, 0x2000],
            error: None,
        },
    );
    assert_eq!(slot.wait().expect("delivered result"), vec![0x1000, 0x2000]);
}

#[test]
fn deliver_with_an_error_surfaces_as_a_memory_error() {
    let waiters = ScanWaiters::new();
    let slot = waiters.register(1).expect("register succeeds");
    waiters.deliver(
        slot.id(),
        vosu_plugin_api::ScanResult {
            addresses: Vec::new(),
            error: Some(vosu_plugin_api::ErrorInfo {
                code: "backend".to_owned(),
                message: "scan failed".to_owned(),
            }),
        },
    );
    let err = slot
        .wait()
        .expect_err("a delivered error must not become Ok");
    let MemoryError::Backend { message, .. } = err else {
        panic!("expected MemoryError::Backend, got {err:?}");
    };
    assert!(message.contains("backend") && message.contains("scan failed"));
}

#[test]
fn deliver_with_permission_denied_becomes_memory_error_permission_denied() {
    let waiters = ScanWaiters::new();
    let slot = waiters.register(1).expect("register succeeds");
    waiters.deliver(
        slot.id(),
        vosu_plugin_api::ScanResult {
            addresses: Vec::new(),
            error: Some(vosu_plugin_api::ErrorInfo {
                code: "permission_denied".to_owned(),
                message: "ptrace scope forbids this scan".to_owned(),
            }),
        },
    );
    let err = slot
        .wait()
        .expect_err("permission_denied must not become Ok");
    assert!(matches!(err, MemoryError::PermissionDenied { .. }));
}

#[test]
fn an_undelivered_slot_times_out_and_orphans_its_permit_until_a_late_result() {
    // A short result timeout (for `wait` below), but a long permit
    // timeout (for the background thread's `register` calls, which must
    // stay genuinely blocked for this test's own assertions to mean
    // anything).
    let waiters = ScanWaiters::with_timeouts(Duration::from_millis(50), Duration::from_secs(30));
    let slot = waiters.register(1).expect("register succeeds");
    let id = slot.id();
    let err = slot.wait().expect_err("nothing was ever delivered");
    assert!(matches!(err, MemoryError::Backend { .. }));

    // The timed-out slot is orphaned, not released outright (P3 review
    // item 4: no scan cancel opcode exists, so the host may still be
    // running it): a fresh registration past the cap must still block
    // until that late result (or detach/shutdown) actually arrives.
    let waiters_for_thread = Arc::clone(&waiters);
    let (tx, rx) = mpsc::sync_channel(1);
    thread::spawn(move || {
        let mut held = Vec::new();
        for _ in 0..MAX_OUTSTANDING_SCANS {
            held.push(waiters_for_thread.register(2).expect("register succeeds"));
        }
        tx.send(()).unwrap();
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_millis(300)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "the orphaned permit must still be held, so 48 more must not all register yet"
    );

    // The late result finally arrives: it must release the orphaned slot
    // immediately (nobody is waiting on it any more).
    waiters.deliver(
        id,
        vosu_plugin_api::ScanResult {
            addresses: vec![1],
            error: None,
        },
    );
    rx.recv_timeout(Duration::from_secs(5))
        .expect("the permit frees once the late result arrives");
}

#[test]
fn a_stale_orphan_is_reclaimed_without_a_detach() {
    // P3 review item 4: a host that silently drops a reply (its own
    // dispatch queue full, never retried) leaves an orphaned permit held
    // forever with no detach/shutdown ever coming. `register_many` must
    // reclaim it on its own once it has been missing for 2x the result
    // timeout.
    let waiters = ScanWaiters::with_timeouts(Duration::from_millis(20), Duration::from_secs(10));
    let slot = waiters.register(1).expect("register succeeds");
    let err = slot.wait().expect_err("nothing was ever delivered");
    assert!(matches!(err, MemoryError::Backend { .. }));

    // Let real time pass beyond 2x the result timeout (40 ms) with no
    // detach/shutdown ever called, via a bounded wait on a channel
    // nobody signals rather than a disallowed std::thread::sleep.
    let (_never_tx, never_rx) = mpsc::sync_channel::<()>(1);
    let _ = never_rx.recv_timeout(Duration::from_millis(80));

    // The next registration must reclaim the stale orphan on its own:
    // all 48 permits (one of them freed only by reclamation, with no
    // detach/shutdown) must become available promptly.
    let (tx, rx) = mpsc::sync_channel(1);
    let waiters_for_thread = Arc::clone(&waiters);
    thread::spawn(move || {
        let held: Vec<super::ScanSlot> = (0..MAX_OUTSTANDING_SCANS)
            .map(|_| waiters_for_thread.register(2).expect("register succeeds"))
            .collect();
        tx.send(held.len()).unwrap();
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5))
            .expect("the stale orphan's permit was reclaimed"),
        MAX_OUTSTANDING_SCANS
    );
}

#[test]
fn the_semaphore_blocks_the_49th_registration_until_one_releases() {
    let waiters = ScanWaiters::new();
    let mut held: Vec<super::ScanSlot> = (0..MAX_OUTSTANDING_SCANS)
        .map(|_| waiters.register(1).expect("register succeeds"))
        .collect();

    let (started, start_rx) = mpsc::sync_channel(1);
    let (done, done_rx) = mpsc::sync_channel(1);
    let waiters_for_thread = Arc::clone(&waiters);
    thread::spawn(move || {
        started.send(()).unwrap();
        let slot = waiters_for_thread.register(1).expect("register succeeds");
        done.send(slot.id()).unwrap();
    });
    start_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("thread started");

    // Give the blocked thread a bounded chance to (wrongly) finish before
    // any permit is released; it must not.
    assert_eq!(
        done_rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "the 49th registration must block while 48 are already outstanding"
    );

    // Release one permit by delivering its result first (a normal
    // completion, not giving up on a scan the host may still be
    // running): dropping an undelivered slot now orphans it instead
    // (item 4), so this is the realistic way to free a permit in a test.
    let releasing = held.pop().expect("48 slots were held");
    waiters.deliver(
        releasing.id(),
        vosu_plugin_api::ScanResult {
            addresses: Vec::new(),
            error: None,
        },
    );
    drop(releasing);
    let id = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("registration unblocks once a permit frees up");
    assert!(id >= 1);
}

#[test]
fn register_many_grants_a_whole_group_or_none_of_it() {
    // A 60-permit request exceeds the semaphore's own total capacity
    // (48): `PlanScanBackend::scan_batch` must never submit a group this
    // large (it groups into chunks of at most 48), so `register_many`
    // asserts against it rather than silently blocking forever.
    let waiters = ScanWaiters::new();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        waiters.register_many(1, MAX_OUTSTANDING_SCANS + 12)
    }));
    assert!(
        result.is_err(),
        "a group above the semaphore's own capacity must be rejected, not accepted"
    );
}

#[test]
fn register_many_atomically_reserves_the_whole_group() {
    let waiters = ScanWaiters::new();
    // Fill 40 of 48 permits individually first.
    let mut held: Vec<super::ScanSlot> = (0..40)
        .map(|_| waiters.register(1).expect("register succeeds"))
        .collect();

    // A group of 10 cannot fit (only 8 free): it must block entirely,
    // never partially claim the 8 that are free.
    let (done, done_rx) = mpsc::sync_channel(1);
    let waiters_for_thread = Arc::clone(&waiters);
    thread::spawn(move || {
        let slots = waiters_for_thread
            .register_many(1, 10)
            .expect("register_many succeeds");
        done.send(slots.len()).unwrap();
    });
    assert_eq!(
        done_rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "a 10-permit group must not proceed while only 8 are free"
    );

    // Free one more (9 free) by delivering, then dropping, one more held
    // slot — still not enough for the group of 10.
    let release_one = |waiters: &Arc<ScanWaiters>, held: &mut Vec<super::ScanSlot>| {
        let slot = held.pop().expect("a held slot remains");
        waiters.deliver(
            slot.id(),
            vosu_plugin_api::ScanResult {
                addresses: Vec::new(),
                error: None,
            },
        );
        drop(slot);
    };
    release_one(&waiters, &mut held);
    assert_eq!(
        done_rx.recv_timeout(Duration::from_millis(200)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "9 free permits still is not the whole group of 10"
    );

    // Freeing one more (10 free) must finally let the whole group through
    // at once.
    release_one(&waiters, &mut held);
    assert_eq!(
        done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the group registers once 10 are free"),
        10
    );
}

#[test]
fn fifo_admission_lets_a_16_permit_group_through_despite_newer_1_permit_requests() {
    // P2 review item 2: first-fit admission lets a steady stream of
    // 1-permit requests starve a larger group indefinitely, since a
    // freed permit always goes to whichever waiter happens to fit. FIFO
    // ticket order must instead give the *older* request priority, even
    // though it needs more permits than a newer, smaller one that would
    // otherwise fit right away.
    let waiters = ScanWaiters::with_timeouts(Duration::from_secs(30), Duration::from_secs(10));
    let mut held: Vec<super::ScanSlot> = (0..MAX_OUTSTANDING_SCANS)
        .map(|_| waiters.register(1).expect("register succeeds"))
        .collect();

    // The 16-permit group registers first (an older ticket) and blocks,
    // since all 48 permits are held.
    let (group_started, group_started_rx) = mpsc::sync_channel(1);
    let (group_done, group_done_rx) = mpsc::sync_channel(1);
    let waiters_for_group = Arc::clone(&waiters);
    thread::spawn(move || {
        group_started.send(()).unwrap();
        let slots = waiters_for_group
            .register_many(1, 16)
            .expect("the 16-group eventually gets in");
        group_done.send(slots.len()).unwrap();
    });
    group_started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("group thread started");
    // Bounded real-time margin (not a disallowed std::thread::sleep) for
    // the group thread to have actually taken its ticket inside
    // `register_many` before the competitor below takes a later one:
    // the channel send above only proves the thread started running,
    // not that it has reached the lock yet.
    let (_settle_tx, settle_rx) = mpsc::sync_channel::<()>(1);
    let _ = settle_rx.recv_timeout(Duration::from_millis(50));

    // Free exactly one permit: not enough for the 16-group, but enough
    // for a 1-permit request — except a newer 1-permit request must
    // still queue behind the older, larger ticket.
    let released = held.pop().expect("48 slots were held");
    waiters.deliver(released.id(), ok_result());
    drop(released);

    let (competitor_done, competitor_done_rx) = mpsc::sync_channel(1);
    let waiters_for_competitor = Arc::clone(&waiters);
    thread::spawn(move || {
        let _slot = waiters_for_competitor
            .register(99)
            .expect("register eventually succeeds");
        competitor_done.send(()).unwrap();
    });
    assert_eq!(
        competitor_done_rx.recv_timeout(Duration::from_millis(300)),
        Err(mpsc::RecvTimeoutError::Timeout),
        "a newer 1-permit request must queue behind the older, larger ticket, even though it would otherwise fit"
    );

    // Free 15 more (16 total): the 16-group, being the oldest ticket,
    // must now be admitted before the still-waiting competitor.
    for _ in 0..15 {
        let slot = held.pop().expect("enough slots remain");
        waiters.deliver(slot.id(), ok_result());
        drop(slot);
    }
    assert_eq!(
        group_done_rx
            .recv_timeout(Duration::from_secs(5))
            .expect("the 16-group is admitted once 16 permits are free"),
        16
    );

    // Release the remaining 32 originally-held permits too, so there is
    // room left for the competitor's single permit (the group's own 16
    // alone would otherwise still fill the semaphore).
    for slot in held.drain(..) {
        waiters.deliver(slot.id(), ok_result());
        drop(slot);
    }

    // With the older ticket satisfied, the competitor's 1-permit request
    // can finally proceed too.
    competitor_done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the competitor is admitted once it becomes the oldest ticket");
}

#[test]
fn detach_releases_only_the_matching_handles_pending_slots() {
    let waiters = ScanWaiters::with_timeout(Duration::from_secs(30));
    let for_a = waiters.register(1).expect("register succeeds");
    let for_b = waiters.register(2).expect("register succeeds");

    waiters.detach(1);
    let err = for_a
        .wait()
        .expect_err("detach must wake a pending scan for that handle");
    assert!(matches!(err, MemoryError::Backend { .. }));

    waiters.deliver(
        for_b.id(),
        vosu_plugin_api::ScanResult {
            addresses: vec![42],
            error: None,
        },
    );
    assert_eq!(
        for_b.wait().expect("undetached handle still delivers"),
        vec![42]
    );
}

#[test]
fn detach_releases_an_orphaned_slot_for_that_handle_too() {
    let waiters = ScanWaiters::with_timeout(Duration::from_millis(20));
    let slot = waiters.register(1).expect("register succeeds");
    let _ = slot.wait(); // times out, orphaning the slot rather than releasing it

    // Without detach, a fresh registration past the (now effectively
    // 47-permit) cap would still have to wait; detach must release it.
    waiters.detach(1);
    let mut held = Vec::new();
    for _ in 0..MAX_OUTSTANDING_SCANS {
        held.push(waiters.register(9).expect("register succeeds"));
    }
}

#[test]
fn shutdown_releases_every_pending_slot_regardless_of_handle() {
    let waiters = ScanWaiters::with_timeout(Duration::from_secs(30));
    let one = waiters.register(1).expect("register succeeds");
    let two = waiters.register(2).expect("register succeeds");
    waiters.shutdown();
    assert!(one.wait().is_err());
    assert!(two.wait().is_err());
}

#[test]
fn a_dropped_slot_whose_result_already_arrived_releases_immediately() {
    let waiters = ScanWaiters::new();
    let used = Arc::new(AtomicUsize::new(0));
    for _ in 0..MAX_OUTSTANDING_SCANS {
        let slot = waiters.register(1).expect("register succeeds");
        waiters.deliver(
            slot.id(),
            vosu_plugin_api::ScanResult {
                addresses: Vec::new(),
                error: None,
            },
        );
        used.fetch_add(1, Ordering::Relaxed);
        drop(slot); // delivered, never explicitly waited on; Drop must still release
    }
    // A fresh registration past the cap must not block, since every
    // earlier slot's result had already arrived before it dropped.
    let slot = waiters.register(1).expect("register succeeds");
    drop(slot);
    assert_eq!(used.load(Ordering::Relaxed), MAX_OUTSTANDING_SCANS);
}

// ---------------------------------------------------------------------
// PlanScanBackend over FakeHost.
// ---------------------------------------------------------------------

fn pattern_image(signatures: &[(&str, u8)], gap: usize, wildcard: u8) -> (Vec<u8>, Vec<Pattern>) {
    let mut image = Vec::new();
    let mut parsed = Vec::new();
    for (signature, _) in signatures {
        let pattern = Pattern::parse(signature, false).expect("valid test pattern");
        image.extend(
            pattern
                .bytes()
                .iter()
                .zip(pattern.mask())
                .map(|(byte, mask)| if *mask == 0 { wildcard } else { *byte }),
        );
        image.extend(std::iter::repeat_n(0, gap));
        parsed.push(pattern);
    }
    (image, parsed)
}

#[test]
fn scan_all_returns_every_match_in_ascending_order() {
    let base = 0x1000;
    let repeated = "AA BB ?? CC";
    // Two occurrences, separated by filler that cannot itself match.
    let mut bytes = Pattern::parse(repeated, false).unwrap().bytes().to_vec();
    bytes[2] = 0x11; // fill the wildcard byte with something concrete
    let mut image = bytes.clone();
    image.extend(std::iter::repeat_n(0u8, 10));
    image.extend(bytes);

    let waiters = ScanWaiters::new();
    let host = SharedFakeHost::new(FakeHost::new(
        deliverer(&waiters),
        image,
        base,
        Arc::new(AtomicBool::new(true)),
    ));
    let backend = PlanScanBackend::new(host, 1, waiters);
    let pattern = Pattern::parse(repeated, false).unwrap();
    let addresses = backend.scan_all(&pattern).expect("scan succeeds");
    assert_eq!(addresses, vec![base, base + 14]);
}

#[test]
fn scan_batch_returns_results_in_request_order_matching_the_single_region_walk() {
    let base = 0x2000;
    let (image, patterns) = pattern_image(
        &[("11 22 33 44", 0), ("55 66 77 88", 0), ("99 AA BB CC", 0)],
        8,
        0xEE,
    );
    let waiters = ScanWaiters::new();
    let host = SharedFakeHost::new(FakeHost::new(
        deliverer(&waiters),
        image,
        base,
        Arc::new(AtomicBool::new(true)),
    ));
    let backend = PlanScanBackend::new(host, 1, waiters);

    // `patterns[0]` matches at the lowest address, `patterns[2]` at the
    // highest. Request the higher-address one first, plus one that never
    // matches, to prove the result order comes from request order (the
    // single-region native scanner's own pattern-index-within-a-region
    // order, `vosu-sys/src/process/scanner.rs`), not from the matched
    // address — sorting by address would put `patterns[0]`'s match
    // first, which the native walk does not do.
    let missing = Pattern::parse("DE AD BE EF", false).unwrap();
    let requested = [patterns[2].clone(), missing, patterns[0].clone()];
    let results = backend.scan_batch(&requested).expect("batch scan succeeds");
    assert_eq!(results.len(), 2);
    assert_eq!(
        results[0].index, 0,
        "requested[0] (patterns[2], the higher address) must come first: request order, not address order"
    );
    assert_eq!(
        results[1].index, 2,
        "requested[2] (patterns[0]) comes second"
    );
    assert!(
        results[0].address > results[1].address,
        "confirms this is request order, not address order: the first result has the higher address"
    );
}

#[test]
fn a_60_pattern_batch_is_grouped_into_at_most_48_at_once_and_completes() {
    let base = 0x6000;
    let (image, patterns) = pattern_image(&[("AA BB CC DD", 0)], 4, 0xEE);
    let repeated: Vec<Pattern> = std::iter::repeat_n(patterns[0].clone(), 60).collect();

    let waiters = ScanWaiters::new();
    let host = SharedFakeHost::new(FakeHost::new(
        deliverer(&waiters),
        image,
        base,
        Arc::new(AtomicBool::new(true)),
    ));
    let backend = PlanScanBackend::new(host, 1, waiters);
    let results = backend
        .scan_batch(&repeated)
        .expect("a >48-pattern batch still succeeds via grouping");
    assert_eq!(results.len(), 60);
}

#[test]
fn four_concurrent_16_pattern_scan_batches_do_not_deadlock() {
    let base = 0x5000;
    let (image, patterns) = pattern_image(&[("AA BB CC DD", 0)], 4, 0xEE);
    let pattern16: Vec<Pattern> = std::iter::repeat_n(patterns[0].clone(), 16).collect();

    let waiters = ScanWaiters::new();
    let host = SharedFakeHost::new(FakeHost::new(
        deliverer(&waiters),
        image,
        base,
        Arc::new(AtomicBool::new(true)),
    ));

    let (tx, rx) = mpsc::sync_channel(4);
    for session in 0..4u64 {
        let backend = PlanScanBackend::new(host.clone(), session, Arc::clone(&waiters));
        let patterns = pattern16.clone();
        let tx = tx.clone();
        thread::spawn(move || {
            let outcome = backend.scan_batch(&patterns).map(|results| results.len());
            // `send` on an mpsc channel never blocks for a bounded
            // capacity (unlike `sync_channel`), so this cannot itself
            // deadlock regardless of how the receiver drains it.
            let _ = tx.send(outcome);
        });
    }
    drop(tx);

    for _ in 0..4 {
        let outcome = rx
            .recv_timeout(Duration::from_secs(10))
            .expect("every scan_batch call must finish, not deadlock forever (P3 review item 1)");
        outcome.expect("each 16-pattern batch succeeds");
    }
}

#[test]
fn scan_permission_denied_becomes_memory_error_permission_denied() {
    let base = 0x3000;
    let (image, patterns) = pattern_image(&[("11 22 33 44", 0)], 4, 0xEE);
    let waiters = ScanWaiters::new();
    let fake = FakeHost::new(
        deliverer(&waiters),
        image,
        base,
        Arc::new(AtomicBool::new(true)),
    );
    fake.deny_scan_with_permission_denied();
    let host = SharedFakeHost::new(fake);
    let backend = PlanScanBackend::new(host, 1, waiters);
    let err = backend
        .scan_all(&patterns[0])
        .expect_err("a permission-denied scan must not succeed");
    assert!(matches!(err, MemoryError::PermissionDenied { .. }));
}

#[test]
fn scan_submission_permission_denied_becomes_memory_error_permission_denied() {
    // The host refuses the VOSU_OP_SCAN_REQUEST submission itself
    // (VOSU_ERR_PERMISSION_DENIED, e.g. it could not open the process at
    // all), rather than answering with a "permission_denied" scan error
    // later: send_for must map this the same way (P2 review item 1).
    let base = 0x3100;
    let (image, patterns) = pattern_image(&[("11 22 33 44", 0)], 4, 0xEE);
    let waiters = ScanWaiters::new();
    let fake = FakeHost::new(
        deliverer(&waiters),
        image,
        base,
        Arc::new(AtomicBool::new(true)),
    );
    fake.deny_scan_submission_with_permission_denied();
    let host = SharedFakeHost::new(fake);
    let backend = PlanScanBackend::new(host, 1, waiters);
    let err = backend
        .scan_all(&patterns[0])
        .expect_err("a permission-denied scan submission must not succeed");
    assert!(matches!(err, MemoryError::PermissionDenied { .. }));
}

#[test]
fn scan_target_gone_flips_the_shared_alive_flag() {
    let base = 0x4000;
    let (image, patterns) = pattern_image(&[("11 22 33 44", 0)], 4, 0xEE);
    let host_alive = Arc::new(AtomicBool::new(true));
    let waiters = ScanWaiters::new();
    let fake = FakeHost::new(deliverer(&waiters), image, base, Arc::clone(&host_alive));
    let host = SharedFakeHost::new(fake);

    let memory = PlanProcessMemory::new(host.clone(), 1, 7, ProcessBitness::X64);
    let shared_alive = memory.alive_flag();
    let backend = PlanScanBackend::with_shared(host, 1, waiters, Arc::clone(&shared_alive));
    assert!(backend.is_alive() && memory.is_alive());

    // Simulate the target dying: the host now rejects every submission
    // with VOSU_ERR_TARGET_GONE.
    host_alive.store(false, Ordering::Relaxed);
    let err = backend
        .scan_all(&patterns[0])
        .expect_err("a scan against a gone target fails");
    assert!(matches!(err, MemoryError::Backend { .. }));
    assert!(!backend.is_alive());
    assert!(
        !memory.is_alive(),
        "the shared alive flag must reflect the scan path's own TARGET_GONE observation"
    );
}

/// Acceptance: `StablePatterns::resolve` over `ProcessHandle::with_scan_backend`
/// (`PlanProcessMemory` + `PlanScanBackend`) equals the region-walk result
/// for the committed `stable_patterns.json` fixture.
#[test]
fn stable_patterns_resolve_over_plan_backends_equals_the_region_walk() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../tests/fixtures/raw_memory/stable_patterns.json"
    ))
    .expect("fixture parses");
    let base = fixture["base_address"].as_u64().unwrap() as usize;
    let gap = fixture["gap_bytes"].as_u64().unwrap() as usize;
    let wildcard = fixture["wildcard_byte"].as_u64().unwrap() as u8;

    let mut image = Vec::new();
    for item in fixture["patterns"].as_array().unwrap() {
        let pattern = Pattern::parse(item["signature"].as_str().unwrap(), false).unwrap();
        image.extend(
            pattern
                .bytes()
                .iter()
                .zip(pattern.mask())
                .map(|(byte, mask)| if *mask == 0 { wildcard } else { *byte }),
        );
        image.extend(std::iter::repeat_n(0, gap));
    }

    let region_walk_handle = ProcessHandle::new(
        Arc::new(FlatMemory::new(image.clone(), base)),
        ProcessBitness::X86,
    );
    let expected = match StablePatterns::resolve(&region_walk_handle).expect("region walk resolves")
    {
        ReadStatus::Ready(value) => value,
        other => panic!("region walk did not resolve the fixture: {other:?}"),
    };

    let waiters = ScanWaiters::new();
    let host = SharedFakeHost::new(FakeHost::new(
        deliverer(&waiters),
        image,
        base,
        Arc::new(AtomicBool::new(true)),
    ));
    let plan_memory = PlanProcessMemory::new(host.clone(), 11, 2468, ProcessBitness::X86);
    let plan_scans = PlanScanBackend::new(host, 11, waiters);
    let plan_handle = ProcessHandle::with_scan_backend(
        Arc::new(plan_memory),
        ProcessBitness::X86,
        Arc::new(plan_scans),
    );
    let via_plans = match StablePatterns::resolve(&plan_handle).expect("plan-backed resolve") {
        ReadStatus::Ready(value) => value,
        other => panic!("plan-backed resolve did not resolve the fixture: {other:?}"),
    };

    assert_eq!(via_plans, expected);
}

/// A minimal `ProcessMemory` walking its readable regions directly
/// (`ScanBackend`-less region walk), the oracle every `PlanScanBackend`
/// test compares against.
struct FlatMemory {
    bytes: Vec<u8>,
    base: usize,
}

impl FlatMemory {
    fn new(bytes: Vec<u8>, base: usize) -> Self {
        Self { bytes, base }
    }
}

impl ProcessMemory for FlatMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let end = address
            .checked_add(dst.len())
            .ok_or(MemoryError::AddressOverflow {
                base: address,
                offset: dst.len(),
            })?;
        if address < self.base || end > self.base + self.bytes.len() {
            return Err(MemoryError::Read {
                address,
                len: dst.len(),
                message: "unmapped".to_owned(),
            });
        }
        dst.copy_from_slice(&self.bytes[address - self.base..end - self.base]);
        Ok(())
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads.iter_mut() {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<vosu_osu_reader::process::MemoryRegion>, MemoryError> {
        Ok(vec![vosu_osu_reader::process::MemoryRegion {
            address: self.base,
            size: self.bytes.len(),
        }])
    }
}
