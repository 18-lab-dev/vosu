use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use vosu_osu_reader::process::{MemoryError, ProcessBitness, ProcessMemory, ReadRequest};

use super::{MAX_SINGLE_OP_BYTES, PlanProcessMemory};
use crate::test_support::FakeHost;

/// Deterministic, non-repeating fill so a misassembled chunk boundary
/// (off-by-one, swapped order, duplicated bytes) shows up as a mismatch
/// rather than accidentally reading as correct.
fn fill(len: usize, seed: u8) -> Vec<u8> {
    (0..len).map(|i| seed.wrapping_add(i as u8)).collect()
}

fn memory(bytes: Vec<u8>, base: usize) -> PlanProcessMemory<FakeHost> {
    let host = FakeHost::new(|_, _| {}, bytes, base, Arc::new(AtomicBool::new(true)));
    PlanProcessMemory::new(host, 1, 4242, ProcessBitness::X64)
}

#[test]
fn read_exact_reads_a_small_region_in_one_submission() {
    let base = 0x1000;
    let bytes = fill(64, 5);
    let process = memory(bytes.clone(), base);
    let mut out = vec![0u8; 64];
    process
        .read_exact(base, &mut out)
        .expect("in-range read succeeds");
    assert_eq!(out, bytes);
}

#[test]
fn read_exact_splits_a_read_longer_than_the_single_op_cap() {
    let base = 0x1000;
    // Just over one op's cap, forcing exactly two round trips: one of
    // `MAX_SINGLE_OP_BYTES` bytes, one of a single remaining byte.
    let total = MAX_SINGLE_OP_BYTES + 1;
    let bytes = fill(total, 7);
    let process = memory(bytes.clone(), base);
    let mut out = vec![0u8; total];
    process
        .read_exact(base, &mut out)
        .expect("split read succeeds");
    assert_eq!(
        out, bytes,
        "every byte across both chunks must land in order"
    );
    assert_eq!(
        process.host.submission_count(),
        2,
        "a read over the single-op cap must take exactly two round trips, not one and not three"
    );
}

#[test]
fn read_exact_exactly_at_the_cap_takes_one_submission() {
    let base = 0x1000;
    let bytes = fill(MAX_SINGLE_OP_BYTES, 9);
    let process = memory(bytes.clone(), base);
    let mut out = vec![0u8; MAX_SINGLE_OP_BYTES];
    process
        .read_exact(base, &mut out)
        .expect("boundary read succeeds");
    assert_eq!(out, bytes);
    assert_eq!(process.host.submission_count(), 1);
}

#[test]
fn read_many_batches_requests_that_fit_one_plan_into_a_single_submission() {
    let base = 0x2000;
    let bytes = fill(300, 1);
    let process = memory(bytes.clone(), base);
    let mut a = [0u8; 100];
    let mut b = [0u8; 100];
    let mut c = [0u8; 100];
    process
        .read_many(&mut [
            ReadRequest::new(base, &mut a),
            ReadRequest::new(base + 100, &mut b),
            ReadRequest::new(base + 200, &mut c),
        ])
        .expect("batched read succeeds");
    assert_eq!(a, bytes[0..100]);
    assert_eq!(b, bytes[100..200]);
    assert_eq!(c, bytes[200..300]);
    assert_eq!(
        process.host.submission_count(),
        1,
        "three small requests must share one round trip"
    );
}

#[test]
fn read_many_splits_into_a_second_submission_once_the_result_cap_is_exceeded() {
    let base = 0x3000;
    // Three equal requests sized so the first two fit one plan's 64 KiB
    // result cap but all three together do not, forcing the third into
    // its own round trip.
    let each = MAX_SINGLE_OP_BYTES / 3;
    let bytes = fill(each * 3, 3);
    let process = memory(bytes.clone(), base);
    let mut a = vec![0u8; each];
    let mut b = vec![0u8; each];
    let mut c = vec![0u8; each];
    process
        .read_many(&mut [
            ReadRequest::new(base, &mut a),
            ReadRequest::new(base + each, &mut b),
            ReadRequest::new(base + 2 * each, &mut c),
        ])
        .expect("chunked read succeeds");
    assert_eq!(a, bytes[0..each]);
    assert_eq!(b, bytes[each..2 * each]);
    assert_eq!(c, bytes[2 * each..3 * each]);
    assert_eq!(
        process.host.submission_count(),
        2,
        "two requests fit the first plan, the third needs a second round trip"
    );
}

