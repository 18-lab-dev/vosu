//! The checker's own contract: a mirrored catalog of real extracted
//! packages passes with nonzero counts, and each AF.10 counterexample
//! (missing schema bytes, a mirror mismatch, an unknown registration, a
//! `vosu.` id from a non-official source, an escaping asset, an
//! unsupported host) fails.
#![expect(
    clippy::disallowed_methods,
    reason = "test fixtures write disposable package trees under a per-test temporary directory, never user state"
)]

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};

use serde_json::{Value, json};

use super::{Args, check, parse_args};
use crate::context::RegistryContext;

const LINUX: &str = "x86_64-unknown-linux-gnu";
const THIRD_PARTY: &str = "acme/plugins";
const OFFICIAL: &str = "18-lab-dev/vosu-tools";

/// A per-test directory removed on drop.
struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        static NEXT: AtomicU32 = AtomicU32::new(0);
        let path = std::env::temp_dir().join(format!(
            "vosu-package-check-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).unwrap();
        Self(path)
    }

    fn write(&self, relative: &str, contents: &str) {
        let path = self.0.join(relative);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, contents).unwrap();
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn package() -> Value {
    json!({"url": "https://example.org/package.zip", "sha256": "a".repeat(64), "size": 1})
}

fn plugin_manifest(id: &str, extra: &str) -> String {
    format!(
        r#"manifest_version = 1
kind = "plugin"
id = "{id}"
name = "Plugin"
version = "9.0.0"
host = ">=2.0.0-alpha.1, <3.0.0"
abi_version = 1
runtime = "process"
{extra}
[targets.{LINUX}]
library = "lib/libplugin.so"

[[provides]]
id = "field:{id}"
version = "1.2.0"
schema = "schemas/namespace.schema.json"

[[provides]]
id = "action:{id}.song.import"
version = "2.0.0"

[[actions]]
name = "song.import"
version = "2.0.0"
title = "Import"
args_schema = "schemas/args.json"
result_schema = "schemas/result.json"
session = "none"
"#
    )
}

fn plugin_entry(id: &str) -> Value {
    json!({"id": id, "name": "Plugin", "version": "9.0.0", "host": ">=2.0.0-alpha.1, <3.0.0",
        "runtime": "process",
        "provides": [
            {"id": format!("field:{id}"), "version": "1.2.0", "schema_version": "1.2.0",
             "lanes": [{"pointer": "/combo", "lane": "regular", "activity": "gameplay"}]},
            {"id": format!("action:{id}.song.import"), "version": "2.0.0"}
        ],
        "actions": [{"name": "song.import", "version": "2.0.0", "title": "Import",
            "args_schema": "schemas/args.json", "result_schema": "schemas/result.json",
            "session": "none", "cli": false}],
        "packages": {LINUX: package()}})
}

const TOOL_MANIFEST: &str = r#"manifest_version = 1
kind = "tool"
id = "acme-ui"
name = "Tool"
version = "1.0.0"
host = ">=2.0.0-alpha.1, <3.0.0"

[ui]
root = "ui"
"#;

/// One plugin and one web tool, both mirrored and extracted.
struct Fixture {
    scratch: Scratch,
    id: String,
    catalog: Value,
}

impl Fixture {
    fn new(id: &str) -> Self {
        let scratch = Scratch::new();
        let plugin = format!("packages/plugins/{id}/{LINUX}");
        scratch.write(&format!("{plugin}/manifest.toml"), &plugin_manifest(id, ""));
        scratch.write(&format!("{plugin}/lib/libplugin.so"), "not a real library");
        let schema = json!({"title": id, "version": "1.2.0", "properties": {
            "combo": {"type": "number",
                      "x-sampling-lane": {"lane": "regular", "activity": "gameplay"}}}});
        scratch.write(
            &format!("{plugin}/schemas/namespace.schema.json"),
            &schema.to_string(),
        );
        scratch.write(
            &format!("{plugin}/schemas/args.json"),
            r#"{"type":"object"}"#,
        );
        scratch.write(&format!("{plugin}/schemas/result.json"), "true");
        scratch.write("packages/tools/acme-ui/any/manifest.toml", TOOL_MANIFEST);
        scratch.write(
            "packages/tools/acme-ui/any/ui/index.html",
            "<!doctype html>",
        );
        let catalog = json!({"catalog_version": 2, "name": "Catalog",
            "published": "2026-11-24T00:00:00Z",
            "plugins": [plugin_entry(id)],
            "tools": [{"id": "acme-ui", "name": "Tool", "version": "1.0.0",
                       "host": ">=2.0.0-alpha.1, <3.0.0", "packages": {"any": package()}}]});
        let fixture = Self {
            scratch,
            id: id.to_owned(),
            catalog,
        };
        fixture.write_context(json!([]));
        fixture.write_catalog();
        fixture
    }

    fn plugin_dir(&self) -> String {
        format!("packages/plugins/{}/{LINUX}", self.id)
    }

    fn write_catalog(&self) {
        self.scratch
            .write("catalog-v2.json", &self.catalog.to_string());
    }

    fn write_context(&self, registrations: Value) {
        let context = json!({"format": 1, "official_sources": [OFFICIAL],
            "registrations": registrations, "namespace_revisions": [],
            "common_defs_schema": {"$defs": {}}});
        self.scratch.write("context.json", &context.to_string());
    }

    fn args(&self, source: &str, host: &str) -> Args {
        Args {
            catalog: self.scratch.0.join("catalog-v2.json"),
            packages_root: self.scratch.0.join("packages"),
            host_version: host.parse().unwrap(),
            source: source.to_owned(),
            registry_context: self.scratch.0.join("context.json"),
        }
    }

    fn run(&self, source: &str) -> Result<super::Report, String> {
        check(&self.args(source, "2.0.0-alpha.1"))
    }
}

#[test]
fn mirrored_extracted_packages_pass_with_nonzero_counts() {
    let fixture = Fixture::new("acme.game");
    let report = fixture.run(THIRD_PARTY).expect("valid catalog passes");
    assert_eq!(report.packages, 2);
    assert_eq!(report.actions, 1);
    assert_eq!(report.schema_assets, 3);
    assert_eq!(
        report.artifacts,
        vec![
            format!(
                "plugins/acme.game/{LINUX}: library lib/libplugin.so, 1 actions, 3 schema assets"
            ),
            "tools/acme-ui/any: ui ui/index.html, 0 actions, 0 schema assets".to_owned(),
        ]
    );
}

#[test]
fn missing_namespace_schema_bytes_fail() {
    let fixture = Fixture::new("acme.game");
    std::fs::remove_file(
        fixture
            .scratch
            .0
            .join(fixture.plugin_dir())
            .join("schemas/namespace.schema.json"),
    )
    .unwrap();
    let error = fixture.run(THIRD_PARTY).unwrap_err();
    assert!(error.contains("schemas/namespace.schema.json"), "{error}");
}

#[test]
fn missing_action_schema_bytes_fail() {
    let fixture = Fixture::new("acme.game");
    std::fs::remove_file(
        fixture
            .scratch
            .0
            .join(fixture.plugin_dir())
            .join("schemas/result.json"),
    )
    .unwrap();
    let error = fixture.run(THIRD_PARTY).unwrap_err();
    assert!(error.contains("schemas/result.json"), "{error}");
}

#[test]
fn catalog_lanes_that_differ_from_the_schema_bytes_fail_the_mirror() {
    let mut fixture = Fixture::new("acme.game");
    fixture.catalog["plugins"][0]["provides"][0]["lanes"][0]["activity"] = json!("always");
    fixture.write_catalog();
    let error = fixture.run(THIRD_PARTY).unwrap_err();
    assert!(
        error.contains("catalog.mirror at /provides/0/lanes"),
        "{error}"
    );
}

#[test]
fn catalog_metadata_that_differs_from_the_manifest_fails_the_mirror() {
    let mut fixture = Fixture::new("acme.game");
    fixture.catalog["plugins"][0]["actions"][0]["title"] = json!("Changed");
    fixture.write_catalog();
    let error = fixture.run(THIRD_PARTY).unwrap_err();
    assert!(error.contains("catalog.mirror at /actions"), "{error}");
}

#[test]
fn a_requirement_on_an_unregistered_namespace_fails_until_the_context_registers_it() {
    let mut fixture = Fixture::new("acme.game");
    let requires = json!([{"id": "field:other.plugin.value", "version": "^1"}]);
    fixture.catalog["plugins"][0]["requires"] = requires;
    fixture.write_catalog();
    fixture.scratch.write(
        &format!("{}/manifest.toml", fixture.plugin_dir()),
        &plugin_manifest(
            "acme.game",
            "[[requires]]\nid = \"field:other.plugin.value\"\nversion = \"^1\"\n",
        ),
    );
    let error = fixture.run(THIRD_PARTY).unwrap_err();
    assert!(error.contains("manifest.unknown-namespace"), "{error}");

    fixture.write_context(json!([{"kind": "plugin", "id": "other.plugin",
        "source": "other/source", "official": false, "builtin": false}]));
    fixture
        .run(THIRD_PARTY)
        .expect("registered namespace resolves");
}

#[test]
fn a_vosu_id_validates_only_from_an_exact_official_source() {
    let fixture = Fixture::new("vosu.tools.game");
    let error = fixture.run(THIRD_PARTY).unwrap_err();
    assert!(error.contains("manifest.namespace"), "{error}");
    // Near-miss identities are not official.
    for alias in [
        "18-lab-dev/vosu-tools/",
        "18-Lab-Dev/vosu-tools",
        "18-lab-dev/vosu",
    ] {
        assert!(fixture.run(alias).is_err(), "{alias}");
    }
    fixture.run(OFFICIAL).expect("official source passes");
}

#[test]
fn an_unsupported_host_version_fails() {
    let fixture = Fixture::new("acme.game");
    let error = check(&fixture.args(THIRD_PARTY, "3.0.0")).unwrap_err();
    assert!(error.contains("host-support"), "{error}");
}

#[test]
fn a_missing_extracted_package_or_target_library_fails() {
    let fixture = Fixture::new("acme.game");
    std::fs::remove_file(
        fixture
            .scratch
            .0
            .join(fixture.plugin_dir())
            .join("lib/libplugin.so"),
    )
    .unwrap();
    let error = fixture.run(THIRD_PARTY).unwrap_err();
    assert!(error.contains("lib/libplugin.so"), "{error}");

    let fixture2 = Fixture::new("acme.game");
    std::fs::remove_dir_all(fixture2.scratch.0.join("packages/tools")).unwrap();
    let error = fixture2.run(THIRD_PARTY).unwrap_err();
    assert!(error.contains("tools/acme-ui/any"), "{error}");
}

#[cfg(unix)]
#[test]
fn an_asset_symlink_out_of_the_package_fails() {
    let fixture = Fixture::new("acme.game");
    let plugin = fixture.scratch.0.join(fixture.plugin_dir());
    fixture.scratch.write("outside.json", "true");
    std::fs::remove_file(plugin.join("schemas/args.json")).unwrap();
    std::os::unix::fs::symlink(
        fixture.scratch.0.join("outside.json"),
        plugin.join("schemas/args.json"),
    )
    .unwrap();
    let error = fixture.run(THIRD_PARTY).unwrap_err();
    assert!(error.contains("not a confined package path"), "{error}");
}

#[test]
fn a_catalog_without_packages_fails() {
    let mut fixture = Fixture::new("acme.game");
    fixture.catalog["plugins"] = json!([]);
    fixture.catalog["tools"] = json!([]);
    fixture.write_catalog();
    let error = fixture.run(THIRD_PARTY).unwrap_err();
    assert!(error.contains("no packages"), "{error}");
}

#[test]
fn the_registry_context_is_strict() {
    let base = json!({"format": 1, "official_sources": [], "registrations": [],
        "namespace_revisions": [], "common_defs_schema": {"$defs": {}}});
    assert!(RegistryContext::parse(&base.to_string()).is_ok());
    let mut unknown = base.clone();
    unknown["trusted"] = json!(true);
    assert!(RegistryContext::parse(&unknown.to_string()).is_err());
    let mut format = base.clone();
    format["format"] = json!(2);
    assert!(RegistryContext::parse(&format.to_string()).is_err());
    let mut defs = base.clone();
    defs["common_defs_schema"] = json!({});
    assert!(RegistryContext::parse(&defs.to_string()).is_err());
    let mut alias = base;
    alias["official_sources"] = json!([" 18-lab-dev/vosu-tools"]);
    assert!(RegistryContext::parse(&alias.to_string()).is_err());
}

#[test]
fn arguments_are_explicit_and_absolute() {
    let absolute = |name: &str| -> String {
        Path::new(&std::env::temp_dir())
            .join(name)
            .to_string_lossy()
            .into_owned()
    };
    let full = |catalog: String| -> Vec<String> {
        vec![
            "--catalog".into(),
            catalog,
            "--packages-root".into(),
            absolute("packages"),
            "--host-version".into(),
            "2.0.0-alpha.1".into(),
            "--source".into(),
            OFFICIAL.into(),
            "--registry-context".into(),
            absolute("context.json"),
        ]
    };
    assert!(parse_args(&full(absolute("catalog-v2.json"))).is_ok());
    assert!(parse_args(&full("catalog-v2.json".into())).is_err());
    let mut missing = full(absolute("catalog-v2.json"));
    missing.truncate(8);
    assert!(parse_args(&missing).is_err());
    let mut unknown = full(absolute("catalog-v2.json"));
    unknown.push("--official".into());
    assert!(parse_args(&unknown).is_err());
}
