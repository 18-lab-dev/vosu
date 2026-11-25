use super::*;
use crate::manifest::{HostSupport, PackageKind, PackageManifest, Registration, ValidationContext};
use serde_json::{Value, json};

fn package() -> Value {
    json!({"url":"https://example.org/package.zip","sha256":"a".repeat(64),"size":u64::MAX})
}
fn plugin() -> Value {
    json!({"id":"acme.game","name":"Plugin","version":"9.0.0","host":">=2.0.0, <3.0.0",
        "runtime":"process","provides":[{"id":"field:acme.game","version":"1.2.0","schema_version":"1.2.0",
            "lanes":[{"pointer":"/combo","lane":"regular","activity":"gameplay"}]}],
        "packages":{"x86_64-unknown-linux-gnu":package()}})
}
fn web() -> Value {
    json!({"id":"counter","name":"Counter","version":"1.0.0","host":">=2.0.0, <3.0.0",
        "homepage":null,"icon_url":null,"banner_url":null,"packages":{"any":package()}})
}
fn document(array: &str, entries: Vec<Value>) -> Value {
    let mut value =
        json!({"catalog_version":2,"name":"Catalog","published":"2026-11-24T00:00:00Z"});
    value[array] = Value::Array(entries);
    value
}
fn parse(value: &Value) -> Result<Catalog, crate::manifest::ValidationError> {
    Catalog::parse(&value.to_string())
}
fn rejects(value: &Value, rule: &str, pointer: &str) {
    let error = parse(value).unwrap_err();
    assert_eq!(error.rule, rule);
    assert_eq!(error.pointer, pointer);
}

