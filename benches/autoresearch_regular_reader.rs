//! Successful production regular capture, including Linux native read/generation checks.
//! Fixtures and their decoded-value contract are shared with the compatibility test.
use criterion::{Criterion, criterion_group, criterion_main};

#[cfg(all(target_os = "linux", target_arch = "x86_64"))]
pub mod fixture {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::sync::{Arc, Mutex};
    use vosu::memory::lazer_offsets::LazerOffsets;
    use vosu::memory::snapshot::{RawRegularSnapshot, RegularCaptureContext};
    use vosu::memory::{
        LazerMemory, LazerPattern, ReaderSynchronization, StableMemory, StablePatterns,
    };
    use vosu::model::enums::ClientType;
    use vosu::model::memory::Menu;
    use vosu::process::{
        MemoryError, MemoryRegion, ProcessBitness, ProcessHandle, ProcessMemory, ReadRequest,
        ReadStatus,
    };
    use vosu::state::{
        RegularCapturePlan, RegularCaptureRequest, RegularSnapshotReader, SourceRegularReader,
    };

    const SIZE: usize = 4 * 1024 * 1024;
    // Stable stores absolute x86 pointers. MAP_32BIT makes the owned image usable
    // by the actual decoder, without translating addresses in the backend.
    struct Image {
        base: usize,
        next: usize,
        width: usize,
    }
    impl Image {
        fn new(width: usize) -> Self {
            // SAFETY: anonymous private mapping, retained until Drop; no executable permissions.
            let p = unsafe {
                libc::mmap(
                    std::ptr::null_mut(),
                    SIZE,
                    libc::PROT_READ | libc::PROT_WRITE,
                    libc::MAP_PRIVATE | libc::MAP_ANONYMOUS | libc::MAP_32BIT,
                    -1,
                    0,
                )
            };
            assert_ne!(p, libc::MAP_FAILED);
            Self {
                base: p as usize,
                next: 0x1000,
                width,
            }
        }
        fn node(&mut self) -> usize {
            let p = self.base + self.next;
            self.next += 0x1000;
            assert!(self.next < SIZE);
            p
        }
        fn put(&self, p: usize, bytes: &[u8]) {
            assert!(p >= self.base && p + bytes.len() <= self.base + SIZE);
            // SAFETY: all writes happen during construction, before native readers exist.
            unsafe {
                std::ptr::copy_nonoverlapping(bytes.as_ptr(), p as *mut u8, bytes.len());
            }
        }
        fn ptr(&self, p: usize, v: usize) {
            if self.width == 4 {
                self.put(p, &(u32::try_from(v).unwrap()).to_le_bytes());
            } else {
                self.put(p, &v.to_le_bytes());
            }
        }
        fn i32(&self, p: usize, v: i32) {
            self.put(p, &v.to_le_bytes());
        }
        fn f64(&self, p: usize, v: f64) {
            self.put(p, &v.to_le_bytes());
        }
        fn f32(&self, p: usize, v: f32) {
            self.put(p, &v.to_le_bytes());
        }
        fn string(&mut self, p: usize, text: &str) {
            let s = self.node();
            self.ptr(p, s);
            let units: Vec<_> = text.encode_utf16().collect();
            self.i32(s + self.width, units.len() as i32);
            for (i, v) in units.iter().enumerate() {
                self.put(s + self.width + 4 + i * 2, &v.to_le_bytes());
            }
        }
        fn child(&mut self, p: usize) -> usize {
            let n = self.node();
            self.ptr(p, n);
            n
        }
        fn chain(&mut self, p: usize, v: usize) {
            let n = self.child(p);
            self.ptr(n, v);
        }
        fn list(&mut self, items: &[usize]) -> usize {
            let list = self.node();
            let array = self.child(list + 8);
            self.i32(list + 0x10, items.len() as i32);
            for (i, &v) in items.iter().enumerate() {
                self.ptr(array + 0x10 + 8 * i, v);
            }
            list
        }
        fn config(&mut self, p: usize, keys: &[(i32, usize)], local: bool) {
            let config = self.child(p);
            let store = self.child(config + 0x20);
            let entries = self.child(store + 0x10);
            self.i32(store + 0x38, keys.len() as i32);
            for (i, &(key, width)) in keys.iter().enumerate() {
                let e = entries + 0x10 + i * 0x18;
                let bind = self.child(e);
                self.i32(e + 0x10, key);
                self.f64(bind + 0x40, 0.5);
                if width == 1 {
                    self.put(bind + 0x40, &[1]);
                } else if width == 4 {
                    self.i32(bind + 0x40, 0);
                }
                if local && [11, 12, 22, 23, 24, 25].contains(&i) {
                    self.f32(bind + 0x40, 1.25);
                }
                if [7, 11].contains(&key) && !local {
                    self.i32(bind + 0x44, 1920);
                    self.i32(bind + 0x48, 1080);
                }
            }
        }
    }
    impl Drop for Image {
        fn drop(&mut self) {
            unsafe {
                libc::munmap(self.base as *mut _, SIZE);
            }
        }
    }

