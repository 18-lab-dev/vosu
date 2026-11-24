use super::*;
use semver::Version;
mod shared_truth_table {
    include!("../../tests/fixtures/requirement_truth_table.rs");
}

fn base(kind: &str, id: &str) -> String {
    format!(
        "manifest_version=1\nkind='{kind}'\nid='{id}'\nname='Package'\nversion='9.0.0'\nhost='>=2.0.0, <3.0.0'\n"
    )
}
fn plugin() -> String {
    format!(
        "{}abi_version=1\nruntime='in-process'\n",
        base("plugin", "a.b")
    )
}
fn cap(id: &str, version: &str) -> Capability {
    Capability {
        id: id.into(),
        version: version.into(),
        schema: None,
    }
}
fn context<'a>(
    app: &'a Version,
    registrations: &'a [Registration<'a>],
    revisions: &'a [NamespaceRevision<'a>],
) -> ValidationContext<'a> {
    ValidationContext {
        application_version: app,
        source: "source",
        official: false,
        registrations,
        namespace_revisions: revisions,
    }
}
fn registration(id: &str) -> Registration<'_> {
    Registration {
        kind: PackageKind::Plugin,
        id,
        source: "source",
        official: false,
        builtin: false,
        replaces: &[],
    }
}
fn error(text: &str, rule: &str, pointer: &str) {
    let err = PackageManifest::parse(text).unwrap_err();
    assert_eq!(err.rule, rule, "{text}");
    assert_eq!(err.pointer, pointer, "{text}");
}

#[test]
fn manifest_three_kinds_strict_shape_defaults_and_metadata() {
    let t = PackageManifest::parse(&format!("{}[ui]\nroot='web'\n", base("tool", "tool"))).unwrap();
    let PackageManifest::Tool(tool) = t else {
        panic!("tool")
    };
    assert_eq!(tool.ui.entry, "index.html");
    assert!(tool.shared.provides.is_empty());
    assert!(PackageManifest::parse(&plugin()).is_ok());
    assert!(PackageManifest::parse(&base("overlay", "overlay")).is_ok());
    for (kind, extra) in [
        ("plugin", "[ui]\nroot='web'"),
        ("overlay", "runtime='process'"),
        ("tool", "resolution=[1,2]"),
        ("overlay", "extension=true"),
    ] {
        let text = if kind == "plugin" {
            plugin()
        } else {
            base(kind, "package")
        };
        assert_eq!(
            PackageManifest::parse(&format!("{text}{extra}\n"))
                .unwrap_err()
                .rule,
            "manifest.shape"
        );
    }
    error(&base("other", "package"), "manifest.kind", "/kind");
    error(
        &base("overlay", "overlay").replace("name='Package'", "name='  '"),
        "manifest.name",
        "/name",
    );
    for homepage in ["ftp://example.org/a", "https:", "not a URL"] {
        error(
            &format!("{}homepage='{homepage}'\n", base("overlay", "overlay")),
            "manifest.homepage",
            "/homepage",
        );
    }
    assert!(
        PackageManifest::parse(&format!(
            "{}homepage='https://example.org/'\n",
            base("overlay", "overlay")
        ))
        .is_ok()
    );
}

#[test]
fn manifest_portable_asset_paths_and_resolution_boundaries() {
    for path in [
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
        error(
            &format!("{}icon='{path}'\n", base("overlay", "overlay")),
            "manifest.path",
            "/icon",
        );
    }
    for size in ["[1,1]", "[16384,16384]"] {
        assert!(
            PackageManifest::parse(&format!(
                "{}resolution={size}\n",
                base("overlay", "overlay")
            ))
            .is_ok()
        );
    }
    for size in ["[0,1]", "[16385,1]"] {
        error(
            &format!("{}resolution={size}\n", base("overlay", "overlay")),
            "manifest.resolution",
            "/resolution/0",
        );
    }
    for size in ["[1]", "[1,2,3]", "[1.0,2]", "[-1,2]"] {
        error(
            &format!("{}resolution={size}\n", base("overlay", "overlay")),
            "manifest.shape",
            "",
        );
    }
}