#[test]
fn catalog_arrays_default_and_version_is_exact() {
    let value = document("plugins", vec![]);
    let parsed = parse(&value).unwrap();
    assert!(parsed.tools.is_empty());
    assert!(parsed.overlays.is_empty());
    let mut bad = value;
    bad["catalog_version"] = json!(1);
    rejects(&bad, "catalog.version", "/catalog_version");
}
#[test]
fn catalog_ids_unique_per_kind_not_across_kinds() {
    let mut value = document("tools", vec![web()]);
    value["overlays"] = json!([web()]);
    assert!(parse(&value).is_ok());
    value["tools"] = json!([web(), web()]);
    rejects(&value, "catalog.duplicate-id", "/tools/1/id");
}
#[test]
fn catalog_kind_fields_rejected_even_when_null_or_empty() {
    for array in ["tools", "overlays"] {
        for key in ["actions", "resource_kinds", "covers", "matcher", "runtime"] {
            let mut entry = web();
            entry[key] = Value::Null;
            rejects(
                &document(array, vec![entry]),
                "catalog.kind-field",
                &format!("/{array}/0/{key}"),
            );
        }
    }
    let mut entry = web();
    entry["ui"] = json!({"root":"ui"});
    rejects(&document("tools", vec![entry]), "catalog.shape", "");
}
#[test]
fn catalog_packages_url_hash_size_and_target_rules() {
    assert!(parse(&document("plugins", vec![plugin()])).is_ok());
    for (field, bad, rule) in [
        ("size", json!(0), "catalog.size"),
        ("sha256", json!("a".repeat(63)), "catalog.sha256"),
        ("sha256", json!("g".repeat(64)), "catalog.sha256"),
        ("url", json!("http://example.org/p.zip"), "catalog.https"),
        (
            "url",
            json!("https://user:secret@example.org/p.zip"),
            "catalog.https",
        ),
        ("url", json!("/package.zip"), "catalog.https"),
    ] {
        let mut entry = web();
        entry["packages"]["any"][field] = bad;
        rejects(
            &document("tools", vec![entry]),
            rule,
            &format!("/tools/0/packages/any/{field}"),
        );
    }
    let mut entry = plugin();
    entry["packages"] = json!({"any":package()});
    rejects(
        &document("plugins", vec![entry]),
        "catalog.target",
        "/plugins/0/packages/any",
    );
    let mut entry = web();
    entry["packages"] = json!({"x86_64-pc-windows-msvc":package()});
    rejects(
        &document("tools", vec![entry]),
        "catalog.targets",
        "/tools/0/packages",
    );
}
#[test]
fn catalog_nullable_art_https_and_resolution() {
    assert!(parse(&document("overlays", vec![web()])).is_ok());
    for key in ["icon_url", "banner_url", "preview"] {
        let mut entry = web();
        entry[key] = json!("https://user:secret@example.org/art.png");
        rejects(
            &document("overlays", vec![entry]),
            "catalog.https",
            &format!("/overlays/0/{key}"),
        );
    }
    let mut entry = web();
    entry["resolution"] = json!([1, 16384]);
    assert!(parse(&document("overlays", vec![entry.clone()])).is_ok());
    entry["resolution"] = json!([1, 16385]);
    rejects(
        &document("overlays", vec![entry]),
        "manifest.resolution",
        "/overlays/0/resolution/1",
    );
}
#[test]
fn catalog_host_and_target_unsupported_not_malformed() {
    let parsed = parse(&document("plugins", vec![plugin()])).unwrap();
    let entry = &parsed.plugins[0];
    let alpha = semver::Version::parse("2.0.0-alpha.1").unwrap();
    let stable = semver::Version::parse("2.1.0").unwrap();
    assert_eq!(
        entry
            .support(PackageKind::Plugin, &alpha, "x86_64-unknown-linux-gnu")
            .unwrap()
            .host,
        HostSupport::Unsupported
    );
    assert!(
        !entry
            .support(PackageKind::Plugin, &stable, "x86_64-pc-windows-msvc")
            .unwrap()
            .target
    );
    assert!(
        entry
            .support(PackageKind::Plugin, &stable, "x86_64-unknown-linux-gnu")
            .unwrap()
            .supported()
    );
    let mut no_native_package = plugin();
    no_native_package["packages"] = json!({});
    let catalog = parse(&document("plugins", vec![no_native_package])).unwrap();
    assert!(
        !catalog.plugins[0]
            .support(PackageKind::Plugin, &stable, "x86_64-unknown-linux-gnu")
            .unwrap()
            .supported()
    );
}
#[test]
fn catalog_schema_mirror_presence_versions_and_records() {
    for field in ["schema_version", "lanes"] {
        let mut entry = plugin();
        entry["provides"][0].as_object_mut().unwrap().remove(field);
        rejects(
            &document("plugins", vec![entry]),
            if field == "lanes" {
                "catalog.lanes"
            } else {
                "catalog.schema-version"
            },
            &format!("/plugins/0/provides/0/{field}"),
        );
    }
    let mut entry = plugin();
    entry["provides"][0]["schema_version"] = json!("1.3.0");
    rejects(
        &document("plugins", vec![entry]),
        "catalog.schema-version",
        "/plugins/0/provides/0/schema_version",
    );
    for record in [
        json!({"pointer":"bad","lane":"regular","activity":"gameplay"}),
        json!({"pointer":"/bad~2escape","lane":"regular","activity":"gameplay"}),
        json!({"pointer":"/combo","lane":"wrong","activity":"gameplay"}),
        json!({"pointer":"/combo","lane":"regular","activity":"wrong"}),
    ] {
        let mut entry = plugin();
        entry["provides"][0]["lanes"] = json!([record]);
        rejects(
            &document("plugins", vec![entry]),
            "catalog.lanes",
            "/plugins/0/provides/0/lanes",
        );
    }
    let mut entry = plugin();
    entry["provides"].as_array_mut().unwrap().push(
        json!({"id":"field:acme.game.combo","version":"1.2.0","schema_version":"1.2.0","lanes":[]}),
    );
    rejects(
        &document("plugins", vec![entry]),
        "catalog.schema-field",
        "/plugins/0/provides/1/schema_version",
    );
}
#[test]
fn catalog_replaces_is_source_local_per_kind() {
    let mut first = web();
    first["replaces"] = json!(["old-counter"]);
    let mut other = first.clone();
    other["id"] = json!("other");
    rejects(
        &document("tools", vec![first.clone(), other]),
        "catalog.replaces",
        "/tools/1/replaces/0",
    );
    let mut current = web();
    current["id"] = json!("old-counter");
    rejects(
        &document("tools", vec![first, current]),
        "catalog.replaces",
        "/tools/0/replaces/0",
    );
}
#[test]
fn catalog_context_checks_cross_source_namespaces_and_officialness() {
    let parsed = parse(&document("plugins", vec![plugin()])).unwrap();
    let app = semver::Version::parse("2.1.0").unwrap();
    let registrations = [Registration {
        kind: PackageKind::Plugin,
        id: "acme.game.child",
        source: "other",
        official: false,
        builtin: false,
        replaces: &[],
    }];
    let mut context = ValidationContext {
        application_version: &app,
        source: "catalog",
        official: false,
        registrations: &[],
        namespace_revisions: &[],
    };
    assert!(parsed.validate_context(&context, &[]).is_ok());
    context.registrations = &registrations;
    assert!(parsed.validate_context(&context, &[]).is_err());
    let mut entry = plugin();
    entry["id"] = json!("vosu.extra");
    entry["provides"] = json!([]);
    let official = parse(&document("plugins", vec![entry])).unwrap();
    context.registrations = &[];
    assert!(official.validate_context(&context, &[]).is_err());
    assert!(official.validate_context(&context, &["vosu.extra"]).is_ok());
}