#[test]
fn read_many_falls_back_to_chunked_read_exact_for_one_oversized_request() {
    let base = 0x4000;
    let total = MAX_SINGLE_OP_BYTES + 500;
    let bytes = fill(total, 11);
    let process = memory(bytes.clone(), base);
    let mut out = vec![0u8; total];
    process
        .read_many(&mut [ReadRequest::new(base, &mut out)])
        .expect("oversized single request still succeeds");
    assert_eq!(out, bytes);
    assert_eq!(process.host.submission_count(), 2);
}

#[test]
fn an_unmapped_address_is_a_read_error_not_a_panic() {
    let base = 0x5000;
    let process = memory(fill(16, 1), base);
    let mut out = [0u8; 16];
    let err = process
        .read_exact(base + 1000, &mut out)
        .expect_err("unmapped read fails");
    assert!(matches!(err, MemoryError::Read { .. }));
}

#[test]
fn per_op_permission_denied_becomes_memory_error_permission_denied() {
    let base = 0x5500;
    let host = FakeHost::new(
        |_, _| {},
        fill(16, 1),
        base,
        Arc::new(AtomicBool::new(true)),
    );
    host.deny_read_permission_at(base);
    let process = PlanProcessMemory::new(host, 1, 4242, ProcessBitness::X64);
    let mut out = [0u8; 16];
    let err = process
        .read_exact(base, &mut out)
        .expect_err("a permission-denied op must not succeed");
    assert!(matches!(err, MemoryError::PermissionDenied { .. }));
}

#[test]
fn submission_level_permission_denied_becomes_memory_error_permission_denied() {
    let base = 0x5600;
    let host = FakeHost::new(
        |_, _| {},
        fill(16, 1),
        base,
        Arc::new(AtomicBool::new(true)),
    );
    host.deny_submission_with_permission_denied();
    let process = PlanProcessMemory::new(host, 1, 4242, ProcessBitness::X64);
    let mut out = [0u8; 16];
    let err = process
        .read_exact(base, &mut out)
        .expect_err("a permission-denied submission must not succeed");
    assert!(matches!(err, MemoryError::PermissionDenied { .. }));
}

#[test]
fn target_gone_flips_is_alive() {
    let base = 0x6000;
    let alive = Arc::new(AtomicBool::new(true));
    let host = FakeHost::new(|_, _| {}, fill(16, 2), base, Arc::clone(&alive));
    let process = PlanProcessMemory::new(host, 1, 99, ProcessBitness::X64);
    assert!(process.is_alive());

    let mut out = [0u8; 16];
    process
        .read_exact(base, &mut out)
        .expect("first read succeeds while alive");
    assert!(process.is_alive());

    alive.store(false, std::sync::atomic::Ordering::Relaxed);
    let err = process
        .read_exact(base, &mut out)
        .expect_err("a read against a gone target fails");
    assert!(matches!(err, MemoryError::Backend { .. }));
    assert!(
        !process.is_alive(),
        "VOSU_ERR_TARGET_GONE on any submission must flip is_alive false"
    );
}

#[test]
fn shared_liveness_flag_is_observed_by_a_second_handle() {
    let base = 0x7000;
    let alive = Arc::new(AtomicBool::new(true));
    let host = FakeHost::new(|_, _| {}, fill(16, 4), base, Arc::clone(&alive));
    let primary = PlanProcessMemory::new(host, 1, 7, ProcessBitness::X64);
    let shared_alive = primary.alive_flag();
    let shared_counter = primary.plan_counter();

    let host2 = FakeHost::new(|_, _| {}, fill(16, 4), base, Arc::clone(&alive));
    let secondary = PlanProcessMemory::with_shared(
        host2,
        1,
        7,
        ProcessBitness::X64,
        shared_counter,
        shared_alive,
    );

    assert!(primary.is_alive() && secondary.is_alive());
    alive.store(false, std::sync::atomic::Ordering::Relaxed);
    // The FakeHost's own simulated death (`alive`) only reaches
    // `PlanProcessMemory`'s tracked liveness once a submission actually
    // observes `VOSU_ERR_TARGET_GONE`; `secondary` submits here, and
    // because it shares its `alive` flag with `primary` (constructed via
    // `with_shared`), `primary.is_alive()` must flip too even though
    // `primary` itself never submitted anything after the target died.
    let mut out = [0u8; 16];
    secondary
        .read_exact(base, &mut out)
        .expect_err("a read against the now-gone target fails");
    assert!(!secondary.is_alive());
    assert!(
        !primary.is_alive(),
        "the shared alive flag must reflect secondary's own TARGET_GONE observation"
    );
}

#[test]
fn readable_regions_is_unsupported_through_read_plans() {
    let process = memory(fill(16, 1), 0x8000);
    assert!(matches!(
        process.readable_regions(),
        Err(MemoryError::Backend { .. })
    ));
}

