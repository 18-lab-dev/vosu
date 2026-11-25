use std::alloc::{GlobalAlloc, Layout, System};
use std::cell::Cell;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;
use vosu_adapter_support::filtered::Filter;
use vosu_proptest::bounded;

use bytes::Bytes;
use tokio::sync::watch;
use vosu::server::websocket::{Outbound, Schema, StateProvider, StateSnapshot, WebSocketHub};
use vosu_osu_calc::beatmap::{
    BeatmapCache, BeatmapEngine, BeatmapError, BeatmapInput, EngineOutput, EngineRequest, ModSet,
    Ruleset, ScoringMode,
};
use vosu_osu_reader::memory::lazer_offsets::LazerOffsets;
use vosu_osu_reader::memory::{
    LazerMemory, LazerPattern, MemoryReader, StableMemory, StablePatterns,
};
use vosu_osu_reader::process::ProcessHandle;
use vosu_plugin_api::memory::{
    MemoryError, MemoryRegion, Pattern, ProcessBitness, ProcessMemory, ReadRequest,
};
use vosu_sys::process::scanner::SCAN_CHUNK_SIZE;

struct TrackingAllocator;

thread_local! {
    static TRACK_ALLOCATIONS: Cell<bool> = const { Cell::new(false) };
    static ALLOCATIONS: Cell<usize> = const { Cell::new(0) };
}

// SAFETY: every method below forwards unchanged to `std::alloc::System`, a
// valid `GlobalAlloc` implementation; this wrapper only adds allocation
// counting around the delegated calls.
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

#[derive(Clone)]
struct FixtureMemory {
    base: usize,
    bytes: Arc<[u8]>,
    max_read: Arc<AtomicUsize>,
}

impl ProcessMemory for FixtureMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.max_read.fetch_max(dst.len(), Ordering::Relaxed);
        let start = address
            .checked_sub(self.base)
            .ok_or_else(|| fixture_error("before fixture"))?;
        let end = start
            .checked_add(dst.len())
            .ok_or_else(|| fixture_error("overflow"))?;
        let source = self
            .bytes
            .get(start..end)
            .ok_or_else(|| fixture_error("outside fixture"))?;
        dst.copy_from_slice(source);
        Ok(())
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
struct BatchMemory {
    bytes: Arc<[u8]>,
    batch_calls: AtomicUsize,
    batch_requests: AtomicUsize,
    mergeable_batch_calls: AtomicUsize,
    forbidden_exact_calls: AtomicUsize,
    forbidden_exact_ranges: Arc<[(usize, usize)]>,
}

impl BatchMemory {
    fn copy(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let end = address
            .checked_add(dst.len())
            .ok_or_else(|| fixture_error("overflow"))?;
        let source = self
            .bytes
            .get(address..end)
            .ok_or_else(|| fixture_error("outside fixture"))?;
        dst.copy_from_slice(source);
        Ok(())
    }

    fn is_windows_mergeable_batch(reads: &[ReadRequest<'_>]) -> bool {
        let Some(first) = reads.first() else {
            return false;
        };
        if reads.len() < 2 || first.dst.is_empty() {
            return false;
        }
        let group_address = first.address;
        let page_end = (group_address & !0xfff) | 0xfff;
        let merge_end = page_end.min(group_address.saturating_add(1023));
        let Some(mut span_end) = group_address.checked_add(first.dst.len()) else {
            return false;
        };
        if span_end - 1 > merge_end {
            return false;
        }
        for request in &reads[1..] {
            if request.dst.is_empty() {
                return false;
            }
            let Some(request_end) = request.address.checked_add(request.dst.len()) else {
                return false;
            };
            if request.address < span_end || request_end - 1 > merge_end {
                return false;
            }
            span_end = request_end;
        }
        true
    }
}

impl ProcessMemory for BatchMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let end = address.saturating_add(dst.len());
        if self.forbidden_exact_ranges.iter().any(|&(start, len)| {
            let forbidden_end = start.saturating_add(len);
            address < forbidden_end && start < end
        }) {
            self.forbidden_exact_calls.fetch_add(1, Ordering::Relaxed);
        }
        self.copy(address, dst)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.batch_calls.fetch_add(1, Ordering::Relaxed);
        self.batch_requests
            .fetch_add(reads.len(), Ordering::Relaxed);
        if Self::is_windows_mergeable_batch(reads) {
            self.mergeable_batch_calls.fetch_add(1, Ordering::Relaxed);
        }
        for read in reads {
            self.copy(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0,
            size: self.bytes.len(),
        }])
    }
}

