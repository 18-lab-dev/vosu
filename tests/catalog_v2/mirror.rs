use super::support::*;
use serde_json::json;
use vosu_plugin_api::{
    catalog::{Catalog, compare_package},
    manifest::PackageKind,
};
#[test]
fn mirror_rejects_each_shared_field_and_requirement_order_independently() {
    let mut manifest = manifest("plugin");
    manifest["description"] = json!("Description");
    manifest["author"] = json!("Author");
    manifest["homepage"] = json!("https://example.org/");
    manifest["requires"] = json!([{"id":"field:vosu.play.combo","version":"^1"},{"id":"field:vosu.chart.title","version":"^1"}]);
    manifest["optional"] = json!([{"id":"resource:chart.background","version":"^1"}]);
    let package = parse(&manifest).unwrap();
    let mut entry = super::catalog::entry("plugins");
    entry["provides"] = json!([]);
    for key in ["description", "author", "homepage", "requires", "optional"] {
        entry[key] = manifest[key].clone();
    }
    let good =
        Catalog::parse(&super::catalog::document("plugins", entry.clone()).to_string()).unwrap();
    compare_package(PackageKind::Plugin, &good.plugins[0], &package, &[]).unwrap();
    for (field, bad) in [
        ("id", json!("other.plugin")),
        ("name", json!("Other name")),
        ("version", json!("10.0.0")),
        ("host", json!("<1.0.0")),
        ("description", json!("Other description")),
        ("author", json!("Other author")),
        ("homepage", json!("https://other.example.org/")),
        ("runtime", json!("process")),
        ("replaces", json!(["old.plugin"])),
        (
            "requires",
            json!([
                manifest["requires"][1].clone(),
                manifest["requires"][0].clone()
            ]),
        ),
        ("optional", json!([])),
        ("covers", json!(["field:vosu.play.combo"])),
        (
            "resource_kinds",
            json!([{"kind":"file","families":["image"]}]),
        ),
    ] {
        let mut bad_entry = entry.clone();
        bad_entry[field] = bad;
        let catalog =
            Catalog::parse(&super::catalog::document("plugins", bad_entry).to_string()).unwrap();
        let error =
            compare_package(PackageKind::Plugin, &catalog.plugins[0], &package, &[]).unwrap_err();
        assert_eq!(error.rule, "catalog.mirror", "{field}");
        assert_eq!(error.pointer, format!("/{field}"));
    }
}
#[test]
fn catalog_support_retains_unsupported_entries_and_web_kinds_are_independent() {
    let mut plugin = super::catalog::entry("plugins");
    plugin["host"] = json!(">=99.0.0");
    let mut value = super::catalog::document("plugins", plugin);
    value["tools"] = json!([super::catalog::entry("tools")]);
    value["overlays"] = json!([super::catalog::entry("overlays")]);
    let parsed = Catalog::parse(&value.to_string()).unwrap();
    assert_eq!(parsed.plugins.len(), 1);
    assert_eq!(parsed.tools.len(), 1);
    assert_eq!(parsed.overlays.len(), 1);
    let support = parsed.plugins[0]
        .support(
            PackageKind::Plugin,
            vosu::application_version(),
            "x86_64-pc-windows-msvc",
        )
        .unwrap();
    assert_eq!(
        support.host,
        vosu_plugin_api::manifest::HostSupport::Unsupported
    );
    assert!(!support.target);
    // Empty omitted arrays default rather than requiring a publisher to manufacture entries.
    let empty =
        Catalog::parse(r#"{"catalog_version":2,"name":"Empty","published":"informational"}"#)
            .unwrap();
    assert!(empty.plugins.is_empty() && empty.tools.is_empty() && empty.overlays.is_empty());
}
