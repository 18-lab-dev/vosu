//! Compile-time embedded, source-pinned reader payloads.
//!
//! `DaemonAssets` embeds the pinned `assets/offsets.json` template (also
//! embedded, separately, by the root crate's own `assets::DaemonAssets` to
//! build the `LazerOffsetLoader` at its instance construction site) and,
//! via `build.rs`, the RID-selected embedded lazer offset generator bundle
//! the offset generator in this crate installs.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../assets/"]
#[include = "offsets.json"]
pub struct DaemonAssets;

include!(concat!(env!("OUT_DIR"), "/daemon_generator_bundle.rs"));
