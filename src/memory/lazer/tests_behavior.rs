use super::test_support::*;
use super::*;

#[test]
fn mod_rate_precedence_and_case_sensitive_mime_match_source() {
    let mods = calculate_mods(vec![
        Mod {
            acronym: "HT".into(),
            settings: None,
        },
        Mod {
            acronym: "DT".into(),
            settings: None,
        },
        Mod {
            acronym: "custom".into(),
            settings: None,
        },
    ]);
    assert_eq!(mods.rate, 1.5);
    assert_eq!(
        calculate_mods(vec![
            Mod {
                acronym: "DT".into(),
                settings: None
            },
            Mod {
                acronym: "HT".into(),
                settings: None
            },
        ])
        .rate,
        0.75,
    );
    assert_eq!(
        calculate_mods(vec![Mod {
            acronym: "DC".into(),
            settings: None
        }])
        .rate,
        0.75
    );
    assert_eq!(
        calculate_mods(vec![
            Mod {
                acronym: "DT".into(),
                settings: None
            },
            Mod {
                acronym: "DC".into(),
                settings: None
            }
        ])
        .rate,
        1.5
    );
    assert_eq!(
        calculate_mods(vec![
            Mod {
                acronym: "DC".into(),
                settings: None
            },
            Mod {
                acronym: "DT".into(),
                settings: None
            }
        ])
        .rate,
        1.5
    );
    assert!(mods.name.contains("CUSTOM"));
    assert_eq!(
        calculate_mods(vec![
            Mod {
                acronym: "NF".into(),
                settings: None
            },
            Mod {
                acronym: "EZ".into(),
                settings: None
            },
        ])
        .name,
        "EZNF",
    );
    assert_eq!(
        calculate_mods(vec![
            Mod {
                acronym: "DT".into(),
                settings: None
            },
            Mod {
                acronym: "NC".into(),
                settings: None
            },
            Mod {
                acronym: "DT".into(),
                settings: None
            },
            Mod {
                acronym: "NC".into(),
                settings: None
            },
        ])
        .name,
        "NCDTNC",
    );
    let mut coerced = IndexMap::new();
    coerced.insert("style".into(), serde_json::json!(["a", 2]));
    coerced.insert("direction".into(), serde_json::json!({"x": 1}));
    let coerced = calculate_mods(vec![Mod {
        acronym: "DA".into(),
        settings: Some(coerced),
    }]);
    assert_eq!(coerced.array[0].settings.as_ref().unwrap()["style"], "a,2");
    assert_eq!(
        coerced.array[0].settings.as_ref().unwrap()["direction"],
        "[object Object]"
    );
    assert_eq!(mime("audio.mp3"), "audio/mpeg; charset=utf-8");
    assert_eq!(mime("audio.MP3"), "");
    assert_eq!(mime(".mp3"), "");
    assert_eq!(mime(".foo.mp3"), "audio/mpeg; charset=utf-8");
    assert_eq!(OSU_OLD_MOD_CATEGORIES[1][6], "FL");
    assert_eq!(OSU_OLD_MOD_CATEGORIES[4][7], "TC");
}

#[test]
fn malformed_nonempty_mod_json_is_a_hard_read_error() {
    let offsets = LazerOffsets::embedded().unwrap();
    let score = 0x1000usize;
    let mut memory = SparseMemory::default();
    memory.string_ptr(
        score
            + offsets
                .osu_game_scoring_score_info
                .mods_json_k_backing_field,
        0x2000,
        "{broken",
    );
    let decoder = reader(Arc::new(memory));
    assert!(matches!(
        decoder.mods(score),
        Err(MemoryError::Corrupt {
            context: "mods json",
            ..
        })
    ));
    let mut memory = SparseMemory::default();
    memory.string_ptr(
        score
            + offsets
                .osu_game_scoring_score_info
                .mods_json_k_backing_field,
        0x3000,
        r#"[{"acronym":"DA","settings":{"style":null}}]"#,
    );
    let decoder = reader(Arc::new(memory));
    assert!(matches!(
        decoder.mods(score),
        Err(MemoryError::Corrupt {
            context: "mods json",
            ..
        })
    ));
}

