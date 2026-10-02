use std::ffi::OsString;
use std::sync::Arc;

use serde_json::{Value, json};

use super::*;
use crate::api::{ApiRuntimeSettings, ManagerInstanceSnapshot, ManagerSnapshot};
use crate::calculation::default_calculated_mods;
use crate::model::memory::{LazerSpectator, User};
use crate::model::states::{
    HitErrorArray, KeyOverlayButton as StateKey, KeypressButton as StateKeypress,
    TourneyManagerChatItem,
};
use crate::state::{
    BassDensityState, BeatmapPPState, GameplayState, GlobalState, LazerMultiSpectatingState,
    MenuState, PreciseState, RankedPlayState, ResultScreenState, SettingsState,
    StateBundleSnapshot, TourneyManagerState, TourneyUserState, UserState,
};

fn statistics() -> Statistics {
    Statistics {
        perfect: 0,
        great: 0,
        good: 0,
        ok: 0,
        meh: 0,
        miss: 0,
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
        legacy_combo_increase: Some(0),
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

fn bundle_with_precise(
    source: ClientType,
    regular: RegularState,
    precise: PreciseState,
) -> Arc<StateBundleSnapshot> {
    Arc::new(StateBundleSnapshot {
        source,
        generation: 1,
        reset_epoch: 0,
        regular: Arc::new(regular),
        precise: Arc::new(precise),
    })
}
fn precise() -> PreciseState {
    PreciseState {
        status: 0,
        play_time: 0.0,
        mode: 0,
        key_overlay: Arc::new(Vec::new()),
        keypresses: Arc::new(Vec::new()),
        hit_errors: Arc::new(HitErrorArray::default()),
        total_hit_errors: 0.0,
        committed_hit_error_index: 0,
        unstable_rate: 0.0,
    }
}
fn bundle(source: ClientType, regular: RegularState) -> Arc<StateBundleSnapshot> {
    let mut latest = precise();
    latest.status = regular.global.status;
    latest.play_time = regular.global.play_time;
    bundle_with_precise(source, regular, latest)
}

fn settings(ipc_id: Option<f64>) -> crate::instance::InstanceSettings {
    crate::instance::InstanceSettings {
        tournament_spectator: ipc_id.is_some(),
        tournament_ipc_id: ipc_id,
        devserver_endpoint: None::<OsString>,
    }
}

fn snapshot(
    focused_pid: u32,
    source: ClientType,
    regular: RegularState,
    extras: Vec<ManagerInstanceSnapshot>,
) -> CombinedSnapshot {
    let focused =
        ManagerInstanceSnapshot::new(focused_pid, settings(None), false, bundle(source, regular));
    CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        1,
        Some(focused_pid),
        true,
        ApiRuntimeSettings::default(),
        std::iter::once(focused).chain(extras),
    )))
}
fn snapshot_with_precise(
    focused_pid: u32,
    source: ClientType,
    regular: RegularState,
    precise: PreciseState,
) -> CombinedSnapshot {
    let focused = ManagerInstanceSnapshot::new(
        focused_pid,
        settings(None),
        false,
        bundle_with_precise(source, regular, precise),
    );
    CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        1,
        Some(focused_pid),
        true,
        ApiRuntimeSettings::default(),
        [focused],
    )))
}

#[test]
fn borrowed_encoding_preserves_populated_graph_and_hit_arrays() {
    let mut state = regular();
    let graph = Arc::make_mut(&mut state.beatmap.graph);
    graph.primary = vec![1.25, 2.5];
    graph.x_axis = vec![0.0, 400.0];
    graph.series = vec![crate::calculation::beatmap::GraphSeries {
        name: "aim".into(),
        data: vec![3.5, 4.75],
    }];
    let mut precise = precise();
    precise.hit_errors = Arc::new(HitErrorArray {
        all: vec![-3.5, 6.25],
        press: vec![-3.5],
        release: vec![6.25],
    });
    let snapshot = snapshot_with_precise(10, ClientType::Stable, state, precise);
    let owned = build(&snapshot).unwrap();
    assert_eq!(
        encode(&snapshot).unwrap().as_ref(),
        crate::api::json::encode(&owned, 0).unwrap()
    );
    let decoded: Value = serde_json::from_slice(&encode(&snapshot).unwrap()).unwrap();
    assert_eq!(decoded["menu"]["pp"]["strains"], json!([1.25, 2.5]));
    assert_eq!(
        decoded["menu"]["pp"]["strainsAll"]["series"][0]["data"],
        json!([3.5, 4.75])
    );
    assert_eq!(
        decoded["gameplay"]["hits"]["hitErrorArray"],
        json!([-3.5, 6.25])
    );
}

