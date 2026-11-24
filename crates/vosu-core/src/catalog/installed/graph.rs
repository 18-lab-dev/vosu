//! The installed provider graph, rebuilt on demand from the current validated
//! package manifests and R0's own selected-provider rule. Nothing here is
//! persisted: a reference is a resolved edge, never a stored counter.
use super::UninstallError;
use crate::catalog::{
    install::{InstallLayout, InstalledDocument, InstalledKind, InstalledRecord, read_manifest},
    resolver::{self, BuiltinProviders, Installed, Node, Snapshot},
};
use vosu_plugin_api::manifest::{PackageManifest, Requirement};

/// One committed package and its current manifest.
#[derive(Debug, Clone)]
pub struct InstalledNode {
    pub record: InstalledRecord,
    pub manifest: PackageManifest,
}

/// A resolved requirement between two installed packages (indices into
/// `InstalledGraph::nodes`). Requirements satisfied by built-ins, and unmet
/// optional requirements, are not edges and never count as references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GraphEdge {
    pub consumer: usize,
    pub provider: usize,
    pub requirement: Requirement,
    pub optional: bool,
}

#[derive(Debug, Clone)]
pub struct InstalledGraph {
    nodes: Vec<InstalledNode>,
}
impl InstalledGraph {
    /// Read every record's manifest from its committed directory. A missing or
    /// mismatched manifest refuses the graph rather than guessing its edges.
    pub fn read(
        layout: &InstallLayout,
        document: &InstalledDocument,
    ) -> Result<Self, UninstallError> {
        let nodes = document
            .records
            .iter()
            .map(|record| {
                Ok(InstalledNode {
                    manifest: read_manifest(layout, record)?,
                    record: record.clone(),
                })
            })
            .collect::<Result<_, UninstallError>>()?;
        Ok(Self { nodes })
    }
    pub fn nodes(&self) -> &[InstalledNode] {
        &self.nodes
    }
    pub fn index(&self, kind: InstalledKind, id: &str) -> Option<usize> {
        self.nodes
            .iter()
            .position(|node| node.record.kind == kind && node.record.id == id)
    }
    /// Resolved edges among the `kept` nodes, exactly as R0 selects them for
    /// kept installed consumers: built-in first, then the first kept installed
    /// provider in record order, optional edges only when such a provider
    /// exists. A kept graph R0 cannot resolve (missing provider, cycle, bound)
    /// is an error, so no removal can commit a broken graph.
    pub fn edges(
        &self,
        builtins: &BuiltinProviders,
        kept: &[bool],
    ) -> Result<Vec<GraphEdge>, UninstallError> {
        let indices: Vec<usize> = (0..self.nodes.len()).filter(|&i| kept[i]).collect();
        let installed: Vec<_> = indices
            .iter()
            .map(|&i| Installed {
                manifest: &self.nodes[i].manifest,
            })
            .collect();
        // No catalog candidates participate, so the host version, target and
        // package limit (candidate eligibility only) are never consulted.
        let application = semver::Version::new(0, 0, 0);
        let resolution = resolver::resolve(
            &Snapshot {
                candidates: &[],
                installed: &installed,
                builtins,
                application: &application,
                target: "",
                max_package_bytes: 0,
            },
            &[],
        );
        if let Some(failure) = resolution.errors.first() {
            return Err(UninstallError::Graph(failure.to_string()));
        }
        resolution
            .edges
            .into_iter()
            .filter_map(|edge| {
                let provider = edge.provider?;
                Some(match (edge.consumer, provider) {
                    (Node::Installed(consumer), Node::Installed(provider)) => Ok(GraphEdge {
                        consumer: indices[consumer],
                        provider: indices[provider],
                        requirement: edge.requirement,
                        optional: edge.optional,
                    }),
                    _ => Err(UninstallError::Graph(
                        "catalog node in installed graph".into(),
                    )),
                })
            })
            .collect()
    }
    /// Whether a kept installed node still satisfies `requirement`.
    pub(super) fn kept_provider(&self, requirement: &Requirement, kept: &[bool]) -> bool {
        self.nodes.iter().enumerate().any(|(i, node)| {
            kept[i]
                && node
                    .manifest
                    .fields()
                    .provides
                    .iter()
                    .any(|p| requirement.satisfied_by(&p.id, &p.version))
        })
    }
}
