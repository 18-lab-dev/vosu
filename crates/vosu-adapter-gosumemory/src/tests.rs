use super::*;
use serde_json::{Value, json};
use std::sync::Arc;
use vosu_plugin_api::{CborValue, StateEntry};
use vosu_surface::state::{CapturedSelection, IdentifiedRoster, IdentifiedSession};

fn document() -> Value {
    json!({
        "session": {"state": {"number": 2, "name": "play"}},
        "settings": {"interfaceVisible": true, "replayUIVisible": false, "chatVisibilityStatus": {"number": -3, "name": "unknown"}, "bassDensity": 0.75},
        "beatmap": {
            "selectionMode": 1, "id": 27, "set": 51, "checksum": "md5", "status": {"number": 4},
            "artist": "artist", "artistUnicode": "芸術家", "title": "title", "titleUnicode": "題", "mapper": "mapper", "version": "Hard",
            "time": {"live": 5123.125, "firstHitObject": 1234, "lastObject": 80000.0, "mp3Length": 82000.0},
            "stats": {"ar": {"original": 8.1, "converted": 9.2}, "cs": {"original": 3.3, "converted": 4.4},
                "od": {"original": 6.5, "converted": 7.6}, "hp": {"original": 5.7, "converted": 6.8}, "stars": {"live": 5.23, "total": 6.24},
                "bpm": {"realtime": 231.25, "common": 180.125, "min": 90.0, "max": 241.0},
                "objects": {"circles": 10, "sliders": 20, "spinners": 3, "holds": 4}, "maxCombo": 741},
        },
        "internal": {"paths": {"stable": {"game": "/private/game", "songs": "/private/songs"}, "lazer": null,
            "skinFolder": "skin", "current": {"beatmapFolder": "song", "beatmapFile": "map.osu", "background": "bg.jpg", "audio": "song.mp3"}}},
        "play": {
            "score": 987654.0, "combo": {"current": 51, "max": 89}, "accuracyPercent": 97.123456789,
            "healthRaw": {"normal": 123.125, "smooth": 177.25}, "judgements": {"miss": 1, "meh": 2, "ok": 3, "good": 4, "great": 5, "perfect": 6},
            "mode": {"number": 3}, "playerName": "player", "statistics": {"sliderEndHits": 7, "smallTickHit": 8, "largeTickHit": 9, "smallTickMiss": 10, "largeTickMiss": 11, "largeBonus": 12, "sliderBreaks": 13},
            "rank": {"current": "A", "maxThisPlay": "SH"}, "unstableRate": 82.125,
            "hitErrors": {"all": [1.25, -2.5], "press": [111], "release": [222]},
            "mods": {"number": 64, "name": "DT"}, "pp": {"current": 123.4, "fc": 234.5, "maxAchievable": 345.6},
        },
        "keys": {"all": [{"name": "other", "count": 999}], "overlay": {"k1": {"isPressed": true, "count": 11}, "k2": {"isPressed": false, "count": 22}, "m1": {"isPressed": true, "count": 33}, "m2": {"isPressed": false, "count": 44}}},
        "leaderboard": [{"name": "slot", "score": 42, "combo": {"current": 9, "max": 19}, "mods": {"number": 8, "name": "HD"}, "statistics": {"miss": 7, "meh": 8, "ok": 9, "good": 10, "great": 11, "perfect": 12}, "team": 2, "position": 3, "isFailed": true}],
        "leaderboardState": {"visible": true, "player": {"name": "main", "score": 84, "combo": 10, "maxCombo": 20, "mods": {"number": 16, "name": "HR"}, "judgements": {"miss": 1, "meh": 2, "ok": 3, "good": 4, "great": 5, "perfect": 6}, "team": 1, "position": 4, "isPassing": true}},
        "performance": {"accuracy": {"90": 90.5, "99": 199.5, "100": 300.5}, "graph": {"primary": [91.0, 92.0], "series": [{"name": "aim", "data": [11.0, 12.0]}, {"name": "speed", "data": [21.0, 22.0]}], "xaxis": [1.0, 2.0]}},
        "result": {"name": "result player", "score": 123456, "accuracy": 98.765, "maxCombo": 333, "mods": {"number": 256, "name": "HT"}, "statistics": {"miss": 21, "meh": 22, "ok": 23, "good": 24, "great": 25, "perfect": 26}, "rank": "S", "createdAt": "2026-11-24"},
        "profile": {"userStatus": {"number": 65793}, "banchoStatus": {"number": 4}, "mode": {"number": 2}, "countryCode": {"number": 111}, "id": 123, "name": "profile", "rankedScore": 5678, "level": 55.25, "accuracy": 95.25, "pp": 5123.5, "playCount": 432, "globalRank": 312, "backgroundColour": "-ff"},
        "tourney": {"ipcState": 0, "bestOf": 0, "team": {"left": "", "right": ""}, "points": {"left": 0, "right": 0}, "scoreVisible": false, "starsVisible": false, "chat": [], "totalScore": {"left": 0, "right": 0}, "clients": []}, "tourneyAvailability": "ready",
    })
}
fn capture(doc: &Value, variant: &str) -> CapturedSelection {
    let session = IdentifiedSession {
        session: "session-17".into(),
        generation: 23,
        owner: "vosu.osu".into(),
        owner_instance: 31,
        game: "osu!".into(),
        variant: variant.into(),
        foreground: Some(false),
    };
    CapturedSelection {
        session: Some(session.clone()),
        focus_revision: 41,
        roster: IdentifiedRoster {
            revision: 43,
            sessions: Arc::from([session]),
        },
        revision: 47,
        namespace_revisions: Arc::from([("vosu.osu".into(), 53)]),
        values: view::POINTERS
            .iter()
            .map(|p| StateEntry {
                ns: "vosu.osu".into(),
                pointer: (*p).into(),
                value: doc.pointer(p).map(|v| CborValue::serialized(v).unwrap()),
                error: None,
            })
            .collect::<Vec<_>>()
            .into(),
    }
}
fn decoded(doc: &Value, client: &str) -> View {
    View::decode(capture(doc, client)).unwrap()
}
fn wire(v: &View) -> Value {
    serde_json::from_slice(&encode(v).unwrap()).unwrap()
}
fn equivalent(v: &View) {
    let dto = build_with::<&[f64]>(v).unwrap();
    assert_eq!(
        encode(v).unwrap().as_ref(),
        vosu_adapter_support::json::encode(&dto, 0).unwrap()
    );
}
#[test]
fn direct_and_dto_match_with_distinct_source_facts() {
    let v = decoded(&document(), "stable");
    equivalent(&v);
    let w = wire(&v);
    assert_eq!(w["gameplay"]["keyOverlay"]["k1"]["count"], 11);
    assert_eq!(w["gameplay"]["keyOverlay"]["k2"]["count"], 22);
    assert_eq!(w["gameplay"]["keyOverlay"]["m1"]["count"], 33);
    assert_eq!(w["gameplay"]["keyOverlay"]["m2"]["count"], 44);
    assert_eq!(w["gameplay"]["hits"]["hitErrorArray"], json!([1.25, -2.5]));
    assert!(w["gameplay"]["hits"].get("pressHitErrors").is_none());
    assert!(w["gameplay"]["hits"].get("releaseHitErrors").is_none());
    assert_eq!(w["menu"]["pp"]["strains"], json!([91, 92]));
    assert_ne!(
        w["menu"]["pp"]["strains"],
        w["menu"]["pp"]["strainsAll"]["series"][0]["data"]
    );
    assert_eq!(w["gameplay"]["hp"]["normal"], 123.125);
    assert_eq!(w["menu"]["isChatEnabled"], 1);
    assert_eq!(w["gameplay"]["leaderboard"]["ourplayer"]["name"], "main");
    assert_eq!(w["gameplay"]["leaderboard"]["slots"][0]["name"], "slot");
    assert_eq!(w["resultsScreen"]["300"], 25);
    assert_eq!(w["gameplay"]["hits"]["300"], 5);
    assert_eq!(w["resultsScreen"]["mode"], 3);
    assert_eq!(w["userProfile"]["backgroundColour"], "-ff");
}
#[test]
fn selected_mods_are_not_replaced_by_result_or_mode_sources() {
    for (state, number, name) in [(0, 8, "HD"), (2, 64, "DT"), (7, 256, "HT")] {
        let mut d = document();
        d["session"]["state"]["number"] = json!(state);
        d["play"]["mods"] = json!({"number":number,"name":name});
        let v = decoded(&d, "stable");
        equivalent(&v);
        let w = wire(&v);
        assert_eq!(w["menu"]["mods"]["num"], number);
        assert_eq!(w["resultsScreen"]["mods"]["num"], 256);
        assert_eq!(w["menu"]["gameMode"], 1);
        assert_eq!(w["gameplay"]["gameMode"], 3);
    }
}
#[test]
fn catch_hits_are_converted_once_from_raw_counts_and_ticks() {
    let mut d = document();
    d["play"]["mode"]["number"] = json!(2);
    let v = decoded(&d, "lazer");
    equivalent(&v);
    let w = wire(&v);
    let h = &w["gameplay"]["hits"];
    assert_eq!(h["0"], 12);
    assert_eq!(h["50"], 8);
    assert_eq!(h["100"], 9);
    assert_eq!(h["katu"], 10);
    assert_eq!(w["resultsScreen"]["0"], 21);
}
#[test]
fn absent_selection_null_and_explicit_zero_are_not_conflated() {
    let d = document();
    let mut c = capture(&d, "stable");
    Arc::make_mut(&mut c.values)
        .iter_mut()
        .find(|e| e.pointer == "/play/score")
        .unwrap()
        .value = None;
    assert!(matches!(View::decode(c), Err(Error::NotReady)));
    let mut d = d;
    d["play"]["score"] = Value::Null;
    assert!(View::decode(capture(&d, "stable")).is_err());
    d["play"]["score"] = json!(0.0);
    assert_eq!(decoded(&d, "stable").play.score, 0.0);
    let mut c = capture(&d, "stable");
    c.session = None;
    assert!(matches!(View::decode(c), Err(Error::NotReady)));
}
#[test]
fn omitted_metadata_is_not_null_and_paths_remain_echo_data() {
    let mut d = document();
    d["beatmap"]["artist"] = Value::Null;
    d["internal"]["paths"]["current"]["beatmapFolder"] = Value::Null;
    d["internal"]["paths"]["current"]["beatmapFile"] = Value::Null;
    let v = decoded(&d, "stable");
    equivalent(&v);
    let w = wire(&v);
    assert!(w["menu"]["bm"]["metadata"].get("artist").is_none());
    assert!(w["menu"]["bm"]["path"].get("folder").is_none());
    assert_eq!(w["menu"]["bm"]["path"]["bg"], "bg.jpg");
    assert_eq!(w["menu"]["bm"]["path"]["full"], "bg.jpg");
    assert_eq!(w["settings"]["folders"]["game"], "/private/game");
}
#[test]
fn lazer_pending_omits_tourney_but_missing_score_errors_and_ready_empty_is_present() {
    let mut d = document();
    d["session"]["state"]["number"] = json!(11);
    d["tourney"] = Value::Null;
    d["tourneyAvailability"] = json!("pending");
    let v = decoded(&d, "lazer");
    equivalent(&v);
    assert!(wire(&v).get("tourney").is_none());
    d["tourneyAvailability"] = json!("missingScore");
    let v = decoded(&d, "lazer");
    assert!(matches!(encode(&v), Err(Error::Serialization(_))));
    assert!(matches!(build(&v), Err(Error::Serialization(_))));
    d["tourney"] = document()["tourney"].clone();
    d["tourneyAvailability"] = json!("ready");
    let v = decoded(&d, "lazer");
    equivalent(&v);
    assert!(wire(&v).get("tourney").is_some());
}
#[test]
fn selectors_request_no_removed_extensions_and_manifest_has_exact_private_grant() {
    assert!(
        view::selectors()
            .iter()
            .any(|s| s.pointer == "/keys/overlay")
    );
    assert!(
        view::selectors()
            .iter()
            .any(|s| s.pointer == "/play/hitErrors/all")
    );
    assert!(
        !view::selectors().iter().any(|s| s.pointer.contains("press")
            || s.pointer.contains("release")
            || s.pointer == "/keys/all")
    );
    let manifest = vosu_plugin_api::manifest::PackageManifest::parse(MANIFEST)
        .and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
        .unwrap();
    assert_eq!(manifest.id, "vosu.adapter.gosumemory");
    assert!(MANIFEST.contains("field:vosu.osu.internal.paths"));
}
#[test]
fn path_join_preserves_node_undefined_empty_and_windows_rules() {
    assert_eq!(helpers::join_node_path("", "", '/'), ".");
    assert_eq!(helpers::join_node_path("map", "../bg.jpg", '/'), "bg.jpg");
    assert_eq!(
        helpers::join_node_path("C:\\Songs\\Map", "bg.jpg", '\\'),
        "C:\\Songs\\Map\\bg.jpg"
    );
}
#[test]
fn tournament_preserves_raw_health_hits_chat_and_nested_removals() {
    let mut d = document();
    let p = &d["play"];
    let client_play = json!({
        "playerName": "spectator", "mode": {"number": 2}, "score": 765432, "accuracy": 93.125,
        "combo": p["combo"], "healthRaw": p["healthRaw"], "rawJudgements": p["judgements"],
        "statistics": p["statistics"], "hitErrors": p["hitErrors"], "mods": p["mods"],
        "rank": p["rank"], "unstableRate": 99.25,
    });
    d["tourney"] = json!({
        "ipcState": 11, "bestOf": 7, "team": {"left":"Alpha","right":"Beta"},
        "points": {"left":2,"right":3}, "scoreVisible":true, "starsVisible":false,
        "totalScore": {"left":765432,"right":123456},
        "chat": [
            {"timestamp":"one","name":"spec","message":"hello"},
            {"timestamp":"two","name":"BanchoBot","message":"start"},
            {"timestamp":"three","name":"stranger","message":"unknown"}
        ],
        "clients": [{"team":"left","user":{"name":"spec","country":"JP","id":67,"accuracy":98.2,"rankedScore":12345,"playCount":678,"globalRank":789,"totalPP":9012},
            "play":client_play}],
    });
    for variant in ["stable", "lazer"] {
        let v = decoded(&d, variant);
        equivalent(&v);
        let w = wire(&v);
        let t = &w["tourney"];
        assert_eq!(t["manager"]["bestOF"], 7);
        assert!(t["manager"].get("bestOf").is_none());
        assert_eq!(t["manager"]["chat"][0]["team"], "left");
        assert_eq!(t["manager"]["chat"][1]["team"], "bot");
        assert_eq!(t["manager"]["chat"][2]["team"], "unknown");
        let p = &t["ipcClients"][0]["gameplay"];
        assert_eq!(p["hits"]["100"], 3);
        assert_eq!(p["hp"]["normal"], 123.125);
        assert_eq!(p["hp"]["smooth"], 177.25);
        assert_eq!(p["hits"]["hitErrorArray"], json!([1.25, -2.5]));
        assert!(p["hits"].get("pressHitErrors").is_none());
        assert!(p["hits"].get("releaseHitErrors").is_none());
        assert_eq!(
            p["hits"]["grade"]["maxThisPlay"],
            if variant == "lazer" { "" } else { "SH" }
        );
    }
}
#[test]
fn numeric_boundary_bits_survive_cbor_decode_without_percent_inversion() {
    for n in [
        -0.0,
        f64::from_bits(97.125f64.to_bits() - 1),
        97.125,
        f64::from_bits(97.125f64.to_bits() + 1),
        9007199254740992.0,
    ] {
        let mut c = capture(&document(), "stable");
        Arc::make_mut(&mut c.values)
            .iter_mut()
            .find(|e| e.pointer == "/play/accuracyPercent")
            .unwrap()
            .value = Some(CborValue::Float(n));
        let v = View::decode(c).unwrap();
        assert_eq!(v.play.accuracy.to_bits(), n.to_bits());
        equivalent(&v);
    }
}
#[test]
fn missing_physical_slots_use_only_pinned_wire_defaults() {
    let mut d = document();
    d["keys"]["overlay"]["k2"] = Value::Null;
    let v = decoded(&d, "stable");
    equivalent(&v);
    let w = wire(&v);
    assert_eq!(
        w["gameplay"]["keyOverlay"]["k2"],
        json!({"isPressed":false,"count":0})
    );
    assert_eq!(w["gameplay"]["keyOverlay"]["k1"]["count"], 11);
    assert_eq!(w["gameplay"]["keyOverlay"]["m1"]["count"], 33);
    assert_eq!(w["gameplay"]["keyOverlay"]["m2"]["count"], 44);
}
#[test]
fn absent_profile_emits_empty_object_not_fabricated_user() {
    let mut d = document();
    d["profile"] = json!({
        "userStatus":{"number":null},"banchoStatus":{"number":null},
        "mode":{"number":null},"countryCode":{"number":null},
        "id":null,"name":null,"rankedScore":null,"level":null,"accuracy":null,
        "pp":null,"playCount":null,"globalRank":null,"backgroundColour":null
    });
    let v = decoded(&d, "lazer");
    equivalent(&v);
    assert_eq!(wire(&v)["userProfile"], json!({}));
}
