//! `OsuSession`: one attached osu! process generation's `ProcessHandle`
//! (P3's `PlanProcessMemory`/`PlanScanBackend`, wired up by `plugin.rs`)
//! and, for a lazer client, the single offset-resolution task that
//! generation ever runs.
//!
//! Ports the semantics of `src/instance.rs`'s `Instance` (1.x, deleted in
//! C3): install-once (`start_lazer_resolver` refuses a second task, or one
//! started after offsets already installed), cancel-is-synchronous
//! (`cancel`, safe on a dispatch thread), and join-on-detach/destroy
//! (`join_resolver`, which only ever runs off a dispatch thread — see
//! `plugin.rs`'s `handle_detached` and `Drop`). `executable_root` reads
//! live through the pid (`vosu_osu_reader::memory::lazer_version::
//! process_executable_root`), exactly as `Instance::executable_root` did,
//! rather than the daemon-view fact frozen at attach time: a lazer
//! install can move its managed assemblies between retries, and this
//! keeps retry_pending's repeated resolution attempts current.

use std::path::PathBuf;
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use vosu_osu_model::enums::ClientType;
use vosu_osu_model::launch::InstanceSettings;
use vosu_osu_reader::memory::lazer_loader::{
    GenerationIdentity, LazerLoadError, LazerOffsetLoader, LogWarn, OffsetGenerator,
    resolve_for_instance_with_logger,
};
use vosu_osu_reader::memory::lazer_offsets::LazerOffsets;
use vosu_osu_reader::memory::lazer_version::process_executable_root;
use vosu_osu_reader::process::ProcessHandle;

use crate::scan::ScanWaiters;
use crate::settings::OsuSettings;
use std::sync::atomic::AtomicBool;

mod capture;

type LazerResolverHandle = JoinHandle<Result<Arc<LazerOffsets>, LazerLoadError>>;

/// A session's scan-admission cancellation, sharing the reader's liveness.
pub struct SessionScanLifecycle {
    pub(crate) waiters: Arc<ScanWaiters>,
    pub(crate) alive: Arc<AtomicBool>,
}

impl SessionScanLifecycle {
    pub(crate) fn cancel(&self, handle: u64) {
        self.waiters.cancel_session(handle, &self.alive);
    }
}

/// One attached osu! process generation. Always carries a `ProcessHandle`
/// (built by `plugin.rs::register_session` for both stable and lazer
/// clients, since P5's reader threads need it either way); only a lazer
/// client ever starts a resolver.
pub struct OsuSession {
    pub handle: u64,
    pub pid: u32,
    pub generation: u64,
    pub client: ClientType,
    pub process: ProcessHandle,
    pub root: PathBuf,
    pub executable_root: PathBuf,
    pub settings: InstanceSettings,
    lifecycle: CancellationToken,
    scans: Option<SessionScanLifecycle>,
    lazer_offsets: ArcSwapOption<LazerOffsets>,
    resolver: tokio::sync::Mutex<Option<LazerResolverHandle>>,
    capture: tokio::sync::Mutex<Option<JoinHandle<()>>>,
    pub(crate) osu_settings: tokio::sync::watch::Sender<OsuSettings>,
}

