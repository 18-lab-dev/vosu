//! M2 exit: the real wire/store path against the live, oracle-pinned 1.x builders.
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    sync::{Arc, RwLock},
};
use vosu::api::ApiRuntimeSettings;
use vosu_core::state::{
    Audience, ClientFacts, InternalGrant, Pointer, PublisherGrant, SchemaRegistry, SessionKey,
    StateStore, StateValue, project_document, project_namespace, project_select,
};
use vosu_osu::project::{Caches, DiffSink, LastValues, Projector, SessionFacts};
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
use vosu_osu_state::StateBundleSnapshot;
use vosu_plugin_api::{PluginManifest, PublishBatchWriter, decode_publish_batch_ref};
#[path = "state_api_compat/oracle.rs"]
#[expect(
    dead_code,
    reason = "shared oracle also serves the complete 1.x wire suite"
)]
mod oracle;

const NAMESPACES: [&str; 6] = [
    "vosu.chart",
    "vosu.client",
    "vosu.osu",
    "vosu.play",
    "vosu.skin",
    "vosu.status",
];
const DIVERGENCES: &str = include_str!("fixtures/upstream/DIVERGENCES.md");

fn exemptions(text: &str) -> BTreeMap<&str, &str> {
    let mut entries = BTreeMap::new();
    for line in text.lines().filter(|line| line.starts_with('|')) {
        let cells: Vec<_> = line.trim_matches('|').split('|').map(str::trim).collect();
        assert_eq!(cells.len(), 5, "malformed divergence row: {line}");
        if cells[0] == "id" || cells[0].starts_with('-') {
            continue;
        }
        assert!(
            matches!(
                cells[1],
                "security" | "contract" | "licensing" | "temporary"
            ),
            "unknown divergence class: {line}"
        );
        assert!(
            cells.iter().all(|cell| !cell.is_empty()),
            "incomplete divergence row"
        );
        assert!(
            entries.insert(cells[0], cells[1]).is_none(),
            "duplicate divergence id"
        );
    }
    entries
}
fn require(entries: &BTreeMap<&str, &str>, id: &str, class: &str) {
    assert_eq!(
        entries.get(id).copied(),
        Some(class),
        "unrecorded compatibility exemption {id}"
    );
}
fn load_input(path: &str) -> oracle::Input {
    serde_json::from_slice(&fs::read(oracle::fixture_root().join(path)).unwrap()).unwrap()
}
fn facts(input: &oracle::Input) -> SessionFacts {
    SessionFacts {
        client: if input.source.as_deref() == Some("lazer") {
            ClientType::Lazer
        } else {
            ClientType::Stable
        },
        foreground: Some(true),
        settings: InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: None,
        },
    }
}
struct Bus {
    registry: Arc<RwLock<SchemaRegistry>>,
    store: StateStore,
    grant: PublisherGrant,
    projector: Projector,
    caches: Caches,
    last: LastValues,
    key: SessionKey,
}
impl Bus {
    fn new() -> Self {
        let mut registry = SchemaRegistry::with_core().unwrap();
        registry
            .register("vosu.osu", "vosu.osu", vosu_osu::OSU_NAMESPACE_SCHEMA)
            .unwrap();
        let registry = Arc::new(RwLock::new(registry));
        Self {
            store: StateStore::new(registry.clone()),
            registry,
            grant: PublisherGrant::from_manifest(
                &PluginManifest::parse(vosu_osu::MANIFEST).unwrap(),
            )
            .unwrap(),
            projector: Projector::default(),
            last: LastValues::default(),
            caches: Caches::default(),
            key: SessionKey(10),
        }
    }
    fn open(&self, input: &oracle::Input, pid: u32) {
        let manifest = PluginManifest::parse(vosu_osu::MANIFEST).unwrap();
        self.store.open_session(
            SessionKey(u64::from(pid)),
            &self.grant,
            ClientFacts {
                game: "osu".into(),
                variant: input.source.clone().unwrap(),
                name: "osu!".into(),
                version: String::new(),
                pid,
                session: format!("oracle-{pid}"),
                plugin_id: manifest.id,
                plugin_version: manifest.version,
            },
            pid == 10,
        );
    }
    fn publish(&mut self, snapshot: &StateBundleSnapshot, facts: &SessionFacts) -> usize {
        let mut writer = PublishBatchWriter::new();
        writer.begin(self.key.0);
        self.projector.project(
            snapshot,
            facts,
            &mut self.caches,
            &mut DiffSink::new(&mut self.last, &mut writer),
        );
        let count = writer.len();
        let mut bytes = Vec::new();
        writer.finish_into(&mut bytes).unwrap();
        let batch = decode_publish_batch_ref(&bytes).unwrap();
        if count != 0 {
            self.store
                .commit(&self.grant, self.key, batch.entries())
                .unwrap();
        }
        count
    }
    fn document(&self, audience: Audience<'_>) -> Value {
        let state = self.store.current(self.key).unwrap();
        let registry = self.registry.read().unwrap();
        Value::Object(
            project_document(&registry, &state, audience)
                .into_iter()
                .map(|(key, value)| (key.to_string(), as_json(&value)))
                .collect(),
        )
    }
}
fn as_json(value: &StateValue) -> Value {
    let mut bytes = Vec::new();
    value.write_json(&mut bytes);
    serde_json::from_slice(&bytes).unwrap()
}
fn schemas() -> BTreeMap<String, jsonschema::Validator> {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("docs/v2/schemas");
    let common: Value =
        serde_json::from_slice(&fs::read(root.join("common-defs.schema.json")).unwrap()).unwrap();
    NAMESPACES
        .into_iter()
        .map(|ns| {
            let schema: Value =
                serde_json::from_slice(&fs::read(root.join(format!("{ns}.schema.json"))).unwrap())
                    .unwrap();
            let resources = jsonschema::Registry::new()
                .add(
                    "https://vosu.dev/schemas/v2/common-defs.schema.json",
                    common.clone(),
                )
                .unwrap()
                .prepare()
                .unwrap();
            let validator = jsonschema::options()
                .with_registry(&resources)
                .build(&schema)
                .unwrap();
            (ns.into(), validator)
        })
        .collect()
}
// JSON numbers model JavaScript numbers, not serde_json's i64/u64/f64 variants.
fn equal(case: &str, path: &str, old: &Value, new: &Value) {
    match (old, new) {
        (Value::Number(a), Value::Number(b)) => assert_eq!(
            a.as_f64(),
            b.as_f64(),
            "{case} {path}: 1.x={old}, 2.0={new}"
        ),
        (Value::Array(a), Value::Array(b)) => {
            assert_eq!(a.len(), b.len(), "{case} {path}: 1.x={old}, 2.0={new}");
            for (i, (a, b)) in a.iter().zip(b).enumerate() {
                equal(case, &format!("{path}/{i}"), a, b);
            }
        }
        (Value::Object(a), Value::Object(b)) => {
            // §7 Eq: an absent 1.x member equals a 2.0 null.
            for key in a.keys().chain(b.keys()).collect::<BTreeSet<_>>() {
                equal(
                    case,
                    &format!("{path}/{key}"),
                    a.get(key).unwrap_or(&Value::Null),
                    b.get(key).unwrap_or(&Value::Null),
                );
            }
        }
        _ => assert_eq!(old, new, "{case} {path}: 1.x={old}, 2.0={new}"),
    }
}
fn at<'a>(value: &'a Value, pointer: &str) -> &'a Value {
    value.pointer(pointer).unwrap_or(&Value::Null)
}
fn row(case: &str, old: &Value, from: &str, new: &Value, to: &str) {
    equal(case, to, at(old, from), at(new, to));
}
fn counts(hits: &Value) -> Value {
    let mut result = json!({});
    for (old, new) in [
        ("geki", "perfect"),
        ("300", "great"),
        ("katu", "good"),
        ("100", "ok"),
        ("50", "meh"),
        ("0", "miss"),
    ] {
        result[new] = hits[old].clone();
    }
    result
}
fn renamed_hits(value: &mut Value) {
    match value {
        Value::Object(object) => {
            if let Some(hits) = object.remove("hits") {
                let mut stats = counts(&hits);
                for key in [
                    "sliderEndHits",
                    "smallTickHits",
                    "largeTickHits",
                    "sliderBreaks",
                ] {
                    if let Some(value) = hits.get(key) {
                        stats[key] = value.clone();
                    }
                }
                object.insert("statistics".into(), stats);
            }
            for value in object.values_mut() {
                renamed_hits(value);
            }
        }
        Value::Array(values) => {
            for value in values {
                renamed_hits(value);
            }
        }
        _ => {}
    }
}
fn parity(
    input: &oracle::Input,
    snapshot: &StateBundleSnapshot,
    v2: &Value,
    precise: &Value,
    doc: &Value,
    private: &Value,
    entries: &BTreeMap<&str, &str>,
) {
    let id = &input.id;
    for (from, to) in [
        ("/game/paused", "/vosu.osu/session/paused"),
        ("/session/playTime", "/vosu.osu/session/playTime"),
        ("/session/playCount", "/vosu.osu/session/playCount"),
        ("/state", "/vosu.osu/session/state"),
        ("/rankedPlay", "/vosu.osu/rankedPlay"),
        ("/profile", "/vosu.osu/profile"),
        ("/room", "/vosu.osu/room"),
        ("/client", "/vosu.client/variant"),
        ("/server", "/vosu.osu/session/server"),
    ] {
        row(id, v2, from, doc, to);
    }
    assert_eq!(
        v2["profile"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<BTreeSet<_>>(),
        doc["vosu.osu"]["profile"]
            .as_object()
            .unwrap()
            .keys()
            .collect()
    );
    #[cfg(windows)]
    row(id, v2, "/game/focused", doc, "/vosu.osu/session/focused");
    #[cfg(not(windows))]
    {
        assert!(v2.pointer("/game/focused").is_none());
        assert!(doc["vosu.osu"]["session"]["focused"].is_null());
    }
    let status = match input.status {
        2 if v2["game"]["paused"] == true => json!("pause"),
        2 => json!("play"),
        4 | 5 | 6 | 11 | 12 | 13 | 15 => json!("selection"),
        7 | 14 | 17 | 18 => json!("results"),
        10 => Value::Null,
        _ => json!("menu"),
    };
    equal(
        id,
        "/vosu.status/status",
        &status,
        &doc["vosu.status"]["status"],
    );
    for key in [
        "isKiai",
        "isBreak",
        "isConvert",
        "time",
        "status",
        "checksum",
        "id",
        "set",
        "mode",
        "artist",
        "artistUnicode",
        "title",
        "titleUnicode",
        "mapper",
        "version",
        "source",
        "tags",
        "stats",
    ] {
        row(
            id,
            v2,
            &format!("/beatmap/{key}"),
            doc,
            &format!("/vosu.osu/beatmap/{key}"),
        );
    }
    for (old, new) in [
        ("artist", "artist"),
        ("title", "title"),
        ("mapper", "charter"),
        ("version", "difficultyName"),
    ] {
        row(
            id,
            v2,
            &format!("/beatmap/{old}"),
            doc,
            &format!("/vosu.chart/{new}"),
        );
    }
    row(
        id,
        v2,
        "/beatmap/time/lastObject",
        doc,
        "/vosu.chart/length",
    );
    // The synthetic oracle map is built at rate 1; derive the same rounded base
    // summary independently of the projector and the rate-adjusted 1.x wire.
    let bpm = snapshot
        .regular
        .beatmap
        .parsed
        .as_ref()
        .map_or(0.0, |parsed| parsed.bpm_base.common);
    equal(
        id,
        "/vosu.chart/bpm",
        &json!(vosu_osu_model::fix_decimals_to(bpm, 4).unwrap()),
        &doc["vosu.chart"]["bpm"],
    );
    let key_count = if v2["beatmap"]["mode"]["number"] == 3 {
        json!(
            v2["beatmap"]["stats"]["cs"]["converted"]
                .as_f64()
                .unwrap()
                .round()
        )
    } else {
        Value::Null
    };
    equal(
        id,
        "/vosu.chart/keyCount",
        &key_count,
        &doc["vosu.chart"]["keyCount"],
    );
    if id == "lazer-play-mania-cl" {
        equal(
            id,
            "mania key count",
            &json!(precise["keys"]["all"].as_array().unwrap().len()),
            &key_count,
        );
    }
    for key in [
        "source",
        "failed",
        "playerName",
        "mode",
        "healthBar",
        "mods",
        "rank",
        "pp",
        "unstableRate",
        "pressHitErrors",
        "releaseHitErrors",
        "hitOffsets",
        "keyHitErrors",
    ] {
        let to = match key {
            "pressHitErrors" => "hitErrors/press",
            "releaseHitErrors" => "hitErrors/release",
            other => other,
        };
        row(
            id,
            v2,
            &format!("/play/{key}"),
            doc,
            &format!("/vosu.osu/play/{to}"),
        );
    }
    for (old, new) in [
        ("sliderBreaks", "sliderBreaks"),
        ("sliderEndHits", "sliderEndHits"),
        ("smallTickHits", "smallTickHit"),
        ("largeTickHits", "largeTickHit"),
        ("katu", "good"),
    ] {
        row(
            id,
            v2,
            &format!("/play/hits/{old}"),
            doc,
            &format!("/vosu.osu/play/statistics/{new}"),
        );
    }
    row(
        id,
        v2,
        "/play/hitErrorArray",
        doc,
        "/vosu.osu/play/hitErrors/all",
    );
    for (old, new) in [
        ("hitErrors", "hitErrors/all"),
        ("pressHitErrors", "hitErrors/press"),
        ("releaseHitErrors", "hitErrors/release"),
        ("hitOffsets", "hitOffsets"),
        ("keyHitErrors", "keyHitErrors"),
        ("catcher", "catcher"),
    ] {
        row(
            id,
            precise,
            &format!("/{old}"),
            doc,
            &format!("/vosu.osu/play/{new}"),
        );
    }
    assert_eq!(
        doc["vosu.osu"]["keys"]
            .as_object()
            .unwrap()
            .keys()
            .collect::<Vec<_>>(),
        ["all"]
    );
    row(id, precise, "/keys/all", doc, "/vosu.osu/keys/all");
    let playing = matches!(status.as_str(), Some("play" | "pause"));
    for (from, to) in [
        ("score", "score"),
        ("combo/current", "combo"),
        ("combo/max", "maxCombo"),
        ("accuracy", "accuracy"),
        ("healthBar/normal", "health"),
        ("mode/number", "accuracyKind"),
        ("../rate", "rate"),
    ] {
        let mut expected = if from == "../rate" {
            v2["beatmap"]["time"]["rate"].clone()
        } else {
            at(v2, &format!("/play/{from}")).clone()
        };
        if !playing {
            expected = Value::Null;
        } else if matches!(to, "accuracy" | "health") {
            expected = json!(expected.as_f64().unwrap() / 100.0);
        } else if to == "accuracyKind" {
            expected =
                json!(["osu-standard", "osu-taiko", "osu-catch", "osu-mania"][input.mode as usize]);
        }
        equal(
            id,
            &format!("/vosu.play/{to}"),
            &expected,
            &doc["vosu.play"][to],
        );
    }
    let last = v2["beatmap"]["time"]["lastObject"].as_f64().unwrap();
    let progress = if playing && last > 0.0 {
        json!((v2["beatmap"]["time"]["live"].as_f64().unwrap() / last).clamp(0.0, 1.0))
    } else {
        Value::Null
    };
    equal(
        id,
        "/vosu.play/progress",
        &progress,
        &doc["vosu.play"]["progress"],
    );
    let judgements = if playing {
        Value::Array([("perfect", "Perfect", "geki"), ("great", "Great", "300"), ("good", "Good", "katu"), ("ok", "Ok", "100"), ("meh", "Meh", "50"), ("miss", "Miss", "0")].into_iter().enumerate().map(|(ordinal, (name, display, key))| json!({"id": name, "displayName": display, "count": v2["play"]["hits"][key], "ordinal": ordinal})).collect())
    } else {
        Value::Null
    };
    equal(
        id,
        "/vosu.play/judgements",
        &judgements,
        &doc["vosu.play"]["judgements"],
    );
    let mut leaderboard = v2["leaderboard"].clone();
    renamed_hits(&mut leaderboard);
    equal(
        id,
        "/vosu.osu/leaderboard",
        &leaderboard,
        &doc["vosu.osu"]["leaderboard"],
    );
    row(
        id,
        v2,
        "/performance/accuracy",
        doc,
        "/vosu.osu/performance/accuracy",
    );
    row(
        id,
        v2,
        "/performance/graph",
        doc,
        "/vosu.osu/performance/graph",
    );
    let mut result = v2["resultsScreen"].clone();
    renamed_hits(&mut result);
    require(entries, "result.statistics.sliderBreaks", "contract");
    assert!(result["statistics"].get("sliderBreaks").is_none());
    result["statistics"]["sliderBreaks"] = json!(0);
    equal(id, "/vosu.osu/result", &result, &doc["vosu.osu"]["result"]);
    let mut settings = v2["settings"].clone();
    let name = settings["skin"]
        .as_object_mut()
        .unwrap()
        .remove("name")
        .unwrap();
    equal(
        id,
        "/vosu.osu/settings",
        &settings,
        &doc["vosu.osu"]["settings"],
    );
    equal(id, "/vosu.skin/name", &name, &doc["vosu.skin"]["name"]);
    assert_eq!(doc["vosu.chart"]["resources"], json!({}));
    assert!(doc["vosu.skin"]["tree"].is_null());
    let variant = if input.source.as_deref() == Some("lazer") {
        "lazer"
    } else {
        "stable"
    };
    let (game, songs) = if variant == "stable" {
        ("game", "songs")
    } else {
        ("data", "files")
    };
    for (old, new) in [("game", game), ("songs", songs)] {
        // state-schema.md:402-403: unknown internal install paths are null,
        // whereas 1.x represented the same unknown path as an empty string.
        let old_path = &v2["folders"][old];
        let expected = if old_path == "" {
            &Value::Null
        } else {
            old_path
        };
        equal(
            id,
            &format!("/vosu.osu/internal/paths/{variant}/{new}"),
            expected,
            at(
                private,
                &format!("/vosu.osu/internal/paths/{variant}/{new}"),
            ),
        );
    }
    assert!(
        private["vosu.osu"]["internal"]["paths"][if variant == "stable" {
            "lazer"
        } else {
            "stable"
        }]
        .is_null()
    );
    tourney(id, snapshot, v2, precise, doc, entries);
}
fn tourney(
    id: &str,
    snapshot: &StateBundleSnapshot,
    v2: &Value,
    precise: &Value,
    doc: &Value,
    entries: &BTreeMap<&str, &str>,
) {
    let mut expected = v2["tourney"].clone();
    if id == "stable-tournament" {
        require(entries, "stable-tournament/tourney", "temporary");
        expected = json!({"scoreVisible": false, "starsVisible": false, "ipcState": 0, "bestOF": 0, "team": {"left": "", "right": ""}, "points": {"left": 0, "right": 0}, "chat": [], "totalScore": {"left": 0, "right": 0}, "clients": []});
    } else {
        assert_eq!(precise["tourney"], json!([]));
    }
    if let Some(object) = expected.as_object_mut() {
        require(entries, "tourney-bestOf-rename", "contract");
        let value = object.remove("bestOF").unwrap();
        object.insert("bestOf".into(), value);
        for (index, client) in object["clients"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            let play = client["play"].as_object_mut().unwrap();
            let errors = play.remove("hitErrorArray").unwrap();
            let press = play.remove("pressHitErrors").unwrap();
            let release = play.remove("releaseHitErrors").unwrap();
            play.insert(
                "hitErrors".into(),
                json!({"all": errors, "press": press, "release": release}),
            );
            let hits = play.remove("hits").unwrap();
            play.insert("judgements".into(), counts(&hits));
            // The schema's additional native statistics have no 1.x wire
            // counterparts; retain the input counts while checking every
            // 1.x tick/slider count through its documented renamed member.
            let score = snapshot
                .regular
                .lazer_multi_spectating
                .data
                .as_ref()
                .unwrap()
                .spectating_clients[index]
                .score
                .as_ref()
                .unwrap();
            play.insert(
                "statistics".into(),
                native_statistics(&score.statistics, &hits),
            );
        }
    }
    equal(
        id,
        "/vosu.osu/tourney",
        &expected,
        &doc["vosu.osu"]["tourney"],
    );
}
fn native_statistics(s: &vosu_osu_model::states::Statistics, hits: &Value) -> Value {
    json!({
        "smallTickMiss":s.small_tick_miss, "smallTickHit":hits["smallTickHits"],
        "largeTickMiss":s.large_tick_miss, "largeTickHit":hits["largeTickHits"],
        "smallBonus":s.small_bonus, "largeBonus":s.large_bonus,
        "ignoreMiss":s.ignore_miss, "ignoreHit":s.ignore_hit, "comboBreak":s.combo_break,
        "sliderTailHit":hits["sliderEndHits"], "sliderBreaks":hits["sliderBreaks"],
        "sliderEndHits":hits["sliderEndHits"], "good":hits["katu"],
        "legacyComboIncrease":s.legacy_combo_increase,
    })
}
fn strings<'a>(value: &'a Value, out: &mut Vec<&'a str>) {
    match value {
        Value::String(s) if !s.is_empty() => out.push(s),
        Value::Array(a) => {
            for v in a {
                strings(v, out);
            }
        }
        Value::Object(o) => {
            for v in o.values() {
                strings(v, out);
            }
        }
        _ => {}
    }
}
fn privacy(bus: &Bus, v2: &Value, doc: &Value) {
    assert!(doc["vosu.osu"].get("internal").is_none());
    let state = bus.store.current(bus.key).unwrap();
    let registry = bus.registry.read().unwrap();
    let namespace = state.namespace("vosu.osu").unwrap();
    let whole = as_json(&project_namespace(
        &registry,
        "vosu.osu",
        namespace,
        Audience::Api,
    ));
    assert!(whole.get("internal").is_none());
    equal(
        "privacy",
        "namespace root selection",
        &whole,
        &as_json(
            &project_select(
                &registry,
                &state,
                "vosu.osu",
                &Pointer::parse("").unwrap(),
                Audience::Api,
            )
            .unwrap(),
        ),
    );
    assert!(
        project_select(
            &registry,
            &state,
            "vosu.osu",
            &Pointer::parse("/internal").unwrap(),
            Audience::Api
        )
        .is_none()
    );
    for _ in 0..2 {
        equal(
            "privacy",
            "encoding cache",
            doc,
            &serde_json::from_slice(state.api_json(&registry)).unwrap(),
        );
    }
    let mut paths = Vec::new();
    for key in ["folders", "files", "directPath"] {
        strings(&v2[key], &mut paths);
    }
    let mut public = Vec::new();
    strings(doc, &mut public);
    for path in paths {
        assert!(!public.contains(&path), "API leaks 1.x path {path}");
    }
}
#[test]
fn oracle_cases_validate_and_match_the_state_bus() {
    let inventory: oracle::Inventory =
        serde_json::from_slice(&fs::read(oracle::fixture_root().join("inventory.json")).unwrap())
            .unwrap();
    let entries = exemptions(DIVERGENCES);
    let validators = schemas();
    let baseline = load_input("stable-defaults/input.json");
    let full = InternalGrant::from_subtrees(vec![Pointer::parse("/internal").unwrap()]);
    for case in inventory.cases {
        let input = load_input(&case.input);
        assert_eq!(input.id, case.id);
        let mut bus = Bus::new();
        if input.source.is_none() {
            assert!(bus.store.current(bus.key).is_none());
            continue;
        }
        bus.open(&input, 10);
        let snapshot = oracle::state(&input);
        let facts = facts(&input);
        assert!(bus.publish(&snapshot, &facts) > 0);
        let doc = bus.document(Audience::Api);
        let private = bus.document(Audience::Plugin(&full));
        for document in [&doc, &private] {
            assert_eq!(
                document
                    .as_object()
                    .unwrap()
                    .keys()
                    .map(String::as_str)
                    .collect::<BTreeSet<_>>(),
                NAMESPACES.into_iter().collect()
            );
            for (ns, validator) in &validators {
                if let Err(error) = validator.validate(&document[ns]) {
                    panic!("{} {ns}: {error}", input.id);
                }
            }
        }
        equal(
            &input.id,
            "client facts",
            &json!({"game":"osu", "variant":input.source, "name":"osu!", "version":"", "pid":10, "session":"oracle-10", "focused":true, "plugin":{"id":"vosu.osu", "version":PluginManifest::parse(vosu_osu::MANIFEST).unwrap().version}}),
            &doc["vosu.client"],
        );
        let manager = oracle::manager_with(
            &input,
            ApiRuntimeSettings {
                enable_key_overlay: true,
                enable_mania_key_hit_errors: true,
                enable_hit_offsets: true,
            },
        );
        let v2: Value = serde_json::from_slice(&oracle::encode("v2", &manager)).unwrap();
        let precise: Value =
            serde_json::from_slice(&oracle::encode("v2-precise", &manager)).unwrap();
        parity(&input, &snapshot, &v2, &precise, &doc, &private, &entries);
        privacy(&bus, &v2, &doc);
        let revision = bus.store.current(bus.key).unwrap().revision;
        assert_eq!(
            bus.publish(&snapshot, &facts),
            0,
            "{} not idempotent",
            input.id
        );
        assert_eq!(bus.store.current(bus.key).unwrap().revision, revision);
        let mut diff = Bus::new();
        diff.open(&input, 10);
        diff.publish(&oracle::state(&baseline), &self::facts(&baseline));
        diff.publish(&snapshot, &facts);
        equal(
            &input.id,
            "diff convergence",
            &private,
            &diff.document(Audience::Plugin(&full)),
        );
        assert!(doc["vosu.osu"]["session"]["ipcId"].is_null());
        if input.tournament.as_deref() == Some("stable") {
            for (pid, ipc) in [(30, 1.0), (40, 2.0)] {
                let instance = manager.manager().instances().get(&pid).unwrap();
                assert!(instance.settings().tournament_spectator);
                assert_eq!(instance.settings().tournament_ipc_id, Some(ipc));
                let mut spectator = Bus::new();
                spectator.key = SessionKey(u64::from(instance.pid()));
                spectator.open(&input, instance.pid());
                let settings = instance.settings().clone();
                spectator.publish(
                    instance.state(),
                    &SessionFacts {
                        settings: settings.clone(),
                        ..self::facts(&input)
                    },
                );
                equal(
                    &input.id,
                    "spectator ipcId",
                    &json!(settings.tournament_ipc_id),
                    &spectator.document(Audience::Api)["vosu.osu"]["session"]["ipcId"],
                );
            }
        }
    }
}
#[test]
fn missing_tourney_score_uses_only_the_recorded_contract_exemption() {
    let entries = exemptions(DIVERGENCES);
    require(&entries, "tourney-null-without-score", "contract");
    let input = load_input("lazer-lobby-tournament/input.json");
    let mut snapshot = oracle::state(&input).as_ref().clone();
    Arc::make_mut(&mut snapshot.regular)
        .lazer_multi_spectating
        .data
        .as_mut()
        .unwrap()
        .spectating_clients[0]
        .score = None;
    let mut bus = Bus::new();
    bus.open(&input, 10);
    bus.publish(&snapshot, &facts(&input));
    assert!(bus.document(Audience::Api)["vosu.osu"]["tourney"].is_null());
    let manager =
        vosu::api::CombinedSnapshot::from_manager(Arc::new(vosu::api::ManagerSnapshot::new(
            1,
            Some(10),
            true,
            ApiRuntimeSettings::default(),
            [vosu::api::ManagerInstanceSnapshot::new(
                10,
                facts(&input).settings,
                false,
                Arc::new(snapshot),
            )],
        )));
    assert!(matches!(
        vosu::api::v2::encode(&manager),
        Err(vosu::api::ApiError::Serialization(_))
    ));
}
#[test]
fn comparison_and_exemption_guards_reject_unlisted_differences() {
    assert!(std::panic::catch_unwind(|| equal("guard", "/n", &json!(1), &json!(2))).is_err());
    assert!(
        std::panic::catch_unwind(|| equal("guard", "/keys", &json!({"a":1}), &json!({}))).is_err()
    );
    equal("guard", "/null", &json!({}), &json!({"a":null}));
    for id in [
        "stable-tournament/tourney",
        "tourney-bestOf-rename",
        "tourney-null-without-score",
        "result.statistics.sliderBreaks",
    ] {
        let mut entries = exemptions(DIVERGENCES);
        let class = entries.remove(id).unwrap();
        assert!(std::panic::catch_unwind(|| require(&entries, id, class)).is_err());
    }
    assert!(
        std::panic::catch_unwind(|| exemptions("| x | unknown | scope | reason | task |")).is_err()
    );
}
