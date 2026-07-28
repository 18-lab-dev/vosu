use std::sync::Arc;

use super::*;
use crate::api::{ApiRuntimeSettings, ManagerInstanceSnapshot, ManagerSnapshot};
use crate::calculation::beatmap::{
    AppliedMod, BeatmapCache, BeatmapInput, ModSet, ParsedBeatmap, Ruleset, ScoringMode,
};
use crate::calculation::state_backend::LiveBeatmapEngine;
use crate::instance::InstanceSettings;
use crate::state::{
    RegularState, StateBundle, StateBundleSnapshot, StateCalculationBackend,
    StateCalculationFuture, StateCalculationRequest, TourneyUserState,
};

fn parsed_timing_map(
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

struct NeverCalculate;
impl StateCalculationBackend for NeverCalculate {
    fn calculate<'a>(&'a self, _: StateCalculationRequest) -> StateCalculationFuture<'a> {
        Box::pin(std::future::pending())
    }
}

fn json<T: Serialize>(value: &T) -> String {
    String::from_utf8(encode_dto(value).unwrap().to_vec()).unwrap()
}

fn default_snapshot(source: ClientType) -> CombinedSnapshot {
    let state = StateBundle::new(source, 1, Arc::new(NeverCalculate)).snapshot();
    let instance = ManagerInstanceSnapshot::new(
        10,
        InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: None,
        },
        false,
        state,
    );
    CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        1,
        Some(10),
        true,
        ApiRuntimeSettings::default(),
        [instance],
    )))
}

fn with_regular(
    base: &Arc<StateBundleSnapshot>,
    regular: RegularState,
) -> Arc<StateBundleSnapshot> {
    Arc::new(StateBundleSnapshot {
        source: base.source,
        generation: base.generation,
        reset_epoch: base.reset_epoch,
        regular: Arc::new(regular),
        precise: Arc::clone(&base.precise),
    })
}

fn manager_entry(
    pid: u32,
    state: Arc<StateBundleSnapshot>,
    manager: bool,
    spectator_ipc: Option<f64>,
) -> ManagerInstanceSnapshot {
    ManagerInstanceSnapshot::new(
        pid,
        InstanceSettings {
            tournament_spectator: spectator_ipc.is_some(),
            tournament_ipc_id: spectator_ipc,
            devserver_endpoint: None,
        },
        manager,
        state,
    )
}

#[test]
fn subscription_document_matches_decoded_encoding_for_ready_and_not_ready_snapshots() {
    for source in [ClientType::Stable, ClientType::Lazer] {
        let snapshot = default_snapshot(source);
        let decoded: Value = serde_json::from_slice(&encode(&snapshot).unwrap()).unwrap();
        assert_eq!(document(&snapshot).unwrap(), decoded);
    }

    let base = StateBundle::new(ClientType::Stable, 1, Arc::new(NeverCalculate)).snapshot();
    let mut precise = (*base.precise).clone();
    precise.play_time = f64::NAN;
    let state = Arc::new(StateBundleSnapshot {
        source: base.source,
        generation: base.generation,
        reset_epoch: base.reset_epoch,
        regular: Arc::clone(&base.regular),
        precise: Arc::new(precise),
    });
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        2,
        Some(10),
        true,
        ApiRuntimeSettings::default(),
        [manager_entry(10, state, false, None)],
    )));
    let decoded: Value = serde_json::from_slice(&encode(&snapshot).unwrap()).unwrap();
    assert_eq!(decoded["beatmap"]["time"]["live"], Value::Null);
    assert_eq!(document(&snapshot).unwrap(), decoded);

    let not_ready = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::not_ready(
        3,
        ApiRuntimeSettings::default(),
    )));
    assert!(matches!(encode(&not_ready), Err(ApiError::NotReady)));
    assert!(matches!(document(&not_ready), Err(ApiError::NotReady)));
}

#[test]
fn source_enum_names_cover_every_mode() {
    assert_eq!(
        (0..=3).map(ruleset_name).collect::<Vec<_>>(),
        ["osu", "taiko", "fruits", "mania"]
    );
    assert_eq!(mania_scroll_name(0), "up");
    assert_eq!(mania_scroll_name(1), "down");
}

#[test]
fn lazer_omits_parsed_beatmap_source_metadata() {
    assert_eq!(
        beatmap_source(ClientType::Lazer, Some("THE iDOLM@STER")),
        ""
    );
    assert_eq!(
        beatmap_source(ClientType::Stable, Some("THE iDOLM@STER")),
        "THE iDOLM@STER"
    );
}

