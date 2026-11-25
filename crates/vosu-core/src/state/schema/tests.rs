use std::time::{Duration, Instant};

use serde_json::{Value, json};

use super::*;

// Preserve the original K1 regression corpus through the SDK compiler.
mod walker {
    use super::*;
    pub struct Walked {
        pub lanes: Vec<(Vec<Arc<str>>, LaneBits)>,
        pub required_top_level: Vec<Arc<str>>,
        pub hidden: Vec<Pointer>,
    }
    pub fn walk(
        namespace: &str,
        owner: &SchemaOwner,
        text: &str,
        common: &Value,
    ) -> Result<Walked, SchemaError> {
        use vosu_plugin_api::manifest::schema::{self, SchemaProvenance};
        let root: Value =
            serde_json::from_str(text).map_err(|e| SchemaError::Json(e.to_string()))?;
        let version = root["version"].as_str().unwrap_or("1.0.0").parse().unwrap();
        let provenance = if *owner == SchemaOwner::Core {
            SchemaProvenance::TrustedOfficial
        } else {
            SchemaProvenance::Untrusted
        };
        let compiled = schema::compile(namespace, &version, provenance, text, common)?;
        Ok(Walked {
            required_top_level: compiled
                .required_top_level()
                .iter()
                .map(|s| Arc::from(s.as_str()))
                .collect(),
            hidden: compiled
                .hidden()
                .iter()
                .map(|s| Pointer::parse(s).unwrap())
                .collect(),
            lanes: compiled
                .lanes()
                .iter()
                .map(|row| {
                    let pointer = Pointer::parse(&row.pointer).unwrap();
                    let path = pointer
                        .tokens()
                        .iter()
                        .map(|s| Arc::from(s.as_ref()))
                        .collect();
                    let bits = schema::demand(std::slice::from_ref(row), &[]);
                    (
                        path,
                        LaneBits {
                            regular_always: bits.regular_always,
                            precise_always: bits.precise_always,
                            regular_gameplay: bits.regular_gameplay,
                            precise_gameplay: bits.precise_gameplay,
                        },
                    )
                })
                .collect(),
        })
    }
}

const OSU: &str = embedded::VOSU_OSU;
const CLIENT: &str = embedded::VOSU_CLIENT;
const COMMON: &str = embedded::COMMON_DEFS;

fn common_defs_map() -> Value {
    let doc: Value = serde_json::from_str(COMMON).expect("common-defs parses");
    doc.get("$defs").cloned().expect("common-defs has $defs")
}

fn core() -> SchemaOwner {
    SchemaOwner::Core
}

fn plugin(id: &str) -> SchemaOwner {
    SchemaOwner::Plugin(Arc::from(id))
}

fn with_osu() -> SchemaRegistry {
    let mut registry = SchemaRegistry::with_core().unwrap();
    assert!(registry.get("vosu.osu").is_none());
    registry
        .register_versioned(
            "vosu.osu",
            "vosu.osu",
            &"1.3.0".parse().unwrap(),
            vosu_plugin_api::manifest::schema::SchemaProvenance::TrustedOfficial,
            OSU,
        )
        .unwrap();
    registry
}

#[test]
fn core_and_osu_schemas_load() {
    let registry = with_osu();
    for ns in CORE_NAMESPACES {
        let schema = registry.get(ns).unwrap_or_else(|| panic!("{ns} missing"));
        assert_eq!(schema.owner, SchemaOwner::Core);
        assert_eq!(&*schema.namespace, ns);
    }
    let osu = registry.get("vosu.osu").expect("vosu.osu registered");
    assert_eq!(osu.owner, plugin("vosu.osu"));
    // Not a specific version string: A2 bumps 1.1.0 -> 1.2.0 in parallel.
    assert!(osu.version >= semver::Version::new(1, 1, 0));
    assert!(registry.get("vosu.sessions").is_none());
}

