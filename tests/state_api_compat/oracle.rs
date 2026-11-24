use std::{collections::BTreeMap, path::PathBuf, sync::Arc};

use serde::Deserialize;
use serde_json::Value;
use vosu::api::{
    self, ApiError, ApiRuntimeSettings, CombinedSnapshot, ManagerInstanceSnapshot, ManagerSnapshot,
};
use vosu_osu_calc::beatmap::{
    BeatmapCache, BeatmapEngine, BeatmapInput, EngineOutput, EngineRequest, ModSet, Ruleset,
    ScoringMode,
};
use vosu_osu_model::launch::InstanceSettings;
use vosu_osu_model::{
    enums::ClientType,
    memory::{LazerSpectator, ResultScreen as RawResultScreen, User as RawUser},
    states::{
        CalculateMods, HitErrorArray, KeyOverlayButton, LeaderboardPlayer, Statistics,
        TourneyManagerChatItem,
    },
};
use vosu_osu_state::{
    BassDensityState, BeatmapPPState, GameplayState, GlobalState, LazerMultiSpectatingState,
    MenuState, PreciseState, RankedPlayState, RegularState, ResultScreenState, SettingsState,
    StateBundleSnapshot, TourneyManagerState, TourneyUserState, UserState,
};

#[derive(Clone, Copy)]
pub struct Revisions {
    pub upstream_source: &'static str,
    pub lazer_calculator: &'static str,
    pub osu: &'static str,
    pub calculator_package: &'static str,
    pub calculator_tree: &'static str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RevisionDocument {
    // The committed source-oracle inventory uses the upstream tosu revision key as parity evidence.
    #[serde(rename = "tosu")]
    pub upstream_source: String,
    pub lazer_calculator: String,
    pub osu: String,
    pub calculator_package: String,
    pub calculator_tree: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Inventory {
    pub format: u64,
    pub revisions: RevisionDocument,
    pub required_coverage: Vec<String>,
    pub cases: Vec<Case>,
}

#[derive(Deserialize)]
pub struct Case {
    pub id: String,
    pub tags: Vec<String>,
    pub input: String,
    pub expected: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Input {
    pub id: String,
    pub source: Option<String>,
    pub status: i64,
    pub mode: i64,
    pub tournament: Option<String>,
    pub services: Services,
}

#[derive(Deserialize)]
pub struct Services {
    #[serde(rename = "bassDensity")]
    pub bass_density: Value,
    pub global: Value,
    pub menu: Value,
    pub gameplay: Value,
    #[serde(rename = "resultScreen")]
    pub result_screen: Value,
    #[serde(rename = "beatmapPP")]
    pub beatmap_pp: Value,
    pub settings: Value,
    pub user: Value,
    #[serde(rename = "lazerMultiSpectating")]
    pub lazer_multi_spectating: Value,
}

pub fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle")
}

pub fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

pub fn integer(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or_default()
}

pub fn number(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or_default()
}

pub fn boolean(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or_default()
}

pub fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

pub fn mods(value: &Value) -> CalculateMods {
    serde_json::from_value(value.clone()).expect("fixture CalculateMods must be valid")
}

pub fn statistics(value: &Value) -> Statistics {
    serde_json::from_value(value.clone()).expect("fixture Statistics must be valid")
}

pub fn hit_errors(value: &Value) -> HitErrorArray {
    let press: Vec<f64> = serde_json::from_value(
        value
            .get("hitErrors")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .expect("hit errors");

    HitErrorArray {
        all: press.clone(),
        press,
        release: Vec::new(),
        key: None,
        offsets: Vec::new(),
    }
}

pub fn strip_vosu_extensions(value: &mut Value) {
    match value {
        Value::Object(object) => {
            object.shift_remove("pressHitErrors");
            object.shift_remove("keyHitErrors");
            object.shift_remove("releaseHitErrors");
            object.shift_remove("catcher");
            // Additive renderer-state fields are vosu-owned, not source-oracle
            // material. Keep the pinned upstream fixtures unchanged.
            if object.contains_key("playerName") && object.contains_key("mods") {
                object.shift_remove("source");
            }
            if object.contains_key("firstObject") && object.contains_key("live") {
                object.shift_remove("rate");
                object.shift_remove("running");
            }
            if object.contains_key("interfaceVisible") {
                object.shift_remove("visual");
            }
            if object.contains_key("scrollSpeed") {
                for field in [
                    "scrollSpeedValue",
                    "scrollTime",
                    "timingBasedNoteColouring",
                    "perBeatmapScrollSpeed",
                ] {
                    object.shift_remove(field);
                }
            }
            for value in object.values_mut() {
                strip_vosu_extensions(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                strip_vosu_extensions(value);
            }
        }
        _ => {}
    }
}

#[derive(Clone, Copy)]
struct ZeroEngine;

impl BeatmapEngine for ZeroEngine {
    fn calculate(
        &self,
        _request: EngineRequest<'_>,
    ) -> Result<EngineOutput, vosu_osu_calc::beatmap::BeatmapError> {
        Ok(EngineOutput::default())
    }
}

pub fn parsed_beatmap(input: &Input) -> Option<Arc<vosu_osu_calc::beatmap::ParsedBeatmap>> {
    let bpm = input.services.beatmap_pp.get("commonBPM")?.as_f64()?;
    if bpm == 0.0 {
        return None;
    }
    let min = input.services.beatmap_pp.get("minBPM")?.as_f64()?;
    let max = input.services.beatmap_pp.get("maxBPM")?.as_f64()?;
    let timing = if min == max {
        format!("0,{},4,2,0,100,1,0", 60_000.0 / bpm)
    } else {
        format!(
            "0,{},4,2,0,100,1,0\n1000,{},4,2,0,100,1,0\n2000,{},4,2,0,100,1,0",
            60_000.0 / min,
            60_000.0 / max,
            60_000.0 / bpm,
        )
    };
    let content = format!(
        "osu file format v14\n\n[General]\nMode:0\n\n[Metadata]\nTitle:\nArtist:\nCreator:\nVersion:\n\n[Difficulty]\nHPDrainRate:0\nCircleSize:0\nOverallDifficulty:0\nApproachRate:0\nSliderMultiplier:1\nSliderTickRate:1\n\n[TimingPoints]\n{timing}\n\n[HitObjects]\n256,192,10000,1,0,0:0:0:0:"
    );
    let mut cache = BeatmapCache::new(ZeroEngine);
    cache
        .load(
            BeatmapInput::Content {
                content: &content,
                identity_path: None,
            },
            Ruleset::Osu,
            &ModSet::new("", Vec::new(), 1.0).expect("valid fixture mods"),
            ScoringMode::Stable,
        )
        .ok()
}

pub fn regular(input: &Input) -> RegularState {
    let services = &input.services;
    let global = &services.global;
    let menu = &services.menu;
    let gameplay = &services.gameplay;
    let result = &services.result_screen;
    let beatmap = &services.beatmap_pp;

    let mut settings_values = SettingsState::default().values;
    if let Some(object) = services.settings.as_object() {
        for (key, value) in object {
            Arc::make_mut(&mut settings_values).insert(key.clone(), value.clone());
        }
    }

    let menu_mods = mods(global.get("menuMods").expect("menuMods"));
    let gameplay_mods = mods(gameplay.get("mods").expect("gameplay mods"));
    let result_mods = mods(result.get("mods").expect("result mods"));
    let gameplay_statistics = statistics(gameplay.get("statistics").expect("gameplay statistics"));
    let result_statistics = statistics(result.get("statistics").expect("result statistics"));
    let maximum_statistics =
        statistics(result.get("maximumStatistics").expect("maximum statistics"));
    let leaderboard_scores: Vec<LeaderboardPlayer> = serde_json::from_value(
        gameplay
            .get("leaderboardScores")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .expect("leaderboard fixture must be valid");
    let key_overlay: Vec<KeyOverlayButton> = serde_json::from_value(
        gameplay
            .get("keyOverlay")
            .cloned()
            .unwrap_or_else(|| Value::Array(Vec::new())),
    )
    .expect("key overlay fixture must be valid");

    let mut pp_acc = BTreeMap::new();
    for accuracy in 90_u8..=100 {
        pp_acc.insert(
            accuracy,
            beatmap
                .get("ppAcc")
                .and_then(|value| value.get(accuracy.to_string()))
                .and_then(Value::as_f64)
                .unwrap_or_default(),
        );
    }
    let parsed = parsed_beatmap(input);
    let realtime_bpm = number(beatmap, "realtimeBPM");

    let mut menu_state = MenuState::default();
    menu_state.gamemode = Some(input.mode);
    menu_state.plays = Some(integer(menu, "plays"));
    menu_state.artist = optional_string(menu, "artist");
    menu_state.artist_original = optional_string(menu, "artistOriginal");
    menu_state.title = optional_string(menu, "title");
    menu_state.title_original = optional_string(menu, "titleOriginal");
    menu_state.ar = Some(number(menu, "ar"));
    menu_state.cs = Some(number(menu, "cs"));
    menu_state.hp = Some(number(menu, "hp"));
    menu_state.od = Some(number(menu, "od"));
    menu_state.audio_filename = optional_string(menu, "audioFilename");
    menu_state.background_filename = optional_string(menu, "backgroundFilename");
    menu_state.folder = optional_string(menu, "folder");
    menu_state.creator = optional_string(menu, "creator");
    menu_state.filename = optional_string(menu, "filename");
    menu_state.difficulty = optional_string(menu, "difficulty");
    menu_state.map_id = Some(integer(menu, "mapID"));
    menu_state.set_id = Some(integer(menu, "setID"));
    menu_state.ranked_status = Some(number(menu, "rankedStatus"));
    menu_state.checksum = optional_string(menu, "checksum");
    menu_state.object_count = Some(0);
    menu_state.mp3_length = Some(number(menu, "mp3Length"));

    RegularState {
        global: GlobalState {
            is_watching_replay: boolean(global, "isWatchingReplay"),
            is_replay_ui_hidden: boolean(global, "isReplayUiHidden"),
            is_multi_spectating: boolean(global, "isMultiSpectating"),
            show_interface: boolean(global, "showInterface"),
            chat_status: integer(global, "chatStatus"),
            status: input.status,
            paused: boolean(global, "paused"),
            game_time: integer(global, "gameTime"),
            play_time: number(global, "playTime"),
            previous_play_time: 0.0,
            menu_mods,
            game_folder: string(global, "gameFolder"),
            skin_folder: string(global, "skinFolder"),
            current_skin: None,
            render: None,
            songs_folder: string(global, "songsFolder"),
            memory_songs_folder: String::new(),
        },
        settings: SettingsState {
            values: settings_values,
        },
        beatmap: BeatmapPPState {
            checksum: optional_string(menu, "checksum"),
            mode: Some(input.mode),
            mods: gameplay_mods.clone(),
            first_object_time: Some(
                beatmap
                    .get("timings")
                    .map_or(0, |timings| integer(timings, "firstObj")),
            ),
            full_pp: 0.0,
            attributes: None,
            pp_acc,
            source_key: None,
            parsed,
            graph: Arc::default(),
            graph_mp3_length: None,
            event_status: vosu_osu_calc::beatmap::EventStatus {
                bpm: realtime_bpm,
                kiai: false,
                break_time: false,
            },
        },
        menu: menu_state,
        bass_density: BassDensityState {
            current_audio_velocity: 0.0,
            density: number(&services.bass_density, "density"),
        },
        gameplay: GameplayState {
            catcher: None,
            is_default_state: false,
            is_key_overlay_default_state: key_overlay.is_empty(),
            failed: boolean(gameplay, "failed"),
            retries: integer(gameplay, "retries"),
            player_name: string(gameplay, "playerName"),
            mode: input.mode,
            keypress_mode: input.mode,
            mods: gameplay_mods,
            score: number(gameplay, "score"),
            player_hp_smooth: number(gameplay, "playerHPSmooth"),
            player_hp: number(gameplay, "playerHP"),
            accuracy: number(gameplay, "accuracy"),
            statistics: gameplay_statistics.clone(),
            maximum_statistics: gameplay_statistics,
            combo: integer(gameplay, "combo"),
            max_combo: integer(gameplay, "maxCombo"),
            combo_previous: 0,
            hit_miss_previous: 0,
            slider_breaks: integer(gameplay, "hitSB"),
            unstable_rate: number(gameplay, "unstableRate"),
            grade_current: string(gameplay, "gradeCurrent"),
            grade_expected: string(gameplay, "gradeExpected"),
            key_overlay: key_overlay.clone(),
            keypresses: key_overlay
                .iter()
                .map(|key| vosu_osu_model::states::KeypressButton {
                    name: Arc::clone(&key.name),
                    is_pressed: key.is_pressed,
                    count: key.count,
                    pressed_at: None,
                    released_at: None,
                })
                .collect(),
            keypress_identity_mode: Some(input.mode),
            hit_errors: hit_errors(gameplay),
            total_hit_errors: 0.0,
            total_hit_error_squares: 0.0,
            previous_hit_error_index: 0,
            previous_play_time: 0.0,
            leaderboard_visible: boolean(gameplay, "isLeaderboardVisible"),
            leaderboard_player: Default::default(),
            leaderboard_scores,
            calculated: Default::default(),
        },
        result_screen: ResultScreenState {
            raw: RawResultScreen {
                online_id: number(result, "onlineId"),
                player_name: string(result, "playerName"),
                mods: result_mods,
                mode: input.mode,
                accuracy: number(result, "accuracy"),
                max_combo: integer(result, "maxCombo"),
                score: number(result, "score"),
                statistics: result_statistics,
                maximum_statistics,
                date: string(result, "date"),
            },
            grade: string(result, "grade"),
            pp: number(result, "pp"),
            fc_pp: number(result, "fcPP"),
            previous_beatmap: String::new(),
        },
        tourney: TourneyManagerState::default(),
        user: UserState {
            profile: Some(
                serde_json::from_value::<RawUser>(services.user.clone()).expect("user fixture"),
            ),
        },
        lazer_multi_spectating: LazerMultiSpectatingState {
            data: services
                .lazer_multi_spectating
                .get("lazerSpectatingData")
                .cloned()
                .filter(|value| !value.is_null())
                .map(|value| {
                    serde_json::from_value::<LazerSpectator>(value)
                        .expect("lazer tournament fixture")
                }),
        },
        ranked_play: RankedPlayState::default(),
        room: Default::default(),
    }
}

pub fn state(input: &Input) -> Arc<StateBundleSnapshot> {
    let regular = regular(input);
    let mode = regular.gameplay.mode;
    let key_overlay = Arc::new(regular.gameplay.key_overlay.clone());
    let keypresses = Arc::new(regular.gameplay.keypresses.clone());
    let hit_errors = Arc::new(regular.gameplay.hit_errors.clone());
    Arc::new(StateBundleSnapshot {
        source: if input.source.as_deref() == Some("lazer") {
            ClientType::Lazer
        } else {
            ClientType::Stable
        },
        generation: 1,
        reset_epoch: 0,
        regular: Arc::new(regular),
        precise: Arc::new(PreciseState {
            catcher: None,
            mode,
            status: input.status,
            play_time: 0.0,
            key_overlay,
            keypresses,
            hit_errors,
            total_hit_errors: 0.0,
            committed_hit_error_index: 0,
            unstable_rate: 0.0,
        }),
    })
}

pub fn manager(input: &Input) -> CombinedSnapshot {
    manager_with(input, ApiRuntimeSettings::default())
}

pub fn manager_with(input: &Input, settings: ApiRuntimeSettings) -> CombinedSnapshot {
    let Some(source) = input.source.as_deref() else {
        return CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::not_ready(1, settings)));
    };
    let focused = ManagerInstanceSnapshot::new(
        10,
        InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: None,
        },
        false,
        state(input),
    );
    let mut instances = vec![focused];
    if input.tournament.as_deref() == Some("stable") {
        let mut manager_state = state(input).as_ref().clone();
        let mut manager_regular = manager_state.regular.as_ref().clone();
        manager_regular.tourney = TourneyManagerState {
            is_default_state: false,
            scoreboard: vosu_osu_model::memory::Tourney {
                ipc_state: 2,
                left_stars: 1,
                right_stars: 2,
                best_of: 5,
                stars_visible: true,
                score_visible: true,
                first_team_name: "Red".into(),
                second_team_name: "Blue".into(),
                first_team_score: 111,
                second_team_score: 222,
            },
            messages: vec![TourneyManagerChatItem {
                time: "12:00".into(),
                name: "BanchoBot".into(),
                content: "match".into(),
            }],
            user: None,
        };
        manager_state.regular = Arc::new(manager_regular);
        instances.push(ManagerInstanceSnapshot::new(
            20,
            InstanceSettings {
                tournament_spectator: false,
                tournament_ipc_id: None,
                devserver_endpoint: None,
            },
            true,
            Arc::new(manager_state),
        ));
        for (pid, ipc, name, country, id) in [(40, 2.0, "Red", "US", 1), (30, 1.0, "Blue", "JP", 2)]
        {
            let mut client_state = state(input).as_ref().clone();
            let mut client_regular = client_state.regular.as_ref().clone();
            client_regular.tourney.user = Some(TourneyUserState {
                accuracy: 0.0,
                ranked_score: 0.0,
                play_count: 0,
                global_rank: 0,
                pp: 0.0,
                name: name.into(),
                country: country.into(),
                id,
            });
            client_state.regular = Arc::new(client_regular);
            instances.push(ManagerInstanceSnapshot::new(
                pid,
                InstanceSettings {
                    tournament_spectator: true,
                    tournament_ipc_id: Some(ipc),
                    devserver_endpoint: None,
                },
                false,
                Arc::new(client_state),
            ));
        }
    }
    let client = if source == "lazer" {
        ClientType::Lazer
    } else {
        ClientType::Stable
    };
    assert_eq!(instances[0].client(), client);
    CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        1,
        Some(10),
        true,
        settings,
        instances,
    )))
}

pub fn encode(schema: &str, snapshot: &CombinedSnapshot) -> Vec<u8> {
    let result = match schema {
        "v1" => api::v1::encode(snapshot),
        "v2" => api::v2::encode(snapshot),
        "v2-precise" => api::precise::encode(snapshot),
        "sc" => api::sc::encode(snapshot),
        other => panic!("unknown schema {other}"),
    };
    match result {
        Ok(bytes) => bytes.to_vec(),
        Err(ApiError::NotReady) => br#"{"error":"not_ready"}"#.to_vec(),
        Err(error) => panic!("schema encoding failed: {error}"),
    }
}
