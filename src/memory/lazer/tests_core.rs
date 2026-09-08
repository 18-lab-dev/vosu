use super::test_support::*;
use super::*;

fn hit_object(address: usize) -> usize {
    0x100_000 + address
}

fn put_hit_event(
    memory: &mut SparseMemory,
    address: usize,
    type_size: i32,
    error: f64,
    result: i32,
) {
    let object = hit_object(address);
    let type_info = 0x200_000 + address;
    memory.ptr(address, object);
    memory.ptr(object, type_info);
    memory.i32(type_info + 4, type_size);
    memory.f64(address + 0x10, error);
    memory.i32(address + 0x18, result);
}

fn put_hold_aggregate(memory: &mut SparseMemory, address: usize, type_size: i32, tail: usize) {
    put_hit_event(memory, address, type_size, 999.0, 0);
    let object = hit_object(address);
    memory.ptr(object + 0x60, tail);
    memory.ptr(object + 0x68, 0x300_000 + address);
}

#[test]
fn source_default_pattern_is_exact_and_uses_normal_masking() {
    let pattern = LazerPattern::source_default();
    assert_eq!(pattern.offset, 0);
    assert_eq!(
        pattern.scaling_container_target_draw_size.bytes(),
        &[0x00, 0x00, 0x80, 0x44, 0x00, 0x00, 0x40, 0x44],
    );
    assert_eq!(pattern.scaling_container_target_draw_size.mask(), &[1; 8],);
    assert!(!pattern.scaling_container_target_draw_size.non_zero_mask());
}

#[test]
fn ranked_status_maps_known_values_and_keeps_unknown_undefined() {
    assert_eq!(
        [-4, -3, -2, -1, 0, 1, 2, 3, 4].map(lazer_ranked_status),
        [1.0, 1.0, 2.0, 2.0, 2.0, 4.0, 5.0, 6.0, 7.0]
    );
    assert!(lazer_ranked_status(5).is_nan());
    assert!(lazer_ranked_status(i32::MIN).is_nan());
}

#[test]
fn anchor_sweeps_deltas_and_validates_vtable() {
    let offsets = LazerOffsets::embedded().unwrap();
    let anchor = 0x4000usize;
    let opener = 0x5000usize;
    let api = 0x6000usize;
    let base = 0x7000usize;
    let vtable = 0x8000usize;
    let mut memory = SparseMemory::default();
    memory.ptr(anchor - 0x2c, opener);
    memory.ptr(
        opener
            + offsets
                .osu_game_online_chat_external_link_opener
                .api_k_backing_field,
        api,
    );
    memory.ptr(api + offsets.osu_game_online_api_apiaccess.game, base);
    memory.ptr(base, vtable);
    memory.i64(vtable, offsets.effective_game_base_vtable() as i64);
    let reader = reader(Arc::new(memory));
    assert_eq!(
        reader.resolve_game_base_from_anchor(anchor).unwrap(),
        Some(base)
    );
}

#[test]
fn anchor_rejects_wrong_vtable() {
    let offsets = LazerOffsets::embedded().unwrap();
    let anchor = 0x4000usize;
    let mut memory = SparseMemory::default();
    memory.ptr(anchor - 0x24, 0x5000);
    memory.ptr(
        0x5000
            + offsets
                .osu_game_online_chat_external_link_opener
                .api_k_backing_field,
        0x6000,
    );
    memory.ptr(0x6000 + offsets.osu_game_online_api_apiaccess.game, 0x7000);
    memory.ptr(0x7000, 0x8000);
    memory.i64(0x8000, 1);
    assert_eq!(
        reader(Arc::new(memory))
            .resolve_game_base_from_anchor(anchor)
            .unwrap(),
        None
    );
}

#[test]
fn game_base_validation_cache_is_capture_scoped() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x7000usize;
    let vtable = base + 0x800;
    let invalid_base = 0x9000usize;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    let memory = Arc::new(memory);
    let mut reader = reader(memory.clone());
    reader.set_resolved_state(base, 0);

    assert_eq!(reader.game_base().unwrap(), base);
    assert_eq!(reader.game_base().unwrap(), base);
    assert_eq!(memory.read_count(base), 1);
    assert_eq!(memory.read_count(vtable), 1);

    *reader
        .game_base_address
        .write()
        .expect("lazer base lock poisoned") = invalid_base;
    assert_eq!(reader.game_base().unwrap(), base);
    assert_eq!(memory.read_count(invalid_base), 0);

    reader.synchronize_capture_config(false, true);
    assert!(reader.game_base().is_err());
    assert_eq!(memory.read_count(invalid_base), 1);
}

