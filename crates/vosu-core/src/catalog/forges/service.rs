use super::*;
use crate::catalog::{
    credentials::{CredentialError, CredentialPresence, CredentialStore},
    sources::{
        CandidateSnapshot, FetchedCatalog, MAX_CATALOG_BYTES, SnapshotError, SourceFailure,
        SourceLifecycle, SourceStore, SourceStoreError,
    },
};
use semver::Version;
use std::{path::Path, sync::Arc};
use tokio_util::sync::CancellationToken;
use vosu_net::streaming::{
    Deadlines, DownloadLimits, NetworkError, RequestPolicy, StreamingClient,
};
use vosu_plugin_api::manifest::{NamespaceRevision, Registration};

struct CredentialLifecycle(Arc<CredentialStore>);
impl SourceLifecycle for CredentialLifecycle {
    fn reconcile_sources(&self, identities: &[String]) -> Result<(), SourceStoreError> {
        self.0
            .reconcile_sources(identities)
            .map(|_| ())
            .map_err(|_| SourceStoreError::RecoveryRequired)
    }
}
/// Owns credential mutation/fetch ordering, not a global authenticated client.
/// Root composition must call `open` before exposing any catalog service. Source
/// recovery reconciles credential membership before this constructor returns.
pub struct CatalogFetchService {
    sources: SourceStore,
    credentials: Arc<CredentialStore>,
    operation: tokio::sync::Mutex<()>,
}
impl CatalogFetchService {
    pub fn open(config_root: &Path) -> Result<Self, SourceStoreError> {
        let credentials = Arc::new(CredentialStore::open(config_root));
        let sources = SourceStore::open(
            config_root,
            Arc::new(CredentialLifecycle(credentials.clone())),
        )?;
        Ok(Self {
            sources,
            credentials,
            operation: tokio::sync::Mutex::new(()),
        })
    }
    pub fn sources(&self) -> &SourceStore {
        &self.sources
    }
    pub fn credential_presence(&self, source: &str) -> CredentialPresence {
        self.credentials.presence(source)
    }
    pub async fn set_credential(
        &self,
        source: &str,
        input: CredentialInput,
    ) -> Result<(), CredentialError> {
        let _operation = self.operation.lock().await;
        self.sources
            .invalidate()
            .map_err(|_| CredentialError::Unavailable)?;
        self.credentials.set(source, input).map(|_| ())
    }
    pub async fn delete_credential(&self, source: &str) -> Result<(), CredentialError> {
        let _operation = self.operation.lock().await;
        self.sources
            .invalidate()
            .map_err(|_| CredentialError::Unavailable)?;
        self.credentials.delete(source).map(|_| ())
    }
    pub async fn repair_credentials(
        &self,
        inputs: Vec<(String, CredentialInput)>,
    ) -> Result<(), CredentialError> {
        let _operation = self.operation.lock().await;
        self.sources
            .invalidate()
            .map_err(|_| CredentialError::Unavailable)?;
        self.credentials.repair(inputs).map(|_| ())
    }
    /// Sequential bounded work owned by the caller; cancellation has no detached
    /// worker. No state mutex spans network, SDK validation or disk operations.
    pub async fn refresh(
        &self,
        client: &StreamingClient,
        context: FetchContext<'_>,
        cancel: &CancellationToken,
    ) -> Result<Arc<CandidateSnapshot>, SnapshotError> {
        self.refresh_with(client, context, cancel, |source, auth| async move {
            retrieve(&source, client, auth.as_ref(), cancel).await
        })
        .await
    }
    async fn refresh_with<F, Fut>(
        &self,
        client: &StreamingClient,
        context: FetchContext<'_>,
        cancel: &CancellationToken,
        mut fetch: F,
    ) -> Result<Arc<CandidateSnapshot>, SnapshotError>
    where
        F: FnMut(DetectedSource, Option<RequestCredential>) -> Fut,
        Fut: Future<Output = Result<Vec<u8>, FetchError>>,
    {
        let _operation = tokio::select! {
            () = cancel.cancelled() => return Err(SnapshotError::Unavailable),
            operation = self.operation.lock() => operation,
        };
        let ticket = self.sources.begin_fetch()?;
        let mut fetched = Vec::new();
        for record in ticket.sources().iter().filter(|record| record.enabled) {
            if cancel.is_cancelled() {
                return Err(SnapshotError::Unavailable);
            }
            let result = match self
                .sources
                .detect_source(&record.identity, client, true, cancel)
                .await
            {
                Ok(detected) => match self.credentials.lease(record.identity.as_str()) {
                    Ok(lease) => match credential(
                        detected.forge,
                        lease.as_ref().map(|(input, _)| input.as_ref()),
                    ) {
                        Ok(auth) => fetch(detected, auth).await,
                        Err(error) => Err(error),
                    },
                    Err(_) => Err(FetchError::CredentialsUnavailable),
                },
                Err(crate::catalog::sources::DetectionError::Undetected) => {
                    Err(FetchError::UndetectedForge)
                }
                Err(crate::catalog::sources::DetectionError::Cancelled) => {
                    Err(FetchError::Cancelled)
                }
                Err(
                    crate::catalog::sources::DetectionError::Unavailable
                    | crate::catalog::sources::DetectionError::Stale,
                ) => Err(FetchError::Unavailable),
            };
            fetched.push(FetchedCatalog {
                source: record.identity.clone(),
                result: result.map_err(source_failure),
            });
        }
        if cancel.is_cancelled() {
            return Err(SnapshotError::Unavailable);
        }
        // The SDK parser/context validator is invoked only by complete_fetch,
        // once against the entire successful enabled set, never per download.
        self.sources.complete_fetch(
            ticket,
            fetched,
            context.registrations,
            context.revisions,
            context.application,
            context.target,
        )
    }
}