#[test]
fn key_overlay_uses_bindable_collection_and_all_button_names() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x1000usize;
    let player = 0x3000usize;
    let overlay = 0x4000usize;
    let controller = 0x5000usize;
    let bindable = 0x6000usize;
    let collection = 0x7000usize;
    let items = 0x8000usize;
    let trigger = 0x9000usize;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    memory.ptr(base + offsets.osu_game_osu_game_base.api_k_backing_field, 0);
    memory.ptr(
        base + offsets
            .osu_game_osu_game_base
            .spectator_client_k_backing_field,
        0,
    );
    memory.ptr(
        player
            + offsets
                .osu_game_screens_play_submitting_player
                .api_k_backing_field,
        0,
    );
    memory.ptr(
        player
            + offsets
                .osu_game_screens_play_submitting_player
                .spectator_client_k_backing_field,
        0,
    );
    memory.ptr(
        player
            + offsets
                .osu_game_screens_play_player
                .hudoverlay_k_backing_field,
        overlay,
    );
    memory.ptr(
        overlay
            + offsets
                .osu_game_screens_play_hudoverlay
                .input_count_controller,
        controller,
    );
    memory.ptr(
        controller
            + offsets
                .osu_game_screens_play_hud_input_count_controller
                .triggers,
        bindable,
    );
    memory.ptr(bindable + 0x18, collection);
    memory.ptr(collection + 8, items);
    memory.i32(collection + 0x10, 18);
    for index in 0..18 {
        let button = trigger + index * 0x400;
        memory.ptr(items + 0x10 + index * 8, button);
        memory.ptr(button + 0x208, button + 0x300);
        memory.i32(button + 0x340, index as i32 - 9);
        memory.put(button + 0x1f4, [(index % 4) as u8]);
    }
    let mut decoder = reader(Arc::new(memory));
    decoder.set_resolved_state(base, player);
    let ReadStatus::Ready(buttons) = decoder.key_overlay(3).unwrap() else {
        panic!("not ready")
    };
    assert_eq!(
        buttons,
        (0..18)
            .map(|index| KeyOverlayButton {
                name: format!("B{}", index + 1).into(),
                is_pressed: index % 4 == 1,
                count: index - 9,
            })
            .collect::<Vec<_>>()
    );
    let ReadStatus::Ready(buttons_again) = decoder.key_overlay(3).unwrap() else {
        panic!("not ready")
    };
    assert_eq!(buttons, buttons_again);
}

