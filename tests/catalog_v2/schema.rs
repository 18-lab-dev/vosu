use serde_json::{Value, json};
use vosu_core::state::schema::SchemaRegistry;
use vosu_plugin_api::manifest::schema::{self, SchemaProvenance};
fn document() -> Value {
    json!({"title":"acme.corpus","version":"1.2.0","type":"object","properties":{
        "combo":{"x-sampling-lane":{"lane":"regular","activity":"always"}},
        "samples":{"type":"array","items":{"x-sampling-lane":{"lane":"precise","activity":"gameplay"}}},
        "map":{"additionalProperties":{"x-sampling-lane":{"lane":"regular","activity":"gameplay"}}}
    }})
}
fn compile(text: &str) -> Result<schema::CompiledSchema, schema::SchemaError> {
    schema::compile(
        "acme.corpus",
        &"1.2.0".parse().unwrap(),
        SchemaProvenance::Untrusted,
        text,
        &json!({}),
    )
}
#[test]
fn d7_sdk_catalog_mirror_and_real_core_demand_are_identical() {
    let text = document().to_string();
    let compiled = compile(&text).unwrap();
    let expected = json!([
        {"pointer":"/combo","lane":"regular","activity":"always"},
        {"pointer":"/map/*","lane":"regular","activity":"gameplay"},
        {"pointer":"/samples/*","lane":"precise","activity":"gameplay"}]);
    assert_eq!(serde_json::to_value(compiled.lanes()).unwrap(), expected);
    let mut registry = SchemaRegistry::with_core().unwrap();
    registry
        .register_versioned(
            "acme.corpus",
            "acme.corpus",
            &"1.2.0".parse().unwrap(),
            SchemaProvenance::Untrusted,
            &text,
        )
        .unwrap();
    for (tokens, bits) in [
        (vec![], (true, false, true, true)),
        (vec!["combo"], (true, false, false, false)),
        (vec!["samples", "0"], (false, false, false, true)),
        (vec!["map", "key"], (false, false, true, false)),
        (vec!["missing"], (false, false, false, false)),
    ] {
        let actual = registry.lanes("acme.corpus", &tokens);
        assert_eq!(
            (
                actual.regular_always,
                actual.precise_always,
                actual.regular_gameplay,
                actual.precise_gameplay
            ),
            bits,
            "{tokens:?}"
        );
        let sdk = schema::demand(compiled.lanes(), &tokens);
        assert_eq!(
            (
                sdk.regular_always,
                sdk.precise_always,
                sdk.regular_gameplay,
                sdk.precise_gameplay
            ),
            bits
        );
    }
    let mut package = super::support::manifest("plugin");
    package["provides"] =
        json!([{"id":"field:acme.corpus","version":"1.2.0","schema":"namespace.json"}]);
    let package = super::support::parse(&package).unwrap();
    let mut entry = super::catalog::entry("plugins");
    entry["provides"][0]["lanes"] = expected;
    let catalog = vosu_plugin_api::catalog::Catalog::parse(
        &super::catalog::document("plugins", entry.clone()).to_string(),
    )
    .unwrap();
    vosu_plugin_api::catalog::compare_package(
        vosu_plugin_api::manifest::PackageKind::Plugin,
        &catalog.plugins[0],
        &package,
        std::slice::from_ref(&compiled),
    )
    .unwrap();
    assert!(
        vosu_plugin_api::catalog::compare_package(
            vosu_plugin_api::manifest::PackageKind::Plugin,
            &catalog.plugins[0],
            &package,
            &[]
        )
        .is_err()
    );
    entry["provides"][0]["lanes"][0]["lane"] = json!("precise");
    let catalog = vosu_plugin_api::catalog::Catalog::parse(
        &super::catalog::document("plugins", entry).to_string(),
    )
    .unwrap();
    assert!(
        vosu_plugin_api::catalog::compare_package(
            vosu_plugin_api::manifest::PackageKind::Plugin,
            &catalog.plugins[0],
            &package,
            &[compiled]
        )
        .is_err()
    );
}
#[test]
fn d7_rejection_corpus_is_atomic_in_core() {
    let valid = document();
    for (path, bad, rule) in [
        ("/title", json!("other.namespace"), "D7.title"),
        ("/version", json!("1.3.0"), "D7.version"),
        (
            "/properties/combo/x-sampling-lane/lane",
            json!("fast"),
            "D7.lane",
        ),
        (
            "/properties/combo/x-sampling-lane/activity",
            json!("never"),
            "D7.lane",
        ),
    ] {
        assert!(compile(&valid.to_string()).is_ok());
        let mut bad_doc = valid.clone();
        *bad_doc.pointer_mut(path).unwrap() = bad;
        let text = bad_doc.to_string();
        assert!(
            compile(&text).unwrap_err().to_string().starts_with(rule),
            "{path}"
        );
        let mut registry = SchemaRegistry::with_core().unwrap();
        assert!(
            registry
                .register_versioned(
                    "acme.corpus",
                    "acme.corpus",
                    &"1.2.0".parse().unwrap(),
                    SchemaProvenance::Untrusted,
                    &text
                )
                .is_err()
        );
        assert!(registry.get("acme.corpus").is_none());
    }
    // state-schema.md:491–494 requires RFC6901 fragments and rejects malformed
    // references. #anchor is a D7.ref rejection, not a mirror pointer record.
    for reference in [
        "https://example.org/schema",
        "file:///schema",
        "#anchor",
        "#/$defs/missing",
    ] {
        let mut good = valid.clone();
        good["$defs"] = json!({"unused":{"type":"string"}});
        assert!(compile(&good.to_string()).is_ok());
        good["$defs"]["unused"]["$ref"] = json!(reference);
        assert!(
            compile(&good.to_string())
                .unwrap_err()
                .to_string()
                .starts_with("D7.ref")
        );
    }
    let mut good = valid;
    good["$defs"] = json!({"unused":{}});
    assert!(compile(&good.to_string()).is_ok());
    good["$defs"]["unused"]["x-visibility"] = json!("internal");
    assert!(
        compile(&good.to_string())
            .unwrap_err()
            .to_string()
            .starts_with("D7.visibility")
    );
}
#[test]
fn d7_exact_byte_boundary() {
    let mut text = document().to_string();
    text.extend(std::iter::repeat_n(' ', schema::MAX_BYTES - text.len()));
    assert!(compile(&text).is_ok());
    text.push(' ');
    assert_eq!(compile(&text).unwrap_err(), schema::SchemaError::TooLarge);
}

