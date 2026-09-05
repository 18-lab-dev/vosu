//! Deterministic stable/lazer decoder microbenchmark.
//!
//! Run with `./tools/autoresearch/memory-reader-bench.sh`. The repository-root
//! `autoresearch.sh` is reserved for the canonical CN_PROC/eBPF process-event
//! experiment.
//!
//! The primary timing is a successful native self-process lazer precise capture
//! with 18 key inputs. The stable timings use successful x86 object graphs over
//! local slice copies and therefore isolate decoder work rather than native
//! cross-process syscall cost. Emitted metrics include backend call and requested
//! byte counts so latency changes cannot hide additional reads.

use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use vosu::memory::lazer_offsets::LazerOffsets;
use vosu::memory::{
    LazerMemory, LazerPattern, MemoryReader, ReaderSynchronization, StableMemory, StablePatterns,
    SynchronizedMemoryReader,
};
use vosu::process::{
    MemoryError, MemoryRegion, ProcessBitness, ProcessHandle, ProcessMemory, ReadRequest,
    ReadStatus,
};

const FIXTURE_SIZE: usize = 1024 * 1024;
const LAZER_WARMUP_ITERATIONS: usize = 2_000;
const LAZER_SAMPLE_ITERATIONS: usize = 1_000;
const STABLE_WARMUP_ITERATIONS: usize = 20_000;
const STABLE_SAMPLE_ITERATIONS: usize = 20_000;
const SAMPLE_COUNT: usize = 9;
const KEY_COUNT: usize = 18;

struct Fixture {
    bytes: Arc<[u8]>,
    origin: usize,
    game_base: usize,
    player: usize,
}

impl Fixture {
    fn active_lazer(offsets: &LazerOffsets) -> Self {
        let mut bytes: Arc<[u8]> = vec![0; FIXTURE_SIZE].into();
        let origin = bytes.as_ptr() as usize;
        let game_base = origin + 0x10_000;
        let player = origin + 0x20_000;
        let api = origin + 0x30_000;
        let spectator_client = origin + 0x40_000;
        let clock = origin + 0x50_000;
        let source = origin + 0x60_000;
        let drawable = origin + 0x70_000;
        let replay_score = origin + 0x71_000;
        let overlay = origin + 0x72_000;
        let controller = origin + 0x73_000;
        let trigger_bindable = origin + 0x74_000;
        let trigger_list = origin + 0x75_000;
        let trigger_array = origin + 0x76_000;
        let processor = origin + 0x77_000;
        let hit_list = origin + 0x78_000;
        let hit_array = origin + 0x79_000;
        let vtable = origin + 0x7a_000;

        let data = Arc::get_mut(&mut bytes).expect("new fixture has one owner");
        let mut writer = FixtureWriter { origin, data };

        writer.pointer(game_base, vtable);
        writer.i64(vtable, offsets.effective_game_base_vtable() as i64);
        writer.pointer(
            game_base + offsets.osu_game_osu_game_base.api_k_backing_field,
            api,
        );
        writer.pointer(
            game_base
                + offsets
                    .osu_game_osu_game_base
                    .spectator_client_k_backing_field,
            spectator_client,
        );

        let submitting = &offsets.osu_game_screens_play_submitting_player;
        writer.pointer(player + submitting.api_k_backing_field, api);
        writer.pointer(
            player + submitting.spectator_client_k_backing_field,
            spectator_client,
        );
        writer.pointer(player + 0x3f0, 0);
        writer.pointer(
            player
                + offsets
                    .osu_game_screens_play_player
                    .drawable_ruleset_k_backing_field,
            drawable,
        );
        writer.pointer(
            drawable
                + offsets
                    .osu_game_rulesets_ui_drawable_ruleset
                    .replay_score_k_backing_field,
            replay_score,
        );

        writer.pointer(
            game_base + offsets.osu_game_osu_game_base.beatmap_clock,
            clock,
        );
        writer.pointer(
            clock
                + offsets
                    .osu_game_beatmaps_framed_beatmap_clock
                    .decoupled_track,
            0,
        );
        writer.pointer(
            clock
                + offsets
                    .osu_game_beatmaps_framed_beatmap_clock
                    .final_clock_source,
            source,
        );
        writer.f64(
            source
                + offsets
                    .osu_framework_timing_framed_clock
                    .current_time_k_backing_field,
            321.5,
        );

        writer.pointer(
            player
                + offsets
                    .osu_game_screens_play_player
                    .hudoverlay_k_backing_field,
            overlay,
        );
        writer.pointer(
            overlay
                + offsets
                    .osu_game_screens_play_hudoverlay
                    .input_count_controller,
            controller,
        );
        writer.pointer(
            controller
                + offsets
                    .osu_game_screens_play_hud_input_count_controller
                    .triggers,
            trigger_bindable,
        );
        writer.pointer(trigger_bindable + 0x18, trigger_list);
        writer.i32(trigger_list + 0x10, KEY_COUNT as i32);
        writer.pointer(trigger_list + 8, trigger_array);
        for index in 0..KEY_COUNT {
            let trigger = origin + 0x80_000 + index * 0x400;
            let count_bindable = trigger + 0x280;
            writer.pointer(trigger_array + 0x10 + index * 8, trigger);
            writer.pointer(trigger + 0x208, count_bindable);
            writer.i32(count_bindable + 0x40, (index * 10) as i32);
            writer.u8(trigger + 0x1f4, (index % 2) as u8);
        }

        writer.pointer(
            player
                + offsets
                    .osu_game_screens_play_player
                    .score_processor_k_backing_field,
            processor,
        );
        writer.pointer(
            processor + offsets.osu_game_rulesets_scoring_score_processor.hit_events,
            hit_list,
        );
        writer.i32(hit_list + 0x10, 0);
        writer.pointer(hit_list + 8, hit_array);

        Self {
            bytes,
            origin,
            game_base,
            player,
        }
    }
}

