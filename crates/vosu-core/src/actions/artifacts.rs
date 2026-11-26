//! Invocation-owned job artifacts (AF.12, `docs/v2/api-v1.md` "Job
//! artifacts", `docs/v2/plugin-abi.md` §6.13).
//!
//! An artifact belongs to exactly one job: it is declared while that job's
//! invocation runs, listed only once the job succeeded, readable only under
//! the job's own credential, and lives no longer than the job is retained.
//! The registry stores declarations, never bytes. Lock order: job registry
//! -> artifact cell. The plugin host is called (liveness, release, expiry
//! timer) only after both are released.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use serde::Serialize;
use tokio::sync::watch;
use vosu_plugin_api::CborValue;

use crate::identity::PluginInstanceId;

/// At most this many artifacts per job.
pub const MAX_ARTIFACTS_PER_JOB: usize = 4;
/// At most this many unreleased artifacts across every job.
pub const MAX_LIVE_ARTIFACTS: usize = 256;
/// At most this many concurrent downloads of one artifact.
pub const MAX_DOWNLOADS_PER_ARTIFACT: usize = 4;
/// At most this many concurrent artifact downloads in total.
pub const MAX_DOWNLOADS: usize = 64;

/// The plugin host behind an artifact. The registry keeps it weakly and
/// calls it only with no registry or cell lock held.
pub trait ArtifactHost: Send + Sync {
    /// Whether `provider` is still the live instance that declared it.
    fn provider_live(&self, provider: PluginInstanceId) -> bool;
    /// Runs `task` once after `delay` (the job's retention deadline). A
    /// dropped host never runs it.
    fn expire_after(&self, delay: Duration, task: Box<dyn FnOnce() + Send>);
}

/// Delivers one `ARTIFACT_RELEASE` for the artifact id it is given,
/// through capacity the host reserved at declaration. Dropping it unsent
/// (instance loss) sends nothing.
pub type ReleaseNotice = Box<dyn FnOnce(&str) + Send>;

/// A release notice that is due: send it with no registry lock held.
#[must_use]
pub(crate) struct DueRelease {
    artifact: String,
    notice: ReleaseNotice,
}

impl DueRelease {
    pub(crate) fn send(self) {
        (self.notice)(&self.artifact);
    }
}

/// One validated declaration, as the plugin host hands it to the job.
pub struct ArtifactDeclaration {
    pub provider: PluginInstanceId,
    pub host: Weak<dyn ArtifactHost>,
    pub key: CborValue,
    pub name: String,
    /// Fixed from `name`'s extension by the host.
    pub media_type: &'static str,
    pub size: u64,
    pub release: ReleaseNotice,
}

/// `ARTIFACT_DECLARED` refusal codes (§6.13).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactRefusal {
    #[error("invocation_finished")]
    InvocationFinished,
    #[error("caller_not_supported")]
    CallerNotSupported,
    #[error("too_many_artifacts")]
    TooManyArtifacts,
    #[error("invalid_name")]
    InvalidName,
}

impl ArtifactRefusal {
    pub fn code(self) -> &'static str {
        match self {
            Self::InvocationFinished => "invocation_finished",
            Self::CallerNotSupported => "caller_not_supported",
            Self::TooManyArtifacts => "too_many_artifacts",
            Self::InvalidName => "invalid_name",
        }
    }
}

/// Why a download cannot start.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ArtifactError {
    /// The job is unknown, expired, evicted or not this credential's.
    #[error("unknown_job")]
    UnknownJob,
    /// The job is readable but lists no such artifact.
    #[error("unknown_artifact")]
    UnknownArtifact,
    /// Listed, but released (instance loss, disable, revocation).
    #[error("released_artifact")]
    Released,
    #[error("too_many_downloads")]
    TooManyDownloads,
}

/// One listed artifact as the job object shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactView {
    pub id: String,
    pub name: String,
    #[serde(rename = "type")]
    pub media_type: &'static str,
    pub size: u64,
    pub url: String,
}

struct CellState {
    released: bool,
    downloads: usize,
    /// Still owed to the plugin once the last download ends.
    release: Option<ReleaseNotice>,
}

/// One declared artifact. Shared by its job entry and active downloads.
pub(crate) struct ArtifactCell {
    pub id: String,
    job: String,
    provider: PluginInstanceId,
    host: Weak<dyn ArtifactHost>,
    key: CborValue,
    name: String,
    media_type: &'static str,
    size: u64,
    state: Mutex<CellState>,
    released: watch::Sender<bool>,
}

