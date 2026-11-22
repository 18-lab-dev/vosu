use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

use super::*;
use crate::process::{ProcessBitness, ProcessMemory, ReadRequest};

struct TestRegion {
    region: MemoryRegion,
    bytes: Vec<u8>,
    readable: bool,
}

struct TestMemory {
    regions: Vec<TestRegion>,
    max_read: AtomicUsize,
    read_calls: AtomicUsize,
}

impl TestMemory {
    fn new(regions: Vec<(usize, Vec<u8>)>) -> Arc<Self> {
        Arc::new(Self {
            regions: regions
                .into_iter()
                .map(|(address, bytes)| TestRegion {
                    region: MemoryRegion {
                        address,
                        size: bytes.len(),
                    },
                    bytes,
                    readable: true,
                })
                .collect(),
            max_read: AtomicUsize::new(0),
            read_calls: AtomicUsize::new(0),
        })
    }

    fn with_unreadable(regions: Vec<(usize, Vec<u8>, bool)>) -> Arc<Self> {
        Arc::new(Self {
            regions: regions
                .into_iter()
                .map(|(address, bytes, readable)| TestRegion {
                    region: MemoryRegion {
                        address,
                        size: bytes.len(),
                    },
                    bytes,
                    readable,
                })
                .collect(),
            max_read: AtomicUsize::new(0),
            read_calls: AtomicUsize::new(0),
        })
    }
}

impl ProcessMemory for TestMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.max_read.fetch_max(dst.len(), Ordering::Relaxed);
        self.read_calls.fetch_add(1, Ordering::Relaxed);
        for item in &self.regions {
            let Some(offset) = address.checked_sub(item.region.address) else {
                continue;
            };
            if offset <= item.bytes.len() && dst.len() <= item.bytes.len().saturating_sub(offset) {
                if !item.readable {
                    break;
                }
                dst.copy_from_slice(&item.bytes[offset..offset + dst.len()]);
                return Ok(());
            }
        }
        Err(MemoryError::Read {
            address,
            len: dst.len(),
            message: "unreadable test range".to_owned(),
        })
    }

    fn read_many(&self, _reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        unreachable!("scanner uses read_exact")
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(self.regions.iter().map(|item| item.region).collect())
    }
}

struct PermissionDeniedMemory;

impl ProcessMemory for PermissionDeniedMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        Err(MemoryError::PermissionDenied {
            address,
            len: dst.len(),
            message: "permission denied".to_owned(),
        })
    }

    fn read_many(&self, _reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        unreachable!("scanner uses read_exact")
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0x1000,
            size: 16,
        }])
    }
}

fn handle(memory: Arc<TestMemory>) -> ProcessHandle {
    ProcessHandle::new(memory, ProcessBitness::X64)
}

fn pattern(value: &str, non_zero_mask: bool) -> Pattern {
    Pattern::parse(value, non_zero_mask).unwrap()
}

#[test]
fn parses_fixed_and_wildcard_bytes_and_rejects_malformed_tokens() {
    let parsed = pattern("0a ?? FF", true);
    assert_eq!(parsed.bytes(), &[0x0a, 0, 0xff]);
    assert_eq!(parsed.mask(), &[1, 0, 1]);
    assert!(parsed.non_zero_mask());
    assert_eq!(parsed.to_string(), "0A ?? FF");

    assert_eq!(
        Pattern::parse("A", false),
        Err(PatternParseError::InvalidToken {
            index: 0,
            token: "A".to_owned(),
        })
    );
    assert!(Pattern::parse("GG", false).is_err());
    assert!(Pattern::parse("?", false).is_err());
}

#[test]
fn fixed_and_wildcard_semantics_match_upstream() {
    let process = handle(TestMemory::new(vec![(
        0x1000,
        vec![0xaa, 0, 0xbb, 7, 0xbb],
    )]));

    assert_eq!(
        process.scan_first(&pattern("AA 00 BB", false)).unwrap(),
        Some(0x1000)
    );
    assert_eq!(
        process.scan_all(&pattern("AA ?? BB", false)).unwrap(),
        vec![0x1000]
    );
    assert_eq!(
        process.scan_all(&pattern("?? BB", false)).unwrap(),
        vec![0x1001, 0x1003]
    );
    assert_eq!(
        process.scan_all(&pattern("?? BB", true)).unwrap(),
        vec![0x1003]
    );
}

