use super::*;
use serde_json::{Value, json};
use std::sync::Arc;
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings, states::HitErrorArray};
use vosu_osu_state::*;
use vosu_plugin_api::{PublishBatchWriter, decode_publish_batch_ref};

fn sample() -> StateBundleSnapshot {
    StateBundleSnapshot {
        source: ClientType::Stable,
        generation: 1,
        reset_epoch: 0,
        regular: Arc::new(RegularState {
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
            room: RoomState::default(),
        }),
        precise: Arc::new(PreciseState {
            status: 2,
            play_time: 123.5,
            mode: 0,
            key_overlay: Arc::new(Vec::new()),
            keypresses: Arc::new(Vec::new()),
            hit_errors: Arc::new(HitErrorArray::default()),
            catcher: None,
            total_hit_errors: 0.0,
            committed_hit_error_index: 0,
            unstable_rate: 0.0,
        }),
    }
}
fn facts() -> SessionFacts {
    SessionFacts {
        client: ClientType::Stable,
        foreground: Some(true),
        settings: InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: None,
        },
    }
}
fn document(s: &StateBundleSnapshot, facts: &SessionFacts) -> Value {
    let mut last = super::super::LastValues::default();
    let mut writer = PublishBatchWriter::new();
    writer.begin(7303);
    project(
        s,
        facts,
        &mut super::super::DiffSink::new(&mut last, &mut writer),
    );
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    let mut doc = json!({});
    for field in decode_publish_batch_ref(&bytes).unwrap().entries() {
        let value = match field.value {
            V::Null => Value::Null,
            V::Bool(v) => json!(v),
            V::I64(v) => json!(v),
            V::U64(v) => json!(v),
            V::F64(v) => json!(v),
            V::String(v) => json!(v),
            V::Cbor(v) => vosu_plugin_api::decode::<Value>(v).unwrap(),
            _ => panic!("unexpected value"),
        };
        let mut parent = doc
            .as_object_mut()
            .unwrap()
            .entry(field.ns)
            .or_insert_with(|| json!({}));
        let tokens: Vec<_> = field.pointer[1..].split('/').collect();
        for key in &tokens[..tokens.len() - 1] {
            parent = parent
                .as_object_mut()
                .unwrap()
                .entry(*key)
                .or_insert_with(|| json!({}));
        }
        parent
            .as_object_mut()
            .unwrap()
            .insert(tokens.last().unwrap().to_string(), value);
    }
    doc
}
#[test]
fn status_map_covers_every_game_state() {
    let expected = [
        "menu",
        "menu",
        "play",
        "menu",
        "selection",
        "selection",
        "selection",
        "results",
        "menu",
        "menu",
        "",
        "selection",
        "selection",
        "selection",
        "results",
        "selection",
        "menu",
        "results",
        "results",
        "menu",
        "menu",
        "menu",
        "menu",
        "menu",
    ];
    for (raw, expected) in expected.into_iter().enumerate() {
        assert!(vosu_osu_model::enums::GameState::from_value(raw as i32).is_some());
        assert_eq!(
            status_map::status(raw as i64, false),
            (!expected.is_empty()).then_some(expected)
        );
    }
    assert_eq!(status_map::status(-1, false), None);
    assert_eq!(status_map::status(24, false), None);
}
#[test]
fn paused_play_maps_to_pause() {
    let mut s = sample();
    Arc::make_mut(&mut s.regular).global.paused = true;
    let d = document(&s, &facts());
    assert_eq!(d["vosu.status"]["status"], "pause");
    assert_eq!(d["vosu.osu"]["session"]["paused"], true);
    assert_eq!(
        d["vosu.osu"]["session"]["state"],
        json!({"number":2,"name":"play"})
    );
    assert!(d["vosu.play"]["score"].is_number());
}
#[test]
fn play_fields_null_outside_play() {
    for status in [0, 1, 7, 10, 11, 14, 17, 18] {
        let mut s = sample();
        Arc::make_mut(&mut s.precise).status = status;
        let d = document(&s, &facts());
        assert_eq!(d["vosu.play"].as_object().unwrap().len(), 9);
        for (key, value) in d["vosu.play"].as_object().unwrap() {
            assert!(value.is_null(), "{status}: {key}");
        }
    }
}
#[test]
fn session_rows_and_ipc_id_only_for_spectators() {
    let mut s = sample();
    Arc::make_mut(&mut s.regular).global.game_time = 9876;
    let mut f = facts();
    f.settings.tournament_ipc_id = Some(4.0);
    let d = document(&s, &f);
    assert_eq!(d["vosu.osu"]["session"]["playTime"], 9876.0);
    assert_eq!(d["vosu.osu"]["session"]["playCount"], 0);
    assert_eq!(d["vosu.osu"]["session"]["server"], "ppy.sh");
    assert_eq!(
        d["vosu.osu"]["session"]["focused"],
        if cfg!(windows) {
            json!(true)
        } else {
            Value::Null
        }
    );
    assert!(d["vosu.osu"]["session"]["ipcId"].is_null());
    f.settings.tournament_spectator = true;
    f.settings.devserver_endpoint = Some("bancho.example".into());
    let d = document(&s, &f);
    assert_eq!(d["vosu.osu"]["session"]["ipcId"], 4.0);
    assert_eq!(d["vosu.osu"]["session"]["server"], "bancho.example");
    f.settings.tournament_ipc_id = None;
    assert!(document(&s, &f)["vosu.osu"]["session"]["ipcId"].is_null());
}
#[test]
fn common_play_rows_preserve_operation_order_and_judgements() {
    let mut s = sample();
    let r = Arc::make_mut(&mut s.regular);
    r.gameplay.score = 123456.0;
    r.gameplay.combo = 42;
    r.gameplay.max_combo = 56;
    r.gameplay.accuracy = 98.1234567;
    r.gameplay.player_hp = 173.21;
    r.gameplay.statistics.perfect = 10;
    r.gameplay.statistics.great = 20;
    r.gameplay.statistics.good = 30;
    r.gameplay.statistics.ok = 40;
    r.gameplay.statistics.meh = 50;
    r.gameplay.statistics.miss = 60;
    for (mode, kind) in [
        (0, "osu-standard"),
        (1, "osu-taiko"),
        (2, "osu-catch"),
        (3, "osu-mania"),
    ] {
        Arc::make_mut(&mut s.regular).gameplay.mode = mode;
        let d = document(&s, &facts());
        let p = &d["vosu.play"];
        assert_eq!(p["score"], 123456.0);
        assert_eq!(p["combo"], 42);
        assert_eq!(p["maxCombo"], 56);
        assert_eq!(p["accuracy"], 98.1234567 / 100.0);
        assert_eq!(p["health"], ((173.21 / 200.0) * 100.0) / 100.0);
        assert_eq!(p["accuracyKind"], kind);
        for (i, (id, name)) in [
            ("perfect", "Perfect"),
            ("great", "Great"),
            ("good", "Good"),
            ("ok", "Ok"),
            ("meh", "Meh"),
            ("miss", "Miss"),
        ]
        .into_iter()
        .enumerate()
        {
            assert_eq!(
                p["judgements"][i],
                json!({"id":id,"displayName":name,"ordinal":i,"count":(i+1)*10})
            );
        }
        assert!(p["progress"].is_null());
    }
}
#[test]
fn beatmap_metadata_rows_and_missing_parsed_defaults() {
    let mut s = sample();
    let r = Arc::make_mut(&mut s.regular);
    r.menu.artist = Some("artist".into());
    r.menu.artist_original = Some("原 artist".into());
    r.menu.title = Some("title".into());
    r.menu.title_original = Some("原 title".into());
    r.menu.creator = Some("mapper".into());
    r.menu.difficulty = Some("version".into());
    r.menu.checksum = Some("checksum".into());
    r.menu.map_id = Some(123);
    r.menu.set_id = Some(456);
    r.menu.ranked_status = Some(4.0);
    r.menu.mp3_length = Some(5000.0);
    r.beatmap.mode = Some(0);
    r.gameplay.mode = 3;
    r.beatmap.first_object_time = Some(100);
    r.beatmap.event_status.kiai = true;
    r.beatmap.event_status.break_time = true;
    let d = document(&s, &facts());
    let b = &d["vosu.osu"]["beatmap"];
    for (key, value) in [
        ("artist", "artist"),
        ("artistUnicode", "原 artist"),
        ("title", "title"),
        ("titleUnicode", "原 title"),
        ("mapper", "mapper"),
        ("version", "version"),
        ("checksum", "checksum"),
        ("source", ""),
        ("tags", ""),
    ] {
        assert_eq!(b[key], value);
    }
    assert_eq!(b["id"], 123);
    assert_eq!(b["set"], 456);
    assert_eq!(b["status"], json!({"number":4.0,"name":"ranked"}));
    assert_eq!(b["mode"], json!({"number":0,"name":"osu"}));
    assert_eq!(b["isKiai"], true);
    assert_eq!(b["isBreak"], true);
    assert_eq!(b["isConvert"], true);
    assert_eq!(
        b["time"],
        json!({"live":123.5,"firstObject":100,"lastObject":0.0,"mp3Length":5000.0,"rate":1.0,"running":null})
    );
    for (chart, value) in [
        ("artist", "artist"),
        ("title", "title"),
        ("charter", "mapper"),
        ("difficultyName", "version"),
    ] {
        assert_eq!(d["vosu.chart"][chart], value);
    }
    let stats = &b["stats"];
    for key in ["ar", "cs", "od", "hp"] {
        assert_eq!(stats[key], json!({"original":0.0,"converted":0.0}));
    }
    assert_eq!(
        stats["objects"],
        json!({"circles":0,"sliders":0,"spinners":0,"holds":0,"total":0})
    );
    assert_eq!(stats["hitWindow"], json!({}));
    assert_eq!(
        stats["bpm"],
        json!({"realtime":0.0,"common":0.0,"min":0.0,"max":0.0})
    );
    assert_eq!(stats["stars"], json!({"live":0.0,"total":0.0}));
    assert_eq!(stats["maxCombo"], 0);
}
#[test]
fn projection_is_allocation_free_after_warmup() {
    let mut s = sample();
    let f = facts();
    let mut projector = super::super::Projector::default();
    let mut caches = super::super::Caches::default();
    let mut writer = PublishBatchWriter::new();
    let mut out = Vec::new();
    writer.begin(7304);
    projector.project_into(&s, &f, &mut writer, &[project], &mut caches);
    writer.finish_into(&mut out).unwrap();
    Arc::make_mut(&mut s.regular).gameplay.score = 1111.0;
    let (_, allocations) = crate::test_alloc::allocations_during(|| {
        writer.begin(7304);
        projector.project_into(&s, &f, &mut writer, &[project], &mut caches);
        writer.finish_into(&mut out).unwrap();
    });
    assert_eq!(allocations, 0);
    assert_eq!(decode_publish_batch_ref(&out).unwrap().len(), 1);
}

