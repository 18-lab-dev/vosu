//! M6b I0: verified package staging, transactional graph install and runtime handoff.
//!
//! `InstallExecutor` implements P0's `CatalogExecutor`. `start` only spawns the
//! owned operation onto the executor's runtime; nothing installs on the caller
//! thread. The worker, holding the operation gate for its whole lifetime:
//! 1. resolves any interrupted commit journal (`recovery`);
//! 2. prepares **every** pinned add before publishing anything: public-only,
//!    credential-free download (`download`), exact size + SHA-256 from the
//!    retained handle, staged ZIP preflight on the destination filesystem
//!    (`preflight`), confined assets and SDK manifest/schema/catalog mirror
//!    equality plus target library/UI checks (`prepare`);
//! 3. journals and commits the dependency-first graph all-or-rollback, with
//!    provenance, approvals and `installed_for` records (`commit`);
//! 4. only then hands committed packages to the runtime under the **current**
//!    T0 policy: native plugins through `RuntimeHandoff`, web packages through
//!    the existing host rescan.
//!
//! Cancellation is honoured until publication starts; publication always runs
//! to commit or rollback, and the operation is finished only afterwards.
//! `shutdown` cancels and joins the worker. No lock is held across network,
//! filesystem, plugin or handoff work.
mod commit;
mod download;
mod installed;
mod preflight;
mod prepare;
mod recovery;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod tests_commit;
#[cfg(test)]
mod tests_prepare;

#[cfg(test)]
pub(in crate::catalog) use commit::FaultPoint;
pub(in crate::catalog) use commit::{Faults, commit_removal};
pub use download::{DownloadFuture, PackageTransport, PublicPackageTransport};
pub use installed::{
    InstalledDocument, InstalledFor, InstalledKind, InstalledRecord, InstalledStore, MAX_RECORDS,
    RECORDS_FILE,
};
pub(in crate::catalog) use installed::{publish_records, read_manifest};
pub use preflight::MAX_PACKAGE_ENTRIES;
pub use recovery::{JOURNAL_FILE, Resolution, recover};

use crate::catalog::{
    approvals::Approval,
    plans::{CatalogExecutor, CatalogOperation, PinnedPlan},
    sources::{SourceGeneration, SourceRecord},
    trust::{CurrentPolicy, InstalledPolicy, PolicyError, Requirement},
};
use std::{
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex, PoisonError},
};
use tokio_util::sync::CancellationToken;
use vosu_plugin_api::manifest::{PackageKind, Runtime};

/// Distinct package roots plus records/journal state and private downloads.
/// Each kind root holds its own staging and backups, so every publication
/// rename stays on the destination's filesystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstallLayout {
    pub state: PathBuf,
    pub plugins: PathBuf,
    pub tools: PathBuf,
    pub overlays: PathBuf,
    pub downloads: PathBuf,
}
impl InstallLayout {
    pub fn new(
        state: PathBuf,
        plugins: PathBuf,
        tools: PathBuf,
        overlays: PathBuf,
        downloads: PathBuf,
    ) -> Result<Self, InstallError> {
        let roots = [&plugins, &tools, &overlays];
        let distinct = roots
            .iter()
            .enumerate()
            .all(|(i, a)| roots[..i].iter().all(|b| a != b));
        if !distinct
            || roots
                .iter()
                .chain([&&state, &&downloads])
                .any(|path| !path.is_absolute())
        {
            return Err(InstallError::Layout);
        }
        Ok(Self {
            state,
            plugins,
            tools,
            overlays,
            downloads,
        })
    }
    pub fn root(&self, kind: PackageKind) -> &Path {
        match kind {
            PackageKind::Plugin => &self.plugins,
            PackageKind::Tool => &self.tools,
            PackageKind::Overlay => &self.overlays,
        }
    }
    /// One portable path component per package id; never a traversal.
    pub fn destination(&self, kind: PackageKind, id: &str) -> Result<PathBuf, InstallError> {
        if id.is_empty()
            || id.starts_with('.')
            || id.contains(['/', '\\', ':', '\0'])
            || !vosu_plugin_api::manifest::files::portable_component(id)
        {
            return Err(InstallError::Unsupported(id.to_owned()));
        }
        Ok(self.root(kind).join(id))
    }
}

