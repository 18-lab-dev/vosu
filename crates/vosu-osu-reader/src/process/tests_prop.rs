//! `prop_scan_backend_matches_region_walk`: a [`ScanBackend`] that simply
//! forwards to the same [`ProcessMemory`] the region walk already uses must
//! yield identical results from every `scan_*` method on [`ProcessHandle`]
//! (F6, plan:208). This pins the contract a real backend (one answering
//! `SCAN_REQUEST` over a read plan, P3) must also satisfy.

use proptest::prelude::*;
use vosu_proptest::vosu_proptest_config;

use super::*;

/// A single readable region backed by an in-memory byte buffer.
#[derive(Debug)]
struct FixtureMemory {
    base: usize,
    bytes: Vec<u8>,
}

impl ProcessMemory for FixtureMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let Some(offset) = address.checked_sub(self.base) else {
            return Err(MemoryError::Read {
                address,
                len: dst.len(),
                message: "below fixture region".to_owned(),
            });
        };
        let end = offset
            .checked_add(dst.len())
            .filter(|&end| end <= self.bytes.len());
        match end {
            Some(end) => {
                dst.copy_from_slice(&self.bytes[offset..end]);
                Ok(())
            }
            None => Err(MemoryError::Read {
                address,
                len: dst.len(),
                message: "past fixture region".to_owned(),
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
        Ok(vec![MemoryRegion {
            address: self.base,
            size: self.bytes.len(),
        }])
    }
}

/// Forwards to the same backing `ProcessMemory` the region walk would use:
/// its results are, by construction, the region walk's own results.
struct EchoBackend {
    memory: Arc<dyn ProcessMemory>,
}

impl ScanBackend for EchoBackend {
    fn scan_all(&self, pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
        self.memory.scan_all(pattern)
    }

    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        self.memory.scan_batch(patterns)
    }
}

/// Generates a buffer, a sub-pattern inside it with some tokens wildcarded,
/// and a second, likely-unrelated pattern, so `scan_batch` exercises both a
/// resolvable and a possibly-missing pattern together.
fn fixture_and_patterns() -> impl Strategy<Value = (usize, Vec<u8>, Pattern, Pattern, bool)> {
    (
        0x1000usize..0x5000,
        prop::collection::vec(any::<u8>(), 8..96),
    )
        .prop_flat_map(|(base, bytes)| {
            let len = bytes.len();
            (
                Just(base),
                Just(bytes.clone()),
                0..len,
                1..8usize.min(len).max(2),
                prop::collection::vec(any::<bool>(), 0..8),
                "[0-9a-f]{2}( [0-9a-f]{2}){1,7}",
                any::<bool>(),
            )
                .prop_map(
                    move |(base, bytes, start, span, wildcards, noise, non_zero_mask)| {
                        let span = span.min(bytes.len().saturating_sub(start)).max(1);
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
                        let pattern = Pattern::parse(&tokens.join(" "), non_zero_mask)
                            .expect("generated pattern tokens are always valid");
                        let noise_pattern = Pattern::parse(&noise, non_zero_mask)
                            .unwrap_or_else(|_| pattern.clone());
                        (base, bytes, pattern, noise_pattern, non_zero_mask)
                    },
                )
        })
}

proptest! {
    #![proptest_config(vosu_proptest_config())]

    #[test]
    fn prop_scan_backend_matches_region_walk(
        (base, bytes, pattern, noise_pattern, _non_zero_mask) in fixture_and_patterns(),
    ) {
        let memory: Arc<dyn ProcessMemory> = Arc::new(FixtureMemory { base, bytes });
        let baseline = ProcessHandle::new(memory.clone(), ProcessBitness::X64);
        let backend: Arc<dyn ScanBackend> = Arc::new(EchoBackend { memory: memory.clone() });
        let injected = ProcessHandle::with_scan_backend(memory.clone(), ProcessBitness::X64, backend);

        prop_assert_eq!(baseline.scan_all(&pattern), injected.scan_all(&pattern));
        prop_assert_eq!(baseline.scan_first(&pattern), injected.scan_first(&pattern));
        prop_assert_eq!(
            baseline.scan_first_valid(&pattern, |address| Ok(address % 2 == 0)),
            injected.scan_first_valid(&pattern, |address| Ok(address % 2 == 0)),
        );
        let patterns = [pattern.clone(), noise_pattern];
        prop_assert_eq!(baseline.scan_batch(&patterns), injected.scan_batch(&patterns));
    }
}