fn parsed_sample() -> StateBundleSnapshot {
    use vosu_osu_calc::beatmap::{BeatmapCache, BeatmapInput, ModSet, Ruleset, ScoringMode};
    let content = "osu file format v14\n[General]\nMode:0\n[Metadata]\nSource:source text\nTags:tag1 tag2\n[Difficulty]\nCircleSize:4\nOverallDifficulty:8\nApproachRate:9\nHPDrainRate:5\n[TimingPoints]\n0,500,4,2,1,80,1,0\n[HitObjects]\n64,192,1000,1,0,0:0:0:0:\n128,192,2000,1,0,0:0:0:0:\n";
    let mut cache = BeatmapCache::new(vosu_osu_state::state_backend::LiveBeatmapEngine);
    let mut parsed = cache
        .load(
            BeatmapInput::Content {
                content,
                identity_path: None,
            },
            Ruleset::Osu,
            &ModSet::new("dt", Vec::new(), 1.5).unwrap(),
            ScoringMode::Stable,
        )
        .unwrap()
        .as_ref()
        .clone();
    parsed.engine.original_difficulty.approach_rate = 9.123456;
    parsed.engine.original_difficulty.circle_size = 4.123456;
    parsed.engine.original_difficulty.overall_difficulty = 8.123456;
    parsed.engine.original_difficulty.drain_rate = 5.123456;
    parsed.engine.converted_difficulty.approach_rate = 10.123456;
    parsed.engine.converted_difficulty.circle_size = 7.654321;
    parsed.engine.converted_difficulty.overall_difficulty = 11.123456;
    parsed.engine.converted_difficulty.drain_rate = 6.123456;
    let a = &mut parsed.engine.difficulty;
    a.stars = 6.123456;
    a.aim = Some(1.123456);
    a.speed = Some(2.123456);
    a.flashlight = Some(3.123456);
    a.slider_factor = Some(4.123456);
    a.stamina = Some(5.123456);
    a.rhythm = Some(6.123456);
    a.color = Some(7.123456);
    a.reading = Some(8.123456);
    a.max_combo = 123;
    let w = &mut parsed.engine.hit_windows;
    w.miss = Some(500.123456);
    w.meh = Some(150.123456);
    w.ok = Some(100.123456);
    w.good = Some(75.123456);
    w.great = Some(50.123456);
    w.perfect = Some(25.123456);
    parsed.object_counts = vosu_osu_calc::beatmap::ObjectCounts {
        circles: 1,
        sliders: 2,
        spinners: 3,
        holds: 4,
    };
    parsed.bpm.common = 180.123456;
    parsed.bpm.minimum = 120.234567;
    parsed.bpm.maximum = 240.345678;
    let mut s = sample();
    let r = Arc::make_mut(&mut s.regular);
    r.beatmap.parsed = Some(Arc::new(parsed));
    r.beatmap.attributes = Some(Default::default());
    r.beatmap.attributes.as_mut().unwrap().stars = 7.891234;
    r.beatmap.event_status.bpm = 180.123456;
    r.gameplay.mods.rate = 1.5;
    Arc::make_mut(&mut r.settings.values)
        .get_mut("skin")
        .unwrap()["name"] = json!("test skin");
    s
}
#[test]
fn beatmap_stats_rows_preserve_rounding_and_hit_windows() {
    let s = parsed_sample();
    let d = document(&s, &facts());
    let b = &d["vosu.osu"]["beatmap"];
    let stats = &b["stats"];
    assert_eq!(b["source"], "source text");
    assert_eq!(b["tags"], "tag1 tag2");
    for (key, original, converted) in [
        ("ar", 9.12, 10.12),
        ("cs", 4.12, 7.65),
        ("od", 8.12, 11.12),
        ("hp", 5.12, 6.12),
    ] {
        assert_eq!(
            stats[key],
            json!({"original":original,"converted":converted})
        );
    }
    assert_eq!(
        stats["stars"],
        json!({"live":7.89,"total":6.12,"aim":1.12,"speed":2.12,"flashlight":3.12,"sliderFactor":4.12,"stamina":5.12,"rhythm":6.12,"color":7.12,"reading":8.12,"hitWindow":50.12})
    );
    assert_eq!(
        stats["hitWindow"],
        json!({"miss":500.123456,"meh":150.123456,"ok":100.123456,"good":75.123456,"great":50.123456,"perfect":25.123456})
    );
    assert_eq!(
        stats["objects"],
        json!({"circles":1,"sliders":2,"spinners":3,"holds":4,"total":10})
    );
    assert_eq!(stats["maxCombo"], 123);
    assert_eq!(
        stats["bpm"],
        json!({"realtime":180.1235,"common":180.1235,"min":120.2346,"max":240.3457})
    );
    assert_eq!(d["vosu.skin"]["name"], "test skin");
}
#[test]
fn keycount_mania_only_including_converts_and_results() {
    let mut s = parsed_sample();
    Arc::make_mut(&mut s.regular).beatmap.mode = Some(0);
    for status in [0, 2, 7] {
        Arc::make_mut(&mut s.precise).status = status;
        for mode in 0..4 {
            let r = Arc::make_mut(&mut s.regular);
            r.menu.gamemode = Some(mode);
            r.gameplay.mode = mode;
            r.result_screen.raw.mode = mode;
            let d = document(&s, &facts());
            assert_eq!(
                d["vosu.chart"]["keyCount"],
                if mode == 3 { json!(8.0) } else { Value::Null }
            );
        }
    }
}
#[test]
fn bpm_is_rate_free_and_progress_uses_precise_map_time() {
    let mut s = parsed_sample();
    for (live, progress) in [(-100.0, 0.0), (500.0, 0.25), (4000.0, 1.0)] {
        Arc::make_mut(&mut s.precise).play_time = live;
        Arc::make_mut(&mut s.regular).global.play_time = 9999.0;
        let d = document(&s, &facts());
        assert_eq!(d["vosu.chart"]["bpm"], 120.0);
        assert_eq!(d["vosu.chart"]["length"], 2000.0);
        assert_eq!(d["vosu.osu"]["beatmap"]["time"]["live"], live);
        assert_eq!(d["vosu.play"]["progress"], progress);
        assert_eq!(d["vosu.play"]["rate"], 1.5);
    }
    let mut f = facts();
    f.client = ClientType::Lazer;
    assert!(document(&s, &f)["vosu.play"]["rate"].is_null());
}
#[test]
fn parsed_projection_remains_allocation_free_when_subtrees_change() {
    let mut s = parsed_sample();
    let f = facts();
    let mut projector = super::super::Projector::default();
    let mut caches = super::super::Caches::default();
    let mut writer = PublishBatchWriter::new();
    let mut bytes = Vec::new();
    writer.begin(7305);
    projector.project_into(&s, &f, &mut writer, &[project], &mut caches);
    writer.finish_into(&mut bytes).unwrap();
    let r = Arc::make_mut(&mut s.regular);
    Arc::make_mut(r.beatmap.parsed.as_mut().unwrap())
        .engine
        .difficulty
        .stars = 7.12;
    r.gameplay.statistics.miss = 1;
    let (_, allocations) = crate::test_alloc::allocations_during(|| {
        writer.begin(7305);
        projector.project_into(&s, &f, &mut writer, &[project], &mut caches);
        writer.finish_into(&mut bytes).unwrap();
    });
    assert_eq!(allocations, 0);
    assert_eq!(decode_publish_batch_ref(&bytes).unwrap().len(), 2);
}

