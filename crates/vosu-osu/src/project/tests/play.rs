use super::*;
use serde_json::{Value, json};
use std::{collections::BTreeMap, sync::Arc};
use vosu_osu_model::{
    launch::InstanceSettings,
    states::{HitErrorArray, KeyHitErrors, KeypressButton, LeaderboardPlayer},
};
use vosu_osu_state::*;
use vosu_plugin_api::{PublishBatchWriter, decode_publish_batch_ref};

fn fixture() -> StateBundleSnapshot {
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
            room: Default::default(),
        }),
        precise: Arc::new(PreciseState {
            status: 2,
            play_time: 123.0,
            mode: 0,
            key_overlay: Arc::new(Vec::new()),
            keypresses: Arc::new(vec![KeypressButton {
                name: Arc::from("B1"),
                is_pressed: true,
                count: 19,
                pressed_at: Some(12.5),
                released_at: None,
            }]),
            hit_errors: Arc::new(HitErrorArray {
                all: vec![-2.0, 3.0],
                press: vec![-2.0],
                release: vec![3.0],
                offsets: vec![1.0, 2.0, 3.0],
                key: Some(KeyHitErrors {
                    press: vec![vec![-2.0]],
                    release: vec![vec![3.0]],
                    press_columns: vec![Some(0)],
                }),
            }),
            catcher: None,
            total_hit_errors: 0.0,
            committed_hit_error_index: 0,
            unstable_rate: 999.0,
        }),
    }
}
fn facts() -> SessionFacts {
    SessionFacts {
        client: ClientType::Stable,
        settings: InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: None,
        },
        foreground: None,
    }
}
#[derive(Default)]
struct Collect(BTreeMap<&'static str, Value>);
impl ProjectionSink for Collect {
    fn put(&mut self, p: P, v: V<'_>) {
        let value = match v {
            V::Null => Value::Null,
            V::Bool(v) => json!(v),
            V::I64(v) => json!(v),
            V::U64(v) => json!(v),
            V::F64(v) => json!(v),
            V::String(v) => json!(v),
            _ => panic!("unexpected scalar"),
        };
        self.0.insert(p.pointer(), value);
    }
    fn put_cbor(&mut self, p: P, write: &mut dyn FnMut(&mut Vec<u8>)) {
        let mut bytes = Vec::new();
        write(&mut bytes);
        let v: Value = vosu_plugin_api::decode(&bytes).unwrap();
        self.0.insert(p.pointer(), v);
    }
}
fn collect(s: &StateBundleSnapshot) -> BTreeMap<&'static str, Value> {
    let mut c = Collect::default();
    project(s, &facts(), &mut c);
    c.0
}

#[test]
fn hit_error_arrays_are_whole_values() {
    let mut s = fixture();
    let first = collect(&s);
    assert_eq!(
        first["/play/hitErrors"],
        json!({"all": [-2.0,3.0], "press": [-2.0], "release": [3.0]})
    );
    assert_eq!(first["/play/hitOffsets"], json!([1.0, 2.0, 3.0]));
    assert_eq!(
        first["/play/keyHitErrors"],
        json!({"press": [[-2.0]], "release": [[3.0]]})
    );
    let regular = Arc::clone(&s.regular);
    Arc::make_mut(&mut s.precise).hit_errors = Arc::new(HitErrorArray::default());
    let next = collect(&s);
    assert!(Arc::ptr_eq(&regular, &s.regular));
    assert_eq!(
        next["/play/hitErrors"],
        json!({"all": [], "press": [], "release": []})
    );
    assert_eq!(
        next["/play/keyHitErrors"],
        json!({"press": [], "release": []})
    );
    assert_eq!(next["/play/hitOffsets"], json!([]));
    assert_eq!(
        next["/play/unstableRate"],
        json!(0.0),
        "regular UR must not be replaced by precise UR"
    );
}

