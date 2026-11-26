//! M6b K0 root composition: the single daemon-lifetime owner of the core
//! catalog services (`vosu_core::catalog::runtime::CatalogRuntime`), its
//! install roots and the native plugin/web runtime handoff.
//!
//! Install roots (`docs/v2/catalog-v2.md` "K0 composition contract"):
//! - state: `<data>/catalog` (`catalog-installed.json`, install journal);
//! - plugins: `<data>/catalog/plugins/<id>` package trees, replaced whole by
//!   the I0 journal. Durable plugin data stays `<data>/plugins/<id>` (the
//!   bundled convention), outside every replaced tree;
//! - tools: core's tool registry user root `<data>/tools`, presented to the
//!   bundled tool host after `ToolRegistry::rescan_installed`;
//! - overlays: the counter static folder resolved at startup, served by the
//!   existing overlay host and pinned for the daemon lifetime;
//! - downloads: `<cache>/catalog/downloads`.
//!
//! I0 never overwrites a directory it did not install, and its transaction
//! names (`.stage-*`, `.vosu-backup-*`) differ from the legacy installers'.
use super::plugin_management::ManagedPlugin;
use super::*;
use crate::{
    catalog::{
        install::{
            HandoffFuture, InstallError, InstallLayout, InstalledRecord, LoadRequest, LoadStatus,
            PackageTransport, PublicPackageTransport, RuntimeHandoff,
        },
        installed::UninstallService,
        resolver::BuiltinProviders,
        runtime::{CatalogHost, CatalogRuntime},
        trust::TrustTier,
    },
    plugin::{
        DEFAULT_QUEUE_CAPACITY, HostError, LoadedPlugin, PluginInstanceId, WorkerPool,
        approval::{ApprovalStore, InstalledApprovals},
        process::{control::StartupStage, supervisor::ProcessPhase},
    },
};
use std::{
    collections::BTreeMap,
    path::Path,
    sync::{OnceLock, atomic::AtomicU64},
};
use vosu_plugin_api::manifest::{HostSupport, PackageKind, Runtime};

/// Catalog-loaded instance ids start far above bundled and `--plugin` ones.
const FIRST_CATALOG_INSTANCE: u64 = 1 << 32;
const CATALOG_CONCURRENT_WORKERS: usize = 4;

/// The `[targets.<triple>]` key this executable loads (catalog-v2 targets).
pub fn catalog_target() -> &'static str {
    if cfg!(all(target_arch = "x86_64", target_os = "windows")) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

pub struct CatalogRoots {
    /// Channel-aware config root (`catalog-source-settings.json`,
    /// `catalog-credentials.toml`).
    pub config: PathBuf,
    pub layout: Result<InstallLayout, InstallError>,
    /// Parent of each catalog plugin's durable `data_dir`.
    pub plugin_data: PathBuf,
}
impl CatalogRoots {
    pub fn production(paths: &Paths, static_root: PathBuf) -> Self {
        let state = paths.data().join("catalog");
        Self {
            config: paths.config().to_path_buf(),
            layout: InstallLayout::new(
                state.clone(),
                state.join("plugins"),
                paths.data().join("tools"),
                static_root,
                paths.cache().join("catalog").join("downloads"),
            ),
            plugin_data: paths.data().join("plugins"),
        }
    }
}

