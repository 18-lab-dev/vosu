//! Source-faithful embedded/cache/network lazer offset resolution.

use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use thiserror::Error;
use tokio_util::sync::CancellationToken;

use super::lazer_offsets::{LazerOffsets, LazerOffsetsError, validate_version};
use crate::logging::Logger;
pub use crate::net::{AddressResolver, ResolverFuture as ResolveFuture};
use crate::net::{Deadlines, DestinationPolicy, DownloadLimits, StreamingClient};

pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
pub const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(240),
    Duration::from_secs(300),
];
pub const DEFAULT_ENDPOINTS: [&str; 2] = ["https://vosu.app/offsets", "https://osuck.net/offsets"];
const MAX_OFFSET_BODY: u64 = 16 * 1024 * 1024;

#[derive(Debug, Error)]
pub enum LazerLoadError {
    #[error(transparent)]
    Schema(#[from] LazerOffsetsError),
    #[error("offset cache I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("all offset sources are unavailable for osu! {0}")]
    Unavailable(String),
    #[error("offset resolution cancelled")]
    Cancelled,
}

#[derive(Debug, Error, Clone)]
#[error("request failed: {message}")]
pub struct FetchError {
    pub message: String,
}

#[derive(Debug, Clone)]
pub struct FetchResponse {
    pub status: u16,
    pub body: Bytes,
}

pub type FetchFuture<'a> =
    Pin<Box<dyn Future<Output = Result<FetchResponse, FetchError>> + Send + 'a>>;

pub trait OffsetFetcher: Send + Sync {
    fn fetch<'a>(&'a self, url: &'a str, user_agent: &'a str, timeout: Duration)
    -> FetchFuture<'a>;
}

#[derive(Clone)]
pub struct ReqwestOffsetFetcher {
    client: StreamingClient,
}

impl ReqwestOffsetFetcher {
    pub fn new() -> Result<Self, FetchError> {
        Ok(Self {
            client: StreamingClient::production(),
        })
    }

    pub fn with_resolver(resolver: Arc<dyn AddressResolver>) -> Self {
        Self {
            client: StreamingClient::new(
                resolver,
                DestinationPolicy::PublicOnly,
                Deadlines::default(),
                DownloadLimits::default(),
            ),
        }
    }

    #[cfg(feature = "test-harness")]
    pub fn with_test_network(config: crate::net::TestNetworkConfig) -> Self {
        Self {
            client: StreamingClient::for_test_network(
                config,
                Deadlines::default(),
                DownloadLimits::default(),
            ),
        }
    }
}

impl OffsetFetcher for ReqwestOffsetFetcher {
    fn fetch<'a>(
        &'a self,
        url: &'a str,
        user_agent: &'a str,
        timeout: Duration,
    ) -> FetchFuture<'a> {
        Box::pin(async move {
            let client = self.client.clone().with_deadlines_and_limits(
                Deadlines {
                    connect: timeout,
                    idle: timeout,
                    total: timeout,
                },
                DownloadLimits {
                    max_bytes: MAX_OFFSET_BODY,
                    max_redirects: 5,
                },
            );
            let response = client
                .request_bytes(url, user_agent, &CancellationToken::new())
                .await
                .map_err(|error| FetchError {
                    message: if matches!(error, crate::net::NetworkError::Timeout) {
                        "offset request timed out".into()
                    } else {
                        error.to_string()
                    },
                })?;
            Ok(FetchResponse {
                status: response.status,
                body: response.body,
            })
        })
    }
}

pub struct LazerOffsetLoader<F> {
    fetcher: F,
    cache_dir: PathBuf,
    embedded: Bytes,
    endpoints: Vec<String>,
}

impl<F: OffsetFetcher> LazerOffsetLoader<F> {
    pub fn new(fetcher: F, cache_dir: PathBuf, embedded: Bytes) -> Self {
        Self {
            fetcher,
            cache_dir,
            embedded,
            endpoints: DEFAULT_ENDPOINTS.iter().map(ToString::to_string).collect(),
        }
    }

    pub fn with_endpoints(mut self, endpoints: impl IntoIterator<Item = String>) -> Self {
        self.endpoints = endpoints.into_iter().collect();
        self
    }

    pub fn cache_path(&self, version: &str) -> Result<PathBuf, LazerLoadError> {
        validate_version(version)?;
        Ok(self.cache_dir.join(format!("{version}.json")))
    }

