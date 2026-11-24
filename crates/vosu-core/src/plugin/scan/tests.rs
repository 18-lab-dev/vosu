use std::sync::atomic::AtomicUsize;
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use super::*;

#[derive(Debug, Default)]
struct FakeMemory {
    regions: Vec<(usize, Vec<u8>)>,
    alive: bool,
}

impl FakeMemory {
    fn new(regions: Vec<(usize, Vec<u8>)>) -> Self {
        Self {
            regions,
            alive: true,
        }
    }
}

impl ProcessMemory for FakeMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        for (base, bytes) in &self.regions {
            let Some(offset) = address.checked_sub(*base) else {
                continue;
            };
            if offset <= bytes.len() && dst.len() <= bytes.len() - offset {
                dst.copy_from_slice(&bytes[offset..offset + dst.len()]);
                return Ok(());
            }
        }
        Err(MemoryError::Read {
            address,
            len: dst.len(),
            message: "unmapped".to_owned(),
        })
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(self
            .regions
            .iter()
            .map(|(address, bytes)| MemoryRegion {
                address: *address,
                size: bytes.len(),
            })
            .collect())
    }

    fn is_alive(&self) -> bool {
        self.alive
    }
}

/// `alive` flips to `false` once `go_dead_after` reads have happened, so a
/// test can simulate a target exiting mid-scan rather than before it.
#[derive(Debug)]
struct DyingMemory {
    region: (usize, Vec<u8>),
    reads_before_death: std::sync::atomic::AtomicUsize,
}

impl ProcessMemory for DyingMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let (base, bytes) = &self.region;
        self.reads_before_death
            .fetch_sub(1, std::sync::atomic::Ordering::Relaxed);
        let offset = address
            .checked_sub(*base)
            .ok_or_else(|| MemoryError::Read {
                address,
                len: dst.len(),
                message: "unmapped".to_owned(),
            })?;
        dst.copy_from_slice(&bytes[offset..offset + dst.len()]);
        Ok(())
    }

    fn read_many(&self, _reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        unreachable!()
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: self.region.0,
            size: self.region.1.len(),
        }])
    }

    fn is_alive(&self) -> bool {
        self.reads_before_death
            .load(std::sync::atomic::Ordering::Relaxed)
            != usize::MAX
    }
}

struct NoModules;

impl ModuleRegions for NoModules {
    fn resolve(&self, _module: &str) -> Option<ModuleRegion> {
        None
    }
}

struct OneModule(ModuleRegion);

impl ModuleRegions for OneModule {
    fn resolve(&self, module: &str) -> Option<ModuleRegion> {
        (module == "osu!.exe").then_some(self.0)
    }
}

fn request(
    module: Option<&str>,
    pattern: &str,
    non_zero_mask: bool,
    all_matches: bool,
) -> ScanRequest {
    ScanRequest {
        session: 1,
        module: module.map(str::to_owned),
        pattern: pattern.to_owned(),
        non_zero_mask,
        all_matches,
    }
}

#[test]
fn finds_first_match_when_all_matches_is_false() {
    let memory = FakeMemory::new(vec![(0x1000, vec![0xaa, 0xbb, 0xaa])]);
    let result = execute(&request(None, "AA", false, false), &memory, &NoModules);
    assert_eq!(
        result,
        ScanResult {
            addresses: vec![0x1000],
            error: None
        }
    );
}

#[test]
fn finds_every_match_when_all_matches_is_true() {
    let memory = FakeMemory::new(vec![(0x1000, vec![0xaa, 0xbb, 0xaa])]);
    let result = execute(&request(None, "AA", false, true), &memory, &NoModules);
    assert_eq!(
        result,
        ScanResult {
            addresses: vec![0x1000, 0x1002],
            error: None
        }
    );
}

#[test]
fn no_match_answers_empty_addresses_with_no_error() {
    let memory = FakeMemory::new(vec![(0x1000, vec![0, 0, 0])]);
    let result = execute(&request(None, "FF", false, true), &memory, &NoModules);
    assert_eq!(
        result,
        ScanResult {
            addresses: vec![],
            error: None
        }
    );
}

#[test]
fn invalid_pattern_token_answers_the_error_form() {
    let memory = FakeMemory::new(vec![(0x1000, vec![0])]);
    let result = execute(&request(None, "ZZ", false, false), &memory, &NoModules);
    assert!(result.addresses.is_empty());
    assert_eq!(result.error.unwrap().code, "invalid_pattern");
}

