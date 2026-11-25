//! Test-only [`ScanBackend`] standing in for core's authorized scan path:
//! `vosu-sys`'s region walk, the same one core runs for a
//! `VOSU_OP_SCAN_REQUEST` (a dev-dependency only; production reader code has
//! no `vosu-sys` edge).

use std::sync::Arc;

use vosu_plugin_api::memory::{
    BatchScanResult, MemoryError, Pattern, ProcessBitness, ProcessMemory,
};
use vosu_sys::process::scanner;

use super::{ProcessHandle, ScanBackend};

/// Answers scans with core's region walk over the fixture memory itself.
pub(crate) struct RegionWalkScans {
    memory: Arc<dyn ProcessMemory>,
}

impl RegionWalkScans {
    pub(crate) fn new(memory: Arc<dyn ProcessMemory>) -> Self {
        Self { memory }
    }
}

impl ScanBackend for RegionWalkScans {
    fn scan_all(&self, pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
        scanner::scan_all(&*self.memory, pattern)
    }

    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        scanner::scan_batch(&*self.memory, patterns)
    }
}

/// A handle whose scans run core's region walk over `memory`.
pub(crate) fn scanning_handle(
    memory: Arc<dyn ProcessMemory>,
    bitness: ProcessBitness,
) -> ProcessHandle {
    let scans = Arc::new(RegionWalkScans::new(Arc::clone(&memory)));
    ProcessHandle::with_scan_backend(memory, bitness, scans)
}