#[test]
fn dependent_chains_use_one_submission_and_target_pointer_width() {
    for bitness in [ProcessBitness::X86, ProcessBitness::X64] {
        let mut bytes = vec![0; 128];
        bytes[..bitness.pointer_size()]
            .copy_from_slice(&32u64.to_le_bytes()[..bitness.pointer_size()]);
        bytes[36..36 + bitness.pointer_size()]
            .copy_from_slice(&64u64.to_le_bytes()[..bitness.pointer_size()]);
        bytes[72..76].copy_from_slice(&0x12345678u32.to_le_bytes());
        let host = FakeHost::new(|_, _| {}, bytes, 0, Arc::new(AtomicBool::new(true)));
        let process = PlanProcessMemory::new(host, 1, 4242, bitness);
        let mut dst = [0; 4];
        process.read_chain(0, &[4, 8], bitness, &mut dst).unwrap();
        assert_eq!(u32::from_le_bytes(dst), 0x12345678);
        assert_eq!(process.host.submission_count(), 1);
    }
}

#[test]
fn dependent_chains_read_null_and_preserve_destination_on_pointer_failure() {
    let process = memory(vec![0; 16], 0);
    let mut dst = [7; 4];
    process
        .read_chain(0, &[0], ProcessBitness::X64, &mut dst)
        .unwrap();
    assert_eq!(dst, [0; 4], "a zero pointer must still read address zero");
    dst = [7; 4];
    let error = process
        .read_chain(100, &[0], ProcessBitness::X64, &mut dst)
        .unwrap_err();
    assert!(matches!(error, MemoryError::Read { address: 100, .. }));
    assert_eq!(dst, [7; 4]);
}

#[test]
fn dependent_chains_report_the_resolved_failure_address() {
    let mut bytes = vec![0; 16];
    bytes[..8].copy_from_slice(&128u64.to_le_bytes());
    let process = memory(bytes, 0);
    let mut dst = [7; 4];
    let error = process
        .read_chain(0, &[4], ProcessBitness::X64, &mut dst)
        .unwrap_err();
    assert!(matches!(error, MemoryError::Read { address: 132, .. }));
    assert_eq!(dst, [7; 4]);
}

#[test]
fn dependent_chain_overflow_and_large_offset_keep_sequential_error_precedence() {
    let mut bytes = vec![0; 16];
    bytes[..8].copy_from_slice(&u64::MAX.to_le_bytes());
    let process = memory(bytes, 0);
    let mut dst = [7; 4];
    assert_eq!(
        process.read_chain(0, &[1], ProcessBitness::X64, &mut dst),
        Err(MemoryError::AddressOverflow {
            base: usize::MAX,
            offset: 1
        })
    );
    assert_eq!(dst, [7; 4]);
    let error = process
        .read_chain(100, &[usize::MAX], ProcessBitness::X64, &mut dst)
        .unwrap_err();
    assert!(matches!(error, MemoryError::Read { address: 100, .. }));
    assert_eq!(dst, [7; 4]);
}

#[test]
fn indirect_many_fuses_holders_and_pointees_without_publishing_on_holder_error() {
    use vosu_osu_reader::process::IndirectReadRequest;
    let mut bytes = vec![0; 96];
    bytes[..8].copy_from_slice(&32u64.to_le_bytes());
    bytes[8..16].copy_from_slice(&64u64.to_le_bytes());
    bytes[36..40].copy_from_slice(&123u32.to_le_bytes());
    bytes[72..76].copy_from_slice(&456u32.to_le_bytes());
    let process = memory(bytes, 0);
    let (mut a, mut b) = ([7; 4], [7; 4]);
    process
        .read_indirect_many(
            &mut [
                IndirectReadRequest::new(0, 4, &mut a),
                IndirectReadRequest::new(8, 8, &mut b),
            ],
            ProcessBitness::X64,
        )
        .unwrap();
    assert_eq!(u32::from_le_bytes(a), 123);
    assert_eq!(u32::from_le_bytes(b), 456);
    if cfg!(windows) {
        assert_eq!(process.host.submission_count(), 2);
    } else {
        assert_eq!(process.host.submission_count(), 1);
    }
    a = [7; 4];
    b = [7; 4];
    let error = process
        .read_indirect_many(
            &mut [
                IndirectReadRequest::new(0, 4, &mut a),
                IndirectReadRequest::new(100, 8, &mut b),
            ],
            ProcessBitness::X64,
        )
        .unwrap_err();
    assert!(matches!(error, MemoryError::Read { address: 100, .. }));
    assert_eq!(a, [7; 4]);
    assert_eq!(b, [7; 4]);
}

mod zero_alloc;
