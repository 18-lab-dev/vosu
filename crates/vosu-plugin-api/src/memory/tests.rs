//! The moved vocabulary must keep the exact semantics the native and plan
//! backends were written against: object-safe dispatch, pointer-width holder
//! reads, ordered publication with no partial success, overflow reported as an
//! error, and 24-request two-level batches with readable null pointers.

use std::sync::{Arc, Mutex};

use super::*;

/// Little-endian memory at `base`, recording every `read_exact` and every
/// `read_many` batch as `(address, len)` pairs in call order.
struct Recorded {
    base: usize,
    bytes: Vec<u8>,
    exact: Mutex<Vec<(usize, usize)>>,
    batches: Mutex<Vec<Vec<(usize, usize)>>>,
}

impl Recorded {
    fn new(base: usize, bytes: Vec<u8>) -> Self {
        Self {
            base,
            bytes,
            exact: Mutex::new(Vec::new()),
            batches: Mutex::new(Vec::new()),
        }
    }

    fn copy(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let start = address.checked_sub(self.base);
        let range = start.and_then(|start| Some(start..start.checked_add(dst.len())?));
        match range.and_then(|range| self.bytes.get(range)) {
            Some(source) => {
                dst.copy_from_slice(source);
                Ok(())
            }
            None => Err(MemoryError::Read {
                address,
                len: dst.len(),
                message: "outside fixture".to_owned(),
            }),
        }
    }
}

impl ProcessMemory for Recorded {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.exact.lock().unwrap().push((address, dst.len()));
        self.copy(address, dst)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.batches.lock().unwrap().push(
            reads
                .iter()
                .map(|read| (read.address, read.dst.len()))
                .collect(),
        );
        for read in reads {
            self.copy(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: self.base,
            size: self.bytes.len(),
        }])
    }
}

fn put(bytes: &mut [u8], offset: usize, value: &[u8]) {
    bytes[offset..offset + value.len()].copy_from_slice(value);
}

#[test]
fn trait_stays_object_safe_and_defaults_dispatch_through_dyn() {
    let mut bytes = vec![0; 0x40];
    put(&mut bytes, 0x00, &0x1010_u64.to_le_bytes());
    put(&mut bytes, 0x14, &[0xaa, 0xbb]);
    let memory: Arc<dyn ProcessMemory> = Arc::new(Recorded::new(0x1000, bytes));

    let mut dst = [0; 2];
    memory
        .read_chain(0x1000, &[4], ProcessBitness::X64, &mut dst)
        .unwrap();
    assert_eq!(dst, [0xaa, 0xbb]);
    assert_eq!(memory.process_id(), None);
    assert!(memory.is_alive());
    assert!(!memory.reads_pin_process_identity());
    let mut scanned = [0; 2];
    memory
        .read_scanner_region_exact(0x1014, &mut scanned)
        .unwrap();
    assert_eq!(scanned, [0xaa, 0xbb]);
}

#[test]
fn chain_holder_reads_use_exactly_the_target_pointer_width() {
    let mut bytes = vec![0; 0x40];
    // Low half points at 0x1020; a nonzero high half must be ignored for X86
    // and change the pointer for X64.
    put(&mut bytes, 0x00, &0x1020_u32.to_le_bytes());
    put(&mut bytes, 0x04, &[0xff; 4]);
    put(&mut bytes, 0x24, &[0x5a]);
    let memory = Recorded::new(0x1000, bytes);

    let mut dst = [0; 1];
    memory
        .read_chain(0x1000, &[4], ProcessBitness::X86, &mut dst)
        .unwrap();
    assert_eq!(dst, [0x5a]);
    assert_eq!(*memory.exact.lock().unwrap(), [(0x1000, 4), (0x1024, 1)]);

    memory.exact.lock().unwrap().clear();
    let error = memory
        .read_chain(0x1000, &[4], ProcessBitness::X64, &mut dst)
        .unwrap_err();
    assert_eq!(memory.exact.lock().unwrap()[0], (0x1000, 8));
    #[cfg(target_pointer_width = "64")]
    assert_eq!(
        error,
        MemoryError::Read {
            address: 0xffff_ffff_0000_1024,
            len: 1,
            message: "outside fixture".to_owned(),
        }
    );
    #[cfg(not(target_pointer_width = "64"))]
    assert!(matches!(error, MemoryError::Corrupt { .. }));
}

#[test]
fn chain_offset_overflow_is_an_error_not_a_wrapped_address() {
    let mut bytes = vec![0; 8];
    put(&mut bytes, 0, &u64::MAX.to_le_bytes());
    let memory = Recorded::new(0x1000, bytes);
    let mut dst = [0; 1];
    let error = memory
        .read_chain(0x1000, &[2], ProcessBitness::X64, &mut dst)
        .unwrap_err();
    let pointer = usize::try_from(u64::MAX).unwrap_or(usize::MAX);
    if usize::BITS == 64 {
        assert_eq!(
            error,
            MemoryError::AddressOverflow {
                base: pointer,
                offset: 2
            }
        );
    } else {
        assert!(matches!(error, MemoryError::Corrupt { .. }));
    }
    assert_eq!(memory.exact.lock().unwrap().len(), 1);
}

