//! Immutable publication and encoding-cache boundary for all public state APIs.
//!
//! Wire builders receive one [`CombinedSnapshot`]. Focus, lifecycle metadata,
//! all instance states, and the focused regular/precise components therefore
//! always come from one atomic publication point.

mod fixed;
mod json;
#[cfg(test)]
pub(crate) use json::JsFormatter;
pub mod precise;
pub mod sc;
pub mod v1;
pub mod v2;

mod snapshot;

pub use snapshot::{
    ApiError, ApiRuntimeSettings, ApiSchema, CombinedSnapshot, ManagerInstanceSnapshot,
    ManagerSnapshot, SchemaDependency, SnapshotManager,
};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod wire_fixtures;
