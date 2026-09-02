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
}

impl CountingMemory {
    fn new(fixture: &Fixture) -> Self {
        Self {
            inner: SliceMemory {
                base: fixture.origin,
                bytes: fixture.bytes.clone(),
            },
            exact_reads: AtomicU64::new(0),
            batch_reads: AtomicU64::new(0),
        }
    }

    fn counts(&self) -> (u64, u64) {
        (
            self.exact_reads.load(Ordering::Relaxed),
            self.batch_reads.load(Ordering::Relaxed),
        )
    }
}

impl ProcessMemory for CountingMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.exact_reads.fetch_add(1, Ordering::Relaxed);
        self.inner.copy(address, dst)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.batch_reads.fetch_add(1, Ordering::Relaxed);
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

    let native_memory = vosu::process::open_process(std::process::id())
        .expect("open benchmark process for native memory reads");
    let mut native_reader = lazer_reader(
        ProcessHandle::new(Arc::new(native_memory), ProcessBitness::X64),
        offsets.clone(),
    );
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
    let (exact_reads, batch_reads) = counting_memory.counts();

    let mut stable_reader = stable_idle_reader();
    stable_idle_cycle(&mut stable_reader);
    let stable_idle_ns = measure::<STABLE_WARMUP_ITERATIONS, STABLE_SAMPLE_ITERATIONS>(|| {
        stable_idle_cycle(&mut stable_reader)
    });

    black_box(&fixture.bytes);
    println!("METRIC memory_reader_ns={memory_reader_ns:.2}");
    println!("METRIC stable_idle_ns={stable_idle_ns:.2}");
    println!("METRIC exact_reads_per_cycle={exact_reads}");
    println!("METRIC batch_reads_per_cycle={batch_reads}");
    println!(
        "METRIC process_reads_per_cycle={}",
        exact_reads + batch_reads
    );
}