#[test]
fn finds_match_crossing_a_chunk_boundary_inside_one_region() {
    let mut bytes = vec![0; SCAN_CHUNK_SIZE + 8];
    bytes[SCAN_CHUNK_SIZE - 2..SCAN_CHUNK_SIZE + 2].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    let memory = TestMemory::new(vec![(0x8000, bytes)]);
    let process = handle(Arc::clone(&memory));
    assert_eq!(
        ScanScratch::new(4, usize::MAX).unwrap().bytes.len(),
        SCAN_CHUNK_SIZE + 3
    );
    assert_eq!(
        ScanScratch::new(4, 64 * 1024).unwrap().bytes.len(),
        64 * 1024
    );

    assert_eq!(
        process.scan_first(&pattern("DE AD BE EF", false)).unwrap(),
        Some(0x8000 + SCAN_CHUNK_SIZE - 2)
    );
    assert!(memory.read_calls.load(Ordering::Relaxed) >= 2);
    assert!(
        memory.max_read.load(Ordering::Relaxed) <= SCAN_CHUNK_SIZE + 3,
        "scratch/read window exceeded 1 MiB plus pattern overlap"
    );
}

#[test]
fn validated_scan_stops_before_reading_the_rest_of_a_large_region() {
    let mut bytes = vec![0; SCAN_CHUNK_SIZE * 3];
    bytes[..4].copy_from_slice(&[0xde, 0xad, 0xbe, 0xef]);
    let memory = TestMemory::new(vec![(0xa000, bytes)]);
    let process = handle(Arc::clone(&memory));
    let mut validations = 0;

    let found = process
        .scan_first_valid(&pattern("DE AD BE EF", false), |address| {
            validations += 1;
            Ok(address == 0xa000)
        })
        .unwrap();

    assert_eq!(found, Some(0xa000));
    assert_eq!(validations, 1);
    assert_eq!(
        memory.read_calls.load(Ordering::Relaxed),
        1,
        "accepted candidate must stop scanning before the next 1 MiB window"
    );
}

#[test]
fn mixed_pattern_overlap_partitions_candidate_starts_without_duplicates() {
    let mut bytes = vec![0; SCAN_CHUNK_SIZE + 4];
    bytes[SCAN_CHUNK_SIZE - 1] = 0xaa;
    bytes[SCAN_CHUNK_SIZE] = 0xaa;
    let memory = TestMemory::new(vec![(0x9000, bytes)]);
    let process = handle(Arc::clone(&memory));
    let short = pattern("AA", false);
    let long = pattern("AA AA AA AA AA AA AA AA", false);

    let mut scratch = ScanScratch::new(long.bytes().len(), memory.regions[0].region.size).unwrap();
    let mut offsets = Vec::new();
    assert!(
        scan_region(
            &process,
            memory.regions[0].region,
            long.bytes().len(),
            &mut scratch,
            |window, base, _| {
                let starts = valid_starts(window.len(), short.bytes().len(), base);
                visit_candidates(&short, window, starts, |offset| {
                    offsets.push(checked_offset_add(base, offset)?);
                    Ok(())
                })
            },
        )
        .unwrap()
    );
    assert_eq!(offsets, vec![SCAN_CHUNK_SIZE - 1, SCAN_CHUNK_SIZE]);
    assert_eq!(
        process.scan_all(&short).unwrap(),
        vec![0x9000 + SCAN_CHUNK_SIZE - 1, 0x9000 + SCAN_CHUNK_SIZE,]
    );
    assert_eq!(
        process.scan_batch(&[long, short]).unwrap(),
        vec![BatchScanResult {
            index: 1,
            address: 0x9000 + SCAN_CHUNK_SIZE - 1,
        }]
    );
}

#[test]
fn first_match_in_later_chunk_adds_chunk_and_region_base_once() {
    let mut bytes = vec![0; SCAN_CHUNK_SIZE + 64];
    bytes[SCAN_CHUNK_SIZE + 37] = 0xcc;
    let process = handle(TestMemory::new(vec![(0x1234_0000, bytes)]));

    assert_eq!(
        process.scan_first(&pattern("CC", false)).unwrap(),
        Some(0x1234_0000 + SCAN_CHUNK_SIZE + 37)
    );
}

#[test]
fn address_additions_report_overflow_instead_of_wrapping() {
    assert_eq!(
        checked_offset_add(usize::MAX, 1),
        Err(MemoryError::AddressOverflow {
            base: usize::MAX,
            offset: 1,
        })
    );

    let mut bytes = vec![0; 20];
    bytes[15] = 0xee;
    let region_base = usize::MAX - 10;
    let process = handle(TestMemory::new(vec![(region_base, bytes)]));
    assert_eq!(
        process.scan_first(&pattern("EE", false)),
        Err(MemoryError::AddressOverflow {
            base: region_base,
            offset: 15,
        })
    );
}

