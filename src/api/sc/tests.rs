use std::sync::Arc;

use crate::api::{ApiRuntimeSettings, ManagerInstanceSnapshot, ManagerSnapshot};
use crate::calculation::beatmap::{EventStatus, GraphSeries, StrainGraph};
use crate::calculation::progressive::{PerformanceAttributes, ProgressiveAttributes};
use crate::instance::InstanceSettings;
use crate::model::enums::ClientType;
use crate::model::memory::User;
use crate::model::states::{
    CalculateMods, HitErrorArray, KeyOverlayButton, KeypressButton, LeaderboardPlayer, Statistics,
};
use crate::state::{
    BassDensityState, BeatmapPPState, GameplayState, GlobalState, LazerMultiSpectatingState,
    MenuState, PreciseState, RankedPlayState, RegularState, ResultScreenState, SettingsState,
    StateBundleSnapshot, TourneyManagerState, UserState,
};

use super::*;

fn mods(number: i64, name: &str, rate: f64) -> CalculateMods {
    CalculateMods {
        checksum: String::new(),
        number,
        name: name.to_owned(),
        array: Vec::new(),
        rate,
    }
}

fn statistics(great: i64, ok: i64, meh: i64, miss: i64) -> Statistics {
    Statistics {
        perfect: 0,
        great,
        good: 0,
        ok,
        meh,
        miss,
        small_tick_miss: 0,
        small_tick_hit: 0,
        large_tick_miss: 0,
        large_tick_hit: 0,
        small_bonus: 0,
        large_bonus: 0,
        ignore_miss: 0,
        ignore_hit: 0,
        combo_break: 0,
        slider_tail_hit: 0,
        legacy_combo_increase: None,
    }
}

fn regular() -> RegularState {
    RegularState {
        global: GlobalState::default(),
        settings: SettingsState::default(),
        beatmap: BeatmapPPState::default(),
        menu: MenuState::default(),
        bass_density: BassDensityState::default(),
        gameplay: GameplayState::default(),
        result_screen: ResultScreenState::default(),
        tourney: TourneyManagerState::default(),
        user: UserState::default(),
        lazer_multi_spectating: LazerMultiSpectatingState::default(),
        ranked_play: RankedPlayState::default(),
        room: Default::default(),
    }
}

fn snapshot(regular: RegularState, enabled: bool) -> CombinedSnapshot {
    let status = regular.global.status;
    let play_time = regular.global.play_time;
    snapshot_with_precise(regular, enabled, status, play_time)
}

fn snapshot_with_precise(
    mut regular: RegularState,
    enabled: bool,
    status: i64,
    play_time: f64,
) -> CombinedSnapshot {
    regular.user.profile = Some(User {
        name: "bancho".to_owned(),
        accuracy: 98.5,
        ranked_score: 1.0,
        id: 42,
        level: 1.0,
        play_count: 2,
        play_mode: 0,
        rank: 3,
        country_code: 225,
        performance_points: 4.0,
        raw_bancho_status: 5,
        background_colour: 0,
        raw_login_status: 65_793,
        matchmaking: None,
    });
    let precise = Arc::new(PreciseState {
        status,
        play_time,
        mode: 0,
        key_overlay: Arc::new(vec![
            KeyOverlayButton {
                name: "K1".into(),
                is_pressed: true,
                count: 1,
            },
            KeyOverlayButton {
                name: "K2".into(),
                is_pressed: false,
                count: 2,
            },
        ]),
        keypresses: Arc::new(vec![
            KeypressButton {
                name: "K1".into(),
                is_pressed: true,
                count: 1,
                pressed_at: Some(play_time),
                released_at: None,
            },
            KeypressButton {
                name: "K2".into(),
                is_pressed: false,
                count: 2,
                pressed_at: None,
                released_at: None,
            },
        ]),
        hit_errors: Arc::new(HitErrorArray {
            all: vec![-4.0, 5.0],
            press: vec![-4.0],
            release: vec![5.0],
        }),
        total_hit_errors: 1.0,
        committed_hit_error_index: 2,
        unstable_rate: 12.345,
    });
    let state = Arc::new(StateBundleSnapshot {
        source: ClientType::Stable,
        generation: 1,
        reset_epoch: 0,
        regular: Arc::new(regular),
        precise,
    });
    let entry = ManagerInstanceSnapshot::new(
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
        ApiRuntimeSettings {
            enable_key_overlay: enabled,
        },
        [entry],
    )))
}