#[test]
fn manifest_capability_permissions_duplicates_and_game_literals() {
    for kind in ["tool", "overlay"] {
        let extra = if kind == "tool" {
            "[ui]\nroot='web'\n"
        } else {
            ""
        };
        for id in ["field:vosu.osu.internal", "field:vosu.osu.internal.paths"] {
            error(
                &format!(
                    "{}{extra}[[requires]]\nid='{id}'\nversion='*'\n",
                    base(kind, "package")
                ),
                "manifest.permission",
                "/requires/0/id",
            );
        }
        assert!(
            PackageManifest::parse(&format!(
                "{}{extra}[[requires]]\nid='field:vosu.osu'\nversion='*'\n",
                base(kind, "package")
            ))
            .is_ok()
        );
        error(
            &format!(
                "{}{extra}[[provides]]\nid='game:osu'\nversion='*'\n",
                base(kind, "package")
            ),
            "manifest.provides-kind",
            "/provides",
        );
    }
    for id in ["game:osu", "action:a.b.import"] {
        error(
            &format!(
                "{}[[optional]]\nid='{id}'\nversion='*'\n",
                base("overlay", "overlay")
            ),
            "manifest.permission",
            "/optional/0/id",
        );
    }
    for list in ["requires", "optional"] {
        let entry = format!("[[{list}]]\nid='field:vosu.play'\nversion='*'\n");
        error(
            &format!("{}{entry}{entry}", base("overlay", "overlay")),
            "manifest.duplicate",
            &format!("/{list}/1"),
        );
    }
    error(
        &format!(
            "{}[[requires]]\nid='game:osu'\nversion='>=0'\n[ui]\nroot='web'\n",
            base("tool", "tool")
        ),
        "manifest.requirement",
        "/requires/0/version",
    );
}

#[test]
fn manifest_plugin_leaf_schema_and_resources() {
    let whole = "[[provides]]\nid='field:a.b'\nversion='1.2.0'\nschema='schema.json'\n";
    let leaf = "[[provides]]\nid='field:a.b.combo'\nversion='1.2.0'\n";
    assert!(PackageManifest::parse(&format!("{}{whole}{leaf}", plugin())).is_ok());
    error(
        &format!("{}{leaf}", plugin()),
        "manifest.own-schema",
        "/provides/0/id",
    );
    error(
        &format!("{}{whole}{}", plugin(), leaf.replace("1.2.0", "9.0.0")),
        "manifest.field-version",
        "/provides/1/version",
    );
    error(
        &format!("{}{whole}{whole}", plugin()),
        "manifest.duplicate",
        "/provides/1",
    );
    for (name, tree) in [
        ("file", false),
        ("audio", false),
        ("background", false),
        ("set", true),
        ("skin", true),
    ] {
        let kind = format!("[[resource_kinds]]\nkind='{name}'\ntree={tree}\nfamilies=['image']\n");
        assert!(PackageManifest::parse(&format!("{}{kind}", plugin())).is_ok());
        error(
            &format!(
                "{}{}",
                plugin(),
                kind.replace(&format!("tree={tree}"), &format!("tree={}", !tree))
            ),
            "manifest.resource-tree",
            "/resource_kinds/0/tree",
        );
        error(
            &format!(
                "{}{kind}[[provides]]\nid='resource:a.b.{name}'\nversion='1.0.0'\n",
                plugin()
            ),
            "manifest.resource-pair",
            "/provides/0/id",
        );
    }
    let own = "[[resource_kinds]]\nkind='replay'\nfamilies=['video']\n[[provides]]\nid='resource:a.b.replay'\nversion='3.0.0'\n";
    assert!(PackageManifest::parse(&format!("{}{own}", plugin())).is_ok());
    error(
        &format!("{}{}", plugin(), own.replace("'video'", "'text/html'")),
        "manifest.resource-family",
        "/resource_kinds/0/families",
    );
    error(
        &format!("{}[[resource_kinds]]\nkind='replay'\n", plugin()),
        "manifest.resource-pair",
        "/resource_kinds/0/kind",
    );
}

