//! Byte-for-byte parity tests across direct CBOR -> JSON encoding,
//! direct `dto::Response` wire encoding, and `serde_json` (`JsFormatter`).
use super::*;
use ciborium::Value as Cbor;
use serde_json::{Value, json};
use vosu_adapter_support::wire_fixtures::{AWKWARD, NUMBERS};
use vosu_osu_model::enums::ClientType;
use vosu_plugin_api::StateEntry;
use vosu_surface::{filter::Filter, projection::Projection, state::CapturedSelection};

fn assert_regular_three_way_parity(capture: &CapturedSelection) -> Value {
    let view = regular::View::from_capture(capture).unwrap();
    let dto_serde = dto_bytes(regular::build(&view));
    let dto_wire = regular::encode(&view).unwrap();
    let direct_wire = regular::encode_capture(capture).unwrap();
    assert_eq!(
        dto_wire.as_ref(),
        dto_serde.as_slice(),
        "wire::regular(Response) must match serde_json + JsFormatter byte-for-byte"
    );
    assert_eq!(
        direct_wire.as_ref(),
        dto_serde.as_slice(),
        "regular::encode_capture must match serde_json + JsFormatter byte-for-byte"
    );
    let projected = provider::Regular.decode(capture.clone()).unwrap();
    assert_eq!(
        projected.encode(&[]).unwrap().as_ref(),
        dto_serde.as_slice(),
        "provider::Regular unfiltered output must match serde_json + JsFormatter"
    );
    serde_json::from_slice(&direct_wire).unwrap()
}

fn assert_precise_three_way_parity(capture: &CapturedSelection) -> Value {
    let view = precise::View::from_capture(capture).unwrap();
    let dto_serde = dto_bytes(precise::build(&view));
    let dto_wire = precise::encode(&view).unwrap();
    let direct_wire = precise::encode_capture(capture).unwrap();
    assert_eq!(dto_wire.as_ref(), dto_serde.as_slice());
    assert_eq!(direct_wire.as_ref(), dto_serde.as_slice());
    let projected = provider::Precise.decode(capture.clone()).unwrap();
    assert_eq!(
        projected.encode(&[]).unwrap().as_ref(),
        dto_serde.as_slice()
    );
    serde_json::from_slice(&direct_wire).unwrap()
}

fn replace_cbor(capture: &mut CapturedSelection, pointer: &str, value: Cbor) {
    let entries = std::sync::Arc::make_mut(&mut capture.values);
    let entry = entries
        .iter_mut()
        .find(|entry| entry.ns == "vosu.osu" && entry.pointer == pointer)
        .unwrap();
    entry.value = Some(value);
}

#[test]
fn empty_default_stable_and_lazer_captures_match_serde_byte_for_byte() {
    for client in [ClientType::Stable, ClientType::Lazer] {
        let mut source = fixture::source();
        source.source = client;
        *std::sync::Arc::make_mut(&mut source.regular) = vosu_osu_state::RegularState {
            global: Default::default(),
            settings: Default::default(),
            beatmap: Default::default(),
            menu: Default::default(),
            bass_density: Default::default(),
            gameplay: Default::default(),
            result_screen: Default::default(),
            tourney: Default::default(),
            user: Default::default(),
            lazer_multi_spectating: Default::default(),
            ranked_play: Default::default(),
            room: Default::default(),
        };
        let capture = fixture::capture(&source, &regular::selectors());
        let doc = assert_regular_three_way_parity(&capture);
        assert_eq!(
            doc["client"],
            if client == ClientType::Lazer {
                "lazer"
            } else {
                "stable"
            }
        );
        // Reversed entry order exercises the non-ordered slot indexer path.
        let mut reversed = capture.clone();
        std::sync::Arc::make_mut(&mut reversed.values).reverse();
        assert_regular_three_way_parity(&reversed);
    }
}

