//! The batched full lazer `menu` read against the sequential decoder.
use super::menu_batch::BATCHED_MENU_FILES;
use super::test_support::*;
use super::*;

use parking_lot::Mutex;

const MD5: &str = "0123456789abcdef0123456789abcdef";
const FILE: &str = "aa0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcd";
const AUDIO: &str = "bb0123456789abcdef0123456789abcd";
const BACKGROUND: &str = "cc0123456789abcdef0123456789abcd";

/// Fixture memory that can disable batches, make one range unreadable, and log reads.
struct Probe {
    memory: Arc<SparseMemory>,
    scalar: bool,
    poisoned: Option<(usize, usize)>,
    log: Mutex<Vec<(usize, usize)>>,
    calls: AtomicUsize,
}

impl Probe {
    fn read(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.log.lock().push((address, dst.len()));
        let end = address + dst.len();
        if self
            .poisoned
            .is_some_and(|(start, stop)| address < stop && start < end)
        {
            return Err(MemoryError::Read {
                address,
                len: dst.len(),
                message: "unreadable fixture range".into(),
            });
        }
        self.memory.read_exact(address, dst)
    }
}

impl ProcessMemory for Probe {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        self.read(address, dst)
    }
    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        if self.scalar {
            return Err(MemoryError::Read {
                address: reads.first().map_or(0, |read| read.address),
                len: 0,
                message: "batches disabled".into(),
            });
        }
        reads
            .iter_mut()
            .try_for_each(|read| self.read(read.address, read.dst))
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

const BASE: usize = 0x10_0000;

fn fixture() -> Arc<SparseMemory> {
    let offsets = LazerOffsets::embedded().unwrap();
    let mut m = SparseMemory::default();
    m.put(BASE, [0u8; 0x1000]);
    m.valid_base(BASE, &offsets);
    put_beatmap(&mut m, BASE, 0x20_0000);
    Arc::new(m)
}