impl OsuSession {
    #[must_use]
    #[expect(
        clippy::too_many_arguments,
        reason = "every field is an independent attach-time fact; a builder would just move this list, not shrink it"
    )]
    pub fn new(
        handle: u64,
        pid: u32,
        generation: u64,
        client: ClientType,
        process: ProcessHandle,
        root: PathBuf,
        executable_root: PathBuf,
        settings: InstanceSettings,
        scans: Option<SessionScanLifecycle>,
    ) -> Arc<Self> {
        Arc::new(Self {
            handle,
            pid,
            generation,
            client,
            process,
            root,
            executable_root,
            settings,
            lifecycle: CancellationToken::new(),
            scans,
            lazer_offsets: ArcSwapOption::empty(),
            resolver: tokio::sync::Mutex::new(None),
            capture: tokio::sync::Mutex::new(None),
            osu_settings: tokio::sync::watch::channel(OsuSettings::default()).0,
        })
    }

    #[must_use]
    pub fn lazer_offsets(&self) -> Option<Arc<LazerOffsets>> {
        self.lazer_offsets.load_full()
    }

    #[must_use]
    pub fn lifecycle_token(&self) -> CancellationToken {
        self.lifecycle.child_token()
    }

    /// Synchronous and non-blocking: safe to call from a dispatch thread.
    /// Only sets the cancellation flag — the resolver task notices at its
    /// next `tokio::select!` cancellation point and exits on its own;
    /// actually reclaiming it is the separate, off-thread (or bounded)
    /// [`Self::join_resolver`].
    pub fn cancel(&self) {
        self.lifecycle.cancel();
        if let Some(scans) = &self.scans {
            scans.cancel(self.handle);
        }
    }

    /// Starts at most one pending offset resolver for this session
    /// (install-once, ported from `Instance::start_lazer_resolver_inner`):
    /// a no-op for a non-lazer client, once cancelled, once a resolver is
    /// already running, or once offsets are already installed. Must run on
    /// the plugin's own 2-worker runtime (DG) — `plugin.rs` only ever
    /// calls this from a task it spawned there, never inline on a
    /// dispatch thread.
    pub async fn start_lazer_resolver<G: OffsetGenerator + 'static>(
        self: &Arc<Self>,
        loader: Arc<LazerOffsetLoader<G>>,
        warn: LogWarn,
    ) -> bool {
        if self.client != ClientType::Lazer || self.lifecycle.is_cancelled() {
            return false;
        }
        let mut resolver = self.resolver.lock().await;
        if resolver.is_some() || self.lazer_offsets().is_some() {
            return false;
        }
        let session = Arc::clone(self);
        *resolver = Some(tokio::spawn(async move {
            resolve_for_instance_with_logger(loader, session, warn).await
        }));
        true
    }

    /// Cancels (idempotent) and joins capture before the resolver, if either
    /// was started. Capture emits Detached only after all readers have joined.
    /// The resolver join is Bounded in practice by the resolver's own
    /// `tokio::select!` on every retry/backoff reacting to cancellation
    /// promptly; a caller that needs a hard deadline regardless (as
    /// `OsuPlugin::destroy`'s 5 s grace, abi:82, does) wraps the call in
    /// `tokio::time::timeout`. Never called from a dispatch thread
    /// directly — only from a task `plugin.rs` spawned on its own runtime
    /// (detach) or from `destroy` itself, which is expected to join.
    pub async fn join_resolver(&self) {
        self.cancel();
        if let Some(task) = self.capture.lock().await.take() {
            let _ = task.await;
        }
        let task = self.resolver.lock().await.take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
    #[cfg(test)]
    pub(crate) fn capture_is_some(&self) -> bool {
        self.capture.blocking_lock().is_some()
    }

    /// Test-only: whether a resolver task is currently recorded (started
    /// by [`Self::start_lazer_resolver`] and not yet taken by
    /// [`Self::join_resolver`]). Synchronous (`blocking_lock`) so a plain
    /// `#[test]` can assert on it without an executor; safe to call from
    /// any thread that is not itself inside this session's own async
    /// runtime.
    #[cfg(test)]
    pub(crate) fn resolver_is_some(&self) -> bool {
        self.resolver.blocking_lock().is_some()
    }
}

impl GenerationIdentity for OsuSession {
    fn client(&self) -> ClientType {
        self.client
    }

    fn pid(&self) -> u32 {
        self.pid
    }

    fn is_alive(&self) -> bool {
        self.process.is_alive()
    }

    fn lifecycle_token(&self) -> CancellationToken {
        OsuSession::lifecycle_token(self)
    }

    fn lazer_offsets(&self) -> Option<Arc<LazerOffsets>> {
        OsuSession::lazer_offsets(self)
    }

    /// Publishes validated immutable offsets exactly once for this
    /// session (ported from `Instance::install_lazer_offsets`): refused
    /// once cancelled (including after detach) or once the process has
    /// gone, even for a resolver attempt that was already in flight and
    /// only now finishing.
    fn install_lazer_offsets(&self, offsets: Arc<LazerOffsets>) -> bool {
        if self.client != ClientType::Lazer
            || self.lifecycle.is_cancelled()
            || !self.process.is_alive()
        {
            return false;
        }
        self.lazer_offsets
            .compare_and_swap(&None::<Arc<LazerOffsets>>, Some(offsets))
            .is_none()
    }

    /// A live `/proc`/module-enumeration read through `pid` (ported from
    /// `Instance::executable_root`), not the daemon-view fact frozen at
    /// attach time: a retry needs the target's current root.
    fn executable_root(&self) -> Result<PathBuf, LazerLoadError> {
        process_executable_root(self.pid).map_err(|error| {
            LazerLoadError::Unavailable(
                "unknown".into(),
                format!("version discovery failed: {error}"),
            )
        })
    }
}

#[cfg(test)]
mod tests;