#[test]
fn mode_and_lazer_team_fixture_matches_source_contract() {
    let modes = (0..=3)
        .map(|mode| number_name(Some(mode), ruleset_name(mode)))
        .collect::<Vec<_>>();
    assert_eq!(
        serde_json::to_string(&modes).unwrap(),
        r#"[{"number":0,"name":"osu"},{"number":1,"name":"taiko"},{"number":2,"name":"fruits"},{"number":3,"name":"mania"}]"#
    );
    assert_eq!(
        lazer_wire_team(crate::model::states::MultiplayerTeamType::Red),
        TourneyTeam::Left
    );
    assert_eq!(
        lazer_wire_team(crate::model::states::MultiplayerTeamType::Blue),
        TourneyTeam::Right
    );
    assert_eq!(
        lazer_wire_team(crate::model::states::MultiplayerTeamType::None),
        TourneyTeam::Right
    );
}

#[test]
fn performance_uses_disabled_defaults_and_retained_state_projection() {
    let base = StateBundle::new(ClientType::Stable, 1, Arc::new(NeverCalculate)).snapshot();
    let mut regular = (*base.regular).clone();
    let disabled = build_performance(&regular).unwrap();
    assert_eq!(disabled.accuracy.acc_90, 0.0);
    assert_eq!(disabled.accuracy.acc_100, 0.0);

    regular.beatmap.pp_acc.insert(90, 321.0);
    regular.beatmap.pp_acc.insert(100, 654.0);
    let retained = build_performance(&regular).unwrap();
    assert_eq!(retained.accuracy.acc_90, 321.0);
    assert_eq!(retained.accuracy.acc_100, 654.0);
    assert!(regular.beatmap.parsed.is_none());
}

#[test]
fn beatmap_live_time_comes_from_atomic_precise_snapshot() {
    let base = StateBundle::new(ClientType::Lazer, 1, Arc::new(NeverCalculate)).snapshot();
    let mut regular = (*base.regular).clone();
    regular.global.play_time = 10.0;
    let mut precise = (*base.precise).clone();
    precise.play_time = 12.5;
    let state = Arc::new(StateBundleSnapshot {
        source: base.source,
        generation: base.generation,
        reset_epoch: base.reset_epoch,
        regular: Arc::new(regular),
        precise: Arc::new(precise),
    });
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        2,
        Some(10),
        true,
        ApiRuntimeSettings::default(),
        [manager_entry(10, state, false, None)],
    )));
    assert_eq!(build(&snapshot).unwrap().beatmap.time.live, 12.5);
}

#[test]
fn absent_profile_omits_undefined_but_present_matchmaking_is_null() {
    assert_eq!(
        json(&build_profile(None)),
        r#"{"userStatus":{"name":""},"banchoStatus":{"name":""},"mode":{"name":""},"countryCode":{"name":""}}"#
    );
    let mut user = crate::model::memory::User {
        name: String::new(),
        accuracy: 0.0,
        ranked_score: 0.0,
        id: 0,
        level: 0.0,
        play_count: 0,
        play_mode: 0,
        rank: 0,
        country_code: 0,
        performance_points: 0.0,
        raw_bancho_status: 0,
        background_colour: 0,
        raw_login_status: 0,
        matchmaking: None,
    };
    let encoded = json(&build_profile(Some(&user)));
    assert!(encoded.ends_with("\"matchmaking\":null}"));
    user.matchmaking = Some(crate::model::memory::MatchmakingStats {
        rating: 1.5,
        rank: None,
        plays: 2,
        wins: 1,
        is_provisional: true,
    });
    assert!(json(&build_profile(Some(&user))).contains("\"rank\":null"));
}

#[test]
fn default_tournament_bytes_match_source_order() {
    assert_eq!(
        json(&empty_tourney()),
        r#"{"scoreVisible":false,"starsVisible":false,"ipcState":0,"bestOF":0,"team":{"left":"","right":""},"points":{"left":0,"right":0},"chat":[],"totalScore":{"left":0,"right":0},"clients":[]}"#
    );
}

