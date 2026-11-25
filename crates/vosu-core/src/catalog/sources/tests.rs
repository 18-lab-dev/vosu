use super::*;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};
#[derive(Default)]
struct Lifecycle {
    members: Mutex<Vec<String>>,
    fail: AtomicBool,
}
impl SourceLifecycle for Lifecycle {
    fn reconcile_sources(&self, identities: &[String]) -> Result<(), SourceStoreError> {
        if self.fail.load(Ordering::SeqCst) {
            return Err(SourceStoreError::Unavailable);
        }
        *self.members.lock().unwrap() = identities.to_vec();
        Ok(())
    }
}
fn input(source: &str, enabled: bool) -> SourceInput {
    SourceInput {
        source: source.into(),
        enabled,
    }
}
fn identity(source: &str) -> SourceIdentity {
    SourceIdentity::parse(source).unwrap().identity
}
#[test]
fn sources_replace_cannot_smuggle_authority() {
    assert!(
        serde_json::from_str::<SourceInput>(r#"{"source":"a/b","enabled":true,"trusted":true}"#)
            .is_err()
    );
    assert!(
        serde_json::from_str::<SourceInput>(
            r#"{"source":"a/b","enabled":true,"private_network":true}"#
        )
        .is_err()
    );
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle::default())).unwrap();
    assert!(
        store
            .replace(&[input("a/b", true), input("a/b", false)])
            .is_err()
    );
    assert!(store.replace(&vec![input("a/b", true); 33]).is_err());
    assert_eq!(store.snapshot().unwrap().1.len(), 3);
}
#[test]
fn sources_flags_survive_disable_reorder_restart_but_not_removal() {
    let root = tempfile::tempdir().unwrap();
    let lifecycle = Arc::new(Lifecycle::default());
    let store = SourceStore::open(root.path(), lifecycle.clone()).unwrap();
    store
        .replace(&[input("a/b", true), input("c/d", true)])
        .unwrap();
    store.set_trust(&identity("a/b"), true).unwrap();
    store.set_private_network(&identity("a/b"), true).unwrap();
    let generation = store.snapshot().unwrap().0;
    store
        .replace(&[input("c/d", true), input("a/b", false)])
        .unwrap();
    let (next, records) = store.snapshot().unwrap();
    assert_ne!(next, generation);
    assert_eq!(records[1].order, 1);
    assert!(!records[1].enabled && records[1].trusted && records[1].private_network);
    drop(store);
    let store = SourceStore::open(root.path(), lifecycle.clone()).unwrap();
    assert_eq!(store.snapshot().unwrap().1, records);
    store.replace(&[input("c/d", true)]).unwrap();
    assert_eq!(*lifecycle.members.lock().unwrap(), vec!["c/d"]);
    store.replace(&[input("a/b", true)]).unwrap();
    let record = &store.snapshot().unwrap().1[0];
    assert!(!record.trusted && !record.private_network);
}
#[test]
fn sources_fixed_official_policy_uses_exact_identity() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle::default())).unwrap();
    let official = identity(OFFICIAL_SOURCES[0]);
    assert_eq!(
        store.set_trust(&official, false),
        Err(SourceStoreError::OfficialPolicy)
    );
    assert_eq!(
        store.set_private_network(&official, true),
        Err(SourceStoreError::OfficialPolicy)
    );
    store
        .replace(&[
            input("18-LAB-DEV/vosu-tools", true),
            input("18-lab-dev/vosu-tools@latest", true),
            input("https://github.com/18-lab-dev/vosu-tools", true),
        ])
        .unwrap();
    assert!(
        store
            .snapshot()
            .unwrap()
            .1
            .iter()
            .all(|r| !r.trusted && !r.private_network)
    );
}
#[test]
fn sources_failed_lifecycle_requires_recovery_and_never_resurrects_removed_flags() {
    let root = tempfile::tempdir().unwrap();
    let lifecycle = Arc::new(Lifecycle::default());
    let store = SourceStore::open(root.path(), lifecycle.clone()).unwrap();
    store.replace(&[input("a/b", true)]).unwrap();
    store.set_trust(&identity("a/b"), true).unwrap();
    lifecycle.fail.store(true, Ordering::SeqCst);
    assert!(store.replace(&[input("c/d", true)]).is_err());
    assert_eq!(store.snapshot(), Err(SourceStoreError::RecoveryRequired));
    assert!(store.replace(&[input("a/b", true)]).is_err());
    drop(store);
    lifecycle.fail.store(false, Ordering::SeqCst);
    let store = SourceStore::open(root.path(), lifecycle.clone()).unwrap();
    assert_eq!(store.snapshot().unwrap().1[0].identity.as_str(), "c/d");
    assert_eq!(*lifecycle.members.lock().unwrap(), vec!["c/d"]);
    store.replace(&[input("a/b", true)]).unwrap();
    assert!(!store.snapshot().unwrap().1[0].trusted);
}
#[test]
fn sources_malformed_persistence_and_legacy_inputs_are_not_rewritten() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog-source-settings.json");
    std::fs::write(&path, b"invalid-source-sentinel").unwrap();
    assert!(SourceStore::open(root.path(), Arc::new(Lifecycle::default())).is_err());
    assert_eq!(std::fs::read(&path).unwrap(), b"invalid-source-sentinel");
    std::fs::remove_file(path).unwrap();
    let legacy = root.path().join("catalog-sources.json");
    std::fs::write(&legacy, b"legacy-migration-input").unwrap();
    SourceStore::open(root.path(), Arc::new(Lifecycle::default())).unwrap();
    assert_eq!(std::fs::read(legacy).unwrap(), b"legacy-migration-input");
}
#[test]
fn sources_context_includes_foreign_catalogs_and_excludes_disabled_sources() {
    use semver::Version;
    use vosu_plugin_api::catalog::Catalog;
    let empty = Catalog::parse(r#"{"catalog_version":2,"name":"Catalog","published":"2026-11-24T00:00:00Z","plugins":[],"tools":[],"overlays":[]}"#).unwrap();
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle::default())).unwrap();
    store
        .replace(&[input("a/b", true), input("c/d", false)])
        .unwrap();
    let records = store.snapshot().unwrap().1;
    let catalogs = [
        SourceCatalog {
            source: &records[0],
            catalog: &empty,
        },
        SourceCatalog {
            source: &records[1],
            catalog: &empty,
        },
    ];
    let version = Version::new(2, 0, 0);
    let result = validate_catalogs(&catalogs, &[], &[], &version);
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].0.as_str(), "a/b");
    assert!(result[0].1.is_ok());
}

