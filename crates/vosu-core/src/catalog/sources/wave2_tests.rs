use super::*;
use std::{
    io,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio_util::sync::CancellationToken;
use vosu_net::streaming::{
    AddressResolver, Deadlines, DestinationPolicy, DownloadLimits, ResolverFuture, StreamingClient,
};
struct Lifecycle;
impl SourceLifecycle for Lifecycle {
    fn reconcile_sources(&self, _: &[String]) -> Result<(), SourceStoreError> {
        Ok(())
    }
}
fn input(source: &str, enabled: bool) -> SourceInput {
    SourceInput {
        source: source.into(),
        enabled,
    }
}
fn id(source: &str) -> SourceIdentity {
    SourceIdentity::parse(source).unwrap().identity
}
struct Resolver(AtomicUsize);
impl AddressResolver for Resolver {
    fn resolve<'a>(&'a self, _: &'a str) -> ResolverFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(io::Error::other("fixture DNS failure")) })
    }
}
fn client() -> (StreamingClient, Arc<Resolver>) {
    let resolver = Arc::new(Resolver(AtomicUsize::new(0)));
    (
        StreamingClient::new(
            resolver.clone(),
            DestinationPolicy::PublicOnly,
            Deadlines::default(),
            DownloadLimits::default(),
        ),
        resolver,
    )
}
#[tokio::test]
async fn detection_known_hosts_and_static_never_probe() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    let sources = [
        ("a/b", Forge::GitHub),
        ("https://github.com/a/b", Forge::GitHub),
        ("https://gitlab.com/group/nested/repo", Forge::GitLab),
        ("https://static.test/catalog-v2.json", Forge::Static),
    ];
    store
        .replace(
            &sources
                .iter()
                .map(|(s, _)| input(s, true))
                .collect::<Vec<_>>(),
        )
        .unwrap();
    let (client, resolver) = client();
    for (source, expected) in sources {
        assert_eq!(
            store
                .detect(&id(source), &client, false, &CancellationToken::new())
                .await,
            Ok(expected)
        );
    }
    assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
}
#[tokio::test]
async fn detection_failure_only_retries_refresh_or_relevant_edit() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    let source = "https://forge.test/a/b";
    store.replace(&[input(source, true)]).unwrap();
    let identity = id(source);
    let (client, resolver) = client();
    let cancel = CancellationToken::new();
    for _ in 0..2 {
        assert_eq!(
            store.detect(&identity, &client, false, &cancel).await,
            Err(DetectionError::Undetected)
        );
    }
    assert_eq!(resolver.0.load(Ordering::SeqCst), 2);
    store.set_trust(&identity, true).unwrap();
    assert_eq!(
        store.detect(&identity, &client, false, &cancel).await,
        Err(DetectionError::Undetected)
    );
    assert_eq!(resolver.0.load(Ordering::SeqCst), 2);
    assert_eq!(
        store.detect(&identity, &client, true, &cancel).await,
        Err(DetectionError::Undetected)
    );
    assert_eq!(resolver.0.load(Ordering::SeqCst), 4);
    store.set_private_network(&identity, true).unwrap();
    assert_eq!(
        store.detect(&identity, &client, false, &cancel).await,
        Err(DetectionError::Undetected)
    );
    assert_eq!(resolver.0.load(Ordering::SeqCst), 6);
}
#[tokio::test]
async fn detection_success_lifetime_and_stale_completion() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    let source = "https://forge.test/a/b";
    let identity = id(source);
    store.replace(&[input(source, true)]).unwrap();
    let generation = store.snapshot().unwrap().0;
    assert_eq!(
        store.finish_detection(&identity, generation, Ok(Forge::Gitea)),
        Ok(Forge::Gitea)
    );
    store.set_private_network(&identity, true).unwrap();
    store.recover().unwrap();
    let (client, resolver) = client();
    assert_eq!(
        store
            .detect(&identity, &client, true, &CancellationToken::new())
            .await,
        Ok(Forge::Gitea)
    );
    assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
    store.replace(&[]).unwrap();
    store.replace(&[input(source, true)]).unwrap();
    assert_eq!(
        store.finish_detection(&identity, generation, Ok(Forge::GitLab)),
        Err(DetectionError::Stale)
    );
    assert_eq!(
        store
            .detect(&identity, &client, false, &CancellationToken::new())
            .await,
        Err(DetectionError::Undetected)
    );
    assert_eq!(resolver.0.load(Ordering::SeqCst), 2);
}
#[tokio::test]
async fn detection_cancelled_attempt_is_not_cached() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    let source = "https://forge.test/a/b";
    store.replace(&[input(source, true)]).unwrap();
    let (client, resolver) = client();
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert_eq!(
        store.detect(&id(source), &client, false, &cancel).await,
        Err(DetectionError::Cancelled)
    );
    assert_eq!(
        store
            .detect(&id(source), &client, false, &CancellationToken::new())
            .await,
        Err(DetectionError::Undetected)
    );
    assert_eq!(resolver.0.load(Ordering::SeqCst), 2);
}
fn catalog(version: &str) -> Vec<u8> {
    format!(r#"{{"catalog_version":2,"name":"fixture","published":"2026-11-24T00:00:00Z","tools":[{{"id":"test-tool","name":"Tool","version":"{version}","host":"^2","packages":{{"any":{{"url":"https://packages.test/tool.zip","sha256":"{}","size":42}}}}}}]}}"#, "a".repeat(64)).into_bytes()
}
fn fetched(source: &str, version: &str) -> FetchedCatalog {
    let bytes = catalog(version);
    vosu_plugin_api::catalog::Catalog::parse(std::str::from_utf8(&bytes).unwrap()).unwrap();
    FetchedCatalog {
        source: id(source),
        result: Ok(bytes),
    }
}
fn complete(
    store: &SourceStore,
    ticket: SourceFetchTicket,
    results: Vec<FetchedCatalog>,
) -> Result<Arc<CandidateSnapshot>, SnapshotError> {
    store.complete_fetch(
        ticket,
        results,
        &[],
        &[],
        &semver::Version::new(2, 0, 0),
        "x86_64-unknown-linux-gnu",
    )
}
#[test]
fn snapshot_keeps_alternatives_order_support_and_no_stale_fallback() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    store
        .replace(&[input("a/b", true), input("c/d", true), input("e/f", false)])
        .unwrap();
    let snapshot = complete(
        &store,
        store.begin_fetch().unwrap(),
        vec![fetched("a/b", "1.0.0"), fetched("c/d", "2.0.0")],
    )
    .unwrap();
    let candidates: Vec<_> = snapshot.candidates().collect();
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        snapshot.sources()[2].catalog().unwrap_err(),
        SourceFailure::Disabled
    );
    assert_eq!(candidates[0].entry.version, "1.0.0");
    assert_eq!(candidates[1].entry.version, "2.0.0");
    assert_eq!(candidates[1].source.order, 1);
    assert_eq!(candidates[0].catalog_index, 0);
    assert!(candidates.iter().all(|c| c.support.supported()));
    let snapshot = complete(
        &store,
        store.begin_fetch().unwrap(),
        vec![fetched("c/d", "3.0.0")],
    )
    .unwrap();
    assert_eq!(snapshot.candidates().count(), 1);
    assert_eq!(
        snapshot.sources()[0].catalog().unwrap_err(),
        SourceFailure::Unavailable
    );
    store
        .replace(&[input("c/d", true), input("a/b", false)])
        .unwrap();
    assert!(store.candidate_snapshot().unwrap().is_none());
    let snapshot = complete(
        &store,
        store.begin_fetch().unwrap(),
        vec![fetched("c/d", "4.0.0")],
    )
    .unwrap();
    assert_eq!(snapshot.candidates().next().unwrap().source.order, 0);
}
#[test]
fn snapshot_rejects_stale_credential_source_and_duplicate_results() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    store
        .replace(&[input("a/b", true), input("c/d", true)])
        .unwrap();
    let ticket = store.begin_fetch().unwrap();
    store.invalidate().unwrap();
    assert!(matches!(
        complete(&store, ticket, vec![fetched("a/b", "1.0.0")]),
        Err(SnapshotError::Stale)
    ));
    assert!(matches!(
        complete(
            &store,
            store.begin_fetch().unwrap(),
            vec![fetched("a/b", "1.0.0"), fetched("a/b", "2.0.0")]
        ),
        Err(SnapshotError::InvalidResults)
    ));
    assert!(matches!(
        complete(
            &store,
            store.begin_fetch().unwrap(),
            vec![fetched("e/f", "1.0.0")]
        ),
        Err(SnapshotError::InvalidResults)
    ));
}
#[test]
fn snapshot_invalid_and_oversized_documents_contribute_no_candidates() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    store
        .replace(&[input("a/b", true), input("c/d", true)])
        .unwrap();
    let snapshot = complete(
        &store,
        store.begin_fetch().unwrap(),
        vec![
            FetchedCatalog {
                source: id("a/b"),
                result: Ok(b"secret malformed response".to_vec()),
            },
            FetchedCatalog {
                source: id("c/d"),
                result: Ok(vec![b' '; MAX_CATALOG_BYTES + 1]),
            },
        ],
    )
    .unwrap();
    assert_eq!(snapshot.candidates().count(), 0);
    assert_eq!(
        snapshot.sources()[0].catalog().unwrap_err(),
        SourceFailure::InvalidCatalog
    );
    assert_eq!(
        snapshot.sources()[1].catalog().unwrap_err(),
        SourceFailure::Limit
    );
}
#[tokio::test]
async fn detection_stalled_dns_is_bounded_and_cancellable() {
    struct Stalled;
    impl AddressResolver for Stalled {
        fn resolve<'a>(&'a self, _: &'a str) -> ResolverFuture<'a> {
            Box::pin(std::future::pending())
        }
    }
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    let source = "https://forge.test/a/b";
    store.replace(&[input(source, true)]).unwrap();
    let client = StreamingClient::new(
        Arc::new(Stalled),
        DestinationPolicy::PublicOnly,
        Deadlines::default(),
        DownloadLimits::default(),
    );
    let cancel = CancellationToken::new();
    let identity = id(source);
    let operation = store.detect(&identity, &client, false, &cancel);
    tokio::pin!(operation);
    tokio::select! {
        result = &mut operation => panic!("unexpected completion {result:?}"),
        () = tokio::task::yield_now() => cancel.cancel(),
    }
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(1), operation)
            .await
            .unwrap(),
        Err(DetectionError::Cancelled)
    );
}

