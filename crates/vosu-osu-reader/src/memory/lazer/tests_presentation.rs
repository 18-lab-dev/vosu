use super::test_support::*;
use super::*;

#[test]
fn native_track_clock_reads_running_and_effective_aggregate_rate() {
    let o = LazerOffsets::embedded().unwrap();
    let mut memory = SparseMemory::default();
    memory.valid_base(0x1000, &o);
    memory.ptr(0x1000 + o.osu_game_osu_game_base.beatmap_clock, 0x3000);
    memory.ptr(
        0x3000 + o.osu_game_beatmaps_framed_beatmap_clock.decoupled_track,
        0x4000,
    );
    memory.put(
        0x4000 + o.osu_framework_timing_decoupling_framed_clock.running,
        [1],
    );
    memory.ptr(
        0x4000
            + o.osu_framework_timing_decoupling_framed_clock
                .source_k_backing_field,
        0x5000,
    );
    memory.ptr(
        0x5000 + o.audio_component.as_ref().unwrap().adjustments,
        0x6000,
    );
    let adjustments = o.audio_adjustments.as_ref().unwrap();
    for (field, aggregate, bindable, value) in [
        (adjustments.frequency, 0x7000, 0x9000, 1.25),
        (adjustments.tempo, 0x8000, 0xa000, 0.8),
    ] {
        memory.ptr(0x6000 + field, aggregate);
        memory.ptr(
            aggregate + o.aggregate_double.as_ref().unwrap().result,
            bindable,
        );
        memory.f64(bindable + o.bindable_double.as_ref().unwrap().value, value);
    }
    let mut decoder = reader(Arc::new(memory));
    decoder.set_resolved_state(0x1000, 0);
    let clock = decoder.playback_clock(0).unwrap();
    assert_eq!(clock.rate, 1.0);
    assert!(clock.running);
}

#[test]
fn local_play_clears_old_replay_and_spectator_flags() {
    let mut decoder = reader(Arc::new(SparseMemory::default()));
    for (replay, spectator, expected) in [
        (true, false, (true, false)),
        (false, false, (false, false)),
        (true, true, (false, true)),
        (false, false, (false, false)),
    ] {
        decoder.set_playback_source(replay, spectator);
        assert_eq!((decoder.watching_replay, decoder.spectating), expected);
    }
}

#[test]
fn imported_skin_family_uses_method_tables_not_display_name() {
    let o = LazerOffsets::embedded().unwrap();
    let manager = 0x1000;
    let current = 0x2000;
    let skin = &o.osu_game_skinning_skin_manager;
    for (ty, parent, expected) in [
        (0x9000, 0, "argon"),
        (0xa000, 0x9000, "argon-pro"),
        (0xb000, 0, "legacy"),
        (0xc000, 0, "triangles"),
        (0xd000, 0, "retro"),
    ] {
        let mut memory = SparseMemory::default();
        memory.ptr(current, ty);
        memory.ptr(ty + 0x10, parent);
        for (offset, object, ty) in [
            (skin.legacy_skin, 0x3000, 0xe000),
            (skin.argon_skin, 0x4000, 0x9000),
            (skin.triangles_skin, 0x5000, 0xc000),
            (skin.retro_skin, 0x6000, 0xd000),
        ] {
            memory.ptr(manager + offset, object);
            memory.ptr(object, ty);
        }
        memory.ptr(0xe010, 0xb000); // DefaultLegacySkin : LegacySkin.
        let decoder = reader(Arc::new(memory));
        assert_eq!(decoder.skin_base(manager, current).unwrap(), Some(expected));
    }
}

#[test]
fn pinned_local_config_keys_are_measured_enum_values() {
    let o = LazerOffsets::embedded().unwrap();
    let k = o.local_config_keys.as_ref().unwrap();
    assert_eq!(
        (
            k.release_stream,
            k.score_display_mode,
            k.replay_settings_overlay
        ),
        (35, 60, 90)
    );
    assert_eq!(
        (k.beatmap_skins, k.beatmap_colours, k.hit_lighting),
        (56, 57, 75)
    );
    assert_eq!(k.combo_colour_normalisation_amount, 87);
    assert_eq!(k.fields().last(), Some(&(87, 4)));
    assert_ne!(
        k.beatmap_skins, 54,
        "54 is ScreenshotFormat in this pinned client"
    );
}

#[test]
fn unset_random_seed_is_null_not_uninitialized_integer_storage() {
    let o = LazerOffsets::embedded().unwrap();
    let offset = o.bindable_nullable_int.as_ref().unwrap().value;
    let mut memory = SparseMemory::default();
    memory.put(0x1000 + offset, [0]);
    // Unset nullable must not read its payload at all (a sparse hole here).
    memory.put(0x2000 + offset, [1]);
    memory.i32(0x2000 + offset + 4, 12345);
    let decoder = reader(Arc::new(memory));
    assert_eq!(decoder.nullable_seed(0x1000).unwrap(), Value::Null);
    assert_eq!(decoder.nullable_seed(0x2000).unwrap(), 12345);
}

#[test]
fn ruleset_visual_config_reads_mania_colour_and_selected_ruleset_only() {
    let o = LazerOffsets::embedded().unwrap();
    let mut memory = SparseMemory::default();
    memory.valid_base(0x1000, &o);
    memory.ptr(
        0x1000 + o.osu_game_osu_game_base.ruleset_config_cache,
        0x3000,
    );
    memory.ptr(
        0x3000 + o.osu_game_rulesets_ruleset_config_cache.config_cache,
        0x4000,
    );
    memory.ptr(0x4010, 0x5000);
    memory.i32(0x4038, 2);
    for (i, name, fields) in [
        (0, "osu", vec![(0, 0), (1, 1), (2, 1)]),
        (1, "mania", vec![(2, 1)]),
    ] {
        let config = 0x6000 + i * 0x1000;
        let store = 0x8000 + i * 0x1000;
        let entries = 0xa000 + i * 0x1000;
        let entry = 0x5010 + i * 0x18;
        memory.string_ptr(entry, 0x10000 + i * 0x100, name);
        memory.ptr(entry + 8, config);
        memory.ptr(config + 0x20, store);
        memory.ptr(store + 0x10, entries);
        memory.i32(store + 0x38, fields.len() as i32);
        for (j, (key, value)) in fields.into_iter().enumerate() {
            let bindable = 0x20000 + i * 0x1000 + j * 0x100;
            memory.ptr(entries + 0x10 + j * 0x18, bindable);
            memory.i32(entries + 0x20 + j * 0x18, key);
            memory.put(bindable + 0x40, [value]);
        }
    }
    let mut decoder = reader(Arc::new(memory));
    decoder.set_resolved_state(0x1000, 0);
    decoder.selected_gamemode = 0;
    let mut settings = Settings::new();
    decoder.ruleset_visual_settings(&mut settings).unwrap();
    assert_eq!(settings["visual.snakingInSliders"], false);
    assert_eq!(settings["visual.snakingOutSliders"], true);
    assert_eq!(settings["mania.timingBasedNoteColouring"], true);
    decoder.selected_gamemode = 2;
    decoder.ruleset_visual_settings(&mut settings).unwrap();
    assert_eq!(settings["visual.hitAnimations"], Value::Null);
    assert_eq!(settings["visual.snakingInSliders"], Value::Null);
    assert_eq!(settings["mania.timingBasedNoteColouring"], true);
}