    pub async fn resolve(
        &self,
        version: &str,
        cancel: &CancellationToken,
    ) -> Result<Arc<LazerOffsets>, LazerLoadError> {
        validate_version(version)?;
        if cancel.is_cancelled() {
            return Err(LazerLoadError::Cancelled);
        }

        if let Ok(offsets) = LazerOffsets::from_json(&self.embedded)
            && offsets.osu_version == version
        {
            return Ok(Arc::new(offsets));
        }

        let cache_path = self.cache_path(version)?;
        if let Ok(bytes) = std::fs::read(&cache_path)
            && let Ok(offsets) = LazerOffsets::from_json(&bytes)
            && offsets.osu_version == version
        {
            return Ok(Arc::new(offsets));
        }

        let user_agent = format!("vosu/{version} (https://vosu.app; i@kotrik.ru)");
        let mut failures = Vec::with_capacity(self.endpoints.len());
        for endpoint in &self.endpoints {
            if cancel.is_cancelled() {
                return Err(LazerLoadError::Cancelled);
            }
            let url = format!("{}/{version}.json", endpoint.trim_end_matches('/'));
            let response = tokio::select! {
                () = cancel.cancelled() => return Err(LazerLoadError::Cancelled),
                response = self.fetcher.fetch(&url, &user_agent, REQUEST_TIMEOUT) => response,
            };
            let response = match response {
                Ok(response) => response,
                Err(error) => {
                    failures.push(format!("{endpoint}: {error}"));
                    continue;
                }
            };
            if !(200..300).contains(&response.status) {
                failures.push(format!("{endpoint}: HTTP {}", response.status));
                continue;
            }
            let offsets = match LazerOffsets::from_json(&response.body) {
                Ok(offsets) => offsets,
                Err(error) => {
                    failures.push(format!("{endpoint}: invalid offsets: {error}"));
                    continue;
                }
            };
            if offsets.osu_version != version {
                failures.push(format!(
                    "{endpoint}: returned osu! version {}",
                    offsets.osu_version
                ));
                continue;
            }
            if cancel.is_cancelled() {
                return Err(LazerLoadError::Cancelled);
            }
            // A cache failure must not discard already validated live offsets.
            let _ = install_cache(&cache_path, &response.body, cancel);
            return Ok(Arc::new(offsets));
        }
        let detail = if failures.is_empty() {
            "no offset endpoints configured".to_owned()
        } else {
            failures.join("; ")
        };
        Err(LazerLoadError::Unavailable(format!("{version}: {detail}")))
    }
}

pub fn install_cache(target: &Path, bytes: &[u8], cancel: &CancellationToken) -> io::Result<()> {
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    let parent = target
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "cache target has no parent"))?;
    std::fs::create_dir_all(parent)?;
    let mut temp = tempfile::Builder::new()
        .prefix(".offsets-")
        .suffix(".tmp")
        .tempfile_in(parent)?;
    temp.write_all(bytes)?;
    temp.as_file_mut().sync_all()?;
    if cancel.is_cancelled() {
        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
    }
    let temp_path = temp.into_temp_path();
    crate::platform::atomic_replace(&temp_path, target)?;
    Ok(())
}

/// Repeatedly discovers a version and resolves offsets without instance churn.
/// The attempt is immediate; failures wait 30/60/120/240/300 seconds, capped.
pub async fn retry_pending<D, Fut, S>(
    discover: D,
    sleep: S,
    cancel: CancellationToken,
) -> Result<Arc<LazerOffsets>, LazerLoadError>
where
    D: FnMut(CancellationToken) -> Fut,
    Fut: Future<Output = Result<Arc<LazerOffsets>, LazerLoadError>>,
    S: FnMut(Duration) -> Pin<Box<dyn Future<Output = ()> + Send>>,
{
    retry_pending_with_observer(discover, sleep, cancel, |_| {}).await
}

async fn retry_pending_with_observer<D, Fut, S, O>(
    mut discover: D,
    mut sleep: S,
    cancel: CancellationToken,
    mut observe: O,
) -> Result<Arc<LazerOffsets>, LazerLoadError>
where
    D: FnMut(CancellationToken) -> Fut,
    Fut: Future<Output = Result<Arc<LazerOffsets>, LazerLoadError>>,
    S: FnMut(Duration) -> Pin<Box<dyn Future<Output = ()> + Send>>,
    O: FnMut(&LazerLoadError) + Send,
{
    let mut retry = 0usize;
    loop {
        if cancel.is_cancelled() {
            return Err(LazerLoadError::Cancelled);
        }
        match discover(cancel.child_token()).await {
            Ok(offsets) => return Ok(offsets),
            Err(LazerLoadError::Cancelled) => return Err(LazerLoadError::Cancelled),
            Err(error) => {
                observe(&error);
            }
        }
        let delay = RETRY_DELAYS[retry.min(RETRY_DELAYS.len() - 1)];
        retry = retry.saturating_add(1);
        tokio::select! {
            () = cancel.cancelled() => return Err(LazerLoadError::Cancelled),
            () = sleep(delay) => {}
        }
    }
}

