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
        slots.push(outstanding.begin().unwrap());
    }
    assert_eq!(outstanding.len(), MAX_OUTSTANDING_SCANS);
    assert_eq!(
        outstanding.begin().unwrap_err(),
        OutstandingScansError::CapExceeded
    );

    slots.pop();
    assert_eq!(outstanding.len(), MAX_OUTSTANDING_SCANS - 1);
    slots.push(outstanding.begin().unwrap());
    assert_eq!(outstanding.len(), MAX_OUTSTANDING_SCANS);
}

#[test]
fn worker_pool_runs_jobs_off_the_calling_thread() {
    let pool = ScanWorkerPool::new(2, 4);
    let (tx, rx) = mpsc::channel();
    let caller_thread = thread::current().id();
    pool.try_spawn(move || {
        tx.send(thread::current().id()).unwrap();
    })
    .unwrap();
    let worker_thread = rx.recv_timeout(Duration::from_secs(5)).unwrap();
    assert_ne!(worker_thread, caller_thread);
}

#[test]
fn worker_pool_refuses_beyond_its_bounded_queue() {
    // One worker, blocked on a gate; a queue of capacity 1 then means the
    // *second* try_spawn still fits the queue slot, and the third is
    // refused while the worker is still blocked on the first job.
    let pool = ScanWorkerPool::new(1, 1);
    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let gate_clone = Arc::clone(&gate);
    pool.try_spawn(move || {
        let (lock, cvar) = &*gate_clone;
        let mut released = lock.lock().unwrap();
        while !*released {
            released = cvar.wait(released).unwrap();
        }
    })
    .unwrap();

    // Give the worker a moment to pick up the blocking job so the queue is
    // actually empty before we fill it.
    std::thread::sleep(Duration::from_millis(50));
    pool.try_spawn(|| {}).unwrap();
    assert_eq!(pool.try_spawn(|| {}), Err(QueueFullError));

    let (lock, cvar) = &*gate;
    *lock.lock().unwrap() = true;
    cvar.notify_all();
}

#[test]
fn submit_runs_execute_off_the_caller_and_frees_its_slot() {
    let outstanding = OutstandingScans::new();
    let pool = ScanWorkerPool::new(1, 4);
    let memory: Arc<dyn ProcessMemory> = Arc::new(FakeMemory::new(vec![(0x1000, vec![0xaa])]));
    let modules: Arc<dyn ModuleRegions + Send + Sync> = Arc::new(NoModulesSync);
    let (tx, rx) = mpsc::channel();

    submit(
        &outstanding,
        &pool,
        request(None, "AA", false, false),
        memory,
        modules,
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
    assert_eq!(outstanding.len(), 0);
}

struct NoModulesSync;
impl ModuleRegions for NoModulesSync {
    fn resolve(&self, _module: &str) -> Option<ModuleRegion> {
        None
    }
}

#[test]
fn submit_refuses_once_the_cap_is_reached() {
    let outstanding = OutstandingScans::new();
    let _slots: Vec<_> = (0..MAX_OUTSTANDING_SCANS)
        .map(|_| outstanding.begin().unwrap())
        .collect();
    let pool = ScanWorkerPool::new(1, 4);
    let memory: Arc<dyn ProcessMemory> = Arc::new(FakeMemory::new(vec![]));
    let modules: Arc<dyn ModuleRegions + Send + Sync> = Arc::new(NoModulesSync);

    let error = submit(
        &outstanding,
        &pool,
        request(None, "AA", false, false),
        memory,
        modules,
        |_| {},
    )
    .unwrap_err();
    assert_eq!(error, ScanSubmitError::CapExceeded);
}
