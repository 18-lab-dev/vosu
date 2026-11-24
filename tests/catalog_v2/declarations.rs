use super::support::*;
use serde_json::json;
pub fn corpus() -> Vec<Case> {
    let base = manifest("plugin");
    let mut rows = Vec::new();
    let mut good = base.clone();
    good["actions"] = json!([
        {"name":"beatmap.import","version":"2.0.0","title":"Import","args_schema":"args.json","result_schema":"result.json","session":"required"},
        {"name":"cache.clear","version":"3.0.0","title":"Clear","args_schema":"args.json","result_schema":"result.json","session":"none"}]);
    good["provides"] = json!([{"id":"action:acme.corpus.beatmap.import","version":"2.0.0"},
        {"id":"action:acme.corpus.cache.clear","version":"3.0.0"}]);
    for (path, bad) in [
        ("/actions/0/version", json!("3.0.0")),
        ("/actions/0/title", json!(" ")),
        ("/actions/0/name", json!("beatmap.Bad")),
        ("/actions/0/args_schema", json!("../args.json")),
        ("/actions/0/result_schema", json!("C:/result.json")),
        ("/provides", json!([good["provides"][0].clone()])),
        ("/actions", json!([good["actions"][0].clone()])),
    ] {
        rows.push(Case::mutation(
            format!("action:{path}:{bad}"),
            &good,
            path,
            bad,
            "manifest.action",
            "/actions",
            false,
        ));
    }
    let mut own = base.clone();
    own["resource_kinds"] = json!([{"kind":"replay","tree":false,"families":["video"]}]);
    own["provides"] = json!([{"id":"resource:acme.corpus.replay","version":"3.0.0"}]);
    rows.push(Case::mutation(
        "custom-resource-family",
        &own,
        "/resource_kinds/0/families",
        json!(["text/html"]),
        "manifest.resource-family",
        "/resource_kinds/0/families",
        false,
    ));
    rows.push(Case::mutation(
        "custom-resource-missing-provide",
        &own,
        "/provides",
        json!([]),
        "manifest.resource-pair",
        "/resource_kinds/0/kind",
        false,
    ));
    rows.push(Case::mutation(
        "custom-resource-missing-kind",
        &own,
        "/resource_kinds",
        json!([]),
        "manifest.resource-pair",
        "/provides/0/id",
        false,
    ));
    for prefix in ["field", "resource", "action", "game"] {
        let mut control = base.clone();
        control["requires"] = json!([{"id":"field:vosu.play","version":"*"}]);
        rows.push(Case::mutation(
            format!("reserved-provide:{prefix}"),
            &control,
            "/requires/0/id",
            json!(format!("{prefix}:vosu.sessions.child")),
            "manifest.capability",
            "/requires/0/id",
            false,
        ));
    }
    rows
}
#[test]
fn action_resource_declarations_use_shared_rules() {
    let schema = offline("manifest");
    for row in corpus() {
        assert!(parse(&row.valid).is_ok(), "{} control", row.label);
        assert!(schema.is_valid(&row.valid), "{} offline control", row.label);
        let error = parse(&row.invalid).unwrap_err();
        assert_eq!(error.rule, row.rule, "{}", row.label);
        assert_eq!(error.pointer, row.pointer, "{}", row.label);
    }
}
