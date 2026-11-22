//! Proves `execute_into` is allocation-free once its output buffer has
//! been warmed to size, for both a single-stage and a dependent-stage
//! plan (the D17 M1 performance gate, mirroring
//! `tests/performance_invariants.rs::scalar_readers_allocate_nothing`'s
//! own pattern one crate down, since that integration test cannot see
//! this crate's private executor). Split out of `tests.rs` so that module
//! stays under the maintained-file line limit
//! (`docs/architecture/source-map.md`).

use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;

use super::*;

struct TrackingAllocator;

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method below forwards unchanged to `std::alloc::System`,
// a valid `GlobalAlloc` implementation; this wrapper only adds
// allocation counting around the delegated calls.
unsafe impl GlobalAlloc for TrackingAllocator {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwards `layout` unchanged to `System::alloc`.
        unsafe { System.alloc(layout) }
    }

    unsafe fn alloc_zeroed(&self, layout: Layout) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwards `layout` unchanged to `System::alloc_zeroed`.
        unsafe { System.alloc_zeroed(layout) }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        TRACK_ALLOCATIONS.with(|enabled| {
            if enabled.get() {
                ALLOCATIONS.with(|count| count.set(count.get() + 1));
            }
        });
        // SAFETY: forwards `ptr`, `layout`, and `new_size` unchanged to
        // `System::realloc`, which has the same safety contract as this
        // method.
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
    let allocations = ALLOCATIONS.with(Cell::get);
    (result, allocations)
}

#[test]
fn execute_into_is_allocation_free_once_warmed_single_stage() {
    let memory = FakeMemory::new(vec![(0x1000, vec![1, 2, 3, 4, 5, 6, 7, 8])]);
    let submit = ReadPlanSubmit {
        plan: 200,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 8)],
        }],
        sync: false,
    };
    let message = Message::ReadPlanSubmit(submit);
    let payload = message.encode_payload().unwrap();
    let deadline = far_deadline();
    let mut out = Vec::new();

    // Warm: grow `out` to its steady-state capacity and warm the
    // thread-local allocator-tracking state, both outside the
    // measured region.
    execute_into(
        &payload,
        0,
        &memory,
        ProcessBitness::X64,
        &NoModules,
        deadline,
        &mut out,
    )
    .unwrap();
    let _ = allocations_during(|| ());

    let (_, allocations) = allocations_during(|| {
        execute_into(
            black_box(&payload),
            0,
            &memory,
            ProcessBitness::X64,
            &NoModules,
            black_box(deadline),
            black_box(&mut out),
        )
        .unwrap();
    });
    assert_eq!(out.len(), 16 + 24 + 8);
    assert_eq!(
        allocations, 0,
        "a warmed single-stage read plan must allocate nothing"
    );
}

#[test]
fn execute_into_is_allocation_free_once_warmed_dependent_stage() {
    // stage 0 reads an 8-byte pointer at 0x1000 pointing at 0x3000;
    // stage 1 reads 4 bytes at 0x3000 + 8.
    let mut pointer_bytes = 0x3000u64.to_le_bytes().to_vec();
    pointer_bytes.extend([0u8; 8]);
    let memory = FakeMemory::new(vec![
        (0x1000, pointer_bytes),
        (0x3000, vec![0; 8].into_iter().chain([9, 9, 9, 9]).collect()),
    ]);
    let submit = ReadPlanSubmit {
        plan: 201,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![
            Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops: vec![op(0, 8)],
            },
            Stage {
                base: StageBase::Dependent {
                    ref_stage: 0,
                    ref_op: 0,
                },
                base_offset: 0,
                ops: vec![op(8, 4)],
            },
        ],
        sync: false,
    };
    let message = Message::ReadPlanSubmit(submit);
    let payload = message.encode_payload().unwrap();
    let deadline = far_deadline();
    let mut out = Vec::new();

    execute_into(
        &payload,
        0,
        &memory,
        ProcessBitness::X64,
        &NoModules,
        deadline,
        &mut out,
    )
    .unwrap();
    let _ = allocations_during(|| ());

    let (_, allocations) = allocations_during(|| {
        execute_into(
            black_box(&payload),
            0,
            &memory,
            ProcessBitness::X64,
            &NoModules,
            black_box(deadline),
            black_box(&mut out),
        )
        .unwrap();
    });
    assert_eq!(out.len(), 16 + 24 * 2 + 8 + 4);
    assert_eq!(
        allocations, 0,
        "a warmed dependent-stage read plan must allocate nothing"
    );
}