#[test]
fn sources_context_rejects_cross_source_namespace_collisions() {
    use semver::Version;
    use serde_json::json;
    use vosu_plugin_api::catalog::Catalog;
    let catalog =
        |id: &str| {
            Catalog::parse(&json!({
        "catalog_version": 2, "name": "Catalog", "published": "2026-11-24T00:00:00Z",
        "plugins": [{"id": id, "name": "Plugin", "version": "1.0.0", "host": "^2",
            "runtime": "process", "provides": [{"id": format!("field:{id}"),
                "version": "1.0.0", "schema_version": "1.0.0", "lanes": []}],
            "packages": {"x86_64-unknown-linux-gnu": {"url": "https://example.org/p.zip",
                "sha256": "a".repeat(64), "size": 42}}}]
    }).to_string()).unwrap()
        };
    let first = catalog("acme.game");
    let collision = catalog("acme.game.child");
    let unrelated = catalog("other.game");
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle::default())).unwrap();
    store
        .replace(&[input("a/b", true), input("c/d", true)])
        .unwrap();
    let records = store.snapshot().unwrap().1;
    let version = Version::new(2, 0, 0);
    let bad = [
        SourceCatalog {
            source: &records[0],
            catalog: &first,
        },
        SourceCatalog {
            source: &records[1],
            catalog: &collision,
        },
    ];
    assert!(
        validate_catalogs(&bad, &[], &[], &version)
            .iter()
            .all(|(_, r)| r.is_err())
    );
    let good = [
        SourceCatalog {
            source: &records[0],
            catalog: &first,
        },
        SourceCatalog {
            source: &records[1],
            catalog: &unrelated,
        },
    ];
    assert!(
        validate_catalogs(&good, &[], &[], &version)
            .iter()
            .all(|(_, r)| r.is_ok())
    );
}
