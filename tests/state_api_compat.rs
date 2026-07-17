use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::PathBuf,
    sync::Arc,
};

use serde::Deserialize;
use serde_json::Value;
use tokio::sync::watch;
use vosu::calculation::beatmap::{
    BeatmapCache, BeatmapEngine, BeatmapInput, EngineOutput, EngineRequest, ModSet, Ruleset,
    ScoringMode,
};
use vosu::server::v2_subscription::{
    V2SubscriptionHub, V2SubscriptionOutbound, V2SubscriptionProvider, V2SubscriptionSnapshot,
};
use vosu::{
    api::{
        self, ApiError, ApiRuntimeSettings, CombinedSnapshot, ManagerInstanceSnapshot,
        ManagerSnapshot,
    },
    instance::InstanceSettings,
    model::{
        enums::ClientType,
        memory::{LazerSpectator, ResultScreen as RawResultScreen, User as RawUser},
        states::{
            CalculateMods, KeyOverlayButton, LeaderboardPlayer, Statistics, TourneyManagerChatItem,
        },
    },
    state::{
        BassDensityState, BeatmapPPState, GameplayState, GlobalState, LazerMultiSpectatingState,
        MenuState, PreciseState, RankedPlayState, RegularState, ResultScreenState, SettingsState,
        StateBundleSnapshot, TourneyManagerState, TourneyUserState, UserState,
    },
};

const EXPECTED_REVISIONS: Revisions = Revisions {
    upstream_source: "1a67fd64cc38fe60306328dde0a429e3211ca4ed",
    lazer_calculator: "9a22c4c74f8e584fa64133a79a204dd045646bac",
    osu: "97e94babde9d5ecbe574ecdd33509db27452cd0f",
    calculator_package: "0.5.0-20260702-main.0",
    calculator_manifest_sha256: "2cc4c2390a89968b952ff0a6d2dcff4309bb530c4fd6f46920c13b1b86a21852",
};
const REQUIRED_COVERAGE: &[&str] = &[
    "no-client",
    "defaults",
    "menu",
    "play",
    "result",
    "lobby",
    "stable",
    "lazer",
    "osu",
    "taiko",
    "catch",
    "mania",
    "HD",
    "FL",
    "DT",
    "HT",
    "CL",
    "DA",
    "structured-speed",
    "leaderboard-empty",
    "leaderboard-populated",
    "stable-tournament",
    "lazer-tournament",
    "precise-short-keys",
    "multi-IPC",
    "SC-equal-BPM",
    "SC-range-BPM",
    "known-omissions",
    "placeholders",
];

