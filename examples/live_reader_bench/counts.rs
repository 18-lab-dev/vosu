use std::{alloc::{GlobalAlloc, Layout, System}, cell::Cell};
thread_local! { static ALLOCS: Cell<u64> = const { Cell::new(0) }; }
pub fn take() -> u64 { ALLOCS.with(|c| c.replace(0)) }
struct Counter;
#[global_allocator]
static ALLOCATOR: Counter = Counter;
// SAFETY: delegates every allocation and deallocation unchanged to System.
unsafe impl GlobalAlloc for Counter {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: caller supplies the valid layout required by System.
        unsafe { System.alloc(layout) }
    }
    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        // SAFETY: caller supplies the allocation and its matching layout.
        unsafe { System.dealloc(ptr, layout) }
    }
    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, size: usize) -> *mut u8 {
        let _ = ALLOCS.try_with(|c| c.set(c.get() + 1));
        // SAFETY: caller supplies the allocation, layout and valid new size.
        unsafe { System.realloc(ptr, layout, size) }
    }
}
