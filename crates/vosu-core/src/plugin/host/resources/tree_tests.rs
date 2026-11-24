use super::{byte_tests::BytesTest, *};
use crate::resource_io::TreeResolver;
use crate::resources::{ReferenceClass, ReferenceIdentity, ResourceSecret};
use vosu_plugin_api::{CborValue, ResourceRef, ResourceResolved, TreeEntry};
fn identity(plugin: &str) -> ReferenceIdentity {
    ReferenceIdentity::new(
        &ResourceSecret::ephemeral().unwrap(),
        plugin,
        ReferenceClass::Tree,
        ResourceRef {
            kind: "set".into(),
            key: CborValue::Integer(91.into()),
            name: "set".into(),
            content_id: CborValue::Integer(92.into()),
        },
    )
    .unwrap()
}
fn list(revision: &str) -> ResourceResolved {
    ResourceResolved::Tree {
        entries: vec![TreeEntry {
            path: "canonical.PNG".into(),
            size: 7,
            hash: None,
        }],
        truncated: true,
        revision: revision.into(),
    }
}
#[test]
fn canonical_alias_revalidation_cache_and_unindexed_proof() {
    let test = BytesTest::new("tree-canonical", 99431);
    std::fs::write(test.dir.path().join("hash"), b"pngdata").unwrap();
    let id = identity("testkit.bytes.tree-canonical");
    let resolver = TreeResolver::new(&test.host, Duration::from_secs(2));
    let families = ["image".into()];
    assert_eq!(test.fixture.resolves.load(Ordering::SeqCst), 0);
    test.fixture.trees.lock().unwrap().extend([
        list("r1"),
        ResourceResolved::TreePath {
            root: "songs".into(),
            path: "hash".into(),
            entry_path: "canonical.PNG".into(),
        },
    ]);
    let member = test
        .runtime
        .block_on(resolver.lookup(test.owner, &id, &families, "alias.jpg"))
        .unwrap();
    assert_eq!(member.entry.media_type, "image/png");
    let crate::resource_io::ResolvedResource::File(file) = member.source else {
        panic!("expected file")
    };
    assert_eq!(test.runtime.block_on(file.read(0, 7)).unwrap(), b"pngdata");
    test.fixture.trees.lock().unwrap().push_back(list("r1"));
    let cached = test
        .runtime
        .block_on(resolver.index(test.owner, &id, &families))
        .unwrap();
    assert!(Arc::ptr_eq(&cached, &member.index));
    test.fixture.trees.lock().unwrap().extend([
        list("r2"),
        ResourceResolved::TreeStream {
            stream: 601,
            size: 7,
            entry_path: "omitted.PNG".into(),
        },
    ]);
    assert!(matches!(
        test.runtime
            .block_on(resolver.lookup(test.owner, &id, &families, "alias.jpg")),
        Err(ResourceRequestError::InvalidReply)
    ));
    assert_eq!(
        test.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        601
    );
    assert_eq!(test.fixture.resolves.load(Ordering::SeqCst), 5);
    resolver.retire(&id.id);
}
#[test]
fn provider_gone_and_not_found_stay_typed() {
    let test = BytesTest::new("tree-errors", 99432);
    let id = identity("testkit.bytes.tree-errors");
    let resolver = TreeResolver::new(&test.host, Duration::from_secs(2));
    for (code, expected) in [
        ("gone", ResourceRequestError::Gone),
        ("not_found", ResourceRequestError::NotFound),
        ("private-sentinel", ResourceRequestError::ProviderFailed),
    ] {
        test.fixture
            .trees
            .lock()
            .unwrap()
            .push_back(ResourceResolved::Error {
                error: vosu_plugin_api::ErrorInfo {
                    code: code.into(),
                    message: "private/root/sentinel".into(),
                },
            });
        assert!(
            matches!(test.runtime.block_on(resolver.index(test.owner, &id, &[])), Err(error) if error == expected)
        );
    }
}

#[test]
fn index_cache_capacity_and_retirement_release_representations() {
    let test = BytesTest::new("tree-capacity", 99433);
    let resolver = TreeResolver::new(&test.host, Duration::from_secs(2));
    let first_id = identity("testkit.bytes.tree-capacity");
    let load = |id: &ReferenceIdentity| {
        test.fixture
            .trees
            .lock()
            .unwrap()
            .push_back(list("same-revision"));
        test.runtime
            .block_on(resolver.index(test.owner, id, &[]))
            .unwrap()
    };
    let first = load(&first_id);
    for _ in 0..crate::resource_io::MAX_CACHED_TREE_INDEXES {
        load(&identity("testkit.bytes.tree-capacity"));
    }
    let reloaded = load(&first_id);
    assert!(!Arc::ptr_eq(&first, &reloaded));
    let same = load(&first_id);
    assert!(Arc::ptr_eq(&same, &reloaded));
    resolver.retire(&first_id.id);
    assert!(!Arc::ptr_eq(&same, &load(&first_id)));
}

#[test]
fn indexed_member_with_missing_physical_file_is_not_found() {
    let test = BytesTest::new("tree-missing-file", 99434);
    let id = identity("testkit.bytes.tree-missing-file");
    let resolver = TreeResolver::new(&test.host, Duration::from_secs(2));
    test.fixture.trees.lock().unwrap().extend([
        list("missing-r1"),
        ResourceResolved::TreePath {
            root: "songs".into(),
            path: "missing-file".into(),
            entry_path: "canonical.PNG".into(),
        },
    ]);
    assert!(matches!(
        test.runtime
            .block_on(resolver.lookup(test.owner, &id, &["image".into()], "alias.png")),
        Err(ResourceRequestError::NotFound)
    ));
}
