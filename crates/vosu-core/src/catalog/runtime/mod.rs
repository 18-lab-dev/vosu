//! M6b K0: daemon-lifetime owner of the composed core catalog services.
//!
//! The root runtime builds exactly one [`CatalogRuntime`] and hands its
//! service handles to H0's routes; nothing else constructs these services.
//!
//! Startup order ([`CatalogRuntime::open`], then [`CatalogRuntime::load_installed`]):
//! 1. `CatalogFetchService::open(config_root)`: S0 source recovery reconciles
//!    C0 credential membership before any credential or fetch service exists.
//! 2. `InstalledStore::open(layout)`: I0 crash recovery rolls an interrupted
//!    commit forward or back before anything is discovered or loaded.
//! 3. One `InstallExecutor` over the current-policy adapter (fresh S0 records
//!    plus this module's daemon-lifetime pending-restart markers), and one
//!    G0 [`GameOffers`] registered as the host's discovery observer.
//! 4. `load_installed`: the committed graph loads under current T0 policy.
//!
//! The root starts discovery only after step 4 and exposes routes after that.
//! [`CatalogRuntime::shutdown`] reverses it: the observer is removed, then the
//! executor is cancelled and joined (publication in progress still finishes
//! its commit or rollback first).
//!
//! G0 offers are refreshed after every source publication or mutation and
//! after every installed-graph change, always with
//! [`CatalogRuntime::installed_generation`], the generation P0 plans pin.
//!
//! Fail closed: a corrupt or unrecoverable source or installed store is never
//! rewritten. The affected feature is disabled and reported by
//! [`CatalogRuntime::status`]; the daemon keeps running.
mod policy;
#[cfg(test)]
mod tests;

pub use policy::{PendingRestart, RevocationReport};

use super::{
    credentials::{CredentialError, CredentialInput},
    forges::{CatalogFetchService, FetchContext},
    game::{GameOfferView, GameOffers, InstalledPlugins, OfferError},
    install::{
        InstallError, InstallExecutor, InstallLayout, InstalledStore, LoadReport, PackageTransport,
        PolicySource, RuntimeHandoff,
    },
    plans::{PlanError, PlanRegistry},
    sources::{
        CandidateSnapshot, SnapshotError, SourceGeneration, SourceIdentity, SourceInput,
        SourceRecord, SourceStoreError,
    },
};
use crate::plugin::{PluginHost, host_platform};
use policy::PolicyChange;
use std::sync::Arc;
use tokio_util::sync::CancellationToken;
use vosu_net::streaming::StreamingClient;
use vosu_plugin_api::manifest::{NamespaceRevision, PackageKind, Registration, SchemaProvenance};

