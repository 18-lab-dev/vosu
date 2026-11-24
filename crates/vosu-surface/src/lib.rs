//! `vosu-surface`: the core-owned, Rust-only API bundled surface plugins
//! use to register routes, WebSocket feeds, and state reads (D3,
//! plan:94). This crate is permanent.
//!
//! The `bridge_b1` module is the one interim part: the bundled osu!
//! plugin's bridge into the legacy gosuMemory/streamCompanion adapters
//! (crate edge `vosu-osu -> vosu-surface`) is bridge B1 (plan:447-452,
//! abi:294 §7.5). It exists only because the ABI's `create(host, init)`
//! takes no Rust-level parameter through which a per-instance hook could
//! otherwise be threaded, so its slot is process-wide rather than
//! per-plugin-instance. Only `bridge_b1` and the `vosu-osu -> vosu-surface`
//! edge are deleted at M4a, when the legacy adapters become ordinary bus
//! projections (docs/v2/implementation-plan.md:261,647); this crate
//! continues on as the permanent D3 surface API.

mod bridge_b1;
pub mod files;
pub mod listener;
pub mod management;
pub mod requester;
pub mod resources;
pub mod routes;
pub mod state;

pub use bridge_b1::{AlreadyInstalled, Installed, LegacySink, current, install};