#[test]
fn osu_lanes_match_1x_pointer_demand() {
    // Copied from `src/server/v2_subscription.rs:267-325`'s `pointer_demand`,
    // each 1.x pointer mapped through `docs/v2/state-schema.md`'s
    // 1.x-to-2.0 mapping table (ss:224-268) to its 2.0 selector(s).
    let registry = with_osu();
    let always = |regular: bool, precise: bool| LaneBits {
        regular_always: regular,
        precise_always: precise,
        ..Default::default()
    };
    let gameplay = |regular: bool, precise: bool| LaneBits {
        regular_gameplay: regular,
        precise_gameplay: precise,
        ..Default::default()
    };
    let cases: &[(&str, &[&str], LaneBits)] = &[
        // precise: `state` (1.x) splits into both of these (MA, R1).
        ("vosu.status", &["status"], always(false, true)),
        ("vosu.osu", &["session", "state"], always(false, true)),
        (
            "vosu.osu",
            &["beatmap", "time", "live"],
            always(false, true),
        ),
        // precise, "also feeds" targets of beatmap.time.live (ss:232).
        ("vosu.play", &["progress"], always(false, true)),
        // regular (always)
        ("vosu.osu", &["session", "playTime"], always(true, false)),
        (
            "vosu.osu",
            &["settings", "bassDensity"],
            always(true, false),
        ),
        ("vosu.osu", &["room"], always(true, false)),
        // gameplay, precise
        ("vosu.osu", &["keys"], gameplay(false, true)),
        (
            "vosu.osu",
            &["play", "hitErrors", "all"],
            gameplay(false, true),
        ),
        (
            "vosu.osu",
            &["play", "hitErrors", "press"],
            gameplay(false, true),
        ),
        (
            "vosu.osu",
            &["play", "hitErrors", "release"],
            gameplay(false, true),
        ),
        ("vosu.osu", &["play", "keyHitErrors"], gameplay(false, true)),
        ("vosu.osu", &["play", "hitOffsets"], gameplay(false, true)),
        ("vosu.osu", &["play", "catcher"], gameplay(false, true)),
        // gameplay, regular
        ("vosu.osu", &["play", "source"], gameplay(true, false)),
        ("vosu.osu", &["play", "failed"], gameplay(true, false)),
        ("vosu.osu", &["play", "playerName"], gameplay(true, false)),
        ("vosu.osu", &["play", "mode"], gameplay(true, false)),
        ("vosu.play", &["score"], gameplay(true, false)),
        ("vosu.play", &["accuracy"], gameplay(true, false)),
        // "also feeds" targets of play.healthBar/.accuracy (ss:244-245).
        ("vosu.play", &["health"], gameplay(true, false)),
        ("vosu.play", &["rate"], gameplay(true, false)),
        ("vosu.osu", &["play", "healthBar"], gameplay(true, false)),
        ("vosu.osu", &["play", "statistics"], gameplay(true, false)),
        ("vosu.play", &["judgements"], gameplay(true, false)),
        ("vosu.play", &["combo"], gameplay(true, false)),
        ("vosu.play", &["maxCombo"], gameplay(true, false)),
        ("vosu.osu", &["play", "mods"], gameplay(true, false)),
        ("vosu.osu", &["play", "rank"], gameplay(true, false)),
        ("vosu.osu", &["play", "pp"], gameplay(true, false)),
        ("vosu.osu", &["leaderboard"], gameplay(true, false)),
        ("vosu.osu", &["rankedPlay"], gameplay(true, false)),
        // A4/T0 clarification: selected stable managers are in Tourney, not
        // Play. The aggregate and its nested precise leaves must remain
        // always-active or spectator subscriptions can never request sampling.
        // The parent also unions unchanged gameplay-regular chart/play leaves.
        (
            "vosu.osu",
            &["tourney"],
            LaneBits {
                regular_gameplay: true,
                ..always(true, true)
            },
        ),
        (
            "vosu.osu",
            &["performance", "accuracy"],
            gameplay(true, false),
        ),
        ("vosu.osu", &["beatmap", "isKiai"], gameplay(true, false)),
        ("vosu.osu", &["beatmap", "isBreak"], gameplay(true, false)),
        (
            "vosu.osu",
            &["beatmap", "stats", "stars", "live"],
            gameplay(true, false),
        ),
        (
            "vosu.osu",
            &["beatmap", "time", "rate"],
            gameplay(true, false),
        ),
        (
            "vosu.osu",
            &["beatmap", "time", "running"],
            gameplay(true, false),
        ),
        (
            "vosu.osu",
            &["settings", "interfaceVisible"],
            gameplay(true, false),
        ),
        (
            "vosu.osu",
            &["settings", "replayUIVisible"],
            gameplay(true, false),
        ),
        // both, gameplay (1.x lists it under both arrays; the `both` lane).
        (
            "vosu.osu",
            &["play", "unstableRate"],
            LaneBits {
                regular_gameplay: true,
                precise_gameplay: true,
                ..Default::default()
            },
        ),
        // No 1.x entry at all: slow settings/identity/metadata (comment at
        // v2_subscription.rs:266).
        ("vosu.osu", &["profile"], LaneBits::default()),
    ];
    for (ns, tokens, expected) in cases {
        let actual = registry.lanes(ns, tokens);
        assert_eq!(
            actual,
            *expected,
            "{ns}/{} lanes mismatch",
            tokens.join("/")
        );
    }

    // R26 (known difference, accepted): 1.x's gameplay_regular array lists
    // the literal field `["paused"]` (v2_subscription.rs:316), which can
    // never match a real subscribed pointer `/game/paused` (tokens
    // `["game", "paused"]`), so 1.x never actually raises demand for it.
    // 2.0 correctly annotates `vosu.osu.session.paused` regular/gameplay:
    // selecting it raises demand where 1.x's dead entry did not. More
    // sampling, harmless; recorded here rather than treated as a bug.
    assert_eq!(
        registry.lanes("vosu.osu", &["session", "paused"]),
        gameplay(true, false)
    );
}

