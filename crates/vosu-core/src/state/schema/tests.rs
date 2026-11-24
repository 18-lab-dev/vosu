use serde_json::{Value, json};

use super::*;

const OSU: &str = embedded::VOSU_OSU;
const CLIENT: &str = embedded::VOSU_CLIENT;
const COMMON: &str = embedded::COMMON_DEFS;

fn common_defs_map() -> Value {
    let doc: Value = serde_json::from_str(COMMON).expect("common-defs parses");
    doc.get("$defs").cloned().expect("common-defs has $defs")
}

#[test]
fn core_and_osu_schemas_load() {
    let registry = SchemaRegistry::with_core().expect("every embedded schema loads");
    for ns in CORE_NAMESPACES {
        let schema = registry.get(ns).unwrap_or_else(|| panic!("{ns} missing"));
        assert_eq!(schema.owner, SchemaOwner::Core);
        assert_eq!(&*schema.namespace, ns);
    }
    let osu = registry.get("vosu.osu").expect("vosu.osu registered");
    assert_eq!(osu.owner, SchemaOwner::Core);
    // Not a specific version string: A2 bumps 1.1.0 -> 1.2.0 in parallel.
    assert!(osu.version >= semver::Version::new(1, 1, 0));
    assert!(registry.get("vosu.sessions").is_none());
}

#[test]
fn osu_lanes_match_1x_pointer_demand() {
    // Copied from `src/server/v2_subscription.rs:267-325`'s `pointer_demand`,
    // each 1.x pointer mapped through `docs/v2/state-schema.md`'s
    // 1.x-to-2.0 mapping table (ss:224-268) to its 2.0 selector(s).
    let registry = SchemaRegistry::with_core().unwrap();
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
        // precise
        (
            "vosu.osu",
            &["beatmap", "time", "live"],
            always(false, true),
        ),
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
        ("vosu.osu", &["tourney"], gameplay(true, false)),
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
    let registry = SchemaRegistry::with_core().unwrap();
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
    let registry = SchemaRegistry::with_core().unwrap();
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
    let registry = SchemaRegistry::with_core().unwrap();
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
        walker::walk("vosu.osu", &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));

    // 2. A nested path under /internal, not the key itself.
    let mutated = mutate(OSU, |v| {
        v["properties"]["internal"]["properties"]["paths"]["x-visibility"] = json!("plugins");
    });
    assert!(matches!(
        walker::walk("vosu.osu", &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));

    // 3. A different namespace entirely (vosu.client).
    let mutated = mutate(CLIENT, |v| {
        v["properties"]["game"]["x-visibility"] = json!("plugins");
    });
    assert!(matches!(
        walker::walk("vosu.client", &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));

    // 4. Inside a $defs entry reachable from a non-internal property
    //    (numberName, referenced e.g. by beatmap.status).
    let mutated = mutate(OSU, |v| {
        v["$defs"]["numberName"]["x-visibility"] = json!("plugins");
    });
    assert!(matches!(
        walker::walk("vosu.osu", &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));

    // 5. The right location, the wrong value.
    let mutated = mutate(OSU, |v| {
        v["properties"]["internal"]["x-visibility"] = json!("admins");
    });
    assert!(matches!(
        walker::walk("vosu.osu", &mutated, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));

    // The unmutated schema still loads (the carve-out itself is accepted).
    assert!(walker::walk("vosu.osu", OSU, &common_defs).is_ok());
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
        walker::walk("vosu.osu", &oversized, &common_defs),
        Err(SchemaError::TooLarge)
    ));

    // 33 levels of JSON object nesting (plain data nesting, not schema
    // semantics: cat:66's "levels of JSON structural nesting").
    let mut nested = json!("leaf");
    for _ in 0..33 {
        nested = json!({ "nest": nested });
    }
    let too_deep = json!({
        "title": "vosu.osu",
        "version": "1.0.0",
        "type": "object",
        "properties": { "padding": nested },
    })
    .to_string();
    assert!(matches!(
        walker::walk("vosu.osu", &too_deep, &common_defs),
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
        walker::walk("vosu.osu", &foreign_ref, &common_defs),
        Err(SchemaError::ForeignRef(r)) if r == "https://example.com/schema.json#/foo"
    ));
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
        walker::walk("vosu.osu", &cyclic, &common_defs),
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
        walker::walk("vosu.osu", &text, &common_defs),
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

    let walked = walker::walk("dev.plugin", &text, &common_defs).expect("valid schema walks");
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
        walker::walk("dev.plugin", &with_visibility, &common_defs),
        Err(SchemaError::VisibilityNotAllowed { .. })
    ));
}