#[test]
fn client_clock_overrides_selected_mod_rate_and_reports_running() {
    use vosu_osu_model::memory::{ClientRenderState, PlaybackClock};
    let mut s = sample();
    let r = Arc::make_mut(&mut s.regular);
    r.global.menu_mods.rate = 0.75;
    r.gameplay.mods.rate = 1.5;
    r.result_screen.raw.mods.rate = 1.25;
    for (status, rate) in [(0, 0.75), (2, 1.5), (7, 1.25)] {
        Arc::make_mut(&mut s.precise).status = status;
        assert_eq!(
            document(&s, &facts())["vosu.osu"]["beatmap"]["time"]["rate"],
            rate
        );
    }
    Arc::make_mut(&mut s.regular).global.render = Some(ClientRenderState {
        spectating: false,
        clock: Some(PlaybackClock {
            rate: 1.37,
            running: false,
        }),
    });
    for client in [ClientType::Stable, ClientType::Lazer] {
        let mut f = facts();
        f.client = client;
        let d = document(&s, &f);
        assert_eq!(d["vosu.osu"]["beatmap"]["time"]["rate"], 1.37);
        assert_eq!(d["vosu.osu"]["beatmap"]["time"]["running"], false);
    }
}

#[test]
fn truthy_star_components_and_ranked_status_names_match_1x() {
    let mut s = parsed_sample();
    for value in [
        None,
        Some(0.0),
        Some(f64::NAN),
        Some(f64::INFINITY),
        Some(-1.234567),
    ] {
        let r = Arc::make_mut(&mut s.regular);
        Arc::make_mut(r.beatmap.parsed.as_mut().unwrap())
            .engine
            .difficulty
            .aim = value;
        let d = document(&s, &facts());
        assert_eq!(
            d["vosu.osu"]["beatmap"]["stats"]["stars"]
                .as_object()
                .unwrap()
                .contains_key("aim"),
            value.is_some_and(|v| v != 0.0 && !v.is_nan())
        );
        assert_eq!(
            d["vosu.osu"]["beatmap"]["stats"]["stars"]["aim"],
            value
                .filter(|v| *v != 0.0 && !v.is_nan())
                .map(fix_decimals)
                .map_or(Value::Null, |v| json!(v))
        );
    }
    for (value, name) in [
        (None, ""),
        (Some(0.0), ""),
        (Some(f64::NAN), ""),
        (Some(f64::INFINITY), ""),
        (Some(7.0), "loved"),
    ] {
        Arc::make_mut(&mut s.regular).menu.ranked_status = value;
        let d = document(&s, &facts());
        assert_eq!(d["vosu.osu"]["beatmap"]["status"]["name"], name);
        assert_eq!(
            d["vosu.osu"]["beatmap"]["status"]["number"],
            value.map_or(Value::Null, |v| json!(v))
        );
    }
}

