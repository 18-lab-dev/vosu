use super::support::*;
use serde_json::json;

pub fn corpus() -> Vec<Case> {
    let mut rows = Vec::new();
    for kind in ["plugin", "tool", "overlay"] {
        let base = manifest(kind);
        for key in ["manifest_version", "kind", "id", "name", "version", "host"] {
            let mut invalid = base.clone();
            invalid.as_object_mut().unwrap().remove(key);
            rows.push(Case {
                label: format!("{kind}:missing:{key}"),
                valid: base.clone(),
                invalid,
                rule: "manifest.shape",
                pointer: if key == "kind" {
                    "/kind".into()
                } else {
                    String::new()
                },
                shape: true,
            });
        }
        rows.push(Case::mutation(
            format!("{kind}:invalid-kind"),
            &base,
            "/kind",
            json!("other"),
            "manifest.kind",
            "/kind",
            true,
        ));
        if kind == "tool" {
            let mut control = base.clone();
            control["ui"]["entry"] = json!("index.html");
            for (path, bad) in [("/ui/root", "../web"), ("/ui/entry", "../index.html")] {
                rows.push(Case::mutation(
                    format!("tool:{path}"),
                    &control,
                    path,
                    json!(bad),
                    "manifest.path",
                    path,
                    true,
                ));
            }
        }
        for (path, bad, rule, shape) in [
            ("/manifest_version", json!(2), "manifest.format", true),
            ("/name", json!("  "), "manifest.name", true),
            ("/version", json!("1.0"), "manifest.version", true),
            ("/host", json!("1 || 2"), "manifest.host", false),
            ("/id", json!("Bad_ID"), "manifest.id", true),
        ] {
            rows.push(Case::mutation(
                format!("{kind}:{path}"),
                &base,
                path,
                bad,
                rule,
                path,
                shape,
            ));
        }
        for (key, bad, rule, pointer, shape) in [
            (
                "homepage",
                json!("ftp://example.org/"),
                "manifest.homepage",
                "/homepage",
                true,
            ),
            ("extension", json!(true), "manifest.shape", "", true),
        ] {
            rows.push(Case::addition(
                format!("{kind}:{key}"),
                &base,
                key,
                bad,
                rule,
                pointer,
                shape,
            ));
        }
        for key in ["icon", "banner"] {
            let mut good = base.clone();
            good[key] = json!("art/icon.svg");
            for bad in [
                "",
                "/absolute",
                "../escape",
                "a/../b",
                "./a",
                "a//b",
                "a/",
                "C:/file",
                "a\\b",
            ] {
                rows.push(Case::mutation(
                    format!("{kind}:{key}:{bad}"),
                    &good,
                    &format!("/{key}"),
                    json!(bad),
                    "manifest.path",
                    &format!("/{key}"),
                    true,
                ));
            }
        }
        for list in ["requires", "optional"] {
            let mut good = base.clone();
            good[list] = json!([{"id":"field:vosu.play.combo","version":"^1.0.0"}]);
            for id in [
                "unknown:vosu.play",
                "field:vosu.sessions",
                "resource:vosu.sessions",
                "action:vosu.sessions",
                "field:vosu.sessions.child",
                "resource:vosu.sessions.child",
                "action:vosu.sessions.child",
                "game:vosu.sessions",
            ] {
                rows.push(Case::mutation(
                    format!("{kind}:{list}:{id}"),
                    &good,
                    &format!("/{list}/0/id"),
                    json!(id),
                    "manifest.capability",
                    &format!("/{list}/0/id"),
                    id.starts_with("unknown:"),
                ));
            }
            let duplicate = json!([good[list][0].clone(), good[list][0].clone()]);
            rows.push(Case::mutation(
                format!("{kind}:{list}:duplicate"),
                &good,
                &format!("/{list}"),
                duplicate,
                "manifest.duplicate",
                &format!("/{list}/1"),
                false,
            ));
            if kind != "plugin" {
                for id in ["field:vosu.osu.internal", "field:vosu.osu.internal.paths"] {
                    rows.push(Case::mutation(
                        format!("{kind}:{list}:internal:{id}"),
                        &good,
                        &format!("/{list}/0/id"),
                        json!(id),
                        "manifest.permission",
                        &format!("/{list}/0/id"),
                        false,
                    ));
                }
            }
            if kind == "overlay" {
                for id in ["game:osu", "action:acme.corpus.import"] {
                    let mut control = good.clone();
                    control[list][0]["version"] = json!("*");
                    rows.push(Case::mutation(
                        format!("overlay:{list}:{id}"),
                        &control,
                        &format!("/{list}/0/id"),
                        json!(id),
                        "manifest.permission",
                        &format!("/{list}/0/id"),
                        false,
                    ));
                }
            }
        }
        if kind != "plugin" {
            rows.push(Case::addition(
                format!("{kind}:provides"),
                &base,
                "provides",
                json!([{"id":"game:osu","version":"*"}]),
                "manifest.provides-kind",
                "/provides",
                true,
            ));
            rows.push(Case::addition(
                format!("{kind}:runtime"),
                &base,
                "runtime",
                json!("process"),
                "manifest.shape",
                "",
                true,
            ));
        }
    }
    let overlay = manifest("overlay");
    for (size, pointer) in [
        (json!([0, 1]), "/resolution/0"),
        (json!([16385, 1]), "/resolution/0"),
        (json!([1, 0]), "/resolution/1"),
        (json!([1, 16385]), "/resolution/1"),
    ] {
        let mut good = overlay.clone();
        good["resolution"] = json!([1, 16384]);
        rows.push(Case::mutation(
            format!("resolution:{size}"),
            &good,
            "/resolution",
            size,
            "manifest.resolution",
            pointer,
            true,
        ));
    }
    for kind in ["tool", "plugin"] {
        let mut good = manifest(kind);
        good["requires"] = json!([{"id":"game:osu","version":"*"}]);
        rows.push(Case::mutation(
            format!("{kind}:literal-game"),
            &good,
            "/requires/0/version",
            json!(">=0"),
            if kind == "plugin" {
                "manifest.game-version"
            } else {
                "manifest.requirement"
            },
            "/requires/0/version",
            false,
        ));
    }
    rows
}

#[test]
fn manifest_rule_matrix_sdk_and_offline_schema() {
    let schema = offline("manifest");
    let rows = corpus();
    assert!(rows.len() >= 120);
    for row in rows {
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
