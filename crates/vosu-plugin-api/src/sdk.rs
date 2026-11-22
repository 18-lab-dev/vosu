//! The Rust SDK (`docs/v2/plugin-abi.md` §9): `Plugin`/`ConcurrentPlugin`,
//! `export_plugin!`, and (behind the unpublished `unstable-sync-reads`
//! feature, D5) `PlanMemory`.

mod glue;
#[cfg(feature = "unstable-sync-reads")]
pub(crate) mod plan_memory;
mod plugin;

#[cfg(feature = "unstable-sync-reads")]
pub use plan_memory::{PlanMemory, PlanMemoryError};
pub use plugin::{ConcurrentPlugin, Init, Plugin, PluginError, VtableHostServices};

#[doc(hidden)]
pub use glue::vtable_for as __vtable_for_serialized;
#[doc(hidden)]
pub use glue::vtable_for_concurrent as __vtable_for_concurrent;
