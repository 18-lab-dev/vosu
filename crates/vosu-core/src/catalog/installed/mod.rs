//! M6b U0: installed provider graph, requested promotion and dependency-only
//! autoremove (catalog-v2 "Dependency-only autoremove", ADR 0022, A6b.5/.7).
//!
//! I0's `InstalledStore` records are the only installed model; they persist
//! provenance, approval and `installed_for`, never a reference count. Every
//! uninstall rebuilds the graph from the committed packages' current
//! manifests and R0's selected-provider rule (`graph`), plans the removal
//! closure purely (`remove`) and commits it through I0's journal under the
//! shared catalog operation gate (`service`). `origin` promotes an explicitly
//! requested installed dependency.
//!
//! Ordering of one uninstall: gate → install recovery → graph + plan (refusals
//! return synchronously, no job) → catalog job → stop runtime activity of the
//! closure → journaled directory moves + records commit → post-commit runtime
//! notification → optional data removal for the explicit target only. No lock
//! is held across awaits, filesystem work or runtime callbacks.
mod graph;
mod origin;
mod remove;
mod service;
#[cfg(test)]
mod tests;

pub use graph::{GraphEdge, InstalledGraph, InstalledNode};
pub use remove::{Dependent, DroppedOptional, Removal, RemovalPlan, RemovalReason, plan_removal};
pub use service::{RemovalRuntime, UninstallRequest, UninstallService};

use crate::catalog::{install::InstallError, plans::PlanError};

/// Redacted local diagnostics: ids, kinds and capability ranges only.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum UninstallError {
    #[error("{0} is not installed from that source")]
    NotInstalled(String),
    #[error("{id} is still required by {}", describe(dependents))]
    Required {
        id: String,
        dependents: Vec<Dependent>,
    },
    #[error("installed graph cannot be resolved: {0}")]
    Graph(String),
    #[error("runtime activity could not be stopped: {0}")]
    Stop(String),
    #[error("package removed, but its data could not be removed: {0}")]
    Data(String),
    #[error(transparent)]
    Plan(#[from] PlanError),
    #[error(transparent)]
    Install(#[from] InstallError),
    #[error("uninstall cancelled")]
    Cancelled,
    #[error("uninstall service is shut down")]
    Shutdown,
    #[error("uninstall worker failed")]
    Worker,
}
impl UninstallError {
    /// Route mapping for H0: unknown 404; refused/inconsistent graph and
    /// busy/stale 409; local failures 500.
    pub fn status_code(&self) -> u16 {
        match self {
            Self::NotInstalled(_) => 404,
            Self::Required { .. } | Self::Graph(_) => 409,
            Self::Plan(error) => error.status_code(),
            Self::Install(InstallError::InstalledManifest(_)) => 409,
            _ => 500,
        }
    }
}

fn describe(dependents: &[Dependent]) -> String {
    dependents
        .iter()
        .map(|d| format!("{} ({} {})", d.id, d.capability, d.range))
        .collect::<Vec<_>>()
        .join(", ")
}