/// Redacted local diagnostics: never URLs, response bodies or credentials.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallError {
    #[error("catalog install cancelled")]
    Cancelled,
    #[error("catalog executor is shut down")]
    Shutdown,
    #[error("confirmed plan missing")]
    NoPlan,
    #[error("package download failed")]
    Download,
    #[error("package size mismatch")]
    SizeMismatch,
    #[error("package SHA-256 mismatch")]
    HashMismatch,
    #[error("{0} is too large to install; raise the package limit in Settings")]
    TooLarge(String),
    #[error("{0} is not installable on this host")]
    Unsupported(String),
    #[error("unsafe package archive: {0}")]
    UnsafeArchive(String),
    #[error("package asset escapes or is missing: {0}")]
    AssetEscapes(String),
    #[error("invalid package asset: {0}")]
    InvalidAsset(String),
    #[error("package differs from its catalog entry: {0}")]
    Manifest(String),
    #[error("package has no library for this host target")]
    MissingTarget,
    #[error("{0} appears twice in one plan")]
    DuplicateAdd(String),
    #[error("{0} exists but is not managed by the catalog; it is never overwritten")]
    Unmanaged(String),
    #[error("{0} is installed from a different source")]
    DifferentSource(String),
    #[error(
        "{0} is installed and named in replaces; data-preserving replacement is not performed by catalog install"
    )]
    ReplacementMigration(String),
    #[error("installed catalog records are unreadable or invalid")]
    RecordsInvalid,
    #[error("installed package {0} is missing or differs from its record")]
    InstalledManifest(String),
    #[error("catalog install recovery required")]
    RecoveryRequired,
    #[error("invalid install layout")]
    Layout,
    #[error("install worker failed")]
    Worker,
    #[error("installed process plugin did not stop: {0}")]
    Stop(String),
    #[error("filesystem operation failed: {0}")]
    Io(String),
    #[cfg(test)]
    #[error("simulated crash")]
    SimulatedCrash,
}
impl InstallError {
    /// Keep only the error kind; OS messages are fixed text without paths.
    pub(super) fn io(error: std::io::Error) -> Self {
        Self::Io(error.kind().to_string())
    }
}

/// Post-commit runtime outcome per package; installation itself succeeded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoadStatus {
    Loaded,
    /// Served by the existing web host after its rescan (until M7).
    WebRescanned,
    ApprovalRequired,
    /// Current source policy/approval refuses execution (T0 §7.1).
    Refused(PolicyError),
    Failed(String),
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadReport {
    pub kind: InstalledKind,
    pub id: String,
    pub status: LoadStatus,
}

/// Exact committed package and its current-policy runtime requirement.
pub struct LoadRequest {
    pub record: InstalledRecord,
    pub directory: PathBuf,
    pub requirement: Requirement,
}

pub type HandoffFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Root composition seam. Called only after the complete graph committed.
pub trait RuntimeHandoff: Send + Sync {
    fn load_plugin(self: Arc<Self>, request: LoadRequest) -> HandoffFuture<'static, LoadStatus>;
    /// Blocking, called before directories move or on source revocation.
    /// Returns whether the old generation should resume if publication fails.
    fn stop_process(&self, _id: &str) -> Result<bool, String> {
        Ok(false)
    }
    fn resume_process(&self, _id: &str) {}
    fn forget_process(&self, _id: &str) {}
    fn rescan_web(&self) -> HandoffFuture<'_, ()>;
    /// The installed graph durably published a new generation (before any
    /// load). Composition refreshes generation-pinned consumers such as G0
    /// game offers here. Must stay synchronous and bounded (no I/O).
    fn installed_changed(&self) {}
}

/// Fresh S0 source policy at the load boundary; `None` refuses every load.
pub trait PolicySource: Send + Sync {
    fn current(&self) -> Option<(SourceGeneration, Vec<SourceRecord>)>;
    /// A6b.7 daemon-lifetime marker: in-process code of this installed
    /// plugin id was revoked while running and stays in the address space,
    /// so every later load is refused until the daemon restarts.
    fn pending_restart(&self, _plugin: &str) -> bool {
        false
    }
}
impl<F> PolicySource for F
where
    F: Fn() -> Option<(SourceGeneration, Vec<SourceRecord>)> + Send + Sync,
{
    fn current(&self) -> Option<(SourceGeneration, Vec<SourceRecord>)> {
        self()
    }
}

struct Shared {
    store: Arc<InstalledStore>,
    transport: Arc<dyn PackageTransport>,
    handoff: Arc<dyn RuntimeHandoff>,
    policy: Arc<dyn PolicySource>,
    cancel: CancellationToken,
    reports: Mutex<Vec<LoadReport>>,
    faults: Mutex<commit::Faults>,
}