#[test]
fn annotation_in_defs_applies_through_ref() {
    let registry = with_osu();
    let bits = registry.lanes("vosu.osu", &["beatmap", "stats", "stars", "live"]);
    assert_eq!(
        bits,
        LaneBits {
            regular_gameplay: true,
            ..Default::default()
        },
        "the `stats` $def's `stars.live` annotation must be reachable through `beatmap.stats`'s $ref"
    );
}

#[test]
fn descendant_selector_inherits_lane() {
    let registry = with_osu();
    let bits = registry.lanes("vosu.osu", &["play", "hitErrors", "all"]);
    assert_eq!(
        bits,
        LaneBits {
            precise_gameplay: true,
            ..Default::default()
        },
        "a selector under an annotated parent must inherit that parent's lane"
    );
}

#[test]
fn parent_selector_unions_lanes() {
    let registry = with_osu();
    let play = registry.lanes("vosu.osu", &["play"]);
    assert!(
        play.regular_gameplay && play.precise_gameplay,
        "selecting /play must union every descendant's lane: {play:?}"
    );

    let root = registry.lanes("vosu.osu", &[]);
    assert!(
        root.regular_always
            && root.precise_always
            && root.regular_gameplay
            && root.precise_gameplay,
        "the namespace root selector must union every annotated path: {root:?}"
    );
}

fn mutate(text: &str, patch: impl FnOnce(&mut Value)) -> String {
    let mut value: Value = serde_json::from_str(text).unwrap();
    patch(&mut value);
    serde_json::to_string(&value).unwrap()
}