/// The selected ruleset and a full current beatmap with three files, allocated from `next`.
fn put_beatmap(m: &mut SparseMemory, base: usize, next: usize) {
    let offsets = LazerOffsets::embedded().unwrap();
    let next = std::cell::Cell::new(next);
    let alloc = |m: &mut SparseMemory| {
        let address = next.replace(next.get() + 0x1000);
        m.put(address, [0u8; 0x800]);
        address
    };
    let text = |m: &mut SparseMemory, field: usize, value: &str| {
        let object = alloc(m);
        m.string_ptr(field, object, value);
    };

    let bindable = alloc(m);
    m.ptr(
        base + offsets.osu_game_osu_game_base.beatmap_k_backing_field,
        bindable,
    );
    let ruleset = alloc(m);
    m.ptr(base + offsets.osu_desktop_osu_game_desktop.ruleset, ruleset);
    let ruleset_info = alloc(m);
    m.ptr(ruleset + 0x20, ruleset_info);
    m.i32(
        ruleset_info
            + offsets
                .osu_game_rulesets_ruleset_info
                .online_id_k_backing_field,
        3,
    );
    let working = alloc(m);
    m.ptr(bindable + 0x20, working);
    let w = &offsets.osu_game_beatmaps_working_beatmap_cache_beatmap_manager_working_beatmap;
    let (info, set) = (alloc(m), alloc(m));
    m.ptr(working + w.beatmap_info, info);
    m.ptr(working + w.beatmap_set_info, set);

    let b = &offsets.osu_game_beatmaps_beatmap_info;
    m.i32(info + b.status_int_k_backing_field, 4);
    text(m, info + b.md5_hash_k_backing_field, MD5);
    text(m, info + b.hash_k_backing_field, FILE);
    text(m, info + b.difficulty_name_k_backing_field, "Insane ☆");
    m.i32(info + b.online_id_k_backing_field, 777);
    m.i32(info + b.total_object_count_k_backing_field, 1234);
    m.i32(
        set + offsets
            .osu_game_beatmaps_beatmap_set_info
            .online_id_k_backing_field,
        888,
    );

    let (metadata, difficulty, author) = (alloc(m), alloc(m), alloc(m));
    m.ptr(info + b.metadata_k_backing_field, metadata);
    m.ptr(info + b.difficulty_k_backing_field, difficulty);
    let md = &offsets.osu_game_beatmaps_beatmap_metadata;
    m.ptr(metadata + md.author_k_backing_field, author);
    text(
        m,
        author + offsets.osu_game_models_realm_user.username_k_backing_field,
        "Mapper",
    );
    text(m, metadata + md.audio_file_k_backing_field, "audio.mp3");
    text(m, metadata + md.background_file_k_backing_field, "bg.png");
    text(m, metadata + md.artist_k_backing_field, "Artist");
    text(
        m,
        metadata + md.artist_unicode_k_backing_field,
        "アーティスト",
    );
    text(
        m,
        metadata + md.title_k_backing_field,
        &"Long title ".repeat(20),
    );
    text(m, metadata + md.title_unicode_k_backing_field, "");
    let d = &offsets.osu_game_beatmaps_beatmap_difficulty;
    for (offset, value) in [
        (d.approach_rate_k_backing_field, 9.5f32),
        (d.circle_size_k_backing_field, 4.0),
        (d.drain_rate_k_backing_field, 5.5),
        (d.overall_difficulty_k_backing_field, 8.25),
    ] {
        m.put(difficulty + offset, value.to_le_bytes());
    }

    let (list, array) = (alloc(m), alloc(m));
    m.ptr(set + 0x20, list);
    m.ptr(list + 8, array);
    let files = [
        ("audio.mp3", AUDIO),
        ("bg.png", BACKGROUND),
        ("map.osu", MD5),
    ];
    m.i32(list + 0x10, files.len() as i32);
    for (index, (name, hash)) in files.into_iter().enumerate() {
        let (entry, realm) = (alloc(m), alloc(m));
        m.ptr(array + 0x10 + index * 8, entry);
        m.ptr(entry + 0x18, realm);
        text(m, realm + 0x18, hash);
        text(m, entry + 0x20, name);
    }
}

fn probe(memory: &Arc<SparseMemory>, scalar: bool, poisoned: Option<(usize, usize)>) -> Arc<Probe> {
    Arc::new(Probe {
        memory: memory.clone(),
        scalar,
        poisoned,
        log: Mutex::new(Vec::new()),
        calls: AtomicUsize::new(0),
    })
}

fn menu(probe: &Arc<Probe>, previous: &str) -> String {
    let mut decoder = reader_with(probe.clone());
    decoder.set_resolved_state(BASE, 0);
    format!("{:?}", decoder.menu(previous))
}

fn reader_with(memory: Arc<dyn ProcessMemory>) -> LazerMemory {
    LazerMemory::new(
        ProcessHandle::new(memory, ProcessBitness::X64),
        Arc::new(LazerOffsets::embedded().expect("embedded offsets")),
        LazerPattern {
            scaling_container_target_draw_size: Pattern::parse("AA", false).unwrap(),
            offset: 0,
        },
    )
}