#[test]
fn manifest_namespace_registration_boundaries_and_officialness() {
    let app = Version::parse("2.0.0-alpha.1").unwrap();
    for ids in [["a.b", "a.b.c"], ["a.b.c", "a.b"]] {
        assert!(NamespaceIndex::new(&ids.map(registration)).is_err());
    }
    let registrations = [
        registration("a.b"),
        registration("a.bc"),
        registration("a.b"),
    ];
    let index = NamespaceIndex::new(&registrations).unwrap();
    assert_eq!(index.resolve("a.bc.combo"), Some(("a.bc", "combo")));
    assert_eq!(index.resolve("a.b.combo"), Some(("a.b", "combo")));
    assert_eq!(index.resolve("a.bcd.combo"), None);
    let p = PackageManifest::parse(&plugin().replace("a.b", "vosu.extra")).unwrap();
    assert_eq!(
        p.validate_context(&context(&app, &[], &[]))
            .unwrap_err()
            .rule,
        "manifest.namespace"
    );
    let mut official = context(&app, &[], &[]);
    official.official = true;
    assert_eq!(
        p.validate_context(&official).unwrap(),
        HostSupport::Unsupported
    );
    for id in [
        "chart.plugin",
        "skin.plugin",
        "vosu.play",
        "vosu.sessions",
        "a.b.c.d.e",
        "a",
    ] {
        assert!(
            PackageManifest::parse(&plugin().replace("id='a.b'", &format!("id='{id}'"))).is_err()
        );
    }
    let p = PackageManifest::parse(&plugin()).unwrap();
    assert_eq!(
        p.validate_context(&context(&app, &registrations, &[]))
            .unwrap(),
        HostSupport::Unsupported
    );
    let p = PackageManifest::parse(&plugin().replace(">=2.0.0, <3.0.0", ">=2.0.0-alpha.1, <3.0.0"))
        .unwrap();
    assert_eq!(
        p.validate_context(&context(&app, &[], &[])).unwrap(),
        HostSupport::Supported
    );
}

#[test]
fn manifest_foreign_leaf_requires_authoritative_revision() {
    let app = Version::new(2, 0, 0);
    let text = format!(
        "{}[[provides]]\nid='field:vosu.play.combo'\nversion='1.0.0'\n",
        plugin()
    );
    let p = PackageManifest::parse(&text).unwrap();
    assert_eq!(
        p.validate_context(&context(&app, &[], &[]))
            .unwrap_err()
            .rule,
        "manifest.authoritative-version"
    );
    let mut revisions = [NamespaceRevision {
        namespace: "vosu.play",
        version: Version::new(1, 0, 0),
        provenance: SchemaProvenance::TrustedBundled,
    }];
    assert_eq!(
        p.validate_context(&context(&app, &[], &revisions)).unwrap(),
        HostSupport::Supported
    );
    revisions[0].version = Version::new(1, 1, 0);
    assert!(p.validate_context(&context(&app, &[], &revisions)).is_err());
    revisions[0].version = Version::new(1, 0, 0);
    revisions[0].provenance = SchemaProvenance::ThirdParty;
    assert!(p.validate_context(&context(&app, &[], &revisions)).is_err());
}

#[test]
fn manifest_source_local_replaces_claims_and_kind_independence() {
    let app = Version::new(2, 0, 0);
    let p =
        PackageManifest::parse(&format!("{}replaces=['old']\n", base("overlay", "new"))).unwrap();
    let mut registrations = [Registration {
        kind: PackageKind::Overlay,
        id: "old",
        source: "source",
        official: false,
        builtin: false,
        replaces: &[],
    }];
    assert!(
        p.validate_context(&context(&app, &registrations, &[]))
            .is_err()
    );
    registrations[0].kind = PackageKind::Tool;
    assert!(
        p.validate_context(&context(&app, &registrations, &[]))
            .is_ok()
    );
    registrations[0].kind = PackageKind::Overlay;
    registrations[0].source = "another";
    assert!(
        p.validate_context(&context(&app, &registrations, &[]))
            .is_ok()
    );
    registrations[0].builtin = true;
    assert!(
        p.validate_context(&context(&app, &registrations, &[]))
            .is_err()
    );
    for value in ["['new']", "['old','old']", "['invalid.id']"] {
        assert!(
            PackageManifest::parse(&format!("{}replaces={value}\n", base("overlay", "new")))
                .is_err()
        );
    }
}