#[derive(Clone, Copy)]
struct Revisions {
    upstream_source: &'static str,
    lazer_calculator: &'static str,
    osu: &'static str,
    calculator_package: &'static str,
    calculator_manifest_sha256: &'static str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RevisionDocument {
    // The committed source-oracle inventory uses the upstream tosu revision key as parity evidence.
    #[serde(rename = "tosu")]
    upstream_source: String,
    lazer_calculator: String,
    osu: String,
    calculator_package: String,
    calculator_manifest_sha256: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Inventory {
    format: u64,
    revisions: RevisionDocument,
    required_coverage: Vec<String>,
    cases: Vec<Case>,
}

#[derive(Deserialize)]
struct Case {
    id: String,
    tags: Vec<String>,
    input: String,
    expected: BTreeMap<String, String>,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct Input {
    id: String,
    source: Option<String>,
    status: i64,
    mode: i64,
    tournament: Option<String>,
    services: Services,
}

#[derive(Deserialize)]
struct Services {
    #[serde(rename = "bassDensity")]
    bass_density: Value,
    global: Value,
    menu: Value,
    gameplay: Value,
    #[serde(rename = "resultScreen")]
    result_screen: Value,
    #[serde(rename = "beatmapPP")]
    beatmap_pp: Value,
    settings: Value,
    user: Value,
    #[serde(rename = "lazerMultiSpectating")]
    lazer_multi_spectating: Value,
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/oracle")
}

fn string(value: &Value, key: &str) -> String {
    value
        .get(key)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

fn integer(value: &Value, key: &str) -> i64 {
    value.get(key).and_then(Value::as_i64).unwrap_or_default()
}

fn number(value: &Value, key: &str) -> f64 {
    value.get(key).and_then(Value::as_f64).unwrap_or_default()
}

fn boolean(value: &Value, key: &str) -> bool {
    value.get(key).and_then(Value::as_bool).unwrap_or_default()
}

fn optional_string(value: &Value, key: &str) -> Option<String> {
    value.get(key).and_then(Value::as_str).map(str::to_owned)
}

fn mods(value: &Value) -> CalculateMods {
    serde_json::from_value(value.clone()).expect("fixture CalculateMods must be valid")
}

fn statistics(value: &Value) -> Statistics {
    serde_json::from_value(value.clone()).expect("fixture Statistics must be valid")
}

#[derive(Clone, Copy)]
struct ZeroEngine;

impl BeatmapEngine for ZeroEngine {
    fn calculate(
        &self,
        _request: EngineRequest<'_>,
    ) -> Result<EngineOutput, vosu::calculation::beatmap::BeatmapError> {
        Ok(EngineOutput::default())
    }
}

fn parsed_beatmap(input: &Input) -> Option<Arc<vosu::calculation::beatmap::ParsedBeatmap>> {
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

fn regular(input: &Input) -> RegularState {
    let services = &input.services;
    let global = &services.global;
    let menu = &services.menu;
    let gameplay = &services.gameplay;
    let result = &services.result_screen;
    let beatmap = &services.beatmap_pp;

    let mut settings_values = SettingsState::default().values;
    if let Some(object) = services.settings.as_object() {
        for (key, value) in object {
            settings_values.insert(key.clone(), value.clone());
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
            event_status: vosu::calculation::beatmap::EventStatus {
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
            is_default_state: false,
            is_key_overlay_default_state: key_overlay.is_empty(),
            failed: boolean(gameplay, "failed"),
            retries: integer(gameplay, "retries"),
            player_name: string(gameplay, "playerName"),
            mode: input.mode,
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
            hit_errors: serde_json::from_value(
                gameplay
                    .get("hitErrors")
                    .cloned()
                    .unwrap_or_else(|| Value::Array(Vec::new())),
            )
            .expect("hit errors"),
            total_hit_errors: 0.0,
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
    }
}

fn state(input: &Input) -> Arc<StateBundleSnapshot> {
    let regular = regular(input);
    let key_overlay = Arc::new(regular.gameplay.key_overlay.clone());
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
            status: input.status,
            play_time: 0.0,
            key_overlay,
            hit_errors,
            total_hit_errors: 0.0,
            committed_hit_error_index: 0,
            unstable_rate: 0.0,
        }),
    })
}

fn manager(input: &Input) -> CombinedSnapshot {
    let Some(source) = input.source.as_deref() else {
        return CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::not_ready(
            1,
            ApiRuntimeSettings::default(),
        )));
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
            scoreboard: vosu::model::memory::Tourney {
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
        ApiRuntimeSettings::default(),
        instances,
    )))
}

fn encode(schema: &str, snapshot: &CombinedSnapshot) -> Vec<u8> {
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

#[test]
fn committed_source_oracles_match_all_rust_wire_builders_byte_for_byte() {
    let root = fixture_root();
    let inventory: Inventory = serde_json::from_slice(
        &fs::read(root.join("inventory.json")).expect("inventory must exist"),
    )
    .expect("inventory must be valid");
    assert_eq!(inventory.format, 1);
    assert_eq!(
        inventory.revisions.upstream_source,
        EXPECTED_REVISIONS.upstream_source
    );
    assert_eq!(
        inventory.revisions.lazer_calculator,
        EXPECTED_REVISIONS.lazer_calculator
    );
    assert_eq!(inventory.revisions.osu, EXPECTED_REVISIONS.osu);
    assert_eq!(
        inventory.revisions.calculator_package,
        EXPECTED_REVISIONS.calculator_package
    );
    assert_eq!(
        inventory.revisions.calculator_manifest_sha256,
        EXPECTED_REVISIONS.calculator_manifest_sha256
    );

    let declared: BTreeSet<_> = inventory
        .required_coverage
        .iter()
        .map(String::as_str)
        .collect();
    let required: BTreeSet<_> = REQUIRED_COVERAGE.iter().copied().collect();
    assert_eq!(
        declared, required,
        "fixture inventory coverage contract changed"
    );

    let covered: BTreeSet<_> = inventory
        .cases
        .iter()
        .flat_map(|case| case.tags.iter().cloned())
        .collect();
    for requirement in &inventory.required_coverage {
        assert!(
            covered.contains(requirement),
            "missing required fixture coverage: {requirement}"
        );
    }
    let mut case_ids = BTreeSet::new();
    for case in &inventory.cases {
        assert!(
            case_ids.insert(case.id.as_str()),
            "duplicate fixture case id: {}",
            case.id
        );
        let schemas: BTreeSet<_> = case.expected.keys().map(String::as_str).collect();
        assert_eq!(
            schemas,
            BTreeSet::from(["sc", "v1", "v2", "v2-precise"]),
            "{} has an incomplete schema inventory",
            case.id
        );
    }

    for case in inventory.cases {
        let input: Input = serde_json::from_slice(
            &fs::read(root.join(&case.input)).expect("input fixture must exist"),
        )
        .expect("input fixture must be valid");
        assert_eq!(input.id, case.id);
        let snapshot = manager(&input);
        for schema in ["v1", "v2", "v2-precise", "sc"] {
            let expected_path = case
                .expected
                .get(schema)
                .unwrap_or_else(|| panic!("{} missing {schema}", case.id));
            let expected = fs::read(root.join(expected_path)).expect("expected fixture must exist");
            #[cfg(not(target_os = "windows"))]
            let expected = if schema == "v2" {
                let mut value: Value =
                    serde_json::from_slice(&expected).expect("expected v2 fixture must be valid");
                if let Some(game) = value.get_mut("game").and_then(Value::as_object_mut) {
                    game.remove("focused");
                }
                serde_json::to_vec(&value).expect("normalized v2 fixture must encode")
            } else {
                expected
            };
            let actual = encode(schema, &snapshot);
            assert_eq!(
                String::from_utf8_lossy(&actual),
                String::from_utf8_lossy(&expected),
                "oracle mismatch for {} / {schema}",
                case.id
            );
        }
    }
}

struct ParitySnapshot {
    snapshot: Arc<CombinedSnapshot>,
}

impl V2SubscriptionSnapshot for ParitySnapshot {
    fn revision(&self) -> u64 {
        self.snapshot.manager().revision()
    }

    fn document(&self) -> Result<Option<Value>, ApiError> {
        match api::v2::encode(&self.snapshot) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(|error| ApiError::serialization(error.to_string())),
            Err(ApiError::NotReady) => Ok(None),
            Err(error) => Err(error),
        }
    }
}