/// Features disabled at startup, with the redacted reason. Nothing here is
/// ever repaired implicitly; explicit repair belongs to the owning store.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogStatus {
    pub sources: Option<SourceStoreError>,
    pub installed: Option<InstallError>,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CatalogError {
    #[error("catalog feature disabled: its store is unavailable")]
    Disabled,
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Source(#[from] SourceStoreError),
    #[error(transparent)]
    Snapshot(#[from] SnapshotError),
    #[error(transparent)]
    Install(#[from] InstallError),
    #[error(transparent)]
    Credential(#[from] CredentialError),
    #[error("catalog worker failed")]
    Worker,
}

/// Host facts the composition supplies once.
#[derive(Debug, Clone)]
pub struct CatalogHost {
    pub application: semver::Version,
    /// The `[targets.<triple>]` key of this executable.
    pub target: String,
}

pub struct CatalogRuntime {
    host: Arc<PluginHost>,
    facts: CatalogHost,
    fetch: Option<Arc<CatalogFetchService>>,
    installed: Option<Arc<InstalledStore>>,
    executor: Option<Arc<InstallExecutor>>,
    plans: PlanRegistry,
    offers: Arc<OfferRefresh>,
    pending: Arc<PendingRestart>,
    status: CatalogStatus,
}

/// Current S0 records plus the daemon-lifetime marker, read at every load.
struct CurrentPolicySource {
    fetch: Option<Arc<CatalogFetchService>>,
    pending: Arc<PendingRestart>,
}
impl PolicySource for CurrentPolicySource {
    fn current(&self) -> Option<(SourceGeneration, Vec<SourceRecord>)> {
        self.fetch.as_ref()?.sources().snapshot().ok()
    }
    fn pending_restart(&self, plugin: &str) -> bool {
        self.pending.contains(plugin)
    }
}

/// G0 publication input: one coherent source/installed view per refresh.
struct OfferRefresh {
    host: Arc<PluginHost>,
    fetch: Option<Arc<CatalogFetchService>>,
    installed: Option<Arc<InstalledStore>>,
    offers: Arc<GameOffers>,
}
impl OfferRefresh {
    fn installed_generation(&self) -> u64 {
        self.installed
            .as_ref()
            .map_or(0, |store| store.generation())
    }
    /// Live host plugins (bundled, development and loaded catalog plugins)
    /// plus every installed catalog plugin record, loaded or not.
    fn plugin_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .host
            .validation_snapshot()
            .providers
            .into_iter()
            .map(|provider| provider.id)
            .collect();
        if let Some(store) = &self.installed {
            ids.extend(
                store
                    .snapshot()
                    .records
                    .into_iter()
                    .filter(|record| record.package_kind() == PackageKind::Plugin)
                    .map(|record| record.id),
            );
        }
        ids.sort();
        ids.dedup();
        ids
    }
    fn refresh(&self) {
        let Some(fetch) = &self.fetch else {
            return;
        };
        let Ok(snapshot) = fetch.sources().candidate_snapshot() else {
            return;
        };
        let generation = match &snapshot {
            Some(snapshot) => snapshot.generation(),
            None => match fetch.sources().snapshot() {
                Ok((generation, _)) => generation,
                Err(_) => return,
            },
        };
        let ids = self.plugin_ids();
        let ids: Vec<&str> = ids.iter().map(String::as_str).collect();
        let installed = InstalledPlugins {
            generation: self.installed_generation(),
            ids: &ids,
        };
        match self.offers.refresh(snapshot, generation, &installed) {
            // A stale refusal means a newer publication already won.
            Ok(()) | Err(OfferError::Stale) => {}
            Err(error) => tracing::warn!(%error, "catalog game offers not refreshed"),
        }
    }
}

/// Wraps the root handoff so every committed installed change refreshes G0.
struct NotifyingHandoff {
    inner: Arc<dyn RuntimeHandoff>,
    offers: Arc<OfferRefresh>,
}
impl RuntimeHandoff for NotifyingHandoff {
    fn load_in_process<'a>(
        &'a self,
        request: super::install::LoadRequest<'a>,
    ) -> super::install::HandoffFuture<'a, Result<(), String>> {
        self.inner.load_in_process(request)
    }
    fn rescan_web(&self) -> super::install::HandoffFuture<'_, ()> {
        self.inner.rescan_web()
    }
    fn installed_changed(&self) {
        self.inner.installed_changed();
        self.offers.refresh();
    }
}

impl CatalogRuntime {
    /// Steps 1-3 of the module contract. Synchronous filesystem work: call
    /// from a blocking context. Never fails; disabled features (including an
    /// invalid install layout) are reported.
    pub fn open(
        host: Arc<PluginHost>,
        facts: CatalogHost,
        config_root: &std::path::Path,
        layout: Result<InstallLayout, InstallError>,
        transport: Arc<dyn PackageTransport>,
        handoff: Arc<dyn RuntimeHandoff>,
        runtime: tokio::runtime::Handle,
    ) -> Self {
        let mut status = CatalogStatus::default();
        let fetch = match CatalogFetchService::open(config_root) {
            Ok(fetch) => Some(Arc::new(fetch)),
            Err(error) => {
                tracing::error!(%error, "catalog sources disabled; the store was left untouched");
                status.sources = Some(error);
                None
            }
        };
        let installed = match layout.and_then(InstalledStore::open) {
            Ok(store) => Some(Arc::new(store)),
            Err(error) => {
                tracing::error!(%error, "catalog installation disabled; records were left untouched");
                status.installed = Some(error);
                None
            }
        };
        let pending = Arc::new(PendingRestart::default());
        let offers = Arc::new(OfferRefresh {
            host: Arc::clone(&host),
            fetch: fetch.clone(),
            installed: installed.clone(),
            offers: Arc::new(GameOffers::new(host_platform())),
        });
        let executor = installed.as_ref().map(|store| {
            Arc::new(InstallExecutor::new(
                Arc::clone(store),
                transport,
                Arc::new(NotifyingHandoff {
                    inner: handoff,
                    offers: Arc::clone(&offers),
                }),
                Arc::new(CurrentPolicySource {
                    fetch: fetch.clone(),
                    pending: Arc::clone(&pending),
                }),
                runtime,
            ))
        });
        if fetch.is_some() {
            host.set_discovery_observer(Some(offers.offers.clone()));
            offers.refresh();
        }
        Self {
            host,
            facts,
            fetch,
            installed,
            executor,
            plans: PlanRegistry::new(),
            offers,
            pending,
            status,
        }
    }