impl ArtifactCell {
    pub(crate) fn new(id: String, job: &str, declaration: ArtifactDeclaration) -> Self {
        Self {
            id,
            job: job.to_owned(),
            provider: declaration.provider,
            host: declaration.host,
            key: declaration.key,
            name: declaration.name,
            media_type: declaration.media_type,
            size: declaration.size,
            state: Mutex::new(CellState {
                released: false,
                downloads: 0,
                release: Some(declaration.release),
            }),
            released: watch::Sender::new(false),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, CellState> {
        self.state.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub(crate) fn host(&self) -> Option<Arc<dyn ArtifactHost>> {
        self.host.upgrade()
    }

    pub(crate) fn provider(&self) -> PluginInstanceId {
        self.provider
    }

    pub(crate) fn view(&self) -> ArtifactView {
        ArtifactView {
            id: self.id.clone(),
            name: self.name.clone(),
            media_type: self.media_type,
            size: self.size,
            url: format!("/api/v1/jobs/{}/artifacts/{}", self.job, self.id),
        }
    }

    pub(crate) fn is_released(&self) -> bool {
        self.lock().released
    }

    /// Marks this artifact released and wakes every active download. The
    /// returned notice is the plugin's `ARTIFACT_RELEASE`, due now because
    /// no download remains; the caller sends it after unlocking. With
    /// `notify: false` (the provider is gone) nothing is ever sent.
    pub(crate) fn release(&self, notify: bool) -> Option<DueRelease> {
        let mut state = self.lock();
        if state.released {
            return None;
        }
        state.released = true;
        self.released.send_replace(true);
        if !notify {
            state.release = None;
        }
        if state.downloads == 0 {
            self.due(&mut state)
        } else {
            None
        }
    }

    fn due(&self, state: &mut CellState) -> Option<DueRelease> {
        state.release.take().map(|notice| DueRelease {
            artifact: self.id.clone(),
            notice,
        })
    }

    /// Reserves one download slot under the job registry lock.
    pub(crate) fn reserve(
        self: &Arc<Self>,
        total: &Arc<AtomicUsize>,
    ) -> Result<ArtifactDownload, ArtifactError> {
        let mut state = self.lock();
        if state.released {
            return Err(ArtifactError::Released);
        }
        // Increments happen only under the job registry lock, so this
        // check-then-add cannot overshoot; decrements are lock-free.
        if state.downloads == MAX_DOWNLOADS_PER_ARTIFACT
            || total.load(Ordering::Acquire) >= MAX_DOWNLOADS
        {
            return Err(ArtifactError::TooManyDownloads);
        }
        state.downloads += 1;
        total.fetch_add(1, Ordering::AcqRel);
        Ok(ArtifactDownload {
            cell: Arc::clone(self),
            total: Arc::clone(total),
            released: self.released.subscribe(),
        })
    }
}

/// One active download: a slot in the artifact's and the global bounds.
/// Dropping it frees both, and sends the plugin's owed `ARTIFACT_RELEASE`
/// once the last download of a released artifact ends. Drop the download's
/// stream lease or file first, so every `STREAM_CLOSE` precedes it.
pub struct ArtifactDownload {
    cell: Arc<ArtifactCell>,
    total: Arc<AtomicUsize>,
    released: watch::Receiver<bool>,
}

impl ArtifactDownload {
    pub fn artifact(&self) -> &str {
        &self.cell.id
    }
    pub fn provider(&self) -> PluginInstanceId {
        self.cell.provider
    }
    pub fn key(&self) -> &CborValue {
        &self.cell.key
    }
    pub fn name(&self) -> &str {
        &self.cell.name
    }
    pub fn media_type(&self) -> &'static str {
        self.cell.media_type
    }
    pub fn size(&self) -> u64 {
        self.cell.size
    }
    pub fn is_released(&self) -> bool {
        *self.released.borrow()
    }

    /// Resolves once the artifact is released (expiry, revocation, cancel,
    /// eviction); a download selects on it to end promptly.
    pub async fn released(&mut self) {
        // The cell owns the sender and outlives this download.
        let _ = self.released.wait_for(|released| *released).await;
    }
}

impl std::fmt::Debug for ArtifactDownload {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ArtifactDownload")
            .field("artifact", &self.cell.id)
            .field("size", &self.cell.size)
            .finish_non_exhaustive()
    }
}

impl Drop for ArtifactDownload {
    fn drop(&mut self) {
        let due = {
            let mut state = self.cell.lock();
            state.downloads -= 1;
            if state.released && state.downloads == 0 {
                self.cell.due(&mut state)
            } else {
                None
            }
        };
        self.total.fetch_sub(1, Ordering::AcqRel);
        if let Some(due) = due {
            due.send();
        }
    }
}

/// A 128-bit random artifact id, as lowercase hex.
pub(crate) fn mint_id() -> Option<String> {
    use ring::rand::{SecureRandom, SystemRandom};
    let mut bytes = [0_u8; 16];
    SystemRandom::new().fill(&mut bytes).ok()?;
    Some(bytes.iter().map(|byte| format!("{byte:02x}")).collect())
}