    pub struct Counts {
        calls: AtomicU64,
        bytes: AtomicU64,
    }
    struct Native {
        inner: vosu::process::LinuxProcessMemory,
        counts: Arc<Counts>,
    }
    impl ProcessMemory for Native {
        fn read_exact(&self, p: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
            self.counts.calls.fetch_add(1, Ordering::Relaxed);
            self.counts
                .bytes
                .fetch_add(dst.len() as u64, Ordering::Relaxed);
            self.inner.read_exact(p, dst)
        }
        fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
            self.counts.calls.fetch_add(1, Ordering::Relaxed);
            self.counts.bytes.fetch_add(
                reads.iter().map(|r| r.dst.len() as u64).sum(),
                Ordering::Relaxed,
            );
            self.inner.read_many(reads)
        }
        fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
            self.inner.readable_regions()
        }
    }
    pub struct Fixture {
        reader: Box<dyn RegularSnapshotReader>,
        request: RegularCaptureRequest,
        counts: Arc<Counts>,
        _image: Image,
        pub lazer: bool,
        pub status: i64,
    }
    impl Fixture {
        pub fn new(lazer: bool, status: i64) -> Self {
            let mut image = Image::new(if lazer { 8 } else { 4 });
            let counts = Arc::new(Counts {
                calls: AtomicU64::new(0),
                bytes: AtomicU64::new(0),
            });
            let process = ProcessHandle::new(
                Arc::new(Native {
                    inner: vosu::process::open_process(std::process::id()).unwrap(),
                    counts: counts.clone(),
                }),
                if lazer {
                    ProcessBitness::X64
                } else {
                    ProcessBitness::X86
                },
            );
            let source = if lazer {
                ClientType::Lazer
            } else {
                ClientType::Stable
            };
            let reader: Box<dyn RegularSnapshotReader> = if lazer {
                let o = Arc::new(LazerOffsets::embedded().unwrap());
                let base = image.node();
                let screen = image.node();
                let api = image.child(base + o.osu_game_osu_game_base.api_k_backing_field);
                let vt = image.child(base);
                image.ptr(vt, o.effective_game_base_vtable());
                let spectator =
                    image.child(base + o.osu_game_osu_game_base.spectator_client_k_backing_field);
                image.child(base + o.osu_game_osu_game_base.score_manager_k_backing_field);
                image.child(base + o.osu_game_osu_game_base.leaderboard_manager_k_backing_field);
                let logo = image.child(base + o.osu_game_osu_game.osu_logo);
                let stack = image.child(
                    base + o
                        .osu_game_osu_game
                        .screen_stack
                        .unwrap_or(o.osu_game_osu_game.screen_stack_k_backing_field),
                );
                let list = image.list(&[screen]);
                image.ptr(stack + o.osu_framework_screens_screen_stack.stack, list);
                let ruleset = image.node();
                let bind = image.child(base + o.osu_desktop_osu_game_desktop.ruleset);
                image.ptr(bind + 0x20, ruleset);
                let chat = image.child(base + o.osu_game_osu_game.chat_overlay);
                let state = image.child(chat + o.osu_game_overlays_chat_overlay.state);
                image.i32(state + 0x40, 1);
                let storage = image.child(base + o.osu_game_osu_game_base.storage_k_backing_field);
                let wrapped = image.child(
                    storage
                        + o.osu_game_io_wrapped_storage
                            .underlying_storage_k_backing_field,
                );
                image.string(
                    wrapped + o.osu_framework_platform_storage.base_path_k_backing_field,
                    "/benchmark/osu",
                );
                let clock = image.child(base + o.osu_game_osu_game_base.beatmap_clock);
                let dec =
                    image.child(clock + o.osu_game_beatmaps_framed_beatmap_clock.decoupled_track);
                image.f64(dec + 0x20, 4321.0);
                let track = image.child(
                    dec + o
                        .osu_framework_timing_decoupling_framed_clock
                        .source_k_backing_field,
                );
                image.f64(track + o.osu_framework_audio_track_track.length, 120000.0);
                let visual = image.child(logo + o.osu_game_screens_menu_osu_logo.visualizer);
                let frequencies = image.child(
                    visual
                        + o.osu_game_screens_menu_logo_visualisation
                            .frequency_amplitudes,
                );
                for i in 0..40 {
                    image.f32(frequencies + 0x10 + i * 4, i as f32 / 40.0);
                }
                let beatbind = image.child(base + o.osu_game_osu_game_base.beatmap_k_backing_field);
                let working = image.child(beatbind + 0x20);
                let info = image.child(
                    working
                        + o.osu_game_beatmaps_working_beatmap_cache_beatmap_manager_working_beatmap
                            .beatmap_info,
                );
                let set = image.child(
                    working
                        + o.osu_game_beatmaps_working_beatmap_cache_beatmap_manager_working_beatmap
                            .beatmap_set_info,
                );
                let b = &o.osu_game_beatmaps_beatmap_info;
                let meta = image.child(info + b.metadata_k_backing_field);
                let diff = image.child(info + b.difficulty_k_backing_field);
                image.string(
                    info + b.md5_hash_k_backing_field,
                    "0123456789abcdef0123456789abcdef",
                );
                image.string(
                    info + b.hash_k_backing_field,
                    "abcdef123456abcdef123456abcdef12",
                );
                image.string(info + b.difficulty_name_k_backing_field, "Expert");
                image.i32(info + b.online_id_k_backing_field, 123);
                image.i32(info + b.total_object_count_k_backing_field, 500);
                image.i32(
                    set + o
                        .osu_game_beatmaps_beatmap_set_info
                        .online_id_k_backing_field,
                    456,
                );
                let mut files = Vec::new();
                for name in ["audio.mp3", "bg.jpg"] {
                    let file = image.node();
                    let realm = image.child(file + 0x18);
                    image.string(realm + 0x18, "0123456789abcdef0123456789abcdef");
                    image.string(file + 0x20, name);
                    files.push(file);
                }
                let files = image.list(&files);
                image.ptr(set + 0x20, files);
                let m = &o.osu_game_beatmaps_beatmap_metadata;
                let author = image.child(meta + m.author_k_backing_field);
                image.string(
                    author + o.osu_game_models_realm_user.username_k_backing_field,
                    "Mapper",
                );
                for (off, text) in [
                    (m.artist_k_backing_field, "Artist"),
                    (m.artist_unicode_k_backing_field, "Artist"),
                    (m.title_k_backing_field, "Title"),
                    (m.title_unicode_k_backing_field, "Title"),
                    (m.audio_file_k_backing_field, "audio.mp3"),
                    (m.background_file_k_backing_field, "bg.jpg"),
                ] {
                    image.string(meta + off, text);
                }
                let d = &o.osu_game_beatmaps_beatmap_difficulty;
                for (off, v) in [
                    (d.approach_rate_k_backing_field, 9.0),
                    (d.circle_size_k_backing_field, 4.0),
                    (d.drain_rate_k_backing_field, 5.0),
                    (d.overall_difficulty_k_backing_field, 8.0),
                ] {
                    image.f32(diff + off, v);
                }
                image.config(
                    base + o.osu_game_osu_game_base.local_config_k_backing_field,
                    &o.local_config_keys.as_ref().unwrap().fields(),
                    true,
                );
                image.config(
                    base + o.osu_game_osu_game.framework_config_k_backing_field,
                    &[(14, 4), (7, 8), (11, 8), (3, 8), (5, 8), (4, 8), (21, 8)],
                    false,
                );
                let skin_manager =
                    image.child(base + o.osu_game_osu_game_base.skin_manager_k_backing_field);
                let current_skin =
                    image.child(skin_manager + o.osu_game_skinning_skin_manager.current_skin);
                let skin = image.child(current_skin + 0x20);
                image.string(
                    skin + o.osu_game_skinning_skin.name_k_backing_field,
                    "Benchmark skin",
                );
                let cache = image.child(base + o.osu_game_osu_game_base.ruleset_config_cache);
                image.child(cache + o.osu_game_rulesets_ruleset_config_cache.config_cache);
                let local = image.child(api + o.osu_game_online_api_apiaccess.local_user_state);
                let bind = image.child(local + o.osu_game_online_api_local_user_state.local_user);
                let user = image.child(bind + 0x20);
                let u = &o.osu_game_online_api_requests_responses_apiuser;
                image.i32(user + u.id_k_backing_field, 42);
                image.string(user + u.username_k_backing_field, "Player");
                image.string(user + u.country_code_string, "US");
                let stats = image.child(user + u.statistics);
                let us = &o.osu_game_users_user_statistics;
                image.f64(stats + us.accuracy, 98.5);
                image.i32(stats + us.level, 100);
                image.i32(stats + us.play_count, 2345);
                image.i32(stats + us.global_rank + 4, 1234);
                // Connected=false keeps menu classification intact while the regular
                // room and ranked-play decoders still traverse a populated room.
                let client =
                    image.child(base + o.osu_game_osu_game_base.multiplayer_client_k_backing_field);
                let room =
                    image.child(client + o.osu_game_online_multiplayer_multiplayer_client.room);
                let ro = &o.osu_game_online_multiplayer_multiplayer_room;
                image.put(room + ro.room_id, &987i64.to_le_bytes());
                let room_user = image.node();
                let ru = &o.osu_game_online_multiplayer_multiplayer_room_user;
                image.i32(room_user + ru.user_id, 42);
                image.ptr(room_user + ru.user_k_backing_field, user);
                let users = image.list(&[room_user]);
                image.ptr(
                    room + ro
                        .users_k_backing_field
                        .expect("embedded room users offset"),
                    users,
                );
                let ranked = image.child(room + ro.match_state_k_backing_field);
                let rp =
                    &o.osu_game_online_multiplayer_match_types_ranked_play_ranked_play_room_state;
                image.i32(ranked + rp.stage_k_backing_field, 1);
                image.i32(ranked + rp.current_round_k_backing_field, 2);
                image.f64(ranked + rp.star_rating_k_backing_field, 5.5);
                image.f64(ranked + rp.damage_multiplier_k_backing_field, 1.25);
                let dict = image.child(ranked + rp.users_k_backing_field);
                let entries = image.child(dict + 0x10);
                image.i32(entries + 8, 1);
                let ranked_user = image.child(entries + 0x10);
                image.i32(entries + 0x18, 1);
                image.i32(entries + 0x20, 42);
                let rpu =
                    &o.osu_game_online_multiplayer_match_types_ranked_play_ranked_play_user_info;
                image.i32(ranked_user + rpu.rating_k_backing_field, 1500);
                image.i32(ranked_user + rpu.life_k_backing_field, 80);
                image.i32(ranked_user + rpu.rounds_won_k_backing_field, 1);
                image.f64(ranked_user + rpu.damage_multiplier_k_backing_field, 1.25);
                let score = image.node();
                let s = &o.osu_game_scoring_score_info;
                let realm = image.child(score + s.realm_user_k_backing_field);
                image.string(
                    realm + o.osu_game_models_realm_user.username_k_backing_field,
                    "Player",
                );
                image.i32(
                    realm + o.osu_game_models_realm_user.online_id_k_backing_field,
                    42,
                );
                image.ptr(score + s.ruleset_k_backing_field, ruleset);
                image.put(
                    score + s.total_score_k_backing_field,
                    &123456i64.to_le_bytes(),
                );
                image.f64(score + s.accuracy_k_backing_field, 0.985);
                image.i32(score + s.max_combo_k_backing_field, 321);
                image.i32(score + s.combo_k_backing_field, 123);
                image.string(score + s.mods_json_k_backing_field, "[]");
                image.put(score + s.passed_k_backing_field, &[1]);
                for (off, value) in [(s.statistics, 100), (s.maximum_statistics, 500)] {
                    let dict = image.child(score + off);
                    let entries = image.child(dict + 0x10);
                    image.i32(dict + 0x38, 1);
                    image.i32(entries + 0x18, 5);
                    image.i32(entries + 0x1c, value);
                }
                if status == 2 {
                    image.ptr(
                        screen
                            + o.osu_game_screens_play_submitting_player
                                .api_k_backing_field,
                        api,
                    );
                    image.ptr(
                        screen
                            + o.osu_game_screens_play_submitting_player
                                .spectator_client_k_backing_field,
                        spectator,
                    );
                    let hud = image
                        .child(screen + o.osu_game_screens_play_player.hudoverlay_k_backing_field);
                    let show = image
                        .child(hud + o.osu_game_screens_play_hudoverlay.show_hud_k_backing_field);
                    image.put(show + 0x40, &[1]);
                    let hp = image.child(
                        screen
                            + o.osu_game_screens_play_player
                                .health_processor_k_backing_field,
                    );
                    let bind = image
                        .child(hp + o.osu_game_rulesets_osu_scoring_osu_health_processor.health);
                    image.f64(bind + 0x40, 0.75);
                    let current =
                        image.child(screen + o.osu_game_screens_play_player.score_k_backing_field);
                    image.ptr(current + 8, score);
                    let proc = image.child(
                        screen
                            + o.osu_game_screens_play_player
                                .score_processor_k_backing_field,
                    );
                    let combo = image
                        .child(proc + o.osu_game_rulesets_osu_scoring_osu_score_processor.combo);
                    image.i32(combo + 0x40, 123);
                } else if status == 7 {
                    image.ptr(
                        screen
                            + o.osu_game_screens_ranking_solo_results_screen
                                .api_k_backing_field,
                        api,
                    );
                    image.ptr(
                        screen + o.osu_game_screens_osu_screen.logo_k_backing_field,
                        logo,
                    );
                    let bind = image.child(
                        screen
                            + o.osu_game_screens_ranking_solo_results_screen
                                .selected_score,
                    );
                    image.ptr(bind + 0x20, score);
                }
                let mut memory = LazerMemory::new(process, o, LazerPattern::source_default());
                memory.set_resolved_state(base, screen);
                Box::new(SourceRegularReader::new(
                    source,
                    memory,
                    Arc::new(Mutex::new(ReaderSynchronization::default())),
                ))
            } else {
                let base = image.node();
                let p = StablePatterns {
                    base_addr: base + 0x100,
                    play_time_addr: base + 0x200,
                    chat_checker_ptr: base + 0x300,
                    skin_data_addr: base + 0x400,
                    settings_class_addr: base + 0x500,
                    configuration_addr: base + 0x600,
                    bindings_addr: base + 0x700,
                    rulesets_addr: base + 0x800,
                    can_run_slowly_addr: base + 0x900,
                    status_ptr: base + 0xa00,
                    menu_mods_ptr: base + 0xb00,
                    get_audio_length_ptr: base + 0xc00,
                    user_profile_ptr: base + 0xd00,
                    raw_login_status_ptr: base + 0xe00,
                    spectating_user_ptr: base + 0xf00,
                    game_time_ptr: base + 0xf80,
                };
                image.chain(p.menu_mods_ptr, 0);
                image.child(p.chat_checker_ptr);
                image.child(p.can_run_slowly_addr + 0x46);
                image.chain(p.game_time_ptr, 999);
                let statusp = image.child(p.status_ptr);
                image.i32(statusp, status as i32);
                let time = image.child(p.play_time_addr + 5);
                image.i32(time, 4321);
                let sc = image.child(p.settings_class_addr + 8);
                let songs = image.child(sc + 0xb8);
                image.string(songs + 4, "/benchmark/Songs");
                let show = image.child(sc + 4);
                image.put(show + 0xc, &[1]);
                let beat = image.node();
                image.chain(p.base_addr - 0xc, beat);
                let mode = image.child(p.base_addr - 0x33);
                image.i32(mode, 0);
                image.i32(mode + 8, 2);
                for (off, text) in [
                    (0x6c, "0123456789abcdef0123456789abcdef"),
                    (0x90, "map.osu"),
                    (0x64, "audio.mp3"),
                    (0x68, "bg.jpg"),
                    (0x18, "Artist"),
                    (0x1c, "Artist"),
                    (0x24, "Title"),
                    (0x28, "Title"),
                    (0x78, "fixture"),
                    (0x7c, "Mapper"),
                    (0xac, "Expert"),
                ] {
                    image.string(beat + off, text);
                }
                for (off, v) in [(0x2c, 9.0), (0x30, 4.0), (0x34, 5.0), (0x38, 8.0)] {
                    image.f32(beat + off, v);
                }
                image.i32(beat + 0xc8, 123);
                image.i32(beat + 0xcc, 456);
                image.i32(beat + 0xf8, 500);
                let audio = image.node();
                image.chain(p.get_audio_length_ptr, audio);
                image.f64(audio + 4, 120000.0);
                let user = image.node();
                image.chain(p.user_profile_ptr, user);
                image.chain(p.raw_login_status_ptr, 1);
                image.string(user + 0x30, "Player");
                image.f64(user + 4, 98.5);
                image.i32(user + 0x70, 42);
                image.f32(user + 0x74, 100.0);
                image.i32(user + 0x7c, 2345);
                image.i32(user + 0x84, 1234);
                let config = image.node();
                image.chain(p.configuration_addr, config);
                let entries = image.child(config + 8);
                image.i32(config + 0x1c, 3);
                for (i, (name, value)) in [
                    ("VolumeUniversal", 50.0),
                    ("VolumeMusic", 60.0),
                    ("DimLevel", 75.0),
                ]
                .iter()
                .enumerate()
                {
                    let e = entries + 8 + i * 16;
                    image.string(e, name);
                    let bind = image.child(e + 4);
                    image.f64(bind + 4, *value);
                }
                let bindings = image.node();
                image.chain(p.bindings_addr, bindings);
                let e = image.child(bindings + 8);
                image.i32(bindings + 0x1c, 1);
                image.i32(e + 8, 2);
                image.i32(e + 20, 90);
                let container = image.child(p.rulesets_addr - 0xb);
                let ruleset = image.child(container + 4);
                let av = image.child(ruleset + 0x44);
                let array = image.child(av + 0x10);
                image.i32(array + 4, 40);
                for i in 0..40 {
                    image.f32(array + 8 + i * 4, i as f32 / 40.0);
                }
                let score = image.node();
                let mods = image.child(score + 0x1c);
                image.i32(mods + 8, 0);
                image.string(score + 0x28, "Player");
                image.i32(score + 0x78, 123456);
                image.put(score + 0x8a, &100i16.to_le_bytes());
                image.put(score + 0x94, &123i16.to_le_bytes());
                image.put(score + 0x68, &321i16.to_le_bytes());
                image.ptr(score + 0x48, user);
                image.ptr(ruleset + 0x38, score);
                let game = image.child(ruleset + 0x64);
                image.ptr(game + 0x38, score);
                let hp = image.child(game + 0x40);
                image.f64(hp + 0x14, 150.0);
                image.f64(hp + 0x1c, 150.0);
                let accuracy = image.child(game + 0x48);
                image.f64(accuracy + 0xc, 100.0);
                let leaderboard = image.child(ruleset + 0x78);
                let addr = image.child(leaderboard + 0x24);
                let player = image.child(addr + 0x10);
                image.ptr(player + 0x20, score);
                image.string(player + 8, "Player");
                image.i32(player + 0x30, 123456);
                let visible = image.child(player + 0x24);
                image.put(visible + 0x20, &[1]);
                let list = image.child(addr + 4);
                image.i32(list + 0xc, 1);
                let items = image.child(list + 4);
                image.ptr(items + 8, player);
                Box::new(SourceRegularReader::new(
                    source,
                    StableMemory::new(process, p),
                    Arc::new(Mutex::new(ReaderSynchronization::default())),
                ))
            };
            // Active play retains the reducer's known checksum/first-object time.
            // Menu/result captures exercise the full populated beatmap path.
            let request = RegularCaptureRequest {
                context: RegularCaptureContext::new(source, 1, 0, 1),
                plan: RegularCapturePlan {
                    status,
                    previous_checksum: if status == 2 {
                        "0123456789abcdef0123456789abcdef".into()
                    } else {
                        String::new()
                    },
                    gameplay_time: 4321.0,
                    first_object_time: Some(1000),
                    read_mp3_length: true,
                    ..Default::default()
                },
            };
            Self {
                reader,
                request,
                counts,
                _image: image,
                lazer,
                status,
            }
        }
        pub fn capture(&mut self) -> Arc<RawRegularSnapshot> {
            self.reader.capture_regular(&self.request).unwrap()
        }
        pub fn measured_capture(&mut self) -> (Arc<RawRegularSnapshot>, u64, u64) {
            self.counts.calls.store(0, Ordering::Relaxed);
            self.counts.bytes.store(0, Ordering::Relaxed);
            let snapshot = self.capture();
            (
                snapshot,
                self.counts.calls.load(Ordering::Relaxed),
                self.counts.bytes.load(Ordering::Relaxed),
            )
        }
        pub fn assert_snapshot(&self, s: &RawRegularSnapshot) {
            let global = ready(s.global());
            assert_eq!(global.status, self.status);
            assert_eq!(global.play_time, 4321.0);
            assert!(global.memory_songs_folder.starts_with("/benchmark/"));
            match ready(s.menu().unwrap()) {
                Menu::Full(m) => {
                    assert_ne!(self.status, 2);
                    assert_eq!(m.title, "Title");
                    assert_eq!(m.artist, "Artist");
                    assert_eq!(m.creator, "Mapper");
                    assert_eq!(m.map_id, 123);
                    assert_eq!(m.set_id, 456);
                    assert_eq!(m.object_count, 500);
                    assert_eq!((m.ar, m.cs, m.hp, m.od), (9.0, 4.0, 5.0, 8.0));
                }
                Menu::Checksum(m) => {
                    assert_eq!(self.status, 2);
                    assert_eq!(m.gamemode, 0);
                }
            }
            assert_eq!(*ready(s.mp3_length().unwrap()), 120000.0);
            let settings = ready(s.settings().unwrap());
            assert_eq!(settings["audio.volume.master"], serde_json::json!(50.0));
            assert_eq!(
                settings["background.dim"],
                serde_json::json!(if self.lazer { 50.0 } else { 75.0 })
            );
            assert_eq!(
                settings["audio.volume.music"],
                serde_json::json!(if self.lazer { 50.0 } else { 60.0 })
            );
            if self.lazer {
                assert_eq!(settings["skin.name"], serde_json::json!("Benchmark skin"));
            } else {
                assert_eq!(settings["keybinds.osu.k1"], serde_json::json!("Z"));
            }
            let user = ready(s.user().unwrap());
            assert_eq!(user.name, "Player");
            assert_eq!(user.id, 42);
            assert_eq!(user.accuracy, 98.5);
            assert_eq!(user.level, 100.0);
            assert_eq!(user.play_count, 2345);
            assert_eq!(user.rank, 1234);
            if self.lazer {
                let room = ready(s.room().unwrap());
                assert_eq!(room.room_id, 987);
                assert_eq!(room.users.len(), 1);
                assert_eq!(room.users[0].username, "Player");
                let ranked = ready(s.ranked_play().unwrap());
                assert_eq!(ranked.current_round, 2);
                assert_eq!(ranked.star_rating, 5.5);
                assert_eq!(ranked.users.len(), 1);
                assert_eq!(ranked.users[0].rating, 1500.0);
            }
            if self.status == 2 {
                let g = ready(s.gameplay().unwrap());
                assert_eq!(g.player_name, "Player");
                assert_eq!(g.score, 123456.0);
                assert_eq!(g.player_hp, 150.0);
                assert_eq!(g.combo, 123);
                assert_eq!(g.max_combo, 321);
                assert_eq!(g.mode, 0);
                assert_eq!(g.statistics.great, 100);
                assert!(!g.failed);
                let l = ready(s.leaderboard().unwrap());
                assert!(l.0);
                assert_eq!(l.1.as_ref().unwrap().name, "Player");
                assert_eq!(l.1.as_ref().unwrap().score, 123456.0);
            }
            if self.status == 2 {
                let g = ready(s.gameplay().unwrap());
                assert_eq!(g.accuracy, if self.lazer { 98.5 } else { 100.0 });
                assert_eq!(g.retries, if self.lazer { 0 } else { 2 });
                assert_eq!(g.maximum_statistics.great, if self.lazer { 500 } else { 0 });
                assert!(g.mods.array.is_empty());
                assert_eq!(g.player_hp_smooth, 150.0);
            }
            if self.status == 0 {
                let audio = ready(s.audio_velocity_base().unwrap());
                assert_eq!(audio.len(), 40);
                for (i, value) in audio.iter().enumerate() {
                    assert_eq!(*value, (i as f32 / 40.0) as f64);
                }
            }
            if self.status == 7 {
                let r = ready(s.result_screen().unwrap());
                assert_eq!(r.player_name, "Player");
                assert_eq!(r.score, 123456.0);
                assert_eq!(r.max_combo, 321);
                assert_eq!(r.statistics.great, 100);
            }
        }
    }
    #[track_caller]
    fn ready<T: std::fmt::Debug>(read: &Result<ReadStatus<T>, MemoryError>) -> &T {
        match read {
            Ok(ReadStatus::Ready(v)) => v,
            other => panic!("successful field required: {other:?}"),
        }
    }
}

