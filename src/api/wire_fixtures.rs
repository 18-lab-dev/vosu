//! Populated state fixtures shared by the direct-writer equivalence tests.
//!
//! The direct encoders must reproduce their DTO serialization byte for byte,
//! so these states fill every field with awkward strings and numbers whose
//! JavaScript spelling differs from naive formatting.

use std::sync::Arc;

use serde_json::json;

use crate::api::{ApiRuntimeSettings, CombinedSnapshot, ManagerInstanceSnapshot, ManagerSnapshot};
use crate::calculation::beatmap::{
    BeatmapCache, BeatmapInput, GraphSeries, ModSet, ParsedBeatmap, Ruleset, ScoringMode,
    StrainGraph,
};
use crate::calculation::progressive::{PerformanceAttributes, ProgressiveAttributes};
use crate::calculation::state_backend::LiveBeatmapEngine;
use crate::instance::InstanceSettings;
use crate::model::enums::ClientType;
use crate::model::states::{
    CalculateMods, HitErrorArray, KeyOverlayButton as StateKeyOverlayButton, KeypressButton,
    Statistics,
};
use crate::state::{
    PreciseState, RegularState, StateBundle, StateBundleSnapshot, StateCalculationBackend,
    StateCalculationFuture, StateCalculationRequest,
};

struct NeverCalculate;
impl StateCalculationBackend for NeverCalculate {
    fn calculate<'a>(&'a self, _: StateCalculationRequest) -> StateCalculationFuture<'a> {
        Box::pin(std::future::pending())
    }
}

/// Strings that exercise every serde_json escape class and non-ASCII text.
pub(crate) const AWKWARD: &str = "q\"b\\s/\u{1}\u{8}\u{c}\n\r\t\u{1f}\u{7f}é\u{2028}🎵";

/// Numbers whose JavaScript spelling differs from naive formatting.
pub(crate) const NUMBERS: [f64; 12] = [
    -0.0,
    0.1,
    -2.5,
    1e-7,
    123_456.789,
    9_007_199_254_740_991.0,
    9_007_199_254_740_993.0,
    1e21,
    -1.5e300,
    f64::NAN,
    f64::INFINITY,
    f64::NEG_INFINITY,
];

/// A beatmap with metadata, breaks, several timing points and every object kind.
pub(crate) fn parsed_beatmap(source: ClientType) -> Arc<ParsedBeatmap> {
    let content = "osu file format v14\n\
        [General]\nMode:0\nPreviewTime:1500\n\
        [Metadata]\nTitle:T\nArtist:A\nCreator:C\nVersion:V\nSource:src \"q\"\nTags:a b\n\
        [Difficulty]\nHPDrainRate:5.5\nCircleSize:4.2\nOverallDifficulty:8.3\nApproachRate:9.1\n\
        SliderMultiplier:1.4\nSliderTickRate:1\n\
        [Events]\n2,4000,9000\n\
        [TimingPoints]\n0,333.333,4,2,1,80,1,0\n1500,-50,4,2,1,80,0,1\n12000,250,4,2,1,80,1,0\n\
        [HitObjects]\n64,192,1000,1,0,0:0:0:0:\n128,192,1400,1,0,0:0:0:0:\n\
        256,192,1800,2,0,B|384:192,1,140\n448,192,3000,8,0,3500\n\
        64,192,10000,1,0,0:0:0:0:\n128,192,13000,1,0,0:0:0:0:\n";
    BeatmapCache::new(LiveBeatmapEngine)
        .load(
            BeatmapInput::Content {
                content,
                identity_path: None,
            },
            Ruleset::Osu,
            &ModSet::new("", Vec::new(), 1.0).unwrap(),
            match source {
                ClientType::Stable => ScoringMode::Stable,
                ClientType::Lazer => ScoringMode::Lazer,
            },
        )
        .unwrap()
}

pub(crate) fn mods(seed: f64) -> CalculateMods {
    let mut mods: CalculateMods = serde_json::from_value(json!({
        "checksum": format!("mods-{seed}"),
        "number": 72,
        "name": "HDDT",
        "array": [
            {"acronym": "HD"},
            {"acronym": "DT", "settings": {"speed_change": seed, "adjust_pitch": false, "nothing": null}},
            {"acronym": AWKWARD, "settings": {}},
        ],
        "rate": 0,
    }))
    .unwrap();
    // JSON cannot carry non-finite rates, so set the field directly.
    mods.rate = seed;
    mods
}

fn statistics(base: i64) -> Statistics {
    serde_json::from_value(json!({
        "perfect": base, "great": base + 1, "good": base + 2, "ok": base + 3, "meh": base + 4,
        "miss": base + 5, "smallTickMiss": base + 6, "smallTickHit": base + 7,
        "largeTickMiss": base + 8, "largeTickHit": base + 9, "smallBonus": base + 10,
        "largeBonus": base + 11, "ignoreMiss": base + 12, "ignoreHit": base + 13,
        "comboBreak": base + 14, "sliderTailHit": base + 15, "legacyComboIncrease": base + 16,
    }))
    .unwrap()
}