#[test]
fn play_and_result_select_the_source_mods_and_precise_values() {
    let mut play = regular();
    play.global.status = i64::from(GameState::Play.value());
    play.global.menu_mods.name = "HD".to_owned();
    play.gameplay.mods.name = "DT".to_owned();
    play.gameplay.mods.number = 64;
    play.result_screen.raw.mods.name = "HR".to_owned();
    play.gameplay.statistics.great = 7;
    play.beatmap.pp_acc.insert(95, 123.456);
    play.global.is_replay_ui_hidden = true;
    let mut precise = precise();
    precise.key_overlay = Arc::new(vec![StateKey {
        name: "K1".into(),
        is_pressed: true,
        count: 3,
    }]);
    precise.keypresses = Arc::new(vec![StateKeypress {
        name: "K1".into(),
        is_pressed: true,
        count: 3,
        pressed_at: Some(123.0),
        released_at: None,
    }]);
    precise.hit_errors = Arc::new(HitErrorArray {
        all: vec![1.5, -2.25],
        press: vec![1.5],
        release: vec![-2.25],
    });
    precise.unstable_rate = 12.5;
    precise.status = i64::from(GameState::Play.value());
    precise.play_time = 123.0;
    let play_snapshot = snapshot_with_precise(10, ClientType::Stable, play, precise);
    let play = serde_json::to_value(build(&play_snapshot).unwrap()).unwrap();
    assert_eq!(play["menu"]["mods"], json!({"num": 64, "str": "DT"}));
    assert_eq!(play["gameplay"]["hits"]["300"], 7);
    assert_eq!(
        play["gameplay"]["hits"]["hitErrorArray"],
        json!([1.5, -2.25])
    );
    assert_eq!(play["gameplay"]["hits"]["pressHitErrors"], json!([1.5]));
    assert_eq!(play["gameplay"]["hits"]["releaseHitErrors"], json!([-2.25]));
    assert_eq!(
        play["gameplay"]["keyOverlay"]["k1"],
        json!({"isPressed": true, "count": 3})
    );
    assert_eq!(
        play["gameplay"]["keyOverlay"]["k2"],
        json!({"isPressed": false, "count": 0})
    );
    let encoded = String::from_utf8(encode(&play_snapshot).unwrap().to_vec()).unwrap();
    assert!(encoded.contains(
            "\"pp\":{\"90\":0,\"91\":0,\"92\":0,\"93\":0,\"94\":0,\"95\":123.46,\"96\":0,\"97\":0,\"98\":0,\"99\":0,\"100\":0"
        ));
    assert!(encoded.contains(
        r#""hitErrorArray":[1.5,-2.25],"pressHitErrors":[1.5],"releaseHitErrors":[-2.25]"#
    ));
    assert!(encoded.contains(
            "\"hits\":{\"0\":0,\"50\":0,\"100\":0,\"300\":7,\"geki\":0,\"katu\":0,\"sliderEndHits\":0,\"smallTickHits\":0,\"largeTickHits\":0"
        ));
    assert_eq!(play["menu"]["state"], GameState::Play.value());
    assert_eq!(play["menu"]["bm"]["time"]["current"], 123.0);
    assert!(encoded.contains("\"current\":123"));
    assert_eq!(play["gameplay"]["hits"]["unstableRate"], 0.0);
    assert_eq!(play["gameplay"]["_isReplayUiHidden"], json!(true));

    let mut result = regular();
    result.global.status = i64::from(GameState::ResultScreen.value());
    result.global.menu_mods.name = "HD".to_owned();
    result.gameplay.mods.name = "DT".to_owned();
    result.result_screen.raw.mods.name = "HR".to_owned();
    result.result_screen.raw.mods.number = 16;
    let result =
        serde_json::to_value(build(&snapshot(11, ClientType::Stable, result, Vec::new())).unwrap())
            .unwrap();
    assert_eq!(result["menu"]["mods"], json!({"num": 16, "str": "HR"}));
    let encoded = serde_json::to_string(
        &build(&snapshot(12, ClientType::Stable, regular(), Vec::new())).unwrap(),
    )
    .unwrap();
    assert!(encoded.contains("\"resultsScreen\":{\"0\":0,\"50\":0,\"100\":0,\"300\":0,\"mode\":0"));
}