#[test]
fn key_overlay_rejects_more_buttons_than_have_names() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x1000usize;
    let player = 0x3000usize;
    let overlay = 0x4000usize;
    let controller = 0x5000usize;
    let bindable = 0x6000usize;
    let collection = 0x7000usize;
    let items = 0x8000usize;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    memory.ptr(base + offsets.osu_game_osu_game_base.api_k_backing_field, 0);
    memory.ptr(
        base + offsets
            .osu_game_osu_game_base
            .spectator_client_k_backing_field,
        0,
    );
    memory.ptr(
        player
            + offsets
                .osu_game_screens_play_submitting_player
                .api_k_backing_field,
        0,
    );
    memory.ptr(
        player
            + offsets
                .osu_game_screens_play_submitting_player
                .spectator_client_k_backing_field,
        0,
    );
    memory.ptr(
        player
            + offsets
                .osu_game_screens_play_player
                .hudoverlay_k_backing_field,
        overlay,
    );
    memory.ptr(
        overlay
            + offsets
                .osu_game_screens_play_hudoverlay
                .input_count_controller,
        controller,
    );
    memory.ptr(
        controller
            + offsets
                .osu_game_screens_play_hud_input_count_controller
                .triggers,
        bindable,
    );
    memory.ptr(bindable + 0x18, collection);
    memory.ptr(collection + 8, items);
    memory.i32(collection + 0x10, 19);

    let mut decoder = reader(Arc::new(memory));
    decoder.set_resolved_state(base, player);

    assert!(matches!(
        decoder.key_overlay(3),
        Err(MemoryError::Corrupt {
            context: "managed collection",
            ..
        })
    ));
}
#[test]
fn settings_missing_required_config_key_is_a_hard_read_failure() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x1000usize;
    let config = 0x2000usize;
    let store = 0x3000usize;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    memory.ptr(
        base + offsets.osu_game_osu_game_base.local_config_k_backing_field,
        config,
    );
    memory.ptr(config + 0x20, store);
    memory.ptr(store + 0x10, 0);
    memory.i32(store + 0x38, 0);
    let mut decoder = reader(Arc::new(memory));
    decoder.set_resolved_state(base, 0);
    assert!(matches!(
        decoder.settings(),
        Err(MemoryError::Corrupt {
            context: "local config",
            ..
        })
    ));
}
#[test]
fn transient_sparse_collections_fail_without_partial_ready_or_sealed_cache() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x1000usize;
    let available = 0x2000usize;
    let dict = 0x3000usize;
    let entries = 0x4000usize;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    memory.ptr(
        base + offsets.osu_desktop_osu_game_desktop.available_mods,
        available,
    );
    memory.ptr(available + 0x20, dict);
    memory.ptr(dict + 0x10, entries);
    memory.i32(dict + 0x38, 5);
    let mut decoder = reader(Arc::new(memory));
    decoder.set_resolved_state(base, 0);
    assert!(matches!(
        decoder.ensure_mod_mapping(0),
        Err(MemoryError::Corrupt {
            context: "available mods dictionary",
            ..
        })
    ));
    assert!(!decoder.mapped_modes[0]);
    let list = 0x5000usize;
    let array = 0x6000usize;
    let object = 0x7000usize;
    let ty = 0x8000usize;
    let mut memory = SparseMemory::default();
    memory.ptr(list + 8, array);
    memory.i32(list + 0x10, 1);
    memory.ptr(array + 0x10, object);
    memory.ptr(object, ty);
    memory.i32(ty, 0x1000000);
    memory.i32(ty + 3, 8193);
    memory.ptr(object + 0x10, 0);
    let decoder = reader(Arc::new(memory));
    assert!(matches!(
        decoder.read_mod_types(list),
        Err(MemoryError::Corrupt {
            context: "mod array",
            ..
        })
    ));
    let set = 0x9000usize;
    let files = 0xa000usize;
    let file_array = 0xb000usize;
    let mut memory = SparseMemory::default();
    memory.ptr(set + 0x20, files);
    memory.ptr(files + 8, file_array);
    memory.i32(files + 0x10, 1);
    memory.ptr(file_array + 0x10, 0);
    let decoder = reader(Arc::new(memory));
    assert!(matches!(
        decoder.beatmap_files(set),
        Err(MemoryError::Corrupt {
            context: "beatmap file",
            ..
        })
    ));
    assert!(matches!(
        required_non_null(0, "ranked play user", 0xc000),
        Err(MemoryError::Corrupt {
            context: "ranked play user",
            address: 0xc000,
            ..
        })
    ));
    assert!(matches!(
        required_non_null(0, "beatmap author", 0xd000),
        Err(MemoryError::Corrupt {
            context: "beatmap author",
            address: 0xd000,
            ..
        })
    ));
    let mut decoder = reader(Arc::new(SparseMemory::default()));
    assert!(decoder.gameplay(0.0, None).is_err());
    assert!(decoder.key_overlay(0).is_err());
    assert!(decoder.hit_errors(0).is_err());
    assert!(decoder.leaderboard(0).is_err());
}
#[test]
fn current_time_and_mp3_length_preserve_javascript_rounding_and_nonfinite_values() {
    fn read_values(value: f64) -> (f64, f64) {
        let offsets = LazerOffsets::embedded().unwrap();
        let base = 0x1000usize;
        let screen = 0x2000usize;
        let clock = 0x3000usize;
        let final_source = 0x4000usize;
        let decoupled = 0x5000usize;
        let track = 0x6000usize;
        let mut memory = SparseMemory::default();
        memory.valid_base(base, &offsets);
        memory.ptr(base + offsets.osu_game_osu_game_base.beatmap_clock, clock);
        memory.ptr(
            clock
                + offsets
                    .osu_game_beatmaps_framed_beatmap_clock
                    .final_clock_source,
            final_source,
        );
        memory.f64(
            final_source
                + offsets
                    .osu_framework_timing_framed_clock
                    .current_time_k_backing_field,
            value,
        );
        memory.ptr(
            clock
                + offsets
                    .osu_game_beatmaps_framed_beatmap_clock
                    .decoupled_track,
            decoupled,
        );
        memory.ptr(
            decoupled
                + offsets
                    .osu_framework_timing_decoupling_framed_clock
                    .source_k_backing_field,
            track,
        );
        memory.f64(
            track + offsets.osu_framework_audio_track_track.length,
            value,
        );
        memory.ptr(
            base + offsets.osu_game_osu_game_base.api_k_backing_field,
            0x7000,
        );
        memory.ptr(base + offsets.osu_game_osu_game.osu_logo, 0x7100);
        memory.ptr(
            base + offsets
                .osu_game_osu_game_base
                .multiplayer_client_k_backing_field,
            0,
        );
        let song_select = offsets
            .osu_game_screens_select_solo_song_select
            .as_ref()
            .map(|group| group.game_k_backing_field)
            .or_else(|| {
                offsets
                    .osu_game_screens_select_v2_solo_song_select
                    .as_ref()
                    .map(|group| group.game_k_backing_field)
            })
            .unwrap_or(
                offsets
                    .osu_game_screens_select_song_select
                    .game_k_backing_field,
            );
        for offset in [
            offsets
                .osu_game_screens_ranking_solo_results_screen
                .api_k_backing_field,
            offsets.osu_game_screens_osu_screen.logo_k_backing_field,
            song_select,
            offsets.osu_game_screens_edit_editor.api_k_backing_field,
            offsets
                .osu_game_screens_play_submitting_player
                .api_k_backing_field,
            offsets.osu_game_screens_play_player.api_k_backing_field,
        ] {
            memory.ptr(screen + offset, 0);
        }
        let mut decoder = reader(Arc::new(memory));
        decoder.set_resolved_state(base, screen);
        let ReadStatus::Ready(precise) = decoder.global_precise().unwrap() else {
            panic!("not ready")
        };
        let ReadStatus::Ready(length) = decoder.mp3_length().unwrap() else {
            panic!("not ready")
        };
        (precise.time, length)
    }
    for (input, expected) in [(-0.5, 0.0), (-1.5, -1.0), (1.5, 2.0)] {
        assert_eq!(read_values(input), (expected, expected));
    }
    let (time, length) = read_values(f64::NAN);
    assert!(time.is_nan() && length.is_nan());
    assert_eq!(read_values(f64::INFINITY), (f64::INFINITY, f64::INFINITY));
    assert_eq!(
        read_values(f64::NEG_INFINITY),
        (f64::NEG_INFINITY, f64::NEG_INFINITY)
    );
}
#[test]
fn global_precise_player_fast_path_skips_screen_classification_and_reuses_player() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x10_000usize;
    let screen = 0x20_000usize;
    let api = 0x30_000usize;
    let spectator_client = 0x40_000usize;
    let clock = 0x50_000usize;
    let source = 0x60_000usize;
    let drawable = 0x70_000usize;
    let submitting = &offsets.osu_game_screens_play_submitting_player;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    memory.ptr(
        base + offsets.osu_game_osu_game_base.api_k_backing_field,
        api,
    );
    memory.ptr(
        base + offsets
            .osu_game_osu_game_base
            .spectator_client_k_backing_field,
        spectator_client,
    );
    memory.ptr(screen + submitting.api_k_backing_field, api);
    memory.ptr(
        screen + submitting.spectator_client_k_backing_field,
        spectator_client,
    );
    memory.ptr(screen + 0x3f0, 0);
    memory.ptr(
        screen
            + offsets
                .osu_game_screens_play_player
                .drawable_ruleset_k_backing_field,
        drawable,
    );
    memory.ptr(
        drawable
            + offsets
                .osu_game_rulesets_ui_drawable_ruleset
                .replay_score_k_backing_field,
        0x80_000,
    );
    memory.ptr(base + offsets.osu_game_osu_game_base.beatmap_clock, clock);
    memory.ptr(
        clock
            + offsets
                .osu_game_beatmaps_framed_beatmap_clock
                .final_clock_source,
        source,
    );
    memory.f64(
        source
            + offsets
                .osu_framework_timing_framed_clock
                .current_time_k_backing_field,
        321.5,
    );
    let memory = Arc::new(memory);
    let mut decoder = reader(memory.clone());
    decoder.set_resolved_state(base, screen);

    let ReadStatus::Ready(value) = decoder.global_precise().unwrap() else {
        panic!("not ready")
    };
    assert_eq!(value.status, GameState::Play.value() as i64);
    assert_eq!(value.time, 322.0);
    assert!(decoder.watching_replay);
    assert!(!decoder.spectating);
    assert!(!decoder.player_loading);
    assert_eq!(
        memory.read_count(screen + submitting.spectator_client_k_backing_field),
        1,
    );
    let song_select = offsets
        .osu_game_screens_select_solo_song_select
        .as_ref()
        .map(|group| group.game_k_backing_field)
        .or_else(|| {
            offsets
                .osu_game_screens_select_v2_solo_song_select
                .as_ref()
                .map(|group| group.game_k_backing_field)
        })
        .unwrap_or(
            offsets
                .osu_game_screens_select_song_select
                .game_k_backing_field,
        );
    for address in [
        screen + offsets.osu_game_screens_osu_screen.logo_k_backing_field,
        screen
            + offsets
                .osu_game_screens_ranking_solo_results_screen
                .api_k_backing_field,
        screen + song_select,
        screen + offsets.osu_game_screens_edit_editor.api_k_backing_field,
        base + offsets
            .osu_game_osu_game_base
            .multiplayer_client_k_backing_field,
    ] {
        assert_eq!(
            memory.read_count(address),
            0,
            "unexpected read at {address:#x}"
        );
    }
}