fn manifest() -> PackageManifest {
    PackageManifest::parse("manifest_version=1\nkind='plugin'\nid='acme.game'\nname='Plugin'\nversion='9.0.0'\nhost='>=2.0.0, <3.0.0'\nabi_version=1\nruntime='process'\n[[provides]]\nid='field:acme.game'\nversion='1.2.0'\nschema='schema.json'\n").unwrap()
}
fn schema() -> crate::manifest::schema::CompiledSchema {
    crate::manifest::schema::compile("acme.game", &semver::Version::parse("1.2.0").unwrap(),
        crate::manifest::schema::SchemaProvenance::Untrusted,
        r#"{"title":"acme.game","version":"1.2.0","type":"object","properties":{"combo":{"type":"number","x-sampling-lane":{"lane":"regular","activity":"gameplay"}}}}"#,
        &json!({})).unwrap()
}
#[test]
fn catalog_mirror_requires_real_schema_and_rejects_changed_lanes() {
    let parsed = parse(&document("plugins", vec![plugin()])).unwrap();
    let manifest = manifest();
    let schema = schema();
    assert_eq!(
        schema.lanes(),
        vec![crate::manifest::schema::LaneRecord {
            pointer: "/combo".into(),
            lane: "regular".into(),
            activity: "gameplay".into()
        }]
    );
    assert!(
        compare_package(
            PackageKind::Plugin,
            &parsed.plugins[0],
            &manifest,
            &[schema]
        )
        .is_ok()
    );
    assert!(compare_package(PackageKind::Plugin, &parsed.plugins[0], &manifest, &[]).is_err());
    assert_eq!(
        compare_package(
            PackageKind::Plugin,
            &parsed.plugins[0],
            &manifest,
            &[self::schema(), self::schema()]
        )
        .unwrap_err()
        .pointer,
        "/provides/0/schema_version"
    );
    let extra = crate::manifest::schema::compile(
        "other.game",
        &semver::Version::parse("1.0.0").unwrap(),
        crate::manifest::schema::SchemaProvenance::Untrusted,
        r#"{"title":"other.game","version":"1.0.0","type":"object"}"#,
        &json!({}),
    )
    .unwrap();
    assert_eq!(
        compare_package(
            PackageKind::Plugin,
            &parsed.plugins[0],
            &manifest,
            &[self::schema(), extra]
        )
        .unwrap_err()
        .pointer,
        "/provides"
    );
    let mut entry = plugin();
    entry["provides"][0]["lanes"][0]["lane"] = json!("precise");
    let changed = parse(&document("plugins", vec![entry])).unwrap();
    assert_eq!(
        compare_package(
            PackageKind::Plugin,
            &changed.plugins[0],
            &manifest,
            &[self::schema()]
        )
        .unwrap_err()
        .pointer,
        "/provides/0/lanes"
    );
}
#[test]
fn catalog_mirror_shared_fields_each_checked() {
    let manifest = manifest();
    for (field, value) in [
        ("name", json!("Changed")),
        ("version", json!("9.0.1")),
        ("host", json!("^2")),
        ("description", json!("Changed")),
        ("author", json!("Changed")),
        ("homepage", json!("https://example.org")),
        ("replaces", json!(["old.plugin"])),
        ("runtime", json!("in-process")),
        ("covers", json!(["field:vosu.play.combo"])),
    ] {
        let mut entry = plugin();
        entry[field] = value;
        let parsed = parse(&document("plugins", vec![entry])).unwrap();
        let error = compare_package(
            PackageKind::Plugin,
            &parsed.plugins[0],
            &manifest,
            &[schema()],
        )
        .unwrap_err();
        assert_eq!(error.pointer, format!("/{field}"));
    }
}
#[test]
fn catalog_mirror_ordered_requirements_and_art_not_local_paths() {
    let mut entry = web();
    entry["requires"] = json!([{"id":"field:vosu.play.combo","version":"^1"},{"id":"field:vosu.play.mods","version":"^1"}]);
    entry["icon_url"] = json!("https://example.org/icon.png");
    let catalog = parse(&document("tools", vec![entry])).unwrap();
    let manifest = PackageManifest::parse("manifest_version=1\nkind='tool'\nid='counter'\nname='Counter'\nversion='1.0.0'\nhost='>=2.0.0, <3.0.0'\nicon='art/local.png'\n[ui]\nroot='ui'\n[[requires]]\nid='field:vosu.play.combo'\nversion='^1'\n[[requires]]\nid='field:vosu.play.mods'\nversion='^1'\n").unwrap();
    assert!(compare_package(PackageKind::Tool, &catalog.tools[0], &manifest, &[]).is_ok());
    let mut entry = catalog.tools[0].clone();
    entry.requires.swap(0, 1);
    assert_eq!(
        compare_package(PackageKind::Tool, &entry, &manifest, &[])
            .unwrap_err()
            .pointer,
        "/requires"
    );
}