#[test]
fn user_background_is_the_only_optional_profile_member_after_a_read() {
    let profile = UserProfile {
        raw_login_status: Some(1),
        name: Some("user".to_owned()),
        accuracy: Some(f64::NAN),
        ranked_score: Some(2.0),
        id: Some(3),
        level: Some(4.0),
        play_count: Some(5),
        play_mode: Some(0),
        rank: Some(6),
        country_code: Some(225),
        performance_points: Some(7.0),
        raw_bancho_status: Some(8),
        background_colour: None,
    };
    let value = serde_json::to_value(profile).unwrap();
    assert_eq!(value["rawLoginStatus"], 1);
    assert!(value.get("backgroundColour").is_none());
    assert_eq!(value["accuracy"], Value::Null);
}

#[test]
fn stable_spectators_sort_fractional_ipc_ids_and_chat_maps_teams() {
    let mut manager_state = regular();
    manager_state.tourney.scoreboard.ipc_state = 11;
    manager_state.tourney.messages.push(TourneyManagerChatItem {
        time: "12:00".to_owned(),
        name: "second".to_owned(),
        content: "hello".to_owned(),
    });
    let manager = ManagerInstanceSnapshot::new(
        20,
        settings(None),
        true,
        bundle(ClientType::Stable, manager_state),
    );
    let spectator = |pid, ipc_id, name: &str| {
        let mut state = regular();
        state.tourney.user = Some(TourneyUserState {
            accuracy: 98.0,
            ranked_score: 1.0,
            play_count: 2,
            pp: 3.0,
            global_rank: 4,
            name: name.to_owned(),
            country: "US".to_owned(),
            id: i64::from(pid),
        });
        state.global.menu_mods.name = "HD".to_owned();
        state.gameplay.mods.name = "DT".to_owned();
        state.gameplay.mods.number = 64;
        state.gameplay.unstable_rate = 4.5;
        let mut latest = precise();
        latest.status = i64::from(GameState::Play.value());
        latest.unstable_rate = 99.0;
        ManagerInstanceSnapshot::new(
            pid,
            settings(Some(ipc_id)),
            false,
            bundle_with_precise(ClientType::Stable, state, latest),
        )
    };
    let value = serde_json::to_value(
        build(&snapshot(
            10,
            ClientType::Stable,
            regular(),
            vec![
                spectator(30, 2.75, "second"),
                manager,
                spectator(40, 1.25, "first"),
            ],
        ))
        .unwrap(),
    )
    .unwrap();
    assert_eq!(
        value["tourney"]["ipcClients"][0]["spectating"]["name"],
        "first"
    );
    assert_eq!(
        value["tourney"]["ipcClients"][1]["spectating"]["name"],
        "second"
    );
    assert_eq!(value["tourney"]["ipcClients"][0]["team"], "left");
    assert_eq!(value["tourney"]["ipcClients"][1]["team"], "right");
    assert_eq!(value["tourney"]["manager"]["chat"][0]["team"], "right");
    assert_eq!(
        value["tourney"]["ipcClients"][0]["spectating"]["globalRank"],
        4
    );
    assert_eq!(
        value["tourney"]["ipcClients"][0]["gameplay"]["mods"],
        json!({"num": 64, "str": "DT"})
    );
    assert_eq!(
        value["tourney"]["ipcClients"][0]["gameplay"]["hits"]["unstableRate"],
        4.5
    );
    assert_eq!(
        value["tourney"]["ipcClients"][0]["gameplay"]["hits"]["hitErrorArray"],
        json!([])
    );
    assert_eq!(
        value["tourney"]["ipcClients"][0]["gameplay"]["hits"]["pressHitErrors"],
        json!([])
    );
    assert_eq!(
        value["tourney"]["ipcClients"][0]["gameplay"]["hits"]["releaseHitErrors"],
        json!([])
    );
    let finite = spectator(50, 3.5, "finite");
    let infinite = spectator(60, f64::INFINITY, "infinite");
    let nan = spectator(70, f64::NAN, "nan");
    assert_eq!(compare_ipc_id(&infinite, &finite), Ordering::Greater);
    assert_eq!(compare_ipc_id(&nan, &finite), Ordering::Equal);
}

