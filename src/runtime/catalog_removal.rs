//! U0 removal effects for the root composition (M6b H0).
//!
//! `stop` runs before anything moves: legacy-hosted catalog tools stop their
//! processes. Overlays are static files with nothing to stop. In-process
//! plugin code cannot be unloaded safely (A6b.7): the host has no per-plugin
//! teardown, so a live plugin keeps running until vosu restarts. After the
//! durable commit `removed` records every such plugin, and every live
//! consumer that lost a resolved optional grant, as "restart required",
//! rescans the web hosts and republishes G0 offers. A plugin that is still
//! loaded never receives a new load: its package directory is gone.
use super::*;
use std::{
    collections::BTreeSet,
    fs,
    sync::{OnceLock, Weak},
};
use vosu_core::catalog::{
    install::{HandoffFuture, InstalledKind},
    installed::{RemovalPlan, RemovalRuntime},
    runtime::CatalogRuntime,
};

/// Daemon-lifetime list of removed (or optional-grant-losing) plugins whose
/// already loaded in-process code keeps running until restart. Bounded by the
/// installed-record limit; only a restart clears it.
#[derive(Default)]
pub struct RestartRequired(parking_lot::Mutex<BTreeSet<String>>);
impl RestartRequired {
    pub fn list(&self) -> Vec<String> {
        self.0.lock().iter().cloned().collect()
    }
    fn extend(&self, ids: impl IntoIterator<Item = String>) {
        self.0.lock().extend(ids);
    }
}

pub(super) struct RootRemoval {
    pub(super) handoff: Arc<RootHandoff>,
    pub(super) runtime: OnceLock<Weak<CatalogRuntime>>,
    pub(super) restart: Arc<RestartRequired>,
}

impl RootRemoval {
    /// Catalog-installed plugin ids with a live in-process instance.
    fn live(&self) -> BTreeSet<String> {
        self.handoff
            .host
            .validation_snapshot()
            .providers
            .into_iter()
            .filter(|provider| !provider.builtin)
            .map(|provider| provider.id)
            .collect()
    }
}

impl RemovalRuntime for RootRemoval {
    fn stop<'a>(&'a self, plan: &'a RemovalPlan) -> HandoffFuture<'a, Result<(), String>> {
        Box::pin(async move {
            // Consumers first, exactly in the closure's order.
            for removal in &plan.removals {
                if removal.kind == InstalledKind::Tool
                    && let Some(tools) = self.handoff.tools.get()
                {
                    tools.revoke_installed(&removal.id).await;
                }
            }
            Ok(())
        })
    }

    fn removed<'a>(&'a self, plan: &'a RemovalPlan) -> HandoffFuture<'a, ()> {
        Box::pin(async move {
            let live = self.live();
            self.restart.extend(
                plan.removals
                    .iter()
                    .filter(|removal| removal.kind == InstalledKind::Plugin)
                    .map(|removal| removal.id.clone())
                    .chain(plan.dropped_optional.iter().map(|d| d.consumer.clone()))
                    .filter(|id| live.contains(id)),
            );
            self.handoff.rescan_web().await;
            if let Some(runtime) = self.runtime.get().and_then(Weak::upgrade) {
                runtime.refresh_offers();
            }
        })
    }

    fn remove_data<'a>(
        &'a self,
        kind: InstalledKind,
        id: &'a str,
    ) -> HandoffFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let path = match kind {
                InstalledKind::Plugin => {
                    if self.live().contains(id) {
                        return Err(format!(
                            "{id} keeps running until vosu restarts; its data was kept"
                        ));
                    }
                    Some(self.handoff.plugin_data.join(id))
                }
                InstalledKind::Tool => {
                    return match self.handoff.tools.get() {
                        Some(tools) => tools
                            .remove_installed_data(id)
                            .await
                            .map_err(|error| error.kind().to_string()),
                        None => Ok(()),
                    };
                }
                InstalledKind::Overlay => self
                    .handoff
                    .counters
                    .get()
                    .map(|counters| counters.settings_store().values_path(id)),
            };
            let Some(path) = path else {
                return Ok(());
            };
            tokio::task::spawn_blocking(move || match fs::symlink_metadata(&path) {
                Ok(metadata) if metadata.is_dir() => fs::remove_dir_all(&path),
                Ok(_) => fs::remove_file(&path),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(error) => Err(error),
            })
            .await
            .map_err(|_| "data removal worker failed".to_owned())?
            .map_err(|error| error.kind().to_string())
        })
    }
}
