use super::*;
use crate::manifest::{PackageKind, PackageManifest};
use serde_json::json;

#[test]
fn catalog_mirror_plugin_structured_fields_are_all_checked() {
    let manifest = PackageManifest::parse(
        r#"
manifest_version=1
kind='plugin'
id='acme.game'
name='Game'
version='9.0.0'
host='^2'
abi_version=1
runtime='process'
covers=['field:vosu.play.combo']
[match]
process_names=['Game.exe']
comm_hints=['game']
linux_name_contains=['game']
[[provides]]
id='game:acme-game'
version='*'
[[provides]]
id='action:acme.game.song.import'
version='2.0.0'
[[provides]]
id='resource:acme.game.replay'
version='3.0.0'
[[actions]]
name='song.import'
version='2.0.0'
title='Import'
args_schema='args.json'
result_schema='result.json'
session='required'
cli=true
[[resource_kinds]]
kind='replay'
tree=false
families=['chart-text']
[[optional]]
id='field:vosu.play.combo'
version='^1'
"#,
    )
    .unwrap();
    let catalog = Catalog::parse(&json!({"catalog_version":2,"name":"Catalog","published":"2026-11-24T00:00:00Z",
        "plugins":[{"id":"acme.game","name":"Game","version":"9.0.0","host":"^2","runtime":"process",
            "covers":["field:vosu.play.combo"],"matcher":{"process_names":["Game.exe"],"comm_hints":["game"],"linux_name_contains":["game"]},
            "provides":[{"id":"game:acme-game","version":"*"},{"id":"action:acme.game.song.import","version":"2.0.0"},{"id":"resource:acme.game.replay","version":"3.0.0"}],
            "actions":[{"name":"song.import","version":"2.0.0","title":"Import","args_schema":"args.json","result_schema":"result.json","session":"required","cli":true}],
            "resource_kinds":[{"kind":"replay","tree":false,"families":["chart-text"]}],
            "optional":[{"id":"field:vosu.play.combo","version":"^1"}],
            "packages":{"x86_64-unknown-linux-gnu":{"url":"https://example.org/p.zip","sha256":"a".repeat(64),"size":1}}}]
    }).to_string()).unwrap();
    let entry = &catalog.plugins[0];
    assert!(compare_package(PackageKind::Plugin, entry, &manifest, &[]).is_ok());
    for (path, mut changed) in [
        ("/matcher", entry.clone()),
        ("/actions", entry.clone()),
        ("/resource_kinds", entry.clone()),
        ("/optional", entry.clone()),
        ("/provides/0", entry.clone()),
        ("/id", entry.clone()),
    ] {
        match path {
            "/matcher" => changed
                .matcher
                .as_mut()
                .unwrap()
                .linux_name_contains
                .clear(),
            "/actions" => changed.actions.as_mut().unwrap()[0].title = "Changed".to_owned(),
            "/resource_kinds" => {
                changed.resource_kinds.as_mut().unwrap()[0].families = vec!["image".to_owned()]
            }
            "/optional" => changed.optional[0].version = "^2".to_owned(),
            "/provides/0" => changed.provides.swap(0, 1),
            "/id" => {
                changed.id = "other.game".to_owned();
                changed.provides[1].id = "action:other.game.song.import".to_owned();
                changed.provides[2].id = "resource:other.game.replay".to_owned();
            }
            _ => unreachable!(),
        }
        assert_eq!(
            compare_package(PackageKind::Plugin, &changed, &manifest, &[])
                .unwrap_err()
                .pointer,
            path
        );
    }
    assert_eq!(
        compare_package(PackageKind::Tool, entry, &manifest, &[])
            .unwrap_err()
            .rule,
        "catalog.kind-field"
    );
}