fn bench(c: &mut Criterion) {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    {
        let mut group = c.benchmark_group("regular_reader");
        for (name, lazer, status) in [
            ("lazer_gameplay", true, 2),
            ("lazer_menu", true, 0),
            ("stable_gameplay", false, 2),
            ("stable_menu", false, 0),
        ] {
            let mut fixture = fixture::Fixture::new(lazer, status);
            // Prime decoder-owned configuration identity caches, as a live steady cycle does.
            let s = fixture.capture();
            fixture.assert_snapshot(&s);
            let (s, calls, bytes) = fixture.measured_capture();
            fixture.assert_snapshot(&s);
            println!("READS regular_reader/{name} native_calls={calls} requested_bytes={bytes}");
            group.bench_function(name, |b| b.iter(|| std::hint::black_box(fixture.capture())));
        }
        group.finish();
    }
    #[cfg(not(all(target_os = "linux", target_arch = "x86_64")))]
    {
        let _ = c;
        panic!("regular_reader requires Linux x86_64 native self-process reads");
    }
}
criterion_group! {
    name = benches;
    config = Criterion::default().sample_size(60)
        .warm_up_time(std::time::Duration::from_secs(2))
        .measurement_time(std::time::Duration::from_secs(5));
    targets = bench
}
criterion_main!(benches);