#[test]
fn keys_all_only() {
    let mut s = fixture();
    let doc = collect(&s);
    assert_eq!(
        doc["/keys/all"],
        json!([{"name":"B1", "isPressed":true, "count":19, "pressedAt":12.5, "releasedAt":null}])
    );
    assert_eq!(doc.keys().filter(|k| k.starts_with("/keys/")).count(), 1);
    Arc::make_mut(&mut s.precise).keypresses = Arc::new(Vec::new());
    assert_eq!(collect(&s)["/keys/all"], json!([]));
}

#[test]
fn catcher_and_offsets_respect_precise_status_and_mode() {
    let mut s = fixture();
    assert_eq!(collect(&s)["/play/catcher"], Value::Null);
    let p = Arc::make_mut(&mut s.precise);
    p.mode = 2;
    p.catcher = Some(vosu_osu_model::memory::Catcher {
        x: 123.5,
        dashing: true,
    });
    assert_eq!(
        collect(&s)["/play/catcher"],
        json!({"x":123.5,"dashing":true})
    );
    Arc::make_mut(&mut s.precise).catcher.as_mut().unwrap().x = 0.1;
    let expected: Value =
        serde_json::from_str(&serde_json::to_string(&s.precise.active_catcher()).unwrap()).unwrap();
    assert_eq!(collect(&s)["/play/catcher"], expected);
    assert_eq!(collect(&s)["/play/hitOffsets"], json!([]));
    Arc::make_mut(&mut s.precise).status = 7;
    assert_eq!(collect(&s)["/play/catcher"], Value::Null);
}

#[test]
fn statistics_rename_includes_good() {
    let mut s = fixture();
    let r = Arc::make_mut(&mut s.regular);
    let g = &mut r.gameplay;
    g.statistics = Statistics {
        perfect: 1,
        great: 2,
        good: 3,
        ok: 4,
        meh: 5,
        miss: 6,
        small_tick_miss: 7,
        small_tick_hit: 8,
        large_tick_miss: 9,
        large_tick_hit: 10,
        small_bonus: 11,
        large_bonus: 12,
        ignore_miss: 13,
        ignore_hit: 14,
        combo_break: 15,
        slider_tail_hit: 16,
        legacy_combo_increase: Some(17),
    };
    g.slider_breaks = 18;
    g.leaderboard_scores.push(LeaderboardPlayer {
        user_id: 27,
        name: "leader".into(),
        score: 12345.0,
        combo: 19,
        accuracy: 98.2,
        max_combo: 29,
        mods: g.mods.clone(),
        statistics: g.statistics.clone(),
        team: 2,
        position: 4,
        is_passing: false,
    });
    r.result_screen.raw.statistics = g.statistics.clone();
    let doc = collect(&s);
    assert_eq!(
        doc["/play/statistics"],
        json!({"smallTickMiss":7,"smallTickHit":8,"largeTickMiss":9,"largeTickHit":10,"smallBonus":11,"largeBonus":12,"ignoreMiss":13,"ignoreHit":14,"comboBreak":15,"sliderTailHit":16,"sliderBreaks":18,"sliderEndHits":16,"legacyComboIncrease":17,"good":3})
    );
    let mut expected = json!({"miss":6,"meh":5,"ok":4,"great":2,"perfect":1,"good":3});
    assert_eq!(doc["/leaderboard"][0]["statistics"], expected);
    for (key, value) in [
        ("sliderEndHits", 16),
        ("smallTickHits", 8),
        ("largeTickHits", 10),
        ("sliderBreaks", 0),
    ] {
        expected[key] = json!(value);
    }
    assert_eq!(doc["/result"]["statistics"], expected);
    let mut leader = doc["/leaderboard"][0].clone();
    leader.as_object_mut().unwrap().remove("statistics");
    assert_eq!(
        leader,
        json!({"isFailed":true,"position":4,"team":2,"id":27,"name":"leader","score":12345.0,"accuracy":98.2,"combo":{"current":19,"max":29},"mods":serde_json::to_value(&s.regular.gameplay.mods).unwrap(),"rank":""})
    );
}