#[test]
fn singleton_pointer_caches_are_capture_scoped() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x10000usize;
    let api_field = base + offsets.osu_game_osu_game_base.api_k_backing_field;
    let spectator_field = base
        + offsets
            .osu_game_osu_game_base
            .spectator_client_k_backing_field;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    memory.ptr(api_field, 0x30000);
    memory.ptr(spectator_field, 0x31000);
    let memory = Arc::new(memory);
    let mut reader = reader(memory.clone());
    reader.set_resolved_state(base, 0);

    assert_eq!(reader.api().unwrap(), 0x30000);
    assert_eq!(reader.api().unwrap(), 0x30000);
    assert_eq!(reader.spectator_client().unwrap(), 0x31000);
    assert_eq!(reader.spectator_client().unwrap(), 0x31000);
    assert_eq!(memory.read_count(api_field), 1);
    assert_eq!(memory.read_count(spectator_field), 1);

    reader.synchronize_capture_config(false, true);
    assert_eq!(reader.api().unwrap(), 0x30000);
    assert_eq!(reader.spectator_client().unwrap(), 0x31000);
    assert_eq!(memory.read_count(api_field), 2);
    assert_eq!(memory.read_count(spectator_field), 2);
}

#[test]
fn screen_predicates_require_matching_singletons() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x10000usize;
    let screen = 0x20000usize;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    memory.ptr(
        base + offsets.osu_game_osu_game_base.api_k_backing_field,
        0x30000,
    );
    memory.ptr(
        base + offsets
            .osu_game_osu_game_base
            .spectator_client_k_backing_field,
        0x31000,
    );
    memory.ptr(
        screen
            + offsets
                .osu_game_screens_play_submitting_player
                .api_k_backing_field,
        0x30000,
    );
    memory.ptr(
        screen
            + offsets
                .osu_game_screens_play_submitting_player
                .spectator_client_k_backing_field,
        0x31000,
    );
    let mut reader = reader(Arc::new(memory));
    reader.set_resolved_state(base, screen);
    assert!(reader.check_player(screen).unwrap());
    assert!(!reader.check_player(0).unwrap());
}

#[test]
fn player_classifier_preserves_replay_fallback_when_submitting_api_differs() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x10000usize;
    let screen = 0x20000usize;
    let api = 0x30000usize;
    let score_manager = 0x31000usize;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    memory.ptr(
        base + offsets.osu_game_osu_game_base.api_k_backing_field,
        api,
    );
    memory.ptr(
        screen
            + offsets
                .osu_game_screens_play_submitting_player
                .api_k_backing_field,
        api + 1,
    );
    memory.ptr(
        screen + offsets.osu_game_screens_play_player.api_k_backing_field,
        api,
    );
    memory.ptr(
        screen
            + offsets
                .osu_game_screens_play_player
                .score_manager_k_backing_field,
        score_manager,
    );
    memory.ptr(
        base + offsets.osu_game_osu_game_base.score_manager_k_backing_field,
        score_manager,
    );

    let mut reader = reader(Arc::new(memory));
    reader.set_resolved_state(base, screen);
    assert!(reader.check_player(screen).unwrap());
}

#[test]
fn managed_collection_layouts_and_statistics_are_exact() {
    let dict = 0x1000usize;
    let entries = 0x2000usize;
    let array = 0x3000usize;
    let mut memory = SparseMemory::default();
    memory.ptr(dict + 0x10, entries);
    memory.i32(dict + 0x38, 2);
    memory.put(entries + 0x10, [0u8; 0x20]);
    memory.i32(entries + 0x18, 5);
    memory.i32(entries + 0x1c, 123);
    memory.i32(entries + 0x28, 1);
    memory.i32(entries + 0x2c, 4);
    memory.i32(array + 8, 2);
    memory.i32(array + 0x10, 20_000_000);
    let reader = reader(Arc::new(memory));
    let stats = reader.statistics_dict(dict).unwrap();
    assert_eq!((stats.great, stats.miss), (123, 4));
    assert_eq!(reader.hybrid_list_info(array).unwrap(), (2, array + 0x10));
}

