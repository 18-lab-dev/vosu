use super::support::*;
use serde_json::json;

pub fn corpus() -> Vec<Case> {
    let base = manifest("plugin");
    let mut rows = Vec::new();
    for id in [
        "a",
        "a.b.c.d.e",
        "chart.plugin",
        "skin.plugin",
        "vosu.play",
        "vosu.sessions",
    ] {
        rows.push(Case::mutation(
            format!("plugin-id:{id}"),
            &base,
            "/id",
            json!(id),
            "manifest.id",
            "/id",
            false,
        ));
    }
    for key in ["ui", "service", "permissions"] {
        rows.push(Case::addition(
            format!("cross-kind:{key}"),
            &base,
            key,
            json!({}),
            "manifest.shape",
            "",
            true,
        ));
    }
    rows.push(Case::mutation(
        "abi",
        &base,
        "/abi_version",
        json!(2),
        "manifest.abi",
        "/abi_version",
        true,
    ));
    let mut whole = base.clone();
    whole["provides"] = json!([{"id":"field:acme.corpus","version":"1.2.0","schema":"schema.json"},
        {"id":"field:acme.corpus.combo","version":"1.2.0"}]);
    rows.push(Case::mutation(
        "field-version",
        &whole,
        "/provides/1/version",
        json!("9.0.0"),
        "manifest.field-version",
        "/provides/1/version",
        false,
    ));
    rows.push(Case::mutation(
        "missing-whole",
        &whole,
        "/provides",
        json!([whole["provides"][1].clone()]),
        "manifest.own-schema",
        "/provides/0/id",
        false,
    ));
    rows.push(Case::addition(
        "self-dependency",
        &whole,
        "requires",
        json!([{"id":"field:acme.corpus.combo","version":"^1"}]),
        "manifest.self-dependency",
        "/requires/0/id",
        false,
    ));
    for (key, good, bad, pointer) in [
        (
            "root_slots",
            json!(["songs"]),
            json!(["songs", "songs"]),
            "/root_slots/1",
        ),
        (
            "covers",
            json!(["field:vosu.play"]),
            json!(["field:vosu.play", "field:vosu.play"]),
            "/covers/1",
        ),
        (
            "resource_kinds",
            json!([{"kind":"file"}]),
            json!([{"kind":"file"},{"kind":"file"}]),
            "/resource_kinds/1",
        ),
    ] {
        let mut control = base.clone();
        control[key] = good;
        rows.push(Case::mutation(
            format!("duplicate:{key}"),
            &control,
            &format!("/{key}"),
            bad,
            "manifest.duplicate",
            pointer,
            false,
        ));
    }
    let mut cover = base.clone();
    cover["covers"] = json!(["field:vosu.play.combo"]);
    for id in [
        "field:other.plugin",
        "field:vosu.chart.resources",
        "field:vosu.skin.tree.foo",
    ] {
        rows.push(Case::mutation(
            format!("covers:{id}"),
            &cover,
            "/covers/0",
            json!(id),
            "manifest.coverage",
            "/covers",
            false,
        ));
    }
    for (name, tree) in [
        ("file", false),
        ("audio", false),
        ("background", false),
        ("set", true),
        ("skin", true),
    ] {
        let mut control = base.clone();
        control["resource_kinds"] = json!([{"kind":name,"tree":tree,"families":["image"]}]);
        rows.push(Case::mutation(
            format!("resource-tree:{name}"),
            &control,
            "/resource_kinds/0/tree",
            json!(!tree),
            "manifest.resource-tree",
            "/resource_kinds/0/tree",
            false,
        ));
    }
    let mut game = base.clone();
    game["provides"] = json!([{"id":"game:corpus","version":"*"}]);
    game["match"] = json!({});
    rows.push(Case::mutation(
        "game-literal",
        &game,
        "/provides/0/version",
        json!(">=0"),
        "manifest.game-version",
        "/provides/0/version",
        false,
    ));
    rows.push(Case::addition(
        "nongame-matcher",
        &base,
        "match",
        json!({}),
        "manifest.game-matcher",
        "/match",
        false,
    ));
    for field in ["comm_hints", "linux_name_contains"] {
        let mut control = game.clone();
        control["match"][field] = json!(["éééééééa"]);
        for hint in ["", "1234567890123456", "éééééééé", "a\0"] {
            rows.push(Case::mutation(
                format!("hint:{field}:{hint:?}"),
                &control,
                &format!("/match/{field}/0"),
                json!(hint),
                "manifest.matcher-hint",
                &format!("/match/{field}/0"),
                false,
            ));
        }
    }
    for field in ["process_path_patterns", "command_line_patterns"] {
        let mut control = game.clone();
        control["match"][field] = json!([".*game"]);
        rows.push(Case::mutation(
            format!("regex:{field}"),
            &control,
            &format!("/match/{field}/0"),
            json!("["),
            "manifest.matcher-regex",
            &format!("/match/{field}/0"),
            false,
        ));
    }
    rows
}

#[test]
fn plugin_rule_matrix_sdk_and_offline_schema() {
    let schema = offline("manifest");
    for row in corpus() {
        assert!(parse(&row.valid).is_ok(), "{} control", row.label);
        assert!(schema.is_valid(&row.valid), "{} offline control", row.label);
        let error = parse(&row.invalid).unwrap_err();
        assert_eq!(error.rule, row.rule, "{}", row.label);
        assert_eq!(error.pointer, row.pointer, "{}", row.label);
        if row.shape {
            assert!(!schema.is_valid(&row.invalid), "{} shape", row.label);
        }
    }
}