#[test]
fn batched_full_menu_matches_sequential_menu_under_every_read_failure() {
    let memory = fixture();
    let sequential = probe(&memory, true, None);
    let expected = menu(&sequential, "");
    assert!(expected.starts_with("Ok(Ready(Full("), "{expected}");
    // Resource paths use the platform separator; compare them as `Debug` escapes them.
    let path = |hash: &str| {
        let path = std::path::Path::new(&hash[..1]).join(&hash[..2]).join(hash);
        format!("{:?}", path.to_string_lossy())
            .trim_matches('"')
            .to_owned()
    };
    for value in [
        "Mapper".to_owned(),
        "アーティスト".to_owned(),
        "Insane ☆".to_owned(),
        path(AUDIO),
        path(BACKGROUND),
        path(FILE),
    ] {
        assert!(expected.contains(&value), "{value} missing from {expected}");
    }
    let batched = probe(&memory, false, None);
    assert_eq!(menu(&batched, ""), expected);
    // Equivalence alone would also hold if every batched attempt fell back.
    assert!(
        batched.calls.load(Ordering::Relaxed) * 3 < sequential.calls.load(Ordering::Relaxed),
        "batched {} vs sequential {}",
        batched.calls.load(Ordering::Relaxed),
        sequential.calls.load(Ordering::Relaxed)
    );

    let mut ranges = sequential
        .log
        .lock()
        .iter()
        .flat_map(|&(address, len)| {
            [
                (address, address + len),
                (address + len, address + len + 16),
            ]
        })
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    ranges.dedup();
    for range in ranges {
        assert_eq!(
            menu(&probe(&memory, false, Some(range)), ""),
            menu(&probe(&memory, true, Some(range)), ""),
            "unreadable range {range:#x?}"
        );
    }
}

#[test]
fn batched_full_menu_returns_the_checksum_for_an_unchanged_beatmap() {
    let memory = fixture();
    let mut decoder = reader_with(probe(&memory, false, None));
    decoder.set_resolved_state(BASE, 0);
    assert_eq!(
        decoder.menu_full_batched(MD5).map(Result::unwrap),
        Some(ReadStatus::Ready(Menu::Checksum(MenuChecksum {
            gamemode: 3,
            ranked_status: lazer_ranked_status(4),
        })))
    );
}

