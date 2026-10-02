use super::*;

fn parsed_hit_window_map(
    original_mode: Ruleset,
    effective_mode: Ruleset,
    source: ClientType,
    mods: Vec<AppliedMod>,
    clock_rate: f64,
) -> Arc<ParsedBeatmap> {
    let content = format!(
        "osu file format v14\n\
         [General]\n\
         Mode:{}\n\
         [Difficulty]\n\
         HPDrainRate:5\n\
         CircleSize:5\n\
         OverallDifficulty:5\n\
         ApproachRate:5\n\
         [TimingPoints]\n\
         0,500,4,2,1,80,1,0\n\
         [HitObjects]\n\
         64,192,1000,1,0,0:0:0:0:\n",
        original_mode.id()
    );
    let mods = ModSet::new("timing-test", mods, clock_rate).unwrap();
    BeatmapCache::new(LiveBeatmapEngine)
        .load(
            BeatmapInput::Content {
                content: &content,
                identity_path: None,
            },
            effective_mode,
            &mods,
            match source {
                ClientType::Stable => ScoringMode::Stable,
                ClientType::Lazer => ScoringMode::Lazer,
            },
        )
        .unwrap()
}

fn applied_mod(acronym: &str) -> AppliedMod {
    AppliedMod {
        acronym: acronym.to_owned(),
        settings_json: None,
    }
}

fn applied_mod_with_settings(acronym: &str, settings_json: &str) -> AppliedMod {
    AppliedMod {
        acronym: acronym.to_owned(),
        settings_json: Some(settings_json.to_owned()),
    }
}

