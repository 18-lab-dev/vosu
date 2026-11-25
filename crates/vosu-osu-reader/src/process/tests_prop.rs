//! `prop_scan_backend_matches_region_walk`: a [`ScanBackend`] that simply
//! forwards to core's region walk over the same [`ProcessMemory`] must
//! yield identical results from `scan_all`/`scan_batch` on [`ProcessHandle`]
//! as that walk run directly (F6, plan:208). This pins the contract a real
//! backend (one answering `SCAN_REQUEST` over a read plan, P3) must also
//! satisfy.
//!
//! `scan_backend_routes_every_scan_method_through_the_backend` complements it
//! with a backend whose output is deliberately unlike anything the region
//! walk could produce, so a dropped `scans` field fails loudly instead of the
//! echo backend's results happening to agree by construction, and
//! `read_only_handle_refuses_scans_without_a_native_fallback` proves a
//! handle without a backend never walks the target itself (M5 R1).

use std::sync::atomic::{AtomicUsize, Ordering};

use proptest::prelude::*;
use vosu_proptest::vosu_proptest_config;

use super::test_scans::scanning_handle;
use super::*;
use vosu_sys::process::scanner::{self, SCAN_CHUNK_SIZE};

/// One or more non-overlapping readable regions backed by in-memory byte
/// buffers (bases spaced `REGION_STRIDE` apart, well past any generated
/// buffer's length, so segments never overlap).
#[derive(Debug)]
struct FixtureMemory {
    segments: Vec<(usize, Vec<u8>)>,
}

const REGION_STRIDE: usize = 0x10_0000;

impl FixtureMemory {
    fn single(base: usize, bytes: Vec<u8>) -> Self {
        Self {
            segments: vec![(base, bytes)],
        }
    }

    fn find(&self, address: usize, len: usize) -> Option<&[u8]> {
        self.segments.iter().find_map(|(base, bytes)| {
            let offset = address.checked_sub(*base)?;
            let end = offset.checked_add(len).filter(|&end| end <= bytes.len())?;
            Some(&bytes[offset..end])
        })
    }
}

impl ProcessMemory for FixtureMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        match self.find(address, dst.len()) {
            Some(slice) => {
                dst.copy_from_slice(slice);
                Ok(())
            }
            None => Err(MemoryError::Read {
                address,
                len: dst.len(),
                message: "outside every fixture segment".to_owned(),
            }),
        }
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(self
            .segments
            .iter()
            .map(|(base, bytes)| MemoryRegion {
                address: *base,
                size: bytes.len(),
            })
            .collect())
    }
}

/// Core's region walk run directly over the fixture memory: the results a
/// `SCAN_RESULT` carries, with no [`ProcessHandle`] in between.
struct RegionWalk {
    memory: Arc<dyn ProcessMemory>,
}

impl RegionWalk {
    fn scan_all(&self, pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
        scanner::scan_all(&*self.memory, pattern)
    }

    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        scanner::scan_batch(&*self.memory, patterns)
    }
}

fn echo_fixture(memory: Arc<dyn ProcessMemory>) -> (RegionWalk, ProcessHandle) {
    let baseline = RegionWalk {
        memory: memory.clone(),
    };
    let injected = scanning_handle(memory, ProcessBitness::X64);
    (baseline, injected)
}

/// Asserts `scan_all`/`scan_batch` agree between the direct walk and the
/// handle built by [`echo_fixture`]; `scan_first`/`scan_first_valid` are
/// derived from `scan_all` (see those methods' doc comments on
/// `ProcessHandle`).
fn assert_scan_all_and_batch_agree(
    baseline: &RegionWalk,
    injected: &ProcessHandle,
    pattern: &Pattern,
    noise_pattern: &Pattern,
) {
    assert_eq!(baseline.scan_all(pattern), injected.scan_all(pattern));
    let patterns = [pattern.clone(), noise_pattern.clone()];
    assert_eq!(
        baseline.scan_batch(&patterns),
        injected.scan_batch(&patterns)
    );
}

/// Builds a pattern matching `bytes[start..start + span]`, with some tokens
/// wildcarded per `wildcards`.
fn make_pattern(
    bytes: &[u8],
    start: usize,
    span: usize,
    wildcards: &[bool],
    non_zero_mask: bool,
) -> Pattern {
    let tokens: Vec<String> = bytes[start..start + span]
        .iter()
        .enumerate()
        .map(|(index, byte)| {
            if wildcards.get(index).copied().unwrap_or(false) {
                "??".to_owned()
            } else {
                format!("{byte:02x}")
            }
        })
        .collect();
    Pattern::parse(&tokens.join(" "), non_zero_mask)
        .expect("generated pattern tokens are always valid")
}