/// `player_screen`'s graph with the screen no longer a player, a local user, and a beatmap
/// track: `global` takes the non-player path inside a connected multiplayer room.
fn menu_screen() -> (Arc<SparseMemory>, usize, usize) {
    let offsets = LazerOffsets::embedded().unwrap();
    let (mut memory, base, screen) = super::tests_batch::player_screen(&offsets);
    memory.ptr(
        screen
            + offsets
                .osu_game_screens_play_submitting_player
                .api_k_backing_field,
        0,
    );
    memory.ptr(
        screen + offsets.osu_game_screens_play_player.api_k_backing_field,
        0,
    );
    // Nor a player loader: its logo is not the game's.
    memory.ptr(
        screen + offsets.osu_game_screens_osu_screen.logo_k_backing_field,
        0x1234,
    );
    let mut next = 0x80_000;
    let mut object = |memory: &mut SparseMemory| {
        let address = next;
        next += 0x1000;
        memory.put(address, [0u8; 0x700]);
        address
    };
    put_beatmap(&mut memory, base, 0x100_000);
    let [state, bindable, user, track] = std::array::from_fn(|_| object(&mut memory));
    let api = {
        let mut bytes = [0u8; 8];
        memory
            .read_exact(
                base + offsets.osu_game_osu_game_base.api_k_backing_field,
                &mut bytes,
            )
            .unwrap();
        usize::from_le_bytes(bytes)
    };
    memory.ptr(
        api + offsets.osu_game_online_api_apiaccess.local_user_state,
        state,
    );
    memory.ptr(
        state + offsets.osu_game_online_api_local_user_state.local_user,
        bindable,
    );
    memory.ptr(bindable + 0x20, user);
    let beatmap_clock = {
        let mut bytes = [0u8; 8];
        memory
            .read_exact(
                base + offsets.osu_game_osu_game_base.beatmap_clock,
                &mut bytes,
            )
            .unwrap();
        usize::from_le_bytes(bytes)
    };
    let decoupled = {
        let mut bytes = [0u8; 8];
        memory
            .read_exact(
                beatmap_clock
                    + offsets
                        .osu_game_beatmaps_framed_beatmap_clock
                        .decoupled_track,
                &mut bytes,
            )
            .unwrap();
        usize::from_le_bytes(bytes)
    };
    memory.ptr(
        decoupled
            + offsets
                .osu_framework_timing_decoupling_framed_clock
                .source_k_backing_field,
        track,
    );
    memory.f64(
        track + offsets.osu_framework_audio_track_track.length,
        98_765.4,
    );
    // A room with an id and a ranked-play state whose user dictionary is still null.
    let read_pointer = |memory: &SparseMemory, address: usize| {
        let mut bytes = [0u8; 8];
        memory.read_exact(address, &mut bytes).unwrap();
        usize::from_le_bytes(bytes)
    };
    let client = read_pointer(
        &memory,
        base + offsets
            .osu_game_osu_game_base
            .multiplayer_client_k_backing_field,
    );
    let room = read_pointer(
        &memory,
        client + offsets.osu_game_online_multiplayer_multiplayer_client.room,
    );
    let room_offsets = &offsets.osu_game_online_multiplayer_multiplayer_room;
    memory.i64(room + room_offsets.room_id, 4242);
    let ranked = object(&mut memory);
    memory.ptr(room + room_offsets.match_state_k_backing_field, ranked);
    let o = &offsets.osu_game_online_multiplayer_match_types_ranked_play_ranked_play_room_state;
    memory.i32(ranked + o.stage_k_backing_field, 2);
    memory.i32(ranked + o.current_round_k_backing_field, 3);
    memory.f64(ranked + o.star_rating_k_backing_field, 5.5);
    memory.f64(ranked + o.damage_multiplier_k_backing_field, 1.25);
    // One room member backed by a blank API user.
    let [users, slots, member, api_user] = std::array::from_fn(|_| object(&mut memory));
    memory.ptr(
        room + room_offsets
            .users_k_backing_field
            .expect("room users offset"),
        users,
    );
    memory.ptr(users + 8, slots);
    memory.i32(users + 0x10, 1);
    memory.ptr(slots + 0x10, member);
    let member_offsets = &offsets.osu_game_online_multiplayer_multiplayer_room_user;
    memory.i32(member + member_offsets.user_id, 77);
    memory.ptr(member + member_offsets.user_k_backing_field, api_user);
    let connected = {
        let client = {
            let mut bytes = [0u8; 8];
            memory
                .read_exact(
                    base + offsets
                        .osu_game_osu_game_base
                        .multiplayer_client_k_backing_field,
                    &mut bytes,
                )
                .unwrap();
            usize::from_le_bytes(bytes)
        };
        let mut bytes = [0u8; 8];
        memory
            .read_exact(
                client
                    + offsets
                        .osu_game_online_multiplayer_online_multiplayer_client
                        .is_connected_k_backing_field,
                &mut bytes,
            )
            .unwrap();
        usize::from_le_bytes(bytes)
    };
    (Arc::new(memory), base, connected)
}

fn menu_capture(probe: &Arc<Probe>, base: usize) -> String {
    let mut decoder = reader_with(probe.clone());
    decoder.set_resolved_state(base, 0);
    let mut out = Vec::new();
    for _ in 0..2 {
        decoder.synchronize_capture_config(false, true);
        out.push(format!("{:?}", decoder.global()));
        out.push(format!("{:?}", decoder.menu("")));
        out.push(format!("{:?}", decoder.mp3_length()));
        out.push(format!("{:?}", decoder.user()));
        out.push(format!("{:?}", decoder.ranked_play()));
        out.push(format!("{:?}", decoder.room()));
        out.push(format!(
            "{} {} {} {} {}",
            decoder.status,
            decoder.watching_replay,
            decoder.spectating,
            decoder.player_loading,
            decoder.multi_room().is_ok_and(|room| room != 0),
        ));
    }
    out.join("\n")
}