fn source_failure(error: FetchError) -> SourceFailure {
    match error {
        FetchError::Unavailable | FetchError::Cancelled => SourceFailure::Unavailable,
        FetchError::CredentialRequired => SourceFailure::CredentialRequired,
        FetchError::CredentialRejected | FetchError::CredentialKind => {
            SourceFailure::CredentialRejected
        }
        FetchError::CredentialsUnavailable => SourceFailure::CredentialStoreUnavailable,
        FetchError::InvalidMetadata | FetchError::CatalogAsset => SourceFailure::MalformedRelease,
        FetchError::AssetOrigin => SourceFailure::UnsafeAsset,
        FetchError::UndetectedForge => SourceFailure::UndetectedForge,
    }
}
/// Complete actual installed/bundled registrations, not a copied provider list.
pub struct FetchContext<'a> {
    pub registrations: &'a [Registration<'a>],
    pub revisions: &'a [NamespaceRevision<'a>],
    pub application: &'a Version,
    pub target: &'a str,
}
fn network_error(error: NetworkError) -> FetchError {
    if matches!(error, NetworkError::Cancelled) {
        FetchError::Cancelled
    } else {
        FetchError::Unavailable
    }
}
async fn retrieve(
    source: &DetectedSource,
    client: &StreamingClient,
    auth: Option<&RequestCredential>,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, FetchError> {
    retrieve_with(source, |url, purpose, max_bytes| {
        let client = client.clone().with_deadlines_and_limits(
            Deadlines::default(),
            DownloadLimits {
                max_bytes,
                max_redirects: 5,
            },
        );
        let policy = RequestPolicy::source(purpose, source.policy.clone());
        async move {
            let policy = policy.map_err(network_error)?;
            client
                .request_scoped(url.as_str(), "vosu", &policy, auth, cancel)
                .await
                .map_err(network_error)
        }
    })
    .await
}

async fn retrieve_with<F, Fut>(
    source: &DetectedSource,
    mut request: F,
) -> Result<Vec<u8>, FetchError>
where
    F: FnMut(Url, RequestPurpose, u64) -> Fut,
    Fut: Future<Output = Result<vosu_net::streaming::NetworkResponse, FetchError>>,
{
    let asset = if source.forge == Forge::Static {
        static_source::catalog_url(source)?
    } else {
        let url = match source.forge {
            Forge::GitHub => github::release_url(source)?,
            Forge::GitLab => gitlab::release_url(source)?,
            Forge::Gitea => gitea::release_url(source)?,
            Forge::Static => unreachable!(),
        };
        let response = request(
            url,
            metadata_purpose(source.forge),
            MAX_METADATA_BYTES as u64,
        )
        .await?;
        status(response.status)?;
        match source.forge {
            Forge::GitHub => github::catalog_asset(&response.body, source)?,
            Forge::GitLab => gitlab::catalog_asset(&response.body, source)?,
            Forge::Gitea => gitea::catalog_asset(&response.body, source)?,
            Forge::Static => unreachable!(),
        }
    };
    let response = request(
        asset,
        RequestPurpose::CatalogAsset,
        MAX_CATALOG_BYTES as u64,
    )
    .await?;
    status(response.status)?;
    if response.body.len() > MAX_CATALOG_BYTES {
        return Err(FetchError::InvalidMetadata);
    }
    Ok(response.body.to_vec())
}
#[cfg(test)]
mod tests;