    /// Step 4: load the committed graph (before discovery starts).
    pub async fn load_installed(&self) -> Vec<LoadReport> {
        let Some(executor) = &self.executor else {
            return Vec::new();
        };
        let reports = executor.load_committed().await;
        // Loaded plugins are now live host providers.
        self.offers.refresh();
        reports
    }

    /// Unregister the observer, then cancel and join the executor worker.
    pub async fn shutdown(&self) {
        if self.fetch.is_some() {
            self.host.set_discovery_observer(None);
        }
        if let Some(executor) = &self.executor {
            executor.shutdown().await;
        }
    }

    pub fn status(&self) -> &CatalogStatus {
        &self.status
    }
    /// S0/C0 service (sources, credential presence, detection), if enabled.
    pub fn fetch(&self) -> Option<&Arc<CatalogFetchService>> {
        self.fetch.as_ref()
    }
    /// P0 plan registry and shared catalog operation gate.
    pub fn plans(&self) -> &PlanRegistry {
        &self.plans
    }
    /// I0 executor (`CatalogExecutor` for confirmation, `load_reports`).
    pub fn executor(&self) -> Option<&Arc<InstallExecutor>> {
        self.executor.as_ref()
    }
    pub fn installed(&self) -> Option<&Arc<InstalledStore>> {
        self.installed.as_ref()
    }
    /// The installed generation P0 `Prerequisites` and G0 both pin.
    pub fn installed_generation(&self) -> u64 {
        self.offers.installed_generation()
    }
    pub fn offers(&self) -> &Arc<GameOffers> {
        &self.offers.offers
    }
    pub fn offer_views(&self) -> Vec<GameOfferView> {
        self.offers.offers.offers()
    }
    /// Re-publishes G0 from the current S0 snapshot and installed generation.
    /// Every mutation/refresh/install path here already calls it; a caller
    /// that publishes an S0 snapshot through `fetch()` directly must too.
    pub fn refresh_offers(&self) {
        self.offers.refresh();
    }
    /// Installed plugin ids revoked while running ("revocation pending restart").
    pub fn pending_restart(&self) -> Vec<String> {
        self.pending.list()
    }
    pub fn host_facts(&self) -> &CatalogHost {
        &self.facts
    }

    fn fetch_service(&self) -> Result<&Arc<CatalogFetchService>, CatalogError> {
        self.fetch.as_ref().ok_or(CatalogError::Disabled)
    }

    /// Ordered source/enabled replacement. Removed sources lose trust first:
    /// their cleared approvals are durable before the list changes.
    pub async fn replace_sources(
        &self,
        input: Vec<SourceInput>,
    ) -> Result<RevocationReport, CatalogError> {
        self.change_policy(PolicyChange::Replace(input)).await
    }

    /// Dedicated trust route (H0 checks listener/provenance/CSRF and shows
    /// the warning before every grant). Untrust applies A6b.7 revocation.
    pub async fn set_trust(
        &self,
        identity: SourceIdentity,
        trusted: bool,
    ) -> Result<RevocationReport, CatalogError> {
        self.change_policy(PolicyChange::Trust(identity, trusted))
            .await
    }

    async fn change_policy(&self, change: PolicyChange) -> Result<RevocationReport, CatalogError> {
        let fetch = Arc::clone(self.fetch_service()?);
        let operation = self.plans.begin_operation()?;
        let live: Vec<String> = self
            .host
            .validation_snapshot()
            .providers
            .into_iter()
            .map(|provider| provider.id)
            .collect();
        let installed = self.installed.clone();
        let pending = Arc::clone(&self.pending);
        let result = tokio::task::spawn_blocking(move || {
            policy::apply(&fetch, installed.as_deref(), &pending, &live, change)
        })
        .await
        .map_err(|_| CatalogError::Worker)
        .and_then(|result| result);
        operation.finish(result.as_ref().map(|_| ()).map_err(ToString::to_string));
        self.offers.refresh();
        result
    }

    pub async fn set_private_network(
        &self,
        identity: SourceIdentity,
        allowed: bool,
    ) -> Result<SourceGeneration, CatalogError> {
        let fetch = Arc::clone(self.fetch_service()?);
        let operation = self.plans.begin_operation()?;
        let result = tokio::task::spawn_blocking(move || {
            fetch
                .sources()
                .set_private_network(&identity, allowed)
                .map_err(CatalogError::from)
        })
        .await
        .map_err(|_| CatalogError::Worker)
        .and_then(|result| result);
        operation.finish(result.as_ref().map(|_| ()).map_err(ToString::to_string));
        self.offers.refresh();
        result
    }