#[test]
fn default_chains_publish_in_order_and_never_report_partial_success() {
    let mut bytes = vec![0; 0x40];
    put(&mut bytes, 0x00, &0x1010_u64.to_le_bytes());
    put(&mut bytes, 0x18, &[1, 2]);
    put(&mut bytes, 0x30, &[9]);
    let memory = Recorded::new(0x1000, bytes);

    let offsets = [8];
    let missing = [0x1_0000];
    let mut first = [0; 2];
    let mut second = [0; 1];
    let mut third = [0xee; 1];
    let mut first_resolved = 0;
    let mut third_resolved = 0x77;
    let error = {
        let mut reads = [
            ChainReadRequest::new(0x1000, &offsets, &mut first)
                .with_resolved_address(&mut first_resolved),
            ChainReadRequest::new(0x1030, &[], &mut second),
            ChainReadRequest::new(0x1000, &missing, &mut third)
                .with_resolved_address(&mut third_resolved),
        ];
        read_chains_default(&memory, &mut reads, ProcessBitness::X64).unwrap_err()
    };

    assert!(matches!(
        error,
        MemoryError::Read {
            address: 0x1_1010,
            ..
        }
    ));
    assert_eq!(first, [1, 2]);
    assert_eq!(first_resolved, 0x1018);
    assert_eq!(second, [9]);
    // The failing request publishes neither bytes nor a resolved address.
    assert_eq!(third, [0xee]);
    assert_eq!(third_resolved, 0x77);
}

#[test]
fn indirect_batches_split_at_24_and_keep_null_pointers_readable() {
    // 30 holders at 0x00.. (8 bytes each); every holder is null, so each
    // pointee read lands at the bare offset, which the fixture maps.
    let mut bytes = vec![0; 0x200];
    for index in 0..30 {
        put(&mut bytes, 0x100 + index * 4, &[index as u8; 4]);
    }
    let memory = Recorded::new(0, bytes);

    let mut destinations = [[0_u8; 4]; 30];
    {
        let mut reads: Vec<_> = destinations
            .iter_mut()
            .enumerate()
            .map(|(index, dst)| IndirectReadRequest::new(index * 8, 0x100 + index * 4, dst))
            .collect();
        read_indirect_many_default(&memory, &mut reads, ProcessBitness::X64).unwrap();
    }

    for (index, dst) in destinations.iter().enumerate() {
        assert_eq!(*dst, [index as u8; 4]);
    }
    let batches = memory.batches.lock().unwrap();
    let sizes: Vec<_> = batches.iter().map(Vec::len).collect();
    assert_eq!(sizes, [24, 24, 6, 6]);
    assert!(batches[0].iter().all(|&(_, len)| len == 8));
    assert_eq!(batches[1][0], (0x100, 4));
    assert!(memory.exact.lock().unwrap().is_empty());
}

#[test]
fn indirect_x86_holders_read_four_bytes() {
    let mut bytes = vec![0; 0x40];
    put(&mut bytes, 0x00, &0x20_u32.to_le_bytes());
    put(&mut bytes, 0x04, &[0xff; 4]);
    put(&mut bytes, 0x22, &[7]);
    let memory = Recorded::new(0, bytes);
    let mut dst = [0; 1];
    read_indirect_many_default(
        &memory,
        &mut [IndirectReadRequest::new(0, 2, &mut dst)],
        ProcessBitness::X86,
    )
    .unwrap();
    assert_eq!(dst, [7]);
    assert_eq!(memory.batches.lock().unwrap()[0], [(0, 4)]);
}

#[test]
fn region_end_reports_overflow() {
    let region = MemoryRegion {
        address: usize::MAX,
        size: 1,
    };
    assert_eq!(
        region.end(),
        Err(MemoryError::AddressOverflow {
            base: usize::MAX,
            offset: 1
        })
    );
    assert_eq!(
        MemoryRegion {
            address: 0x1000,
            size: 0x10
        }
        .end(),
        Ok(0x1010)
    );
}

#[test]
fn parses_fixed_and_wildcard_bytes_and_rejects_malformed_tokens() {
    let parsed = Pattern::parse("0a ?? FF", true).unwrap();
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
fn fixed_anchor_is_the_longest_fixed_run() {
    assert_eq!(
        Pattern::parse("AA ?? BB CC DD ?? EE", false)
            .unwrap()
            .fixed_anchor(),
        (2, 3)
    );
    assert_eq!(Pattern::parse("?? ??", false).unwrap().fixed_anchor().1, 0);
    assert_eq!(Pattern::parse("", false).unwrap().fixed_anchor(), (0, 0));
}

#[test]
fn matches_at_applies_mask_and_never_reads_past_the_window() {
    let window = [0xaa, 0x00, 0xbb, 0xaa, 0x01, 0xbb];
    let any = Pattern::parse("AA ?? BB", false).unwrap();
    let non_zero = Pattern::parse("AA ?? BB", true).unwrap();
    assert!(any.matches_at(&window, 0));
    assert!(any.matches_at(&window, 3));
    assert!(!non_zero.matches_at(&window, 0));
    assert!(non_zero.matches_at(&window, 3));
    assert!(!any.matches_at(&window, 1));
    // A start whose pattern would extend past the window is no match.
    assert!(!any.matches_at(&window, 4));
    assert!(!any.matches_at(&window, usize::MAX));
}