#[test]
fn never_combines_adjacent_regions() {
    let process = handle(TestMemory::new(vec![
        (0x1000, vec![0xaa]),
        (0x1001, vec![0xbb]),
    ]));
    assert_eq!(process.scan_first(&pattern("AA BB", false)).unwrap(), None);
    assert!(
        process
            .scan_all(&pattern("AA BB", false))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn first_and_all_follow_region_then_offset_order() {
    let process = handle(TestMemory::new(vec![
        (0x3000, vec![9, 1, 9, 1]),
        (0x1000, vec![1, 9, 1]),
    ]));
    let target = pattern("01", false);

    assert_eq!(process.scan_first(&target).unwrap(), Some(0x3001));
    assert_eq!(
        process.scan_all(&target).unwrap(),
        vec![0x3001, 0x3003, 0x1000, 0x1002]
    );
}

#[test]
fn batch_loops_regions_then_caller_patterns_and_omits_missing() {
    let process = handle(TestMemory::new(vec![
        (0x2000, vec![0xbb, 0xaa]),
        (0x1000, vec![0xcc]),
    ]));
    let patterns = [
        pattern("CC", false),
        pattern("AA", false),
        pattern("DD", false),
        pattern("", false),
        pattern("BB", false),
    ];

    assert_eq!(
        process.scan_batch(&patterns).unwrap(),
        vec![
            BatchScanResult {
                index: 1,
                address: 0x2001,
            },
            BatchScanResult {
                index: 3,
                address: 0x2000,
            },
            BatchScanResult {
                index: 4,
                address: 0x2000,
            },
            BatchScanResult {
                index: 0,
                address: 0x1000,
            },
        ]
    );
}

#[test]
fn handles_empty_all_wildcard_and_no_match_deliberately() {
    let process = handle(TestMemory::new(vec![(0x5000, vec![0, 1, 2])]));
    let empty = pattern("  ", false);

    assert_eq!(process.scan_first(&empty).unwrap(), Some(0x5000));
    assert!(process.scan_all(&empty).unwrap().is_empty());
    assert!(process.scan_batch(&[]).unwrap().is_empty());
    assert_eq!(
        process.scan_all(&pattern("?? ??", false)).unwrap(),
        vec![0x5000, 0x5001]
    );
    assert_eq!(
        process.scan_all(&pattern("?? ??", true)).unwrap(),
        vec![0x5001]
    );
    assert_eq!(process.scan_first(&pattern("FF", false)).unwrap(), None);
    assert!(process.scan_all(&pattern("FF", false)).unwrap().is_empty());
}

#[test]
fn unreadable_regions_are_skipped_without_leaking_partial_matches() {
    let memory = TestMemory::with_unreadable(vec![
        (0x1000, vec![0xaa], false),
        (0x2000, vec![0xaa], true),
    ]);
    let process = handle(memory);

    assert_eq!(
        process.scan_first(&pattern("AA", false)).unwrap(),
        Some(0x2000)
    );
    assert_eq!(
        process.scan_all(&pattern("AA", false)).unwrap(),
        vec![0x2000]
    );
}

#[test]
fn batch_discards_failed_region_matches_before_reusing_slots() {
    let mut bytes = vec![0; SCAN_CHUNK_SIZE];
    bytes[16] = 0xaa;
    let mut memory = TestMemory::new(vec![(0x1000, bytes), (0x400000, vec![0xbb])]);
    Arc::get_mut(&mut memory).unwrap().regions[0].region.size = 2 * SCAN_CHUNK_SIZE;
    let process = handle(memory.clone());

    assert_eq!(
        process
            .scan_batch(&[pattern("AA", false), pattern("BB", false)])
            .unwrap(),
        vec![BatchScanResult {
            index: 1,
            address: 0x400000,
        }]
    );
    assert_eq!(memory.read_calls.load(Ordering::Relaxed), 3);
}

#[test]
fn permission_denials_stop_scanning_instead_of_appearing_unreadable() {
    let process = ProcessHandle::new(Arc::new(PermissionDeniedMemory), ProcessBitness::X64);
    let target = pattern("AA", false);

    assert!(matches!(
        process.scan_first(&target),
        Err(MemoryError::PermissionDenied {
            address: 0x1000,
            len: 16,
            ..
        })
    ));
    assert!(matches!(
        process.scan_first_valid(&target, |_| Ok(false)),
        Err(MemoryError::PermissionDenied {
            address: 0x1000,
            len: 16,
            ..
        })
    ));
}
