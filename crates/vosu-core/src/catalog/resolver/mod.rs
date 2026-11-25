//! Pure deterministic whole-graph resolution over immutable SDK-validated snapshots.
//! Limits are centralized: 4096 candidates, 256 graph nodes, 256 required
//! edges, and 100000 work units (candidate probes and graph traversal).
mod builtins;
mod candidates;
mod diagnostics;
mod optional;
mod solve;
#[cfg(test)]
mod tests;

pub use builtins::BuiltinProviders;
pub use candidates::{Candidate, Installed, Request, Snapshot};
pub use diagnostics::{Failure, RequirementSite};
pub use solve::{Edge, Node, Resolution, resolve};

pub const MAX_CANDIDATES: usize = 4096;
pub const MAX_NODES: usize = 256;
pub const MAX_SEARCH_WORK: usize = 100_000;
pub const MAX_REQUIREMENTS: usize = 256;