struct FixtureWriter<'a> {
    origin: usize,
    data: &'a mut [u8],
}

impl FixtureWriter<'_> {
    fn put(&mut self, address: usize, value: &[u8]) {
        let start = address
            .checked_sub(self.origin)
            .expect("fixture address precedes allocation");
        let end = start
            .checked_add(value.len())
            .expect("fixture write address overflow");
        self.data
            .get_mut(start..end)
            .expect("fixture write exceeds allocation")
            .copy_from_slice(value);
    }

    fn pointer(&mut self, address: usize, value: usize) {
        self.put(address, &value.to_le_bytes());
    }

    fn i32(&mut self, address: usize, value: i32) {
        self.put(address, &value.to_le_bytes());
    }

    fn i64(&mut self, address: usize, value: i64) {
        self.put(address, &value.to_le_bytes());
    }

    fn f64(&mut self, address: usize, value: f64) {
        self.put(address, &value.to_le_bytes());
    }

    fn u8(&mut self, address: usize, value: u8) {
        self.put(address, &[value]);
    }
}

struct SliceMemory {
    base: usize,
    bytes: Arc<[u8]>,
}

impl SliceMemory {
    fn copy(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let start = address
            .checked_sub(self.base)
            .ok_or_else(|| memory_error("address before fixture"))?;
        let end = start
            .checked_add(dst.len())
            .ok_or_else(|| memory_error("address overflow"))?;
        let source = self
            .bytes
            .get(start..end)
            .ok_or_else(|| memory_error("address outside fixture"))?;
        dst.copy_from_slice(source);
        Ok(())
    }
}

impl ProcessMemory for SliceMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.copy(address, dst)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
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

struct CountingMemory {
    inner: SliceMemory,
    exact_reads: AtomicU64,
    batch_reads: AtomicU64,
    read_bytes: AtomicU64,
}

impl CountingMemory {
    fn new(fixture: &Fixture) -> Self {
        Self::from_parts(fixture.origin, fixture.bytes.clone())
    }

    fn from_parts(base: usize, bytes: Arc<[u8]>) -> Self {
        Self {
            inner: SliceMemory { base, bytes },
            exact_reads: AtomicU64::new(0),
            batch_reads: AtomicU64::new(0),
            read_bytes: AtomicU64::new(0),
        }
    }

    fn reset_counts(&self) {
        self.exact_reads.store(0, Ordering::Relaxed);
        self.batch_reads.store(0, Ordering::Relaxed);
        self.read_bytes.store(0, Ordering::Relaxed);
    }

    fn counts(&self) -> (u64, u64, u64) {
        (
            self.exact_reads.load(Ordering::Relaxed),
            self.batch_reads.load(Ordering::Relaxed),
            self.read_bytes.load(Ordering::Relaxed),
        )
    }
}

impl ProcessMemory for CountingMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.exact_reads.fetch_add(1, Ordering::Relaxed);
        self.read_bytes
            .fetch_add(dst.len() as u64, Ordering::Relaxed);
        self.inner.copy(address, dst)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.batch_reads.fetch_add(1, Ordering::Relaxed);
        self.read_bytes.fetch_add(
            reads.iter().map(|read| read.dst.len() as u64).sum(),
            Ordering::Relaxed,
        );
        for read in reads {
            self.inner.copy(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        self.inner.readable_regions()
    }
}

