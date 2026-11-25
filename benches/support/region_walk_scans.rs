//! Core's region walk (`vosu_sys::process::scanner`, what a
//! `VOSU_OP_SCAN_REQUEST` runs) as the scan backend for root harnesses that
//! open a target or fixture themselves. A reader `ProcessHandle` never walks
//! regions on its own (M5 R1), so a harness that scans must say so here.

use std::sync::Arc;

use vosu_osu_reader::process::{ProcessHandle, ScanBackend};
use vosu_plugin_api::memory::{
    BatchScanResult, MemoryError, Pattern, ProcessBitness, ProcessMemory,
};

struct RegionWalkScans(Arc<dyn ProcessMemory>);

impl ScanBackend for RegionWalkScans {
    fn scan_all(&self, pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
        vosu_sys::process::scanner::scan_all(&*self.0, pattern)
    }

    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        vosu_sys::process::scanner::scan_batch(&*self.0, patterns)
    }
}

/// A handle over `memory` whose scans run core's region walk over it.
pub fn scanning_handle(memory: Arc<dyn ProcessMemory>, bitness: ProcessBitness) -> ProcessHandle {
    let scans = Arc::new(RegionWalkScans(Arc::clone(&memory)));
    ProcessHandle::with_scan_backend(memory, bitness, scans)
}
