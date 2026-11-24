//! `vosu-surface`: interim bridge **B1**'s process-global hook slot
//! (plan:447-452, abi:294 §7.5), deleted in M4a.
//!
//! The bundled osu! plugin's bridge into the legacy gosuMemory/streamCompanion
//! adapters (crate edge `vosu-osu -> vosu-surface`) is bridge B1. It exists
//! only because the ABI's `create(host, init)` takes no Rust-level
//! parameter through which a per-instance hook could otherwise be threaded,
//! so the slot below is process-wide rather than per-plugin-instance. B1 is
//! deleted at M4a, when the legacy adapters become ordinary bus projections
//! (docs/v2/implementation-plan.md:261,647) and this crate is removed along
//! with it.

mod bridge_b1;

pub use bridge_b1::{AlreadyInstalled, Installed, LegacySink, current, install};