/// Native load through the plugin host; web packages through their hosts.
pub(super) struct RootHandoff {
    host: Arc<PluginHost>,
    plugin_data: PathBuf,
    overlays: Option<PathBuf>,
    pool: Arc<WorkerPool>,
    next_instance: AtomicU64,
    version: semver::Version,
    processes: parking_lot::RwLock<BTreeMap<String, Arc<ManagedPlugin>>>,
    tools: OnceLock<Arc<crate::tools::ToolRegistry>>,
    overlay: OnceLock<Arc<dyn OverlayComposition>>,
}
impl RuntimeHandoff for RootHandoff {
    fn load_plugin(self: Arc<Self>, request: LoadRequest) -> HandoffFuture<'static, LoadStatus> {
        Box::pin(async move {
            tokio::task::spawn_blocking(move || self.load(request))
                .await
                .unwrap_or_else(|error| LoadStatus::Failed(error.to_string()))
        })
    }

    fn stop_process(&self, id: &str) -> Result<bool, String> {
        let Some(plugin) = self.process(id) else {
            return Ok(false);
        };
        let active = self
            .host
            .process_status(plugin.instance())
            .is_some_and(|status| {
                matches!(
                    status.phase,
                    ProcessPhase::Starting | ProcessPhase::Running | ProcessPhase::Backoff { .. }
                )
            });
        match self.host.teardown_instance(plugin.instance()) {
            Ok(()) | Err(HostError::DestroyTimedOut | HostError::UnknownInstance) => Ok(active),
            Err(error) => Err(error.to_string()),
        }
    }

    fn resume_process(&self, id: &str) {
        if let Some(plugin) = self.process(id)
            && let Err(error) = self.host.restart_process_instance(plugin.instance())
        {
            tracing::warn!(plugin = id, %error, "could not resume the installed plugin after rollback");
        }
    }
    fn forget_process(&self, id: &str) {
        self.remove_process(id);
    }

    fn rescan_web(&self) -> HandoffFuture<'_, ()> {
        Box::pin(async move {
            if let Some(tools) = self.tools.get() {
                tools.rescan_installed().await;
            }
            // The overlay host serves its current static folder on demand.
            if let (Some(overlay), Some(overlays)) = (self.overlay.get(), &self.overlays)
                && overlay.current_root() != *overlays
            {
                tracing::warn!(
                    "the static folder changed after startup; catalog overlays install into the startup folder until vosu restarts"
                );
            }
        })
    }
}

impl RootHandoff {
    pub(super) fn process(&self, id: &str) -> Option<Arc<ManagedPlugin>> {
        self.processes.read().get(id).cloned()
    }

    pub(super) fn processes(&self) -> Vec<Arc<ManagedPlugin>> {
        self.processes.read().values().cloned().collect()
    }

    pub(super) fn remove_process(&self, id: &str) {
        if let Some(plugin) = self.processes.write().remove(id) {
            self.host.forget_stopped_process(plugin.instance());
        }
    }

    pub(super) fn validate_process(
        &self,
        record: &InstalledRecord,
        directory: &Path,
    ) -> Result<LoadedPlugin, HostError> {
        let provenance = record
            .provenance()
            .ok_or_else(|| HostError::State("missing installed provenance".into()))?;
        // The old generation being replaced may still be registered during
        // approval review. That is this exact installed id, not a new provider.
        let mut snapshot = self.host.validation_snapshot();
        if self.process(&record.id).is_some() {
            snapshot.providers.retain(|p| p.id != record.id);
        }
        crate::plugin::load_installed_process(
            &self.version,
            &snapshot,
            directory,
            &record.id,
            provenance,
            record.source.is_official(),
        )
    }

    fn load(&self, request: LoadRequest) -> LoadStatus {
        let id = &request.record.id;
        if let Some(plugin) = self.process(id)
            && let Some(status) = self.host.process_status(plugin.instance())
        {
            return match status.phase {
                ProcessPhase::Running => LoadStatus::Loaded,
                _ => {
                    LoadStatus::Failed("the installed process plugin is already registered".into())
                }
            };
        }
        let snapshot = self.host.validation_snapshot();
        if snapshot.providers.iter().any(|p| p.id == *id) || self.host.has_development_process(id) {
            return LoadStatus::Failed("an instance of this plugin is already registered".into());
        }
        if request
            .record
            .artifact
            .as_ref()
            .is_some_and(|artifact| artifact.runtime == Runtime::Process)
        {
            return match self.validate_process(&request.record, &request.directory) {
                Ok(plugin) => self.start_process(&plugin).1,
                Err(error) => LoadStatus::Failed(error.to_string()),
            };
        }
        let loaded = match crate::plugin::load_installed(
            &self.version,
            &snapshot,
            &request.directory,
            request.record.source.as_str(),
            request.requirement.trust == TrustTier::Official,
        ) {
            Ok(plugin) => plugin,
            Err(error) => return LoadStatus::Failed(error.to_string()),
        };
        self.create(&loaded).1
    }

    pub(super) fn start_process(&self, loaded: &LoadedPlugin) -> (Arc<ManagedPlugin>, LoadStatus) {
        let (instance, status) = self.create(loaded);
        let plugin = Arc::new(ManagedPlugin::installed(instance, loaded));
        self.processes
            .write()
            .insert(loaded.manifest().id.clone(), Arc::clone(&plugin));
        (plugin, status)
    }