fn performance(seed: f64) -> PerformanceAttributes {
    PerformanceAttributes {
        pp: seed,
        accuracy: seed * 1.001,
        aim: -seed,
        difficulty: seed / 3.0,
        flashlight: seed * 1e-9,
        speed: seed + 0.005,
        reading: seed,
    }
}

pub(crate) fn populated_regular(base: &RegularState, number: f64, text: &str) -> RegularState {
    let mut regular = base.clone();
    regular.beatmap.parsed = Some(parsed_beatmap(ClientType::Stable));
    let global = &mut regular.global;
    global.show_interface = true;
    global.is_replay_ui_hidden = true;
    global.chat_status = 2;
    global.paused = true;
    global.game_time = -123_456;
    global.menu_mods = mods(number);
    global.game_folder = format!("{text}game");
    global.skin_folder = format!("skins{}{text}", std::path::MAIN_SEPARATOR);
    global.songs_folder = text.to_owned();

    let settings = &mut regular.settings.values;
    settings.insert("leaderboardType".into(), json!(1));
    settings.insert("progressBarType".into(), json!(2.75));
    settings.insert("sortType".into(), json!(3));
    settings.insert("groupType".into(), json!(-4));
    settings.insert(
        "resolution".into(),
        json!({"fullscreen": true, "width": 1920.9, "height": "x", "widthFullscreen": 2560}),
    );
    settings.insert(
        "client".into(),
        json!({"updateAvailable": 1, "branch": 2, "version": text}),
    );
    settings.insert("scoreMeter".into(), json!({"type": 2, "size": number}));
    settings.insert(
        "cursor".into(),
        json!({"size": null, "menuSize": "large", "autoSize": true}),
    );
    settings.insert(
        "mouse".into(),
        json!({"sensitivity": number, "rawInput": true}),
    );
    settings.insert(
        "tablet".into(),
        json!({"enabled": true, "x": 1, "y": -0.0, "width": 1e21, "rotation": 90.5}),
    );
    settings.insert(
        "mania".into(),
        json!({"speedBPMScale": true, "scrollSpeed": 27, "scrollDirection": 1}),
    );
    settings.insert("skin".into(), json!({"name": text, "useTaikoSkin": true}));
    settings.insert(
        "audio".into(),
        json!({"volume": {"master": 100, "music": number}, "offset": {"universal": -12.5}}),
    );
    settings.insert("background".into(), json!({"blur": 0.25, "dim": 1}));
    settings.insert(
        "keybinds".into(),
        json!({
            "osu": {"k1": "Z", "k2": text},
            "fruits": {"Dash": "LeftShift"},
            "taiko": {"innerLeft": "X", "outerRight": "V"},
            "quickRetry": "`",
        }),
    );

    let beatmap = &mut regular.beatmap;
    beatmap.mode = Some(0);
    beatmap.first_object_time = Some(1_234);
    beatmap.attributes = Some(ProgressiveAttributes {
        stars: number,
        pp: number * 2.0,
        max_achieved: 1.005,
        max_achievable: 2.675,
        fc_pp: -0.0,
        current: performance(number),
        fc: performance(1.125),
    });
    beatmap.pp_acc.insert(90, number);
    beatmap.pp_acc.insert(95, 1.115);
    beatmap.pp_acc.insert(100, 9_007_199_254_740_993.0);
    beatmap.graph = Arc::new(StrainGraph {
        primary: vec![1.0],
        series: vec![GraphSeries {
            name: text.to_owned(),
            data: vec![number, 0.5],
        }],
        x_axis: vec![0.0, number],
    });
    beatmap.event_status.bpm = 180.123_456_789;
    beatmap.event_status.kiai = true;
    beatmap.event_status.break_time = true;

    let menu = &mut regular.menu;
    menu.gamemode = Some(3);
    menu.artist = Some(text.to_owned());
    menu.artist_original = Some(format!("{text}!"));
    menu.title = Some("title".into());
    menu.title_original = Some(String::new());
    menu.creator = Some("creator".into());
    menu.difficulty = Some(text.to_owned());
    menu.folder = Some(format!("..{}{text}", std::path::MAIN_SEPARATOR));
    menu.filename = Some("map.osu".into());
    menu.background_filename = Some(text.to_owned());
    menu.audio_filename = None;
    menu.map_id = Some(-7);
    menu.set_id = Some(9_007_199_254_740_993);
    menu.ranked_status = Some(number);
    menu.checksum = Some(text.to_owned());
    menu.mp3_length = Some(number);

    regular.bass_density.density = number;

    let gameplay = &mut regular.gameplay;
    gameplay.failed = true;
    gameplay.player_name = text.to_owned();
    gameplay.mode = 3;
    gameplay.mods = mods(number);
    gameplay.score = number;
    gameplay.accuracy = 99.123_456;
    gameplay.player_hp = 199.999;
    gameplay.player_hp_smooth = number;
    gameplay.statistics = statistics(10);
    gameplay.combo = 321;
    gameplay.max_combo = 654;
    gameplay.slider_breaks = 3;
    gameplay.unstable_rate = number;
    gameplay.grade_current = "SH".into();
    gameplay.grade_expected = text.to_owned();
    gameplay.leaderboard_visible = true;
    gameplay.leaderboard_scores = (0..3)
        .map(|index| {
            serde_json::from_value(json!({
                "userId": index, "name": format!("{text}{index}"), "score": 1.5 * index as f64,
                "combo": index, "accuracy": 97.25, "maxCombo": 2 * index,
                "mods": mods(index as f64), "statistics": statistics(index),
                "team": index % 2, "position": index + 1, "isPassing": index != 1,
            }))
            .unwrap()
        })
        .collect();
    let main = &mut gameplay.leaderboard_player;
    main.name = text.to_owned();
    main.score = number;
    main.combo = 12;
    main.max_combo = 34;
    main.mods = mods(number);
    main.statistics = statistics(3);
    main.team = 1;
    main.position = 2;
    main.is_passing = true;

    let result = &mut regular.result_screen;
    result.raw.online_id = 9_007_199_254_740_993.0;
    result.raw.player_name = text.to_owned();
    result.raw.mods = mods(0.5);
    result.raw.mode = 2;
    result.raw.accuracy = number;
    result.raw.max_combo = 77;
    result.raw.score = 1e21;
    result.raw.statistics = statistics(40);
    result.raw.date = "2026-10-13T00:00:00Z".into();
    result.grade = "X".into();
    result.pp = number;
    result.fc_pp = 0.125;

    regular.user.profile = Some(
        serde_json::from_value(json!({
            "name": text, "accuracy": 98.76, "rankedScore": 9_007_199_254_740_993.0_f64,
            "id": 2, "level": 100.5, "playCount": 3, "playMode": 1, "rank": 4,
            "countryCode": 216, "performancePoints": 1234.5, "rawBanchoStatus": 2,
            "backgroundColour": -255, "rawLoginStatus": 65_793,
            "matchmaking": {"rating": 1500.5, "rank": null, "plays": 3, "wins": 1, "isProvisional": true},
        }))
        .unwrap(),
    );
    regular.room.info = Some(
        serde_json::from_value(json!({
            "roomID": 99,
            "users": [
                {"id": 1, "username": text, "countryCode": 5, "avatarUrl": "https://a/1"},
                {"id": 2, "username": "plain", "countryCode": 0},
            ],
        }))
        .unwrap(),
    );
    regular
}

