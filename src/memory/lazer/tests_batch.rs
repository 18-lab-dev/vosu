use super::test_support::*;
use super::*;

/// Forwards single reads and rejects every batch, so batched decoders always defer to their
/// sequential paths.
struct ScalarOnly(Arc<SparseMemory>);

impl ProcessMemory for ScalarOnly {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.0.read_exact(address, dst)
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        Err(MemoryError::Read {
            address: reads.first().map_or(0, |read| read.address),
            len: 0,
            message: "batches disabled".into(),
        })
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

fn decoder(memory: Arc<dyn ProcessMemory>) -> LazerMemory {
    decoder_with(memory, LazerOffsets::embedded().expect("embedded offsets"))
}

fn decoder_with(memory: Arc<dyn ProcessMemory>, offsets: LazerOffsets) -> LazerMemory {
    LazerMemory::new(
        ProcessHandle::new(memory, ProcessBitness::X64),
        Arc::new(offsets),
        LazerPattern {
            scaling_container_target_draw_size: Pattern::parse("AA", false).unwrap(),
            offset: 0,
        },
    )
}

#[test]
fn maximum_offsets_disable_spans_and_keep_checked_address_errors() {
    let embedded = LazerOffsets::embedded().unwrap();
    // A root and a screen field at the top of the address space: the root and screen spans
    // would overflow, so both fast paths must step aside for the checked per-field reads.
    let mut offsets = embedded.clone();
    offsets.osu_game_osu_game_base.realm = usize::MAX - 2;
    offsets
        .osu_game_screens_play_player
        .health_processor_k_backing_field = usize::MAX - 2;
    let decode = |batched: bool| {
        let (memory, base, _) = player_screen(&embedded);
        let memory = Arc::new(memory);
        let mut decoder = if batched {
            decoder_with(memory, offsets.clone())
        } else {
            decoder_with(Arc::new(ScalarOnly(memory)), offsets.clone())
        };
        decoder.set_resolved_state(base, 0);
        decoder.synchronize_capture_config(false, true);
        let global = decoder.global();
        let gameplay = decoder.gameplay(0.0, None);
        (format!("{global:?}"), gameplay)
    };
    let (global, gameplay) = decode(true);
    assert!(
        matches!(gameplay, Err(MemoryError::AddressOverflow { .. })),
        "{gameplay:?}"
    );
    let (sequential_global, sequential_gameplay) = decode(false);
    assert_eq!(global, sequential_global);
    assert_eq!(format!("{gameplay:?}"), format!("{sequential_gameplay:?}"));
}

/// A gameplay screen whose player has a replay drawable ruleset, a paused bindable, a HUD,
/// and a gameplay clock, inside a connected multiplayer room.
fn player_screen(offsets: &LazerOffsets) -> (SparseMemory, usize, usize) {
    let mut memory = SparseMemory::default();
    let mut next = 0x10_000;
    let mut object = |memory: &mut SparseMemory| {
        let address = next;
        next += 0x1000;
        memory.put(address, [0u8; 0x700]);
        address
    };
    let base = object(&mut memory);
    memory.valid_base(base, offsets);
    let [
        api,
        spectator,
        client,
        ruleset,
        info,
        stack,
        list,
        items,
        screen,
    ] = std::array::from_fn(|_| object(&mut memory));
    let [drawable, pause, hud, show, container, clock, decoupled] =
        std::array::from_fn(|_| object(&mut memory));
    let [
        connected,
        room,
        chat,
        state,
        storage,
        wrapped,
        path,
        beatmap_clock,
        beatmap_decoupled,
    ] = std::array::from_fn(|_| object(&mut memory));

    let game = &offsets.osu_game_osu_game;
    let game_base = &offsets.osu_game_osu_game_base;
    memory.ptr(base + game_base.api_k_backing_field, api);
    memory.ptr(base + game_base.spectator_client_k_backing_field, spectator);
    memory.ptr(base + game_base.multiplayer_client_k_backing_field, client);
    memory.ptr(base + offsets.osu_desktop_osu_game_desktop.ruleset, ruleset);
    memory.ptr(
        base + game
            .screen_stack
            .unwrap_or(game.screen_stack_k_backing_field),
        stack,
    );
    memory.ptr(base + game.chat_overlay, chat);
    memory.ptr(base + game_base.storage_k_backing_field, storage);
    memory.ptr(base + game_base.beatmap_clock, beatmap_clock);

    memory.ptr(ruleset + 0x20, info);
    memory.i32(
        info + offsets
            .osu_game_rulesets_ruleset_info
            .online_id_k_backing_field,
        1,
    );
    memory.ptr(
        stack + offsets.osu_framework_screens_screen_stack.stack,
        list,
    );
    memory.ptr(list + 8, items);
    memory.i32(list + 0x10, 1);
    memory.ptr(items + 0x10, screen);

    let player = &offsets.osu_game_screens_play_player;
    let submitting = &offsets.osu_game_screens_play_submitting_player;
    memory.ptr(screen + submitting.api_k_backing_field, api);
    memory.ptr(
        screen + submitting.spectator_client_k_backing_field,
        spectator,
    );
    memory.ptr(screen + player.api_k_backing_field, api);
    memory.ptr(screen + player.drawable_ruleset_k_backing_field, drawable);
    memory.ptr(screen + player.hudoverlay_k_backing_field, hud);
    let container_offset = player
        .gameplay_clock_container_k_backing_field
        .expect("gameplay clock container offset");
    memory.ptr(screen + container_offset, container);

    let ruleset_ui = &offsets.osu_game_rulesets_ui_drawable_ruleset;
    memory.ptr(drawable + ruleset_ui.replay_score_k_backing_field, 0x5000);
    memory.ptr(
        drawable + ruleset_ui.is_paused.expect("pause offset"),
        pause,
    );
    memory.put(pause + 0x40, [1]);
    memory.ptr(
        hud + offsets
            .osu_game_screens_play_hudoverlay
            .show_hud_k_backing_field,
        show,
    );
    memory.put(show + 0x40, [1]);
    let clock_group = offsets
        .osu_game_screens_play_gameplay_clock_container
        .as_ref()
        .expect("gameplay clock container group");
    memory.ptr(container + clock_group.gameplay_clock, clock);
    let decoupled_track = offsets
        .osu_game_beatmaps_framed_beatmap_clock
        .decoupled_track;
    memory.ptr(clock + decoupled_track, decoupled);
    memory.f64(decoupled + 0x20, 1234.6);
    memory.ptr(beatmap_clock + decoupled_track, beatmap_decoupled);
    memory.f64(beatmap_decoupled + 0x20, 99.0);

    memory.ptr(
        client
            + offsets
                .osu_game_online_multiplayer_online_multiplayer_client
                .is_connected_k_backing_field,
        connected,
    );
    memory.put(connected + 0x40, [1]);
    memory.ptr(
        client + offsets.osu_game_online_multiplayer_multiplayer_client.room,
        room,
    );
    memory.ptr(chat + offsets.osu_game_overlays_chat_overlay.state, state);
    memory.i32(state + 0x40, 2);
    memory.ptr(
        storage
            + offsets
                .osu_game_io_wrapped_storage
                .underlying_storage_k_backing_field,
        wrapped,
    );
    memory.string_ptr(
        wrapped
            + offsets
                .osu_framework_platform_storage
                .base_path_k_backing_field,
        path,
        "/games/osu",
    );
    (memory, base, screen)
}

#[test]
fn batched_global_matches_sequential_decoding_for_a_player_screen() {
    let offsets = LazerOffsets::embedded().unwrap();
    let decode = |batched: bool| {
        let (memory, base, _) = player_screen(&offsets);
        let memory = Arc::new(memory);
        let mut decoder = if batched {
            decoder(memory.clone())
        } else {
            decoder(Arc::new(ScalarOnly(memory.clone())))
        };
        decoder.set_resolved_state(base, 0);
        decoder.synchronize_capture_config(false, true);
        let global = decoder.global().expect("global");
        if batched {
            // Shared gamemode/screen levels plus three player-screen levels.
            assert!(
                memory.batches.load(Ordering::Relaxed) >= 5,
                "batched path not taken"
            );
        }
        let state = (
            decoder.selected_gamemode,
            decoder.current_screen,
            decoder.watching_replay,
            decoder.spectating,
            decoder.show_interface,
            decoder.status,
            decoder.player_loading,
            decoder.precise_player.is_some(),
        );
        (global, state)
    };
    let (batched, batched_state) = decode(true);
    let (sequential, sequential_state) = decode(false);
    let ReadStatus::Ready(global) = &batched else {
        panic!("player screen global not ready: {batched:?}");
    };
    assert_eq!(
        (
            global.status,
            global.play_time,
            global.paused,
            global.is_watching_replay,
            global.show_interface,
            global.chat_status,
            global.is_multi_spectating,
            global.memory_songs_folder.as_str(),
        ),
        (
            GameState::Play.value() as i64,
            1235.0,
            Some(true),
            true,
            true,
            2,
            false,
            "/games/osu/files",
        )
    );
    assert_eq!(batched, sequential);
    assert_eq!(batched_state, sequential_state);
}

/// Rejects the first batch containing one chosen read, then permits that read on fallback.
/// Single reads remain available, like a transient failure of the native batch operation.
struct FailBatchOnce {
    memory: Arc<SparseMemory>,
    address: usize,
    failed: std::sync::atomic::AtomicBool,
}

impl ProcessMemory for FailBatchOnce {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.memory.read_exact(address, dst)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        if reads.iter().any(|read| read.address == self.address)
            && !self.failed.swap(true, Ordering::Relaxed)
        {
            return Err(MemoryError::Read {
                address: self.address,
                len: 0,
                message: "transient carried read failure".into(),
            });
        }
        self.memory.read_many(reads)
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        self.memory.readable_regions()
    }
}

