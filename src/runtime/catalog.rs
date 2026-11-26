//! M6b K0 root composition: the single daemon-lifetime owner of the core
//! catalog services (`vosu_core::catalog::runtime::CatalogRuntime`), its
//! install roots and the in-process/web runtime handoff.
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
use super::*;
use std::sync::{OnceLock, atomic::AtomicU64};
use vosu_core::{
    catalog::{
        install::{
            HandoffFuture, InstallError, InstallLayout, LoadRequest, LoadStatus, PackageTransport,
            PublicPackageTransport, RuntimeHandoff,
        },
        installed::UninstallService,
        resolver::BuiltinProviders,
        runtime::{CatalogHost, CatalogRuntime},
        trust::TrustTier,
    },
    plugin::{DEFAULT_QUEUE_CAPACITY, PluginInstanceId, WorkerPool},
};

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

/// In-process load through the plugin host; web packages through the
/// existing tool/overlay hosts until M7.
struct RootHandoff {
    host: Arc<PluginHost>,
    plugin_data: PathBuf,
    overlays: Option<PathBuf>,
    pool: Arc<WorkerPool>,
    next_instance: AtomicU64,
    tools: OnceLock<Arc<crate::tools::ToolRegistry>>,
    counters: OnceLock<Arc<ReloadableCounterService>>,
}
impl RuntimeHandoff for RootHandoff {
    fn load_in_process<'a>(
        &'a self,
        request: LoadRequest<'a>,
    ) -> HandoffFuture<'a, Result<(), String>> {
        let host = Arc::clone(&self.host);
        let pool = Arc::clone(&self.pool);
        let directory = request.directory.to_path_buf();
        let id = request.record.id.clone();
        let source = request.record.source.as_str().to_owned();
        // Current-policy tier from `authorize_load`: the exact installed
        // source identity is official. Selects the SDK context and
        // `TrustedOfficial` schema registration, exactly as I0 compiled it.
        let official = request.requirement.trust == TrustTier::Official;
        let data = self.plugin_data.join(&id);
        let instance = PluginInstanceId::new(self.next_instance.fetch_add(1, Ordering::Relaxed));
        Box::pin(async move {
            tokio::task::spawn_blocking(move || {
                let snapshot = host.validation_snapshot();
                if snapshot.providers.iter().any(|provider| provider.id == id) {
                    // Loaded native code cannot be unloaded safely.
                    return Err(
                        "an instance of this plugin is already loaded; restart vosu to load the installed version"
                            .to_owned(),
                    );
                }
                let loaded = vosu_core::plugin::load_installed(
                    crate::application_version(),
                    &snapshot,
                    &directory,
                    &source,
                    official,
                )
                .map_err(|error| error.to_string())?;
                host.create_instance(instance, &loaded, data, Some(pool))
                    .map_err(|error| error.to_string())
            })
            .await
            .map_err(|error| error.to_string())?
        })
    }
    fn rescan_web(&self) -> HandoffFuture<'_, ()> {
        Box::pin(async move {
            if let Some(tools) = self.tools.get() {
                tools.rescan_installed().await;
            }
            // The overlay host serves its current static folder on demand.
            if let (Some(counters), Some(overlays)) = (self.counters.get(), &self.overlays)
                && counters.current_root() != *overlays
            {
                tracing::warn!(
                    "the static folder changed after startup; catalog overlays install into the startup folder until vosu restarts"
                );
            }
        })
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
    pub async fn start(
        host: Arc<PluginHost>,
        roots: CatalogRoots,
        transport: Arc<dyn PackageTransport>,
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
            tools: OnceLock::new(),
            counters: OnceLock::new(),
        });
        let facts = CatalogHost {
            application: crate::application_version().clone(),
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
        for report in runtime.load_installed().await {
            if !matches!(report.status, LoadStatus::Loaded | LoadStatus::WebRescanned) {
                tracing::warn!(
                    id = %report.id,
                    status = ?report.status,
                    "installed catalog package was not loaded"
                );
            }
        }
        let runtime = Arc::new(runtime);
        let builtins = Arc::new(builtin_providers()?);
        let restart = Arc::new(removal::RestartRequired::default());
        let removal = Arc::new(removal::RootRemoval {
            handoff: Arc::clone(&handoff),
            runtime: OnceLock::new(),
            restart: Arc::clone(&restart),
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

    /// Web-package hosts exist only after recovery ran; attach them before
    /// any catalog operation can start.
    pub fn attach_web_hosts(
        &self,
        tools: Arc<crate::tools::ToolRegistry>,
        counters: Arc<ReloadableCounterService>,
    ) {
        let _ = self.handoff.tools.set(tools);
        let _ = self.handoff.counters.set(counters);
    }

    /// Core service handles for H0's routes.
    pub fn services(&self) -> &Arc<CatalogRuntime> {
        &self.runtime
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
fn builtin_providers() -> io::Result<BuiltinProviders> {
    let registry = vosu_core::state::schema::SchemaRegistry::with_core()
        .map_err(|error| io::Error::other(error.to_string()))?;
    let bundled = crate::plugins::BUNDLED
        .iter()
        .map(|plugin| vosu_plugin_api::manifest::PackageManifest::parse(plugin.manifest))
        .collect::<Result<Vec<_>, _>>()
        .map_err(|error| io::Error::other(error.to_string()))?;
    Ok(BuiltinProviders::from_registry(&registry, &bundled))
}

#[path = "catalog_removal.rs"]
mod removal;
