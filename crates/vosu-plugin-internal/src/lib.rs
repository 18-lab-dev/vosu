//! Workspace-only plugin glue (AF.9, implementation plan D11): never
//! published, so nothing here reaches crates.io in any feature-gated form.
//!
//! - [`PlanMemory`]: synchronous read plans (D5) on a plugin's own reader
//!   threads, with its zero-allocation [`PlanMemory::read_flat`] path (D17).
//! - The dispatch glue that routes a synchronous `VOSU_OP_READ_PLAN_RESULT`
//!   to `PlanMemory` before the plugin sees it, and [`export_plugin!`]'s
//!   bundled-registration form for the root crate's `BUNDLED` array (D13).
//! - [`read_plan_profile`]: opt-in per-thread read-plan phase diagnostics
//!   shared by `PlanMemory` and the host.
//!
//! Plugin code itself is identical either way: it implements the published
//! SDK's `Plugin`/`ConcurrentPlugin` and only its registration differs.
#![forbid(unsafe_op_in_unsafe_fn)]

mod glue;
mod plan_memory;
pub mod read_plan_profile;

pub use glue::{vtable_for, vtable_for_concurrent};
pub use plan_memory::{FlatReadResults, PlanMemory, PlanMemoryError, SyncReadBuffers};

/// The published SDK, for [`export_plugin!`]'s expansion in crates that
/// name it under another path.
#[doc(hidden)]
pub use vosu_plugin_api as __api;