#[test]
fn regular_play_and_results_mapping() {
    let mut s = fixture();
    let r = Arc::make_mut(&mut s.regular);
    let g = &mut r.gameplay;
    g.failed = true;
    g.player_name = "player".into();
    g.mode = 3;
    g.player_hp = 123.45;
    g.player_hp_smooth = 99.1;
    g.grade_current = "A".into();
    g.grade_expected = "S".into();
    g.unstable_rate = 123.4;
    let a = r.beatmap.attributes.get_or_insert_default();
    a.pp = 123.456;
    a.fc_pp = 234.567;
    a.max_achieved = 345.678;
    a.max_achievable = 456.789;
    a.current.aim = 1.234;
    a.current.speed = 2.345;
    a.current.accuracy = 3.456;
    a.current.difficulty = 4.567;
    a.current.flashlight = 5.678;
    a.fc = a.current;
    let raw = &mut r.result_screen.raw;
    raw.online_id = 55.0;
    raw.player_name = "winner".into();
    raw.mode = 2;
    raw.score = 98765.0;
    raw.accuracy = 95.5;
    raw.max_combo = 123;
    raw.date = "2026-11-24T00:00:00Z".into();
    r.result_screen.grade = "SH".into();
    r.result_screen.pp = 123.456789;
    r.result_screen.fc_pp = 234.567891;
    let doc = collect(&s);
    for (key, value) in [
        ("source", json!("player")),
        ("failed", json!(true)),
        ("playerName", json!("player")),
        ("mode", json!({"number":3,"name":"mania"})),
        (
            "healthBar",
            json!({"normal":(123.45_f64/200.0)*100.0,"smooth":(99.1_f64/200.0)*100.0}),
        ),
        ("rank", json!({"current":"A","maxThisPlay":"S"})),
        ("unstableRate", json!(123.4)),
    ] {
        assert_eq!(doc[format!("/play/{key}").as_str()], value, "{key}");
    }
    assert_eq!(
        doc["/play/mods"],
        serde_json::to_value(&s.regular.gameplay.mods).unwrap()
    );
    let detailed = json!({"aim":fix_decimals(1.234),"speed":fix_decimals(2.345),"accuracy":fix_decimals(3.456),"difficulty":fix_decimals(4.567),"flashlight":fix_decimals(5.678),"total":fix_decimals(123.456)});
    assert_eq!(doc["/play/pp"]["detailed"]["current"], detailed);
    let mut fc = detailed;
    fc["total"] = json!(fix_decimals(234.567));
    assert_eq!(
        doc["/play/pp"],
        json!({"current":fix_decimals(123.456),"fc":fix_decimals(234.567),"maxAchieved":fix_decimals(345.678),"maxAchievable":fix_decimals(456.789),"detailed":{"current":doc["/play/pp"]["detailed"]["current"],"fc":fc}})
    );
    let mut result = doc["/result"].clone();
    result.as_object_mut().unwrap().remove("statistics");
    assert_eq!(
        result,
        json!({"scoreId":55.0,"playerName":"winner","mode":{"number":2,"name":"fruits"},"score":98765.0,"accuracy":95.5,"name":"winner","mods":serde_json::to_value(&s.regular.result_screen.raw.mods).unwrap(),"maxCombo":123,"rank":"SH","pp":{"current":123.456789,"fc":234.567891},"createdAt":"2026-11-24T00:00:00Z"})
    );
}

#[test]
fn source_precedence_and_mod_selection_match_1x() {
    let mut s = fixture();
    Arc::make_mut(&mut s.regular).global.is_watching_replay = true;
    assert_eq!(collect(&s)["/play/source"], json!("watching"));
    let mut c = Collect::default();
    let mut f = facts();
    f.client = ClientType::Lazer;
    project(&s, &f, &mut c);
    assert_eq!(c.0["/play/source"], json!("replay"));
    Arc::make_mut(&mut s.regular).global.is_multi_spectating = true;
    assert_eq!(collect(&s)["/play/source"], json!("spectator"));
    let r = Arc::make_mut(&mut s.regular);
    r.global.is_watching_replay = false;
    r.global.is_multi_spectating = false;
    r.gameplay.mods.array.push(vosu_osu_model::states::Mod {
        acronym: "AT".into(),
        settings: None,
    });
    assert_eq!(collect(&s)["/play/source"], json!("autoplay"));
    for status in [0, 7] {
        Arc::make_mut(&mut s.precise).status = status;
        let r = Arc::make_mut(&mut s.regular);
        r.global.menu_mods.name = "menu".into();
        r.result_screen.raw.mods.name = "result".into();
        let doc = collect(&s);
        assert_eq!(doc["/play/source"], Value::Null);
        assert_eq!(
            doc["/play/mods"]["name"],
            json!(if status == 7 { "result" } else { "menu" })
        );
        assert_eq!(doc["/play/mode"], json!({"number":0,"name":"osu"}));
    }
}

