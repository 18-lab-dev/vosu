//! Nonzero, distinct seeded inputs traverse the same producer/ABI/bus/adapter
//! path as the pinned corpus. They must change every schema's bytes, and their
//! vosu-only extension facts must stay on the bus while every upstream wire,
//! including nested tournament copies and embedded SC JSON strings, omits them.
use super::{NOT_READY_CORPUS, Value, bus, json, oracle, ready, seeds};
use std::fs;

const SCHEMAS: [&str; 4] = ["v1", "v2", "v2-precise", "sc"];
/// Names that exist only as vosu extensions; no pinned upstream schema has them
/// at any depth, so a raw byte match also covers SC's embedded JSON strings.
const EXTENSION_NAMES: [&str; 7] = [
    "pressHitErrors",
    "releaseHitErrors",
    "keyHitErrors",
    "hitOffsets",
    "press_hit_errors",
    "release_hit_errors",
    "catcher",
];

fn walk(value: &Value, path: &mut Vec<String>, out: &mut Vec<Vec<String>>) {
    match value {
        Value::Object(object) => {
            for (key, child) in object {
                path.push(key.clone());
                out.push(path.clone());
                walk(child, path, out);
                path.pop();
            }
        }
        Value::Array(items) => {
            for item in items {
                path.push("*".into());
                walk(item, path, out);
                path.pop();
            }
        }
        _ => {}
    }
}

/// Extension member paths present in one emitted wire document.
pub(super) fn leaks(schema: &str, bytes: &[u8]) -> Vec<String> {
    let text = String::from_utf8_lossy(bytes);
    let mut found: Vec<String> = EXTENSION_NAMES
        .iter()
        .filter(|name| text.contains(*name))
        .map(|name| format!("raw:{name}"))
        .collect();
    let wire: Value = serde_json::from_slice(bytes).expect("adapter emitted JSON");
    let mut paths = Vec::new();
    walk(&wire, &mut Vec::new(), &mut paths);
    for path in paths {
        let ends = |suffix: &[&str]| {
            path.len() >= suffix.len()
                && path[path.len() - suffix.len()..]
                    .iter()
                    .zip(suffix)
                    .all(|(a, b)| a == b)
        };
        let logical_keys = ends(&["keys", "all"]);
        let regular_keys = schema == "v2"
            && (path == ["keys"] || path.as_slice() == ["tourney", "clients", "*", "keys"]);
        let clock = schema == "v2" && (ends(&["time", "rate"]) || ends(&["time", "running"]));
        let source = schema == "v2" && ends(&["play", "source"]);
        if logical_keys || regular_keys || clock || source {
            found.push(path.join("/"));
        }
    }
    found
}

fn seeded(original: &oracle::Input) -> oracle::Input {
    let mut input = seeds::seeded(original);
    // Accurate clock and spectator facts the v2 wire no longer carries.
    input.services.global["render"] =
        json!({"spectating": true, "clock": {"rate": 1.37, "running": false}});
    input
}

fn non_empty(replay: &oracle::Replay, pointer: &str) -> Value {
    let value = bus::fact(replay, "vosu.osu", pointer)
        .unwrap_or_else(|| panic!("bus fact {pointer} must be committed"));
    assert!(
        !(value.is_null() || value.as_array().is_some_and(Vec::is_empty)),
        "positive control {pointer} must be a nonempty source fact: {value}"
    );
    value
}

