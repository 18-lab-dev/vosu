//! Offline packaging structure is intentionally weaker than SDK semantics.
use serde_json::{Value, json};
use vosu_plugin_api::{catalog::Catalog, manifest::PackageManifest};

fn schema(name: &str) -> jsonschema::Validator {
    let text = match name {
        "manifest" => include_str!("../../../docs/v2/schemas/manifest.schema.json"),
        "catalog" => include_str!("../../../docs/v2/schemas/catalog-v2.schema.json"),
        _ => unreachable!(),
    };
    let schema: Value = serde_json::from_str(text).unwrap();
    // No resources are registered or retrieved: every reference must be a local fragment.
    fn local(value: &Value) {
        match value {
            Value::Object(map) => {
                if let Some(reference) = map.get("$ref") {
                    assert!(reference.as_str().unwrap().starts_with("#/$defs/"));
                }
                for child in map.values() {
                    local(child);
                }
            }
            Value::Array(values) => {
                for child in values {
                    local(child);
                }
            }
            _ => {}
        }
    }
    local(&schema);
    // build validates the schema against the selected meta-schema as well.
    jsonschema::options()
        .with_draft(jsonschema::Draft::Draft202012)
        .build(&schema)
        .unwrap()
}

fn manifest(kind: &str) -> Value {
    let mut value = json!({"manifest_version":1,"kind":kind,"id":"meter","name":"Meter",
        "version":"9.2.1","host":">=2.0.0-alpha.1, <3.0.0"});
    match kind {
        "plugin" => {
            value["id"] = json!("acme.meter");
            value["abi_version"] = json!(1);
            value["runtime"] = json!("process");
        }
        "tool" => value["ui"] = json!({"root":"web"}),
        "overlay" => value["resolution"] = json!([400, 150]),
        _ => unreachable!(),
    }
    value
}

fn manifest_ok(value: &Value) -> bool {
    // The manifest schema consumes the data model, never TOML source text.
    let source = toml::to_string(value).unwrap();
    let projected: Value = toml::from_str(&source).unwrap();
    assert_eq!(&projected, value);
    PackageManifest::parse(&source).is_ok()
}