#[test]
fn manifest_matcher_game_equivalence_portable_hints_and_regex() {
    error(
        &format!("{}[match]\n", plugin()),
        "manifest.game-matcher",
        "/match",
    );
    let game = format!("{}[[provides]]\nid='game:test'\nversion='*'\n", plugin());
    error(&game, "manifest.game-matcher", "/match");
    for field in ["comm_hints", "linux_name_contains"] {
        for hint in ["a", "123456789012345", "éééééééa"] {
            assert!(
                PackageManifest::parse(&format!("{game}[match]\n{field}=['{hint}']\n")).is_ok()
            );
        }
        for hint in ["", "1234567890123456", "éééééééé"] {
            error(
                &format!("{game}[match]\n{field}=['{hint}']\n"),
                "manifest.matcher-hint",
                &format!("/match/{field}/0"),
            );
        }
        let mut matcher = MatchTable::default();
        if field == "comm_hints" {
            matcher.comm_hints.push("x\0".into());
        } else {
            matcher.linux_name_contains.push("x\0".into());
        }
        assert_eq!(
            matcher.validate_document().unwrap_err().rule,
            "manifest.matcher-hint"
        );
    }
    for pattern in [r".*\\osu!\.exe", r".*\d+", "(?i)game"] {
        assert!(
            PackageManifest::parse(&format!(
                "{game}[match]\nprocess_path_patterns=['{pattern}']\n"
            ))
            .is_ok()
        );
    }
    error(
        &format!("{game}[match]\ncommand_line_patterns=['[']\n"),
        "manifest.matcher-regex",
        "/match/command_line_patterns/0",
    );
}

#[test]
fn manifest_requirement_semver_truth_table_and_candidate_conjunction() {
    for (req, version, expected) in [
        ("=1.2.3", "1.2.3", true),
        ("=1.2.3", "1.2.4", false),
        ("^1.2.0", "1.2.0", true),
        ("^1.2.0", "1.9.0", true),
        ("^1.2.0", "2.0.0", false),
        ("^0.2.3", "0.2.9", true),
        ("^0.2.3", "0.3.0", false),
        ("^0.0.3", "0.0.3", true),
        ("^0.0.3", "0.0.4", false),
        ("~1.2.3", "1.2.9", true),
        ("~1.2.3", "1.3.0", false),
        ("1.*", "1.9.0", true),
        ("1.*", "2.0.0", false),
        (">=1.2.0, <2.0.0", "1.1.9", false),
        (">=1.2.0, <2.0.0", "1.2.0", true),
        (">=1.2.0, <2.0.0", "2.0.0", false),
        (">=2.0.0, <3.0.0", "2.0.0-alpha.1", false),
        (">=2.0.0-alpha.1, <3.0.0", "2.0.0-alpha.1", true),
        (">=2.0.0-alpha.1, <3.0.0", "2.1.0-alpha.1", false),
        ("^1.2.0", "1.9.0+build", true),
        ("^1.2.0", "2.0.0+build", false),
    ] {
        let r = Requirement::parse("field:a.b.combo", req).unwrap();
        assert_eq!(
            r.satisfied_by("field:a.b", version),
            expected,
            "{req} {version}"
        );
        assert!(!r.satisfied_by("resource:a.b.combo", version));
        assert!(!r.satisfied_by("field:a.bc", version));
    }
    for invalid in ["1 || 2", "1.0.0 - 2.0.0", "nonsense"] {
        assert!(Requirement::parse("field:a.b", invalid).is_err());
    }
    assert!(
        !Requirement::parse("field:a.b", "*")
            .unwrap()
            .satisfied_by("field:a.b.combo", "1.0.0")
    );
    let requirements = [
        Requirement::parse("field:a.b.combo", "^1").unwrap(),
        Requirement::parse("resource:a.b.replay", "^3").unwrap(),
    ];
    assert!(candidate_satisfies(
        &requirements,
        &[
            cap("field:a.b", "1.2.0"),
            cap("resource:a.b.replay", "3.0.0")
        ]
    ));
    assert!(!candidate_satisfies(
        &requirements,
        &[
            cap("field:a.b", "1.2.0"),
            cap("resource:a.b.replay", "2.0.0")
        ]
    ));
    let game = Requirement::parse("game:osu", "*").unwrap();
    assert!(game.satisfied_by("game:osu", "*"));
    assert!(!game.satisfied_by("game:osu", "1.0.0"));
    assert!(Requirement::parse("game:osu", ">=0").is_err());
    assert!(!candidate_satisfies(&[game], &[]));
}

