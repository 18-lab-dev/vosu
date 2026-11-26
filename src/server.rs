//! The bundled composition's HTTP providers that still name bundled crates:
//! the adapter-backed state providers and capture endpoint, overlay folder
//! opening and the production route delegate. The generic transport lives in
//! `vosu_core::server`.

#[cfg(feature = "test-harness")]
pub mod adapter_capture;
pub mod bus_provider;
pub mod overlay_folders;
pub mod service;