fn memory_error(message: &'static str) -> MemoryError {
    MemoryError::Backend {
        operation: "memory reader benchmark fixture",
        message: message.into(),
    }
}

fn lazer_reader(process: ProcessHandle, offsets: Arc<LazerOffsets>) -> LazerMemory {
    LazerMemory::new(process, offsets, LazerPattern::source_default())
}

fn precise_cycle(reader: &mut LazerMemory, synchronization: ReaderSynchronization) {
    reader.synchronize_precise(synchronization, 3);
    let global = reader.global_precise().expect("read precise global state");
    let keys = reader.key_overlay(3).expect("read key overlay");
    let hit_errors = reader.hit_errors(0).expect("read hit errors");
    match (&global, &keys, &hit_errors) {
        (ReadStatus::Ready(global), ReadStatus::Ready(keys), ReadStatus::Ready(hit_errors)) => {
            assert_eq!(global.time, 322.0);
            assert_eq!(keys.len(), KEY_COUNT);
            assert_eq!(hit_errors.index, 0);
        }
        _ => panic!("active precise fixture was not ready"),
    }
    black_box((global, keys, hit_errors));
}

fn stable_patterns() -> StablePatterns {
    StablePatterns {
        base_addr: 0x100,
        play_time_addr: 0x100,
        chat_checker_ptr: 0x100,
        skin_data_addr: 0x100,
        settings_class_addr: 0x100,
        configuration_addr: 0x100,
        bindings_addr: 0x100,
        rulesets_addr: 0x100,
        can_run_slowly_addr: 0x100,
        status_ptr: 0x100,
        menu_mods_ptr: 0x100,
        get_audio_length_ptr: 0x100,
        user_profile_ptr: 0x100,
        raw_login_status_ptr: 0x100,
        spectating_user_ptr: 0x100,
        game_time_ptr: 0x100,
    }
}

fn stable_idle_reader() -> StableMemory {
    let bytes: Arc<[u8]> = vec![0; 64 * 1024].into();
    StableMemory::new(
        ProcessHandle::new(
            Arc::new(SliceMemory { base: 0, bytes }),
            ProcessBitness::X86,
        ),
        stable_patterns(),
    )
}

fn stable_idle_cycle(reader: &mut StableMemory) {
    reader.synchronize_capture_config(false, true);
    let global = reader.global().expect("read stable global state");
    let menu = reader.menu("").expect("read stable menu state");
    let gameplay = reader
        .gameplay(0.0, None)
        .expect("read stable gameplay state");
    assert!(matches!(global, ReadStatus::Ready(_)));
    assert!(matches!(menu, ReadStatus::NotReady));
    assert!(matches!(gameplay, ReadStatus::NotReady));
    black_box((global, menu, gameplay));
}

fn put_u32(bytes: &mut [u8], address: usize, value: u32) {
    bytes[address..address + 4].copy_from_slice(&value.to_le_bytes());
}

fn put_i32(bytes: &mut [u8], address: usize, value: i32) {
    bytes[address..address + 4].copy_from_slice(&value.to_le_bytes());
}

fn stable_key_fixture() -> Arc<[u8]> {
    let mut bytes = vec![0; 0x4000];
    let ruleset_anchor = 0x1200usize;
    let ruleset = 0x1300usize;
    let overlay = 0x1400usize;
    let nested = 0x1500usize;
    let array = 0x1600usize;
    put_u32(
        &mut bytes,
        stable_patterns().rulesets_addr - 0xb,
        ruleset_anchor as u32,
    );
    put_u32(&mut bytes, ruleset_anchor + 4, ruleset as u32);
    put_u32(&mut bytes, ruleset + 0xac, overlay as u32);
    put_u32(&mut bytes, overlay + 0x10, nested as u32);
    put_u32(&mut bytes, nested + 4, array as u32);
    put_i32(&mut bytes, array + 4, KEY_COUNT as i32);
    for index in 0..KEY_COUNT {
        let button = 0x1800 + index * 0x40;
        put_u32(&mut bytes, array + 8 + index * 4, button as u32);
        put_i32(&mut bytes, button + 0x14, index as i32 + 10);
        bytes[button + 0x1c] = u8::from(index % 2 == 0);
    }
    bytes.into()
}

fn stable_key_reader(memory: Arc<dyn ProcessMemory>) -> StableMemory {
    StableMemory::new(
        ProcessHandle::new(memory, ProcessBitness::X86),
        stable_patterns(),
    )
}