#[test]
fn collection_header_errors_preserve_scalar_field_precedence() {
    let list = 0x1000;
    for marker_present in [false, true] {
        let mut memory = SparseMemory::default();
        if marker_present {
            memory.i32(list + 0x10, 1);
        }
        let reader = reader(Arc::new(memory));
        let error = reader.hybrid_list_info(list).unwrap_err();
        let expected = if marker_present {
            (list + 8, 8)
        } else {
            (list + 0x10, 4)
        };
        assert!(
            matches!(error, MemoryError::Read { address, len, .. } if (address, len) == expected)
        );
    }
}

#[test]
fn collection_header_ignores_padding_after_x86_pointer() {
    let list = 0x1000;
    let array = 0x2000;
    let mut memory = SparseMemory::default();
    memory.i32(list + 8, array);
    memory.i32(list + 0xc, 0x1234);
    memory.i32(list + 0x10, 1);
    let reader = LazerMemory::new(
        ProcessHandle::new(Arc::new(memory), ProcessBitness::X86),
        Arc::new(LazerOffsets::embedded().unwrap()),
        LazerPattern::source_default(),
    );
    assert_eq!(
        reader.hybrid_list_info(list).unwrap(),
        (1, array as usize + 0x10)
    );
}

#[test]
fn hit_event_size_result_and_identity_rules_match_modes() {
    let event = 0x1000usize;
    let object = 0x2000usize;
    let type_info = 0x3000usize;
    let tail_88 = 0x4000usize;
    let mut memory = SparseMemory::default();
    memory.ptr(event, object);
    memory.ptr(object, type_info);
    memory.i32(type_info + 4, 0xe8);
    memory.f64(event + 0x10, -12.5);
    memory.i32(event + 0x18, 3);
    put_hit_event(&mut memory, event + 0x40, 0x58, 4.5, 2);
    put_hold_aggregate(&mut memory, event + 0x80, 0x88, tail_88);
    put_hit_event(&mut memory, event + 0xc0, 0x90, 999.0, 0);
    put_hit_event(&mut memory, event + 0x100, 0x98, 999.0, 0);
    let mut reader = reader(Arc::new(memory));
    reader.selected_gamemode = 0;
    assert_eq!(
        reader.read_hit_event(event).unwrap(),
        Some(HitErrorEvent::Timing {
            value: -12.5,
            object,
        })
    );
    reader.selected_gamemode = 3;
    assert_eq!(
        reader.read_hit_event(event + 0x40).unwrap(),
        Some(HitErrorEvent::Timing {
            value: 4.5,
            object: hit_object(event + 0x40),
        })
    );
    assert_eq!(
        reader.read_hit_event(event + 0x80).unwrap(),
        Some(HitErrorEvent::ManiaHoldAggregate { tail: tail_88 })
    );
    assert_eq!(reader.read_hit_event(event + 0xc0).unwrap(), None);
    assert_eq!(reader.read_hit_event(event + 0x100).unwrap(), None);
    reader.selected_gamemode = 2;
    assert_eq!(reader.read_hit_event(event).unwrap(), None);
}

#[test]
fn non_mania_duplicate_timings_remain_press_only() {
    let items = 0x10_000usize;
    let mut memory = SparseMemory::default();
    put_hit_event(&mut memory, items, 0xe8, -2.0, 3);
    put_hit_event(&mut memory, items + 0x40, 0xe0, -2.0, 10);
    put_hit_event(&mut memory, items + 0x80, 0x68, -2.0, 5);
    put_hit_event(&mut memory, items + 0xc0, 0x50, -2.0, 2);
    let mut reader = reader(Arc::new(memory));

    for mode in [0, 1] {
        reader.selected_gamemode = mode;
        let suffix = reader.read_hit_error_suffix(items, 4, 0).unwrap();
        assert_eq!(suffix.errors.all, [-2.0, -2.0]);
        assert_eq!(suffix.errors.press, [-2.0, -2.0]);
        assert!(suffix.errors.release.is_empty());
        assert!(suffix.release_corrections.is_empty());
    }

    reader.selected_gamemode = 2;
    let catch = reader.read_hit_error_suffix(items, 4, 0).unwrap();
    assert!(catch.errors.all.is_empty());
    assert!(catch.release_corrections.is_empty());
}