#[test]
fn x_visibility_rejected_outside_osu_internal() {
    let common_defs = common_defs_map();

    // 1. Another vosu.osu top-level key, not /internal.
    let mutated = mutate(OSU, |v| {
        v["properties"]["session"]["x-visibility"] = json!("plugins");
    });
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));

    // 2. A nested path under /internal, not the key itself.
    let mutated = mutate(OSU, |v| {
        v["properties"]["internal"]["properties"]["paths"]["x-visibility"] = json!("plugins");
    });
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));

    // 3. A different namespace entirely (vosu.client).
    let mutated = mutate(CLIENT, |v| {
        v["properties"]["game"]["x-visibility"] = json!("plugins");
    });
    assert!(matches!(
        walker::walk("vosu.client", &core(), &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));

    // 4. Inside a *used* $defs entry (numberName, referenced e.g. by
    //    beatmap.status) — the carve-out must sit literally at
    //    /properties/internal, not inside a definition $ref reaches.
    let mutated = mutate(OSU, |v| {
        v["$defs"]["numberName"]["x-visibility"] = json!("plugins");
    });
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));

    // 5. The right location, the wrong value.
    let mutated = mutate(OSU, |v| {
        v["properties"]["internal"]["x-visibility"] = json!("admins");
    });
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));

    // The unmutated schema still loads (the carve-out itself is accepted).
    assert!(walker::walk("vosu.osu", &core(), OSU, &common_defs).is_ok());
}

#[test]
fn x_visibility_rejected_at_schema_root() {
    let common_defs = common_defs_map();
    let mutated = mutate(OSU, |v| {
        v["x-visibility"] = json!("plugins");
    });
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));
}

#[test]
fn x_visibility_rejected_in_unused_defs_entry() {
    let common_defs = common_defs_map();
    // A brand new $defs entry nothing in the document $refs.
    let mutated = mutate(OSU, |v| {
        v["$defs"]["neverUsed"] = json!({ "type": "object", "x-visibility": "plugins" });
    });
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));
}

#[test]
fn x_visibility_rejected_in_other_json_schema_keywords() {
    let common_defs = common_defs_map();
    let mutations: &[fn(&mut Value)] = &[
        |v| {
            v["properties"]["session"]["additionalProperties"] =
                json!({ "x-visibility": "plugins" })
        },
        |v| {
            v["properties"]["session"]["patternProperties"] =
                json!({ "^x$": { "x-visibility": "plugins" } })
        },
        |v| v["properties"]["session"]["prefixItems"] = json!([{ "x-visibility": "plugins" }]),
        |v| v["properties"]["session"]["not"] = json!({ "x-visibility": "plugins" }),
        |v| v["properties"]["session"]["if"] = json!({ "x-visibility": "plugins" }),
        |v| v["properties"]["session"]["then"] = json!({ "x-visibility": "plugins" }),
        |v| v["properties"]["session"]["else"] = json!({ "x-visibility": "plugins" }),
        |v| {
            v["properties"]["session"]["dependentSchemas"] =
                json!({ "playTime": { "x-visibility": "plugins" } })
        },
    ];
    for (i, apply) in mutations.iter().enumerate() {
        let mutated = mutate(OSU, *apply);
        assert!(
            matches!(
                walker::walk("vosu.osu", &core(), &mutated, &common_defs),
                Err(SchemaError::VisibilityNotAllowed { .. })
            ),
            "mutation #{i} must be rejected"
        );
    }
}

#[test]
fn x_visibility_rejected_through_ref_oneof_indirection() {
    let common_defs = common_defs_map();
    // /internal itself becomes a oneOf branch that $refs a $defs entry
    // carrying x-visibility: the annotation's *literal* location is inside
    // $defs, never /properties/internal itself, so it must still be
    // rejected even though the indirection ends up "at" /internal.
    let mutated = mutate(OSU, |v| {
        v["$defs"]["indirectInternal"] = json!({ "type": "object", "x-visibility": "plugins" });
        v["properties"]["internal"] = json!({
            "oneOf": [
                { "$ref": "#/$defs/indirectInternal" },
                { "type": "null" },
            ],
        });
    });
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));
}

#[test]
fn x_visibility_non_string_value_rejected() {
    let common_defs = common_defs_map();
    let mutated = mutate(OSU, |v| {
        v["properties"]["internal"]["x-visibility"] = json!(true);
    });
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));
}