fn fixture_error(message: &'static str) -> MemoryError {
    MemoryError::Backend {
        operation: "performance fixture",
        message: message.into(),
    }
}

#[path = "../benches/support/region_walk_scans.rs"]
mod region_walk_scans;

fn fixture_handle(bytes: Vec<u8>, max_read: Arc<AtomicUsize>) -> ProcessHandle {
    region_walk_scans::scanning_handle(
        Arc::new(FixtureMemory {
            base: 0x1000,
            bytes: bytes.into(),
            max_read,
        }),
        ProcessBitness::X64,
    )
}

#[test]
fn scalar_readers_allocate_nothing() {
    let max_read = Arc::new(AtomicUsize::new(0));
    let mut bytes = vec![0_u8; 64];
    bytes[0..8].copy_from_slice(&0x0123_4567_89AB_CDEF_u64.to_le_bytes());
    bytes[8..16].copy_from_slice(&42.25_f64.to_le_bytes());
    let handle = fixture_handle(bytes, max_read);

    // Warm thread-local allocator state before the measured region.
    let _ = allocations_during(|| ());
    let (values, allocations) = allocations_during(|| {
        black_box((
            handle.read_u8(0x1000).unwrap(),
            handle.read_i8(0x1000).unwrap(),
            handle.read_u16(0x1000).unwrap(),
            handle.read_i16(0x1000).unwrap(),
            handle.read_u32(0x1000).unwrap(),
            handle.read_i32(0x1000).unwrap(),
            handle.read_u64(0x1000).unwrap(),
            handle.read_i64(0x1000).unwrap(),
            handle.read_f32(0x1008).unwrap(),
            handle.read_f64(0x1008).unwrap(),
            handle.read_bool(0x1000).unwrap(),
        ))
    });

    assert_eq!(values.6, 0x0123_4567_89AB_CDEF);
    assert_eq!(
        allocations, 0,
        "fixed-width scalar decoding must remain stack-only"
    );
}

#[cfg(feature = "test-harness")]
#[path = "../examples/live_reader_bench/plan.rs"]
pub mod plan_attachment;

#[cfg(feature = "test-harness")]
#[test]
fn plan_memory_scalar_readers_allocate_nothing() {
    let pid = 910_600_101;
    struct InjectedSession(u32);
    impl Drop for InjectedSession {
        fn drop(&mut self) {
            vosu_core::plugin::test_support::clear_session_memory(self.0);
        }
    }
    let mut bytes = vec![0_u8; 64];
    bytes[0..8].copy_from_slice(&0x0123_4567_89AB_CDEF_u64.to_le_bytes());
    bytes[8..16].copy_from_slice(&42.25_f64.to_le_bytes());
    vosu_core::plugin::test_support::set_session_memory(
        pid,
        Arc::new(FixtureMemory {
            base: 0x1000,
            bytes: bytes.into(),
            max_read: Arc::new(AtomicUsize::new(0)),
        }),
        ProcessBitness::X64,
    );
    let _injected = InjectedSession(pid);
    let mut record = plan_attachment::native_record(std::process::id()).unwrap();
    record.pid = pid;
    let attachment = plan_attachment::Attachment::new(record, "vosu.test.plan.scalar").unwrap();
    let handle = &attachment.process;
    let read = || {
        black_box((
            handle.read_u8(0x1000).unwrap(),
            handle.read_i8(0x1000).unwrap(),
            handle.read_u16(0x1000).unwrap(),
            handle.read_i16(0x1000).unwrap(),
            handle.read_u32(0x1000).unwrap(),
            handle.read_i32(0x1000).unwrap(),
            handle.read_u64(0x1000).unwrap(),
            handle.read_i64(0x1000).unwrap(),
            handle.read_f32(0x1008).unwrap(),
            handle.read_f64(0x1008).unwrap(),
            handle.read_bool(0x1000).unwrap(),
        ))
    };
    read();
    let (values, allocations) = allocations_during(read);
    assert_eq!(values.6, 0x0123_4567_89AB_CDEF);
    assert_eq!(values.9, 42.25);
    assert_eq!(
        allocations, 0,
        "warmed scalar readers over the real host must remain stack-only"
    );
}

