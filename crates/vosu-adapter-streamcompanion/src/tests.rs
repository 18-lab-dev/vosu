use super::*;
#[path = "tests/core_capture.rs"]
mod core_capture;
use serde_json::{Value, json};
use std::sync::Arc;
use vosu_plugin_api::{CborValue, StateEntry};
use vosu_surface::state::{CapturedSelection, IdentifiedRoster, IdentifiedSession};

fn entry(pointer: &str, value: Value) -> StateEntry {
    StateEntry {
        ns: "vosu.osu".into(),
        pointer: pointer.into(),
        value: Some(CborValue::serialized(&value).unwrap()),
        error: None,
    }
}
pub(crate) fn fixture() -> CapturedSelection {
    let mods = json!({"checksum":"DT","number":64,"name":"DT","rate":1.5,"array":[]});
    let counts = json!({"miss":1,"meh":2,"ok":3,"good":4,"great":5,"perfect":6});
    let player = json!({"name":"main\"\\雪","score":123456.0,"combo":12,"maxCombo":34,"mods":mods,"judgements":counts,"team":1,"position":2,"isPassing":true});
    let values = vec![
        entry(
            "/beatmap",
            json!({"artist":null,"artistUnicode":"作曲家","title":"title\"\\雪","titleUnicode":null,"mapper":"mapper","version":null,"checksum":"abc","id":12,"set":34,"selectionMode":1,"status":{"number":4.0},"time":{"mp3Length":120123.5}}),
        ),
        entry(
            "/internal/paths",
            json!({"current":{"beatmapFolder":"echo-only","beatmapFile":"map.osu","background":"bg.jpg","audio":"audio.mp3"},"skinFolder":"skin-echo"}),
        ),
        entry("/play/playerName", json!("player\"\\雪")),
        entry("/play/mode", json!({"number":3})),
        entry("/play/score", json!(9876543.25)),
        entry("/play/combo", json!({"current":37,"max":82})),
        entry("/play/accuracyPercent", json!(98.7654321)),
        entry("/play/healthRaw", json!({"normal":181.25,"smooth":174.75})),
        entry("/play/judgements", counts.clone()),
        entry("/play/retries", json!(7)),
        entry("/play/preciseUnstableRate", json!(102.125)),
        entry("/play/mods", mods.clone()),
        entry("/play/rank", json!({"current":"SH","maxThisPlay":"XH"})),
        entry(
            "/play/statistics",
            json!({"sliderBreaks":13,"smallTickMiss":17,"smallTickHit":19,"largeTickMiss":23,"largeTickHit":29,"largeBonus":31}),
        ),
        entry(
            "/profile",
            json!({"id":42,"name":"bancho","userStatus":{"number":65793},"countryCode":{"number":111}}),
        ),
        entry("/session/state", json!({"number":2})),
        entry("/session/watchingReplay", json!(true)),
        entry("/beatmap/time/live", json!(61234.5)),
        entry("/settings/chatVisibilityStatus", json!({"number":-1})),
        entry("/settings/interfaceVisible", json!(false)),
        entry("/settings/leaderboard", json!({"number":3})),
        entry(
            "/keys/overlay",
            json!({"k1":{"isPressed":true,"count":11},"k2":null,"m1":{"isPressed":false,"count":33},"m2":{"isPressed":true,"count":44}}),
        ),
        entry("/play/hitErrors/all", json!([-3.25, 4.5, 12.75])),
        entry("/leaderboardState", json!({"visible":true,"player":player})),
        entry(
            "/leaderboard",
            json!([{"name":"slot\"\\雪","score":654321.0,"combo":{"current":56,"max":78},"mods":mods,"statistics":counts,"team":2,"position":1,"isFailed":true}]),
        ),
        entry(
            "/play/pp",
            json!({"current":123.45,"fc":234.56,"detailed":{"current":{"accuracy":11.11,"aim":22.22,"speed":33.33,"difficulty":44.44}}}),
        ),
        entry(
            "/performance/accuracy",
            json!({"90":90.9,"95":95.5,"96":96.6,"97":97.7,"98":98.8,"99":99.9,"100":100.01}),
        ),
        entry(
            "/beatmap/stats",
            json!({"stars":{"live":7.21},"bpm":{"realtime":187.1234}}),
        ),
        entry("/result/mode", json!({"number":2})),
        // Nonempty removed histories and unrelated logical keys must not leak.
        entry("/play/hitErrors/press", json!([1001])),
        entry("/play/hitErrors/release", json!([2002])),
        entry("/keys/all", json!([{"name":"logical","count":999}])),
    ];
    let session = IdentifiedSession {
        session: "session-a".into(),
        generation: 7,
        owner: "vosu.osu".into(),
        owner_instance: 1,
        game: "osu".into(),
        variant: "stable".into(),
        foreground: Some(true),
    };
    CapturedSelection {
        session: Some(session.clone()),
        focus_revision: 5,
        roster: IdentifiedRoster {
            revision: 9,
            sessions: Arc::from([session]),
        },
        revision: 11,
        namespace_revisions: Arc::from([("vosu.osu".into(), 11)]),
        values: values.into(),
    }
}
fn replace(capture: &mut CapturedSelection, pointer: &str, value: Value) {
    let values = Arc::make_mut(&mut capture.values);
    *values.iter_mut().find(|v| v.pointer == pointer).unwrap() = entry(pointer, value);
}
fn parity(view: &view::View, chart: Option<&chart::ChartFacts>) -> Value {
    let dto = build(view, chart).unwrap();
    let expected = vosu_adapter_support::json::encode(&dto, 0).unwrap();
    let direct = encode(view, chart).unwrap();
    assert_eq!(direct.as_ref(), expected.as_slice());
    serde_json::from_slice(&direct).unwrap()
}
#[test]
fn flat_tokens_embedded_documents_and_extensions() {
    let view = view::View::decode(fixture()).unwrap();
    let wire = parity(&view, None);
    assert_eq!(wire["score"], 9876543.25);
    assert_eq!(wire["playerHp"], 181.25);
    assert_eq!(wire["playerHpSmooth"], 174.75);
    assert_eq!(wire["rawStatus"], 2);
    assert_eq!(wire["status"], 8);
    assert_eq!(wire["chatIsEnabled"], 0);
    assert_eq!(wire["banchoIsConnected"], 1);
    assert_eq!(wire["mapArtistTitle"], "undefined - title\"\\雪");
    assert_eq!(wire["mapDiff"], "[undefined]");
    let keys: Value = serde_json::from_str(wire["keyOverlay"].as_str().unwrap()).unwrap();
    assert_eq!(keys["Enabled"], true);
    assert_eq!(keys["K1Count"], 11);
    assert_eq!(keys["K2Count"], 0);
    assert_eq!(keys["M1Count"], 33);
    assert_eq!(keys["M2Count"], 44);
    assert_eq!(keys["M1Pressed"], false);
    let main: Value =
        serde_json::from_str(wire["leaderBoardMainPlayer"].as_str().unwrap()).unwrap();
    let players: Value =
        serde_json::from_str(wire["leaderBoardPlayers"].as_str().unwrap()).unwrap();
    assert_eq!(main["Username"], "main\"\\雪");
    assert_eq!(main["Combo"], 12);
    assert_eq!(players[0]["Combo"], 56);
    assert_eq!(players[0]["IsPassing"], false);
    assert_eq!(wire["hitErrors"], json!([-3.25, 4.5, 12.75]));
    assert!(wire.get("pressHitErrors").is_none());
    assert!(wire.get("releaseHitErrors").is_none());
    assert!(!wire.as_object().unwrap().keys().any(|key| {
        matches!(
            key.to_ascii_lowercase().as_str(),
            "presshiterrors" | "releasehiterrors" | "press_hit_errors" | "release_hit_errors"
        )
    }));
    assert_eq!(wire["unstableRate"], 153.19);
    assert_eq!(wire["convertedUnstableRate"], 102.13);
}
#[test]
fn chart_raw_bpm_range_equal_and_timing_quirks() {
    let view = view::View::decode(fixture()).unwrap();
    let mut chart = chart::ChartFacts::default();
    chart.bpm = chart::Bpm {
        common: 180.123456,
        minimum: 180.123456,
        maximum: 180.123456,
    };
    chart.times = chart::Times {
        full: 120234.5,
        first_object: 1234,
        preview: 5432,
    };
    chart.timing_points.push(dto::MapTimingPoint {
        start_time: f64::NAN,
        bpm: Some(180.0),
        beat_length: 333.333,
    });
    let wire = parity(&view, Some(&chart));
    assert_eq!(wire["mBpm"], "180.123456");
    assert_eq!(wire["mainBpm"], 180.1235);
    assert_eq!(wire["firstHitObjectTime"], 1234);
    assert_eq!(wire["previewtime"], 5432);
    assert_eq!(
        wire["mapTimingPoints"][0],
        json!({"startTime":0,"beatLength":333.333})
    );
    assert_eq!(wire["mapKiaiPoints"], json!([]));
    chart.bpm.minimum = 90.49;
    chart.bpm.maximum = 201.5;
    assert_eq!(parity(&view, Some(&chart))["mBpm"], "90-202 (180)");
}
#[test]
fn raw_modes_status_login_and_missing_metadata_stay_distinct() {
    let mut capture = fixture();
    for (status, expected_mode, expected_status) in
        [(1, 1, 16), (2, 3, 8), (7, 2, 32), (14, 1, 32), (99, 1, 1)]
    {
        replace(&mut capture, "/session/state", json!({"number":status}));
        let wire = parity(&view::View::decode(capture.clone()).unwrap(), None);
        assert_eq!(wire["mode"], expected_mode);
        assert_eq!(wire["status"], expected_status);
        assert_eq!(wire["rawStatus"], status);
    }
    replace(
        &mut capture,
        "/profile",
        json!({"id":null,"name":null,"userStatus":{"number":null},"countryCode":{"number":null}}),
    );
    let wire = parity(&view::View::decode(capture).unwrap(), None);
    assert_eq!(wire["banchoIsConnected"], 0);
    assert!(wire.get("banchoId").is_none());
    assert!(wire.get("banchoStatus").is_none());
    assert!(wire.get("artistRoman").is_none());
}
#[test]
fn catch_and_taiko_use_original_tick_statistics() {
    let mut capture = fixture();
    replace(&mut capture, "/play/mode", json!({"number":2}));
    let wire = parity(&view::View::decode(capture.clone()).unwrap(), None);
    assert_eq!(wire["miss"], 24);
    assert_eq!(wire["c50"], 19);
    assert_eq!(wire["c100"], 29);
    assert_eq!(wire["katsu"], 17);
    replace(&mut capture, "/play/mode", json!({"number":1}));
    let wire = parity(&view::View::decode(capture).unwrap(), None);
    assert_eq!(wire["geki"], 31);
    assert_eq!(wire["katsu"], 31);
}
#[test]
fn absent_null_and_zero_are_not_interchangeable() {
    let mut capture = fixture();
    replace(&mut capture, "/play/score", json!(0.0));
    assert_eq!(view::View::decode(capture.clone()).unwrap().play.score, 0.0);
    replace(&mut capture, "/play/score", Value::Null);
    assert_eq!(
        view::View::decode(capture.clone()).unwrap_err(),
        Error::NotReady
    );
    capture.values = capture
        .values
        .iter()
        .filter(|v| v.pointer != "/play/score")
        .cloned()
        .collect();
    assert_eq!(view::View::decode(capture).unwrap_err(), Error::NotReady);
}
#[test]
fn selectors_never_request_calc_private_payload_or_removed_histories() {
    let selectors = view::selectors();
    assert!(
        selectors
            .iter()
            .any(|s| s.ns == "vosu.chart" && s.pointer == "/resources/file")
    );
    assert!(selectors.iter().any(|s| s.pointer == "/keys/overlay"));
    assert!(
        selectors
            .iter()
            .filter(|s| s.pointer.starts_with("/internal"))
            .all(|s| s.pointer == "/internal/paths")
    );
    assert!(!selectors.iter().any(|s| s.pointer.contains("press")
        || s.pointer.contains("release")
        || s.pointer == "/keys/all"));
}