#[test]
fn catalog_game_matcher_reuses_portable_checks_without_overlap_policy() {
    let mut first = plugin();
    first["provides"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"game:acme-game","version":"*"}));
    assert!(parse(&document("plugins", vec![first.clone()])).is_err());
    first["matcher"] =
        json!({"process_names":["game"],"comm_hints":["game"],"linux_name_contains":["game"]});
    assert!(parse(&document("plugins", vec![first.clone()])).is_ok());
    let mut second = first.clone();
    second["id"] = json!("other.game");
    second["provides"][0]["id"] = json!("field:other.game");
    assert!(parse(&document("plugins", vec![first.clone(), second])).is_ok());
    first["matcher"]["comm_hints"] = json!(["x".repeat(16)]);
    assert!(parse(&document("plugins", vec![first])).is_err());
    let mut nongame = plugin();
    nongame["matcher"] = json!({});
    assert!(parse(&document("plugins", vec![nongame])).is_err());
}

#[test]
fn catalog_actions_resources_and_requirements_reuse_manifest_rules() {
    let mut entry = plugin();
    entry["actions"] = json!([{"name":"song.import","version":"2.0.0","title":"Import","args_schema":"args.json","result_schema":"result.json","session":"required","cli":true}]);
    entry["provides"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"action:acme.game.song.import","version":"2.0.0"}));
    entry["resource_kinds"] = json!([{"kind":"replay","tree":false,"families":["chart-text"]}]);
    entry["provides"]
        .as_array_mut()
        .unwrap()
        .push(json!({"id":"resource:acme.game.replay","version":"3.0.0"}));
    assert!(parse(&document("plugins", vec![entry.clone()])).is_ok());
    entry["provides"][1]["version"] = json!("2.0.1");
    assert!(parse(&document("plugins", vec![entry])).is_err());
    let mut tool = web();
    tool["requires"] = json!([{"id":"field:vosu.osu.internal.memory","version":"^1"}]);
    assert!(parse(&document("tools", vec![tool])).is_err());
    let mut overlay = web();
    overlay["requires"] = json!([{"id":"game:acme-game","version":"*"}]);
    assert!(parse(&document("overlays", vec![overlay])).is_err());
}

#[test]
fn catalog_lane_canonical_order_distinct_contributions_and_escapes() {
    let mut entry = plugin();
    entry["provides"][0]["lanes"] = json!([
        {"pointer":"/map/*/a~1b~0c","lane":"precise","activity":"gameplay"},
        {"pointer":"/map/*/a~1b~0c","lane":"regular","activity":"always"}
    ]);
    assert!(parse(&document("plugins", vec![entry.clone()])).is_ok());
    entry["provides"][0]["lanes"]
        .as_array_mut()
        .unwrap()
        .reverse();
    rejects(
        &document("plugins", vec![entry.clone()]),
        "catalog.lanes",
        "/plugins/0/provides/0/lanes",
    );
    let duplicate = entry["provides"][0]["lanes"][0].clone();
    entry["provides"][0]["lanes"] = json!([duplicate.clone(), duplicate]);
    rejects(
        &document("plugins", vec![entry]),
        "catalog.lanes",
        "/plugins/0/provides/0/lanes",
    );
}
