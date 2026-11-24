use semver::Version;
use vosu_core::catalog::{
    resolver::{BuiltinProviders, Candidate, Failure, Installed, Request, Snapshot, resolve},
    sources::SourceIdentity,
};
use vosu_plugin_api::{
    catalog::Catalog,
    manifest::{PackageKind, PackageManifest},
};

fn fixture() -> (Vec<SourceIdentity>, Vec<Catalog>) {
    let values: Vec<serde_json::Value> =
        serde_json::from_str(include_str!("../fixtures/catalog-v2/resolver-ranked.json")).unwrap();
    let sources = values
        .iter()
        .map(|v| {
            SourceIdentity::parse(v["source"].as_str().unwrap())
                .unwrap()
                .identity
        })
        .collect();
    let catalogs = values
        .iter()
        .map(|v| Catalog::parse(&v["catalog"].to_string()).unwrap())
        .collect();
    (sources, catalogs)
}
#[test]
fn resolver_fixture_backtracks_with_sdk_validated_candidates_and_no_optional_install() {
    let (sources, catalogs) = fixture();
    let mut candidates = Vec::new();
    for (source_order, catalog) in catalogs.iter().enumerate() {
        for (catalog_order, (kind, entry)) in catalog.entries().enumerate() {
            candidates.push(Candidate {
                kind,
                entry,
                source: &sources[source_order],
                source_order,
                catalog_order,
                trusted: false,
            });
        }
    }
    let builtins = BuiltinProviders::from_registry(
        &vosu_core::state::schema::SchemaRegistry::with_core().unwrap(),
        &[],
    );
    let application = Version::new(2, 1, 0);
    let snapshot = Snapshot {
        candidates: &candidates,
        installed: &[],
        builtins: &builtins,
        application: &application,
        target: "x86_64-unknown-linux-gnu",
        max_package_bytes: 100,
    };
    let requests = [Request {
        kind: PackageKind::Tool,
        id: "root",
        source: &sources[0],
    }];
    let result = resolve(&snapshot, &requests);
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    let adds: Vec<_> = result
        .adds
        .iter()
        .map(|&i| {
            (
                candidates[i].entry.id.as_str(),
                candidates[i].entry.version.as_str(),
                candidates[i].source.as_str(),
            )
        })
        .collect();
    assert_eq!(
        adds,
        [
            ("acme.provider", "1.0.0", "acme/alternate"),
            ("root", "1.0.0", "acme/preferred")
        ]
    );
    assert_eq!(result.edges.len(), 1);
    assert!(!result.edges[0].optional);
    let again = resolve(&snapshot, &requests);
    assert_eq!(again.adds, result.adds);
    assert_eq!(again.edges, result.edges);
    let snapshot = Snapshot {
        max_package_bytes: 35,
        ..snapshot
    };
    let failed = resolve(&snapshot, &requests);
    assert!(failed.adds.is_empty());
    assert!(failed.edges.is_empty());
    assert!(matches!(
        failed.errors.as_slice(),
        [Failure::Missing(_)] | [Failure::Size(_)]
    ));
}
#[test]
fn resolver_builtin_satisfiers_never_create_catalog_nodes() {
    let registry = vosu_core::state::schema::SchemaRegistry::with_core().unwrap();
    let builtins = BuiltinProviders::from_registry(&registry, &[]);
    let application = Version::new(2, 1, 0);
    let consumer = PackageManifest::parse(
        r#"
kind = "tool"
manifest_version = 1
id = "kept"
name = "Kept"
version = "1.0.0"
host = "^2"
[[requires]]
id = "field:vosu.chart"
version = "*"
[[optional]]
id = "resource:skin"
version = "*"
[ui]
root = "ui"
"#,
    )
    .unwrap();
    let installed = [Installed {
        manifest: &consumer,
    }];
    let snapshot = Snapshot {
        candidates: &[],
        installed: &installed,
        builtins: &builtins,
        application: &application,
        target: "x86_64-pc-windows-msvc",
        max_package_bytes: 100,
    };
    let result = resolve(&snapshot, &[]);
    assert!(result.errors.is_empty());
    assert!(result.adds.is_empty());
    assert_eq!(result.edges.len(), 2);
    assert!(result.edges.iter().all(|e| e.provider.is_none()));
    assert_eq!(result.edges.iter().filter(|e| e.optional).count(), 1);
}