#[test]
fn non_mania_suffix_stops_before_an_invalid_timing() {
    let items = 0x10_000usize;
    let mut memory = SparseMemory::default();
    put_hit_event(&mut memory, items, 0xe8, 1.25, 3);
    put_hit_event(&mut memory, items + 0x40, 0xe8, 10_001.0, 3);
    let mut reader = reader(Arc::new(memory));
    reader.selected_gamemode = 0;
    // The third event is absent: stopping must precede any attempt to read it.
    let suffix = reader.read_hit_error_suffix(items, 3, 0).unwrap();
    assert_eq!(suffix.index, 1);
    assert_eq!(suffix.errors.all, [1.25]);
    assert_eq!(suffix.errors.press, [1.25]);
    assert!(suffix.errors.release.is_empty());
}

#[test]
fn mania_suffix_classifies_interleaved_duplicate_timings_by_object_identity() {
    let items = 0x10_000usize;
    let mut memory = SparseMemory::default();
    put_hit_event(&mut memory, items, 0x58, 7.0, 2);
    put_hit_event(&mut memory, items + 0x40, 0x58, -2.0, 2);
    put_hit_event(&mut memory, items + 0x80, 0x58, -2.0, 2);
    put_hold_aggregate(&mut memory, items + 0xc0, 0x88, hit_object(items + 0x40));
    put_hit_event(&mut memory, items + 0x100, 0x58, -2.0, 2);
    put_hit_event(&mut memory, items + 0x140, 0x58, -2.0, 2);
    put_hold_aggregate(&mut memory, items + 0x180, 0x88, hit_object(items + 0x100));
    let mut reader = reader(Arc::new(memory));
    reader.selected_gamemode = 3;

    let suffix = reader.read_hit_error_suffix(items, 7, 0).unwrap();

    assert_eq!(suffix.index, 7);
    assert_eq!(suffix.errors.all, [7.0, -2.0, -2.0, -2.0, -2.0]);
    assert_eq!(suffix.errors.press, [7.0, -2.0, -2.0]);
    assert_eq!(suffix.errors.release, [-2.0, -2.0]);
    assert_eq!(
        suffix.errors.all.len(),
        suffix.errors.press.len() + suffix.errors.release.len()
    );
    assert!(suffix.release_corrections.is_empty());
}

#[test]
fn suffix_boundary_correction_targets_prior_press_offset_and_skips_known_tail() {
    let items = 0x10_000usize;
    let mut memory = SparseMemory::default();
    put_hit_event(&mut memory, items, 0x58, -2.0, 2);
    put_hit_event(&mut memory, items + 0x40, 0x58, 10.0, 2);
    put_hit_event(&mut memory, items + 0x80, 0x58, 20.0, 2);
    put_hit_event(&mut memory, items + 0xc0, 0x58, 30.0, 2);
    put_hold_aggregate(&mut memory, items + 0x100, 0x88, hit_object(items + 0x80));
    put_hit_event(&mut memory, items + 0x140, 0x58, 40.0, 2);
    put_hold_aggregate(&mut memory, items + 0x180, 0x88, hit_object(items));
    put_hit_event(&mut memory, items + 0x1c0, 0x58, 50.0, 2);
    let mut reader = reader(Arc::new(memory));
    reader.selected_gamemode = 3;

    let before_boundary = reader.read_hit_error_suffix(items, 6, 0).unwrap();
    let suffix = reader.read_hit_error_suffix(items, 8, 6).unwrap();

    assert_eq!(before_boundary.errors.press, [-2.0, 10.0, 30.0, 40.0]);
    assert_eq!(before_boundary.errors.release, [20.0]);
    assert!(before_boundary.release_corrections.is_empty());
    assert_eq!(suffix.errors.all, [50.0]);
    assert_eq!(suffix.errors.press, [50.0]);
    assert!(suffix.errors.release.is_empty());
    assert_eq!(
        suffix.release_corrections,
        [ReleaseCorrection {
            value: -2.0,
            press_offset: 3,
        }]
    );
    assert_eq!(
        suffix.errors.all.len(),
        suffix.errors.press.len() + suffix.errors.release.len()
    );
}