#[test]
fn manifest_action_bijection_swapped_versions_and_session_modes() {
    let mut p = crate::manifest::PackageManifest::parse(&plugin())
        .and_then(crate::manifest::PackageManifest::into_plugin)
        .unwrap();
    for (i, session) in [
        ActionSessionMode::Required,
        ActionSessionMode::Optional,
        ActionSessionMode::None,
    ]
    .into_iter()
    .enumerate()
    {
        let name = format!("beatmap.import{i}");
        let version = format!("{}.0.0", i + 1);
        p.actions.push(ActionDeclaration {
            name: name.clone(),
            version: version.clone(),
            title: "Import".into(),
            args_schema: "schemas/args.json".into(),
            result_schema: "schemas/result.json".into(),
            session,
            cli: false,
        });
        p.provides
            .push(cap(&format!("action:a.b.{name}"), &version));
    }
    assert!(p.validate().is_ok());
    p.provides.swap(0, 1);
    assert!(p.validate().is_ok());
    let first = p.provides[0].version.clone();
    p.provides[0].version = p.provides[1].version.clone();
    p.provides[1].version = first;
    assert!(matches!(p.validate(), Err(ManifestError::Action(_))));
    p.provides[0].version = "2.0.0".into();
    p.provides[1].version = "1.0.0".into();
    p.actions.pop();
    assert!(matches!(p.validate(), Err(ManifestError::Action(_))));
}

#[test]
fn manifest_duplicate_slots_covers_and_kinds_rejected() {
    for (extra, path) in [
        ("root_slots=['songs','songs']\n", "/root_slots/1"),
        (
            "covers=['field:vosu.play','field:vosu.play']\n",
            "/covers/1",
        ),
        (
            "[[resource_kinds]]\nkind='file'\n[[resource_kinds]]\nkind='file'\n",
            "/resource_kinds/1",
        ),
    ] {
        error(&format!("{}{extra}", plugin()), "manifest.duplicate", path);
    }
    error(
        &format!("{}[targets.any]\nlibrary='plugin.so'\n", plugin()),
        "manifest.target",
        "/targets/any",
    );
    let app = Version::new(2, 0, 0);
    for id in ["action:vosu.play.combo", "resource:vosu.play.combo"] {
        let p = PackageManifest::parse(&format!(
            "{}[[requires]]\nid='{id}'\nversion='*'\n",
            plugin()
        ))
        .unwrap();
        assert_eq!(
            p.validate_context(&context(&app, &[], &[]))
                .unwrap_err()
                .rule,
            "manifest.capability-owner"
        );
    }
}

#[test]
fn manifest_game_self_dependency_and_resource_leaf_boundary() {
    let game = format!(
        "{}[[provides]]\nid='game:test'\nversion='*'\n[[requires]]\nid='game:test'\nversion='*'\n[match]\n",
        plugin()
    );
    error(&game, "manifest.self-dependency", "/requires/0/id");
    let app = Version::new(2, 0, 0);
    let p = PackageManifest::parse(&format!(
        "{}[[requires]]\nid='resource:a.c.kind.child'\nversion='*'\n",
        plugin()
    ))
    .unwrap();
    let registrations = [registration("a.c")];
    assert_eq!(
        p.validate_context(&context(&app, &registrations, &[]))
            .unwrap_err()
            .rule,
        "manifest.capability-boundary"
    );
}

#[test]
fn manifest_shared_requirement_resolution_truth_table() {
    use shared_truth_table::{INVALID_REQUIREMENTS, REQUIREMENT_ROWS};
    for &(range, version, expected) in REQUIREMENT_ROWS {
        let requirement = Requirement::parse("field:acme.corpus.combo", range).unwrap();
        assert_eq!(
            requirement.satisfied_by("field:acme.corpus", version),
            expected,
            "{range} {version}"
        );
    }
    for range in INVALID_REQUIREMENTS {
        assert!(
            Requirement::parse("field:acme.corpus", range).is_err(),
            "{range}"
        );
    }
}