/// Resolves and publishes offsets for one lazer instance. Discovery is repeated
/// on every attempt, and manager removal cancels both in-flight fetches and waits.
pub async fn resolve_for_instance<F>(
    loader: Arc<LazerOffsetLoader<F>>,
    instance: Arc<crate::instance::Instance>,
) -> Result<Arc<LazerOffsets>, LazerLoadError>
where
    F: OffsetFetcher + 'static,
{
    resolve_for_instance_inner(loader, instance, None).await
}

pub(crate) async fn resolve_for_instance_with_logger<F>(
    loader: Arc<LazerOffsetLoader<F>>,
    instance: Arc<crate::instance::Instance>,
    logger: Logger,
) -> Result<Arc<LazerOffsets>, LazerLoadError>
where
    F: OffsetFetcher + 'static,
{
    resolve_for_instance_inner(loader, instance, Some(logger)).await
}

async fn resolve_for_instance_inner<F>(
    loader: Arc<LazerOffsetLoader<F>>,
    instance: Arc<crate::instance::Instance>,
    logger: Option<Logger>,
) -> Result<Arc<LazerOffsets>, LazerLoadError>
where
    F: OffsetFetcher + 'static,
{
    if instance.client() != crate::model::enums::ClientType::Lazer {
        return Err(LazerLoadError::Unavailable("not a lazer instance".into()));
    }
    if let Some(offsets) = instance.lazer_offsets() {
        return Ok(offsets);
    }
    let pid = instance.pid();
    let cancel = instance.lifecycle_token();
    let offsets = retry_pending_with_observer(
        {
            let loader = loader.clone();
            let instance = instance.clone();
            move |attempt_cancel| {
                let loader = loader.clone();
                let instance = instance.clone();
                async move {
                    if !instance.process().is_alive() {
                        return Err(LazerLoadError::Cancelled);
                    }
                    let pid = instance.process().process_id().ok_or_else(|| {
                        LazerLoadError::Unavailable("target PID is unavailable".into())
                    })?;
                    let version =
                        super::lazer_version::discover_for_process(pid).map_err(|error| {
                            LazerLoadError::Unavailable(format!(
                                "osu! version discovery failed: {error}"
                            ))
                        })?;
                    loader.resolve(&version, &attempt_cancel).await
                }
            }
        },
        tokio_sleep,
        cancel,
        move |error| {
            if let Some(logger) = logger.as_ref() {
                logger.warn(format!(
                    "Lazer offset resolution failed for PID {pid}; retrying: {error}"
                ));
            }
        },
    )
    .await?;
    if instance.install_lazer_offsets(offsets.clone()) {
        Ok(offsets)
    } else if let Some(installed) = instance.lazer_offsets() {
        Ok(installed)
    } else {
        Err(LazerLoadError::Cancelled)
    }
}