#[test]
fn suffix_boundary_emits_multiple_identity_corrections_from_one_press_vector() {
    let items = 0x10_000usize;
    let mut memory = SparseMemory::default();
    put_hit_event(&mut memory, items, 0x58, -2.0, 2);
    put_hit_event(&mut memory, items + 0x40, 0x58, 10.0, 2);
    put_hit_event(&mut memory, items + 0x80, 0x58, -2.0, 2);
    put_hit_event(&mut memory, items + 0xc0, 0x58, 20.0, 2);
    put_hold_aggregate(&mut memory, items + 0x100, 0x88, hit_object(items));
    put_hit_event(&mut memory, items + 0x140, 0x58, -2.0, 2);
    put_hold_aggregate(&mut memory, items + 0x180, 0x88, hit_object(items + 0x80));
    let mut reader = reader(Arc::new(memory));
    reader.selected_gamemode = 3;

    let suffix = reader.read_hit_error_suffix(items, 7, 4).unwrap();

    assert_eq!(suffix.errors.all, [-2.0]);
    assert_eq!(suffix.errors.press, [-2.0]);
    assert!(suffix.errors.release.is_empty());
    assert_eq!(
        suffix.release_corrections,
        [
            ReleaseCorrection {
                value: -2.0,
                press_offset: 3,
            },
            ReleaseCorrection {
                value: -2.0,
                press_offset: 1,
            },
        ]
    );
    assert_eq!(
        suffix.errors.all.len(),
        suffix.errors.press.len() + suffix.errors.release.len()
    );
}

#[test]
fn hit_error_suffix_rewinds_to_current_size_when_list_shrinks() {
    let items = 0x10_000usize;
    let mut memory = SparseMemory::default();
    put_hit_event(&mut memory, items, 0xe8, -2.0, 3);
    put_hit_event(&mut memory, items + 0x40, 0xe0, 4.0, 5);
    let mut reader = reader(Arc::new(memory));
    reader.selected_gamemode = 0;

    let rewound = reader.read_hit_error_suffix(items, 2, 10).unwrap();
    assert_eq!(rewound.index, 2);
    assert!(rewound.errors.all.is_empty());
    assert!(rewound.errors.press.is_empty());
    assert!(rewound.release_corrections.is_empty());

    let cleared = reader.read_hit_error_suffix(items, 0, 10).unwrap();
    assert_eq!(cleared.index, 0);
    assert!(cleared.errors.all.is_empty());
}

#[test]
fn mod_mapping_normalizes_settings_and_hashes_json() {
    let mut settings = IndexMap::new();
    settings.insert("speed_change".into(), Value::from(1.3333));
    settings.insert("style".into(), Value::from(2));
    let mods = calculate_mods(vec![
        Mod {
            acronym: "NC".into(),
            settings: Some(settings),
        },
        Mod {
            acronym: "HD".into(),
            settings: None,
        },
    ]);
    assert_eq!(mods.number, 584);
    assert_eq!(mods.name, "HDNC");
    assert_eq!(mods.rate, 1.33);
    assert_eq!(mods.array[0].settings.as_ref().unwrap()["style"], "2");
    assert_eq!(mods.checksum, "609317c090a4a8a102ed229874fb548e");
    let mut integral = IndexMap::new();
    integral.insert("circle_size".into(), Value::from(1.0));
    assert_eq!(
        calculate_mods(vec![Mod {
            acronym: "DA".into(),
            settings: Some(integral)
        }])
        .checksum,
        "f44d7a9cef27c056d526130b90cac6f3"
    );
}

#[test]
fn audio_amplitudes_match_source_sequential_reads() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x1000usize;
    let logo = 0x2000usize;
    let visualizer = 0x3000usize;
    let array = 0x4000usize;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    memory.ptr(base + offsets.osu_game_osu_game.osu_logo, logo);
    memory.ptr(
        logo + offsets.osu_game_screens_menu_osu_logo.visualizer,
        visualizer,
    );
    memory.ptr(
        visualizer
            + offsets
                .osu_game_screens_menu_logo_visualisation
                .frequency_amplitudes,
        array,
    );
    for index in 0..40 {
        memory.put(array + 0x10 + index * 4, (index as f32).to_le_bytes());
    }
    let memory = Arc::new(memory);
    let mut reader = reader(memory.clone());
    reader.set_resolved_state(base, 0);
    let ReadStatus::Ready(values) = reader.audio_velocity_base().unwrap() else {
        panic!("not ready")
    };
    assert_eq!((values.len(), values[39]), (40, 39.0));
    assert_eq!(memory.batches.load(Ordering::Relaxed), 0);
}