#[test]
fn x_visibility_owner_gated_not_just_title() {
    let common_defs = common_defs_map();
    // A schema titled "vosu.osu" with the canonical carve-out, loaded under
    // an owner that is neither Core nor the vosu.osu plugin: must still be
    // rejected (finding 3's "base the exception on the owner as well as
    // the title").
    assert!(matches!(
        walker::walk("vosu.osu", &plugin("some.other.plugin"), OSU, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));
    // The eligible owners both succeed.
    assert!(walker::walk("vosu.osu", &core(), OSU, &common_defs).is_ok());
    assert!(walker::walk("vosu.osu", &plugin("vosu.osu"), OSU, &common_defs).is_err());
}

#[test]
fn additional_properties_lane_is_walked_not_dropped() {
    let common_defs = common_defs_map();
    let text = json!({
        "title": "dev.plugin",
        "version": "1.0.0",
        "type": "object",
        "properties": {
            "scores": {
                "type": "object",
                "additionalProperties": {
                    "type": "number",
                    "x-sampling-lane": { "lane": "regular", "activity": "always" },
                },
            },
        },
    })
    .to_string();
    let walked = walker::walk("dev.plugin", &plugin("dev.plugin"), &text, &common_defs)
        .expect("valid schema walks");
    let expected_path: Vec<Arc<str>> = vec![Arc::from("scores"), Arc::from("*")];
    assert!(
        walked
            .lanes
            .iter()
            .any(|(path, bits)| *path == expected_path
                && *bits
                    == LaneBits {
                        regular_always: true,
                        ..Default::default()
                    }),
        "a lane inside additionalProperties must be recorded, not silently dropped: {:?}",
        walked.lanes
    );
}

#[test]
fn bounds_enforced() {
    let common_defs = common_defs_map();

    // 257 KiB of schema text.
    let oversized = format!(
        r#"{{"title":"vosu.osu","version":"1.0.0","type":"object","properties":{{"padding":{{"type":"string","description":"{}"}}}}}}"#,
        "x".repeat(257 * 1024)
    );
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &oversized, &common_defs),
        Err(SchemaError::TooLarge)
    ));

    // Exactly 32 levels of JSON object nesting passes (plain data nesting,
    // not schema semantics: cat:66's "at most 32 levels").
    let exactly_32 = format!(
        r#"{{"title":"vosu.osu","version":"1.0.0","type":"object","properties":{{"padding":{}}}}}"#,
        nested_object_json(29)
    );
    assert!(
        walker::walk("vosu.osu", &core(), &exactly_32, &common_defs).is_ok(),
        "exactly 32 levels of nesting must pass"
    );

    // 33 levels must not.
    let exactly_33 = format!(
        r#"{{"title":"vosu.osu","version":"1.0.0","type":"object","properties":{{"padding":{}}}}}"#,
        nested_object_json(30)
    );
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &exactly_33, &common_defs),
        Err(SchemaError::TooDeep)
    ));

    // A $ref outside the schema itself and common-defs.
    let foreign_ref = json!({
        "title": "vosu.osu",
        "version": "1.0.0",
        "type": "object",
        "properties": { "field": { "$ref": "https://example.com/schema.json#/foo" } },
    })
    .to_string();
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &foreign_ref, &common_defs),
        Err(SchemaError::ForeignRef(r)) if r == "https://example.com/schema.json#/foo"
    ));
}

/// Builds `{"nest": {"nest": ... {"type": "object"} ...}}` with `extra_wraps`
/// additional `"nest"` levels around a depth-1 leaf object, so the whole
/// document (root object -> `properties` object -> `padding` -> this value)
/// measures exactly `extra_wraps + 3` total levels: `nested_object_json(29)`
/// gives a document depth of exactly 32, `nested_object_json(30)` exactly 33.
fn nested_object_json(extra_wraps: usize) -> String {
    let mut value = json!({ "type": "object" });
    for _ in 0..extra_wraps {
        value = json!({ "nest": value });
    }
    value.to_string()
}

