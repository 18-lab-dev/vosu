use super::byte_tests::BytesTest;
use super::*;
use crate::plugin::host_services::PluginHostHandler;
use crate::resource_io::{ResolvedResource, ResourceResolver};
use crate::resources::{ReferenceClass, ReferenceIdentity, ResourceSecret};

fn identity(plugin: &str) -> ReferenceIdentity {
    ReferenceIdentity::new(
        &ResourceSecret::ephemeral().unwrap(),
        plugin,
        ReferenceClass::Resource,
        vosu_plugin_api::ResourceRef {
            kind: "file".into(),
            key: vosu_plugin_api::CborValue::Text("chart-73".into()),
            name: "chart.osu".into(),
            content_id: vosu_plugin_api::CborValue::Text("digest-83".into()),
        },
    )
    .unwrap()
}
#[test]
fn resolve_is_lazy_failed_opens_retry_successes_cache_and_snapshot_bytes() {
    let fixture = BytesTest::new("resolve", 99311);
    *fixture.fixture.path.lock().unwrap() = Some("chart.osu".into());
    let identity = identity("testkit.bytes.resolve");
    let resolver = ResourceResolver::new(&fixture.host, Duration::from_secs(2));
    assert_eq!(fixture.fixture.resolves.load(Ordering::SeqCst), 0);
    assert!(
        fixture
            .runtime
            .block_on(resolver.resolve(fixture.owner, &identity))
            .is_err()
    );
    assert_eq!(fixture.fixture.resolves.load(Ordering::SeqCst), 1);
    std::fs::write(fixture.dir.path().join("chart.osu"), b"original-chart").unwrap();
    let ResolvedResource::File(file) = fixture
        .runtime
        .block_on(resolver.resolve(fixture.owner, &identity))
        .unwrap()
    else {
        panic!("file");
    };
    assert_eq!(fixture.fixture.resolves.load(Ordering::SeqCst), 2);
    assert_eq!(fixture.runtime.block_on(file.read(2, 5)).unwrap(), b"igina");
    std::fs::write(fixture.dir.path().join("chart.osu"), b"mutated--chart").unwrap();
    let ResolvedResource::File(cached) = fixture
        .runtime
        .block_on(resolver.resolve(fixture.owner, &identity))
        .unwrap()
    else {
        panic!("file");
    };
    assert!(Arc::ptr_eq(&file, &cached));
    assert_eq!(fixture.fixture.resolves.load(Ordering::SeqCst), 2);
    assert_eq!(
        fixture.runtime.block_on(cached.read(0, 100)).unwrap(),
        b"original-chart"
    );
    resolver.retire(&identity.id);
    assert!(!file.is_current());
    assert_eq!(
        fixture.runtime.block_on(file.read(0, 1)),
        Err(ResourceRequestError::Cancelled)
    );
    std::fs::write(fixture.dir.path().join("chart.osu"), b"original-chart").unwrap();
    let ResolvedResource::File(reopened) = fixture
        .runtime
        .block_on(resolver.resolve(fixture.owner, &identity))
        .unwrap()
    else {
        panic!("file");
    };
    assert_eq!(fixture.fixture.resolves.load(Ordering::SeqCst), 3);
    assert_eq!(
        fixture.runtime.block_on(reopened.read(0, 100)).unwrap(),
        b"original-chart"
    );
}
#[test]
fn resolve_rejects_traversal_and_revoked_root_capabilities() {
    let fixture = BytesTest::new("revoke", 99312);
    let resolver = ResourceResolver::new(&fixture.host, Duration::from_secs(2));
    assert!(
        fixture
            .runtime
            .block_on(resolver.open_file(fixture.owner, "songs", "../outside.osu"))
            .is_err()
    );
    std::fs::write(fixture.dir.path().join("chart.osu"), b"retained").unwrap();
    let file = fixture
        .runtime
        .block_on(resolver.open_file(fixture.owner, "songs", "chart.osu"))
        .unwrap();
    assert_eq!(fixture.runtime.block_on(file.read(1, 4)).unwrap(), b"etai");
    assert_eq!(
        fixture.runtime.block_on(file.read(u64::MAX, 2)),
        Err(ResourceRequestError::InvalidRequest)
    );
    assert_eq!(
        fixture.host.handle(
            fixture.instance,
            Message::RootDeclare(RootDeclare {
                handle: None,
                name: "songs".into(),
                path: fixture.dir.path().to_str().unwrap().into()
            }),
            0
        ),
        vosu_status::VOSU_OK
    );
    assert!(!file.is_current());
    assert_eq!(
        fixture.runtime.block_on(file.read(0, 1)),
        Err(ResourceRequestError::Cancelled)
    );
    assert!(
        fixture
            .runtime
            .block_on(resolver.open_file(fixture.owner, "songs", "chart.osu"))
            .is_err()
    );
}
#[test]
fn resolve_streams_are_never_cached_and_eviction_closes_only_its_lease() {
    let fixture = BytesTest::new("fresh", 99313);
    let resolver = ResourceResolver::new(&fixture.host, Duration::from_secs(2));
    let identity = identity("testkit.bytes.fresh");
    let ResolvedResource::Stream(first) = fixture
        .runtime
        .block_on(resolver.resolve(fixture.owner, &identity))
        .unwrap()
    else {
        panic!("stream");
    };
    let ResolvedResource::Stream(second) = fixture
        .runtime
        .block_on(resolver.resolve(fixture.owner, &identity))
        .unwrap()
    else {
        panic!("stream");
    };
    assert_eq!(fixture.fixture.resolves.load(Ordering::SeqCst), 2);
    drop(first);
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        101
    );
    assert!(second.is_current());
    drop(second);
    assert_eq!(
        fixture.closed.recv_timeout(Duration::from_secs(2)).unwrap(),
        102
    );
}