#[test]
fn snapshot_stale_workers_cannot_publish_after_credential_or_source_edit() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    store.replace(&[input("a/b", true)]).unwrap();
    let ticket = store.begin_fetch().unwrap();
    store.invalidate().unwrap();
    assert!(matches!(
        complete(&store, ticket, vec![fetched("a/b", "1.0.0")]),
        Err(SnapshotError::Stale)
    ));
    assert!(store.candidate_snapshot().unwrap().is_none());
    let ticket = store.begin_fetch().unwrap();
    store.replace(&[input("a/b", false)]).unwrap();
    assert!(matches!(
        complete(&store, ticket, vec![fetched("a/b", "1.0.0")]),
        Err(SnapshotError::Stale)
    ));
    let snapshot = complete(&store, store.begin_fetch().unwrap(), vec![]).unwrap();
    assert_eq!(snapshot.candidates().count(), 0);
    assert_eq!(snapshot.sources().len(), 1);
    assert_eq!(
        snapshot.sources()[0].catalog().unwrap_err(),
        SourceFailure::Disabled
    );
}

#[test]
fn snapshot_rejects_duplicate_and_disabled_source_results() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    store
        .replace(&[input("a/b", true), input("c/d", false)])
        .unwrap();
    assert!(matches!(
        complete(
            &store,
            store.begin_fetch().unwrap(),
            vec![fetched("a/b", "1.0.0"), fetched("a/b", "2.0.0")]
        ),
        Err(SnapshotError::InvalidResults)
    ));
    assert!(matches!(
        complete(
            &store,
            store.begin_fetch().unwrap(),
            vec![fetched("c/d", "1.0.0")]
        ),
        Err(SnapshotError::InvalidResults)
    ));
}

