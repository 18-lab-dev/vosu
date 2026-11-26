//! Core uninstall management facade for H0 (no routes here). Every mutation
//! holds P0's shared catalog operation gate, so uninstall, promotion, plan
//! confirmation and installation are serialized, and must share the same
//! `InstalledStore` as I0's `InstallExecutor`.
use super::{InstalledGraph, RemovalPlan, UninstallError, origin, plan_removal};
use crate::catalog::{
    install::{Faults, HandoffFuture, InstalledKind, InstalledStore, commit_removal, recover},
    plans::{CatalogOperation, ConfirmResponse, PlanRegistry},
    resolver::BuiltinProviders,
    sources::SourceIdentity,
};
use std::sync::{Arc, Mutex, PoisonError};
use tokio_util::sync::CancellationToken;

/// Root composition seam for the runtime effects of one removal closure.
pub trait RemovalRuntime: Send + Sync {
    /// Before anything moves: stop all runtime activity of every package in
    /// `plan.removals` (consumers first) and end the `dropped_optional` grants.
    /// In-process native code cannot be unloaded safely; stopping ends its
    /// instances, and a library the OS still holds open can make the directory
    /// move fail, which rolls the whole closure back. An error aborts with
    /// nothing removed. Errors must be redacted local diagnostics.
    fn stop<'a>(&'a self, plan: &'a RemovalPlan) -> HandoffFuture<'a, Result<(), String>>;
    /// After a known rollback, resume process plugins stopped by this attempt.
    fn rolled_back(&self) -> HandoffFuture<'_, ()> {
        Box::pin(async {})
    }
    /// After the durable commit: forget the removed packages, rescan web hosts.
    fn removed<'a>(&'a self, plan: &'a RemovalPlan) -> HandoffFuture<'a, ()>;
    /// Only for the explicit target, only when the user chose `remove_data`.
    /// Never called for autoremoved dependencies, whose data/settings stay.
    fn remove_data<'a>(
        &'a self,
        kind: InstalledKind,
        id: &'a str,
    ) -> HandoffFuture<'a, Result<(), String>>;
}

/// Exact installed identity plus the user's explicit data choice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UninstallRequest {
    pub kind: InstalledKind,
    pub id: String,
    pub source: SourceIdentity,
    pub remove_data: bool,
}

struct Shared {
    store: Arc<InstalledStore>,
    builtins: Arc<BuiltinProviders>,
    runtime: Arc<dyn RemovalRuntime>,
    cancel: CancellationToken,
    faults: Mutex<Faults>,
}