#[test]
fn batched_menu_global_matches_sequential_reads_under_every_read_failure() {
    let (memory, base, connected) = menu_screen();
    let sequential = probe(&memory, true, None);
    let expected = menu_capture(&sequential, base);
    assert!(
        expected.contains(&format!("status: {}", GameState::Lobby.value())),
        "{expected}"
    );
    assert!(expected.contains("Ok(Ready(98765.0))"), "{expected}");
    assert!(expected.contains("Ok(Ready(Full(MenuFull {"), "{expected}");
    assert!(
        expected.contains("LazerRoom { room_id: 4242, users: [LazerRoomUser { id: 77,"),
        "{expected}"
    );
    let batched = probe(&memory, false, None);
    assert_eq!(menu_capture(&batched, base), expected);
    // `global`, `check_multi_select`, `global_precise`, and the test's own `multi_room` each
    // check the connection; the batched capture reads it once per capture.
    let flag_reads = |probe: &Probe| {
        probe
            .log
            .lock()
            .iter()
            .filter(|(address, _)| *address == connected + 0x40)
            .count()
    };
    assert_eq!(flag_reads(&batched), 2);
    assert_eq!(flag_reads(&sequential), 8);

    let mut ranges = sequential
        .log
        .lock()
        .iter()
        .flat_map(|&(address, len)| {
            [
                (address, address + len),
                (address + len, address + len + 16),
            ]
        })
        .collect::<Vec<_>>();
    ranges.sort_unstable();
    ranges.dedup();
    for range in ranges {
        assert_eq!(
            menu_capture(&probe(&memory, false, Some(range)), base),
            menu_capture(&probe(&memory, true, Some(range)), base),
            "unreadable range {range:#x?}"
        );
    }
}

#[test]
fn batched_full_menu_takes_at_most_one_list_read_of_files() {
    let offsets = LazerOffsets::embedded().unwrap();
    let read_pointer = |memory: &SparseMemory, address: usize| {
        let mut bytes = [0u8; 8];
        memory.read_exact(address, &mut bytes).unwrap();
        usize::from_le_bytes(bytes)
    };
    let menu_with_files = |count: usize| {
        let mut m = SparseMemory::default();
        m.put(BASE, [0u8; 0x1000]);
        m.valid_base(BASE, &offsets);
        put_beatmap(&mut m, BASE, 0x20_0000);
        let bindable = read_pointer(
            &m,
            BASE + offsets.osu_game_osu_game_base.beatmap_k_backing_field,
        );
        let working = read_pointer(&m, bindable + 0x20);
        let set = read_pointer(
            &m,
            working
                + offsets
                    .osu_game_beatmaps_working_beatmap_cache_beatmap_manager_working_beatmap
                    .beatmap_set_info,
        );
        let list = read_pointer(&m, set + 0x20);
        let array = read_pointer(&m, list + 8);
        // Every slot repeats the first file, so the map equals the sequential one.
        let entry = read_pointer(&m, array + 0x10);
        m.i32(list + 0x10, count as i32);
        for index in 0..count {
            m.ptr(array + 0x10 + index * 8, entry);
        }
        let memory = Arc::new(m);
        let mut batched = reader_with(probe(&memory, false, None));
        batched.set_resolved_state(BASE, 0);
        let mut sequential = reader_with(probe(&memory, true, None));
        sequential.set_resolved_state(BASE, 0);
        (
            batched
                .menu_full_batched("")
                .map(|menu| format!("{menu:?}")),
            format!("{:?}", sequential.menu("")),
        )
    };
    let (at_cap, sequential) = menu_with_files(BATCHED_MENU_FILES);
    assert!(sequential.starts_with("Ok(Ready(Full("), "{sequential}");
    assert_eq!(at_cap.as_deref(), Some(sequential.as_str()));
    let (above_cap, sequential) = menu_with_files(BATCHED_MENU_FILES + 1);
    assert!(sequential.starts_with("Ok(Ready(Full("), "{sequential}");
    assert_eq!(above_cap, None);
}

#[test]
fn resource_paths_join_like_the_platform_path() {
    for hash in [MD5, FILE, AUDIO, BACKGROUND] {
        let joined = std::path::Path::new(&hash[..1]).join(&hash[..2]).join(hash);
        assert_eq!(lazer_path(hash).unwrap(), joined.to_string_lossy());
    }
    assert_eq!(lazer_path("").unwrap(), "");
    assert!(lazer_path("not-a-digest").is_err());
}
