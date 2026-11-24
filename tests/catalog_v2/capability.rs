use vosu_plugin_api::manifest::{Capability, Requirement, candidate_satisfies};
mod truth {
    include!("../../crates/vosu-plugin-api/tests/fixtures/requirement_truth_table.rs");
}
#[test]
fn semver_resolution_literal_truth_table() {
    assert_eq!(truth::REQUIREMENT_ROWS.len(), 38);
    for &(range, version, expected) in truth::REQUIREMENT_ROWS {
        let r = Requirement::parse("field:acme.corpus.combo", range).unwrap();
        assert_eq!(
            r.satisfied_by("field:acme.corpus", version),
            expected,
            "{range} {version}"
        );
        assert!(!r.satisfied_by("field:acme.corpus2", version));
        assert!(!r.satisfied_by("resource:acme.corpus", version));
        assert!(
            !Requirement::parse("field:acme.corpus", range)
                .unwrap()
                .satisfied_by("field:acme.corpus.combo", version)
        );
    }
    for range in truth::INVALID_REQUIREMENTS {
        assert!(Requirement::parse("field:acme.corpus", range).is_err());
    }
}
#[test]
fn one_candidate_conjoins_capability_versions_not_package_releases() {
    let requirements = [
        Requirement::parse("field:acme.corpus.combo", "^1.2.0").unwrap(),
        Requirement::parse("resource:acme.corpus.replay", "^3").unwrap(),
    ];
    for (field, resource, expected) in [
        ("1.2.0", "3.0.0", true),
        ("1.9.0", "3.9.0", true),
        ("2.0.0", "3.0.0", false),
        ("1.2.0", "2.0.0", false),
        ("9.0.0", "9.0.0", false),
    ] {
        let provides = [
            Capability {
                id: "field:acme.corpus".into(),
                version: field.into(),
                schema: None,
            },
            Capability {
                id: "resource:acme.corpus.replay".into(),
                version: resource.into(),
                schema: None,
            },
        ];
        assert_eq!(candidate_satisfies(&requirements, &provides), expected);
    }
    let game = Requirement::parse("game:corpus", "*").unwrap();
    assert!(game.satisfied_by("game:corpus", "*"));
    assert!(!game.satisfied_by("game:corpus", "1.0.0"));
    assert!(Requirement::parse("game:corpus", ">=0").is_err());
    assert!(!candidate_satisfies(&[game], &[]));
    // Optional absence means no required predicate and therefore no invented install/grant.
    assert!(candidate_satisfies(&[], &[]));
}