/// Bounded single-worker executor: P0 admits one running operation.
pub struct InstallExecutor {
    shared: Arc<Shared>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    runtime: tokio::runtime::Handle,
}
impl InstallExecutor {
    pub fn new(
        store: Arc<InstalledStore>,
        transport: Arc<dyn PackageTransport>,
        handoff: Arc<dyn RuntimeHandoff>,
        policy: Arc<dyn PolicySource>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                store,
                transport,
                handoff,
                policy,
                cancel: CancellationToken::new(),
                reports: Mutex::new(Vec::new()),
                faults: Mutex::new(commit::Faults::default()),
            }),
            worker: Mutex::new(None),
            runtime,
        }
    }
    /// Load outcomes of the most recent committed operation, or of
    /// [`Self::load_committed`] until the next operation commits.
    pub fn load_reports(&self) -> Vec<LoadReport> {
        self.shared
            .reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub(crate) fn update_plugin_report(&self, id: &str, status: Option<LoadStatus>) {
        let mut reports = self
            .shared
            .reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        let matching = reports
            .iter()
            .position(|report| report.kind == InstalledKind::Plugin && report.id == id);
        match (matching, status) {
            (Some(index), Some(status)) => reports[index].status = status,
            (Some(index), None) => {
                reports.remove(index);
            }
            (None, Some(status)) => reports.push(LoadReport {
                kind: InstalledKind::Plugin,
                id: id.to_owned(),
                status,
            }),
            (None, None) => {}
        }
    }

    /// Startup load of the already committed graph (the store's `open` ran
    /// recovery first) through the same current-policy handoff as a fresh
    /// install, dependency-first in record order. Composition calls it once,
    /// before discovery starts and before any catalog operation can run.
    pub async fn load_committed(&self) -> Vec<LoadReport> {
        let layout = self.shared.store.layout().clone();
        let records = self.shared.store.snapshot().records;
        let reports = hand_off(&self.shared, &layout, &records).await;
        self.shared
            .reports
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone_from(&reports);
        reports
    }
    /// Await the current operation without cancelling it (its job is
    /// finished, committed or rolled back, when this returns).
    pub async fn wait_idle(&self) {
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            let _ = worker.await;
        }
    }
    /// Cancel and join the owned worker. Publication already in progress
    /// finishes its commit or rollback first; the job is then finished.
    pub async fn shutdown(&self) {
        self.shared.cancel.cancel();
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take();
        if let Some(worker) = worker {
            let _ = worker.await;
        }
    }
    #[cfg(test)]
    fn inject(&self, faults: commit::Faults) {
        *self.shared.faults.lock().unwrap() = faults;
    }
}
impl Drop for InstallExecutor {
    fn drop(&mut self) {
        // An unjoined worker still rolls back and finishes its job.
        self.shared.cancel.cancel();
    }
}
impl CatalogExecutor for InstallExecutor {
    fn start(&self, operation: CatalogOperation) -> Result<(), String> {
        if self.shared.cancel.is_cancelled() {
            return Err(InstallError::Shutdown.to_string());
        }
        let mut worker = self.worker.lock().unwrap_or_else(PoisonError::into_inner);
        if worker.as_ref().is_some_and(|handle| !handle.is_finished()) {
            return Err("catalog executor busy".into());
        }
        let shared = Arc::clone(&self.shared);
        *worker = Some(self.runtime.spawn(run(shared, operation)));
        Ok(())
    }
}

async fn run(shared: Arc<Shared>, operation: CatalogOperation) {
    let confirmed = operation
        .confirmed()
        .map(|c| (Arc::clone(&c.plan), c.approvals.clone()));
    let result = match confirmed {
        Some((plan, approvals)) => execute(&shared, &operation, &plan, approvals).await,
        None => Err(InstallError::NoPlan),
    };
    operation.finish(result.map_err(|error| error.to_string()));
}

