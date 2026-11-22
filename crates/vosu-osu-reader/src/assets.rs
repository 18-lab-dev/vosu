//! Compile-time embedded, source-pinned reader payloads.
//!
//! Duplicates the root crate's `assets::DaemonAssets` embed of the same
//! pinned `assets/offsets.json` (the root crate also needs it directly for
//! the lazer offset generator and release report, which stay there for
//! now); both crates embed the identical file by its own relative
//! `rust-embed` folder rather than sharing one Rust type across the crate
//! boundary.

use rust_embed::RustEmbed;

#[derive(RustEmbed)]
#[folder = "../../assets/"]
#[include = "offsets.json"]
pub struct DaemonAssets;