#[test]
fn user_ranked_score_crosses_the_javascript_number_boundary() {
    let offsets = LazerOffsets::embedded().unwrap();
    let user = 0x1000usize;
    let stats = 0x2000usize;
    let mode = 0x3000usize;
    let name = 0x4000usize;
    let country = 0x5000usize;
    let mut memory = SparseMemory::default();
    let api_user = &offsets.osu_game_online_api_requests_responses_apiuser;
    let user_stats = &offsets.osu_game_users_user_statistics;
    memory.i32(user + api_user.id_k_backing_field, 42);
    memory.ptr(user + api_user.statistics, stats);
    memory.put(stats + user_stats.pp + 8, [0; 16]);
    memory.f64(stats + user_stats.accuracy, 0.95);
    memory.i64(stats + user_stats.ranked_score, 9_007_199_254_740_993);
    memory.i32(stats + user_stats.level, 100);
    memory.i32(stats + user_stats.play_count, 123);
    memory.i32(stats + user_stats.global_rank + 4, 7);
    memory.string_ptr(
        user + offsets
            .osu_game_online_api_requests_responses_apime
            .play_mode,
        mode,
        "osu",
    );
    memory.ptr(user + api_user.matchmaking_statistics, 0);
    memory.string_ptr(user + api_user.username_k_backing_field, name, "Player");
    memory.string_ptr(user + api_user.country_code_string, country, "US");
    let decoded = reader(Arc::new(memory)).read_user_at(user).unwrap();
    assert_eq!(decoded.ranked_score, 9_007_199_254_740_992.0);
}
#[test]
fn result_and_chat_dates_truncate_dotnet_ticks_to_javascript_milliseconds() {
    const UNIX_EPOCH_TICKS: u64 = 621_355_968_000_000_000;
    let result_date = 0x1000usize;
    let chat_date = 0x2000usize;
    let mut memory = SparseMemory::default();
    memory.i64(result_date, (UNIX_EPOCH_TICKS - 9_999) as i64);
    memory.i64(chat_date, (UNIX_EPOCH_TICKS - 9_999) as i64);
    let process = ProcessHandle::new(Arc::new(memory), ProcessBitness::X64);
    assert_eq!(
        net_date(&process, result_date).unwrap(),
        "1970-01-01T00:00:00.000Z"
    );
    assert_eq!(
        net_date(&process, chat_date).unwrap(),
        "1970-01-01T00:00:00.000Z"
    );
}

