//! Versioned cache, embedded, and local generator lazer offset resolution.

use std::future::Future;
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

pub const RETRY_DELAYS: [Duration; 5] = [
    Duration::from_secs(30),
    Duration::from_secs(60),
    Duration::from_secs(120),
    Duration::from_secs(240),
    Duration::from_secs(300),
];

#[derive(Debug, Error)]
pub enum LazerLoadError {
    #[error(transparent)]
    Schema(#[from] LazerOffsetsError),
    #[error("offset cache I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("offset generation unavailable for osu! {0}: {1}")]
    Unavailable(String, String),
    #[error("offset resolution cancelled")]
    Cancelled,
}

pub type GenerationFuture<'a> = Pin<Box<dyn Future<Output = io::Result<Bytes>> + Send + 'a>>;

pub trait OffsetGenerator: Send + Sync {
    fn generate<'a>(
        &'a self,
        pid: u32,
        version: &'a str,
        cancel: &'a CancellationToken,
    ) -> GenerationFuture<'a>;
}

pub struct LazerOffsetLoader<G> {
    generator: G,
    cache_dir: PathBuf,
    embedded: Bytes,
}

impl<G: OffsetGenerator> LazerOffsetLoader<G> {
    pub fn new(generator: G, cache_dir: PathBuf, embedded: Bytes) -> Self {
        Self {
            generator,
            cache_dir,
            embedded,
        }
    }

    pub fn cache_path(&self, version: &str) -> Result<PathBuf, LazerLoadError> {
        validate_version(version)?;
        Ok(self.cache_dir.join(format!("{version}.json")))
    }

    pub async fn resolve(
        &self,
        pid: u32,
        version: &str,
        cancel: &CancellationToken,
    ) -> Result<Arc<LazerOffsets>, LazerLoadError> {
        validate_version(version)?;
        if cancel.is_cancelled() {
            return Err(LazerLoadError::Cancelled);
        }
        let cache_path = self.cache_path(version)?;
        if let Some(offsets) = read_matching(&cache_path, version) {
            return Ok(Arc::new(offsets));
        }
        if let Ok(offsets) = LazerOffsets::from_json(&self.embedded)
            && offsets.osu_version == version
        {
            return Ok(Arc::new(offsets));
        }

        let generated = tokio::select! {
            () = cancel.cancelled() => return Err(LazerLoadError::Cancelled),
            result = self.generator.generate(pid, version, cancel) => result,
        }
        .map_err(|error| {
            if error.kind() == io::ErrorKind::Interrupted || cancel.is_cancelled() {
                LazerLoadError::Cancelled
            } else {
                LazerLoadError::Unavailable(version.to_owned(), error.to_string())
            }
        })?;
        if cancel.is_cancelled() {
            return Err(LazerLoadError::Cancelled);
        }
        let offsets = LazerOffsets::from_json(&generated)?;
        if offsets.osu_version != version {
            return Err(LazerLoadError::Unavailable(
                version.to_owned(),
                format!("generator returned osu! version {}", offsets.osu_version),
            ));
        }
        if cancel.is_cancelled() {
            return Err(LazerLoadError::Cancelled);
        }
        install_cache(&cache_path, &generated, cancel)?;
        Ok(Arc::new(offsets))
    }
}

fn read_matching(path: &Path, version: &str) -> Option<LazerOffsets> {
    let bytes = std::fs::read(path).ok()?;
    let offsets = LazerOffsets::from_json(&bytes).ok()?;
    (offsets.osu_version == version).then_some(offsets)
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
            Err(error) => observe(&error),
        }
        let delay = RETRY_DELAYS[retry.min(RETRY_DELAYS.len() - 1)];
        retry = retry.saturating_add(1);
        tokio::select! {
            () = cancel.cancelled() => return Err(LazerLoadError::Cancelled),
            () = sleep(delay) => {}
        }
    }
}