#[test]
fn config_dictionary_updates_settings_and_runtime_flags() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x1000usize;
    let local = 0x2000usize;
    let local_store = 0x3000usize;
    let local_entries = 0x4000usize;
    let framework = 0x8000usize;
    let framework_store = 0x9000usize;
    let framework_entries = 0xa000usize;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    memory.ptr(
        base + offsets.osu_game_osu_game_base.local_config_k_backing_field,
        local,
    );
    memory.ptr(local + 0x20, local_store);
    memory.ptr(local_store + 0x10, local_entries);
    let local_keys = [
        34, 58, 30, 40, 39, 12, 88, 6, 7, 22, 23, 2, 3, 4, 10, 19, 20, 56, 54,
    ];
    memory.i32(local_store + 0x38, local_keys.len() as i32);
    for (index, key) in local_keys.into_iter().enumerate() {
        let entry = local_entries + 0x10 + index * 0x18;
        let boxed = 0x10000 + index * 0x100;
        memory.ptr(entry, boxed);
        memory.i32(entry + 0x10, key);
        memory.put(boxed + 0x40, [0u8; 8]);
    }
    memory.i32(0x10100 + 0x40, 1);
    memory.put(0x10500 + 0x40, [1]);
    memory.f64(0x10700 + 0x40, 0.425);
    memory.ptr(
        base + offsets.osu_game_osu_game.framework_config_k_backing_field,
        framework,
    );
    memory.ptr(framework + 0x20, framework_store);
    memory.ptr(framework_store + 0x10, framework_entries);
    let framework_keys = [14, 7, 3, 5, 4, 11, 21];
    memory.i32(framework_store + 0x38, framework_keys.len() as i32);
    for (index, key) in framework_keys.into_iter().enumerate() {
        let entry = framework_entries + 0x10 + index * 0x18;
        let boxed = 0x20000 + index * 0x100;
        memory.ptr(entry, boxed);
        memory.i32(entry + 0x10, key);
        memory.put(boxed + 0x40, [0u8; 16]);
    }
    let cache = 0x28000usize;
    let dict = 0x29000usize;
    memory.ptr(
        base + offsets.osu_game_osu_game_base.ruleset_config_cache,
        cache,
    );
    memory.ptr(
        cache + offsets.osu_game_rulesets_ruleset_config_cache.config_cache,
        dict,
    );
    memory.ptr(dict + 0x10, 0);
    memory.i32(dict + 0x38, 0);
    let mut reader = reader(Arc::new(memory));
    reader.set_resolved_state(base, 0);
    let values = reader.read_osu_config().unwrap();
    assert_eq!(reader.scoring_display_mode, 1);
    assert!(reader.leaderboard_visible);
    assert_eq!(values["background.dim"], 42.5);
}

