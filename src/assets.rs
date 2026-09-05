//! Compile-time embedded, source-pinned daemon assets.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/server/"]
pub struct ServerAssets;

/// Only the updater offsets are embedded in the daemon; server assets remain
/// exclusively owned by `ServerAssets`.
#[derive(RustEmbed)]
#[folder = "assets/"]
#[include = "offsets.json"]
pub struct DaemonAssets;