#[derive(Clone, Copy, Debug)]
enum CarriedBeatmapCase {
    PlayerHash,
    HeadWithoutPlayerHash,
    TransientHeadFailure,
}

fn assert_global_menu_mp3_equivalence(case: CarriedBeatmapCase) {
    const HASH: &str = "0123456789abcdef0123456789abcdef";
    const LENGTH: f64 = 234_567.6;
    let offsets = LazerOffsets::embedded().unwrap();
    let decode = |batched: bool| {
        let (mut memory, base, _) = player_screen(&offsets);
        let [bindable, working, info, set, hash, track] =
            [0x40_000, 0x41_000, 0x42_000, 0x43_000, 0x44_000, 0x45_000];
        for address in [bindable, working, info, set, hash, track] {
            memory.put(address, [0u8; 0x700]);
        }
        memory.ptr(
            base + offsets.osu_game_osu_game_base.beatmap_k_backing_field,
            bindable,
        );
        memory.ptr(bindable + 0x20, working);
        let working_offsets =
            &offsets.osu_game_beatmaps_working_beatmap_cache_beatmap_manager_working_beatmap;
        memory.ptr(working + working_offsets.beatmap_info, info);
        memory.ptr(working + working_offsets.beatmap_set_info, set);
        let beatmap = &offsets.osu_game_beatmaps_beatmap_info;
        let status_address = info + beatmap.status_int_k_backing_field;
        memory.i32(status_address, 1);
        memory.string_ptr(info + beatmap.md5_hash_k_backing_field, hash, HASH);
        // Resolve the existing fixture's beatmap clock instead of relying on its allocation
        // order; it is deliberately distinct from the player's gameplay clock.
        let memory = Arc::new(memory);
        let process = ProcessHandle::new(memory.clone(), ProcessBitness::X64);
        let beatmap_clock = process
            .read_pointer(base + offsets.osu_game_osu_game_base.beatmap_clock)
            .unwrap();
        let decoupled = process
            .read_pointer(
                beatmap_clock
                    + offsets
                        .osu_game_beatmaps_framed_beatmap_clock
                        .decoupled_track,
            )
            .unwrap();
        drop(process);
        let mut memory = Arc::try_unwrap(memory).ok().expect("fixture memory unique");
        memory.ptr(
            decoupled
                + offsets
                    .osu_framework_timing_decoupling_framed_clock
                    .source_k_backing_field,
            track,
        );
        let length_address = track + offsets.osu_framework_audio_track_track.length;
        memory.f64(length_address, LENGTH);
        let memory = Arc::new(memory);
        let failure = match case {
            CarriedBeatmapCase::PlayerHash => None,
            CarriedBeatmapCase::HeadWithoutPlayerHash => Some(hash + 8),
            CarriedBeatmapCase::TransientHeadFailure => Some(status_address),
        };
        let fault = failure.map(|address| {
            Arc::new(FailBatchOnce {
                memory: memory.clone(),
                address,
                failed: std::sync::atomic::AtomicBool::new(false),
            })
        });
        let process: Arc<dyn ProcessMemory> = if !batched {
            Arc::new(ScalarOnly(memory.clone()))
        } else if let Some(fault) = &fault {
            fault.clone()
        } else {
            memory.clone()
        };
        let mut decoder = decoder(process);
        decoder.set_resolved_state(base, 0);
        decoder.synchronize_capture_config(false, true);
        let global = decoder.global().expect("global");
        assert!(matches!(&global, ReadStatus::Ready(_)));
        if batched {
            if let Some(fault) = &fault {
                assert!(fault.failed.load(Ordering::Relaxed), "{case:?}");
            }
            match case {
                CarriedBeatmapCase::PlayerHash | CarriedBeatmapCase::HeadWithoutPlayerHash => {
                    let (head, gamemode, carried_hash) = decoder
                        .captured_beatmap
                        .as_ref()
                        .expect("carried beatmap head");
                    assert_eq!((head.info, head.status, head.hash), (info, 1, hash));
                    assert_eq!(*gamemode, 1);
                    assert_eq!(
                        carried_hash.as_deref(),
                        matches!(case, CarriedBeatmapCase::PlayerHash).then_some(HASH),
                    );
                }
                CarriedBeatmapCase::TransientHeadFailure => {
                    assert!(decoder.captured_beatmap.is_none());
                    assert_eq!(memory.read_count(status_address), 0);
                }
            }
            assert_eq!(
                decoder.captured_track_length,
                (!matches!(case, CarriedBeatmapCase::HeadWithoutPlayerHash)).then_some(LENGTH),
            );
        } else {
            assert!(decoder.captured_beatmap.is_none());
            assert!(decoder.captured_track_length.is_none());
        }
        let hash_reads = memory.read_count(hash + 8);
        let status_reads = memory.read_count(status_address);
        let length_reads = memory.read_count(length_address);
        let batches = memory.batches.load(Ordering::Relaxed);
        let menu = decoder.menu(HASH).expect("menu checksum");
        assert_eq!(
            menu,
            ReadStatus::Ready(Menu::Checksum(MenuChecksum {
                gamemode: 1,
                ranked_status: 4.0,
            })),
        );
        assert!(decoder.captured_beatmap.is_none());
        if batched {
            match case {
                CarriedBeatmapCase::PlayerHash => {
                    assert_eq!(memory.read_count(hash + 8), hash_reads);
                    assert_eq!(memory.read_count(status_address), status_reads);
                    assert_eq!(memory.batches.load(Ordering::Relaxed), batches);
                }
                CarriedBeatmapCase::HeadWithoutPlayerHash => {
                    assert!(memory.read_count(hash + 8) > hash_reads);
                    assert_eq!(memory.read_count(status_address), status_reads);
                }
                CarriedBeatmapCase::TransientHeadFailure => {
                    assert!(memory.read_count(status_address) > status_reads);
                    assert!(memory.batches.load(Ordering::Relaxed) > batches);
                }
            }
        }
        let length = decoder.mp3_length().expect("mp3 length");
        assert_eq!(length, ReadStatus::Ready(234_568.0));
        assert!(decoder.captured_track_length.is_none());
        if batched && !matches!(case, CarriedBeatmapCase::HeadWithoutPlayerHash) {
            assert_eq!(memory.read_count(length_address), length_reads);
        } else {
            assert!(memory.read_count(length_address) > length_reads);
        }
        (global, menu, length)
    };
    assert_eq!(decode(true), decode(false), "{case:?}");
}

#[test]
fn batched_global_carries_beatmap_head_player_hash_and_track_for_menu_and_mp3() {
    assert_global_menu_mp3_equivalence(CarriedBeatmapCase::PlayerHash);
}

#[test]
fn batched_global_carries_beatmap_head_when_player_hash_read_falls_back() {
    assert_global_menu_mp3_equivalence(CarriedBeatmapCase::HeadWithoutPlayerHash);
}

#[test]
fn transient_global_beatmap_head_failure_recovers_in_menu_with_carried_track() {
    assert_global_menu_mp3_equivalence(CarriedBeatmapCase::TransientHeadFailure);
}