pub async fn resolve_for_instance<G>(
    loader: Arc<LazerOffsetLoader<G>>,
    instance: Arc<crate::instance::Instance>,
) -> Result<Arc<LazerOffsets>, LazerLoadError>
where
    G: OffsetGenerator + 'static,
{
    resolve_for_instance_inner(loader, instance, None).await
}

pub(crate) async fn resolve_for_instance_with_logger<G>(
    loader: Arc<LazerOffsetLoader<G>>,
    instance: Arc<crate::instance::Instance>,
    logger: Logger,
) -> Result<Arc<LazerOffsets>, LazerLoadError>
where
    G: OffsetGenerator + 'static,
{
    resolve_for_instance_inner(loader, instance, Some(logger)).await
}

async fn resolve_for_instance_inner<G>(
    loader: Arc<LazerOffsetLoader<G>>,
    instance: Arc<crate::instance::Instance>,
    logger: Option<Logger>,
) -> Result<Arc<LazerOffsets>, LazerLoadError>
where
    G: OffsetGenerator + 'static,
{
    if instance.client() != crate::model::enums::ClientType::Lazer {
        return Err(LazerLoadError::Unavailable(
            "not a lazer instance".into(),
            String::new(),
        ));
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
                        LazerLoadError::Unavailable(
                            "unknown".into(),
                            "target PID unavailable".into(),
                        )
                    })?;
                    let version =
                        super::lazer_version::discover_for_process(pid).map_err(|error| {
                            LazerLoadError::Unavailable(
                                "unknown".into(),
                                format!("version discovery failed: {error}"),
                            )
                        })?;
                    loader.resolve(pid, &version, &attempt_cancel).await
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
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use tokio::sync::{Barrier, Mutex, Notify};

    struct FakeGenerator {
        result: Mutex<Bytes>,
        calls: Mutex<Vec<(u32, String)>>,
        started: Notify,
        barrier: Option<Barrier>,
    }

    impl FakeGenerator {
        fn new(result: Bytes) -> Self {
            Self {
                result: Mutex::new(result),
                calls: Mutex::new(Vec::new()),
                started: Notify::new(),
                barrier: None,
            }
        }
    }

    impl OffsetGenerator for FakeGenerator {
        fn generate<'a>(
            &'a self,
            pid: u32,
            version: &'a str,
            _: &'a CancellationToken,
        ) -> GenerationFuture<'a> {
            Box::pin(async move {
                self.calls.lock().await.push((pid, version.to_owned()));
                self.started.notify_one();
                if let Some(barrier) = &self.barrier {
                    barrier.wait().await;
                }
                Ok(self.result.lock().await.clone())
            })
        }
    }

    fn fixture(version: &str) -> Bytes {
        let asset = crate::assets::DaemonAssets::get("offsets.json").unwrap();
        let mut value: serde_json::Value = serde_json::from_slice(&asset.data).unwrap();
        value["OsuVersion"] = version.into();
        Bytes::from(serde_json::to_vec(&value).unwrap())
    }

    #[tokio::test]
    async fn mismatched_cache_generates_for_exact_instance_then_reuses_result() {
        let dir = tempfile::tempdir().unwrap();
        let version = "2027.1.0";
        let target = dir.path().join(format!("{version}.json"));
        std::fs::write(&target, fixture("2026.1.0")).unwrap();
        let generated = fixture(version);
        let loader = LazerOffsetLoader::new(
            FakeGenerator::new(generated.clone()),
            dir.path().into(),
            Bytes::new(),
        );
        let cancel = CancellationToken::new();
        assert_eq!(
            loader
                .resolve(4242, version, &cancel)
                .await
                .unwrap()
                .osu_version,
            version
        );
        assert_eq!(std::fs::read(&target).unwrap(), generated);
        assert_eq!(
            loader
                .resolve(99, version, &cancel)
                .await
                .unwrap()
                .osu_version,
            version
        );
        assert_eq!(
            *loader.generator.calls.lock().await,
            vec![(4242, version.to_owned())]
        );
    }

    #[tokio::test]
    async fn rejected_generation_preserves_previous_cache() {
        let dir = tempfile::tempdir().unwrap();
        let version = "2027.1.0";
        let target = dir.path().join(format!("{version}.json"));
        let old = fixture("2025.1.0");
        std::fs::write(&target, &old).unwrap();
        for output in [Bytes::from_static(b"invalid"), fixture("2026.1.0")] {
            let loader =
                LazerOffsetLoader::new(FakeGenerator::new(output), dir.path().into(), Bytes::new());
            assert!(
                loader
                    .resolve(1, version, &CancellationToken::new())
                    .await
                    .is_err()
            );
            assert_eq!(std::fs::read(&target).unwrap(), old);
        }
    }

    #[tokio::test]
    async fn concurrent_generation_converges_atomically() {
        let dir = tempfile::tempdir().unwrap();
        let version = "2027.2.0";
        let generated = fixture(version);
        let mut generator = FakeGenerator::new(generated.clone());
        generator.barrier = Some(Barrier::new(2));
        let loader = LazerOffsetLoader::new(generator, dir.path().into(), Bytes::new());
        let cancel = CancellationToken::new();
        let (a, b) = tokio::join!(
            loader.resolve(1, version, &cancel),
            loader.resolve(2, version, &cancel)
        );
        assert_eq!(a.unwrap().osu_version, version);
        assert_eq!(b.unwrap().osu_version, version);
        assert_eq!(
            std::fs::read(dir.path().join(format!("{version}.json"))).unwrap(),
            generated
        );
    }

    #[tokio::test]
    async fn cancellation_interrupts_inflight_generation_without_writing_cache() {
        let dir = tempfile::tempdir().unwrap();
        let version = "2027.3.0";
        let loader = LazerOffsetLoader::new(
            FakeGenerator::new(fixture(version)),
            dir.path().into(),
            Bytes::new(),
        );
        let _hold = loader.generator.result.lock().await;
        let cancel = CancellationToken::new();
        let (result, ()) = tokio::join!(loader.resolve(7, version, &cancel), async {
            loader.generator.started.notified().await;
            cancel.cancel();
        });
        assert!(matches!(result, Err(LazerLoadError::Cancelled)));
        assert!(!loader.cache_path(version).unwrap().exists());
    }

    #[tokio::test(start_paused = true)]
    async fn local_readiness_retries_back_off_until_success() {
        let attempts = AtomicUsize::new(0);
        let started = tokio::time::Instant::now();
        let offsets = retry_pending(
            |_| async {
                if attempts.fetch_add(1, Ordering::SeqCst) < 6 {
                    Err(LazerLoadError::Unavailable(
                        "2027.1.0".into(),
                        "assemblies not ready".into(),
                    ))
                } else {
                    Ok(Arc::new(
                        LazerOffsets::from_json(&fixture("2027.1.0")).unwrap(),
                    ))
                }
            },
            tokio_sleep,
            CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(offsets.osu_version, "2027.1.0");
        assert_eq!(attempts.load(Ordering::SeqCst), 7);
        assert_eq!(
            started.elapsed(),
            Duration::from_secs(30 + 60 + 120 + 240 + 300 + 300)
        );
    }

    #[tokio::test]
    async fn cancellation_interrupts_readiness_retry_delay() {
        let cancel = CancellationToken::new();
        let attempted = Notify::new();
        let (result, ()) = tokio::join!(
            retry_pending(
                |_| async {
                    attempted.notify_one();
                    Err(LazerLoadError::Unavailable(
                        "2027.1.0".into(),
                        "not ready".into(),
                    ))
                },
                tokio_sleep,
                cancel.clone()
            ),
            async {
                attempted.notified().await;
                cancel.cancel();
            }
        );
        assert!(matches!(result, Err(LazerLoadError::Cancelled)));
    }
}