#[cfg(feature = "test-harness")]
#[path = "../benches/support/stable_fixture.rs"]
mod stable_fixture;

#[cfg(feature = "test-harness")]
#[test]
fn plan_memory_stable_capture_backend_call_count() {
    struct CountedMemory {
        bytes: Vec<u8>,
        calls: Arc<AtomicUsize>,
        ops: Arc<AtomicUsize>,
        health: Arc<AtomicUsize>,
    }
    impl CountedMemory {
        fn copy(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
            let source = address
                .checked_add(dst.len())
                .and_then(|end| self.bytes.get(address..end))
                .ok_or_else(|| fixture_error("outside stable fixture"))?;
            dst.copy_from_slice(source);
            Ok(())
        }
    }
    impl ProcessMemory for CountedMemory {
        fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.ops.fetch_add(1, Ordering::Relaxed);
            self.copy(address, dst)
        }
        fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            self.ops.fetch_add(reads.len(), Ordering::Relaxed);
            for read in reads {
                self.copy(read.address, read.dst)?;
            }
            Ok(())
        }
        fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
            panic!("absolute capture must not refresh process regions")
        }
        fn process_id(&self) -> Option<u32> {
            panic!("capture must not reopen or re-resolve the process")
        }
        fn is_alive(&self) -> bool {
            self.health.fetch_add(1, Ordering::Relaxed);
            true
        }
        fn reads_pin_process_identity(&self) -> bool {
            true
        }
    }
    let make_memory = || {
        let calls = Arc::new(AtomicUsize::new(0));
        let ops = Arc::new(AtomicUsize::new(0));
        let health = Arc::new(AtomicUsize::new(0));
        let memory = Arc::new(CountedMemory {
            bytes: stable_fixture::bytes(),
            calls: Arc::clone(&calls),
            ops: Arc::clone(&ops),
            health: Arc::clone(&health),
        });
        (memory, calls, ops, health)
    };
    let (native, native_calls, native_ops, native_health) = make_memory();
    let mut native = StableMemory::new(
        ProcessHandle::new(native, ProcessBitness::X86),
        stable_fixture::patterns(),
    );
    let (plan, plan_calls, plan_ops, plan_health) = make_memory();
    let pid = 910_600_103;
    vosu_core::plugin::test_support::set_session_memory(pid, plan, ProcessBitness::X86);
    struct Cleanup(u32);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            vosu_core::plugin::test_support::clear_session_memory(self.0);
        }
    }
    let _cleanup = Cleanup(pid);
    let mut record = plan_attachment::native_record(std::process::id()).unwrap();
    record.pid = pid;
    record.bitness = vosu_plugin_api::Bitness::X86;
    let attachment = plan_attachment::Attachment::new(record, "vosu.test.plan.counts").unwrap();
    let mut plan = StableMemory::new(attachment.process.clone(), stable_fixture::patterns());
    stable_fixture::assert_capture(&mut native);
    stable_fixture::assert_capture(&mut plan);
    for counter in [
        &native_calls,
        &native_ops,
        &native_health,
        &plan_calls,
        &plan_ops,
        &plan_health,
    ] {
        counter.store(0, Ordering::Relaxed);
    }
    let submissions_before = attachment.submissions.load(Ordering::Relaxed);
    assert_eq!(
        format!("{:?}", stable_fixture::capture(&mut native)),
        format!("{:?}", stable_fixture::capture(&mut plan))
    );
    let native_regular_calls = native_calls.load(Ordering::Relaxed);
    let plan_regular_calls = plan_calls.load(Ordering::Relaxed);
    let submissions = attachment.submissions.load(Ordering::Relaxed) - submissions_before;
    assert!(submissions > 0);
    assert!(
        submissions <= 13,
        "regular fixture plan budget: {submissions}"
    );
    eprintln!(
        "stable regular: native calls={native_regular_calls} ops={} health={}; plan calls={plan_regular_calls} ops={} health={}",
        native_ops.load(Ordering::Relaxed),
        native_health.load(Ordering::Relaxed),
        plan_ops.load(Ordering::Relaxed),
        plan_health.load(Ordering::Relaxed),
    );
    assert_eq!(native_regular_calls, 14);
    // The executor reads each dependency level of a plan in one native
    // batch (no longer one call per stage), so the plan path makes as many
    // backend calls as the native reader.
    assert_eq!(plan_regular_calls, 14);
    assert_eq!(native_ops.load(Ordering::Relaxed), 56);
    assert_eq!(plan_ops.load(Ordering::Relaxed), 56);
    assert_eq!(plan_health.load(Ordering::Relaxed), 0);
    for counter in [
        &native_calls,
        &native_ops,
        &native_health,
        &plan_calls,
        &plan_ops,
        &plan_health,
    ] {
        counter.store(0, Ordering::Relaxed);
    }
    let native_precise = native.global_precise().unwrap();
    let plan_precise = plan.global_precise().unwrap();
    assert!(matches!(
        native_precise,
        vosu_plugin_api::memory::ReadStatus::Ready(_)
    ));
    assert_eq!(format!("{native_precise:?}"), format!("{plan_precise:?}"));
    eprintln!(
        "stable precise (non-playing): native calls={} ops={} health={}; plan calls={} ops={} health={}",
        native_calls.load(Ordering::Relaxed),
        native_ops.load(Ordering::Relaxed),
        native_health.load(Ordering::Relaxed),
        plan_calls.load(Ordering::Relaxed),
        plan_ops.load(Ordering::Relaxed),
        plan_health.load(Ordering::Relaxed),
    );
    assert_eq!(
        plan_calls.load(Ordering::Relaxed),
        native_calls.load(Ordering::Relaxed)
    );
    assert_eq!(plan_health.load(Ordering::Relaxed), 0);
}