#[test]
fn snapshot_keeps_unsupported_candidates_but_redacts_invalid_documents() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    store
        .replace(&[input("a/b", true), input("c/d", true)])
        .unwrap();
    let bytes = String::from_utf8(catalog("1.0.0"))
        .unwrap()
        .replace("\"host\":\"^2\"", "\"host\":\"^9\"")
        .into_bytes();
    let snapshot = complete(
        &store,
        store.begin_fetch().unwrap(),
        vec![
            FetchedCatalog {
                source: id("a/b"),
                result: Ok(bytes),
            },
            FetchedCatalog {
                source: id("c/d"),
                result: Ok(b"private-sentinel".to_vec()),
            },
        ],
    )
    .unwrap();
    assert_eq!(snapshot.candidates().count(), 1);
    assert!(!snapshot.candidates().next().unwrap().support.supported());
    assert_eq!(
        snapshot.sources()[1].catalog().unwrap_err(),
        SourceFailure::InvalidCatalog
    );
    assert_eq!(SourceFailure::InvalidCatalog.to_string(), "invalid catalog");
}

#[tokio::test]
async fn detection_exports_exact_source_and_derived_authority_without_credentials() {
    let root = tempfile::tempdir().unwrap();
    let store = SourceStore::open(root.path(), Arc::new(Lifecycle)).unwrap();
    store
        .replace(&[
            input("a/b@v2", true),
            input("https://github.com/a/catalog-v2.json", true),
        ])
        .unwrap();
    let (client, resolver) = client();
    let source = store
        .detect_source(&id("a/b@v2"), &client, false, &CancellationToken::new())
        .await
        .unwrap();
    assert_eq!(source.parsed.identity.as_str(), "a/b@v2");
    assert_eq!(source.configured_host(), "github.com");
    assert_eq!(source.credential_origin().host(), "api.github.com");
    assert_eq!(source.credential_origin().port(), 443);
    assert_eq!(source.generation, store.snapshot().unwrap().0);
    let source = store
        .detect_source(
            &id("https://github.com/a/catalog-v2.json"),
            &client,
            false,
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(source.forge, Forge::Static);
    assert_eq!(source.credential_origin().host(), "github.com");
    assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
}