#[test]
fn fully_populated_document_with_awkward_strings_and_all_optional_subtrees_matches_serde() {
    for client in [ClientType::Stable, ClientType::Lazer] {
        let mut source = fixture::source();
        source.source = client;
        let mut selected = fixture::capture(&source, &regular::selectors());

        fixture::replace(&mut selected, "/session/server", json!(AWKWARD));
        fixture::replace(
            &mut selected,
            "/rankedPlay",
            json!({
                "stage": 4,
                "currentRound": 3,
                "starRating": 7.25,
                "damageMultiplier": 1.5,
                "activeUserId": 1001,
                "winningUserId": 1002,
                "users": [
                    {"id": 1001, "rating": 1520.5, "life": 800000.0, "roundsWon": 2, "damageMultiplier": 1.25},
                    {"id": 1002, "rating": 1480.0, "life": 650000.0, "roundsWon": 1, "damageMultiplier": 1.0}
                ]
            }),
        );
        fixture::replace(
            &mut selected,
            "/profile",
            json!({
                "userStatus": {"number": 65793, "name": "Connected"},
                "banchoStatus": {"number": 2, "name": "Playing"},
                "id": 42,
                "name": AWKWARD,
                "mode": {"number": 0, "name": "osu"},
                "rankedScore": 9_007_199_254_740_991.0,
                "level": 101.75,
                "accuracy": 99.12,
                "pp": 12345.67,
                "playCount": 54321,
                "globalRank": 7,
                "countryCode": {"number": 108, "name": "JP"},
                "backgroundColour": "ff00aa",
                "matchmaking": {
                    "rating": 2100.5,
                    "rank": null,
                    "plays": 19,
                    "wins": 14,
                    "isProvisional": true
                }
            }),
        );
        let mut beatmap: Value = view::decode(&selected, "/beatmap").unwrap();
        beatmap["artist"] = json!(AWKWARD);
        beatmap["artistUnicode"] = json!(AWKWARD);
        beatmap["title"] = json!(AWKWARD);
        beatmap["titleUnicode"] = json!(AWKWARD);
        beatmap["mapper"] = json!(AWKWARD);
        beatmap["version"] = json!(AWKWARD);
        beatmap["source"] = json!(AWKWARD);
        beatmap["tags"] = json!(AWKWARD);
        beatmap["checksum"] = json!("deadbeef");
        beatmap["id"] = json!(12345);
        beatmap["set"] = json!(67890);
        beatmap["time"]["mp3Length"] = json!(215000.5);
        beatmap["status"] = json!({"number": 4.0, "name": "ranked"});
        beatmap["stats"]["stars"] = json!({
            "live": 5.5,
            "aim": 2.75,
            "speed": 2.25,
            "flashlight": 0.5,
            "sliderFactor": 0.99,
            "stamina": 1.1,
            "rhythm": 1.2,
            "color": 1.3,
            "reading": 1.4,
            "hitWindow": 32.5,
            "total": 6.12
        });
        beatmap["stats"]["hitWindow"] = json!({
            "miss": 400.0,
            "meh": 150.0,
            "ok": 100.0,
            "good": 75.0,
            "great": 50.0,
            "perfect": 16.5
        });
        fixture::replace(&mut selected, "/beatmap", beatmap);

        let mods_with_settings = json!({
            "checksum": "hd-dt",
            "number": 72,
            "name": format!("HDDT{AWKWARD}"),
            "array": [
                {"acronym": "HD"},
                {"acronym": "DT", "settings": {"speed_change": 1.5, "label": AWKWARD, "flag": true, "extra": [1, 2.5, null]}}
            ],
            "rate": 1.5
        });
        fixture::replace(&mut selected, "/play/mods", mods_with_settings.clone());
        fixture::replace(&mut selected, "/play/playerName", json!(AWKWARD));
        fixture::replace(
            &mut selected,
            "/leaderboard",
            json!([
                {
                    "isFailed": false, "position": 1, "team": 0, "id": 7, "name": AWKWARD,
                    "score": 1_000_000.0, "accuracy": 99.5,
                    "statistics": {"miss": 0, "meh": 1, "ok": 2, "great": 300, "perfect": 50, "good": 5},
                    "combo": {"current": 400, "max": 400},
                    "mods": mods_with_settings,
                    "rank": "S"
                },
                {
                    "isFailed": true, "position": 2, "team": 1, "id": 8, "name": "second",
                    "score": 500_000.0, "accuracy": 88.25,
                    "hits": {"0": 10, "50": 5, "100": 20, "300": 150, "geki": 10, "katu": 3},
                    "combo": {"current": 12, "max": 99},
                    "mods": {"checksum": "", "number": 0, "name": "", "array": [], "rate": 1.0},
                    "rank": "B"
                }
            ]),
        );
        fixture::replace(
            &mut selected,
            "/performance",
            json!({
                "accuracy": {"90": 100.0, "91": 110.5, "92": 120.0, "93": 130.0, "94": 140.0, "95": 150.25, "96": 160.0, "97": 170.0, "98": 180.0, "99": 190.0, "100": 200.0},
                "graph": {
                    "primary": [1.0, 2.0],
                    "series": [
                        {"name": "aim", "data": [0.0, 1.5, -2.5]},
                        {"name": AWKWARD, "data": [3.25, 4.0]}
                    ],
                    "xaxis": [0.0, 1000.0, 2000.0]
                }
            }),
        );
        fixture::replace(
            &mut selected,
            "/room",
            json!({
                "roomId": 999,
                "users": [
                    {"id": 1, "username": AWKWARD, "countryCode": 108, "avatarUrl": "https://example.test/a.png"},
                    {"id": 2, "username": "plain", "countryCode": 233}
                ]
            }),
        );
        let stats = view::decode::<Value>(&selected, "/beatmap").unwrap()["stats"].clone();
        let pp = view::decode::<Value>(&selected, "/play/pp").unwrap();
        fixture::replace(
            &mut selected,
            "/tourney",
            json!({
                "scoreVisible": true,
                "starsVisible": true,
                "ipcState": 3,
                "bestOf": 9,
                "team": {"left": AWKWARD, "right": "Blue"},
                "points": {"left": 2, "right": 3},
                "chat": [{"team": "left", "timestamp": "01:23", "name": AWKWARD, "message": AWKWARD}],
                "totalScore": {"left": 1234567.0, "right": 7654321.5},
                "clients": [{
                    "ipcId": 1.0,
                    "team": "right",
                    "settings": {"mania": {"scrollSpeed": 28}},
                    "user": {
                        "id": 77, "name": AWKWARD, "country": "US", "accuracy": 97.5,
                        "rankedScore": 987654321.0, "playCount": 1234, "globalRank": 55, "totalPP": 8888.8
                    },
                    "beatmap": {"stats": stats},
                    "play": {
                        "failed": false, "playerName": AWKWARD, "mode": {"number": 0, "name": "osu"},
                        "score": 555555.0, "accuracyPercent": 96.25,
                        "healthBar": {"normal": 80.0, "smooth": 79.5},
                        "judgements": {"miss": 1, "meh": 2, "ok": 3, "good": 4, "great": 5, "perfect": 6},
                        "statistics": {"sliderBreaks": 7, "sliderEndHits": 8, "smallTickHit": 9, "largeTickHit": 10},
                        "hitErrors": {"all": [-12.5, 0.0, 4.25], "press": [1.0], "release": [2.0]},
                        "combo": {"current": 50, "max": 100},
                        "mods": mods_with_settings,
                        "rank": {"current": "S", "maxThisPlay": "SH"},
                        "pp": pp,
                        "unstableRate": 85.125
                    }
                }]
            }),
        );
        // Set vosu.skin /name to AWKWARD too.
        std::sync::Arc::make_mut(&mut selected.values)
            .iter_mut()
            .find(|e| e.ns == "vosu.skin" && e.pointer == "/name")
            .unwrap()
            .value = Some(Cbor::Text(AWKWARD.into()));

        let doc = assert_regular_three_way_parity(&selected);
        assert_eq!(doc["settings"]["skin"]["name"], AWKWARD);
        assert_eq!(doc["profile"]["matchmaking"]["rank"], Value::Null);
        assert_eq!(doc["room"]["roomID"], 999);
        assert_eq!(doc["tourney"]["bestOF"], 9);

        // Also test profile with id: Some + matchmaking: null -> emits `"matchmaking":null`.
        let mut profile: Value = view::decode(&selected, "/profile").unwrap();
        profile["matchmaking"] = Value::Null;
        fixture::replace(&mut selected, "/profile", profile);
        let doc2 = assert_regular_three_way_parity(&selected);
        assert!(
            doc2["profile"]
                .as_object()
                .unwrap()
                .contains_key("matchmaking")
        );
        assert_eq!(doc2["profile"]["matchmaking"], Value::Null);

        // Also test filtered projection through provider::Regular.
        let projected = provider::Regular.decode(selected.clone()).unwrap();
        let filters = [
            Filter::Field("room".into()),
            Filter::Nested {
                field: "profile".into(),
                keys: vec![
                    Filter::Field("name".into()),
                    Filter::Field("matchmaking".into()),
                ],
            },
        ];
        let view = regular::View::from_capture(&selected).unwrap();
        assert_eq!(
            projected.encode(&filters).unwrap(),
            regular::filtered(&view).encode(&filters).unwrap()
        );
    }
}