pub(crate) fn populated_precise(base: &PreciseState, number: f64, text: &str) -> PreciseState {
    let mut precise = base.clone();
    precise.status = 2;
    precise.play_time = number;
    precise.key_overlay = Arc::new(
        (0..3)
            .map(|index| StateKeyOverlayButton {
                name: Arc::from(format!("{text}{index}")),
                is_pressed: index == 1,
                count: index * 1000,
            })
            .collect(),
    );
    precise.keypresses = Arc::new(
        (0..3)
            .map(|index| KeypressButton {
                name: Arc::from(if index == 0 {
                    text.to_owned()
                } else {
                    format!("K{index}")
                }),
                is_pressed: index % 2 == 0,
                count: -index,
                pressed_at: (index != 1).then_some(number),
                released_at: (index == 2).then_some(-0.0),
            })
            .collect(),
    );
    precise.hit_errors = Arc::new(HitErrorArray {
        all: NUMBERS.to_vec(),
        press: vec![number],
        release: Vec::new(),
        key: None,
        offsets: Vec::new(),
    });
    precise
}

pub(crate) fn snapshot_with(
    source: ClientType,
    endpoint: Option<&str>,
    runtime: ApiRuntimeSettings,
    edit: impl Fn(&RegularState, &PreciseState) -> (RegularState, PreciseState),
) -> CombinedSnapshot {
    let base = StateBundle::new(source, 1, Arc::new(NeverCalculate)).snapshot();
    let (regular, precise) = edit(&base.regular, &base.precise);
    let state = Arc::new(StateBundleSnapshot {
        source,
        generation: base.generation,
        reset_epoch: base.reset_epoch,
        regular: Arc::new(regular),
        precise: Arc::new(precise),
    });
    let instance = ManagerInstanceSnapshot::new(
        10,
        InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: endpoint.map(Into::into),
        },
        false,
        state,
    );
    CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        1,
        Some(10),
        true,
        runtime,
        [instance],
    )))
}