#[test]
fn unresolved_module_answers_module_not_found() {
    let memory = FakeMemory::new(vec![(0x1000, vec![0xaa])]);
    let result = execute(
        &request(Some("missing.exe"), "AA", false, false),
        &memory,
        &NoModules,
    );
    assert!(result.addresses.is_empty());
    assert_eq!(result.error.unwrap().code, "module_not_found");
}

#[test]
fn module_scoping_clips_regions_outside_the_module_window() {
    // Two regions: one entirely inside the module window, one entirely
    // outside it; only the inside match must be reported.
    let memory = FakeMemory::new(vec![(0x1000, vec![0xaa]), (0x9000, vec![0xaa])]);
    let modules = OneModule(ModuleRegion {
        base: 0x1000,
        size: 0x100,
    });
    let result = execute(
        &request(Some("osu!.exe"), "AA", false, true),
        &memory,
        &modules,
    );
    assert_eq!(
        result,
        ScanResult {
            addresses: vec![0x1000],
            error: None
        }
    );
}

#[test]
fn dead_target_answers_target_gone_without_touching_memory() {
    let memory = FakeMemory {
        regions: vec![(0x1000, vec![0xaa])],
        alive: false,
    };
    let result = execute(&request(None, "AA", false, false), &memory, &NoModules);
    assert!(result.addresses.is_empty());
    assert_eq!(result.error.unwrap().code, "target_gone");
}

#[test]
fn target_gone_mid_scan_is_reported_even_though_the_scan_itself_found_nothing() {
    // The scanner treats a read failure as "unreadable region, skip it",
    // so a target that dies partway through would otherwise answer
    // "found nothing" instead of the error form (§6.4).
    let memory = DyingMemory {
        region: (0x1000, vec![0; 3]),
        reads_before_death: std::sync::atomic::AtomicUsize::new(0),
    };
    let result = execute(&request(None, "00", false, true), &memory, &NoModules);
    assert!(result.addresses.is_empty());
    assert_eq!(result.error.unwrap().code, "target_gone");
}

#[test]
fn backend_read_errors_map_to_their_scan_error_code() {
    struct DeniedMemory;
    impl ProcessMemory for DeniedMemory {
        fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
            Err(MemoryError::PermissionDenied {
                address,
                len: dst.len(),
                message: "denied".to_owned(),
            })
        }
        fn read_many(&self, _reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
            unreachable!()
        }
        fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
            Ok(vec![MemoryRegion {
                address: 0x1000,
                size: 16,
            }])
        }
    }

    let result = execute(
        &request(None, "AA", false, false),
        &DeniedMemory,
        &NoModules,
    );
    assert!(result.addresses.is_empty());
    assert_eq!(result.error.unwrap().code, "permission_denied");
}

#[test]
fn outstanding_scans_enforces_the_cap() {
    let outstanding = OutstandingScans::new();
    let mut slots = Vec::new();
    for _ in 0..MAX_OUTSTANDING_SCANS {
        slots.push(outstanding.begin(1).unwrap());
    }
    assert_eq!(outstanding.len(), MAX_OUTSTANDING_SCANS);
    assert_eq!(
        outstanding.begin(1).unwrap_err(),
        OutstandingScansError::CapExceeded
    );

    slots.pop();
    assert_eq!(outstanding.len(), MAX_OUTSTANDING_SCANS - 1);
    slots.push(outstanding.begin(1).unwrap());
    assert_eq!(outstanding.len(), MAX_OUTSTANDING_SCANS);
}

#[test]
fn cancel_session_frees_only_that_sessions_slots() {
    let outstanding = OutstandingScans::new();
    let a = outstanding.begin(1).unwrap();
    let _b = outstanding.begin(2).unwrap();
    assert_eq!(outstanding.len(), 2);

    let _ = outstanding.cancel_session(1);
    assert_eq!(outstanding.len(), 1);

    // `a`'s slot is already gone; dropping it must not double-free (and so
    // must not panic or underflow the count).
    drop(a);
    assert_eq!(outstanding.len(), 1);
}

#[test]
fn worker_pool_runs_jobs_off_the_calling_thread() {
    let pool = ScanWorkerPool::new(2);
    let (tx, rx) = mpsc::channel();
    let caller_thread = thread::current().id();
    pool.submit(1, move || {
        tx.send(thread::current().id()).unwrap();
    });
    let worker_thread = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_ne!(worker_thread, caller_thread);
}

