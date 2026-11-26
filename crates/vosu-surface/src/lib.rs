//! `vosu-surface`: the core-owned, Rust-only API bundled surface plugins
//! use to register routes, WebSocket feeds, and state reads (D3,
//! plan:94). This crate is permanent.
pub mod files;
pub mod filter;
pub mod listener;
pub mod management;
pub mod projection;
pub mod requester;
pub mod resources;
pub mod routes;
pub mod state;
pub mod tool_view;