#[test]
fn missing_focus_is_not_ready() {
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::not_ready(
        0,
        ApiRuntimeSettings::default(),
    )));
    assert_eq!(build(&snapshot), Err(ApiError::NotReady));
    assert_eq!(encode(&snapshot), Err(ApiError::NotReady));
}

#[test]
fn menu_and_play_select_exact_modes_mods_paths_and_precise_values() {
    let mut menu = regular();
    menu.global.menu_mods = mods(8, "HD", 1.0);
    menu.global.play_time = 61_234.0;
    menu.global.skin_folder = "skin-dir".to_owned();
    menu.menu.gamemode = Some(1);
    menu.menu.artist = Some("artist".to_owned());
    menu.menu.artist_original = Some("artist-u".to_owned());
    menu.menu.title = Some("title".to_owned());
    menu.menu.title_original = Some("title-u".to_owned());
    menu.menu.difficulty = Some("Insane".to_owned());
    menu.menu.folder = Some("set".to_owned());
    menu.menu.filename = Some("map.osu".to_owned());
    menu.menu.background_filename = Some("bg.jpg".to_owned());
    menu.menu.map_id = Some(123);
    menu.menu.set_id = Some(456);
    menu.menu.mp3_length = Some(99_000.0);
    menu.settings
        .values
        .insert("leaderboardType".to_owned(), serde_json::json!(4));

    let menu_result = build(&snapshot(menu, false)).expect("menu response");
    assert_eq!(menu_result.game_mode.as_deref(), Some("taiko"));
    assert_eq!(menu_result.mods, "HD");
    assert_eq!(menu_result.map_artist_title, "artist - title");
    assert_eq!(menu_result.osu_file_location, safe_join(["set", "map.osu"]));
    assert_eq!(
        menu_result.background_image_location,
        safe_join(["set", "bg.jpg"])
    );
    assert_eq!(menu_result.map_position, "00:01:01.234");
    assert_eq!(menu_result.song_selection_ranking_type, 4);
    assert_eq!(menu_result.bancho_country, "US");
    assert_eq!(menu_result.bancho_is_connected, 1);
    assert_eq!(menu_result.hit_errors, [-4.0, 5.0]);
    assert_eq!(menu_result.press_hit_errors, [-4.0]);
    assert_eq!(menu_result.release_hit_errors, [5.0]);
    let value = serde_json::to_value(&menu_result).unwrap();
    assert_eq!(value["hitErrors"], serde_json::json!([-4.0, 5.0]));
    assert_eq!(value["pressHitErrors"], serde_json::json!([-4.0]));
    assert_eq!(value["releaseHitErrors"], serde_json::json!([5.0]));
    assert!(
        serde_json::to_string(&menu_result)
            .unwrap()
            .contains(r#""hitErrors":[-4.0,5.0],"pressHitErrors":[-4.0],"releaseHitErrors":[5.0]"#)
    );
    assert_eq!(menu_result.unstable_rate, 12.35);
    assert_eq!(
        menu_result.osu_90_pp, 0.0,
        "disabled calculation publishes zero"
    );
    assert_eq!(
        menu_result.osu_sspp, 0.0,
        "disabled calculation publishes zero"
    );
    assert_eq!(
        menu_result.key_overlay,
        r#"{"Enabled":false,"K1Pressed":true,"K1Count":1,"K2Pressed":false,"K2Count":2,"M1Pressed":false,"M1Count":0,"M2Pressed":false,"M2Count":0}"#
    );

    let mut play = regular();
    play.global.status = PLAY;
    play.gameplay.mode = 3;
    play.gameplay.mods = mods(64, "DT", 1.5);
    play.gameplay.grade_current = "A".to_owned();
    play.gameplay.grade_expected = "S".to_owned();
    play.gameplay.accuracy = 97.126;
    play.gameplay.statistics = statistics(300, 20, 4, 2);
    play.beatmap.attributes = Some(ProgressiveAttributes {
        stars: 6.126,
        pp: 123.456,
        fc_pp: 234.567,
        current: PerformanceAttributes {
            accuracy: 10.126,
            aim: 20.126,
            speed: 30.126,
            difficulty: 40.126,
            flashlight: 0.0,
            ..PerformanceAttributes::default()
        },
        ..ProgressiveAttributes::default()
    });
    play.beatmap.pp_acc.insert(90, 90.126);
    play.beatmap.pp_acc.insert(100, 100.126);
    let play_result = build(&snapshot(play, true)).expect("play response");
    assert_eq!(play_result.game_mode.as_deref(), Some("mania"));
    assert_eq!(play_result.mods, "DT");
    assert_eq!(play_result.mods_enum, 64);
    assert_eq!(play_result.unstable_rate, 18.52);
    assert_eq!(play_result.grade, Some(4));
    assert_eq!(play_result.max_grade, Some(3));
    assert_eq!(play_result.pp_if_map_ends_now, 123.46);
    assert_eq!(play_result.pp_if_rest_fced, 234.57);
    assert_eq!(play_result.acc_pp_if_map_ends_now, 10.13);
    assert_eq!(play_result.strain_pp_if_map_ends_now, 40.13);
    assert_eq!(play_result.osu_90_pp, 90.13);
    assert_eq!(play_result.osu_m90_pp, 90.13);
    assert_eq!(play_result.osu_sspp, 100.13);
    assert_eq!(play_result.osu_m_sspp, 100.13);
}

#[test]
fn precise_only_publication_drives_status_time_and_play_selection() {
    let mut state = regular();
    state.global.status = 0;
    state.global.play_time = 1.0;
    state.global.menu_mods = mods(8, "HD", 1.0);
    state.menu.gamemode = Some(1);
    state.gameplay.mode = 3;
    state.gameplay.mods = mods(64, "DT", 1.5);

    let result = build(&snapshot_with_precise(state, true, PLAY, 61_234.5))
        .expect("precise publication response");
    assert_eq!(result.raw_status, PLAY);
    assert_eq!(result.status, PLAY);
    assert_eq!(result.mode, Some(3));
    assert_eq!(result.game_mode.as_deref(), Some("mania"));
    assert_eq!(result.mods, "DT");
    assert_eq!(result.time, 61.2345);
    assert_eq!(result.map_position, "00:01:01.234.5");
    assert_eq!(result.time_left, format_milliseconds(-61_234.5));
}

#[test]
fn source_millisecond_format_preserves_fractional_and_nonfinite_strings() {
    assert_eq!(format_milliseconds(61_234.0), "00:01:01.234");
    assert_eq!(format_milliseconds(1.0), "00:00:00.1");
    assert_eq!(format_milliseconds(f64::NAN), "NaN:NaN:NaN.NaN");
    assert_eq!(format_milliseconds(f64::INFINITY), "Infinity:NaN:NaN.NaN");
}

#[test]
fn leaderboard_nested_strings_have_exact_source_key_order() {
    let mut state = regular();
    state.gameplay.leaderboard_visible = true;
    state.gameplay.leaderboard_player = LeaderboardPlayerState {
        user_id: Some(7),
        name: "main".to_owned(),
        score: 900 as _,
        combo: 8,
        accuracy: 99.0,
        max_combo: 10,
        mods: mods(16, "HR", 1.0),
        statistics: statistics(3, 2, 1, 4),
        team: 1,
        position: 2,
        is_passing: true,
    };
    state.gameplay.leaderboard_scores.push(LeaderboardPlayer {
        user_id: 8,
        name: "other".to_owned(),
        score: 800 as _,
        combo: 7,
        accuracy: 98.0,
        max_combo: 9,
        mods: mods(8, "HD", 1.0),
        statistics: statistics(30, 20, 10, 5),
        team: 2,
        position: 3,
        is_passing: false,
    });
    let result = build(&snapshot(state, true)).expect("response");
    assert_eq!(
        result.leader_board_main_player,
        r#"{"IsLeaderboardVisible":true,"Username":"main","Score":900,"Combo":8,"MaxCombo":10,"Mods":{"ModsXor1":-1,"ModsXor2":-1,"Value":16},"Hit300":3,"Hit100":2,"Hit50":1,"HitMiss":4,"Team":1,"Position":2,"IsPassing":true}"#
    );
    assert_eq!(
        result.leader_board_players,
        r#"[{"Username":"other","Score":800,"Combo":7,"MaxCombo":9,"Mods":{"ModsXor1":-1,"ModsXor2":-1,"Value":8},"Hit300":30,"Hit100":20,"Hit50":10,"HitMiss":5,"Team":2,"Position":3,"IsPassing":false}]"#
    );
}

#[test]
fn total_scores_follow_javascript_number_rounding_above_two_to_the_53() {
    const SOURCE_LONG: i64 = 9_007_199_254_740_993;
    const JS_NUMBER: f64 = 9_007_199_254_740_992.0;

    let mut state = regular();
    state.gameplay.score = SOURCE_LONG as _;
    state.gameplay.leaderboard_player.score = SOURCE_LONG as _;
    state.gameplay.leaderboard_scores.push(LeaderboardPlayer {
        user_id: 8,
        name: "other".to_owned(),
        score: SOURCE_LONG as _,
        combo: 0,
        accuracy: 0.0,
        max_combo: 0,
        mods: mods(0, "", 1.0),
        statistics: statistics(0, 0, 0, 0),
        team: 0,
        position: 0,
        is_passing: false,
    });

    let snapshot = snapshot(state, true);
    let result = build(&snapshot).expect("response");
    assert_eq!(result.score, JS_NUMBER);
    assert!(
        result
            .leader_board_main_player
            .contains(r#""Score":9007199254740992"#)
    );
    assert!(
        result
            .leader_board_players
            .contains(r#""Score":9007199254740992"#)
    );
    let encoded = encode(&snapshot).expect("encoded");
    assert!(
        encoded
            .windows(b"\"score\":9007199254740992".len())
            .any(|window| window == b"\"score\":9007199254740992")
    );
}

#[test]
fn strain_integer_keys_follow_javascript_order_and_nonfinite_encodes_null() {
    let strains = map_strains(
        &[
            10.0,
            2.0,
            1.0,
            4_294_967_295.0,
            3.5,
            -0.0,
            4_294_967_294.0,
            -1.0,
            0.0,
        ],
        Some(&[1.0, -2.0, f64::INFINITY, 4.0, 5.0, 6.0, 7.0, 8.0, 9.0]),
    );
    assert_eq!(
        strains.keys().map(String::as_str).collect::<Vec<_>>(),
        ["0", "1", "2", "10", "4294967294", "4294967295", "3.5", "-1"]
    );
    assert_eq!(strains["2"], 0.0);
    assert_eq!(strains["0"], 9.0);
    assert_eq!(strains["4294967294"], 7.0);

    let mut state = regular();
    state.beatmap.graph = Arc::new(StrainGraph {
        primary: Vec::new(),
        series: vec![GraphSeries {
            name: "aim".to_owned(),
            data: vec![f64::INFINITY],
        }],
        x_axis: vec![1.0],
    });
    state.beatmap.event_status = EventStatus {
        bpm: f64::INFINITY,
        kiai: false,
        break_time: false,
    };
    state.gameplay.player_hp = f64::NAN;
    state.global.play_time = f64::INFINITY;
    state.menu.mp3_length = Some(f64::INFINITY);
    let snapshot = snapshot(state, true);
    let bytes = encode(&snapshot).expect("encoded");
    let text = std::str::from_utf8(&bytes).expect("utf8");
    assert!(text.contains(r#""currentBpm":null"#));
    assert!(text.contains(r#""mapStrains":{"1":null}"#));
    assert!(text.contains(r#""playerHp":null"#));
    assert!(text.contains(r#""time":null"#));
    assert!(text.contains(r#""totalAudioTime":null"#));
    assert_eq!(
        bytes,
        encode(&snapshot).expect("cached deterministic bytes")
    );
}

#[test]
fn full_time_projection_preserves_javascript_number_boundaries() {
    let (full, _, draining) = map_time_projection(12.5, 2, 1.0);
    assert_eq!(full, 12.5);
    assert_eq!(draining, 10.5);
    assert_eq!(
        String::from_utf8(encode_json_bytes(&full).unwrap()).unwrap(),
        "12.5"
    );

    let (full, _, draining) = map_time_projection(1e21, 2, 1.0);
    assert_eq!(full, 1e21);
    assert_eq!(draining, 1e21);
    assert_eq!(
        String::from_utf8(encode_json_bytes(&full).unwrap()).unwrap(),
        "1e+21"
    );

    let (full, _, draining) = map_time_projection(f64::INFINITY, 2, 1.0);
    assert!(full.is_infinite());
    assert!(draining.is_infinite());
    assert_eq!(
        String::from_utf8(encode_json_bytes(&full).unwrap()).unwrap(),
        "null"
    );
}

#[test]
fn full_root_order_types_and_all_exact_placeholders_are_frozen() {
    let mut state = regular();
    state.menu.artist = Some("artist".to_owned());
    state.menu.artist_original = Some("artist-u".to_owned());
    state.menu.title = Some("title".to_owned());
    state.menu.title_original = Some("title-u".to_owned());
    state.menu.difficulty = Some("difficulty".to_owned());
    state.menu.creator = Some("creator".to_owned());
    state.menu.checksum = Some("checksum".to_owned());
    state.menu.gamemode = Some(0);
    state.menu.mp3_length = Some(1.0);
    state.menu.map_id = Some(2);
    state.menu.set_id = Some(3);
    state.menu.audio_filename = Some("audio.mp3".to_owned());
    state.menu.filename = Some("map.osu".to_owned());
    state.menu.background_filename = Some("background.jpg".to_owned());
    state.menu.ranked_status = Some(4.0);
    state.menu.folder = Some("set".to_owned());
    state.gameplay.grade_current = "A".to_owned();
    state.gameplay.grade_expected = "S".to_owned();
    let snapshot = snapshot(state, true);
    let bytes = encode(&snapshot).expect("encoded");
    assert_eq!(
        bytes,
        encode(&snapshot).expect("cached deterministic bytes")
    );
    let text = std::str::from_utf8(&bytes).expect("utf8");
    let keys = [
        "osuIsRunning",
        "chatIsEnabled",
        "ingameInterfaceIsEnabled",
        "isBreakTime",
        "banchoIsConnected",
        "banchoId",
        "banchoUsername",
        "banchoStatus",
        "banchoCountry",
        "artistRoman",
        "artistUnicode",
        "titleRoman",
        "titleUnicode",
        "mapArtistTitle",
        "mapArtistTitleUnicode",
        "diffName",
        "mapDiff",
        "creator",
        "liveStarRating",
        "mStars",
        "ar",
        "mAR",
        "od",
        "mOD",
        "cs",
        "mCS",
        "hp",
        "mHP",
        "currentBpm",
        "bpm",
        "mainBpm",
        "maxBpm",
        "minBpm",
        "mMainBpm",
        "mMaxBpm",
        "mMinBpm",
        "mBpm",
        "md5",
        "gameMode",
        "mode",
        "time",
        "previewtime",
        "totaltime",
        "timeLeft",
        "drainingtime",
        "totalAudioTime",
        "firstHitObjectTime",
        "mapid",
        "mapsetid",
        "mapStrains",
        "mapBreaks",
        "mapKiaiPoints",
        "mapPosition",
        "mapTimingPoints",
        "sliders",
        "circles",
        "spinners",
        "maxCombo",
        "mp3Name",
        "osuFileName",
        "backgroundImageFileName",
        "osuFileLocation",
        "backgroundImageLocation",
        "retries",
        "username",
        "score",
        "playerHp",
        "playerHpSmooth",
        "combo",
        "currentMaxCombo",
        "keyOverlay",
        "geki",
        "c300",
        "katsu",
        "c100",
        "c50",
        "miss",
        "sliderBreaks",
        "acc",
        "unstableRate",
        "convertedUnstableRate",
        "grade",
        "maxGrade",
        "hitErrors",
        "mods",
        "modsEnum",
        "ppIfMapEndsNow",
        "ppIfRestFced",
        "leaderBoardMainPlayer",
        "leaderBoardPlayers",
        "rankedStatus",
        "songSelectionRankingType",
        "rawStatus",
        "dir",
        "dl",
        "osu_90PP",
        "osu_95PP",
        "osu_96PP",
        "osu_97PP",
        "osu_98PP",
        "osu_99PP",
        "osu_SSPP",
        "osu_m90PP",
        "osu_m95PP",
        "osu_m96PP",
        "osu_m97PP",
        "osu_m98PP",
        "osu_m99PP",
        "osu_mSSPP",
        "accPpIfMapEndsNow",
        "aimPpIfMapEndsNow",
        "speedPpIfMapEndsNow",
        "strainPpIfMapEndsNow",
        "noChokePp",
        "skin",
        "skinPath",
        "songSelectionScores",
        "songSelectionMainPlayerScore",
        "songSelectionTotalScores",
        "status",
        "osu_99_9PP",
        "osu_m99_9PP",
        "mania_1_000_000PP",
        "mania_m1_000_000PP",
        "simulatedPp",
        "plays",
        "tags",
        "source",
        "starsNomod",
        "comboLeft",
        "localTime",
        "localTimeISO",
        "sl",
        "sv",
        "threadid",
        "test",
    ];
    let mut cursor = 0;
    for key in keys {
        let needle = format!("\"{key}\":");
        let offset = text[cursor..]
            .find(&needle)
            .unwrap_or_else(|| panic!("missing or out-of-order {key}"));
        cursor += offset + needle.len();
    }
    let value: serde_json::Value = serde_json::from_slice(&bytes).expect("JSON");
    assert!(value["osuIsRunning"].is_i64());
    assert!(value["time"].is_number());
    assert!(value["keyOverlay"].is_string());
    assert!(value["mapBreaks"].is_array());
    assert!(value["mapStrains"].is_object());
    for (key, expected) in [
        ("songSelectionScores", serde_json::json!("[]")),
        ("songSelectionMainPlayerScore", serde_json::json!("{}")),
        ("songSelectionTotalScores", serde_json::json!(-1)),
        ("osu_99_9PP", serde_json::json!(-1)),
        ("osu_m99_9PP", serde_json::json!(-1)),
        ("mania_1_000_000PP", serde_json::json!(-1)),
        ("mania_m1_000_000PP", serde_json::json!(-1)),
        ("plays", serde_json::json!(0)),
        ("tags", serde_json::json!("")),
        ("source", serde_json::json!("")),
        ("starsNomod", serde_json::json!(-1)),
        ("comboLeft", serde_json::json!(-1)),
        ("localTime", serde_json::json!(0)),
        ("localTimeISO", serde_json::json!("0")),
        ("sl", serde_json::json!(-1)),
        ("sv", serde_json::json!(-1)),
        ("threadid", serde_json::json!(0)),
        ("test", serde_json::json!("")),
    ] {
        assert_eq!(value[key], expected, "{key}");
    }
}
#[test]
fn stream_companion_status_maps_replay_and_result_variants() {
    assert_eq!(stream_companion_status(2, false), 2);
    assert_eq!(stream_companion_status(2, true), 8);
    assert_eq!(stream_companion_status(1, false), 16);
    assert_eq!(stream_companion_status(4, false), 16);
    assert_eq!(stream_companion_status(7, false), 32);
    assert_eq!(stream_companion_status(18, false), 32);
    assert_eq!(stream_companion_status(0, false), 1);
}