#[test]
fn exponential_ref_fanout_bounded() {
    let common_defs = common_defs_map();
    // A chain of defs, each referencing its successor twice through allOf:
    // a_i = {"allOf": [{"$ref": "#/$defs/a_{i+1}"}, {"$ref": "#/$defs/a_{i+1}"}]},
    // a_N a plain leaf. A naive walker re-expands this to ~2^N node visits;
    // this must instead fail fast with a bounded error (reviewer finding 2).
    const CHAIN_LEN: usize = 40;
    let mut defs = serde_json::Map::new();
    for i in 0..CHAIN_LEN {
        defs.insert(
            format!("a{i}"),
            json!({
                "allOf": [
                    { "$ref": format!("#/$defs/a{}", i + 1) },
                    { "$ref": format!("#/$defs/a{}", i + 1) },
                ],
            }),
        );
    }
    defs.insert(
        format!("a{CHAIN_LEN}"),
        json!({
            "type": "object",
            "properties": {
                "leaf": {
                    "type": "number",
                    "x-sampling-lane": { "lane": "regular", "activity": "always" },
                },
            },
        }),
    );
    let text = json!({
        "title": "dev.plugin",
        "version": "1.0.0",
        "type": "object",
        "$defs": Value::Object(defs),
        "properties": { "field": { "$ref": "#/$defs/a0" } },
    })
    .to_string();

    let start = Instant::now();
    let result = walker::walk("dev.plugin", &plugin("dev.plugin"), &text, &common_defs);
    let elapsed = start.elapsed();

    assert!(
        elapsed < Duration::from_secs(5),
        "walk must be bounded, took {elapsed:?}"
    );
    assert!(
        matches!(result, Err(SchemaError::TooManyNodes)),
        "an exponential-fanout schema must fail with a bounded node-count error"
    );
}

#[test]
fn title_must_match_provides() {
    let mut registry = SchemaRegistry::with_core().unwrap();
    let wrong_title = json!({
        "title": "some.other.namespace",
        "version": "1.0.0",
        "type": "object",
        "properties": {},
    })
    .to_string();
    let err = registry
        .register("dev.plugin", "dev.plugin", &wrong_title)
        .expect_err("a mismatched title must be refused");
    assert!(matches!(err, SchemaError::TitleMismatch { .. }));
}

#[test]
fn register_refuses_core_and_sessions_namespaces() {
    let mut registry = SchemaRegistry::with_core().unwrap();
    for reserved in ["vosu.client", "vosu.sessions"] {
        let text = json!({
            "title": reserved,
            "version": "1.0.0",
            "type": "object",
            "properties": {},
        })
        .to_string();
        let err = registry
            .register("dev.plugin", reserved, &text)
            .expect_err("a core or sessions namespace id must be refused");
        assert!(matches!(err, SchemaError::ReservedNamespace(_)));
    }
}

#[test]
fn register_refuses_conflicting_owner_for_vosu_osu() {
    let mut registry = with_osu();
    let text = json!({
        "title": "vosu.osu",
        "version": "1.0.0",
        "type": "object",
        "properties": {},
    })
    .to_string();
    let err = registry
        .register("unrelated.plugin", "vosu.osu", &text)
        .expect_err("an unrelated provider must not replace the registered vosu.osu");
    assert!(matches!(err, SchemaError::NamespaceConflict(ns) if ns == "vosu.osu"));
    // The plugin-owned schema is untouched.
    assert_eq!(registry.get("vosu.osu").unwrap().owner, plugin("vosu.osu"));
}

