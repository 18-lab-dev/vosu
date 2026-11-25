use std::sync::Arc;
use std::time::{Duration, Instant};

use vosu_plugin_api::{vosu_opcode, vosu_status};

use super::super::control;
use super::*;

fn waiters() -> (Waiters, Arc<Ledger>) {
    let ledger = Arc::new(Ledger::default());
    (Waiters::new(Arc::clone(&ledger)), ledger)
}

fn result_header() -> FrameHeader {
    FrameHeader {
        opcode: vosu_opcode::VOSU_OP_READ_PLAN_RESULT as u32,
        flags: 0,
        correlation: 0,
    }
}

#[test]
fn ids_strictly_increase_and_are_never_reused() {
    let (waiters, ledger) = waiters();
    let first = waiters
        .reserve(RequestKind::Status, Settle::Nothing)
        .unwrap();
    waiters.release(first);
    let second = waiters.reserve(RequestKind::Sync, Settle::Nothing).unwrap();
    assert_eq!((first, second), (1, 2));
    assert_eq!(ledger.kind(second), Some(RequestKind::Sync));
    assert_eq!(
        ledger.kind(first),
        None,
        "a release closes the ledger entry"
    );
}

/// A5.2: 64 waiters per instance, shared by status and SYNC requests; the
/// 65th is refused before anything is written.
#[test]
fn the_sixty_fifth_waiter_is_refused_and_a_reply_frees_one() {
    let (waiters, ledger) = waiters();
    let ids: Vec<u64> = (0..MAX_REQUESTS)
        .map(|index| {
            let kind = if index % 2 == 0 {
                RequestKind::Status
            } else {
                RequestKind::Sync
            };
            waiters.reserve(kind, Settle::Nothing).unwrap()
        })
        .collect();
    assert_eq!(ledger.len(), MAX_REQUESTS);
    assert_eq!(
        waiters.reserve(RequestKind::Status, Settle::CancelTimer(3)),
        Err(Settle::CancelTimer(3)),
        "the overload refusal hands the settle back"
    );
    assert!(waiters.complete_status(ids[0], vosu_status::VOSU_OK, |_, _| {}));
    assert!(
        waiters
            .reserve(RequestKind::Status, Settle::Nothing)
            .is_ok()
    );
    assert_eq!(
        waiters.reserved(),
        MAX_REQUESTS + 1,
        "the answered one awaits its thread"
    );
}

/// T1 counterexample: a late reply for a released id never satisfies a
/// newer waiter, and runs no settlement.
#[test]
fn a_late_reply_is_discarded_and_counted_never_delivered() {
    let (waiters, _ledger) = waiters();
    let old = waiters
        .reserve(RequestKind::Status, Settle::Nothing)
        .unwrap();
    waiters.release(old);
    let new = waiters
        .reserve(RequestKind::Status, Settle::Nothing)
        .unwrap();
    let mut settled = false;
    assert!(
        !waiters.complete_status(old, vosu_status::VOSU_ERR_CANCELLED, |_, _| {
            settled = true;
        })
    );
    assert!(!settled);
    let mut buffer = PayloadBuffer::new();
    buffer.fill(b"late result");
    assert!(!waiters.complete_result(old, result_header(), &mut buffer));
    assert_eq!(
        buffer.bytes(),
        b"late result",
        "a discarded result stays put"
    );
    // Never issued.
    assert!(!waiters.complete_status(999, vosu_status::VOSU_OK, |_, _| {}));
    assert_eq!(waiters.discarded(), 3);
    let deadline = Instant::now() + Duration::from_millis(20);
    assert!(
        matches!(
            waiters.wait(new, deadline),
            Err(control::EXIT_TRANSPORT_TIMEOUT)
        ),
        "the newer waiter saw none of the late replies"
    );
    // A second reply for an answered id is discarded too.
    assert!(waiters.complete_status(new, vosu_status::VOSU_OK, |_, _| {}));
    assert!(!waiters.complete_status(new, vosu_status::VOSU_OK, |_, _| {}));
}

#[test]
fn the_settle_runs_before_the_waiter_can_see_its_status() {
    let (waiters, _ledger) = waiters();
    let waiters = Arc::new(waiters);
    let id = waiters
        .reserve(RequestKind::Status, Settle::CancelPlan(4))
        .unwrap();
    let settled = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let waiting = std::thread::spawn({
        let waiters = Arc::clone(&waiters);
        let settled = Arc::clone(&settled);
        move || {
            let reply = waiters.wait(id, Instant::now() + Duration::from_secs(20));
            (reply, settled.load(std::sync::atomic::Ordering::SeqCst))
        }
    });
    assert!(
        waiters.complete_status(id, vosu_status::VOSU_OK, |settle, status| {
            assert_eq!(*settle, Settle::CancelPlan(4));
            assert_eq!(status, vosu_status::VOSU_OK);
            settled.store(true, std::sync::atomic::Ordering::SeqCst);
        })
    );
    let (reply, settled_first) = waiting.join().unwrap();
    assert!(matches!(reply, Ok(Reply::Status(vosu_status::VOSU_OK))));
    assert!(settled_first);
}

/// A SYNC result moves into the waiter without a copy; the reader keeps a
/// spare, and buffers are reused.
#[test]
fn a_sync_result_moves_its_buffer_to_the_waiter() {
    let (waiters, ledger) = waiters();
    let id = waiters.reserve(RequestKind::Sync, Settle::Nothing).unwrap();
    let mut buffer = PayloadBuffer::new();
    buffer.fill(&[7; 40]);
    let data = buffer.bytes().as_ptr();
    assert!(waiters.complete_result(id, result_header(), &mut buffer));
    assert!(buffer.bytes().is_empty(), "the reader got a spare");
    assert_eq!(ledger.kind(id), None, "the reply closed the request");
    match waiters.wait(id, Instant::now()).unwrap() {
        Reply::Result { header, payload } => {
            assert_eq!(header, result_header());
            assert_eq!(payload.bytes(), [7; 40]);
            assert_eq!(payload.bytes().as_ptr(), data, "moved, not copied");
            waiters.recycle(payload);
        }
        Reply::Status(status) => panic!("unexpected status {status:?}"),
    }
    let id = waiters.reserve(RequestKind::Sync, Settle::Nothing).unwrap();
    let mut buffer = PayloadBuffer::new();
    buffer.fill(&[1; 8]);
    assert!(waiters.complete_result(id, result_header(), &mut buffer));
    assert!(
        buffer.capacity_bytes() >= 40,
        "the recycled buffer became the reader's spare"
    );
}

#[test]
fn close_ends_every_wait_and_refuses_new_waiters() {
    let (waiters, _ledger) = waiters();
    let waiters = Arc::new(waiters);
    let id = waiters.reserve(RequestKind::Sync, Settle::Nothing).unwrap();
    let waiting = std::thread::spawn({
        let waiters = Arc::clone(&waiters);
        move || waiters.wait(id, Instant::now() + Duration::from_secs(20))
    });
    waiters.close();
    assert!(matches!(
        waiting.join().unwrap(),
        Err(control::EXIT_CHANNEL_LOST)
    ));
    assert!(
        waiters
            .reserve(RequestKind::Status, Settle::Nothing)
            .is_err()
    );
}