#[test]
fn repeated_source_rounding_preserves_sc_bytes_at_boundaries() {
    use vosu_adapter_support::fixed::{fix_decimals, fix_decimals_to};
    fn check(value: f64) {
        for decimals in [2, 4] {
            let once = fix_decimals_to(value, decimals).unwrap();
            let twice = if decimals == 2 {
                fix_decimals(once)
            } else {
                fix_decimals_to(once, decimals).unwrap()
            };
            assert_eq!(
                vosu_adapter_support::json::encode(&once, 0).unwrap(),
                vosu_adapter_support::json::encode(&twice, 0).unwrap(),
                "value={value:?}, decimals={decimals}",
            );
        }
    }
    for value in [
        0.0,
        -0.0,
        f64::NAN,
        f64::INFINITY,
        f64::NEG_INFINITY,
        1.005,
        1.015,
        -1.005,
        102.125,
        180.123456,
        9_007_199_254_740_991.0,
    ] {
        check(value);
        if value.is_finite() && value > 0.0 {
            check(f64::from_bits(value.to_bits() - 1));
            check(f64::from_bits(value.to_bits() + 1));
        }
    }
    let mut state = 0x9e3779b97f4a7c15_u64;
    for _ in 0..10_000 {
        state ^= state << 13;
        state ^= state >> 7;
        state ^= state << 17;
        check(f64::from_bits(state));
    }
}

#[test]
fn null_overlay_slots_use_legacy_empty_record_but_absent_and_errors_are_not_ready() {
    let mut capture = fixture();
    replace(&mut capture, "/keys/overlay", Value::Null);
    let view = view::View::decode(capture.clone()).unwrap();
    let wire = parity(&view, None);
    let keys: Value = serde_json::from_str(wire["keyOverlay"].as_str().unwrap()).unwrap();
    for key in ["K1", "K2", "M1", "M2"] {
        assert_eq!(keys[format!("{key}Count")], 0);
        assert_eq!(keys[format!("{key}Pressed")], false);
    }
    let mut errored = capture.clone();
    Arc::make_mut(&mut errored.values)
        .iter_mut()
        .find(|entry| entry.pointer == "/keys/overlay")
        .unwrap()
        .error = Some(vosu_plugin_api::ErrorInfo {
        code: "forbidden".into(),
        message: "distinct denied overlay".into(),
    });
    assert_eq!(view::View::decode(errored).unwrap_err(), Error::NotReady);
    capture.values = capture
        .values
        .iter()
        .filter(|entry| entry.pointer != "/keys/overlay")
        .cloned()
        .collect();
    assert_eq!(view::View::decode(capture).unwrap_err(), Error::NotReady);
}