#[test]
fn ranked_dictionary_decodes_users_and_rejects_bad_bounds() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x1000usize;
    let client = 0x2000usize;
    let connected = 0x2100usize;
    let room = 0x3000usize;
    let state = 0x4000usize;
    let dict = 0x5000usize;
    let entries = 0x6000usize;
    let user = 0x7000usize;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    memory.ptr(
        base + offsets
            .osu_game_osu_game_base
            .multiplayer_client_k_backing_field,
        client,
    );
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
    memory.ptr(
        room + offsets
            .osu_game_online_multiplayer_multiplayer_room
            .match_state_k_backing_field,
        state,
    );
    let o = &offsets.osu_game_online_multiplayer_match_types_ranked_play_ranked_play_room_state;
    memory.i32(state + o.stage_k_backing_field, 7);
    memory.i32(state + o.current_round_k_backing_field, 3);
    memory.f64(state + o.star_rating_k_backing_field, f64::NAN);
    memory.f64(state + o.damage_multiplier_k_backing_field, 1.25);
    memory.ptr(state + o.users_k_backing_field, dict);
    memory.put(state + o.active_user_id_k_backing_field, [1]);
    memory.i32(state + o.active_user_id_k_backing_field + 4, 42);
    memory.put(state + o.winning_user_id_k_backing_field, [0]);
    memory.ptr(dict + 0x10, entries);
    memory.i32(entries + 8, 1);
    memory.ptr(entries + 0x10, user);
    memory.i32(entries + 0x18, 1);
    memory.i32(entries + 0x20, 42);
    let u = &offsets.osu_game_online_multiplayer_match_types_ranked_play_ranked_play_user_info;
    memory.i32(user + u.rating_k_backing_field, 1234);
    memory.i32(user + u.life_k_backing_field, 2);
    memory.i32(user + u.rounds_won_k_backing_field, 1);
    memory.f64(user + u.damage_multiplier_k_backing_field, 1.5);
    let mut reader = reader(Arc::new(memory));
    reader.set_resolved_state(base, 0);
    let ReadStatus::Ready(value) = reader.ranked_play().unwrap() else {
        panic!("not ready")
    };
    assert_eq!(
        (
            value.active_user_id,
            value.winning_user_id,
            value.users[0].id
        ),
        (42, -1, 42)
    );
    assert_eq!((value.users[0].rating, value.users[0].life), (1234.0, 2.0));
    assert!(value.star_rating.is_nan());
}
#[test]
fn ranked_validation_short_circuits_later_unreadable_fields() {
    fn read(round: i32, stars: Option<f64>) -> MemoryRead<RankedPlay> {
        let offsets = LazerOffsets::embedded().unwrap();
        let base = 0x1000usize;
        let client = 0x2000usize;
        let connected = 0x2100usize;
        let room = 0x3000usize;
        let state = 0x4000usize;
        let mut memory = SparseMemory::default();
        memory.valid_base(base, &offsets);
        memory.ptr(
            base + offsets
                .osu_game_osu_game_base
                .multiplayer_client_k_backing_field,
            client,
        );
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
        memory.ptr(
            room + offsets
                .osu_game_online_multiplayer_multiplayer_room
                .match_state_k_backing_field,
            state,
        );
        let o = &offsets.osu_game_online_multiplayer_match_types_ranked_play_ranked_play_room_state;
        memory.i32(state + o.stage_k_backing_field, 7);
        memory.i32(state + o.current_round_k_backing_field, round);
        if let Some(value) = stars {
            memory.f64(state + o.star_rating_k_backing_field, value);
        }
        let mut decoder = reader(Arc::new(memory));
        decoder.set_resolved_state(base, 0);
        decoder.ranked_play()
    }
    assert_eq!(read(0, None).unwrap(), ReadStatus::NotReady);
    assert_eq!(read(3, Some(21.0)).unwrap(), ReadStatus::NotReady);
}

#[test]
fn loading_sentinels_and_display_score_boundaries_match_source() {
    let mut reader = reader(Arc::new(SparseMemory::default()));
    reader.player_loading = true;
    assert_eq!(
        reader.key_overlay(0).unwrap(),
        ReadStatus::Ready(Vec::new())
    );
    assert_eq!(
        reader.hit_errors(99).unwrap(),
        ReadStatus::Ready(HitErrors {
            index: 0,
            errors: HitErrorArray::default(),
            release_corrections: Vec::new(),
            stable_replay_frames: None,
        })
    );
    assert_eq!(display_score(3, 765_432.0, 100), 765_432.0);
    assert_eq!(display_score(2, 1_000_000.0, 10), 102_162.0);
    assert_eq!(display_score(1, -15.0, 0), -1.0);
    assert_eq!(js_long(9_007_199_254_740_993), 9_007_199_254_740_992.0);
    assert_eq!(js_long(9_007_199_254_740_995), 9_007_199_254_740_996.0);
}

