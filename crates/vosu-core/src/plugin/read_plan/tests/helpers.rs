//! Shared fixtures for `tests::stages`, `tests::sync`, `tests::native`, and
//! `tests::edge_cases`: fake [`ProcessMemory`]/[`ModuleResolver`]
//! implementations and the small `op`/`far_deadline` builders every plan
//! submission uses. Split out of `tests.rs` so each topic module stays
//! under the maintained-file line limit (`docs/architecture/source-map.md`).

use std::sync::Mutex;
use std::time::{Duration, Instant};

use super::*;

#[derive(Debug, Default)]
pub(super) struct FakeMemory {
    regions: Mutex<Vec<(usize, Vec<u8>)>>,
}

impl FakeMemory {
    pub(super) fn new(regions: Vec<(usize, Vec<u8>)>) -> Self {
        Self {
            regions: Mutex::new(regions),
        }
    }
}

impl ProcessMemory for FakeMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let regions = self.regions.lock().unwrap();
        for (base, bytes) in regions.iter() {
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
            message: "unmapped test address".to_owned(),
        })
    }

    fn read_many(
        &self,
        reads: &mut [vosu_sys::process::ReadRequest<'_>],
    ) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<vosu_sys::process::MemoryRegion>, MemoryError> {
        Ok(self
            .regions
            .lock()
            .unwrap()
            .iter()
            .map(|(address, bytes)| vosu_sys::process::MemoryRegion {
                address: *address,
                size: bytes.len(),
            })
            .collect())
    }
}

pub(super) struct PermissionDeniedMemory;

impl ProcessMemory for PermissionDeniedMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        Err(MemoryError::PermissionDenied {
            address,
            len: dst.len(),
            message: "denied".to_owned(),
        })
    }

    fn read_many(
        &self,
        reads: &mut [vosu_sys::process::ReadRequest<'_>],
    ) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<vosu_sys::process::MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

/// A single-threaded read that sleeps past the deadline before returning,
/// so [`execute`] observes a timeout on its next deadline check.
pub(super) struct SlowMemory;

impl ProcessMemory for SlowMemory {
    fn read_exact(&self, _address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        std::thread::sleep(Duration::from_millis(20));
        dst.fill(0);
        Ok(())
    }

    fn read_many(
        &self,
        reads: &mut [vosu_sys::process::ReadRequest<'_>],
    ) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<vosu_sys::process::MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

pub(super) struct Modules(pub(super) Vec<(&'static str, u64)>);

impl ModuleResolver for Modules {
    fn resolve(&self, module: &str) -> Option<u64> {
        self.0
            .iter()
            .find(|(name, _)| *name == module)
            .map(|(_, base)| *base)
    }
}

pub(super) struct NoModules;

impl ModuleResolver for NoModules {
    fn resolve(&self, _module: &str) -> Option<u64> {
        None
    }
}

pub(super) fn op(offset: i64, length: u32) -> PlanOp {
    PlanOp { offset, length }
}

pub(super) fn far_deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}
