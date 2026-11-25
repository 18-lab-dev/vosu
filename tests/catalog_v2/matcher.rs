use serde_json::json;
use vosu_core::plugin::{
    CandidateProcess, CompiledMatcher, MatchPlatform, MatcherEngine, RegisterError, overlaps,
};
use vosu_plugin_api::MatchTable;
#[test]
fn matcher_portable_document_bounds_and_runtime_platform_truth_table() {
    for platform in [MatchPlatform::Linux, MatchPlatform::Windows] {
        for (hint, valid) in [
            ("", false),
            ("a", true),
            ("123456789012345", true),
            ("1234567890123456", false),
            ("éééééééa", true),
            ("éééééééé", false),
            ("a\0", false),
        ] {
            for field in ["comm_hints", "linux_name_contains"] {
                let mut value = json!({});
                value[field] = json!([hint]);
                let table: MatchTable = serde_json::from_value(value).unwrap();
                assert_eq!(table.validate_document().is_ok(), valid);
                assert_eq!(
                    CompiledMatcher::compile(platform, &table).is_ok(),
                    valid,
                    "{platform:?}:{hint:?}"
                );
            }
        }
        let candidate = CandidateProcess {
            name: "mygame".into(),
            path: "/opt/mygame".into(),
            command_line: String::new(),
        };
        let hints = MatchTable {
            comm_hints: vec!["mygame".into()],
            ..Default::default()
        };
        assert!(
            !CompiledMatcher::compile(platform, &hints)
                .unwrap()
                .matches(&candidate)
        );
        let names = MatchTable {
            linux_name_contains: vec!["game".into()],
            ..Default::default()
        };
        assert_eq!(
            CompiledMatcher::compile(platform, &names)
                .unwrap()
                .matches(&candidate),
            platform == MatchPlatform::Linux
        );
        let anchored = MatchTable {
            process_path_patterns: vec!["/opt/mygame".into()],
            ..Default::default()
        };
        let matcher = CompiledMatcher::compile(platform, &anchored).unwrap();
        assert!(matcher.matches(&candidate));
        let extended = CandidateProcess {
            path: "/opt/mygame.backup".into(),
            ..candidate
        };
        assert!(!matcher.matches(&extended));
    }
}
#[test]
fn catalog_alternatives_can_overlap_but_enabled_set_is_bounded() {
    let table = MatchTable {
        comm_hints: vec!["mygame".into()],
        ..Default::default()
    };
    let mut entry = super::catalog::entry("plugins");
    entry["provides"] = json!([{"id":"game:corpus","version":"*"}]);
    entry["matcher"] = json!({"comm_hints":["mygame"]});
    let mut second = entry.clone();
    second["id"] = json!("other.plugin");
    let value = json!({"catalog_version":2,"name":"Alternatives","published":"informational","plugins":[entry,second]});
    assert!(vosu_plugin_api::catalog::Catalog::parse(&value.to_string()).is_ok());
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    engine
        .register("first.plugin".into(), table.clone())
        .unwrap();
    assert!(matches!(
        engine.register("other.plugin".into(), table),
        Err(RegisterError::Overlap(_))
    ));
    let a = MatchTable {
        linux_name_contains: vec!["osu!".into()],
        ..Default::default()
    };
    let b = MatchTable {
        linux_name_contains: vec!["lazer".into()],
        ..Default::default()
    };
    assert!(!overlaps(MatchPlatform::Linux, &a, &b));
    let mut engine = MatcherEngine::new(MatchPlatform::Linux);
    let bounded = MatchTable {
        comm_hints: (0..256).map(|i| format!("hint{i:03}")).collect(),
        ..Default::default()
    };
    engine.register("first.plugin".into(), bounded).unwrap();
    let excess = MatchTable {
        comm_hints: vec!["unique257".into()],
        ..Default::default()
    };
    assert!(matches!(
        engine.register("other.plugin".into(), excess),
        Err(RegisterError::PreFilterLimit(_))
    ));
}