#[test]
fn edge_case_floats_and_integers_match_serde_js_formatter() {
    let source = fixture::source();
    let mut selected = fixture::capture(&source, &regular::selectors());
    replace_cbor(
        &mut selected,
        "/play/hitErrors/all",
        Cbor::Array(NUMBERS.iter().map(|&n| Cbor::Float(n)).collect()),
    );
    replace_cbor(&mut selected, "/play/score", Cbor::Float(-0.0));
    replace_cbor(
        &mut selected,
        "/play/accuracyPercent",
        Cbor::Integer(100.into()),
    );
    replace_cbor(&mut selected, "/play/unstableRate", Cbor::Float(f64::NAN));
    replace_cbor(&mut selected, "/session/playTime", Cbor::Float(1e21));
    replace_cbor(
        &mut selected,
        "/play/combo",
        Cbor::Map(vec![
            (Cbor::Text("current".into()), Cbor::Integer(i64::MIN.into())),
            (Cbor::Text("max".into()), Cbor::Integer(i64::MAX.into())),
        ]),
    );
    let doc = assert_regular_three_way_parity(&selected);
    assert_eq!(doc["play"]["score"], 0);
    assert_eq!(doc["play"]["unstableRate"], Value::Null);
    assert_eq!(doc["play"]["combo"]["current"], i64::MIN);
    assert_eq!(doc["play"]["combo"]["max"], i64::MAX);
}