#[test]
fn successful_file_cache_is_bounded_and_eviction_revokes_retained_capability() {
    let fixture = BytesTest::new("file-capacity", 99314);
    *fixture.fixture.path.lock().unwrap() = Some("chart.osu".into());
    std::fs::write(fixture.dir.path().join("chart.osu"), b"bounded-snapshot").unwrap();
    let resolver = ResourceResolver::new(&fixture.host, Duration::from_secs(2));
    let first_identity = identity("testkit.bytes.file-capacity");
    let ResolvedResource::File(first) = fixture
        .runtime
        .block_on(resolver.resolve(fixture.owner, &first_identity))
        .unwrap()
    else {
        panic!("file");
    };
    let secret = ResourceSecret::ephemeral().unwrap();
    for key in 0..crate::resource_io::MAX_CACHED_FILE_RESOLUTIONS {
        let reference = ReferenceIdentity::new(
            &secret,
            "testkit.bytes.file-capacity",
            ReferenceClass::Resource,
            vosu_plugin_api::ResourceRef {
                kind: "file".into(),
                key: vosu_plugin_api::CborValue::Integer((key as u64).into()),
                name: "chart.osu".into(),
                content_id: vosu_plugin_api::CborValue::Integer(97.into()),
            },
        )
        .unwrap();
        assert!(matches!(
            fixture
                .runtime
                .block_on(resolver.resolve(fixture.owner, &reference))
                .unwrap(),
            ResolvedResource::File(_)
        ));
    }
    assert!(!first.is_current());
    assert_eq!(
        fixture.runtime.block_on(first.read(0, 1)),
        Err(ResourceRequestError::Cancelled)
    );
    assert_eq!(
        fixture.fixture.resolves.load(Ordering::SeqCst),
        crate::resource_io::MAX_CACHED_FILE_RESOLUTIONS + 1
    );
}

#[test]
fn wrong_provider_identity_never_dispatches() {
    let fixture = BytesTest::new("provider", 99315);
    let resolver = ResourceResolver::new(&fixture.host, Duration::from_secs(2));
    assert!(matches!(
        fixture
            .runtime
            .block_on(resolver.resolve(fixture.owner, &identity("testkit.other"))),
        Err(ResourceRequestError::InvalidRequest)
    ));
    assert_eq!(fixture.fixture.resolves.load(Ordering::SeqCst), 0);
}
