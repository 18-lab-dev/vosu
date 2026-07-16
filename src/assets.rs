//! Compile-time embedded, source-pinned daemon assets.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "assets/server/"]
pub struct ServerAssets;

#[derive(RustEmbed)]
#[folder = "assets/"]
pub struct DaemonAssets;