#[test]
fn scanner_scratch_is_bounded_to_one_mib_plus_overlap() {
    let pattern_len = 257;
    let overlap = pattern_len - 1;
    let mut bytes = vec![0_u8; SCAN_CHUNK_SIZE * 3 + 17];
    let start = SCAN_CHUNK_SIZE - 128;
    bytes[start..start + pattern_len].fill(0xA5);
    let max_read = Arc::new(AtomicUsize::new(0));
    let handle = fixture_handle(bytes, Arc::clone(&max_read));
    let signature = std::iter::repeat_n("A5", pattern_len)
        .collect::<Vec<_>>()
        .join(" ");
    let pattern = Pattern::parse(&signature, false).unwrap();

    assert_eq!(handle.scan_first(&pattern).unwrap(), Some(0x1000 + start));
    assert!(
        max_read.load(Ordering::Relaxed) <= SCAN_CHUNK_SIZE + overlap,
        "scanner must not allocate/read a whole region-sized scratch buffer"
    );
}
struct FailingMemory {
    reads: Arc<AtomicUsize>,
}

impl ProcessMemory for FailingMemory {
    fn read_exact(&self, _address: usize, _dst: &mut [u8]) -> Result<(), MemoryError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Err(fixture_error("intentional reader failure"))
    }

    fn read_many(&self, _reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        Err(fixture_error("intentional reader failure"))
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

fn stable_patterns() -> StablePatterns {
    StablePatterns {
        base_addr: 0x200,
        play_time_addr: 0x300,
        chat_checker_ptr: 0x400,
        skin_data_addr: 0x500,
        settings_class_addr: 0x600,
        configuration_addr: 0x700,
        bindings_addr: 0x800,
        rulesets_addr: 0x900,
        can_run_slowly_addr: 0xa00,
        status_ptr: 0xb00,
        menu_mods_ptr: 0xc00,
        get_audio_length_ptr: 0xd00,
        user_profile_ptr: 0xe00,
        raw_login_status_ptr: 0xf00,
        spectating_user_ptr: 0x1000,
        game_time_ptr: 0x1100,
    }
}

#[test]
fn stable_and_lazer_snapshot_reads_surface_backend_statuses() {
    let stable_reads = Arc::new(AtomicUsize::new(0));
    let stable_handle = ProcessHandle::new(
        Arc::new(FailingMemory {
            reads: Arc::clone(&stable_reads),
        }),
        ProcessBitness::X86,
    );
    let mut stable = StableMemory::new(stable_handle, stable_patterns());
    assert!(stable.global().is_err());
    assert!(stable_reads.load(Ordering::Relaxed) > 0);

    let lazer_reads = Arc::new(AtomicUsize::new(0));
    let lazer_handle = ProcessHandle::new(
        Arc::new(FailingMemory {
            reads: Arc::clone(&lazer_reads),
        }),
        ProcessBitness::X64,
    );
    let mut lazer = LazerMemory::new(
        lazer_handle,
        Arc::new(LazerOffsets::embedded().expect("embedded offsets")),
        LazerPattern::source_default(),
    );
    lazer.set_resolved_state(0x1000, 0x2000);
    assert!(lazer.global().is_err());
    assert!(lazer_reads.load(Ordering::Relaxed) > 0);
}
fn put_u32(bytes: &mut [u8], address: usize, value: usize) {
    bytes[address..address + 4].copy_from_slice(&(value as u32).to_le_bytes());
}

fn put_u64(bytes: &mut [u8], address: usize, value: usize) {
    bytes[address..address + 8].copy_from_slice(&(value as u64).to_le_bytes());
}

#[test]
fn stable_groups_snapshot_reads_while_lazer_audio_matches_source_reads() {
    let mut stable_bytes = vec![0_u8; 0x2000];
    put_u32(&mut stable_bytes, 0x8f5, 0x1000);
    put_u32(&mut stable_bytes, 0x1004, 0x1100);
    put_u32(&mut stable_bytes, 0x1164, 0x1200);
    put_u32(&mut stable_bytes, 0x1238, 0x1300);
    put_u32(&mut stable_bytes, 0x1240, 0x1400);
    put_u32(&mut stable_bytes, 0x1248, 0x1700);
    put_u32(&mut stable_bytes, 0x131c, 0x1500);
    put_u32(&mut stable_bytes, 0x1cd, 0x1600);
    for (index, value) in [1_i16, 2, 3, 4, 5, 6, 7].into_iter().enumerate() {
        stable_bytes[0x1388 + 2 * index..0x138a + 2 * index].copy_from_slice(&value.to_le_bytes());
    }
    stable_bytes[0x1368..0x136a].copy_from_slice(&8_i16.to_le_bytes());
    stable_bytes[0x141c..0x1424].copy_from_slice(&1.0_f64.to_le_bytes());
    stable_bytes[0x1414..0x141c].copy_from_slice(&0.5_f64.to_le_bytes());
    stable_bytes[0x170c..0x1714].copy_from_slice(&75.0_f64.to_le_bytes());
    let stable_memory = Arc::new(BatchMemory {
        bytes: stable_bytes.into(),
        batch_calls: AtomicUsize::new(0),
        batch_requests: AtomicUsize::new(0),
        mergeable_batch_calls: AtomicUsize::new(0),
        forbidden_exact_calls: AtomicUsize::new(0),
        forbidden_exact_ranges: Arc::from([(0x1388, 14), (0x1368, 2)]),
    });
    let mut stable = StableMemory::new(
        ProcessHandle::new(stable_memory.clone(), ProcessBitness::X86),
        stable_patterns(),
    );
    assert!(matches!(
        stable.gameplay(1_000.0, Some(1_000)).unwrap(),
        vosu_plugin_api::memory::ReadStatus::Ready(_)
    ));
    let batch_calls = stable_memory.batch_calls.load(Ordering::Relaxed);
    assert!(batch_calls > 0);
    assert_eq!(
        stable_memory.mergeable_batch_calls.load(Ordering::Relaxed),
        batch_calls
    );
    assert!(stable_memory.batch_requests.load(Ordering::Relaxed) > batch_calls);
    assert_eq!(
        stable_memory.forbidden_exact_calls.load(Ordering::Relaxed),
        0
    );

    let offsets = Arc::new(LazerOffsets::embedded().expect("embedded offsets"));
    let base = 0x1000;
    let logo = 0x2000;
    let visualizer = 0x3000;
    let array = 0x4000;
    let mut lazer_bytes = vec![0_u8; 0x5000];
    let vtable = base + 0x800;
    put_u64(&mut lazer_bytes, base, vtable);
    put_u64(
        &mut lazer_bytes,
        vtable,
        offsets.effective_game_base_vtable(),
    );
    put_u64(
        &mut lazer_bytes,
        base + offsets.osu_game_osu_game.osu_logo,
        logo,
    );
    put_u64(
        &mut lazer_bytes,
        logo + offsets.osu_game_screens_menu_osu_logo.visualizer,
        visualizer,
    );
    put_u64(
        &mut lazer_bytes,
        visualizer
            + offsets
                .osu_game_screens_menu_logo_visualisation
                .frequency_amplitudes,
        array,
    );
    for index in 0..40 {
        lazer_bytes[array + 0x10 + index * 4..array + 0x14 + index * 4]
            .copy_from_slice(&(index as f32).to_le_bytes());
    }
    let lazer_memory = Arc::new(BatchMemory {
        bytes: lazer_bytes.into(),
        batch_calls: AtomicUsize::new(0),
        batch_requests: AtomicUsize::new(0),
        mergeable_batch_calls: AtomicUsize::new(0),
        forbidden_exact_calls: AtomicUsize::new(0),
        forbidden_exact_ranges: Arc::from([(array + 0x10, 160)]),
    });
    let mut lazer = LazerMemory::new(
        ProcessHandle::new(lazer_memory.clone(), ProcessBitness::X64),
        offsets,
        LazerPattern::source_default(),
    );
    lazer.set_resolved_state(base, 0);
    let vosu_plugin_api::memory::ReadStatus::Ready(values) = lazer.audio_velocity_base().unwrap()
    else {
        panic!("lazer amplitudes were not ready");
    };
    assert_eq!((values.len(), values[39]), (40, 39.0));
    assert_eq!(lazer_memory.batch_calls.load(Ordering::Relaxed), 1);
    assert_eq!(lazer_memory.batch_requests.load(Ordering::Relaxed), 40);
    assert_eq!(
        lazer_memory.forbidden_exact_calls.load(Ordering::Relaxed),
        0
    );
}

#[derive(Default)]
struct CountingEngine {
    calculations: Arc<AtomicUsize>,
}

impl BeatmapEngine for CountingEngine {
    fn calculate(&self, request: EngineRequest<'_>) -> Result<EngineOutput, BeatmapError> {
        assert!(request.content.contains("[HitObjects]"));
        self.calculations.fetch_add(1, Ordering::Relaxed);
        Ok(EngineOutput::default())
    }
}

const MAP: &str = "osu file format v14\n\n[General]\nMode:0\n\n[Difficulty]\nHPDrainRate:5\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nSliderMultiplier:1.4\nSliderTickRate:1\n\n[TimingPoints]\n0,500,4,2,1,80,1,0\n\n[HitObjects]\n64,192,1000,1,0,0:0:0:0:\n";

#[test]
fn unchanged_map_key_reuses_parse_and_calculation() {
    let calculations = Arc::new(AtomicUsize::new(0));
    let engine = CountingEngine {
        calculations: Arc::clone(&calculations),
    };
    let mut cache = BeatmapCache::new(engine);
    let mods = ModSet::new("nomod", Vec::new(), 1.0).unwrap();
    let input = || BeatmapInput::Content {
        content: MAP,
        identity_path: None,
    };

    let first = cache
        .load(input(), Ruleset::Osu, &mods, ScoringMode::Lazer)
        .unwrap();
    let second = cache
        .load(input(), Ruleset::Osu, &mods, ScoringMode::Lazer)
        .unwrap();

    assert!(Arc::ptr_eq(&first, &second));
    assert_eq!(cache.parse_count(), 1);
    assert_eq!(calculations.load(Ordering::Relaxed), 1);
}

struct TestSnapshot {
    generation: u64,
    encodes: Arc<AtomicUsize>,
    filtered_encodes: Arc<AtomicUsize>,
}

impl StateSnapshot for TestSnapshot {
    fn generation(&self) -> u64 {
        self.generation
    }
    fn encode_unfiltered(&self) -> Result<Bytes, String> {
        self.encodes.fetch_add(1, Ordering::Relaxed);
        Ok(Bytes::from(self.generation.to_string()))
    }
    fn encode_filtered(&self, _filters: &[Filter]) -> Result<Bytes, String> {
        self.filtered_encodes.fetch_add(1, Ordering::Relaxed);
        Ok(Bytes::from(format!("filtered-{}", self.generation)))
    }
}

struct TestProvider {
    generation: AtomicU64,
    encodes: Arc<AtomicUsize>,
    filtered_encodes: Arc<AtomicUsize>,
    event: watch::Sender<u64>,
}

impl TestProvider {
    fn new(generation: u64) -> Self {
        let (event, _) = watch::channel(generation);
        Self {
            generation: AtomicU64::new(generation),
            encodes: Arc::new(AtomicUsize::new(0)),
            filtered_encodes: Arc::new(AtomicUsize::new(0)),
            event,
        }
    }

    fn publish(&self) -> u64 {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.event.send_replace(generation);
        generation
    }
}

impl StateProvider for TestProvider {
    fn poll_rate(&self, _schema: Schema) -> Duration {
        Duration::from_millis(20)
    }
    fn subscribe(&self, _schema: Schema) -> watch::Receiver<u64> {
        self.event.subscribe()
    }
    fn capture(&self, _schema: Schema) -> Option<Arc<dyn StateSnapshot>> {
        Some(Arc::new(TestSnapshot {
            generation: self.generation.load(Ordering::Acquire),
            encodes: Arc::clone(&self.encodes),
            filtered_encodes: Arc::clone(&self.filtered_encodes),
        }))
    }
}

async fn text(receiver: &mut vosu::server::websocket::OutboundReceiver) -> Bytes {
    match tokio::time::timeout(Duration::from_secs(2), receiver.recv())
        .await
        .unwrap()
    {
        Some(Outbound::Text(bytes)) => bytes,
        frame => panic!("expected text frame, got {frame:?}"),
    }
}

#[tokio::test]
async fn one_unfiltered_encoding_is_shared_per_generation() {
    let provider = Arc::new(TestProvider::new(17));
    let hub = WebSocketHub::new(Schema::V2, provider.clone());
    let mut clients = Vec::new();
    for _ in 0..100 {
        clients.push(hub.connect());
    }
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });

    let first = bounded!(text(&mut clients[0].1));
    assert_eq!(first, Bytes::from_static(b"17"));
    for (_, receiver) in &mut clients[1..] {
        let frame = bounded!(text(receiver));
        assert_eq!(frame, first);
        assert_eq!(
            frame.as_ptr(),
            first.as_ptr(),
            "plain clients must share the encoded Bytes allocation"
        );
    }
    assert_eq!(provider.encodes.load(Ordering::Relaxed), 1);

    hub.shutdown();
    bounded!(runner).unwrap();
}
#[tokio::test]
async fn filtered_clients_encode_independently_from_shared_plain_payload() {
    let provider = Arc::new(TestProvider::new(9));
    let hub = WebSocketHub::new(Schema::StreamCompanion, provider.clone());
    let (plain, mut plain_receiver) = hub.connect();
    let (filtered, mut filtered_receiver) = hub.connect();
    filtered.set_filters(vec![Filter::Field("menu".into())]);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });

    assert_eq!(
        bounded!(text(&mut plain_receiver)),
        Bytes::from_static(b"9")
    );
    assert_eq!(
        bounded!(text(&mut filtered_receiver)),
        Bytes::from_static(b"filtered-9")
    );
    assert_eq!(provider.encodes.load(Ordering::Relaxed), 1);
    assert_eq!(provider.filtered_encodes.load(Ordering::Relaxed), 1);

    plain.write_completed();
    filtered.write_completed();
    assert_eq!(provider.publish(), 10);
    assert_eq!(
        bounded!(text(&mut plain_receiver)),
        Bytes::from_static(b"10")
    );
    assert_eq!(
        bounded!(text(&mut filtered_receiver)),
        Bytes::from_static(b"filtered-10")
    );
    assert_eq!(provider.encodes.load(Ordering::Relaxed), 2);
    assert_eq!(provider.filtered_encodes.load(Ordering::Relaxed), 2);

    hub.shutdown();
    bounded!(runner).unwrap();
}

