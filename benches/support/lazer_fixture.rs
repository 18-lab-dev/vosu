//! Successful active lazer graph shared by decoder and PlanMemory benchmarks.
use std::sync::Arc;
use vosu_osu_reader::memory::lazer_offsets::LazerOffsets;
use vosu_sys::process::ProcessMemory;
const FIXTURE_SIZE: usize = 1024 * 1024;
const KEY_COUNT: usize = 18;
pub(super) struct Fixture {
    pub(super) bytes: Arc<[u8]>,
    pub(super) origin: usize,
    pub(super) game_base: usize,
    pub(super) player: usize,
}

impl Fixture {
    pub(super) fn active_lazer(offsets: &LazerOffsets) -> Self {
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

impl ProcessMemory for Fixture {
    fn read_exact(
        &self,
        address: usize,
        dst: &mut [u8],
    ) -> Result<(), vosu_sys::process::MemoryError> {
        use vosu_sys::process::MemoryError;
        let source = address
            .checked_sub(self.origin)
            .and_then(|start| {
                start
                    .checked_add(dst.len())
                    .and_then(|end| self.bytes.get(start..end))
            })
            .ok_or_else(|| MemoryError::Backend {
                operation: "fixture",
                message: "outside fixture".into(),
            })?;
        #[cfg(not(windows))]
        dst.copy_from_slice(source);
        #[cfg(windows)]
        native().read_exact(source.as_ptr() as usize, dst)?;
        Ok(())
    }
    fn read_many(
        &self,
        reads: &mut [vosu_sys::process::ReadRequest<'_>],
    ) -> Result<(), vosu_sys::process::MemoryError> {
        #[cfg(windows)]
        {
            native().read_many(reads)
        }
        #[cfg(not(windows))]
        {
            for read in reads {
                self.read_exact(read.address, read.dst)?;
            }
            Ok(())
        }
    }
    fn readable_regions(
        &self,
    ) -> Result<Vec<vosu_sys::process::MemoryRegion>, vosu_sys::process::MemoryError> {
        Ok(vec![vosu_sys::process::MemoryRegion {
            address: self.origin,
            size: self.bytes.len(),
        }])
    }
    fn reads_pin_process_identity(&self) -> bool {
        cfg!(windows)
    }
}

#[cfg(windows)]
fn native() -> &'static vosu_sys::process::WindowsProcessMemory {
    static PROCESS: std::sync::LazyLock<vosu_sys::process::WindowsProcessMemory> =
        std::sync::LazyLock::new(|| {
            vosu_sys::process::WindowsProcessMemory::open(std::process::id()).unwrap()
        });
    &PROCESS
}
