use super::*;
use std::time::Instant;

#[derive(Clone)]
struct AcceptHost;
impl vosu_plugin_api::HostServices for AcceptHost {
    fn send(&self, _: &vosu_plugin_api::Message, _: u64) -> vosu_plugin_api::vosu_status {
        vosu_plugin_api::vosu_status::VOSU_OK
    }
}

#[test]
fn detached_session_rejects_a_preparation_thread_registering_after_detach() {
    let waiters = ScanWaiters::new();
    let alive = Arc::new(AtomicBool::new(true));
    let backend = PlanScanBackend::with_shared(AcceptHost, 61063, waiters.clone(), alive.clone());
    let (start_tx, start_rx) = mpsc::sync_channel(1);
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let task = thread::spawn(move || {
        start_rx.recv_timeout(Duration::from_secs(1)).unwrap();
        let patterns = vec![Pattern::parse("AA BB", false).unwrap(); 16];
        done_tx.send(backend.scan_batch(&patterns)).unwrap();
    });
    waiters.cancel_session(61063, &alive);
    start_tx.send(()).unwrap();
    let result = done_rx
        .recv_timeout(Duration::from_millis(500))
        .expect("cancelled admission must not wait for the 30-second scan timeout");
    assert!(matches!(result, Err(MemoryError::Backend { .. })));
    task.join().unwrap();
    assert!(!alive.load(Ordering::Acquire));
    assert!(waiters.inner.lock().unwrap().slots.is_empty());
    assert_eq!(waiters.outstanding.lock().unwrap().count, 0);
}

#[test]
fn cancelling_a_queued_session_wakes_admission_without_releasing_other_sessions_permits() {
    let waiters = ScanWaiters::new();
    let held = waiters.register_many(61064, MAX_OUTSTANDING_SCANS).unwrap();
    let alive = Arc::new(AtomicBool::new(true));
    let caller = waiters.clone();
    let caller_alive = alive.clone();
    let (done_tx, done_rx) = mpsc::sync_channel(1);
    let task = thread::spawn(move || {
        done_tx
            .send(caller.register_many_cancellable(61065, 16, Some(&caller_alive)))
            .unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    let mut admission = waiters.outstanding.lock().unwrap();
    while admission.waiting.is_empty() {
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero(), "scan registration never queued");
        admission = waiters
            .permit_freed
            .wait_timeout(admission, remaining)
            .unwrap()
            .0;
    }
    assert_eq!(admission.count, MAX_OUTSTANDING_SCANS);
    drop(admission);
    waiters.cancel_session(61065, &alive);
    let result = done_rx
        .recv_timeout(Duration::from_millis(500))
        .expect("queued cancellation must wake immediately");
    assert!(matches!(result, Err(MemoryError::Backend { .. })));
    task.join().unwrap();
    assert!(waiters.outstanding.lock().unwrap().waiting.is_empty());
    assert_eq!(
        waiters.outstanding.lock().unwrap().count,
        MAX_OUTSTANDING_SCANS
    );
    waiters.detach(61064);
    drop(held);
    assert!(waiters.inner.lock().unwrap().slots.is_empty());
    assert_eq!(waiters.outstanding.lock().unwrap().count, 0);
}

#[test]
fn cancelling_after_admission_releases_every_registered_slot() {
    let waiters = ScanWaiters::new();
    let alive = AtomicBool::new(true);
    let slots = waiters
        .register_many_cancellable(61066, 16, Some(&alive))
        .unwrap();
    waiters.cancel_session(61066, &alive);
    for slot in slots {
        assert!(matches!(slot.wait(), Err(MemoryError::Backend { .. })));
    }
    assert!(waiters.inner.lock().unwrap().slots.is_empty());
    assert_eq!(waiters.outstanding.lock().unwrap().count, 0);
}
