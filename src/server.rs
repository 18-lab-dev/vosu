//! The bundled composition's HTTP pieces that still name bundled crates:
//! overlay folder opening and the production route delegate. The generic
//! transport, including the bus-backed schema provider and the test-harness
//! capture endpoint, lives in `vosu_core::server`.

pub mod overlay_folders;
pub mod service;