async fn execute(
    shared: &Arc<Shared>,
    operation: &CatalogOperation,
    plan: &PinnedPlan,
    approvals: Vec<Approval>,
) -> Result<(), InstallError> {
    let layout = shared.store.layout().clone();
    let recovered = layout.clone();
    tokio::task::spawn_blocking(move || recover(&recovered))
        .await
        .map_err(|_| InstallError::Worker)??;
    let common_defs = Arc::new(
        crate::state::schema::SchemaRegistry::with_core()
            .map_err(|_| InstallError::Worker)?
            .common_defs()
            .clone(),
    );
    let context = prepare::PrepareContext {
        layout: &layout,
        transport: shared.transport.as_ref(),
        common_defs: &common_defs,
        cancel: &shared.cancel,
    };
    let mut prepared = Vec::with_capacity(plan.adds().len());
    for add in plan.adds() {
        if shared.cancel.is_cancelled() {
            return Err(InstallError::Cancelled);
        }
        operation.progress("downloading", &add.entry.id);
        prepared.push(prepare::prepare(&context, add).await?);
    }
    if shared.cancel.is_cancelled() {
        return Err(InstallError::Cancelled);
    }
    operation.progress("installing", "");
    let store = Arc::clone(&shared.store);
    let faults = *shared.faults.lock().unwrap_or_else(PoisonError::into_inner);
    let handoff = Arc::clone(&shared.handoff);
    let committed = tokio::task::spawn_blocking(move || {
        let current = store.snapshot();
        let mut stopped = Vec::new();
        let result = commit::commit(
            store.layout(),
            &current,
            prepared,
            &approvals,
            faults,
            |next| {
                for record in next.iter().rev() {
                    if current
                        .record(record.package_kind(), &record.id)
                        .and_then(|previous| previous.artifact.as_ref())
                        .is_some_and(|artifact| artifact.runtime == Runtime::Process)
                    {
                        let resume = handoff
                            .stop_process(&record.id)
                            .map_err(InstallError::Stop)?;
                        stopped.push((record.id.clone(), resume));
                    }
                }
                Ok(())
            },
        );
        match result {
            Ok(committed) => {
                store.published(committed.document);
                for (id, _) in stopped {
                    handoff.forget_process(&id);
                }
                Ok(committed.records)
            }
            Err(error) => {
                if !matches!(error, InstallError::RecoveryRequired) {
                    for (id, resume) in stopped.iter().rev() {
                        if *resume {
                            handoff.resume_process(id);
                        }
                    }
                }
                Err(error)
            }
        }
    })
    .await
    .map_err(|_| InstallError::Worker)??;
    shared.handoff.installed_changed();
    operation.progress("loading", "");
    let reports = hand_off(shared, &layout, &committed).await;
    *shared
        .reports
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = reports;
    Ok(())
}

async fn hand_off(
    shared: &Shared,
    layout: &InstallLayout,
    records: &[InstalledRecord],
) -> Vec<LoadReport> {
    let current = shared.policy.current();
    let mut reports = Vec::with_capacity(records.len());
    let mut web = Vec::new();
    for record in records {
        let matches_fixture = record.matches_fixture_source();
        if record.fixture {
            tracing::info!(
                id = %record.id,
                source = record.source.as_str(),
                trust = "official-fixture",
                fixture = true,
                selected = matches_fixture,
                "found installed catalog fixture"
            );
        }
        let kind = record.package_kind();
        if kind != PackageKind::Plugin {
            web.push(record);
            continue;
        }
        if !matches_fixture {
            reports.push(LoadReport {
                kind: record.kind,
                id: record.id.clone(),
                status: LoadStatus::Refused(PolicyError::FixtureSourceMismatch),
            });
            continue;
        }
        let status = match (
            &current,
            &record.artifact,
            layout.destination(kind, &record.id),
        ) {
            (Some((generation, sources)), Some(artifact), Ok(directory)) => {
                let policy = CurrentPolicy {
                    generation: *generation,
                    sources,
                };
                let requirement = if artifact.runtime == Runtime::Process {
                    // Even an unapproved process package is registered. M5's
                    // approval gate is the only place it can acquire authority.
                    policy.requirement(artifact)
                } else {
                    policy.authorize_load(
                        &InstalledPolicy::new(artifact.clone(), None)
                            .with_pending_restart(shared.policy.pending_restart(&record.id)),
                    )
                };
                match requirement {
                    Err(error) => LoadStatus::Refused(error),
                    Ok(requirement) => {
                        Arc::clone(&shared.handoff)
                            .load_plugin(LoadRequest {
                                record: record.clone(),
                                directory,
                                requirement,
                            })
                            .await
                    }
                }
            }
            _ => LoadStatus::Refused(PolicyError::StalePolicy),
        };
        reports.push(LoadReport {
            kind: record.kind,
            id: record.id.clone(),
            status,
        });
    }
    if !web.is_empty() {
        shared.handoff.rescan_web().await;
        reports.extend(web.into_iter().map(|record| LoadReport {
            kind: record.kind,
            id: record.id.clone(),
            status: LoadStatus::WebRescanned,
        }));
    }
    reports
}