struct ParityProvider {
    snapshot: Arc<CombinedSnapshot>,
    publication: watch::Sender<u64>,
}

impl ParityProvider {
    fn new(snapshot: CombinedSnapshot) -> Arc<Self> {
        let revision = snapshot.manager().revision();
        let (publication, _) = watch::channel(revision);
        Arc::new(Self {
            snapshot: Arc::new(snapshot),
            publication,
        })
    }
}

impl V2SubscriptionProvider for ParityProvider {
    fn subscribe(&self) -> watch::Receiver<u64> {
        self.publication.subscribe()
    }

    fn capture(&self) -> Arc<dyn V2SubscriptionSnapshot> {
        Arc::new(ParitySnapshot {
            snapshot: Arc::clone(&self.snapshot),
        })
    }
}

#[tokio::test]
async fn v2_subscription_emitted_values_match_parsed_v2_wire_values() {
    let root = fixture_root();
    let inventory: Inventory = serde_json::from_slice(
        &fs::read(root.join("inventory.json")).expect("inventory must exist"),
    )
    .expect("inventory must be valid");

    for case in inventory.cases {
        let input: Input = serde_json::from_slice(
            &fs::read(root.join(&case.input)).expect("input fixture must exist"),
        )
        .expect("input fixture must be valid");
        let snapshot = manager(&input);
        let expected = match api::v2::encode(&snapshot) {
            Ok(bytes) => Some(
                serde_json::from_slice::<Value>(&bytes).expect("encoded v2 must be valid JSON"),
            ),
            Err(ApiError::NotReady) => None,
            Err(error) => panic!("v2 encoding failed for {}: {error}", case.id),
        };
        let provider = ParityProvider::new(snapshot);
        let hub = V2SubscriptionHub::new(provider, std::time::Duration::ZERO);
        let runner = tokio::spawn({
            let hub = hub.clone();
            async move { hub.run().await }
        });
        let (connection, mut receiver) = hub.connect();
        connection.subscribe(vec![String::new()]).unwrap();
        let frame = match receiver.recv().await.unwrap() {
            V2SubscriptionOutbound::Text(bytes) => {
                serde_json::from_slice::<Value>(&bytes).expect("subscription frame must be valid")
            }
            frame => panic!("unexpected subscription frame for {}: {frame:?}", case.id),
        };
        match expected {
            Some(expected) => assert_eq!(
                frame.pointer("/2/0/1"),
                Some(&expected),
                "subscription emitted-value parity failed for {}",
                case.id
            ),
            None => assert_eq!(frame, serde_json::json!([0, 1, []])),
        }
        hub.shutdown();
        runner.await.unwrap();
    }
}