#[test]
fn lazer_lobby_preserves_client_order_and_source_placeholders() {
    const SOURCE_PLUS_ONE: i64 = 9_007_199_254_740_993;
    const SOURCE_PLUS_THREE: i64 = 9_007_199_254_740_995;
    const ROUNDED_PLUS_ONE: f64 = SOURCE_PLUS_ONE as f64;
    const ROUNDED_PLUS_THREE: f64 = SOURCE_PLUS_THREE as f64;
    let score = |name: &str, points| Score {
        failed: false,
        retries: 0,
        player_name: name.to_owned(),
        mods: default_calculated_mods(),
        mode: 0,
        score: points,
        player_hp_smooth: 100.0,
        player_hp: 100.0,
        accuracy: 100.0,
        statistics: statistics(),
        maximum_statistics: statistics(),
        combo: 1,
        max_combo: 2,
        pp: None,
    };
    let user = |id, name: &str| User {
        name: name.to_owned(),
        accuracy: 100.0,
        ranked_score: ROUNDED_PLUS_ONE,
        id,
        level: 1.0,
        play_count: 0,
        play_mode: 0,
        rank: 0,
        country_code: 225,
        performance_points: 0.0,
        raw_bancho_status: 0,
        background_colour: 0,
        raw_login_status: 0,
        matchmaking: None,
    };
    let mut state = regular();
    state.global.status = i64::from(GameState::Lobby.value());
    state.lazer_multi_spectating.data = Some(LazerSpectator {
        chat: Vec::new(),
        spectating_clients: vec![
            LazerSpectatorEntry {
                team: MultiplayerTeamType::Blue,
                user: user(2, "blue"),
                result_screen: None,
                score: Some(score("blue", 20.0)),
            },
            LazerSpectatorEntry {
                team: MultiplayerTeamType::Red,
                user: user(1, "red"),
                result_screen: None,
                score: Some(score("red", ROUNDED_PLUS_ONE)),
            },
            LazerSpectatorEntry {
                team: MultiplayerTeamType::Red,
                user: user(3, "red-two"),
                result_screen: None,
                score: Some(score("red-two", ROUNDED_PLUS_THREE)),
            },
        ],
    });
    let snapshot = snapshot(10, ClientType::Lazer, state, Vec::new());
    let response = build(&snapshot).unwrap();
    assert_eq!(
        response
            .tourney
            .as_ref()
            .unwrap()
            .manager
            .gameplay
            .score
            .left,
        ROUNDED_PLUS_ONE + ROUNDED_PLUS_THREE
    );
    assert_eq!(
        response.tourney.as_ref().unwrap().ipc_clients[0]
            .spectating
            .ranked_score,
        ROUNDED_PLUS_ONE
    );
    assert_eq!(
        response.tourney.as_ref().unwrap().ipc_clients[1]
            .gameplay
            .score,
        ROUNDED_PLUS_ONE
    );
    assert_eq!(
        response.tourney.as_ref().unwrap().ipc_clients[2]
            .gameplay
            .score,
        ROUNDED_PLUS_THREE
    );
    let value = serde_json::to_value(response).unwrap();
    assert_eq!(
        value["tourney"]["ipcClients"][0]["spectating"]["name"],
        "blue"
    );
    assert_eq!(
        value["tourney"]["ipcClients"][1]["spectating"]["name"],
        "red"
    );
    assert_eq!(
        value["tourney"]["manager"]["gameplay"]["score"],
        json!({"left": ROUNDED_PLUS_ONE + ROUNDED_PLUS_THREE, "right": 20.0})
    );
    assert_eq!(
        value["tourney"]["ipcClients"][0]["gameplay"]["hits"]["sliderBreaks"],
        0
    );
    assert_eq!(
        value["tourney"]["ipcClients"][0]["gameplay"]["hits"]["hitErrorArray"],
        json!([])
    );
    assert_eq!(
        value["tourney"]["ipcClients"][0]["gameplay"]["hits"]["pressHitErrors"],
        json!([])
    );
    assert_eq!(
        value["tourney"]["ipcClients"][0]["gameplay"]["hits"]["releaseHitErrors"],
        json!([])
    );
    assert!(
        value["tourney"]["ipcClients"][0]["gameplay"]["hits"]
            .get("maxThisPlay")
            .is_none()
    );
    assert_eq!(
        value["tourney"]["ipcClients"][0]["gameplay"]["hits"]["grade"]["maxThisPlay"],
        ""
    );
    let encoded = String::from_utf8(encode(&snapshot).unwrap().to_vec()).unwrap();
    assert!(encoded.contains(r#""hitErrorArray":[],"pressHitErrors":[],"releaseHitErrors":[]"#));
    assert!(!encoded.contains("9007199254740993"));
    assert!(!encoded.contains("9007199254740995"));
}

#[test]
fn read_long_values_round_as_javascript_numbers_before_encoding() {
    const SOURCE_PLUS_ONE: i64 = 9_007_199_254_740_993;
    const SOURCE_PLUS_THREE: i64 = 9_007_199_254_740_995;
    const ROUNDED_PLUS_ONE: f64 = SOURCE_PLUS_ONE as f64;
    const ROUNDED_PLUS_THREE: f64 = SOURCE_PLUS_THREE as f64;

    let mut state = regular();
    state.gameplay.score = ROUNDED_PLUS_ONE;
    state.gameplay.leaderboard_player.score = ROUNDED_PLUS_THREE;
    state.result_screen.raw.score = ROUNDED_PLUS_THREE;
    state.user.profile = Some(User {
        name: "player".to_owned(),
        accuracy: 100.0,
        ranked_score: ROUNDED_PLUS_ONE,
        id: 1,
        level: 1.0,
        play_count: 1,
        play_mode: 0,
        rank: 1,
        country_code: 225,
        performance_points: 1.0,
        raw_bancho_status: 0,
        background_colour: 0,
        raw_login_status: 1,
        matchmaking: None,
    });
    let snapshot = snapshot(10, ClientType::Lazer, state, Vec::new());
    let response = build(&snapshot).unwrap();
    assert_eq!(response.gameplay.score, ROUNDED_PLUS_ONE);
    assert_eq!(
        response.gameplay.leaderboard.ourplayer.score,
        ROUNDED_PLUS_THREE
    );
    assert_eq!(response.results_screen.score, ROUNDED_PLUS_THREE);
    assert_eq!(response.user_profile.ranked_score, Some(ROUNDED_PLUS_ONE));

    let encoded = String::from_utf8(encode(&snapshot).unwrap().to_vec()).unwrap();
    assert!(encoded.contains("\"score\":9007199254740992"));
    assert!(encoded.contains("\"rankedScore\":9007199254740992"));
    assert!(!encoded.contains("9007199254740993"));
    assert!(encoded.contains("\"score\":9007199254740996"));
    assert!(!encoded.contains("9007199254740995"));
}

#[test]
fn full_time_preserves_javascript_number_boundaries() {
    let encode_time = |full| {
        let value = BeatmapTimings {
            first_obj: 0,
            current: 0.0,
            full,
            mp3: None,
        };
        let mut bytes = Vec::new();
        let mut serializer = Serializer::with_formatter(&mut bytes, JsFormatter);
        value.serialize(&mut serializer).unwrap();
        String::from_utf8(bytes).unwrap()
    };
    assert_eq!(
        encode_time(12.5),
        r#"{"firstObj":0,"current":0,"full":12.5}"#
    );
    assert_eq!(
        encode_time(1e21),
        r#"{"firstObj":0,"current":0,"full":1e+21}"#
    );
    assert_eq!(
        encode_time(f64::INFINITY),
        r#"{"firstObj":0,"current":0,"full":null}"#
    );
}

#[test]
fn full_path_and_finite_numbers_use_javascript_wire_semantics() {
    let separator = std::path::MAIN_SEPARATOR;
    assert_eq!(joined_path("Set", "."), "Set");
    assert_eq!(
        joined_path("Set/child", "../bg.jpg"),
        format!("Set{separator}bg.jpg")
    );
    assert_eq!(
        joined_path("/Songs/Set", "/bg.jpg"),
        format!("{separator}Songs{separator}Set{separator}bg.jpg")
    );
    assert_eq!(
        joined_path(" Set ", " bg.jpg "),
        format!(" Set {separator} bg.jpg ")
    );
    #[cfg(not(windows))]
    assert_eq!(
        joined_path(r"Set\literal", r"bg\literal.jpg"),
        r"Set\literal/bg\literal.jpg"
    );
    assert_eq!(
        join_node_path(r"\\server\share\Set", "bg.jpg", '\\'),
        r"\\server\share\Set\bg.jpg"
    );
    assert_eq!(
        join_node_path(r"\\server\share\Set", r"..\..\bg.jpg", '\\'),
        r"\\server\share\bg.jpg"
    );
    assert_eq!(
        join_node_path(r"\\server\share", "", '\\'),
        "\\\\server\\share\\"
    );
    assert_eq!(
        join_node_path(r"C:\Songs\Set", r"..\..\..\bg.jpg", '\\'),
        r"C:\bg.jpg"
    );

    let mut state = regular();
    state.bass_density.density = -0.0;
    state.gameplay.accuracy = 1e21;
    state.gameplay.player_hp = 1e-7;
    state.global.play_time = f64::NAN;
    state.menu.mp3_length = Some(f64::INFINITY);
    let snapshot = snapshot(10, ClientType::Stable, state, Vec::new());
    let encoded = String::from_utf8(encode(&snapshot).unwrap().to_vec()).unwrap();
    assert!(encoded.contains("\"bassDensity\":0"));
    assert!(encoded.contains("\"accuracy\":1e+21"));
    assert!(encoded.contains("\"hp\":{\"normal\":1e-7"));
    assert!(encoded.contains("\"time\":{\"firstObj\":0,\"current\":null,\"full\":0,\"mp3\":null}"));
}