fn decode_mod_fixture(acronym: &str, mode: i64) -> Mod {
    let object = 0x1000usize;
    let mut memory = SparseMemory::default();
    for (index, offset) in [0x10usize, 0x18, 0x20, 0x28, 0x30, 0x38, 0x40]
        .into_iter()
        .enumerate()
    {
        let bindable = 0x2000 + index * 0x100;
        memory.ptr(object + offset, bindable);
        memory.f64(bindable + 0x40, 1.25 + index as f64);
        memory.i32(bindable + 0x48, 100 + index as i32);
    }
    if acronym == "EZ" {
        memory.i32(object + 0x20, 3);
    }
    if acronym == "DA" {
        for (index, offset) in [0x10usize, 0x18, 0x28, 0x30].into_iter().enumerate() {
            let outer = 0x4000 + index * 0x100;
            let current = 0x5000 + index * 0x100;
            memory.ptr(object + offset, outer);
            memory.ptr(outer + 0x60, current);
            let value = if offset == 0x18 {
                11.0
            } else {
                4.0 + index as f32
            };
            memory.put(current + 0x40, value.to_le_bytes());
        }
        let extended = 0x6000;
        memory.ptr(object + 0x20, extended);
        memory.put(extended + 0x40, [1]);
        if mode == 2 {
            let hard_rock = 0x6100;
            memory.ptr(object + 0x38, hard_rock);
            memory.put(hard_rock + 0x40, [1]);
        }
    }
    let mut decoder = reader(Arc::new(memory));
    decoder.selected_gamemode = mode;
    decoder.read_mod(acronym, object).unwrap()
}