#[test]
fn unsupported_and_pure_boundary_semantics_are_stable() {
    let mut reader = reader(Arc::new(SparseMemory::default()));
    assert!(matches!(
        reader.tourney().unwrap(),
        ReadStatus::Unsupported("Lazer: tourney not implemented")
    ));
    assert!(matches!(
        reader.tourney_chat(&[]).unwrap(),
        ReadStatus::Unsupported(_)
    ));
    assert!(matches!(
        reader.tourney_user().unwrap(),
        ReadStatus::Unsupported(_)
    ));
    assert_eq!(country_code("US"), 225);
    assert_eq!(country_code("invalid"), 0);
    assert_eq!(
        lazer_path("abcdef"),
        Path::new("a").join("ab").join("abcdef").to_string_lossy()
    );
    assert_eq!(
        lazer_path("a"),
        Path::new("a").join("a").join("a").to_string_lossy()
    );
    #[cfg(windows)]
    assert!(lazer_path("abcdef").contains('\\'));
    assert!(matches!(
        add(usize::MAX, 1),
        Err(MemoryError::AddressOverflow { .. })
    ));
}
#[test]
fn score_reader_decodes_realm_user_stats_and_json_mods() {
    let offsets = LazerOffsets::embedded().unwrap();
    let info = 0x1000usize;
    let realm = 0x3000usize;
    let ruleset = 0x3500usize;
    let mut memory = SparseMemory::default();
    let o = &offsets.osu_game_scoring_score_info;
    memory.ptr(info + o.statistics, 0);
    memory.ptr(info + o.maximum_statistics, 0);
    memory.ptr(info + o.ruleset_k_backing_field, ruleset);
    memory.i32(
        ruleset
            + offsets
                .osu_game_rulesets_ruleset_info
                .online_id_k_backing_field,
        3,
    );
    memory.ptr(info + o.user, 0);
    memory.ptr(info + o.realm_user_k_backing_field, realm);
    memory.string_ptr(
        realm + offsets.osu_game_models_realm_user.username_k_backing_field,
        0x4000,
        "Autoplay",
    );
    memory.string_ptr(
        info + o.mods_json_k_backing_field,
        0x5000,
        r#"[{"acronym":"DT","settings":{"speed_change":1.25}}]"#,
    );
    memory.i64(info + o.total_score_k_backing_field, 9_007_199_254_740_995);
    memory.f64(info + o.accuracy_k_backing_field, 0.9876);
    memory.i32(info + o.combo_k_backing_field, 123);
    memory.i32(info + o.max_combo_k_backing_field, 456);
    let mut decoder = reader(Arc::new(memory));
    decoder.selected_gamemode = 3;
    let score = decoder.read_score(info, 0.75, 2).unwrap();
    assert_eq!(
        (
            score.player_name.as_str(),
            score.mode,
            score.score,
            score.combo,
            score.max_combo
        ),
        ("osu!", 3, 9_007_199_254_740_996.0, 0, 456)
    );
    assert_eq!(score.accuracy, 98.76);
    assert_eq!(score.mods.rate, 1.25);
}

#[test]
fn empty_spectator_room_is_ready_and_rounding_matches_javascript() {
    let offsets = LazerOffsets::embedded().unwrap();
    let base = 0x1000usize;
    let screen = 0x3000usize;
    let gameplay = 0x4000usize;
    let client = 0x5000usize;
    let room = 0x6000usize;
    let users = 0x7000usize;
    let manager = 0x8000usize;
    let joined = 0x9000usize;
    let collection = 0xa000usize;
    let array = 0xb000usize;
    let mut memory = SparseMemory::default();
    memory.valid_base(base, &offsets);
    memory.ptr(
        screen
            + offsets
                .osu_game_screens_spectate_spectator_screen
                .gameplay_states,
        gameplay,
    );
    memory.ptr(gameplay + 0x10, 0);
    memory.i32(gameplay + 0x38, 0);
    memory.ptr(
        base + offsets
            .osu_game_osu_game_base
            .multiplayer_client_k_backing_field,
        client,
    );
    memory.ptr(
        client + offsets.osu_game_online_multiplayer_multiplayer_client.room,
        room,
    );
    memory.ptr(room + 0x10, users);
    memory.i32(users + 8, 0);
    memory.i32(users + 0x10, 20_000_000);
    memory.i32(
        room + offsets
            .osu_game_online_multiplayer_multiplayer_room
            .channel_id_k_backing_field,
        0,
    );
    memory.ptr(base + offsets.osu_game_osu_game.channel_manager, manager);
    memory.ptr(
        manager + offsets.osu_game_online_chat_channel_manager.joined_channels,
        joined,
    );
    memory.ptr(joined + 0x18, collection);
    memory.ptr(collection + 8, array);
    memory.i32(collection + 0x10, 0);
    assert!(matches!(
        spectator_user_state(&BTreeMap::new(), 42),
        Err(MemoryError::Corrupt {
            context: "spectator room user",
            address: 42,
            ..
        })
    ));
    let mut decoder = reader(Arc::new(memory));
    decoder.set_resolved_state(base, screen);
    let ReadStatus::Ready(value) = decoder.spectating_data(false).unwrap() else {
        panic!("not ready")
    };
    assert!(value.chat.is_empty() && value.spectating_clients.is_empty());
    assert_eq!(js_round_number(-1.5), -1.0);
    assert_eq!(js_fixed2(0.425), 0.42);
    assert_eq!(js_fixed2(1.125), 1.13);
    assert!(!hit_error_out_of_range(f64::NAN));
    assert!(hit_error_out_of_range(10_001.0));
}
