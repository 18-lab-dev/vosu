use super::*;
use crate::catalog::sources::SourceIdentity;
use semver::Version;
use serde_json::{Value, json};
use vosu_plugin_api::{
    catalog::{Catalog, CatalogEntry},
    manifest::{PackageKind, PackageManifest},
};
const TARGET: &str = "x86_64-unknown-linux-gnu";
fn source(value: &str) -> SourceIdentity {
    SourceIdentity::parse(value).unwrap().identity
}
fn cap(id: &str, version: &str) -> Value {
    json!({"id":id,"version":version})
}
fn plugin(id: &str, version: &str, schema: &str, requires: Vec<Value>) -> CatalogEntry {
    entry(
        PackageKind::Plugin,
        json!({"id":id,"name":id,"version":version,"host":"^2.0.0","runtime":"process",
        "provides":[{"id":format!("field:{id}"),"version":schema,"schema_version":schema,"lanes":[]}],
        "requires":requires,"packages":{TARGET:{"url":"https://example.org/p.zip","sha256":"a".repeat(64),"size":10}}}),
    )
}
fn tool(id: &str, requires: Vec<Value>, optional: Vec<Value>) -> CatalogEntry {
    entry(
        PackageKind::Tool,
        json!({"id":id,"name":id,"version":"1.0.0","host":"^2.0.0","requires":requires,"optional":optional,
        "packages":{"any":{"url":"https://example.org/t.zip","sha256":"b".repeat(64),"size":20}}}),
    )
}
fn entry(kind: PackageKind, entry: Value) -> CatalogEntry {
    let array = match kind {
        PackageKind::Plugin => "plugins",
        PackageKind::Tool => "tools",
        PackageKind::Overlay => "overlays",
    };
    let mut document =
        json!({"catalog_version":2,"name":"Fixture","published":"2026-11-24T00:00:00Z"});
    document[array] = json!([entry]);
    let mut catalog = Catalog::parse(&document.to_string()).unwrap();
    match kind {
        PackageKind::Plugin => catalog.plugins.remove(0),
        PackageKind::Tool => catalog.tools.remove(0),
        PackageKind::Overlay => catalog.overlays.remove(0),
    }
}
fn candidate<'a>(
    kind: PackageKind,
    entry: &'a CatalogEntry,
    source: &'a SourceIdentity,
    order: usize,
) -> Candidate<'a> {
    Candidate {
        kind,
        entry,
        source,
        source_order: order,
        catalog_order: 0,
        trusted: false,
    }
}
fn solve(
    candidates: &[Candidate<'_>],
    installed: &[Installed<'_>],
    builtins: &BuiltinProviders,
) -> Resolution {
    let application = Version::new(2, 1, 0);
    let snapshot = Snapshot {
        candidates,
        installed,
        builtins,
        application: &application,
        target: TARGET,
        max_package_bytes: 100,
    };
    let root = candidates[0];
    resolve(
        &snapshot,
        &[Request {
            kind: root.kind,
            id: &root.entry.id,
            source: root.source,
        }],
    )
}
#[test]
fn ranked_backtracking_finds_first_complete_not_newest() {
    let src = source("acme/catalog");
    let other = source("acme/other");
    let root = tool("root", vec![cap("field:acme.provider", "*")], vec![]);
    let dead = plugin(
        "acme.provider",
        "9.0.0",
        "1.0.0",
        vec![cap("field:acme.missing", "^1")],
    );
    let good = plugin("acme.provider", "1.0.0", "1.0.0", vec![]);
    let newer = plugin("acme.provider", "99.0.0", "1.0.0", vec![]);
    let candidates = [
        candidate(PackageKind::Tool, &root, &src, 0),
        candidate(PackageKind::Plugin, &dead, &src, 0),
        candidate(PackageKind::Plugin, &good, &other, 1),
        candidate(PackageKind::Plugin, &newer, &other, 2),
    ];
    let result = solve(&candidates, &[], &BuiltinProviders::default());
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.adds, [2, 0]);
}
#[test]
fn exact_official_beats_source_order_and_alias_does_not() {
    let src = source("acme/catalog");
    let official = source("18-lab-dev/vosu-tools");
    let alias = source("https://github.com/18-lab-dev/vosu-tools");
    let root = tool("root", vec![cap("field:acme.provider", "^1")], vec![]);
    let p = plugin("acme.provider", "1.0.0", "1.0.0", vec![]);
    let candidates = [
        candidate(PackageKind::Tool, &root, &src, 0),
        candidate(PackageKind::Plugin, &p, &alias, 0),
        candidate(PackageKind::Plugin, &p, &official, 5),
    ];
    assert_eq!(
        solve(&candidates, &[], &BuiltinProviders::default()).adds,
        [2, 0]
    );
    assert_eq!(
        solve(&candidates[..2], &[], &BuiltinProviders::default()).adds,
        [1, 0]
    );
}
#[test]
fn one_plugin_assignment_conjoins_distinct_capabilities() {
    let src = source("acme/catalog");
    let other = source("acme/other");
    let root = tool(
        "root",
        vec![
            cap("field:acme.provider", "^1"),
            cap("action:acme.provider.run", "^2"),
        ],
        vec![],
    );
    let mut first = plugin("acme.provider", "1.0.0", "1.0.0", vec![]);
    first.provides.push(
        serde_json::from_value(json!({"id":"action:acme.provider.run","version":"1.0.0"})).unwrap(),
    );
    first.actions = Some(serde_json::from_value(json!([{"name":"run","version":"1.0.0","title":"Run","args_schema":"args.json","result_schema":"result.json","session":"none","cli":false}])).unwrap());
    let mut second = first.clone();
    second.version = "2.0.0".into();
    second.provides[1].version = "2.0.0".into();
    second.actions.as_mut().unwrap()[0].version = "2.0.0".into();
    let candidates = [
        candidate(PackageKind::Tool, &root, &src, 0),
        candidate(PackageKind::Plugin, &first, &src, 0),
        candidate(PackageKind::Plugin, &second, &other, 1),
    ];
    let result = solve(&candidates, &[], &BuiltinProviders::default());
    assert_eq!(result.adds, [2, 0], "{:?}", result.errors);
    let mut incompatible = second.clone();
    incompatible.provides[0].version = "2.0.0".into();
    incompatible.provides[0].schema_version = Some("2.0.0".into());
    let candidates = [
        candidates[0],
        candidates[1],
        candidate(PackageKind::Plugin, &incompatible, &other, 1),
    ];
    let result = solve(&candidates, &[], &BuiltinProviders::default());
    assert!(result.adds.is_empty());
    assert!(
        matches!(result.errors.as_slice(),[Failure::Conflict(sites)] if sites.iter().any(|s|s.range=="^1") && sites.iter().any(|s|s.range=="^2"))
    );
}
#[test]
fn installed_version_is_pinned_and_kept_tool_requirements_are_roots() {
    let src = source("acme/catalog");
    let root = tool("root", vec![cap("field:acme.provider", "^2")], vec![]);
    let old = manifest(
        &plugin("acme.provider", "1.0.0", "1.0.0", vec![]),
        PackageKind::Plugin,
    );
    let new = plugin("acme.provider", "2.0.0", "2.0.0", vec![]);
    let kept = manifest(
        &tool("kept", vec![cap("field:acme.provider", "^1")], vec![]),
        PackageKind::Tool,
    );
    let candidates = [
        candidate(PackageKind::Tool, &root, &src, 0),
        candidate(PackageKind::Plugin, &new, &src, 0),
    ];
    let installed = [Installed { manifest: &old }, Installed { manifest: &kept }];
    let result = solve(&candidates, &installed, &BuiltinProviders::default());
    assert!(result.adds.is_empty());
    assert!(!result.errors.is_empty());
    let application = Version::new(2, 1, 0);
    let builtins = BuiltinProviders::default();
    let snapshot = Snapshot {
        candidates: &candidates,
        installed: &installed,
        builtins: &builtins,
        application: &application,
        target: TARGET,
        max_package_bytes: 100,
    };
    // An explicit update still may not break a kept installed tool.
    let result = resolve(
        &snapshot,
        &[Request {
            kind: PackageKind::Plugin,
            id: "acme.provider",
            source: &src,
        }],
    );
    assert!(result.adds.is_empty());
    assert!(!result.errors.is_empty());
    let snapshot = Snapshot {
        installed: &installed[..1],
        ..snapshot
    };
    assert_eq!(
        resolve(
            &snapshot,
            &[Request {
                kind: PackageKind::Plugin,
                id: "acme.provider",
                source: &src
            }]
        )
        .adds,
        [1]
    );
}
#[test]
fn optional_fixed_point_sees_later_required_selection_but_never_installs() {
    let src = source("acme/catalog");
    let root = tool(
        "root",
        vec![cap("field:acme.a", "^1"), cap("field:acme.b", "^1")],
        vec![cap("field:acme.unused", "^1")],
    );
    let mut a = plugin("acme.a", "1.0.0", "1.0.0", vec![]);
    a.optional = vec![serde_json::from_value(cap("field:acme.b", "^1")).unwrap()];
    let b = plugin("acme.b", "1.0.0", "1.0.0", vec![]);
    let unused = plugin("acme.unused", "1.0.0", "1.0.0", vec![]);
    let candidates = [
        candidate(PackageKind::Tool, &root, &src, 0),
        candidate(PackageKind::Plugin, &a, &src, 0),
        candidate(PackageKind::Plugin, &b, &src, 0),
        candidate(PackageKind::Plugin, &unused, &src, 0),
    ];
    let result = solve(&candidates, &[], &BuiltinProviders::default());
    assert_eq!(result.adds, [2, 1, 0], "{:?}", result.errors);
    assert_eq!(result.edges.iter().filter(|e| e.optional).count(), 1);
    assert!(result.edges.iter().any(|e| e.optional
        && e.consumer == Node::Catalog(1)
        && e.provider == Some(Node::Catalog(2))));
    let only_optional = tool("only", vec![], vec![cap("field:acme.unused", "^1")]);
    let candidates = [
        candidate(PackageKind::Tool, &only_optional, &src, 0),
        candidates[3],
    ];
    assert_eq!(
        solve(&candidates, &[], &BuiltinProviders::default()).adds,
        [0]
    );
}
#[test]
fn cycle_reports_complete_capability_chain_and_backtracks() {
    let src = source("acme/catalog");
    let other = source("acme/other");
    let a = plugin("acme.a", "1.0.0", "1.0.0", vec![cap("field:acme.b", "^1")]);
    let b = plugin("acme.b", "1.0.0", "1.0.0", vec![cap("field:acme.a", "^1")]);
    let good = plugin("acme.b", "2.0.0", "1.0.0", vec![]);
    let candidates = [
        candidate(PackageKind::Plugin, &a, &src, 0),
        candidate(PackageKind::Plugin, &b, &src, 0),
        candidate(PackageKind::Plugin, &good, &other, 1),
    ];
    let result = solve(&candidates[..2], &[], &BuiltinProviders::default());
    assert!(result.adds.is_empty());
    assert!(
        matches!(result.errors.as_slice(),[Failure::Cycle(chain)] if chain.len()==2 && chain[0].capability=="field:acme.b" && chain[1].capability=="field:acme.a")
    );
    assert_eq!(
        solve(&candidates, &[], &BuiltinProviders::default()).adds,
        [2, 0]
    );
}
#[test]
fn builtin_schema_versions_and_resources_resolve_without_nodes() {
    let registry = crate::state::schema::SchemaRegistry::with_core().unwrap();
    let bundled = manifest(
        &plugin("acme.bundled", "8.0.0", "1.0.0", vec![]),
        PackageKind::Plugin,
    );
    let builtins = BuiltinProviders::from_registry(&registry, &[bundled]);
    let src = source("acme/catalog");
    let root = tool(
        "root",
        vec![
            cap("field:vosu.chart", "*"),
            cap("resource:chart.file", "*"),
            cap("resource:skin", "*"),
            cap("field:acme.bundled", "^1"),
        ],
        vec![],
    );
    let candidates = [candidate(PackageKind::Tool, &root, &src, 0)];
    let result = solve(&candidates, &[], &builtins);
    assert_eq!(result.adds, [0]);
    assert_eq!(result.edges.len(), 4);
    assert!(result.edges.iter().all(|e| e.provider.is_none()));
    for namespace in crate::state::schema::CORE_NAMESPACES {
        let version = registry.get(namespace).unwrap().version.to_string();
        assert!(
            builtins.satisfies(
                &vosu_plugin_api::manifest::Requirement::parse(
                    &format!("field:{namespace}"),
                    &format!("={version}")
                )
                .unwrap()
            )
        );
    }
}
#[test]
fn eligibility_errors_are_distinct_and_later_eligible_candidate_wins() {
    let src = source("acme/catalog");
    let root = tool("root", vec![cap("field:acme.provider", "^1")], vec![]);
    let good = plugin("acme.provider", "1.0.0", "1.0.0", vec![]);
    let mut host = good.clone();
    host.host = "^9".into();
    let mut target = good.clone();
    target.packages = [(
        "x86_64-pc-windows-msvc".into(),
        good.packages[TARGET].clone(),
    )]
    .into();
    let mut size = good.clone();
    size.packages.get_mut(TARGET).unwrap().size = 101;
    let mut runtime = good.clone();
    runtime.runtime = Some(vosu_plugin_api::manifest::Runtime::InProcess);
    for (bad, expected) in [
        (&host, Failure::Host("acme.provider".into())),
        (&target, Failure::Target("acme.provider".into())),
        (&size, Failure::Size("acme.provider".into())),
        (&runtime, Failure::Runtime("acme.provider".into())),
    ] {
        let candidates = [
            candidate(PackageKind::Tool, &root, &src, 0),
            candidate(PackageKind::Plugin, bad, &src, 0),
            candidate(PackageKind::Plugin, &good, &src, 1),
        ];
        assert_eq!(
            solve(&candidates[..2], &[], &BuiltinProviders::default()).errors,
            [expected]
        );
        assert_eq!(
            solve(&candidates, &[], &BuiltinProviders::default()).adds,
            [2, 0]
        );
    }
}
#[test]
fn central_limits_fail_closed_without_partial_results() {
    let src = source("acme/catalog");
    let root = tool("root", vec![], vec![]);
    let candidates = vec![candidate(PackageKind::Tool, &root, &src, 0); MAX_CANDIDATES + 1];
    let result = solve(&candidates, &[], &BuiltinProviders::default());
    assert_eq!(result.errors, [Failure::ResolutionLimit]);
    assert!(result.adds.is_empty());
    assert!(result.edges.is_empty());
    let mut root = root;
    root.requires = (0..=MAX_REQUIREMENTS)
        .map(|i| serde_json::from_value(cap(&format!("field:acme.p{i}"), "^1")).unwrap())
        .collect();
    let candidates = [candidate(PackageKind::Tool, &root, &src, 0)];
    assert_eq!(
        solve(&candidates, &[], &BuiltinProviders::default()).errors,
        [Failure::ResolutionLimit]
    );
}
#[test]
fn installed_optional_provider_is_used_without_catalog_add() {
    let src = source("acme/catalog");
    let root = tool("root", vec![], vec![cap("field:acme.provider", "^1")]);
    let installed = manifest(
        &plugin("acme.provider", "3.0.0", "1.0.0", vec![]),
        PackageKind::Plugin,
    );
    let candidates = [candidate(PackageKind::Tool, &root, &src, 0)];
    let result = solve(
        &candidates,
        &[Installed {
            manifest: &installed,
        }],
        &BuiltinProviders::default(),
    );
    assert_eq!(result.adds, [0]);
    assert!(
        result
            .edges
            .iter()
            .any(|e| e.optional && e.provider == Some(Node::Installed(0)))
    );
}
fn manifest(entry: &CatalogEntry, kind: PackageKind) -> PackageManifest {
    let kind_name = match kind {
        PackageKind::Plugin => "plugin",
        PackageKind::Tool => "tool",
        PackageKind::Overlay => "overlay",
    };
    let mut text = format!(
        "kind = {kind_name:?}\nid = {:?}\nname = {:?}\nversion = {:?}\nhost = {:?}\n",
        entry.id, entry.name, entry.version, entry.host
    );
    text.push_str("manifest_version = 1\n");
    if kind == PackageKind::Plugin {
        text.push_str("abi_version = 1\nruntime = \"process\"\n");
    }
    for provide in &entry.provides {
        text.push_str(&format!(
            "[[provides]]\nid = {:?}\nversion = {:?}\n",
            provide.id, provide.version
        ));
        if provide.schema_version.is_some() {
            text.push_str("schema = \"namespace.json\"\n");
        }
    }
    for (name, capabilities) in [("requires", &entry.requires), ("optional", &entry.optional)] {
        for capability in capabilities {
            text.push_str(&format!(
                "[[{name}]]\nid = {:?}\nversion = {:?}\n",
                capability.id, capability.version
            ));
        }
    }
    if kind == PackageKind::Plugin {
        text.push_str("[targets.x86_64-unknown-linux-gnu]\nlibrary = \"plugin.so\"\n");
    }
    if kind == PackageKind::Tool {
        text.push_str("[ui]\nroot = \"ui\"\n");
    }
    PackageManifest::parse(&text).unwrap()
}

#[test]
fn exponential_dead_ends_hit_central_work_limit_without_partial_graph() {
    let src = source("acme/catalog");
    let other = source("acme/other");
    let root = tool("root", vec![cap("field:acme.p0", "^1")], vec![]);
    let entries: Vec<_> = (0..14)
        .flat_map(|i| {
            let dependency = if i == 13 {
                "field:acme.missing".to_owned()
            } else {
                format!("field:acme.p{}", i + 1)
            };
            [
                plugin(
                    &format!("acme.p{i}"),
                    "1.0.0",
                    "1.0.0",
                    vec![cap(&dependency, "^1")],
                ),
                plugin(
                    &format!("acme.p{i}"),
                    "2.0.0",
                    "1.0.0",
                    vec![cap(&dependency, "^1")],
                ),
            ]
        })
        .collect();
    let mut candidates = vec![candidate(PackageKind::Tool, &root, &src, 0)];
    candidates.extend(entries.iter().enumerate().map(|(i, entry)| {
        candidate(
            PackageKind::Plugin,
            entry,
            if i % 2 == 0 { &src } else { &other },
            i % 2,
        )
    }));
    let result = solve(&candidates, &[], &BuiltinProviders::default());
    assert_eq!(result.errors, [Failure::ResolutionLimit]);
    assert!(result.adds.is_empty());
    assert!(result.edges.is_empty());
}

#[test]
fn kept_overlay_requirement_cannot_disappear() {
    let src = source("acme/catalog");
    let root = tool("root", vec![], vec![]);
    let overlay = PackageManifest::parse(
        r#"
kind = "overlay"
manifest_version = 1
id = "kept-overlay"
name = "Kept overlay"
version = "1.0.0"
host = "^2"
[[requires]]
id = "field:acme.missing"
version = "^7"
"#,
    )
    .unwrap();
    let candidates = [candidate(PackageKind::Tool, &root, &src, 0)];
    let result = solve(
        &candidates,
        &[Installed { manifest: &overlay }],
        &BuiltinProviders::default(),
    );
    assert!(result.adds.is_empty());
    assert!(
        matches!(result.errors.as_slice(),[Failure::Missing(site)] if site.consumer=="kept-overlay" && site.range=="^7")
    );
    assert_eq!(
        solve(&candidates, &[], &BuiltinProviders::default()).adds,
        [0]
    );
}