#[test]
fn one_keys_backlog_never_blocks_another_keys_job() {
    // P2-7: a single worker, so key 1 genuinely contends for the only
    // thread. Key 1 then floods its own sub-queue well past any size a
    // shared, fixed-capacity queue would have refused beyond — key 2's
    // own job must still queue (never refused) and run promptly once the
    // worker reaches key 2's turn (round-robin across keys), long before
    // key 1's own 200-deep backlog has drained, rather than waiting
    // behind the whole of it the way a single shared FIFO queue would
    // (F6: the previous version of this test only checked that key 2's
    // job ran at all, which a FIFO pool satisfies too, just last).
    let pool = ScanWorkerPool::new(1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let gate_clone = Arc::clone(&gate);
    let (started_tx, started_rx) = mpsc::channel();
    pool.submit(1, move || {
        started_tx.send(()).unwrap();
        let (lock, cvar) = &*gate_clone;
        let released = lock.lock().unwrap();
        let (released, _) = cvar
            .wait_timeout_while(released, Duration::from_secs(10), |released| !*released)
            .unwrap();
        assert!(*released, "scan worker gate was not released within 10s");
    });
    // Wait for the blocking job to actually start (and so leave key 1's
    // own sub-queue empty) before this floods it, rather than guessing
    // with a sleep (N4).
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the blocking job must start");

    let key1_completed = Arc::new(AtomicUsize::new(0));
    for _ in 0..200 {
        let key1_completed = Arc::clone(&key1_completed);
        pool.submit(1, move || {
            key1_completed.fetch_add(1, Ordering::SeqCst);
        });
    }

    let (tx, rx) = mpsc::channel();
    let key1_completed_at_key2 = Arc::clone(&key1_completed);
    pool.submit(2, move || {
        tx.send(key1_completed_at_key2.load(Ordering::SeqCst))
            .unwrap();
    });

    let (lock, cvar) = &*gate;
    *lock.lock().unwrap() = true;
    cvar.notify_all();

    let completed_when_key2_ran = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("key 2's job must run promptly, not behind key 1's whole backlog");
    assert!(
        completed_when_key2_ran < 200,
        "key 2's job only ran once all 200 of key 1's backlog had already drained (got \
         {completed_when_key2_ran}) — a FIFO pool shared across keys would serialize exactly \
         this way, instead of each key getting its own sub-queue"
    );
}

/// F1: cancelling a still-queued job must remove it from its key's own
/// sub-queue at once, not merely skip it when a worker eventually
/// reaches it — otherwise a plugin that loops submit-then-cancel against
/// its own already-admitted cap, while this key's one worker stays busy
/// on its own other, already-running job, grows the queue without bound
/// (the second-pass review's own finding). 10,000 iterations against a
/// single busy worker prove the queue never grows past empty.
#[test]
fn cancelled_jobs_never_pile_up_in_their_keys_queue() {
    let pool = ScanWorkerPool::new(1);
    let key = 1u64;
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let gate_clone = Arc::clone(&gate);
    let (started_tx, started_rx) = mpsc::channel();
    pool.submit(key, move || {
        started_tx.send(()).unwrap();
        let (lock, cvar) = &*gate_clone;
        let released = lock.lock().unwrap();
        let (released, _) = cvar
            .wait_timeout_while(released, Duration::from_secs(10), |released| !*released)
            .unwrap();
        assert!(*released, "scan worker gate was not released within 10s");
    });
    // N4: wait for the blocking job to actually start (and so leave
    // `key`'s own sub-queue empty) before this loop runs — a fixed sleep
    // here could still race it on a loaded machine, making iteration 0
    // see `queue_len == 1` (the blocker itself) and fail outright rather
    // than merely slow down.
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the blocking job must start");

    for i in 0..10_000u64 {
        let ticket = pool.submit(key, || {});
        let removed = pool.cancel(key, ticket);
        assert!(
            removed,
            "iteration {i}: a just-submitted job must still be queued to cancel, since the \
             worker is still blocked on the earlier job"
        );
        assert_eq!(
            pool.queue_len_for_tests(key),
            0,
            "iteration {i}: a cancelled job must never be left behind in its key's own \
             sub-queue"
        );
    }

    let (lock, cvar) = &*gate;
    *lock.lock().unwrap() = true;
    cvar.notify_all();
}

struct NoModulesSync;
impl ModuleRegions for NoModulesSync {
    fn resolve(&self, _module: &str) -> Option<ModuleRegion> {
        None
    }
}

#[test]
fn submit_runs_execute_off_the_caller_and_frees_its_slot() {
    let outstanding = OutstandingScans::new();
    let pool = ScanWorkerPool::new(1);
    let memory: Arc<dyn ProcessMemory> = Arc::new(FakeMemory::new(vec![(0x1000, vec![0xaa])]));
    let modules: Arc<dyn ModuleRegions + Send + Sync> = Arc::new(NoModulesSync);
    let (tx, rx) = mpsc::channel();

    submit(
        &outstanding,
        &pool,
        request(None, "AA", false, false),
        memory,
        modules,
        ScanRouting {
            session: 1,
            queue_key: 1,
        },
        move |result| {
            tx.send(result).unwrap();
        },
    )
    .unwrap();

    let result = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(
        result,
        ScanResult {
            addresses: vec![0x1000],
            error: None
        }
    );
    // §6.4/P3: `deliver` runs before the slot is dropped, so the slot may
    // still be briefly held for a moment after the result is observed;
    // poll rather than assert immediately.
    let freed = (0..100).any(|_| {
        if outstanding.is_empty() {
            true
        } else {
            std::thread::sleep(Duration::from_millis(10));
            false
        }
    });
    assert!(
        freed,
        "scan slot was never freed after its result was delivered"
    );
    assert_eq!(outstanding.len(), 0);
}

#[test]
fn submit_refuses_once_the_cap_is_reached() {
    let outstanding = OutstandingScans::new();
    let _slots: Vec<_> = (0..MAX_OUTSTANDING_SCANS)
        .map(|_| outstanding.begin(1).unwrap())
        .collect();
    let pool = ScanWorkerPool::new(1);
    let memory: Arc<dyn ProcessMemory> = Arc::new(FakeMemory::new(vec![]));
    let modules: Arc<dyn ModuleRegions + Send + Sync> = Arc::new(NoModulesSync);

    let error = submit(
        &outstanding,
        &pool,
        request(None, "AA", false, false),
        memory,
        modules,
        ScanRouting {
            session: 1,
            queue_key: 1,
        },
        |_| {},
    )
    .unwrap_err();
    assert_eq!(error, ScanSubmitError::CapExceeded);
}

#[test]
fn submit_refuses_a_malformed_pattern_before_taking_a_slot_or_a_worker() {
    let outstanding = OutstandingScans::new();
    let pool = ScanWorkerPool::new(1);
    let memory: Arc<dyn ProcessMemory> = Arc::new(FakeMemory::new(vec![]));
    let modules: Arc<dyn ModuleRegions + Send + Sync> = Arc::new(NoModulesSync);

    let error = submit(
        &outstanding,
        &pool,
        request(None, "ZZ", false, false),
        memory,
        modules,
        ScanRouting {
            session: 1,
            queue_key: 1,
        },
        |_| panic!("a malformed pattern must never reach a worker"),
    )
    .unwrap_err();
    assert_eq!(error, ScanSubmitError::InvalidPattern);
    assert_eq!(outstanding.len(), 0);
}

#[test]
fn cancel_session_frees_the_slot_and_suppresses_the_workers_own_result() {
    let outstanding = OutstandingScans::new();
    let pool = ScanWorkerPool::new(1);
    // A worker gate the test controls, so the scan only finishes once the
    // test has already called `cancel_session`.
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release_clone = Arc::clone(&release);
    pool.submit(1, move || {
        let (lock, cvar) = &*release_clone;
        let released = lock.lock().unwrap();
        let (released, _) = cvar
            .wait_timeout_while(released, Duration::from_secs(10), |released| !*released)
            .unwrap();
        assert!(*released, "scan worker gate was not released within 10s");
    });
    std::thread::sleep(Duration::from_millis(50));

    let memory: Arc<dyn ProcessMemory> = Arc::new(FakeMemory::new(vec![(0x1000, vec![0xaa])]));
    let modules: Arc<dyn ModuleRegions + Send + Sync> = Arc::new(NoModulesSync);
    let (tx, rx) = mpsc::channel();
    submit(
        &outstanding,
        &pool,
        request(None, "AA", false, false),
        memory,
        modules,
        ScanRouting {
            session: 7,
            queue_key: 1,
        },
        move |result| tx.send(result).unwrap(),
    )
    .unwrap();

    // The scan job is queued behind the blocked job above (same key, so
    // the same sub-queue), so it has not run yet: cancelling now must
    // win the race.
    let _ = outstanding.cancel_session(7);
    assert_eq!(outstanding.len(), 0);

    let (lock, cvar) = &*release;
    *lock.lock().unwrap() = true;
    cvar.notify_all();

    let result = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(result.error.unwrap().code, "session_detached");
}

#[test]
fn cancel_session_racing_admission_still_answers_exactly_once() {
    // F6: `begin_internal` registers this scan's gate, then (via the
    // deterministic race hook, standing in for a `cancel_session` call
    // from another thread landing in that exact window) the session
    // detaches and delivers the session-detached form before this scan's
    // own job ever runs on the pool. `submit` must not also report an
    // error for a scan whose result the plugin already received.
    let outstanding = OutstandingScans::new();
    let pool = ScanWorkerPool::new(1);

    // One worker, blocked, so this test's own job provably has not run
    // by the time the race hook fires below.
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release_clone = Arc::clone(&release);
    pool.submit(1, move || {
        let (lock, cvar) = &*release_clone;
        let released = lock.lock().unwrap();
        let (released, _) = cvar
            .wait_timeout_while(released, Duration::from_secs(10), |released| !*released)
            .unwrap();
        assert!(*released, "scan worker gate was not released within 10s");
    });
    std::thread::sleep(Duration::from_millis(50));

    let handle = 11;
    let outstanding_for_hook = Arc::clone(&outstanding);
    set_race_hook_for_tests(move || {
        let _ = outstanding_for_hook.cancel_session(handle);
    });

    let memory: Arc<dyn ProcessMemory> = Arc::new(FakeMemory::new(vec![(0x1000, vec![0xaa])]));
    let modules: Arc<dyn ModuleRegions + Send + Sync> = Arc::new(NoModulesSync);
    let (tx, rx) = mpsc::channel();
    let outcome = submit(
        &outstanding,
        &pool,
        request(None, "AA", false, false),
        memory,
        modules,
        ScanRouting {
            session: handle,
            queue_key: 1,
        },
        move |result| tx.send(result).unwrap(),
    );

    assert_eq!(
        outcome,
        Ok(()),
        "a session already detached mid-admission must not also surface a submit error"
    );
    let result = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(result.error.unwrap().code, "session_detached");
    assert_eq!(outstanding.len(), 0);

    let (lock, cvar) = &*release;
    *lock.lock().unwrap() = true;
    cvar.notify_all();
}

/// N3: the race above also used to leave this scan's own job sitting in
/// the pool's queue until a worker happened to reach it and found its
/// gate already taken — `set_queue_ticket` reporting the race lets
/// `submit` remove it immediately instead, the same leak F1 closed for
/// `READ_PLAN_CANCEL` applied to this narrower, detach-only window.
#[test]
fn cancel_session_racing_admission_also_removes_the_queued_job() {
    let outstanding = OutstandingScans::new();
    let pool = ScanWorkerPool::new(1);

    // One worker, blocked, so this test's own job provably has not run
    // by the time the race hook fires below — waited for through a
    // started signal rather than a fixed sleep, so a loaded machine
    // cannot make this test flake.
    let (started_tx, started_rx) = mpsc::channel();
    let release = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let release_clone = Arc::clone(&release);
    pool.submit(1, move || {
        started_tx.send(()).unwrap();
        let (lock, cvar) = &*release_clone;
        let released = lock.lock().unwrap();
        let (released, _) = cvar
            .wait_timeout_while(released, Duration::from_secs(10), |released| !*released)
            .unwrap();
        assert!(*released, "scan worker gate was not released within 10s");
    });
    started_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the blocking job must start");

    let handle = 12;
    let outstanding_for_hook = Arc::clone(&outstanding);
    set_race_hook_for_tests(move || {
        let _ = outstanding_for_hook.cancel_session(handle);
    });

    let memory: Arc<dyn ProcessMemory> = Arc::new(FakeMemory::new(vec![(0x1000, vec![0xaa])]));
    let modules: Arc<dyn ModuleRegions + Send + Sync> = Arc::new(NoModulesSync);
    let (tx, rx) = mpsc::channel();
    submit(
        &outstanding,
        &pool,
        request(None, "AA", false, false),
        memory,
        modules,
        ScanRouting {
            session: handle,
            queue_key: 1,
        },
        move |result| tx.send(result).unwrap(),
    )
    .unwrap();
    let result = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_eq!(result.error.unwrap().code, "session_detached");

    assert_eq!(
        pool.queue_len_for_tests(1),
        0,
        "N3: the cancelled scan's own job must be removed from the pool immediately, not left \
         to sit there until the blocked worker eventually reaches it"
    );

    let (lock, cvar) = &*release;
    *lock.lock().unwrap() = true;
    cvar.notify_all();
}