/// Generates 1-3 non-overlapping regions, a sub-pattern inside one of them
/// with some tokens wildcarded, and a second, likely-unrelated pattern, so
/// `scan_batch` exercises both a resolvable and a possibly-missing pattern
/// together.
fn fixture_and_patterns() -> impl Strategy<Value = (Vec<(usize, Vec<u8>)>, Pattern, Pattern)> {
    (1usize..=3, prop::collection::vec(any::<bool>(), 0..8))
        .prop_flat_map(|(region_count, wildcards)| {
            (
                prop::collection::vec(prop::collection::vec(any::<u8>(), 8..32), region_count),
                0..region_count,
                "[0-9a-f]{2}( [0-9a-f]{2}){1,7}",
                any::<bool>(),
                Just(wildcards),
            )
        })
        .prop_flat_map(
            |(buffers, needle_region, noise, non_zero_mask, wildcards)| {
                let needle_len = buffers[needle_region].len();
                (
                    Just(buffers),
                    Just(needle_region),
                    0..needle_len,
                    1usize..8,
                    Just(noise),
                    Just(non_zero_mask),
                    Just(wildcards),
                )
            },
        )
        .prop_map(
            |(buffers, needle_region, start, span, noise, non_zero_mask, wildcards)| {
                let span = span
                    .min(buffers[needle_region].len().saturating_sub(start))
                    .max(1);
                let start = start.min(buffers[needle_region].len().saturating_sub(span));
                let pattern = make_pattern(
                    &buffers[needle_region],
                    start,
                    span,
                    &wildcards,
                    non_zero_mask,
                );
                let noise_pattern =
                    Pattern::parse(&noise, non_zero_mask).unwrap_or_else(|_| pattern.clone());
                let segments = buffers
                    .into_iter()
                    .enumerate()
                    .map(|(index, bytes)| (0x1000 + index * REGION_STRIDE, bytes))
                    .collect();
                (segments, pattern, noise_pattern)
            },
        )
}

proptest! {
    #![proptest_config(vosu_proptest_config())]

    #[test]
    fn prop_scan_backend_matches_region_walk(
        (segments, pattern, noise_pattern) in fixture_and_patterns(),
    ) {
        let memory: Arc<dyn ProcessMemory> = Arc::new(FixtureMemory { segments });
        let (baseline, injected) = echo_fixture(memory);
        assert_scan_all_and_batch_agree(&baseline, &injected, &pattern, &noise_pattern);
    }
}

/// One region just past `SCAN_CHUNK_SIZE`, with the needle in the second
/// chunk, run once (not inside the randomized property) to keep the
/// multi-megabyte allocation cheap.
#[test]
fn prop_scan_backend_matches_region_walk_across_a_chunk_boundary() {
    let mut bytes = vec![0u8; SCAN_CHUNK_SIZE + 64];
    let needle = b"\xDE\xAD\xBE\xEF\xCA\xFE";
    let needle_at = SCAN_CHUNK_SIZE + 8;
    bytes[needle_at..needle_at + needle.len()].copy_from_slice(needle);
    let pattern = Pattern::parse("de ad be ef ca fe", false).unwrap();
    let noise_pattern = Pattern::parse("00 11 22 33", false).unwrap();

    let memory: Arc<dyn ProcessMemory> = Arc::new(FixtureMemory::single(0x1000, bytes));
    let (baseline, injected) = echo_fixture(memory);
    assert_scan_all_and_batch_agree(&baseline, &injected, &pattern, &noise_pattern);
    assert_eq!(
        baseline.scan_all(&pattern).unwrap(),
        vec![0x1000 + needle_at]
    );
}

/// A backend whose output is unlike anything the region walk could produce
/// (a fixed out-of-range sentinel, unreachable by any real scan over
/// [`FixtureMemory`]'s bounded segments), plus call counters, so a dropped
/// `scans` field or an inverted dispatch `match` arm fails this test instead
/// of silently agreeing with [`EchoBackend`] by construction.
#[derive(Default)]
struct DistortingBackend {
    scan_all_calls: AtomicUsize,
    scan_batch_calls: AtomicUsize,
}