#[test]
fn prefix_root_capture_and_missing_play_clock_match_serde() {
    let source = fixture::source();
    let full = fixture::capture(&source, &regular::selectors());
    // Collapse `/play/*` entries into one `/play` ancestor map to verify prefix fallback.
    let mut play_fields = Vec::new();
    let mut entries = Vec::new();
    for entry in full.values.iter() {
        if entry.ns == "vosu.osu" && entry.pointer == regular::PLAY_CLOCK {
            continue;
        }
        if entry.ns == "vosu.osu"
            && let Some(field) = entry.pointer.strip_prefix("/play/")
            && !field.contains('/')
        {
            play_fields.push((Cbor::Text(field.into()), entry.value.clone().unwrap()));
        } else {
            entries.push(entry.clone());
        }
    }
    entries.push(StateEntry {
        ns: "vosu.osu".into(),
        pointer: "/play".into(),
        value: Some(Cbor::Map(play_fields)),
        error: None,
    });
    let collapsed = CapturedSelection {
        values: entries.into(),
        ..full
    };
    let doc = assert_regular_three_way_parity(&collapsed);
    assert!(doc.pointer("/game/paused").is_none());
}

#[test]
fn direct_join_and_push_direct_join_match_on_all_edge_paths() {
    let cases = [
        (None, None),
        (Some(""), Some("")),
        (Some("folder"), None),
        (None, Some("file.osu")),
        (Some("folder"), Some("file.osu")),
        (Some("."), Some(".")),
        (Some(".."), Some("a.osu")),
        (Some("folder/../distinct-chart"), Some("a \" unicode🎵.osu")),
        (Some("a/b/c"), Some("../../d.mp3")),
        (Some("a\\b"), Some("c\\d.jpg")),
        (Some("/abs/dir"), Some("bg.png")),
        (Some("C:drive"), Some("file.osu")),
        (Some(AWKWARD), Some(AWKWARD)),
    ];
    for (folder, file) in cases {
        let owned = paths::direct_join(folder, file);
        let expected = serde_json::to_string(&owned).unwrap();
        let mut streamed = Vec::new();
        paths::push_direct_join(&mut streamed, folder, file);
        assert_eq!(
            String::from_utf8(streamed).unwrap(),
            expected,
            "folder={folder:?} file={file:?}"
        );
    }
}

#[test]
fn precise_direct_capture_encoder_matches_dto_and_serde() {
    let mut selected = fixture::capture(&fixture::source(), &precise::selectors());
    let slot = |n| json!({"k1":{"isPressed":true,"count":n},"k2":null,"m1":{"isPressed":false,"count":0},"m2":null});
    fixture::replace(
        &mut selected,
        "/tourney",
        json!({"clients":[
            {"ipcId":30.25,"keysOverlay":slot(31),"play":{"hitErrors":{"all":[32.0, -0.0]}}},
            {"ipcId":2.5,"keysOverlay":slot(41),"play":{"hitErrors":{"all":[]}}}
        ]}),
    );
    replace_cbor(
        &mut selected,
        "/play/hitErrors/all",
        Cbor::Array(NUMBERS.iter().map(|&n| Cbor::Float(n)).collect()),
    );
    assert_precise_three_way_parity(&selected);
    // Reversed entry order exercises general lookup.
    let mut reversed = selected.clone();
    std::sync::Arc::make_mut(&mut reversed.values).reverse();
    assert_precise_three_way_parity(&reversed);
}
