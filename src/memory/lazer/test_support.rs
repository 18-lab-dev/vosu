use super::*;

pub(super) use crate::process::{MemoryRegion, ProcessBitness, ProcessMemory};
use std::collections::BTreeMap;
use std::sync::atomic::AtomicUsize;
pub(super) use std::sync::atomic::Ordering;

#[derive(Default)]
pub(super) struct SparseMemory {
    pub(super) bytes: BTreeMap<usize, u8>,
    pub(super) batches: AtomicUsize,
}

impl SparseMemory {
    pub(super) fn put(&mut self, address: usize, value: impl AsRef<[u8]>) {
        for (index, byte) in value.as_ref().iter().copied().enumerate() {
            self.bytes.insert(address + index, byte);
        }
    }
    pub(super) fn ptr(&mut self, address: usize, value: usize) {
        self.put(address, value.to_le_bytes());
    }
    pub(super) fn i32(&mut self, address: usize, value: i32) {
        self.put(address, value.to_le_bytes());
    }
    pub(super) fn i64(&mut self, address: usize, value: i64) {
        self.put(address, value.to_le_bytes());
    }
    pub(super) fn f64(&mut self, address: usize, value: f64) {
        self.put(address, value.to_le_bytes());
    }
    pub(super) fn valid_base(&mut self, base: usize, offsets: &LazerOffsets) {
        let vtable = base + 0x800;
        self.ptr(base, vtable);
        self.i64(vtable, offsets.effective_game_base_vtable() as i64);
    }
    pub(super) fn string_ptr(&mut self, field: usize, object: usize, text: &str) {
        self.ptr(field, object);
        let units: Vec<u16> = text.encode_utf16().collect();
        self.i32(object + 8, units.len() as i32);
        for (index, unit) in units.into_iter().enumerate() {
            self.put(object + 0xc + index * 2, unit.to_le_bytes());
        }
    }
}

impl ProcessMemory for SparseMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let len = dst.len();
        for (index, byte) in dst.iter_mut().enumerate() {
            *byte = *self
                .bytes
                .get(&(address + index))
                .ok_or_else(|| MemoryError::Read {
                    address,
                    len,
                    message: "sparse hole".into(),
                })?;
        }
        Ok(())
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.batches.fetch_add(1, Ordering::Relaxed);
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

pub(super) fn reader(memory: Arc<SparseMemory>) -> LazerMemory {
    LazerMemory::new(
        ProcessHandle::new(memory, ProcessBitness::X64),
        Arc::new(LazerOffsets::embedded().expect("embedded offsets")),
        LazerPattern {
            scaling_container_target_draw_size: Pattern::parse("AA", false).unwrap(),
            offset: 0,
        },
    )
}
