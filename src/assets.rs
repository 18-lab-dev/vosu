//! Compile-time embedded, source-pinned daemon assets.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/server/"]
pub struct ServerAssets;

/// Compile-time daemon payloads. The generator bundle is selected by Cargo's
/// target triple and is never fetched or discovered at runtime.
#[derive(RustEmbed)]
#[folder = "assets/"]
#[include = "offsets.json"]
pub struct DaemonAssets;

include!(concat!(env!("OUT_DIR"), "/daemon_generator_bundle.rs"));
