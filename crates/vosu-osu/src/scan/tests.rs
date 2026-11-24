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

// ---------------------------------------------------------------------
// ScanWaiters: bounded capacity, semaphore, timeout, detach, shutdown.
// ---------------------------------------------------------------------

#[test]
fn register_and_deliver_round_trips_addresses() {
    let waiters = ScanWaiters::new();
    let slot = waiters.register(1);
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
    let slot = waiters.register(1);
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
fn an_undelivered_slot_times_out_and_releases_its_permit() {
    let waiters = ScanWaiters::with_timeout(Duration::from_millis(50));
    let slot = waiters.register(1);
    let err = slot.wait().expect_err("nothing was ever delivered");
    assert!(matches!(err, MemoryError::Backend { .. }));

    // The timed-out slot's permit must be back: registering
    // `MAX_OUTSTANDING_SCANS` more slots must not block.
    let (tx, rx) = mpsc::sync_channel(1);
    let waiters_for_thread = Arc::clone(&waiters);
    thread::spawn(move || {
        let mut slots = Vec::new();
        for _ in 0..MAX_OUTSTANDING_SCANS {
            slots.push(waiters_for_thread.register(2));
        }
        tx.send(slots.len()).unwrap();
    });
    assert_eq!(
        rx.recv_timeout(Duration::from_secs(5))
            .expect("registration did not block"),
        MAX_OUTSTANDING_SCANS
    );
}

#[test]
fn the_semaphore_blocks_the_49th_registration_until_one_releases() {
    let waiters = ScanWaiters::new();
    let mut held: Vec<super::ScanSlot> = (0..MAX_OUTSTANDING_SCANS)
        .map(|_| waiters.register(1))
        .collect();

    let (started, start_rx) = mpsc::sync_channel(1);
    let (done, done_rx) = mpsc::sync_channel(1);
    let waiters_for_thread = Arc::clone(&waiters);
    thread::spawn(move || {
        started.send(()).unwrap();
        let slot = waiters_for_thread.register(1);
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

    held.pop(); // release one permit
    let id = done_rx
        .recv_timeout(Duration::from_secs(5))
        .expect("registration unblocks once a permit frees up");
    assert!(id >= 1);
}

#[test]
fn detach_releases_only_the_matching_handles_pending_slots() {
    let waiters = ScanWaiters::with_timeout(Duration::from_secs(30));
    let for_a = waiters.register(1);
    let for_b = waiters.register(2);

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
fn shutdown_releases_every_pending_slot_regardless_of_handle() {
    let waiters = ScanWaiters::with_timeout(Duration::from_secs(30));
    let one = waiters.register(1);
    let two = waiters.register(2);
    waiters.shutdown();
    assert!(one.wait().is_err());
    assert!(two.wait().is_err());
}

#[test]
fn a_dropped_slot_releases_its_permit_without_waiting() {
    let waiters = ScanWaiters::new();
    let used = Arc::new(AtomicUsize::new(0));
    for _ in 0..MAX_OUTSTANDING_SCANS {
        let slot = waiters.register(1);
        used.fetch_add(1, Ordering::Relaxed);
        drop(slot); // never waited on; Drop alone must release
    }
    // A fresh registration past the cap must not block, since every
    // earlier slot released on drop.
    let slot = waiters.register(1);
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
        {
            let waiters = Arc::clone(&waiters);
            move |id, result| waiters.deliver(id, result)
        },
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
fn scan_batch_reports_results_by_pattern_index_skipping_unmatched_patterns() {
    let base = 0x2000;
    let (image, patterns) = pattern_image(
        &[("11 22 33 44", 0), ("55 66 77 88", 0), ("99 AA BB CC", 0)],
        8,
        0xEE,
    );
    let waiters = ScanWaiters::new();
    let host = SharedFakeHost::new(FakeHost::new(
        {
            let waiters = Arc::clone(&waiters);
            move |id, result| waiters.deliver(id, result)
        },
        image,
        base,
        Arc::new(AtomicBool::new(true)),
    ));
    let backend = PlanScanBackend::new(host, 1, waiters);

    // Only patterns 0 and 2 exist in the image; pattern 1 is replaced by
    // one that never occurs, so `scan_batch` must omit index 1 entirely.
    let missing = Pattern::parse("DE AD BE EF", false).unwrap();
    let requested = [patterns[0].clone(), missing, patterns[2].clone()];
    let mut results = backend.scan_batch(&requested).expect("batch scan succeeds");
    results.sort_by_key(|result| result.index);
    assert_eq!(results.len(), 2);
    assert_eq!(results[0].index, 0);
    assert_eq!(results[1].index, 2);
    assert!(results[0].address < results[1].address);
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
        {
            let waiters = Arc::clone(&waiters);
            move |id, result| waiters.deliver(id, result)
        },
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
