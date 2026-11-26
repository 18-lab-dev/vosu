//! Game-agnostic HTTP and WebSocket mechanics (M7 C0): both listeners and
//! their reloadable bind, the exact route classification and envelopes,
//! peer/Origin/Host policy, API keys and the native broker, file
//! confinement, the latest-only WebSocket transport and its bus-backed
//! schema provider, API v1 and the catalog routes. Bundled providers own
//! their data; the root composes them.

#[cfg(feature = "test-harness")]
pub mod adapter_capture;
pub mod api_keys;
pub mod api_v1;
pub mod auth;
pub mod broker;
pub mod bus_provider;
pub mod catalog_api;
pub mod files;
pub mod ids;
pub mod origins;
pub mod overlay_folders;
pub mod pages;
pub mod routes;
pub mod runtime;
pub mod service;
pub mod subscription_wire;
pub mod websocket;