#[test]
fn d7_unused_depth_cycle_fanout_and_escaped_ref_siblings() {
    let mut doc = document();
    let mut nested = json!({});
    for _ in 0..29 {
        nested = json!({"nest":nested});
    }
    doc["$defs"] = json!({"unused":nested});
    assert!(compile(&doc.to_string()).is_ok());
    doc["$defs"]["unused"] = json!({"nest":doc["$defs"]["unused"].clone()});
    assert_eq!(
        compile(&doc.to_string()).unwrap_err(),
        schema::SchemaError::TooDeep
    );
    doc = document();
    doc["$defs"] = json!({"a":{"type":"number"},"b":{"$ref":"#/$defs/a"}});
    assert!(compile(&doc.to_string()).is_ok());
    doc["$defs"]["a"] = json!({"$ref":"#/$defs/b"});
    assert!(matches!(
        compile(&doc.to_string()),
        Err(schema::SchemaError::CyclicRef(_))
    ));
    doc = document();
    doc["$defs"] = json!({"a/b~c":{"properties":{"n":{"x-sampling-lane":{"lane":"regular","activity":"always"}}}}});
    doc["properties"]["joined"] = json!({"$ref":"#/$defs/a~1b~0c","properties":{
        "n":{"x-sampling-lane":{"lane":"precise","activity":"gameplay"}}}});
    let compiled = compile(&doc.to_string()).unwrap();
    let joined = compiled
        .lanes()
        .iter()
        .filter(|r| r.pointer == "/joined/n")
        .map(|r| (r.lane.as_str(), r.activity.as_str()))
        .collect::<Vec<_>>();
    assert_eq!(joined, [("precise", "gameplay"), ("regular", "always")]);
    doc = document();
    assert!(compile(&doc.to_string()).is_ok());
    for i in 0..30 {
        doc["$defs"][format!("a{i}")] = json!({"allOf":[{"$ref":format!("#/$defs/a{}",i+1)},{"$ref":format!("#/$defs/a{}",i+1)}]});
    }
    doc["$defs"]["a30"] = json!({"x-sampling-lane":{"lane":"regular","activity":"always"}});
    doc["properties"]["x"] = json!({"$ref":"#/$defs/a0"});
    assert_eq!(
        compile(&doc.to_string()).unwrap_err(),
        schema::SchemaError::TooManyNodes
    );
}