#[test]
fn fractional_and_nonfinite_ipc_ids_follow_javascript_json() {
    let keys = KeyOverlay {
        k1: KeyOverlayButton {
            is_pressed: false,
            count: 0,
        },
        k2: KeyOverlayButton {
            is_pressed: false,
            count: 0,
        },
        m1: KeyOverlayButton {
            is_pressed: false,
            count: 0,
        },
        m2: KeyOverlayButton {
            is_pressed: false,
            count: 0,
        },
    };
    let keypresses = Keypresses {
        mode: NumberName {
            number: Some(0),
            name: "osu".to_owned(),
        },
        keys: Vec::new(),
    };
    let dto = PreciseTourney {
        ipc_id: 7.25,
        keys: keys.clone(),
        keypresses: keypresses.clone(),
        hit_errors: Default::default(),
    };
    assert!(json(&dto).starts_with(r#"{"ipcId":7.25,"keys":"#));
    let dto = PreciseTourney {
        ipc_id: f64::INFINITY,
        keys,
        keypresses,
        hit_errors: Default::default(),
    };
    assert!(json(&dto).starts_with(r#"{"ipcId":null,"keys":"#));
}

#[test]
fn mp3_length_preserves_fraction_and_serializes_nonfinite_as_null() {
    let time = |mp3_length| BeatmapTime {
        live: 0.0,
        first_object: 0,
        last_object: 0.0,
        mp3_length,
    };
    assert_eq!(
        json(&time(None)),
        r#"{"live":0,"firstObject":0,"lastObject":0}"#
    );
    assert_eq!(
        json(&time(Some(12.5))),
        r#"{"live":0,"firstObject":0,"lastObject":0,"mp3Length":12.5}"#
    );
    assert_eq!(
        json(&time(Some(f64::NEG_INFINITY))),
        r#"{"live":0,"firstObject":0,"lastObject":0,"mp3Length":null}"#
    );
    assert_eq!(
        json(&BeatmapTime {
            live: 0.0,
            first_object: 0,
            last_object: 12.5,
            mp3_length: None,
        }),
        r#"{"live":0,"firstObject":0,"lastObject":12.5}"#
    );
    assert_eq!(
        json(&BeatmapTime {
            live: 0.0,
            first_object: 0,
            last_object: 1e21,
            mp3_length: None,
        }),
        r#"{"live":0,"firstObject":0,"lastObject":1e+21}"#
    );
    assert_eq!(
        json(&BeatmapTime {
            live: 0.0,
            first_object: 0,
            last_object: f64::NEG_INFINITY,
            mp3_length: None,
        }),
        r#"{"live":0,"firstObject":0,"lastObject":null}"#
    );
    assert_eq!(
        json(&Session {
            play_time: 12.5,
            play_count: 0
        }),
        r#"{"playTime":12.5,"playCount":0}"#
    );
    assert_eq!(
        json(&Session {
            play_time: f64::INFINITY,
            play_count: 0,
        }),
        r#"{"playTime":null,"playCount":0}"#
    );
    assert_eq!(
        json(&BeatmapTime {
            live: f64::NAN,
            first_object: 0,
            last_object: 0.0,
            mp3_length: None,
        }),
        r#"{"live":null,"firstObject":0,"lastObject":0}"#
    );
    assert_eq!(
        json(&Session {
            play_time: i64::MAX as f64,
            play_count: 0,
        }),
        r#"{"playTime":9223372036854776000,"playCount":0}"#
    );
}

#[test]
fn default_builder_has_exact_root_order_and_source_default_tourney() {
    let snapshot = default_snapshot(ClientType::Stable);
    let bytes = encode(&snapshot).unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    let ordered = [
        "\"game\":",
        "\"client\":",
        "\"server\":",
        "\"state\":",
        "\"session\":",
        "\"settings\":",
        "\"profile\":",
        "\"beatmap\":",
        "\"play\":",
        "\"leaderboard\":",
        "\"performance\":",
        "\"resultsScreen\":",
        "\"folders\":",
        "\"files\":",
        "\"directPath\":",
        "\"tourney\":",
    ];
    let mut offset = 0;
    for key in ordered {
        let found = text[offset..]
            .find(key)
            .unwrap_or_else(|| panic!("missing {key}: {text}"));
        offset += found + key.len();
    }
    assert!(!text.contains("\"rankedPlay\":"));
    assert!(text.ends_with(r#""clients":[]}}"#));
    assert!(text.contains(r#""hitErrorArray":{"all":[],"press":[],"release":[]}"#));
}

#[test]
fn play_threads_press_and_release_hit_errors_with_exact_wire_shape() {
    let base = StateBundle::new(ClientType::Stable, 1, Arc::new(NeverCalculate)).snapshot();
    let mut precise = (*base.precise).clone();
    precise.hit_errors = Arc::new(HitErrorArray {
        all: vec![-3.5, 6.25],
        press: vec![-3.5],
        release: vec![6.25],
    });
    let state = Arc::new(StateBundleSnapshot {
        source: base.source,
        generation: base.generation,
        reset_epoch: base.reset_epoch,
        regular: Arc::clone(&base.regular),
        precise: Arc::new(precise),
    });
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        1,
        Some(10),
        true,
        ApiRuntimeSettings::default(),
        [manager_entry(10, state, false, None)],
    )));

    assert!(
        json(&build(&snapshot).unwrap().play)
            .contains(r#""hitErrorArray":{"all":[-3.5,6.25],"press":[-3.5],"release":[6.25]}"#)
    );
}

#[test]
fn timing_windows_cover_both_clients_and_all_rulesets_with_live_maps() {
    let cases: &[(Ruleset, &[&str])] = &[
        (Ruleset::Osu, &["0", "50", "100", "300"]),
        (Ruleset::Taiko, &["0", "100", "300"]),
        (Ruleset::Catch, &["300"]),
        (Ruleset::Mania, &["0", "50", "100", "katu", "300", "geki"]),
    ];

    for source in [ClientType::Stable, ClientType::Lazer] {
        for &(mode, expected_keys) in cases {
            let mods = if source == ClientType::Stable {
                vec![applied_mod("CL")]
            } else {
                Vec::new()
            };
            let parsed = parsed_timing_map(mode, mode, source, mods, 1.0);
            let windows = serde_json::to_value(timing_windows(Some(&parsed))).unwrap();
            let object = windows.as_object().unwrap();
            assert_eq!(object.len(), expected_keys.len(), "{source:?} {mode:?}");
            for key in expected_keys {
                assert!(
                    object.contains_key(*key),
                    "{source:?} {mode:?} missing {key}"
                );
            }
        }
    }
}

#[test]
fn timing_windows_match_difficulty_modifier_matrix_at_od_five() {
    let cases = [
        (
            Ruleset::Osu,
            "EZ",
            serde_json::json!({"0": 400.0, "50": 174.5, "100": 119.5, "300": 64.5}),
        ),
        (
            Ruleset::Osu,
            "HR",
            serde_json::json!({"0": 400.0, "50": 129.5, "100": 83.5, "300": 37.5}),
        ),
        (
            Ruleset::Osu,
            "DA",
            serde_json::json!({"0": 400.0, "50": 119.5, "100": 75.5, "300": 31.5}),
        ),
        (
            Ruleset::Taiko,
            "EZ",
            serde_json::json!({"0": 114.5, "100": 99.5, "300": 41.5}),
        ),
        (
            Ruleset::Taiko,
            "HR",
            serde_json::json!({"0": 84.5, "100": 67.5, "300": 28.5}),
        ),
        (
            Ruleset::Taiko,
            "DA",
            serde_json::json!({"0": 79.5, "100": 61.5, "300": 25.5}),
        ),
        (Ruleset::Catch, "EZ", serde_json::json!({"300": 57.65})),
        (Ruleset::Catch, "HR", serde_json::json!({"300": 33.73})),
        (Ruleset::Catch, "DA", serde_json::json!({"300": 30.74})),
        (
            Ruleset::Mania,
            "EZ",
            serde_json::json!({
                "0": 242.5,
                "50": 190.5,
                "100": 156.5,
                "katu": 114.5,
                "300": 68.5,
                "geki": 27.5,
            }),
        ),
        (
            Ruleset::Mania,
            "HR",
            serde_json::json!({
                "0": 123.5,
                "50": 97.5,
                "100": 80.5,
                "katu": 58.5,
                "300": 35.5,
                "geki": 13.5,
            }),
        ),
        (
            Ruleset::Mania,
            "DA",
            serde_json::json!({
                "0": 164.5,
                "50": 127.5,
                "100": 103.5,
                "katu": 73.5,
                "300": 40.5,
                "geki": 16.5,
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
            let parsed = parsed_timing_map(mode, mode, source, vec![game_mod.clone()], 1.0);

            assert_eq!(
                serde_json::to_value(timing_windows(Some(&parsed))).unwrap(),
                expected,
                "{source:?} {mode:?} {acronym}",
            );
        }
    }
}

#[test]
fn timing_windows_match_stable_classic_mania_difficulty_modifiers() {
    for (acronym, expected) in [
        (
            "EZ",
            serde_json::json!({
                "0": 242.5,
                "50": 190.5,
                "100": 156.5,
                "katu": 114.5,
                "300": 68.5,
                "geki": 22.5,
            }),
        ),
        (
            "HR",
            serde_json::json!({
                "0": 123.5,
                "50": 97.5,
                "100": 80.5,
                "katu": 58.5,
                "300": 35.5,
                "geki": 11.5,
            }),
        ),
    ] {
        let parsed = parsed_timing_map(
            Ruleset::Mania,
            Ruleset::Mania,
            ClientType::Stable,
            vec![applied_mod("CL"), applied_mod(acronym)],
            1.0,
        );

        assert_eq!(
            serde_json::to_value(timing_windows(Some(&parsed))).unwrap(),
            expected,
            "Stable Mania {acronym}",
        );
    }
}

#[test]
fn timing_windows_project_fixed_mania_rates_after_source_rounding() {
    for (label, game_mod, clock_rate, lazer_expected, stable_perfect) in [
        (
            "DT 1.5x",
            applied_mod("DT"),
            1.5,
            serde_json::json!({
                "0": 260.25,
                "50": 204.75,
                "100": 168.75,
                "katu": 123.75,
                "300": 74.25,
                "geki": 29.25,
            }),
            24.75,
        ),
        (
            "HT 0.75x",
            applied_mod("HT"),
            0.75,
            serde_json::json!({
                "0": 130.13,
                "50": 102.38,
                "100": 84.38,
                "katu": 61.88,
                "300": 37.13,
                "geki": 14.63,
            }),
            12.38,
        ),
        (
            "DT 1.3x",
            applied_mod_with_settings("DT", r#"{"speed_change":1.3}"#),
            1.3,
            serde_json::json!({
                "0": 225.55,
                "50": 177.45,
                "100": 146.25,
                "katu": 107.25,
                "300": 64.35,
                "geki": 25.35,
            }),
            21.45,
        ),
    ] {
        let lazer = parsed_timing_map(
            Ruleset::Mania,
            Ruleset::Mania,
            ClientType::Lazer,
            vec![game_mod.clone()],
            clock_rate,
        );
        assert_eq!(
            serde_json::to_value(timing_windows(Some(&lazer))).unwrap(),
            lazer_expected,
            "Lazer Mania {label}",
        );

        let stable = parsed_timing_map(
            Ruleset::Mania,
            Ruleset::Mania,
            ClientType::Stable,
            vec![applied_mod("CL"), game_mod],
            clock_rate,
        );
        let mut stable_expected = lazer_expected;
        stable_expected["geki"] = serde_json::json!(stable_perfect);
        assert_eq!(
            serde_json::to_value(timing_windows(Some(&stable))).unwrap(),
            stable_expected,
            "Stable Mania {label}",
        );
    }
}

#[test]
fn timing_windows_round_osu_dt_once_at_the_api_edge() {
    let parsed = parsed_timing_map(
        Ruleset::Osu,
        Ruleset::Osu,
        ClientType::Lazer,
        vec![applied_mod("DT")],
        1.5,
    );
    assert_eq!(
        serde_json::to_value(timing_windows(Some(&parsed))).unwrap(),
        serde_json::json!({"0": 266.67, "50": 99.67, "100": 66.33, "300": 33.0})
    );
}

#[test]
fn timing_windows_apply_lazer_extended_da_before_clock_rate_scaling() {
    let da = || AppliedMod {
        acronym: "DA".to_owned(),
        settings_json: Some(r#"{"overall_difficulty":11.0,"extended_limits":true}"#.to_owned()),
    };

    let parsed = parsed_timing_map(
        Ruleset::Osu,
        Ruleset::Osu,
        ClientType::Lazer,
        vec![da()],
        1.0,
    );
    assert_eq!(
        serde_json::to_value(timing_windows(Some(&parsed))).unwrap(),
        serde_json::json!({"0": 400.0, "50": 89.5, "100": 51.5, "300": 13.5})
    );

    let parsed = parsed_timing_map(
        Ruleset::Osu,
        Ruleset::Osu,
        ClientType::Lazer,
        vec![da()],
        1.5,
    );
    assert_eq!(
        serde_json::to_value(timing_windows(Some(&parsed))).unwrap(),
        serde_json::json!({"0": 266.67, "50": 59.67, "100": 34.33, "300": 9.0})
    );
}

#[test]
fn timing_windows_apply_lazer_extended_da_to_classic_mania() {
    let difficulty_adjust = AppliedMod {
        acronym: "DA".to_owned(),
        settings_json: Some(r#"{"overall_difficulty":11.0,"extended_limits":true}"#.to_owned()),
    };
    let parsed = parsed_timing_map(
        Ruleset::Mania,
        Ruleset::Mania,
        ClientType::Lazer,
        vec![applied_mod("CL"), difficulty_adjust],
        1.0,
    );

    assert_eq!(
        serde_json::to_value(timing_windows(Some(&parsed))).unwrap(),
        serde_json::json!({
            "0": 158.5,
            "50": 121.5,
            "100": 97.5,
            "katu": 67.5,
            "300": 34.5,
            "geki": 16.5,
        })
    );
}

#[test]
fn timing_windows_match_lazer_mania_da_od_twelve_point_five() {
    let difficulty_adjust = AppliedMod {
        acronym: "DA".to_owned(),
        settings_json: Some(r#"{"overall_difficulty":12.5,"extended_limits":true}"#.to_owned()),
    };
    let parsed = parsed_timing_map(
        Ruleset::Mania,
        Ruleset::Mania,
        ClientType::Lazer,
        vec![difficulty_adjust],
        1.0,
    );

    assert_eq!(
        serde_json::to_value(timing_windows(Some(&parsed))).unwrap(),
        serde_json::json!({
            "0": 150.5,
            "50": 113.5,
            "100": 89.5,
            "katu": 59.5,
            "300": 26.5,
            "geki": 11.5,
        })
    );
}

#[test]
fn timing_windows_use_classic_map_values_for_stable_converted_mania() {
    let parsed = parsed_timing_map(
        Ruleset::Osu,
        Ruleset::Mania,
        ClientType::Stable,
        vec![applied_mod("CL")],
        1.0,
    );
    assert_eq!(
        serde_json::to_value(timing_windows(Some(&parsed))).unwrap()["300"],
        serde_json::json!(34.5)
    );
}

#[test]
fn timing_windows_use_score_v2_map_values_for_stable_mania() {
    let parsed = parsed_timing_map(
        Ruleset::Mania,
        Ruleset::Mania,
        ClientType::Stable,
        vec![applied_mod("CL"), applied_mod("SV2")],
        1.0,
    );
    let windows = serde_json::to_value(timing_windows(Some(&parsed))).unwrap();
    assert_eq!(windows["300"], serde_json::json!(49.5));
    assert_eq!(windows["geki"], serde_json::json!(19.5));
}

#[test]
fn timing_windows_expose_catch_spatial_boundary_for_all_clients_and_speeds() {
    for source in [ClientType::Stable, ClientType::Lazer] {
        for (mods, clock_rate) in [
            (Vec::new(), 1.0),
            (vec![applied_mod("DT")], 1.5),
            (vec![applied_mod("HT")], 0.75),
        ] {
            let parsed =
                parsed_timing_map(Ruleset::Catch, Ruleset::Catch, source, mods, clock_rate);
            let state = StateBundle::new(source, 1, Arc::new(NeverCalculate)).snapshot();
            let stats = serde_json::to_value(build_stats(&state.regular, Some(&parsed))).unwrap();
            assert_eq!(
                stats["timingWindows"],
                serde_json::json!({"300": 42.7}),
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
fn timing_windows_are_empty_for_missing_maps_and_default_responses() {
    assert_eq!(
        serde_json::to_value(timing_windows(None)).unwrap(),
        serde_json::json!({})
    );

    for source in [ClientType::Stable, ClientType::Lazer] {
        let response: serde_json::Value =
            serde_json::from_slice(&encode(&default_snapshot(source)).unwrap()).unwrap();
        assert_eq!(
            response
                .get("beatmap")
                .and_then(|beatmap| beatmap.get("stats"))
                .and_then(|stats| stats.get("timingWindows")),
            Some(&serde_json::json!({})),
            "{source:?}"
        );
    }
}

#[test]
fn stable_tournament_is_sorted_by_fractional_ipc_id_not_pid() {
    let base = StateBundle::new(ClientType::Stable, 1, Arc::new(NeverCalculate)).snapshot();
    let manager = manager_entry(50, Arc::clone(&base), true, None);
    let spectator = |pid, ipc, name: &str| {
        let mut regular = (*base.regular).clone();
        regular.tourney.user = Some(TourneyUserState {
            accuracy: 98.5,
            ranked_score: 3.0,
            play_count: 4,
            global_rank: 5,
            pp: 6.5,
            name: name.to_owned(),
            country: "US".to_owned(),
            id: i64::from(pid),
        });
        manager_entry(pid, with_regular(&base, regular), false, Some(ipc))
    };
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        2,
        Some(50),
        false,
        ApiRuntimeSettings::default(),
        [
            manager,
            spectator(90, 7.5, "later"),
            spectator(80, 2.25, "first"),
        ],
    )));
    let tourney = build(&snapshot).unwrap().tourney.unwrap();
    assert_eq!(
        tourney
            .clients
            .iter()
            .map(|client| client.ipc_id)
            .collect::<Vec<_>>(),
        [2.25, 7.5]
    );
    assert_eq!(
        tourney
            .clients
            .iter()
            .map(|client| client.user.global_rank)
            .collect::<Vec<_>>(),
        [0, 0]
    );
    let encoded = json(&tourney);
    assert!(encoded.find(r#""ipcId":2.25"#).unwrap() < encoded.find(r#""ipcId":7.5"#).unwrap());
    assert!(encoded.contains(r#""hitErrorArray":{"all":[],"press":[],"release":[]}"#));
}
#[test]
fn stable_tournament_missing_user_projects_source_zero_defaults() {
    let base = StateBundle::new(ClientType::Stable, 1, Arc::new(NeverCalculate)).snapshot();
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        2,
        Some(50),
        false,
        ApiRuntimeSettings::default(),
        [
            manager_entry(50, Arc::clone(&base), true, None),
            manager_entry(60, Arc::clone(&base), false, Some(1.0)),
        ],
    )));
    let client = build(&snapshot).unwrap().tourney.unwrap().clients.remove(0);
    assert_eq!(client.user.id, 0);
    assert_eq!(client.user.name, "");
    assert_eq!(client.user.country, "");
    assert_eq!(client.user.accuracy, 0.0);
    assert_eq!(client.user.ranked_score, 0.0);
    assert_eq!(client.user.play_count, 0);
    assert_eq!(client.user.global_rank, 0);
    assert_eq!(client.user.total_pp, 0.0);
}

#[test]
fn javascript_numbers_paths_defaults_and_dynamic_omissions_match_source() {
    assert_eq!(
        json(&Session {
            play_time: 1_000_000_000_000_000_128.0,
            play_count: 0,
        }),
        r#"{"playTime":1000000000000000100,"playCount":0}"#
    );
    assert_eq!(
        json(&Session {
            play_time: 1e20,
            play_count: 0,
        }),
        r#"{"playTime":100000000000000000000,"playCount":0}"#
    );
    assert_eq!(
        json(&Session {
            play_time: 1e21,
            play_count: 0,
        }),
        r#"{"playTime":1e+21,"playCount":0}"#
    );
    let mut tournament = empty_tourney();
    tournament.total_score.left = 9_007_199_254_740_993_u64 as f64;
    tournament.total_score.right = 9_007_199_254_740_995_u64 as f64;
    let tournament_json = json(&tournament);
    assert!(
        tournament_json
            .contains(r#""totalScore":{"left":9007199254740992,"right":9007199254740996}"#)
    );
    assert_eq!(source_country_name(216), "TL");
    assert_eq!(source_country_name(239), "RS");
    assert_eq!(source_country_name(242), "ME");
    assert_eq!(source_country_name(245), "A2");
    assert_eq!(source_country_name(252), "MF");
    assert_eq!(source_country_name(253), "");
    let mut response = build(&default_snapshot(ClientType::Stable)).unwrap();
    let plus_one = 9_007_199_254_740_993_u64 as f64;
    let plus_three = 9_007_199_254_740_995_u64 as f64;
    response.play.score = plus_one;
    response.results_screen.score = plus_three;
    assert!(json(&response.play).contains(r#""score":9007199254740992"#));
    assert!(json(&response.results_screen).contains(r#""score":9007199254740996"#));
    let leaderboard = Leaderboard {
        is_failed: false,
        position: 0,
        team: 0,
        id: 0,
        name: String::new(),
        score: plus_three,
        accuracy: 0.0,
        hits: LeaderboardHits {
            h0: 0,
            h50: 0,
            h100: 0,
            h300: 0,
            geki: 0,
            katu: 0,
        },
        combo: Combo { current: 0, max: 0 },
        mods: response.play.mods.clone(),
        rank: String::new(),
    };
    assert!(json(&leaderboard).contains(r#""score":9007199254740996"#));
    assert_eq!(
        json(&ScrollDirection {
            number: 99,
            name: None,
        }),
        r#"{"number":99}"#
    );

    let native_a_c = PathBuf::from("a").join("c").to_string_lossy().into_owned();
    let native_a_b = PathBuf::from("a").join("b").to_string_lossy().into_owned();
    assert_eq!(direct_join(None, None), ".");
    assert_eq!(direct_join(Some("a//b"), Some("../c")), native_a_c);
    assert_eq!(direct_join(Some("  a  "), Some("")), "  a  ");
    assert_eq!(direct_join(Some("a"), Some("/b")), native_a_b);
    #[cfg(unix)]
    assert_eq!(direct_join(Some(r"a\b"), Some(r"..\c")), r"a\b/..\c");
    #[cfg(windows)]
    assert_eq!(direct_join(Some(r"a\b"), Some(r"..\c")), r"a\c");

    let mut settings = SettingsObject::new();
    settings.insert("number".to_owned(), Value::Null);
    assert!(f64_value(&settings, &["number"]).is_nan());
    assert_eq!(
        json(&ScoreMeter {
            kind: number_name(Some(0), ""),
            size: f64_value(&settings, &["number"]),
        }),
        r#"{"type":{"number":0,"name":""},"size":null}"#
    );
    assert_eq!(
        json(&BeatmapStatus {
            number: None,
            name: String::new(),
        }),
        r#"{"name":""}"#
    );
    assert_eq!(
        json(&BeatmapStatus {
            number: Some(f64::NAN),
            name: String::new(),
        }),
        r#"{"number":null,"name":""}"#
    );

    assert_eq!(truthy(Some(0.0)), None);
    assert_eq!(truthy(Some(f64::NAN)), None);
    assert_eq!(truthy(Some(f64::INFINITY)), Some(f64::INFINITY));

    let normal = PlayHits::Normal(Hits {
        h300: 4,
        geki: 5,
        h100: 3,
        katu: 6,
        h50: 2,
        h0: 1,
        slider_breaks: 10,
        slider_end_hits: 7,
        small_tick_hits: 8,
        large_tick_hits: 9,
    });
    assert_eq!(
        json(&normal),
        r#"{"0":1,"50":2,"100":3,"300":4,"geki":5,"katu":6,"sliderBreaks":10,"sliderEndHits":7,"smallTickHits":8,"largeTickHits":9}"#
    );
    let lazer = PlayHits::Lazer(LazerHits {
        h0: 1,
        h50: 2,
        h100: 3,
        h300: 4,
        geki: 5,
        katu: 6,
        slider_end_hits: 7,
        small_tick_hits: 8,
        large_tick_hits: 9,
        slider_breaks: 10,
    });
    assert_eq!(
        json(&lazer),
        r#"{"0":1,"50":2,"100":3,"300":4,"geki":5,"katu":6,"sliderEndHits":7,"smallTickHits":8,"largeTickHits":9,"sliderBreaks":10}"#
    );
}

#[test]
fn precise_status_and_default_beatmap_drive_source_projection() {
    let base = StateBundle::new(ClientType::Stable, 1, Arc::new(NeverCalculate)).snapshot();
    for converted_mode in 1..=3 {
        let mut regular = (*base.regular).clone();
        regular.global.status = 0;
        regular.gameplay.mode = converted_mode;
        regular.beatmap.mode = Some(0);
        let mut precise = (*base.precise).clone();
        precise.status = 2;
        let state = Arc::new(StateBundleSnapshot {
            source: base.source,
            generation: base.generation,
            reset_epoch: base.reset_epoch,
            regular: Arc::new(regular),
            precise: Arc::new(precise),
        });
        let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
            2,
            Some(10),
            true,
            ApiRuntimeSettings::default(),
            [manager_entry(10, state, false, None)],
        )));
        let response = build(&snapshot).unwrap();
        assert_eq!(response.state.number, Some(2));
        assert_eq!(response.beatmap.mode.number, Some(0));
        assert_eq!(response.beatmap.mode.name, "osu");
        assert!(
            response.beatmap.is_convert,
            "converted mode {converted_mode}"
        );
        assert_eq!(response.beatmap.status.number, None);
        assert_eq!(response.beatmap.status.name, "");
        assert_eq!(response.folders.game, "");
        assert_eq!(response.folders.skin, "");
        assert_eq!(response.folders.songs, "");
        assert_eq!(response.direct_path.skin_folder, "");
    }
}

#[test]
fn lazer_lobby_without_spectator_data_omits_tourney() {
    let base = StateBundle::new(ClientType::Lazer, 1, Arc::new(NeverCalculate)).snapshot();
    let regular = (*base.regular).clone();
    let mut precise = (*base.precise).clone();
    precise.status = 11;
    let state = Arc::new(StateBundleSnapshot {
        source: base.source,
        generation: base.generation,
        reset_epoch: base.reset_epoch,
        regular: Arc::new(regular),
        precise: Arc::new(precise),
    });
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        2,
        Some(10),
        true,
        ApiRuntimeSettings::default(),
        [manager_entry(10, state, false, None)],
    )));
    let response = build(&snapshot).unwrap();
    assert!(response.tourney.is_none());
    assert!(!json(&response).contains("\"tourney\":"));
    assert_eq!(zero_stats(TimingWindows::default()).stars.aim, Some(0.0));
}