#[test]
fn malformed_server_endpoint_matches_lossy_1x_without_allocating() {
    #[cfg(unix)]
    let endpoint = {
        use std::os::unix::ffi::OsStringExt;
        std::ffi::OsString::from_vec(b"host-\xff-\xed\xa0\x80-\xe2\x82".to_vec())
    };
    #[cfg(windows)]
    let endpoint = {
        use std::os::windows::ffi::OsStringExt;
        std::ffi::OsString::from_wide(&[0x68, 0xd800, 0x6f, 0xdc00])
    };
    let s = sample();
    let mut f = facts();
    let expected = endpoint.to_string_lossy().into_owned();
    f.settings.devserver_endpoint = Some(endpoint);
    assert_eq!(document(&s, &f)["vosu.osu"]["session"]["server"], expected);
    let mut last = super::super::LastValues::default();
    let mut writer = PublishBatchWriter::new();
    writer.begin(7306);
    session::project(
        &s,
        &f,
        &mut super::super::DiffSink::new(&mut last, &mut writer),
    );
    let (_, allocations) = crate::test_alloc::allocations_during(|| {
        writer.begin(7306);
        session::project(
            &s,
            &f,
            &mut super::super::DiffSink::new(&mut last, &mut writer),
        );
    });
    assert_eq!(allocations, 0);
    assert!(writer.is_empty());
}