/// Owns at most one removal worker; `shutdown` cancels and joins it.
pub struct UninstallService {
    shared: Arc<Shared>,
    worker: Mutex<Option<tokio::task::JoinHandle<()>>>,
    runtime: tokio::runtime::Handle,
}
impl UninstallService {
    pub fn new(
        store: Arc<InstalledStore>,
        builtins: Arc<BuiltinProviders>,
        runtime: Arc<dyn RemovalRuntime>,
        handle: tokio::runtime::Handle,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                store,
                builtins,
                runtime,
                cancel: CancellationToken::new(),
                faults: Mutex::new(Faults::default()),
            }),
            worker: Mutex::new(None),
            runtime: handle,
        }
    }
    /// Advisory read-only review of the closure and refusals: no gate, no
    /// recovery and no job. `start` recomputes it under the gate.
    pub async fn preview(&self, request: &UninstallRequest) -> Result<RemovalPlan, UninstallError> {
        let shared = Arc::clone(&self.shared);
        let request = request.clone();
        tokio::task::spawn_blocking(move || compute(&shared, &request))
            .await
            .map_err(|_| UninstallError::Worker)?
    }
    /// Admit one uninstall under the shared gate, recover any interrupted
    /// journal, rebuild the graph and plan the closure. Refusals (unknown,
    /// still-required, unresolvable graph, busy) return here with no job and
    /// no change; otherwise a catalog polling job runs the removal.
    pub async fn start(
        &self,
        registry: &PlanRegistry,
        request: UninstallRequest,
    ) -> Result<ConfirmResponse, UninstallError> {
        if self.shared.cancel.is_cancelled() {
            return Err(UninstallError::Shutdown);
        }
        let operation = registry.begin_operation()?;
        let shared = Arc::clone(&self.shared);
        let planned = request.clone();
        // The gate moves into the blocking task, so it stays held until
        // recovery/planning finish even if this future is dropped.
        let (operation, plan) = tokio::task::spawn_blocking(move || {
            let plan = recover(shared.store.layout())
                .map_err(UninstallError::from)
                .and_then(|_| compute(&shared, &planned));
            (operation, plan)
        })
        .await
        .map_err(|_| UninstallError::Worker)?;
        let plan = plan?;
        let response = operation.track_job("stopping")?;
        let shared = Arc::clone(&self.shared);
        *self.worker.lock().unwrap_or_else(PoisonError::into_inner) =
            Some(self.runtime.spawn(run(shared, operation, request, plan)));
        Ok(response)
    }
    /// Explicit request for an installed dependency: promote it to requested
    /// (records only, no download). `Ok(false)` when already requested.
    pub async fn promote(
        &self,
        registry: &PlanRegistry,
        kind: InstalledKind,
        id: &str,
        source: &SourceIdentity,
    ) -> Result<bool, UninstallError> {
        let operation = registry.begin_operation()?;
        let shared = Arc::clone(&self.shared);
        let (id, source) = (id.to_owned(), source.clone());
        tokio::task::spawn_blocking(move || {
            let layout = shared.store.layout();
            let result = recover(layout)
                .map_err(UninstallError::from)
                .and_then(|_| origin::promote(layout, &shared.store.snapshot(), kind, &id, &source))
                .map(|promoted| match promoted {
                    Some(document) => {
                        shared.store.published(document);
                        true
                    }
                    None => false,
                });
            operation.finish(result.as_ref().map(|_| ()).map_err(ToString::to_string));
            result
        })
        .await
        .map_err(|_| UninstallError::Worker)?
    }
    /// Await the current removal without cancelling it.
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
    /// Cancel (honoured before runtime activity stops) and join the worker; a
    /// commit already running finishes or rolls back first.
    pub async fn shutdown(&self) {
        self.shared.cancel.cancel();
        self.wait_idle().await;
    }
    #[cfg(test)]
    pub(super) fn inject(&self, faults: Faults) {
        *self.shared.faults.lock().unwrap() = faults;
    }
}
impl Drop for UninstallService {
    fn drop(&mut self) {
        self.shared.cancel.cancel();
    }
}

fn compute(shared: &Shared, request: &UninstallRequest) -> Result<RemovalPlan, UninstallError> {
    let graph = InstalledGraph::read(shared.store.layout(), &shared.store.snapshot())?;
    plan_removal(
        &graph,
        &shared.builtins,
        request.kind,
        &request.id,
        &request.source,
    )
}

async fn run(
    shared: Arc<Shared>,
    operation: CatalogOperation,
    request: UninstallRequest,
    plan: RemovalPlan,
) {
    let result = execute(&shared, &operation, &request, &plan).await;
    operation.finish(result.map_err(|error| error.to_string()));
}

async fn execute(
    shared: &Arc<Shared>,
    operation: &CatalogOperation,
    request: &UninstallRequest,
    plan: &RemovalPlan,
) -> Result<(), UninstallError> {
    if shared.cancel.is_cancelled() {
        return Err(UninstallError::Cancelled);
    }
    shared
        .runtime
        .stop(plan)
        .await
        .map_err(UninstallError::Stop)?;
    operation.progress("removing", &request.id);
    let store = Arc::clone(&shared.store);
    let removals: Vec<_> = plan
        .removals
        .iter()
        .map(|removal| (removal.kind, removal.id.clone()))
        .collect();
    let faults = *shared.faults.lock().unwrap_or_else(PoisonError::into_inner);
    let result = tokio::task::spawn_blocking(move || {
        let document = commit_removal(store.layout(), &store.snapshot(), &removals, faults)?;
        store.published(document);
        Ok::<_, UninstallError>(())
    })
    .await
    .map_err(|_| UninstallError::Worker)?;
    if let Err(error) = result {
        if !matches!(
            error,
            UninstallError::Install(crate::catalog::install::InstallError::RecoveryRequired)
        ) {
            shared.runtime.rolled_back().await;
        }
        return Err(error);
    }
    shared.runtime.removed(plan).await;
    if request.remove_data {
        operation.progress("removing data", &request.id);
        shared
            .runtime
            .remove_data(request.kind, &request.id)
            .await
            .map_err(UninstallError::Data)?;
    }
    Ok(())
}