#[test]
fn register_refuses_conflicting_owner_between_two_plugins() {
    let mut registry = SchemaRegistry::with_core().unwrap();
    let text = |title: &str| {
        json!({ "title": title, "version": "1.0.0", "type": "object", "properties": {} })
            .to_string()
    };
    registry
        .register("plugin.a", "custom.ns", &text("custom.ns"))
        .expect("first registration succeeds");
    let err = registry
        .register("plugin.b", "custom.ns", &text("custom.ns"))
        .expect_err("a second plugin must not silently take over another's namespace");
    assert!(matches!(err, SchemaError::NamespaceConflict(ns) if ns == "custom.ns"));
}

#[test]
fn register_allows_same_owner_to_reregister() {
    let mut registry = SchemaRegistry::with_core().unwrap();
    let text = |version: &str| {
        json!({ "title": "custom.ns", "version": version, "type": "object", "properties": {} })
            .to_string()
    };
    registry
        .register("plugin.a", "custom.ns", &text("1.0.0"))
        .expect("first registration succeeds");
    let updated = registry
        .register("plugin.a", "custom.ns", &text("1.1.0"))
        .expect("the same owner reloading its own schema must be allowed");
    assert_eq!(updated.version, semver::Version::new(1, 1, 0));
}

#[test]
fn cyclic_ref_rejected() {
    let common_defs = common_defs_map();
    let cyclic = json!({
        "title": "vosu.osu",
        "version": "1.0.0",
        "type": "object",
        "$defs": { "a": { "$ref": "#/$defs/a" } },
        "properties": { "field": { "$ref": "#/$defs/a" } },
    })
    .to_string();
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &cyclic, &common_defs),
        Err(SchemaError::CyclicRef(_))
    ));
}

#[test]
fn unresolved_ref_rejected() {
    let common_defs = common_defs_map();
    let text = json!({
        "title": "vosu.osu",
        "version": "1.0.0",
        "type": "object",
        "properties": { "field": { "$ref": "#/$defs/missing" } },
    })
    .to_string();
    assert!(matches!(
        walker::walk("vosu.osu", &core(), &text, &common_defs),
        Err(SchemaError::UnresolvedRef(_))
    ));
}

#[test]
fn synthetic_schema_lane_and_visibility_rules() {
    // A plugin schema exercising every rule generically, independent of
    // the vosu.osu text A2 edits in parallel: a core-style ref into
    // common-defs, a wildcard-array lane, and the rejected x-visibility.
    let common_defs = common_defs_map();
    let text = json!({
        "title": "dev.plugin",
        "version": "1.0.0",
        "type": "object",
        "required": ["list"],
        "properties": {
            "list": {
                "type": "array",
                "items": {
                    "type": "object",
                    "properties": {
                        "id": {
                            "$ref": "common-defs.schema.json#/$defs/pluginId",
                        },
                        "value": {
                            "type": "number",
                            "x-sampling-lane": { "lane": "both", "activity": "always" },
                        },
                    },
                },
            },
        },
    })
    .to_string();

    let walked = walker::walk("dev.plugin", &plugin("dev.plugin"), &text, &common_defs)
        .expect("valid schema walks");
    assert_eq!(walked.required_top_level.len(), 1);
    assert_eq!(walked.required_top_level[0].as_ref(), "list");
    assert!(walked.hidden.is_empty());

    let expected_path: Vec<Arc<str>> = vec![Arc::from("list"), Arc::from("*"), Arc::from("value")];
    let bits = walked
        .lanes
        .iter()
        .find(|(path, _)| *path == expected_path)
        .map(|(_, bits)| *bits)
        .expect("the wildcard-array lane must be recorded");
    assert_eq!(
        bits,
        LaneBits {
            regular_always: true,
            precise_always: true,
            ..Default::default()
        }
    );

    // The same carve-out rejected on a third-party schema (cat:70-72).
    let with_visibility = json!({
        "title": "dev.plugin",
        "version": "1.0.0",
        "type": "object",
        "properties": { "internal": { "type": "object", "x-visibility": "plugins" } },
    })
    .to_string();
    assert!(matches!(
        walker::walk(
            "dev.plugin",
            &plugin("dev.plugin"),
            &with_visibility,
            &common_defs
        ),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));
}