#[test]
fn hit_windows_match_difficulty_modifier_matrix_at_od_five() {
    let cases = [
        (
            Ruleset::Osu,
            "EZ",
            serde_json::json!({"miss": 400.0, "meh": 174.5, "ok": 119.5, "great": 64.5}),
        ),
        (
            Ruleset::Osu,
            "HR",
            serde_json::json!({"miss": 400.0, "meh": 129.5, "ok": 83.5, "great": 37.5}),
        ),
        (
            Ruleset::Osu,
            "DA",
            serde_json::json!({"miss": 400.0, "meh": 119.5, "ok": 75.5, "great": 31.5}),
        ),
        (
            Ruleset::Taiko,
            "EZ",
            serde_json::json!({"miss": 114.5, "ok": 99.5, "great": 41.5}),
        ),
        (
            Ruleset::Taiko,
            "HR",
            serde_json::json!({"miss": 84.5, "ok": 67.5, "great": 28.5}),
        ),
        (
            Ruleset::Taiko,
            "DA",
            serde_json::json!({"miss": 79.5, "ok": 61.5, "great": 25.5}),
        ),
        (
            Ruleset::Mania,
            "EZ",
            serde_json::json!({
                "miss": 242.5,
                "meh": 190.5,
                "ok": 156.5,
                "good": 114.5,
                "great": 68.5,
                "perfect": 27.5,
            }),
        ),
        (
            Ruleset::Mania,
            "HR",
            serde_json::json!({
                "miss": 123.5,
                "meh": 97.5,
                "ok": 80.5,
                "good": 58.5,
                "great": 35.5,
                "perfect": 13.5,
            }),
        ),
        (
            Ruleset::Mania,
            "DA",
            serde_json::json!({
                "miss": 164.5,
                "meh": 127.5,
                "ok": 103.5,
                "good": 73.5,
                "great": 40.5,
                "perfect": 16.5,
            }),
        ),
    ];

    for (mode, acronym, expected) in cases {
        let game_mod = if acronym == "DA" {
            applied_mod_with_settings(acronym, r#"{"overall_difficulty":8.0,"circle_size":7.0}"#)
        } else {
            applied_mod(acronym)
        };

        for source in [ClientType::Lazer, ClientType::Stable] {
            if source == ClientType::Stable && (acronym == "DA" || mode == Ruleset::Mania) {
                continue;
            }
            let parsed = parsed_hit_window_map(mode, mode, source, vec![game_mod.clone()], 1.0);

            assert_eq!(
                serde_json::to_value(hit_window(Some(&parsed))).unwrap(),
                expected,
                "{source:?} {mode:?} {acronym}",
            );
        }
    }
}

#[test]
fn hit_windows_match_stable_classic_mania_difficulty_modifiers() {
    for (acronym, expected) in [
        (
            "EZ",
            serde_json::json!({
                "miss": 242.5,
                "meh": 190.5,
                "ok": 156.5,
                "good": 114.5,
                "great": 68.5,
                "perfect": 22.5,
            }),
        ),
        (
            "HR",
            serde_json::json!({
                "miss": 123.5,
                "meh": 97.5,
                "ok": 80.5,
                "good": 58.5,
                "great": 35.5,
                "perfect": 11.5,
            }),
        ),
    ] {
        let parsed = parsed_hit_window_map(
            Ruleset::Mania,
            Ruleset::Mania,
            ClientType::Stable,
            vec![applied_mod("CL"), applied_mod(acronym)],
            1.0,
        );

        assert_eq!(
            serde_json::to_value(hit_window(Some(&parsed))).unwrap(),
            expected,
            "Stable Mania {acronym}",
        );
    }
}

#[test]
fn hit_windows_apply_gameplay_rate_before_floor_plus_half_quantization_for_mania() {
    for (label, game_mod, clock_rate, mut lazer_expected, stable_perfect) in [
        (
            "DT 1.5x",
            applied_mod("DT"),
            1.5,
            serde_json::json!({
                "miss": 259.5,
                "meh": 204.5,
                "ok": 168.5,
                "good": 123.5,
                "great": 73.5,
                "perfect": 29.5,
            }),
            24.5,
        ),
        (
            "HT 0.75x",
            applied_mod("HT"),
            0.75,
            serde_json::json!({
                "miss": 129.5,
                "meh": 102.5,
                "ok": 84.5,
                "good": 61.5,
                "great": 36.5,
                "perfect": 14.5,
            }),
            12.5,
        ),
        (
            "DT 1.3x",
            applied_mod_with_settings("DT", r#"{"speed_change":1.3}"#),
            1.3,
            serde_json::json!({
                "miss": 224.5,
                "meh": 176.5,
                "ok": 145.5,
                "good": 106.5,
                "great": 63.5,
                "perfect": 25.5,
            }),
            20.5,
        ),
    ] {
        let lazer = parsed_hit_window_map(
            Ruleset::Mania,
            Ruleset::Mania,
            ClientType::Lazer,
            vec![game_mod.clone()],
            clock_rate,
        );
        for value in lazer_expected.as_object_mut().unwrap().values_mut() {
            *value = serde_json::json!(value.as_f64().unwrap() / clock_rate);
        }
        assert_eq!(
            serde_json::to_value(hit_window(Some(&lazer))).unwrap(),
            lazer_expected,
            "Lazer Mania {label}",
        );

        let stable = parsed_hit_window_map(
            Ruleset::Mania,
            Ruleset::Mania,
            ClientType::Stable,
            vec![applied_mod("CL"), game_mod],
            clock_rate,
        );
        let mut stable_expected = lazer_expected;
        stable_expected["perfect"] = serde_json::json!(stable_perfect / clock_rate);
        assert_eq!(
            serde_json::to_value(hit_window(Some(&stable))).unwrap(),
            stable_expected,
            "Stable Mania {label}",
        );
    }
}

#[test]
fn hit_windows_apply_lazer_extended_da_before_clock_rate_scaling() {
    let da = || AppliedMod {
        acronym: "DA".to_owned(),
        settings_json: Some(r#"{"overall_difficulty":11.0,"extended_limits":true}"#.to_owned()),
    };

    let parsed = parsed_hit_window_map(
        Ruleset::Osu,
        Ruleset::Osu,
        ClientType::Lazer,
        vec![da()],
        1.0,
    );
    assert_eq!(
        serde_json::to_value(hit_window(Some(&parsed))).unwrap(),
        serde_json::json!({"miss": 400.0, "meh": 89.5, "ok": 51.5, "great": 13.5})
    );

    let parsed = parsed_hit_window_map(
        Ruleset::Osu,
        Ruleset::Osu,
        ClientType::Lazer,
        vec![da()],
        1.5,
    );
    assert_eq!(
        serde_json::to_value(hit_window(Some(&parsed))).unwrap(),
        serde_json::json!({"miss": 400.0 / 1.5, "meh": 89.5 / 1.5, "ok": 51.5 / 1.5, "great": 9.0})
    );
}

#[test]
fn hit_windows_apply_lazer_extended_da_to_classic_mania() {
    let difficulty_adjust = AppliedMod {
        acronym: "DA".to_owned(),
        settings_json: Some(r#"{"overall_difficulty":11.0,"extended_limits":true}"#.to_owned()),
    };
    let parsed = parsed_hit_window_map(
        Ruleset::Mania,
        Ruleset::Mania,
        ClientType::Lazer,
        vec![applied_mod("CL"), difficulty_adjust],
        1.0,
    );

    assert_eq!(
        serde_json::to_value(hit_window(Some(&parsed))).unwrap(),
        serde_json::json!({
            "miss": 158.5,
            "meh": 121.5,
            "ok": 97.5,
            "good": 67.5,
            "great": 34.5,
            "perfect": 16.5,
        })
    );
}

#[test]
fn hit_windows_match_lazer_mania_da_od_twelve_point_five() {
    let difficulty_adjust = AppliedMod {
        acronym: "DA".to_owned(),
        settings_json: Some(r#"{"overall_difficulty":12.5,"extended_limits":true}"#.to_owned()),
    };
    let parsed = parsed_hit_window_map(
        Ruleset::Mania,
        Ruleset::Mania,
        ClientType::Lazer,
        vec![difficulty_adjust],
        1.0,
    );

    assert_eq!(
        serde_json::to_value(hit_window(Some(&parsed))).unwrap(),
        serde_json::json!({
            "miss": 150.5,
            "meh": 113.5,
            "ok": 89.5,
            "good": 59.5,
            "great": 26.5,
            "perfect": 11.5,
        })
    );
}

#[test]
fn hit_windows_use_classic_map_values_for_stable_converted_mania() {
    let parsed = parsed_hit_window_map(
        Ruleset::Osu,
        Ruleset::Mania,
        ClientType::Stable,
        vec![applied_mod("CL")],
        1.0,
    );
    assert_eq!(
        serde_json::to_value(hit_window(Some(&parsed))).unwrap()["great"],
        serde_json::json!(34.5)
    );
}

#[test]
fn hit_windows_use_score_v2_map_values_for_stable_mania() {
    let parsed = parsed_hit_window_map(
        Ruleset::Mania,
        Ruleset::Mania,
        ClientType::Stable,
        vec![applied_mod("CL"), applied_mod("SV2")],
        1.0,
    );
    let windows = serde_json::to_value(hit_window(Some(&parsed))).unwrap();
    assert_eq!(windows["great"], serde_json::json!(49.5));
    assert_eq!(windows["perfect"], serde_json::json!(19.5));
}

#[test]
fn hit_windows_omit_catch_spatial_boundary_for_all_clients_and_speeds() {
    for source in [ClientType::Stable, ClientType::Lazer] {
        for (mods, clock_rate) in [
            (Vec::new(), 1.0),
            (vec![applied_mod("DT")], 1.5),
            (vec![applied_mod("HT")], 0.75),
        ] {
            let parsed =
                parsed_hit_window_map(Ruleset::Catch, Ruleset::Catch, source, mods, clock_rate);
            let state = StateBundle::new(source, 1, Arc::new(NeverCalculate)).snapshot();
            let stats = serde_json::to_value(build_stats(&state.regular, Some(&parsed))).unwrap();
            assert_eq!(
                stats["hitWindow"],
                serde_json::json!({"miss": 0.0, "meh": 0.0, "ok": 0.0, "good": 0.0, "great": 0.0, "perfect": 0.0}),
                "{source:?} at {clock_rate}x",
            );
            assert!(
                stats["stars"].get("hitWindow").is_none(),
                "Catch spatial units leaked into stars.hitWindow for {source:?} at {clock_rate}x",
            );
            assert_eq!(parsed.engine.difficulty.hit_window, None);
        }
    }
}

#[test]
fn hit_windows_are_empty_for_missing_maps_and_default_responses() {
    assert_eq!(
        serde_json::to_value(hit_window(None)).unwrap(),
        serde_json::json!({})
    );

    for source in [ClientType::Stable, ClientType::Lazer] {
        let response: serde_json::Value =
            serde_json::from_slice(&encode(&default_snapshot(source)).unwrap()).unwrap();
        assert_eq!(
            response
                .get("beatmap")
                .and_then(|beatmap| beatmap.get("stats"))
                .and_then(|stats| stats.get("hitWindow")),
            Some(&serde_json::json!({})),
            "{source:?}"
        );
    }
}
