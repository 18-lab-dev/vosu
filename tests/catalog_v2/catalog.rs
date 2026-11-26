use super::support::*;
use serde_json::{Value, json};
use vosu_plugin_api::catalog::Catalog;

pub fn document(array: &str, entry: Value) -> Value {
    let mut v = json!({"catalog_version":2,"name":"Corpus","published":"informational"});
    v[array] = json!([entry]);
    v
}
pub fn entry(kind: &str) -> Value {
    let mut v = json!({"id":"corpus","name":"Corpus","version":"9.0.0","host":format!("={}",vosu::BUNDLE.version),
        "homepage":null,"icon_url":null,"banner_url":null,
        "packages":{"any":{"url":"https://example.org/p.zip","sha256":"a".repeat(64),"size":u64::MAX}}});
    if kind == "plugins" {
        v["id"] = json!("acme.corpus");
        v["runtime"] = json!("in-process");
        v["packages"] = json!({"x86_64-unknown-linux-gnu":v["packages"]["any"].clone()});
        v["provides"] = json!([{"id":"field:acme.corpus","version":"1.2.0","schema_version":"1.2.0",
            "lanes":[{"pointer":"/combo","lane":"regular","activity":"gameplay"}]}]);
    }
    v
}
pub fn corpus() -> Vec<Case> {
    let mut rows = Vec::new();
    for array in ["plugins", "tools", "overlays"] {
        let good = document(array, entry(array));
        rows.push(Case::mutation(
            format!("{array}:format"),
            &good,
            "/catalog_version",
            json!(1),
            "catalog.version",
            "/catalog_version",
            true,
        ));
        rows.push(Case::mutation(
            format!("{array}:duplicate"),
            &good,
            &format!("/{array}"),
            json!([entry(array), entry(array)]),
            "catalog.duplicate-id",
            &format!("/{array}/1/id"),
            false,
        ));
        let target = if array == "plugins" {
            "x86_64-unknown-linux-gnu"
        } else {
            "any"
        };
        for (field, bad, rule, shape) in [
            ("size", json!(0), "catalog.size", true),
            ("sha256", json!("a".repeat(63)), "catalog.sha256", true),
            ("sha256", json!("g".repeat(64)), "catalog.sha256", true),
            (
                "url",
                json!("http://example.org/p.zip"),
                "catalog.https",
                true,
            ),
            (
                "url",
                json!("https://user:secret@example.org/p.zip"),
                "catalog.https",
                true,
            ),
            ("url", json!("/p.zip"), "catalog.https", true),
        ] {
            let path = format!("/{array}/0/packages/{target}/{field}");
            rows.push(Case::mutation(
                format!("{array}:{field}:{bad}"),
                &good,
                &path,
                bad,
                rule,
                &path,
                shape,
            ));
        }
        for key in ["icon_url", "banner_url"] {
            let path = format!("/{array}/0/{key}");
            rows.push(Case::mutation(
                format!("{array}:{key}"),
                &good,
                &path,
                json!("https://user:secret@example.org/art.png"),
                "catalog.https",
                &path,
                true,
            ));
        }
        if array != "plugins" {
            for key in ["actions", "resource_kinds", "covers", "matcher", "runtime"] {
                let mut invalid = good.clone();
                invalid[array][0][key] = Value::Null;
                rows.push(Case {
                    label: format!("{array}:kind:{key}"),
                    valid: good.clone(),
                    invalid,
                    rule: "catalog.kind-field",
                    pointer: format!("/{array}/0/{key}"),
                    shape: true,
                });
            }
        }
    }
    let good = document("plugins", entry("plugins"));
    for (path, bad, rule) in [
        (
            "/plugins/0/provides/0/schema_version",
            json!("1.3.0"),
            "catalog.schema-version",
        ),
        (
            "/plugins/0/provides/0/lanes/0/pointer",
            json!("/bad~2"),
            "catalog.lanes",
        ),
    ] {
        rows.push(Case::mutation(
            path,
            &good,
            path,
            bad,
            rule,
            if rule == "catalog.lanes" {
                "/plugins/0/provides/0/lanes"
            } else {
                path
            },
            false,
        ));
    }
    rows
}
#[test]
fn catalog_rule_matrix_sdk_and_offline_schema() {
    let schema = offline("catalog");
    for row in corpus() {
        assert!(
            Catalog::parse(&row.valid.to_string()).is_ok(),
            "{} control",
            row.label
        );
        assert!(schema.is_valid(&row.valid), "{} offline control", row.label);
        let error = Catalog::parse(&row.invalid.to_string()).unwrap_err();
        assert_eq!(error.rule, row.rule, "{}", row.label);
        assert_eq!(error.pointer, row.pointer, "{}", row.label);
        if row.shape {
            assert!(!schema.is_valid(&row.invalid), "{} shape", row.label);
        }
    }
}