pub fn tokio_sleep(delay: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
    Box::pin(tokio::time::sleep(delay))
}
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use tokio::sync::Mutex;

    use super::*;

    #[derive(Clone, Default)]
    struct FakeFetcher {
        responses: Arc<Mutex<VecDeque<Result<FetchResponse, FetchError>>>>,
        calls: Arc<Mutex<Vec<(String, String, Duration)>>>,
    }

    impl FakeFetcher {
        async fn push(&self, response: Result<FetchResponse, FetchError>) {
            self.responses.lock().await.push_back(response);
        }
    }

    impl OffsetFetcher for FakeFetcher {
        fn fetch<'a>(
            &'a self,
            url: &'a str,
            user_agent: &'a str,
            timeout: Duration,
        ) -> FetchFuture<'a> {
            Box::pin(async move {
                self.calls
                    .lock()
                    .await
                    .push((url.to_owned(), user_agent.to_owned(), timeout));
                self.responses.lock().await.pop_front().unwrap_or_else(|| {
                    Err(FetchError {
                        message: "offline".into(),
                    })
                })
            })
        }
    }

    fn embedded_bytes() -> Bytes {
        Bytes::copy_from_slice(
            &crate::assets::DaemonAssets::get("offsets.json")
                .unwrap()
                .data,
        )
    }

    fn with_version(bytes: &Bytes, version: &str) -> Bytes {
        let mut value: serde_json::Value = serde_json::from_slice(bytes).unwrap();
        value["OsuVersion"] = serde_json::Value::from(version);
        Bytes::from(serde_json::to_vec(&value).unwrap())
    }

    #[tokio::test]
    async fn embedded_then_cache_precedence_bypasses_network() {
        let fetcher = FakeFetcher::default();
        let cache = tempfile::tempdir().unwrap();
        let embedded = embedded_bytes();
        let version: serde_json::Value = serde_json::from_slice(&embedded).unwrap();
        let version = version["OsuVersion"].as_str().unwrap();
        let loader = LazerOffsetLoader::new(fetcher.clone(), cache.path().to_owned(), embedded);
        assert_eq!(
            loader
                .resolve(version, &CancellationToken::new())
                .await
                .unwrap()
                .osu_version,
            version
        );
        assert!(fetcher.calls.lock().await.is_empty());

        let cache_version = "2026.518.0";
        std::fs::write(
            cache.path().join(format!("{cache_version}.json")),
            with_version(&embedded_bytes(), cache_version),
        )
        .unwrap();
        let loader =
            LazerOffsetLoader::new(fetcher.clone(), cache.path().to_owned(), embedded_bytes());
        assert_eq!(
            loader
                .resolve(cache_version, &CancellationToken::new())
                .await
                .unwrap()
                .osu_version,
            cache_version
        );
        assert!(fetcher.calls.lock().await.is_empty());
    }

    #[tokio::test]
    async fn invalid_primary_falls_through_and_valid_secondary_is_cached_atomically() {
        let fetcher = FakeFetcher::default();
        let version = "2026.518.0";
        fetcher
            .push(Ok(FetchResponse {
                status: 500,
                body: Bytes::new(),
            }))
            .await;
        let valid = with_version(&embedded_bytes(), version);
        fetcher
            .push(Ok(FetchResponse {
                status: 200,
                body: valid.clone(),
            }))
            .await;
        let cache = tempfile::tempdir().unwrap();
        let loader =
            LazerOffsetLoader::new(fetcher.clone(), cache.path().to_owned(), embedded_bytes());
        let result = loader
            .resolve(version, &CancellationToken::new())
            .await
            .unwrap();
        assert_eq!(result.osu_version, version);
        let calls = fetcher.calls.lock().await;
        assert_eq!(calls.len(), 2);
        assert!(calls[0].0.starts_with("https://vosu.app/offsets/"));
        assert!(calls[1].0.starts_with("https://osuck.net/offsets/"));
        assert_eq!(
            calls[0].1,
            "vosu/2026.518.0 (https://vosu.app; i@kotrik.ru)"
        );
        assert_eq!(calls[0].2, REQUEST_TIMEOUT);
        drop(calls);
        assert_eq!(
            std::fs::read(cache.path().join(format!("{version}.json"))).unwrap(),
            valid
        );
        assert!(!std::fs::read_dir(cache.path()).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".offsets-")
        }));
    }

    #[tokio::test]
    async fn failed_offset_sources_include_each_endpoint_diagnostic() {
        let fetcher = FakeFetcher::default();
        fetcher
            .push(Err(FetchError {
                message: "offline".into(),
            }))
            .await;
        fetcher
            .push(Ok(FetchResponse {
                status: 503,
                body: Bytes::new(),
            }))
            .await;
        let loader = LazerOffsetLoader::new(
            fetcher,
            tempfile::tempdir().unwrap().path().to_owned(),
            embedded_bytes(),
        );
        let error = loader
            .resolve("2026.518.0", &CancellationToken::new())
            .await
            .expect_err("all sources should fail");
        let message = error.to_string();
        assert!(message.contains("https://vosu.app/offsets: request failed: offline"));
        assert!(message.contains("https://osuck.net/offsets: HTTP 503"));
    }

    #[tokio::test]
    async fn wrong_remote_version_never_overwrites_existing_cache() {
        let fetcher = FakeFetcher::default();
        let cache = tempfile::tempdir().unwrap();
        let version = "2026.518.0";
        let target = cache.path().join(format!("{version}.json"));
        std::fs::write(&target, b"existing corrupt cache").unwrap();
        fetcher
            .push(Ok(FetchResponse {
                status: 200,
                body: embedded_bytes(),
            }))
            .await;
        fetcher
            .push(Err(FetchError {
                message: "offline".into(),
            }))
            .await;
        let loader = LazerOffsetLoader::new(fetcher, cache.path().to_owned(), embedded_bytes());
        assert!(matches!(
            loader.resolve(version, &CancellationToken::new()).await,
            Err(LazerLoadError::Unavailable(_))
        ));
        assert_eq!(std::fs::read(target).unwrap(), b"existing corrupt cache");
    }

    #[tokio::test]
    async fn retry_schedule_caps_and_success_does_not_churn() {
        let attempts = Arc::new(AtomicUsize::new(0));
        let slept = Arc::new(Mutex::new(Vec::new()));
        let success = Arc::new(LazerOffsets::from_json(&embedded_bytes()).unwrap());
        let result = retry_pending(
            {
                let attempts = attempts.clone();
                let success = success.clone();
                move |_| {
                    let attempt = attempts.fetch_add(1, Ordering::SeqCst);
                    let success = success.clone();
                    async move {
                        if attempt == 6 {
                            Ok(success)
                        } else {
                            Err(LazerLoadError::Unavailable("pending".into()))
                        }
                    }
                }
            },
            {
                let slept = slept.clone();
                move |delay| {
                    let slept = slept.clone();
                    Box::pin(async move {
                        slept.lock().await.push(delay);
                    })
                }
            },
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(result.osu_version, success.osu_version);
        assert_eq!(attempts.load(Ordering::SeqCst), 7);
        assert_eq!(
            *slept.lock().await,
            vec![
                Duration::from_secs(30),
                Duration::from_secs(60),
                Duration::from_secs(120),
                Duration::from_secs(240),
                Duration::from_secs(300),
                Duration::from_secs(300),
            ]
        );
    }

    #[tokio::test]
    async fn cancellation_stops_pending_retry() {
        let cancel = CancellationToken::new();
        cancel.cancel();
        let attempts = Arc::new(AtomicUsize::new(0));
        let result = retry_pending(
            {
                let attempts = attempts.clone();
                move |_| {
                    attempts.fetch_add(1, Ordering::SeqCst);
                    async { Err(LazerLoadError::Unavailable("pending".into())) }
                }
            },
            |_| Box::pin(async {}),
            cancel,
        )
        .await;
        assert!(matches!(result, Err(LazerLoadError::Cancelled)));
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn cancelled_cache_install_preserves_target() {
        let cache = tempfile::tempdir().unwrap();
        let target = cache.path().join("2026.518.0.json");
        std::fs::write(&target, b"old").unwrap();
        let cancel = CancellationToken::new();
        cancel.cancel();
        assert_eq!(
            install_cache(&target, b"new", &cancel).unwrap_err().kind(),
            io::ErrorKind::Interrupted
        );
        assert_eq!(std::fs::read(target).unwrap(), b"old");
    }

    #[derive(Debug)]
    struct SlowResolver;
    impl AddressResolver for SlowResolver {
        fn resolve<'a>(&'a self, _: &'a str) -> ResolveFuture<'a> {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(vec!["1.1.1.1".parse().unwrap()])
            })
        }
    }

    #[tokio::test]
    async fn one_deadline_includes_dns_resolution() {
        let fetcher = ReqwestOffsetFetcher::with_resolver(Arc::new(SlowResolver));
        let error = fetcher
            .fetch(
                "https://deadline.test/offsets.json",
                "vosu/test",
                Duration::from_millis(1),
            )
            .await
            .unwrap_err();
        assert_eq!(error.message, "offset request timed out");
    }

    #[cfg(feature = "test-harness")]
    #[test]
    fn explicit_test_network_is_the_only_loopback_fetcher_path() {
        let config = crate::net::TestNetworkConfig::new(
            Arc::new(SlowResolver),
            Arc::from([String::from("offsets.invalid")]),
            Arc::from(["127.0.0.1:24051".parse().unwrap()]),
        )
        .unwrap();
        let harness = ReqwestOffsetFetcher::with_test_network(config);
        assert!(matches!(
            harness.client.destination_policy(),
            DestinationPolicy::LoopbackHarness(_)
        ));
        let production = ReqwestOffsetFetcher::new().unwrap();
        assert!(matches!(
            production.client.destination_policy(),
            DestinationPolicy::PublicOnly
        ));
    }
}