#[test]
fn difficulty_adjust_extended_od_survives_structured_mod_decoding() {
    assert_eq!(
        serde_json::to_value(decode_mod_fixture("DA", 0)).unwrap(),
        serde_json::json!({
            "acronym": "DA",
            "settings": {
                "approach_rate": 7.0,
                "circle_size": 6.0,
                "drain_rate": 4.0,
                "overall_difficulty": 11.0,
                "extended_limits": true,
            },
        }),
    );
}

#[test]
fn every_supported_mod_setting_decoder_has_observable_keys() {
    let cases: &[(&str, i64, &[&str])] = &[
        ("EZ", 0, &["retries"]),
        ("EZ", 1, &[]),
        ("HT", 0, &["speed_change", "adjust_pitch"]),
        ("DT", 0, &["speed_change", "adjust_pitch"]),
        ("DC", 0, &["speed_change"]),
        ("NC", 0, &["speed_change"]),
        ("SD", 0, &["fail_on_slider_tail", "restart"]),
        ("SD", 1, &["restart"]),
        ("PF", 3, &["require_perfect_hits", "restart"]),
        ("PF", 0, &["restart"]),
        ("HD", 0, &["only_fade_approach_circles"]),
        ("HD", 1, &[]),
        (
            "FL",
            0,
            &["follow_delay", "combo_based_size", "size_multiplier"],
        ),
        ("FL", 1, &["size_multiplier", "combo_based_size"]),
        (
            "AC",
            0,
            &["minimum_accuracy", "accuracy_judge_mode", "restart"],
        ),
        ("TP", 0, &["seed", "metronome"]),
        (
            "DA",
            0,
            &[
                "approach_rate",
                "circle_size",
                "drain_rate",
                "overall_difficulty",
                "extended_limits",
            ],
        ),
        (
            "DA",
            1,
            &[
                "scroll_speed",
                "drain_rate",
                "overall_difficulty",
                "extended_limits",
            ],
        ),
        (
            "DA",
            2,
            &[
                "approach_rate",
                "circle_size",
                "hard_rock_offsets",
                "drain_rate",
                "overall_difficulty",
                "extended_limits",
            ],
        ),
        (
            "DA",
            3,
            &["drain_rate", "overall_difficulty", "extended_limits"],
        ),
        (
            "CL",
            0,
            &[
                "no_slider_head_accuracy",
                "classic_note_lock",
                "always_play_tail_sample",
                "fade_hit_circle_early",
                "classic_health",
            ],
        ),
        ("CL", 1, &[]),
        ("RD", 0, &["angle_sharpness", "seed"]),
        ("RD", 1, &["seed"]),
        ("RD", 2, &["angle_sharpness", "seed"]),
        ("RD", 3, &["seed"]),
        ("MR", 0, &["reflection"]),
        ("MR", 1, &["reflection"]),
        ("MR", 2, &[]),
        ("WG", 0, &["strength"]),
        ("GR", 0, &["start_scale"]),
        ("DF", 0, &["start_scale"]),
        ("WU", 0, &["initial_rate", "final_rate", "adjust_pitch"]),
        ("WD", 0, &["initial_rate", "final_rate", "adjust_pitch"]),
        ("BR", 0, &["spin_speed", "direction"]),
        ("AD", 0, &["scale", "style"]),
        (
            "MU",
            0,
            &[
                "inverse_muting",
                "enable_metronome",
                "mute_combo_count",
                "affects_hit_sounds",
            ],
        ),
        ("NS", 0, &["hidden_combo_count"]),
        ("NS", 2, &["hidden_combo_count"]),
        ("MG", 0, &["attraction_strength"]),
        ("RP", 0, &["repulsion_strength"]),
        ("AS", 0, &["initial_rate", "adjust_pitch"]),
        ("DP", 0, &["max_depth", "show_approach_circles"]),
        ("CO", 0, &["coverage", "direction"]),
        ("BM", 0, &["max_size_combo_count", "max_cursor_size"]),
        (
            "SR",
            0,
            &[
                "one_third_conversion",
                "one_sixth_conversion",
                "one_eighth_conversion",
            ],
        ),
    ];

    for &(acronym, mode, expected) in cases {
        let decoded = decode_mod_fixture(acronym, mode);
        let keys = decoded
            .settings
            .as_ref()
            .map(|settings| settings.keys().map(String::as_str).collect::<Vec<_>>())
            .unwrap_or_default();
        assert_eq!(keys, expected, "{acronym} mode {mode}");
    }
}