fn entry(kind: &str) -> Value {
    let mut value = json!({"id":"meter","name":"Meter","version":"9.2.1","host":"*",
        "packages":{"any":{"url":"https://example.org/meter.zip","sha256":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","size":1}}});
    if kind == "plugins" {
        value["id"] = json!("acme.meter");
        value["runtime"] = json!("process");
        let package = value["packages"]["any"].take();
        value["packages"] = json!({"x86_64-unknown-linux-gnu":package});
    }
    value
}

fn catalog() -> Value {
    json!({"catalog_version":2,"name":"Offline corpus","published":"2026-11-24T00:00:00Z"})
}
fn catalog_ok(value: &Value) -> bool {
    Catalog::parse(&value.to_string()).is_ok()
}

#[test]
fn manifest_schema_positive_projection_and_defaults() {
    let schema = schema("manifest");
    for kind in ["plugin", "tool", "overlay"] {
        let mut value = manifest(kind);
        assert!(manifest_ok(&value), "{kind}");
        assert!(schema.is_valid(&value), "{kind}");
        value["description"] = json!("");
        value["author"] = json!("Author");
        value["homepage"] = json!("https://example.org/project");
        value["icon"] = json!("art/icon.svg");
        value["banner"] = json!("art/banner.webp");
        value["requires"] = json!([{"id":"field:vosu.play.combo","version":"^1.0.0"}]);
        value["optional"] = json!([{"id":"resource:chart.background","version":"~1.0"}]);
        value["provides"] = json!([]);
        assert!(manifest_ok(&value), "{kind} metadata");
        assert!(schema.is_valid(&value), "{kind} metadata");
    }
    let mut plugin = manifest("plugin");
    plugin["provides"] = json!([
        {"id":"field:acme.meter","version":"1.2.0","schema":"schemas/meter.json"},
        {"id":"action:acme.meter.reset","version":"2.1.0"},
        {"id":"resource:acme.meter.replay","version":"3.0.0"},
        {"id":"game:meter","version":"*"}]);
    plugin["actions"] = json!([{"name":"reset","version":"2.1.0","title":"Reset",
        "args_schema":"args.json","result_schema":"result.json","session":"none"}]);
    plugin["resource_kinds"] = json!([{"kind":"replay","families":["chart-text"]}]);
    plugin["match"] = json!({"process_names":["meter"],"comm_hints":["meter"]});
    plugin["targets"] = json!({"x86_64-unknown-linux-gnu":{"library":"lib/meter.so"}});
    plugin["root_slots"] = json!(["songs"]);
    assert!(manifest_ok(&plugin));
    assert!(schema.is_valid(&plugin));
}

#[test]
fn manifest_schema_shape_rejections() {
    let schema = schema("manifest");
    for kind in ["plugin", "tool", "overlay"] {
        for (rule, key, invalid) in [
            ("manifest.version", "manifest_version", json!(2)),
            ("manifest.kind", "kind", json!("unknown")),
            ("manifest.name", "name", json!("   ")),
            ("manifest.package-version", "version", json!("1.2")),
            ("manifest.path", "icon", json!("../icon.svg")),
            ("manifest.path", "banner", json!("C:/banner.webp")),
            ("manifest.unknown", "typo", json!(true)),
            ("manifest.shape", "requires", json!({})),
        ] {
            let mut value = manifest(kind);
            value[key] = invalid;
            assert!(!manifest_ok(&value), "{rule} {kind}");
            assert!(!schema.is_valid(&value), "{rule} {kind}");
        }
        for key in ["manifest_version", "kind", "id", "name", "version", "host"] {
            let mut value = manifest(kind);
            value.as_object_mut().unwrap().remove(key);
            assert!(!manifest_ok(&value), "missing {key}");
            assert!(!schema.is_valid(&value), "missing {key}");
        }
    }
    for (kind, key, invalid) in [
        ("plugin", "ui", json!({"root":"web"})),
        ("plugin", "resolution", json!([400, 150])),
        ("tool", "runtime", json!("process")),
        ("tool", "ui", json!({"root":"web","extra":true})),
        ("overlay", "ui", json!({"root":"web"})),
        ("overlay", "resolution", json!([0, 150])),
        ("overlay", "resolution", json!([400, 16385])),
        ("overlay", "resolution", json!([400, 150, 1])),
        (
            "tool",
            "provides",
            json!([{"id":"game:meter","version":"*"}]),
        ),
    ] {
        let mut value = manifest(kind);
        value[key] = invalid;
        assert!(!manifest_ok(&value), "{kind}/{key}");
        assert!(!schema.is_valid(&value), "{kind}/{key}");
    }
}

#[test]
fn manifest_schema_semantic_only_limits() {
    let schema = schema("manifest");
    for (rule, key, invalid) in [
        ("manifest.host", "host", json!("not a semver range")),
        (
            "manifest.duplicate-capability",
            "requires",
            json!([
            {"id":"field:vosu.play.combo","version":"*"},
            {"id":"field:vosu.play.combo","version":"^1"}]),
        ),
        ("manifest.replaces", "replaces", json!(["meter"])),
    ] {
        let mut value = manifest("tool");
        value[key] = invalid;
        assert!(!manifest_ok(&value), "{rule}");
        assert!(schema.is_valid(&value), "SDK-only {rule}");
    }
}

#[test]
fn catalog_schema_positive_defaults_and_nulls() {
    let schema = schema("catalog");
    let mut value = catalog();
    assert!(catalog_ok(&value));
    assert!(schema.is_valid(&value));
    for kind in ["plugins", "tools", "overlays"] {
        let mut e = entry(kind);
        for key in [
            "description",
            "author",
            "homepage",
            "icon_url",
            "banner_url",
        ] {
            e[key] = Value::Null;
        }
        if kind == "plugins" {
            e["provides"] = json!([{"id":"field:acme.meter","version":"1.2.0","schema_version":"1.2.0",
                "lanes":[{"pointer":"/combo","lane":"regular","activity":"gameplay"}]}]);
        } else if kind == "overlays" {
            e["preview"] = Value::Null;
            e["resolution"] = json!([1, 16384]);
        }
        value[kind] = json!([e]);
    }
    assert!(catalog_ok(&value));
    assert!(schema.is_valid(&value));
}

#[test]
fn catalog_schema_shape_rejections() {
    let schema = schema("catalog");
    for (rule, key, invalid) in [
        ("catalog.version", "catalog_version", json!(1)),
        ("catalog.shape", "plugins", json!({})),
        ("catalog.shape", "tools", Value::Null),
        ("catalog.shape", "extra", json!(true)),
    ] {
        let mut value = catalog();
        value[key] = invalid;
        assert!(!catalog_ok(&value), "{rule}");
        assert!(!schema.is_valid(&value), "{rule}");
    }
    for kind in ["plugins", "tools", "overlays"] {
        for (key, invalid) in [
            (
                "packages",
                json!({"any":{"url":"http://example.org/a.zip","sha256":"bad","size":0}}),
            ),
            (
                "icon_url",
                json!("https://user:password@example.org/icon.svg"),
            ),
            ("unknown", json!(true)),
            (
                "requires",
                json!([{"id":"field:vosu.play","version":"*","schema":"bad.json"}]),
            ),
        ] {
            let mut e = entry(kind);
            e[key] = invalid;
            let mut value = catalog();
            value[kind] = json!([e]);
            assert!(!catalog_ok(&value), "{kind}/{key}");
            assert!(!schema.is_valid(&value), "{kind}/{key}");
        }
        let forbidden = if kind == "plugins" { "ui" } else { "runtime" };
        let mut e = entry(kind);
        e[forbidden] = Value::Null;
        let mut value = catalog();
        value[kind] = json!([e]);
        assert!(!catalog_ok(&value));
        assert!(!schema.is_valid(&value));
    }
}

#[test]
fn catalog_schema_semantic_only_limits() {
    let schema = schema("catalog");
    let e = entry("tools");
    let mut value = catalog();
    value["tools"] = json!([e.clone(), e]);
    assert!(!catalog_ok(&value));
    assert!(schema.is_valid(&value));
    let mut e = entry("plugins");
    e["provides"] =
        json!([{"id":"field:acme.meter","version":"1.2.0","schema_version":"1.3.0","lanes":[]}]);
    value = catalog();
    value["plugins"] = json!([e]);
    assert!(!catalog_ok(&value));
    assert!(schema.is_valid(&value));
}