    pub async fn set_credential(
        &self,
        source: &str,
        input: CredentialInput,
    ) -> Result<(), CatalogError> {
        let fetch = self.fetch_service()?;
        let operation = self.plans.begin_operation()?;
        let result = fetch
            .set_credential(source, input)
            .await
            .map_err(CatalogError::from);
        operation.finish(result.clone().map_err(|error| error.to_string()));
        self.offers.refresh();
        result
    }

    pub async fn delete_credential(&self, source: &str) -> Result<(), CatalogError> {
        let fetch = self.fetch_service()?;
        let operation = self.plans.begin_operation()?;
        let result = fetch
            .delete_credential(source)
            .await
            .map_err(CatalogError::from);
        operation.finish(result.clone().map_err(|error| error.to_string()));
        self.offers.refresh();
        result
    }

    /// Explicit source refresh: one SDK-validated publication against the
    /// actual bundled, loaded and installed registrations, then G0 refresh.
    pub async fn refresh_sources(
        &self,
        client: &StreamingClient,
        cancel: &CancellationToken,
    ) -> Result<Arc<CandidateSnapshot>, CatalogError> {
        let fetch = self.fetch_service()?;
        let inputs = self.registration_inputs();
        let registrations = inputs.registrations();
        let revisions = inputs.revisions();
        let result = fetch
            .refresh(
                client,
                FetchContext {
                    registrations: &registrations,
                    revisions: &revisions,
                    application: &self.facts.application,
                    target: &self.facts.target,
                },
                cancel,
            )
            .await
            .map_err(CatalogError::from);
        self.offers.refresh();
        result
    }

    /// Owned registration context: bundled and live providers from the host,
    /// installed catalog records with their exact source identity.
    pub fn registration_inputs(&self) -> RegistrationInputs {
        let live = self.host.validation_snapshot();
        let mut inputs = RegistrationInputs::default();
        let records = self
            .installed
            .as_ref()
            .map(|store| store.snapshot().records)
            .unwrap_or_default();
        for provider in &live.providers {
            if records.iter().any(|record| {
                record.package_kind() == PackageKind::Plugin && record.id == provider.id
            }) {
                continue;
            }
            inputs.packages.push(OwnedRegistration {
                kind: PackageKind::Plugin,
                id: provider.id.clone(),
                source: if provider.builtin {
                    "bundled"
                } else {
                    "development"
                }
                .into(),
                official: provider.official,
                builtin: provider.builtin,
                replaces: provider.replaces.clone(),
            });
            if let Some(version) = &provider.revision {
                inputs.revisions.push((
                    provider.id.clone(),
                    version.clone(),
                    if provider.builtin {
                        SchemaProvenance::TrustedBundled
                    } else {
                        SchemaProvenance::ThirdParty
                    },
                ));
            }
        }
        for record in records {
            let provider = live.providers.iter().find(|provider| {
                record.package_kind() == PackageKind::Plugin && provider.id == record.id
            });
            let official = record.source.is_official();
            if let Some(version) = provider.and_then(|provider| provider.revision.clone()) {
                inputs.revisions.push((
                    record.id.clone(),
                    version,
                    if official {
                        SchemaProvenance::Official
                    } else {
                        SchemaProvenance::ThirdParty
                    },
                ));
            }
            inputs.packages.push(OwnedRegistration {
                kind: record.package_kind(),
                replaces: provider.map(|p| p.replaces.clone()).unwrap_or_default(),
                id: record.id,
                source: record.source.as_str().into(),
                official,
                builtin: false,
            });
        }
        inputs.revisions.extend(
            live.core_revisions
                .into_iter()
                .map(|(namespace, version)| (namespace, version, SchemaProvenance::TrustedBundled)),
        );
        inputs
    }
}

struct OwnedRegistration {
    kind: PackageKind,
    id: String,
    source: String,
    official: bool,
    builtin: bool,
    replaces: Vec<String>,
}

/// Owned SDK registration context; borrow it into a `FetchContext`.
#[derive(Default)]
pub struct RegistrationInputs {
    packages: Vec<OwnedRegistration>,
    revisions: Vec<(String, semver::Version, SchemaProvenance)>,
}
impl RegistrationInputs {
    pub fn registrations(&self) -> Vec<Registration<'_>> {
        self.packages
            .iter()
            .map(|package| Registration {
                kind: package.kind,
                id: &package.id,
                source: &package.source,
                official: package.official,
                builtin: package.builtin,
                replaces: &package.replaces,
            })
            .collect()
    }
    pub fn revisions(&self) -> Vec<NamespaceRevision<'_>> {
        self.revisions
            .iter()
            .map(|(namespace, version, provenance)| NamespaceRevision {
                namespace,
                version: version.clone(),
                provenance: *provenance,
            })
            .collect()
    }
}
