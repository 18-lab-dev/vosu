//! Immutable catalog plans, bounded lazy-expiry registry and single-use confirmation.
//!
//! M6b I0 implements `CatalogExecutor`: `start` only dispatches an owned operation
//! to its bounded worker; it must never install on the caller thread. The worker
//! owns cancellation, joins on shutdown, prepares all pinned packages, journals
//! all-or-rollback publication and calls `finish` once. Dropping an unfinished
//! operation fails its catalog job and releases the gate (also on dispatch error).
//! Source/config/installed mutations must use `PlanRegistry::begin_operation`
//! and retain its guard through publication; fingerprint reads at confirm are
//! synchronous snapshots under this same gate, never network/filesystem work.
//! No registry lock is held during executor callbacks, downloads or installation.
//! Catalog polling is deliberately separate from API-v1 action jobs.
mod build;
mod confirm;
mod registry;
#[cfg(test)]
mod tests;

pub use build::{PinnedAdd, PinnedPlan, PlanAdd, PlanView, Prerequisites, Reason, build};
pub use confirm::{CatalogExecutor, CatalogOperation, ConfirmedPlan};
pub use registry::{
    CatalogJob, ConfirmResponse, MAX_PENDING_PLANS, PLAN_TTL, PlanError, PlanRegistry,
};
