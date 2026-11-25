//! The SDK memory vocabulary's ordered defaults stay allocation-free: plan
//! and native backends call them per read, so a moved helper that started to
//! allocate (for example a conversion layer or a heap batch) would put an
//! allocation on every reader tick.

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;

use vosu_plugin_api::memory::{
    ChainReadRequest, IndirectReadRequest, MemoryError, MemoryRegion, ProcessBitness,
    ProcessMemory, ReadRequest, read_chains_default, read_indirect_many_default,
};

struct TrackingAllocator;

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

fn record() {
    TRACK_ALLOCATIONS.with(|enabled| {
        if enabled.get() {
            ALLOCATIONS.with(|count| count.set(count.get() + 1));
        }
    });
}

// SAFETY: every method forwards unchanged to `System`, which upholds the
// `GlobalAlloc` contract; the only addition is a thread-local counter that
// never allocates.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: forwards `layout` unchanged to `System::alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        record();
        // SAFETY: forwards `layout` unchanged to `System::alloc_zeroed`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        record();
        // SAFETY: forwards `ptr`, `layout`, and `new_size` unchanged to
        // `System::realloc`, which has the same safety contract.
        unsafe { System.realloc(ptr, layout, new_size) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: forwards `ptr` and `layout` unchanged to `System::dealloc`.
        unsafe { System.dealloc(ptr, layout) }
    }
}

#[global_allocator]
static GLOBAL_ALLOCATOR: TrackingAllocator = TrackingAllocator;

fn allocations_during<T>(operation: impl FnOnce() -> T) -> (T, usize) {
    ALLOCATIONS.with(|count| count.set(0));
    TRACK_ALLOCATIONS.with(|enabled| enabled.set(true));
    let result = operation();
    TRACK_ALLOCATIONS.with(|enabled| enabled.set(false));
    (result, ALLOCATIONS.with(Cell::get))
}

/// Fixed little-endian memory at address zero; reads never allocate.
struct Fixed([u8; 0x200]);

impl Fixed {
    fn copy(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let source = address
            .checked_add(dst.len())
            .and_then(|end| self.0.get(address..end))
            .ok_or(MemoryError::AddressOverflow {
                base: address,
                offset: dst.len(),
            })?;
        dst.copy_from_slice(source);
        Ok(())
    }
}

impl ProcessMemory for Fixed {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.copy(address, dst)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        reads
            .iter_mut()
            .try_for_each(|read| self.copy(read.address, read.dst))
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0,
            size: self.0.len(),
        }])
    }
}

#[test]
fn ordered_default_helpers_do_not_allocate() {
    let mut bytes = [0; 0x200];
    bytes[..8].copy_from_slice(&0x40_u64.to_le_bytes());
    bytes[0x48] = 0x5a;
    let memory: &dyn ProcessMemory = &Fixed(bytes);
    let offsets = [8];
    let mut chain = [0; 1];
    let mut resolved = 0;
    let mut direct = [0; 4];
    let mut indirect = [[0_u8; 2]; 30];

    let (result, allocations) = allocations_during(|| {
        memory.read_chain(0, &offsets, ProcessBitness::X64, &mut chain)?;
        let mut chains = [
            ChainReadRequest::new(0, &offsets, &mut chain).with_resolved_address(&mut resolved),
            ChainReadRequest::new(0x48, &[], &mut direct),
        ];
        read_chains_default(memory, &mut chains, ProcessBitness::X86)?;
        let [a, b, c, ..] = &mut indirect;
        let mut requests = [
            IndirectReadRequest::new(0, 0x08, a),
            IndirectReadRequest::new(0, 0x48, b),
            IndirectReadRequest::new(0x10, 0x48, c),
        ];
        read_indirect_many_default(memory, &mut requests, ProcessBitness::X64)
    });
    result.unwrap();
    assert_eq!(allocations, 0);
    assert_eq!(chain, [0x5a]);
    assert_eq!(resolved, 0x48);
    assert_eq!(direct, [0x5a, 0, 0, 0]);
    assert_eq!(indirect[0], [0x5a, 0]);
    assert_eq!(indirect[1], [0, 0]);
    // A null holder is an ordinary address: offset 0x48 from zero.
    assert_eq!(indirect[2], [0x5a, 0]);
}
