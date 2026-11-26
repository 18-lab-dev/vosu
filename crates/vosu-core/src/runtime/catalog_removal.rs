//! U0 removal effects for the root composition (M6b H0).
//!
//! `stop` runs before anything moves: catalog process plugins and tools are
//! stopped. In-process native code keeps running until vosu restarts (A6b.7).
//! After commit `removed` forgets the stopped processes, records the native
//! restart requirements, rescans the web hosts and republishes offers.
use super::*;
use crate::catalog::{
    install::{HandoffFuture, InstalledKind},
    installed::{RemovalPlan, RemovalRuntime},
    runtime::CatalogRuntime,
};
use std::{
    collections::BTreeSet,
    fs,
    sync::{OnceLock, Weak},
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
    pub(super) suspended: parking_lot::Mutex<Vec<String>>,
}

impl RootRemoval {
    /// Catalog-installed plugin ids with a live in-process instance.
    fn live(&self) -> BTreeSet<String> {
        self.handoff
            .host
            .validation_snapshot()
            .providers
            .into_iter()
            .filter(|provider| !provider.builtin && self.handoff.process(&provider.id).is_none())
            .map(|provider| provider.id)
            .collect()
    }
}

impl RemovalRuntime for RootRemoval {
    fn stop<'a>(&'a self, plan: &'a RemovalPlan) -> HandoffFuture<'a, Result<(), String>> {
        Box::pin(async move {
            let ids: Vec<_> = plan
                .removals
                .iter()
                .filter(|removal| removal.kind == InstalledKind::Plugin)
                .map(|removal| removal.id.clone())
                .collect();
            let handoff = Arc::clone(&self.handoff);
            let suspended = tokio::task::spawn_blocking(move || {
                let mut suspended = Vec::new();
                for id in ids {
                    match handoff.stop_process(&id) {
                        Ok(true) => suspended.push(id),
                        Ok(false) => {}
                        Err(error) => {
                            for id in suspended.iter().rev() {
                                handoff.resume_process(id);
                            }
                            return Err(error);
                        }
                    }
                }
                Ok(suspended)
            })
            .await
            .map_err(|_| "plugin teardown worker failed".to_owned())??;
            *self.suspended.lock() = suspended;
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

    fn rolled_back(&self) -> HandoffFuture<'_, ()> {
        let suspended = std::mem::take(&mut *self.suspended.lock());
        let handoff = Arc::clone(&self.handoff);
        Box::pin(async move {
            if let Err(error) = tokio::task::spawn_blocking(move || {
                for id in suspended.iter().rev() {
                    handoff.resume_process(id);
                }
            })
            .await
            {
                tracing::warn!(%error, "could not resume installed process plugins after rollback");
            }
        })
    }

    fn removed<'a>(&'a self, plan: &'a RemovalPlan) -> HandoffFuture<'a, ()> {
        Box::pin(async move {
            self.suspended.lock().clear();
            for removal in &plan.removals {
                if removal.kind == InstalledKind::Plugin {
                    self.handoff.remove_process(&removal.id);
                }
            }
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
                if let Some(executor) = runtime.executor() {
                    for removal in &plan.removals {
                        if removal.kind == InstalledKind::Plugin {
                            executor.update_plugin_report(&removal.id, None);
                        }
                    }
                }
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
                    .overlay
                    .get()
                    .map(|overlay| overlay.values_path(id)),
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
