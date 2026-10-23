use super::*;
use crate::model::memory::{ClientRenderState, PlaybackClock};

#[test]
fn source_distinguishes_own_gameplay_watching_and_non_gameplay() {
    let base = StateBundle::new(ClientType::Lazer, 1, Arc::new(NeverCalculate)).snapshot();
    let mut state = (*base.regular).clone();
    let mods = state.global.menu_mods.clone();
    assert_eq!(play_source(&state, 5, ClientType::Lazer, &mods), None);
    assert_eq!(
        play_source(&state, 2, ClientType::Lazer, &mods),
        Some(PlaySource::Player)
    );
    state.global.is_watching_replay = true;
    assert_eq!(
        play_source(&state, 2, ClientType::Lazer, &mods),
        Some(PlaySource::Replay)
    );
    assert_eq!(
        play_source(&state, 2, ClientType::Stable, &mods),
        Some(PlaySource::Watching)
    );
    state.global.is_watching_replay = false;
    state.global.render = Some(ClientRenderState {
        spectating: true,
        clock: None,
    });
    assert_eq!(
        play_source(&state, 2, ClientType::Lazer, &mods),
        Some(PlaySource::Spectator)
    );
    state.global.render = None;
    let mut autoplay = mods;
    autoplay.array.push(crate::model::states::Mod {
        acronym: "AT".into(),
        settings: None,
    });
    assert_eq!(
        play_source(&state, 2, ClientType::Lazer, &autoplay),
        Some(PlaySource::Autoplay)
    );
    assert_eq!(play_source(&state, 7, ClientType::Lazer, &autoplay), None);
}

#[test]
fn lazer_preserves_fractional_scroll_and_exposes_base_scroll_time() {
    let base = StateBundle::new(ClientType::Lazer, 1, Arc::new(NeverCalculate)).snapshot();
    let mut state = (*base.regular).clone();
    state.settings.values.get_mut("mania").unwrap()["scrollSpeed"] = serde_json::json!(8.7);
    state.settings.values.get_mut("mania").unwrap()["timingBasedNoteColouring"] = Value::Bool(true);
    let settings = build_settings(&state, ClientType::Lazer);
    assert_eq!(settings.mania.scroll_speed, 8, "legacy integer retained");
    assert_eq!(settings.mania.scroll_speed_value, Some(8.7));
    assert_eq!(settings.mania.scroll_time, Some(11485.0 / 8.7));
    assert_eq!(settings.mania.timing_based_note_colouring, Some(true));
    assert_eq!(
        settings.mania.scroll_direction.name.as_deref(),
        Some("down")
    );
    let stable = build_settings(&state, ClientType::Stable);
    assert_eq!(stable.mania.scroll_speed_value, None);
    assert_eq!(
        stable.mania.scroll_time, None,
        "stable is not lazer's scroll formula"
    );
}

#[test]
fn song_select_encode_includes_clock_mod_settings_ruleset_and_visuals() {
    let base = StateBundle::new(ClientType::Lazer, 1, Arc::new(NeverCalculate)).snapshot();
    let mut regular = (*base.regular).clone();
    regular.global.status = 5;
    regular.menu.gamemode = Some(3);
    regular.beatmap.mode = Some(0);
    regular.global.render = Some(ClientRenderState {
        spectating: false,
        clock: Some(PlaybackClock {
            rate: 1.2345,
            running: true,
        }),
    });
    regular
        .global
        .menu_mods
        .array
        .push(crate::model::states::Mod {
            acronym: "DT".into(),
            settings: Some([("speed_change".into(), serde_json::json!(1.23))].into()),
        });
    regular.settings.values.get_mut("mania").unwrap()["scrollSpeed"] = serde_json::json!(8.7);
    regular.settings.values.get_mut("visual").unwrap()["snakingInSliders"] = Value::Bool(false);
    regular.settings.values.get_mut("visual").unwrap()["hitLighting"] = Value::Bool(true);
    regular.settings.values.get_mut("visual").unwrap()["comboColourNormalisation"] =
        serde_json::json!(0.375);
    let mut precise = (*base.precise).clone();
    precise.status = 5;
    precise.play_time = 12345.0;
    let state = Arc::new(StateBundleSnapshot {
        regular: Arc::new(regular),
        precise: Arc::new(precise),
        source: base.source,
        generation: base.generation,
        reset_epoch: base.reset_epoch,
    });
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        1,
        Some(10),
        true,
        ApiRuntimeSettings::default(),
        [manager_entry(10, state, false, None)],
    )));
    // Debug encode asserts direct writer == DTO, covering all additive fields.
    let value: Value = serde_json::from_slice(&encode(&snapshot).unwrap()).unwrap();
    assert_eq!(value["play"]["source"], Value::Null);
    assert_eq!(value["beatmap"]["time"]["live"], 12345);
    assert_eq!(value["beatmap"]["time"]["rate"], 1.2345);
    assert_eq!(value["beatmap"]["time"]["running"], true);
    assert_eq!(value["settings"]["mode"]["number"], 3);
    assert_eq!(
        value["play"]["mods"]["array"][0]["settings"]["speed_change"],
        1.23
    );
    assert_eq!(value["settings"]["visual"]["snakingInSliders"], false);
    assert_eq!(value["settings"]["visual"]["hitLighting"], true);
    assert_eq!(
        value["settings"]["visual"]["comboColourNormalisation"],
        0.375
    );
    assert_eq!(
        value["settings"]["visual"]["snakingOutSliders"],
        Value::Null
    );
}

#[test]
fn unknown_running_is_not_invented_for_stable_and_lazer_errors() {
    for source in [ClientType::Stable, ClientType::Lazer] {
        let value: Value =
            serde_json::from_slice(&encode(&default_snapshot(source)).unwrap()).unwrap();
        assert_eq!(value["beatmap"]["time"]["running"], Value::Null);
        if source == ClientType::Lazer {
            assert_eq!(value["beatmap"]["time"]["rate"], Value::Null);
        } else {
            assert_eq!(value["beatmap"]["time"]["rate"], 1.0);
        }
    }
}

#[test]
fn stable_combo_colour_normalisation_remains_unknown() {
    let base = StateBundle::new(ClientType::Stable, 1, Arc::new(NeverCalculate)).snapshot();
    let mut state = (*base.regular).clone();
    state.settings.values.get_mut("visual").unwrap()["hitLighting"] = Value::Bool(true);
    let visual = build_settings(&state, ClientType::Stable).visual.unwrap();
    assert_eq!(visual.combo_colour_normalisation, None);
    assert_eq!(
        serde_json::to_value(visual).unwrap()["comboColourNormalisation"],
        Value::Null
    );
}
