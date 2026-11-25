//! Pure dependency-closure removal planning over an `InstalledGraph`.
//!
//! 1. The explicit target must be an installed record from that exact source.
//! 2. A6b.7: refuse when a kept consumer's resolved **required** edge selects
//!    the target and no other kept installed package satisfies it, naming
//!    every such dependent. Optional edges never block; they are dropped.
//! 3. Recompute the kept graph's edges and remove every dependency-only
//!    plugin with no incoming edge (required or resolved optional), then
//!    recompute again until nothing changes. Requested packages, tools and
//!    overlays are never autoremoved; built-ins are never graph nodes.
use super::{
    UninstallError,
    graph::{GraphEdge, InstalledGraph},
};
use crate::catalog::{
    install::{InstalledFor, InstalledKind},
    resolver::BuiltinProviders,
    sources::SourceIdentity,
};
use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RemovalReason {
    /// The package the user asked to uninstall.
    Explicit,
    /// A dependency-only plugin no kept package references any more.
    Unreferenced,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Removal {
    pub kind: InstalledKind,
    pub id: String,
    pub version: String,
    pub source: SourceIdentity,
    pub reason: RemovalReason,
}

/// A kept package that requires the target with no other kept provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Dependent {
    pub kind: InstalledKind,
    pub id: String,
    pub capability: String,
    pub range: String,
}

/// A resolved optional grant a kept consumer loses with this removal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DroppedOptional {
    pub consumer: String,
    pub provider: String,
    pub capability: String,
}

/// Exact removal closure: `removals` is consumers-first (the explicit target,
/// then each round of newly unreferenced dependencies).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RemovalPlan {
    pub removals: Vec<Removal>,
    pub dropped_optional: Vec<DroppedOptional>,
}

pub fn plan_removal(
    graph: &InstalledGraph,
    builtins: &BuiltinProviders,
    kind: InstalledKind,
    id: &str,
    source: &SourceIdentity,
) -> Result<RemovalPlan, UninstallError> {
    let nodes = graph.nodes();
    let target = graph
        .index(kind, id)
        .filter(|&i| nodes[i].record.source == *source)
        .ok_or_else(|| UninstallError::NotInstalled(id.to_owned()))?;
    let full = graph.edges(builtins, &vec![true; nodes.len()])?;
    let mut kept = vec![true; nodes.len()];
    kept[target] = false;
    let mut dependents: Vec<Dependent> = Vec::new();
    for edge in full
        .iter()
        .filter(|e| !e.optional && e.provider == target && kept[e.consumer])
    {
        if graph.kept_provider(&edge.requirement, &kept) {
            continue;
        }
        let consumer = &nodes[edge.consumer].record;
        let dependent = Dependent {
            kind: consumer.kind,
            id: consumer.id.clone(),
            capability: edge.requirement.id.clone(),
            range: edge.requirement.raw_version.clone(),
        };
        if !dependents.contains(&dependent) {
            dependents.push(dependent);
        }
    }
    if !dependents.is_empty() {
        return Err(UninstallError::Required {
            id: id.to_owned(),
            dependents,
        });
    }
    let removal = |i: usize, reason| {
        let record = &nodes[i].record;
        Removal {
            kind: record.kind,
            id: record.id.clone(),
            version: record.version.clone(),
            source: record.source.clone(),
            reason,
        }
    };
    let mut removals = vec![removal(target, RemovalReason::Explicit)];
    loop {
        // Recomputed every round: a consumer whose provider left may now
        // select another kept provider, which then is referenced again.
        let edges = graph.edges(builtins, &kept)?;
        let orphans: Vec<usize> = (0..nodes.len())
            .filter(|&i| {
                kept[i]
                    && nodes[i].record.kind == InstalledKind::Plugin
                    && nodes[i].record.installed_for == InstalledFor::Dependency
                    && !edges.iter().any(|e| e.provider == i)
            })
            .collect();
        if orphans.is_empty() {
            break;
        }
        for i in orphans {
            kept[i] = false;
            removals.push(removal(i, RemovalReason::Unreferenced));
        }
    }
    let dropped_optional = full
        .iter()
        .filter(|e: &&GraphEdge| e.optional && !kept[e.provider] && kept[e.consumer])
        .map(|e| DroppedOptional {
            consumer: nodes[e.consumer].record.id.clone(),
            provider: nodes[e.provider].record.id.clone(),
            capability: e.requirement.id.clone(),
        })
        .collect();
    Ok(RemovalPlan {
        removals,
        dropped_optional,
    })
}