    fn create(&self, loaded: &LoadedPlugin) -> (PluginInstanceId, LoadStatus) {
        let id = &loaded.manifest().id;
        let instance = PluginInstanceId::new(self.next_instance.fetch_add(1, Ordering::Relaxed));
        let status = match self.host.create_instance(
            instance,
            loaded,
            self.plugin_data.join(id),
            Some(Arc::clone(&self.pool)),
        ) {
            Ok(()) => LoadStatus::Loaded,
            Err(HostError::ProcessStartupFailed(error))
                if error.stage == StartupStage::Approval =>
            {
                LoadStatus::ApprovalRequired
            }
            Err(error) => LoadStatus::Failed(error.to_string()),
        };
        (instance, status)
    }
}

/// Root owner of the composed catalog services; see the module docs.
///
/// H0 adds U0's [`UninstallService`] over the SAME `Arc<InstalledStore>` the
/// I0 executor commits through; every uninstall/promote passes the runtime's
/// one `PlanRegistry`, so both share P0's catalog operation gate.
pub struct CatalogComposition {
    runtime: Arc<CatalogRuntime>,
    handoff: Arc<RootHandoff>,
    uninstall: Option<Arc<UninstallService>>,
    builtins: Arc<BuiltinProviders>,
    restart: Arc<removal::RestartRequired>,
    cancel: CancellationToken,
}
impl CatalogComposition {
    /// Recovery, then the committed graph's load. Call after bundled and
    /// `--plugin` plugins loaded and before discovery starts or any route
    /// exists. Never fails over optional catalog state: corrupt stores are
    /// left untouched and their feature is disabled (`status()`).
    /// `bundle` supplies the executable's version and the compiled bundled
    /// manifests the catalog treats as built-in providers.
    pub async fn start(
        host: Arc<PluginHost>,
        roots: CatalogRoots,
        transport: Arc<dyn PackageTransport>,
        bundle: &Bundle,
        approvals: &ApprovalStore,
    ) -> io::Result<Self> {
        let overlays = roots
            .layout
            .as_ref()
            .ok()
            .map(|layout| layout.overlays.clone());
        let handoff = Arc::new(RootHandoff {
            host: Arc::clone(&host),
            plugin_data: roots.plugin_data,
            overlays,
            pool: WorkerPool::new(CATALOG_CONCURRENT_WORKERS, DEFAULT_QUEUE_CAPACITY),
            next_instance: AtomicU64::new(FIRST_CATALOG_INSTANCE),
            version: bundle.version(),
            processes: parking_lot::RwLock::default(),
            tools: OnceLock::new(),
            overlay: OnceLock::new(),
        });
        let facts = CatalogHost {
            application: bundle.version(),
            target: catalog_target().to_owned(),
        };
        let runtime_handoff: Arc<dyn RuntimeHandoff> = handoff.clone();
        let handle = Handle::current();
        let runtime = tokio::task::spawn_blocking(move || {
            CatalogRuntime::open(
                host,
                facts,
                &roots.config,
                roots.layout,
                transport,
                runtime_handoff,
                handle,
            )
        })
        .await
        .map_err(io::Error::other)?;
        let runtime = Arc::new(runtime);
        let authority: Arc<dyn InstalledApprovals> = runtime.clone();
        approvals.attach_catalog(Arc::downgrade(&authority));
        for report in runtime.load_installed().await {
            if !matches!(report.status, LoadStatus::Loaded | LoadStatus::WebRescanned) {
                tracing::warn!(
                    id = %report.id,
                    status = ?report.status,
                    "installed catalog package was not loaded"
                );
            }
        }
        let builtins = Arc::new(builtin_providers(bundle)?);
        let restart = Arc::new(removal::RestartRequired::default());
        let removal = Arc::new(removal::RootRemoval {
            handoff: Arc::clone(&handoff),
            runtime: OnceLock::new(),
            restart: Arc::clone(&restart),
            suspended: parking_lot::Mutex::default(),
        });
        let _ = removal.runtime.set(Arc::downgrade(&runtime));
        let uninstall = runtime.installed().map(|store| {
            Arc::new(UninstallService::new(
                Arc::clone(store),
                Arc::clone(&builtins),
                removal,
                Handle::current(),
            ))
        });
        Ok(Self {
            runtime,
            handoff,
            uninstall,
            builtins,
            restart,
            cancel: CancellationToken::new(),
        })
    }