#[test]
fn seeded_sources_change_every_schema_while_extensions_stay_off_the_wire() {
    let root = oracle::fixture_root();
    let inventory: oracle::Inventory =
        serde_json::from_slice(&fs::read(root.join("inventory.json")).unwrap()).unwrap();
    let mut sourced = 0;
    for case in inventory.cases {
        let original: oracle::Input =
            serde_json::from_slice(&fs::read(root.join(&case.input)).unwrap()).unwrap();
        if original.source.is_none() {
            continue;
        }
        sourced += 1;
        let input = seeded(&original);
        let replay = oracle::replay(&input);
        bus::published(&replay, &input);
        // Positive controls: every extension source exists on the bus.
        assert_eq!(non_empty(&replay, "/play/hitErrors/press"), json!([3.75]));
        assert_eq!(non_empty(&replay, "/play/hitErrors/release"), json!([-4.5]));
        if input.status == 2 && input.mode == 0 {
            // Offsets are a standard-mode gameplay source fact only.
            non_empty(&replay, "/play/hitOffsets");
        }
        assert_eq!(non_empty(&replay, "/beatmap/time/rate"), json!(1.37));
        assert_eq!(
            bus::fact(&replay, "vosu.osu", "/beatmap/time/running"),
            Some(json!(false))
        );
        if input.status == 2 {
            assert_eq!(non_empty(&replay, "/play/source"), json!("spectator"));
        }
        if input.mode == 3 {
            assert_eq!(non_empty(&replay, "/keys/all").as_array().unwrap().len(), 4);
            non_empty(&replay, "/play/keyHitErrors/press");
        }
        if input.mode == 2 {
            non_empty(&replay, "/play/catcher");
        }
        // The state-schema rename is a bus fact only; the wire keeps roomID.
        assert_eq!(non_empty(&replay, "/room/roomId"), json!(314));
        let install = format!("/compat-private/{}/install", input.id);
        for schema in SCHEMAS {
            let bytes = ready(&input.id, schema, replay.fixture.outcome(schema));
            let pinned = fs::read(root.join(&case.expected[schema])).unwrap();
            assert_ne!(
                bytes, pinned,
                "{}/{schema} ignored its seeded bus input",
                input.id
            );
            assert_ne!(bytes, NOT_READY_CORPUS);
            assert_eq!(
                leaks(schema, &bytes),
                Vec::<String>::new(),
                "{}/{schema} leaked an extension",
                input.id
            );
            assert!(!String::from_utf8_lossy(&bytes).contains("roomId"));
            if schema == "v2" {
                let wire: Value = serde_json::from_slice(&bytes).unwrap();
                assert_eq!(wire["room"]["roomID"], 314, "{}", input.id);
                assert_eq!(wire["folders"]["game"], install.as_str());
            }
        }
    }
    assert_eq!(sourced, 12);
}

#[test]
fn extension_detector_rejects_each_removed_member_and_spares_upstream_names() {
    for (schema, leaking) in [
        ("v2", json!({"keys": {"k1": {}}})),
        ("v2", json!({"beatmap": {"time": {"live": 1, "rate": 1.5}}})),
        ("v2", json!({"beatmap": {"time": {"running": true}}})),
        ("v2", json!({"play": {"source": "player"}})),
        (
            "v2",
            json!({"tourney": {"clients": [{"play": {"source": "x"}}]}}),
        ),
        ("v2", json!({"tourney": {"clients": [{"keys": {}}]}})),
        ("v2", json!({"play": {"hitOffsets": [1]}})),
        ("v2-precise", json!({"keys": {"k1": null, "all": []}})),
        ("v2-precise", json!({"tourney": [{"keys": {"all": []}}]})),
        ("v2-precise", json!({"tourney": [{"pressHitErrors": []}]})),
        ("v2-precise", json!({"catcher": null})),
        (
            "v1",
            json!({"tourney": {"ipcClients": [{"gameplay": {"hits": {"releaseHitErrors": []}}}]}}),
        ),
        ("sc", json!({"keyOverlay": "{\"keyHitErrors\":[]}"})),
        ("sc", json!({"press_hit_errors": []})),
    ] {
        let bytes = serde_json::to_vec(&leaking).unwrap();
        assert!(!leaks(schema, &bytes).is_empty(), "{schema}: {leaking}");
    }
    for (schema, legitimate) in [
        (
            "v2-precise",
            json!({"keys": {"k1": {}, "k2": {}, "m1": {}, "m2": {}}, "hitErrors": []}),
        ),
        (
            "v2-precise",
            json!({"tourney": [{"ipcId": 1, "keys": {"k1": {}}, "hitErrors": []}]}),
        ),
        (
            "v2",
            json!({"play": {"hitErrorArray": [], "mods": {"rate": 1.5}}, "beatmap": {"time": {"live": 1}}}),
        ),
        (
            "v1",
            json!({"gameplay": {"keyOverlay": {"k1": {}}, "hits": {"hitErrorArray": []}}}),
        ),
    ] {
        let bytes = serde_json::to_vec(&legitimate).unwrap();
        assert!(leaks(schema, &bytes).is_empty(), "{schema}: {legitimate}");
    }
}
