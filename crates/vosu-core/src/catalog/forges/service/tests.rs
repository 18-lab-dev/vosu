use super::*;
use crate::catalog::{
    forges::tests::{detected, fixture},
    sources::{SourceIdentity, SourceInput},
};
use std::{
    io,
    sync::atomic::{AtomicUsize, Ordering},
};
use vosu_net::streaming::{AddressResolver, DestinationPolicy, NetworkResponse, ResolverFuture};
struct NoNetwork(AtomicUsize);
impl AddressResolver for NoNetwork {
    fn resolve<'a>(&'a self, _: &'a str) -> ResolverFuture<'a> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Err(io::Error::other("mock DNS")) })
    }
}
fn client() -> (StreamingClient, Arc<NoNetwork>) {
    let resolver = Arc::new(NoNetwork(AtomicUsize::new(0)));
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
fn input(source: &str, enabled: bool) -> SourceInput {
    SourceInput {
        source: source.into(),
        enabled,
    }
}
fn context(version: &Version) -> FetchContext<'_> {
    FetchContext {
        registrations: &[],
        revisions: &[],
        application: version,
        target: "x86_64-unknown-linux-gnu",
    }
}
#[tokio::test]
async fn mock_endpoints_use_exact_api_asset_not_browser_url_and_typed_purposes() {
    for (value, forge, key, expected_metadata, expected_asset) in [
        (
            "alice/catalog",
            Forge::GitHub,
            "github",
            "https://api.github.com/repos/alice/catalog/releases/latest",
            "https://api.github.com/repos/alice/catalog/releases/assets/42",
        ),
        (
            "https://gitlab.com/acme/team/catalog",
            Forge::GitLab,
            "gitlab",
            "https://gitlab.com/api/v4/projects/acme%2Fteam%2Fcatalog/releases/permalink/latest",
            "https://gitlab.com/acme/team/catalog/-/releases/v8.2/downloads/catalog-v2.json",
        ),
        (
            "https://codeberg.org/alice/catalog",
            Forge::Gitea,
            "gitea",
            "https://codeberg.org/api/v1/repos/alice/catalog/releases/latest",
            "https://codeberg.org/alice/catalog/releases/download/v5.1/catalog-v2.json",
        ),
    ] {
        let source = detected(value, forge);
        let mut calls = Vec::new();
        let bytes = retrieve_with(&source, |url, purpose, limit| {
            calls.push((url.to_string(), purpose, limit));
            let body = if calls.len() == 1 {
                serde_json::to_vec(&fixture()[key]).unwrap()
            } else {
                serde_json::to_vec(&fixture()["catalog"]).unwrap()
            };
            std::future::ready(Ok(NetworkResponse {
                status: 200,
                body: body.into(),
                final_url: url,
            }))
        })
        .await
        .unwrap();
        assert_eq!(
            calls,
            vec![
                (
                    expected_metadata.into(),
                    metadata_purpose(forge),
                    MAX_METADATA_BYTES as u64
                ),
                (
                    expected_asset.into(),
                    RequestPurpose::CatalogAsset,
                    MAX_CATALOG_BYTES as u64
                )
            ]
        );
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap(),
            fixture()["catalog"]
        );
    }
}
#[tokio::test]
async fn mock_private_404_never_asserts_repository_existence_or_requests_asset() {
    let source = detected("alice/catalog", Forge::GitHub);
    let mut calls = 0;
    let result = retrieve_with(&source, |url, _, _| {
        calls += 1;
        std::future::ready(Ok(NetworkResponse {
            status: 404,
            body: b"private secret response".as_slice().into(),
            final_url: url,
        }))
    })
    .await;
    assert_eq!(result, Err(FetchError::Unavailable));
    assert_eq!(calls, 1);
    assert_eq!(
        source_failure(result.unwrap_err()),
        SourceFailure::Unavailable
    );
}
#[tokio::test]
async fn coordinator_ingests_sdk_once_skips_disabled_and_publishes_no_stale_fallback() {
    let root = tempfile::tempdir().unwrap();
    let service = CatalogFetchService::open(root.path()).unwrap();
    service
        .sources
        .replace(&[input("alice/catalog", true), input("other/disabled", false)])
        .unwrap();
    service
        .set_credential(
            "alice/catalog",
            CredentialInput::Token {
                token: "private-sentinel".into(),
            },
        )
        .await
        .unwrap();
    let (client, resolver) = client();
    let version = Version::new(2, 0, 0);
    let cancel = CancellationToken::new();
    let mut calls = 0;
    let snapshot = service
        .refresh_with(&client, context(&version), &cancel, |source, auth| {
            calls += 1;
            assert_eq!(source.parsed.identity.as_str(), "alice/catalog");
            assert!(auth.is_some());
            std::future::ready(Ok(serde_json::to_vec(&fixture()["catalog"]).unwrap()))
        })
        .await
        .unwrap();
    assert_eq!(calls, 1);
    assert_eq!(resolver.0.load(Ordering::SeqCst), 0);
    assert_eq!(
        snapshot.sources()[1].catalog().unwrap_err(),
        SourceFailure::Disabled
    );
    assert_eq!(snapshot.candidates().count(), 1);
    assert_eq!(snapshot.candidates().next().unwrap().entry.version, "2.3.4");
    let failed = service
        .refresh_with(&client, context(&version), &cancel, |_, _| {
            std::future::ready(Err(FetchError::Unavailable))
        })
        .await
        .unwrap();
    assert_eq!(failed.candidates().count(), 0);
    assert_eq!(
        failed.sources()[0].catalog().unwrap_err(),
        SourceFailure::Unavailable
    );
    let invalid = service
        .refresh_with(&client, context(&version), &cancel, |_, _| {
            std::future::ready(Ok(br#"{"catalog_version":1}"#.to_vec()))
        })
        .await
        .unwrap();
    assert_eq!(
        invalid.sources()[0].catalog().unwrap_err(),
        SourceFailure::InvalidCatalog
    );
}
async fn preserves_failure(error: FetchError, expected: SourceFailure) {
    let root = tempfile::tempdir().unwrap();
    let service = CatalogFetchService::open(root.path()).unwrap();
    service
        .sources
        .replace(&[input("alice/catalog", true)])
        .unwrap();
    let (client, _) = client();
    let version = Version::new(2, 0, 0);
    let snapshot = service
        .refresh_with(
            &client,
            context(&version),
            &CancellationToken::new(),
            |_, _| std::future::ready(Err(error)),
        )
        .await
        .unwrap();
    assert_eq!(snapshot.sources()[0].catalog().unwrap_err(), expected);
    assert_eq!(snapshot.candidates().count(), 0);
    assert!(!expected.to_string().contains("sentinel"));
}

#[tokio::test]
async fn coordinator_preserves_credential_required() {
    preserves_failure(status(401).unwrap_err(), SourceFailure::CredentialRequired).await;
}
#[tokio::test]
async fn coordinator_preserves_credential_rejected() {
    preserves_failure(status(403).unwrap_err(), SourceFailure::CredentialRejected).await;
}
#[tokio::test]
async fn coordinator_preserves_credential_store_unavailable() {
    preserves_failure(
        FetchError::CredentialsUnavailable,
        SourceFailure::CredentialStoreUnavailable,
    )
    .await;
}
#[tokio::test]
async fn coordinator_preserves_malformed_release() {
    preserves_failure(FetchError::InvalidMetadata, SourceFailure::MalformedRelease).await;
    preserves_failure(FetchError::CatalogAsset, SourceFailure::MalformedRelease).await;
}
#[tokio::test]
async fn coordinator_preserves_unsafe_asset() {
    preserves_failure(FetchError::AssetOrigin, SourceFailure::UnsafeAsset).await;
}
#[tokio::test]
async fn coordinator_preserves_undetected_forge() {
    preserves_failure(FetchError::UndetectedForge, SourceFailure::UndetectedForge).await;
}
#[tokio::test]
async fn coordinator_stale_source_and_cancellation_cannot_publish() {
    let root = tempfile::tempdir().unwrap();
    let service = CatalogFetchService::open(root.path()).unwrap();
    service
        .sources
        .replace(&[input("alice/catalog", true)])
        .unwrap();
    let (client, _) = client();
    let version = Version::new(2, 0, 0);
    let result = service
        .refresh_with(
            &client,
            context(&version),
            &CancellationToken::new(),
            |_, _| {
                service.sources.replace(&[]).unwrap();
                std::future::ready(Ok(serde_json::to_vec(&fixture()["catalog"]).unwrap()))
            },
        )
        .await;
    assert!(matches!(result, Err(SnapshotError::Stale)));
    assert!(service.sources.candidate_snapshot().unwrap().is_none());
    let cancel = CancellationToken::new();
    cancel.cancel();
    assert!(
        service
            .refresh(&client, context(&version), &cancel)
            .await
            .is_err()
    );
    assert!(service.sources.candidate_snapshot().unwrap().is_none());
}
#[tokio::test]
async fn composition_reconciles_before_exposure_and_removal_purges_credentials() {
    let root = tempfile::tempdir().unwrap();
    let service = CatalogFetchService::open(root.path()).unwrap();
    service
        .sources
        .replace(&[input("alice/catalog", true)])
        .unwrap();
    service
        .set_credential(
            "alice/catalog",
            CredentialInput::Token {
                token: "must-not-revive".into(),
            },
        )
        .await
        .unwrap();
    let before = service.sources.snapshot().unwrap().0;
    service.delete_credential("alice/catalog").await.unwrap();
    assert_ne!(service.sources.snapshot().unwrap().0, before);
    service
        .set_credential(
            "alice/catalog",
            CredentialInput::Token {
                token: "must-not-revive".into(),
            },
        )
        .await
        .unwrap();
    drop(service);
    let service = CatalogFetchService::open(root.path()).unwrap();
    assert!(service.credential_presence("alice/catalog").has_credential);
    service.sources.replace(&[]).unwrap();
    service
        .sources
        .replace(&[input("alice/catalog", true)])
        .unwrap();
    assert!(!service.credential_presence("alice/catalog").has_credential);
    assert!(
        service
            .credentials
            .lease("alice/catalog")
            .unwrap()
            .is_none()
    );
    assert!(
        service
            .sources
            .set_private_network(
                &SourceIdentity::parse("alice/catalog").unwrap().identity,
                true
            )
            .is_ok()
    );
}

#[tokio::test]
async fn static_mock_endpoint_is_literal_and_has_no_metadata_request() {
    let source = detected("https://static.test/path/catalog-v2.json", Forge::Static);
    let mut calls = 0;
    let result = retrieve_with(&source, |url, purpose, limit| {
        calls += 1;
        assert_eq!(url.as_str(), "https://static.test/path/catalog-v2.json");
        assert_eq!(purpose, RequestPurpose::CatalogAsset);
        assert_eq!(limit, MAX_CATALOG_BYTES as u64);
        std::future::ready(Ok(NetworkResponse {
            status: 200,
            body: serde_json::to_vec(&fixture()["catalog"]).unwrap().into(),
            final_url: url,
        }))
    })
    .await
    .unwrap();
    assert_eq!(calls, 1);
    assert!(!result.is_empty());
}

#[tokio::test]
async fn malformed_credential_store_is_preserved_and_blocks_secret_use() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("catalog-credentials.toml");
    std::fs::write(&path, b"malformed-private-token").unwrap();
    let service = CatalogFetchService::open(root.path()).unwrap();
    service
        .sources
        .replace(&[input("alice/catalog", true)])
        .unwrap();
    let (client, _) = client();
    let version = Version::new(2, 0, 0);
    let mut calls = 0;
    let snapshot = service
        .refresh_with(
            &client,
            context(&version),
            &CancellationToken::new(),
            |_, _| {
                calls += 1;
                std::future::ready(Err(FetchError::Unavailable))
            },
        )
        .await
        .unwrap();
    assert_eq!(calls, 0);
    assert_eq!(
        snapshot.sources()[0].catalog().unwrap_err(),
        SourceFailure::CredentialStoreUnavailable
    );
    assert_eq!(std::fs::read(path).unwrap(), b"malformed-private-token");
}

#[tokio::test]
async fn failed_unknown_forge_is_published_without_any_authenticated_fetch() {
    let root = tempfile::tempdir().unwrap();
    let service = CatalogFetchService::open(root.path()).unwrap();
    service
        .sources
        .replace(&[input("https://unknown.test/owner/catalog", true)])
        .unwrap();
    let (client, resolver) = client();
    let version = Version::new(2, 0, 0);
    let mut calls = 0;
    let snapshot = service
        .refresh_with(
            &client,
            context(&version),
            &CancellationToken::new(),
            |_, _| {
                calls += 1;
                std::future::ready(Err(FetchError::Unavailable))
            },
        )
        .await
        .unwrap();
    assert_eq!(calls, 0);
    assert_eq!(resolver.0.load(Ordering::SeqCst), 2);
    assert_eq!(
        snapshot.sources()[0].catalog().unwrap_err(),
        SourceFailure::UndetectedForge
    );
}

#[tokio::test]
async fn credential_mutation_waits_for_lease_then_invalidates_published_snapshot() {
    let root = tempfile::tempdir().unwrap();
    let service = CatalogFetchService::open(root.path()).unwrap();
    service
        .sources
        .replace(&[input("alice/catalog", true)])
        .unwrap();
    let (client, _) = client();
    let version = Version::new(2, 0, 0);
    let cancel = CancellationToken::new();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let mut started_tx = Some(started_tx);
    let mut release_rx = Some(release_rx);
    let refresh = service.refresh_with(&client, context(&version), &cancel, |_, auth| {
        assert!(auth.is_none());
        started_tx.take().unwrap().send(()).unwrap();
        let release = release_rx.take().unwrap();
        async move {
            release.await.unwrap();
            Ok(serde_json::to_vec(&fixture()["catalog"]).unwrap())
        }
    });
    let mutation = async {
        started_rx.await.unwrap();
        assert!(service.operation.try_lock().is_err());
        release_tx.send(()).unwrap();
        service
            .set_credential(
                "alice/catalog",
                CredentialInput::Token {
                    token: "new-generation-sentinel".into(),
                },
            )
            .await
            .unwrap();
    };
    let (result, ()) = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        tokio::join!(refresh, mutation)
    })
    .await
    .unwrap();
    assert_eq!(result.unwrap().candidates().count(), 1);
    assert!(service.sources.candidate_snapshot().unwrap().is_none());
    assert!(service.credential_presence("alice/catalog").has_credential);
}