    /// Strict public-only package transport. A `test-harness` build with
    /// `VOSU_CATALOG_TEST_SOCKET` serves `https://catalog.invalid:<port>`
    /// packages from that exact loopback listener instead.
    pub fn production_transport() -> Arc<dyn PackageTransport> {
        #[cfg(feature = "test-harness")]
        if let Some(transport) = crate::server::catalog_api::harness_transport() {
            return transport;
        }
        Arc::new(PublicPackageTransport::new(
            vosu_net::streaming::StreamingClient::production(),
        ))
    }

    /// Bind the tool registry after catalog recovery, before routes activate.
    /// Its update view follows this catalog (M7 D1).
    pub fn attach_tools(&self, tools: Arc<crate::tools::ToolRegistry>) {
        let runtime = Arc::clone(&self.runtime);
        tools.set_update_source(Arc::new(move |id: &str| {
            update_available(&runtime, PackageKind::Tool, id)
        }));
        let _ = self.handoff.tools.set(tools);
    }

    /// Bind the overlay host after catalog recovery, before routes activate.
    pub fn attach_overlay(&self, overlay: Arc<dyn OverlayComposition>) {
        let _ = self.handoff.overlay.set(overlay);
    }

    /// Core service handles for H0's routes.
    pub fn services(&self) -> &Arc<CatalogRuntime> {
        &self.runtime
    }

    pub(super) fn attach_management(
        &self,
        management: &mut super::plugin_management::PluginManagement,
    ) {
        management.attach_catalog(Arc::clone(&self.runtime), Arc::clone(&self.handoff));
    }

    /// H0's dashboard-only `/api/catalog/*` facade over these services.
    /// `package_limit` reads the current checked `MAX_PACKAGE_MIB` cap;
    /// `network` is `crate::server::catalog_api::network()` in production.
    pub fn api(
        &self,
        package_limit: Arc<dyn Fn() -> Option<u64> + Send + Sync>,
        network: Arc<dyn crate::server::catalog_api::CatalogNetwork>,
    ) -> crate::server::catalog_api::CatalogApi {
        let restart = Arc::clone(&self.restart);
        crate::server::catalog_api::CatalogApi::new(crate::server::catalog_api::CatalogServices {
            runtime: Arc::clone(&self.runtime),
            uninstall: self.uninstall.clone(),
            builtins: Arc::clone(&self.builtins),
            restart_required: Arc::new(move || restart.list()),
            package_limit,
            network,
            cancel: self.cancel.clone(),
        })
    }

    /// Route work is cancelled, the uninstall worker joined, then the
    /// observer is removed and the executor cancelled and joined.
    pub async fn shutdown(&self) {
        self.cancel.cancel();
        if let Some(uninstall) = &self.uninstall {
            uninstall.shutdown().await;
        }
        self.runtime.shutdown().await;
    }
}

/// R0/U0 built-ins: the actual core schemas plus every compiled bundled
/// manifest's `provides`, never a catalog copy.
fn builtin_providers(bundle: &Bundle) -> io::Result<BuiltinProviders> {
    let registry = crate::state::schema::SchemaRegistry::with_core()
        .map_err(|error| io::Error::other(error.to_string()))?;
    let bundled = bundle
        .plugins
        .iter()
        .map(|plugin| vosu_plugin_api::manifest::PackageManifest::parse(plugin.manifest))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(BuiltinProviders::from_registry(&registry, &bundled))
}

/// Core's read-only update source (M7 D1): the catalog-installed `kind`
/// package `id` has an update when the last validated publication of its
/// exact installed source lists a newer version this host and target can
/// install. Nothing is fetched here; a refresh publishes new candidates.
fn update_available(runtime: &CatalogRuntime, kind: PackageKind, id: &str) -> bool {
    let Some(record) = runtime.installed().and_then(|store| {
        store
            .snapshot()
            .records
            .into_iter()
            .find(|record| record.package_kind() == kind && record.id == id)
    }) else {
        return false;
    };
    let Ok(installed) = semver::Version::parse(&record.version) else {
        return false;
    };
    let Some(snapshot) = runtime
        .fetch()
        .and_then(|fetch| fetch.sources().candidate_snapshot().ok().flatten())
    else {
        return false;
    };
    snapshot.candidates().any(|candidate| {
        candidate.kind == kind
            && candidate.entry.id == id
            && candidate.source.identity == record.source
            && candidate.support.host == HostSupport::Supported
            && candidate.support.target
            && semver::Version::parse(&candidate.entry.version)
                .is_ok_and(|version| version > installed)
    })
}

#[path = "catalog_removal.rs"]
mod removal;