fn stable_key_cycle(reader: &mut StableMemory) {
    reader.synchronize_precise(ReaderSynchronization::default(), 3);
    let keys = reader.key_overlay(3).expect("read stable key overlay");
    assert!(matches!(&keys, ReadStatus::Ready(keys) if keys.len() == KEY_COUNT));
    black_box(keys);
}

fn measure<const WARMUP: usize, const ITERATIONS: usize>(mut workload: impl FnMut()) -> f64 {
    for _ in 0..WARMUP {
        workload();
    }

    let mut samples = [0_u128; SAMPLE_COUNT];
    for sample in &mut samples {
        let start = Instant::now();
        for _ in 0..ITERATIONS {
            workload();
        }
        *sample = start.elapsed().as_nanos();
    }
    samples.sort_unstable();
    samples[SAMPLE_COUNT / 2] as f64 / ITERATIONS as f64
}

fn main() {
    let offsets = Arc::new(LazerOffsets::embedded().expect("load embedded lazer offsets"));
    let fixture = Fixture::active_lazer(&offsets);
    let synchronization = ReaderSynchronization {
        game_base: Some(fixture.game_base),
        current_screen: Some(fixture.player),
    };

    #[cfg(target_os = "windows")]
    let native_process = vosu::process::open_process(std::process::id())
        .expect("open benchmark process for native memory reads");
    #[cfg(not(target_os = "windows"))]
    let native_process = ProcessHandle::new(
        Arc::new(
            vosu::process::open_process(std::process::id())
                .expect("open benchmark process for native memory reads"),
        ),
        ProcessBitness::X64,
    );
    let mut native_reader = lazer_reader(native_process, offsets.clone());
    precise_cycle(&mut native_reader, synchronization);
    let memory_reader_ns = measure::<LAZER_WARMUP_ITERATIONS, LAZER_SAMPLE_ITERATIONS>(|| {
        precise_cycle(&mut native_reader, synchronization)
    });

    let counting_memory = Arc::new(CountingMemory::new(&fixture));
    let mut counting_reader = lazer_reader(
        ProcessHandle::new(counting_memory.clone(), ProcessBitness::X64),
        offsets,
    );
    precise_cycle(&mut counting_reader, synchronization);
    let (exact_reads, batch_reads, read_bytes) = counting_memory.counts();

    let mut stable_reader = stable_idle_reader();
    stable_idle_cycle(&mut stable_reader);
    let stable_idle_ns = measure::<STABLE_WARMUP_ITERATIONS, STABLE_SAMPLE_ITERATIONS>(|| {
        stable_idle_cycle(&mut stable_reader)
    });

    let stable_key_bytes = stable_key_fixture();
    let mut stable_timing_reader = stable_key_reader(Arc::new(SliceMemory {
        base: 0,
        bytes: stable_key_bytes.clone(),
    }));
    stable_key_cycle(&mut stable_timing_reader);
    let stable_key_ns = measure::<STABLE_WARMUP_ITERATIONS, STABLE_SAMPLE_ITERATIONS>(|| {
        stable_key_cycle(&mut stable_timing_reader)
    });

    let stable_counting_memory = Arc::new(CountingMemory::from_parts(0, stable_key_bytes.clone()));
    let mut stable_counting_reader = stable_key_reader(stable_counting_memory.clone());
    stable_key_cycle(&mut stable_counting_reader);
    stable_counting_memory.reset_counts();
    stable_key_cycle(&mut stable_counting_reader);
    let (stable_exact_reads, stable_batch_reads, stable_read_bytes) =
        stable_counting_memory.counts();

    black_box(&fixture.bytes);
    println!("METRIC memory_reader_ns={memory_reader_ns:.2}");
    println!("METRIC stable_idle_ns={stable_idle_ns:.2}");
    println!("METRIC stable_key_overlay_ns={stable_key_ns:.2}");
    println!("METRIC exact_reads_per_cycle={exact_reads}");
    println!("METRIC batch_reads_per_cycle={batch_reads}");
    println!("METRIC process_bytes_per_cycle={read_bytes}");
    println!(
        "METRIC process_reads_per_cycle={}",
        exact_reads + batch_reads
    );
    println!("METRIC stable_exact_reads_per_cycle={stable_exact_reads}");
    println!("METRIC stable_batch_reads_per_cycle={stable_batch_reads}");
    println!(
        "METRIC stable_process_reads_per_cycle={}",
        stable_exact_reads + stable_batch_reads
    );
    println!("METRIC stable_process_bytes_per_cycle={stable_read_bytes}");
}
