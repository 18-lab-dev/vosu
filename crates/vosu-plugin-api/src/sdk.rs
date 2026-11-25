//! The Rust SDK (`docs/v2/plugin-abi.md` §9): `Plugin`/`ConcurrentPlugin`
//! and the external `cdylib` form of `export_plugin!`. Synchronous reads
//! (D5) and bundled registration live in the unpublished
//! `vosu-plugin-internal` crate (AF.9), never here.

mod glue;
mod plugin;

pub use plugin::{ConcurrentPlugin, Init, Plugin, PluginError, VtableHostServices};

#[doc(hidden)]
pub use glue::vtable_for as __vtable_for_serialized;
#[doc(hidden)]
pub use glue::vtable_for_concurrent as __vtable_for_concurrent;