#[tokio::test]
async fn each_client_retains_at_most_one_in_flight_frame() {
    let provider = Arc::new(TestProvider::new(1));
    let hub = WebSocketHub::new(Schema::V1, provider.clone());
    let (connection, mut receiver) = hub.connect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });
    assert_eq!(bounded!(text(&mut receiver)), Bytes::from_static(b"1"));

    for _ in 0..32 {
        provider.publish();
    }
    tokio::task::yield_now().await;
    assert!(
        receiver.try_recv().is_none(),
        "pending writer must not acquire a second queued state frame"
    );

    connection.write_completed();
    assert_eq!(bounded!(text(&mut receiver)), Bytes::from_static(b"33"));
    assert!(receiver.try_recv().is_none());

    hub.shutdown();
    bounded!(runner).unwrap();
}

#[tokio::test]
async fn one_thousand_rapid_updates_coalesce_to_latest() {
    let provider = Arc::new(TestProvider::new(0));
    let hub = WebSocketHub::new(Schema::V2Precise, provider.clone());
    let (connection, mut receiver) = hub.connect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });
    assert_eq!(bounded!(text(&mut receiver)), Bytes::from_static(b"0"));

    let mut latest = 0;
    for _ in 0..1_000 {
        latest = provider.publish();
    }
    tokio::task::yield_now().await;
    assert!(receiver.try_recv().is_none());

    connection.write_completed();
    hub.wake();
    assert_eq!(
        bounded!(text(&mut receiver)),
        Bytes::from(latest.to_string())
    );
    assert_eq!(latest, 1_000);
    assert!(receiver.try_recv().is_none());

    hub.shutdown();
    bounded!(runner).unwrap();
}

#[path = "../benches/support/adapter/mod.rs"]
mod adapter;
#[path = "performance/adapter.rs"]
mod adapter_cost;
#[path = "adapter_fixture/mod.rs"]
mod adapter_fixture;
#[path = "performance/api_v1.rs"]
mod api_v1_cost;