#[test]
fn graph_published_once_per_change() {
    let mut s = fixture();
    let r = Arc::make_mut(&mut s.regular);
    for n in 90..=100 {
        r.beatmap.pp_acc.insert(n, f64::from(n) + 0.12345);
    }
    let mut projector = super::super::Projector::default();
    let mut writer = PublishBatchWriter::new();
    writer.begin(4);
    let mut caches = super::super::Caches::default();
    projector.project_into(&s, &facts(), &mut writer, &[project], &mut caches);
    let mut initial = Vec::new();
    writer.finish_into(&mut initial).unwrap();
    let initial = decode_publish_batch_ref(&initial).unwrap();
    let expected: std::collections::BTreeSet<_> = collect(&s).keys().copied().collect();
    let actual: std::collections::BTreeSet<_> = initial
        .entries()
        .filter(|e| expected.contains(e.pointer))
        .map(|e| e.pointer)
        .collect();
    assert_eq!(actual.len(), 19);
    assert_eq!(actual, expected);
    let doc = collect(&s);
    for n in 90..=100 {
        assert_eq!(
            doc["/performance/accuracy"][n.to_string()],
            json!(fix_decimals(f64::from(n) + 0.12345))
        );
    }
    assert_eq!(doc["/performance/graph"], json!({"series":[],"xaxis":[]}));
    writer.begin(4);
    projector.project_into(&s, &facts(), &mut writer, &[project], &mut caches);
    assert!(writer.is_empty());
    Arc::make_mut(&mut Arc::make_mut(&mut s.regular).beatmap.graph)
        .x_axis
        .push(1.5);
    writer.begin(4);
    projector.project_into(&s, &facts(), &mut writer, &[project], &mut caches);
    assert_eq!(writer.len(), 1);
    let mut encoded = Vec::new();
    writer.finish_into(&mut encoded).unwrap();
    let batch = decode_publish_batch_ref(&encoded).unwrap();
    assert_eq!(
        batch.entries().next().unwrap().pointer,
        "/performance/graph"
    );
    assert_eq!(
        collect(&s)["/performance/graph"],
        json!({"series":[],"xaxis":[1.5]})
    );
}

// This boundary deliberately includes shared fix_decimals rounding, not only
// the subtree writer: D17 requires both to remain allocation-free.
#[test]
fn projection_steady_state_allocates_nothing() {
    let mut s = fixture();
    let mut projector = super::super::Projector::default();
    let mut writer = PublishBatchWriter::new();
    let f = facts();
    writer.begin(4);
    let mut caches = super::super::Caches::default();
    projector.project_into(&s, &f, &mut writer, &[project], &mut caches);
    Arc::make_mut(&mut s.regular).gameplay.player_name = "changed".into();
    Arc::make_mut(&mut Arc::make_mut(&mut s.precise).hit_errors).all[0] = 4.0;
    // Warm larger string capacities, then change same-size values to exercise diff/encode.
    writer.begin(4);
    projector.project_into(&s, &f, &mut writer, &[project], &mut caches);
    Arc::make_mut(&mut s.regular).gameplay.player_name = "another".into();
    Arc::make_mut(&mut Arc::make_mut(&mut s.precise).hit_errors).all[0] = 5.0;
    writer.begin(4);
    let (_, allocations) = crate::test_alloc::allocations_during(|| {
        projector.project_into(&s, &f, &mut writer, &[project], &mut caches)
    });
    assert_eq!(allocations, 0);
    assert_eq!(
        writer.len(),
        2,
        "exercise changed regular and precise fields, not an Arc shortcut"
    );
}