#[cfg(windows)]
#[test]
fn focused_uses_foreground_not_plugin_focus_on_windows() {
    let s = sample();
    let mut f = facts();
    for foreground in [None, Some(false), Some(true)] {
        f.foreground = foreground;
        assert_eq!(
            document(&s, &f)["vosu.osu"]["session"]["focused"],
            foreground.map_or(Value::Null, |v| json!(v))
        );
    }
}

#[cfg(not(windows))]
#[test]
fn focused_is_null_without_platform_foreground_source() {
    let s = sample();
    let mut f = facts();
    for foreground in [None, Some(false), Some(true)] {
        f.foreground = foreground;
        assert!(document(&s, &f)["vosu.osu"]["session"]["focused"].is_null());
    }
}

#[test]
fn optional_stats_omit_only_missing_components_and_match_embedded_schema() {
    let mut s = parsed_sample();
    let r = Arc::make_mut(&mut s.regular);
    let p = Arc::make_mut(r.beatmap.parsed.as_mut().unwrap());
    p.engine.difficulty.aim = None;
    p.engine.difficulty.speed = Some(0.0);
    p.engine.difficulty.reading = Some(f64::NAN);
    p.engine.hit_windows.miss = None;
    p.engine.hit_windows.good = None;
    p.engine.hit_windows.perfect = Some(0.0);
    let d = document(&s, &facts());
    let stats = &d["vosu.osu"]["beatmap"]["stats"];
    assert_eq!(
        stats["hitWindow"],
        json!({"meh":150.123456,"ok":100.123456,"great":50.123456,"perfect":0.0})
    );
    assert_eq!(
        stats["stars"],
        json!({"live":7.89,"total":6.12,"flashlight":3.12,"sliderFactor":4.12,"stamina":5.12,"rhythm":6.12,"color":7.12,"hitWindow":50.12})
    );
    let schema: Value = serde_json::from_str(crate::OSU_NAMESPACE_SCHEMA).unwrap();
    assert_eq!(schema["version"], "1.2.0");
    assert_eq!(
        schema["$defs"]["stats"]["properties"]["stars"]["properties"]["hitWindow"],
        json!({"type":"number"})
    );
    for (group, properties) in [
        (
            "stars",
            &schema["$defs"]["stats"]["properties"]["stars"]["properties"],
        ),
        ("hitWindow", &schema["$defs"]["hitWindow"]["properties"]),
    ] {
        for (key, value) in stats[group].as_object().unwrap() {
            assert_eq!(properties[key]["type"], "number", "{group}.{key}");
            assert!(value.is_number(), "{group}.{key}");
        }
    }
}