impl DistortingBackend {
    const SENTINEL: usize = 0xDEAD_0000;

    fn scan_all_calls(&self) -> usize {
        self.scan_all_calls.load(Ordering::SeqCst)
    }

    fn scan_batch_calls(&self) -> usize {
        self.scan_batch_calls.load(Ordering::SeqCst)
    }
}

impl ScanBackend for DistortingBackend {
    fn scan_all(&self, _pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
        self.scan_all_calls.fetch_add(1, Ordering::SeqCst);
        Ok(vec![Self::SENTINEL])
    }

    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        self.scan_batch_calls.fetch_add(1, Ordering::SeqCst);
        Ok((0..patterns.len())
            .map(|index| BatchScanResult {
                index,
                address: Self::SENTINEL,
            })
            .collect())
    }
}

#[test]
fn scan_backend_routes_every_scan_method_through_the_backend() {
    // A real needle that the region walk (and a routing bug that bypassed
    // the backend) would actually find, distinct from the backend's
    // sentinel, so a routing failure cannot pass by accident.
    let mut bytes = vec![0u8; 32];
    bytes[4..8].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
    let pattern = Pattern::parse("aa bb cc dd", false).unwrap();
    let memory: Arc<dyn ProcessMemory> = Arc::new(FixtureMemory::single(0x2000, bytes));

    let backend = Arc::new(DistortingBackend::default());
    let handle = ProcessHandle::with_scan_backend(memory, ProcessBitness::X64, backend.clone());

    assert_eq!(
        handle.scan_all(&pattern).unwrap(),
        vec![DistortingBackend::SENTINEL]
    );
    assert_eq!(
        handle.scan_first(&pattern).unwrap(),
        Some(DistortingBackend::SENTINEL)
    );
    assert_eq!(
        handle
            .scan_first_valid(&pattern, |address| Ok(
                address == DistortingBackend::SENTINEL
            ))
            .unwrap(),
        Some(DistortingBackend::SENTINEL)
    );
    // A validator that rejects every candidate proves scan_first_valid
    // actually calls it, rather than short-circuiting on the first address.
    assert_eq!(
        handle.scan_first_valid(&pattern, |_| Ok(false)).unwrap(),
        None
    );
    // A validator error must propagate rather than being swallowed.
    let validate_error = handle
        .scan_first_valid(&pattern, |_| {
            Err(MemoryError::Backend {
                operation: "test",
                message: "validator refused".to_owned(),
            })
        })
        .unwrap_err();
    assert!(matches!(validate_error, MemoryError::Backend { .. }));
    assert_eq!(
        handle.scan_batch(std::slice::from_ref(&pattern)).unwrap(),
        vec![BatchScanResult {
            index: 0,
            address: DistortingBackend::SENTINEL,
        }]
    );

    // scan_all, scan_first, and three scan_first_valid calls each reach the
    // backend exactly once.
    assert_eq!(backend.scan_all_calls(), 5);
    assert_eq!(backend.scan_batch_calls(), 1);
}

/// Counterexample "scanner fallback bypasses broker": a handle without a
/// backend refuses every scan method even though a native region walk over
/// the same memory would find the needle, and reads still work.
#[test]
fn read_only_handle_refuses_scans_without_a_native_fallback() {
    let mut bytes = vec![0u8; 32];
    bytes[4..8].copy_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
    let pattern = Pattern::parse("aa bb cc dd", false).unwrap();
    let memory: Arc<dyn ProcessMemory> = Arc::new(FixtureMemory::single(0x2000, bytes));
    assert_eq!(scanner::scan_all(&*memory, &pattern).unwrap(), vec![0x2004]);

    let handle = ProcessHandle::new(memory, ProcessBitness::X64);
    let refused = |error: MemoryError| {
        assert!(
            matches!(
                error,
                MemoryError::Backend {
                    operation: "scan",
                    ..
                }
            ),
            "expected the no-backend refusal, got {error:?}"
        );
    };
    refused(handle.scan_all(&pattern).unwrap_err());
    refused(handle.scan_first(&pattern).unwrap_err());
    refused(
        handle
            .scan_first_valid(&pattern, |_| panic!("no candidate may be offered"))
            .unwrap_err(),
    );
    refused(
        handle
            .scan_batch(std::slice::from_ref(&pattern))
            .unwrap_err(),
    );
    assert_eq!(handle.read_u32(0x2004).unwrap(), 0xDDCC_BBAA);
}