#[test]
fn publisher_encodes_both_lanes_without_allocating() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use vosu_plugin_api::{HostServices, Message, vosu_opcode, vosu_status};
    #[derive(Default)]
    struct Host {
        calls: AtomicUsize,
        regular: AtomicUsize,
        precise: AtomicUsize,
    }
    struct SharedHost(Arc<Host>);
    impl HostServices for SharedHost {
        fn send(&self, _: &Message, _: u64) -> vosu_status {
            panic!("successful publication must not log")
        }
        fn send_flat(&self, opcode: vosu_opcode, _: u32, _: u64, bytes: &[u8]) -> vosu_status {
            assert_eq!(opcode, vosu_opcode::VOSU_OP_PUBLISH_BATCH);
            let batch = decode_publish_batch_ref(bytes).unwrap();
            self.0.calls.fetch_add(1, Ordering::Relaxed);
            for entry in batch.entries() {
                if entry.pointer == "/play/playerName" {
                    self.0.regular.fetch_add(1, Ordering::Relaxed);
                }
                if entry.pointer == "/play/hitErrors" {
                    self.0.precise.fetch_add(1, Ordering::Relaxed);
                }
            }
            vosu_status::VOSU_OK
        }
    }
    let host = Arc::new(Host::default());
    let bus = crate::publish::BusPublisher::new(
        SharedHost(Arc::clone(&host)),
        81004,
        Arc::new(AtomicBool::new(true)),
        facts(),
    );
    let mut initial = fixture();
    Arc::make_mut(&mut initial.regular).gameplay.player_name = "initial".into();
    let mut next = initial.clone();
    Arc::make_mut(&mut next.regular).gameplay.player_name = "changed".into();
    Arc::make_mut(&mut Arc::make_mut(&mut next.precise).hit_errors).all[0] = 5.0;
    bus.publish(Arc::new(initial), Delivery::All).unwrap();
    let next = Arc::new(next);
    let (_, allocations) = crate::test_alloc::allocations_during(|| {
        bus.publish(next, Delivery::Precise).unwrap();
    });
    assert_eq!(allocations, 0);
    assert_eq!(host.calls.load(Ordering::Relaxed), 2);
    assert_eq!(host.regular.load(Ordering::Relaxed), 2);
    assert_eq!(host.precise.load(Ordering::Relaxed), 2);
}

#[test]
fn mods_settings_preserve_json_values_and_integer_extremes() {
    let mut s = fixture();
    let mods = &mut Arc::make_mut(&mut s.regular).gameplay.mods;
    mods.number = i64::MIN;
    mods.checksum = "設定".into();
    mods.name = "HD".into();
    mods.rate = 1.25;
    mods.array.push(vosu_osu_model::states::Mod {
        acronym: "HD".into(),
        settings: Some(
            serde_json::from_value(json!({
                "null": null,
                "bool": true,
                "negative": -9223372036854775808_i64,
                "large": 18446744073709551615_u64,
                "nested": [0.125, "設定", {"key": false}],
                "long": "abcdefghijklmnopqrstuvwxyzabcdefghijklmnopqrstuvwxyz"
            }))
            .unwrap(),
        ),
    });
    let expected = serde_json::to_value(mods).unwrap();
    assert_eq!(collect(&s)["/play/mods"], expected);
}

#[test]
fn embedded_schema_requires_the_good_statistics_count() {
    let schema: Value = serde_json::from_str(crate::OSU_NAMESPACE_SCHEMA).unwrap();
    assert_eq!(schema["version"], "1.2.0");
    assert_eq!(
        schema["$defs"]["statistics"]["properties"]["good"]["type"],
        "integer"
    );
    assert!(
        schema["$defs"]["statistics"]["required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "good")
    );
}
