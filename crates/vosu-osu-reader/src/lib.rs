#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "unit-test fixtures write scratch files, sleep to sequence threads, and use unbounded test channels"
    )
)]

//! Client decoding: stable/lazer memory readers and the osu!-specific
//! discovery inputs they and the generic process discoverer both need.
//!
//! `process` holds the pointer-width managed-heap read layer (`ProcessHandle`,
//! AOB scanning) built over `vosu_sys::process`'s object-safe syscall
//! boundary; `discovery` holds the client argv parser (`StartupArgs`) and
//! Wine data-path resolution (`resolve_osu_path`), both read by the generic
//! process discoverer that still lives in the root crate until M1 hands the
//! osu! classification and resolver lifecycle to the bundled plugin.
//! `offset_generator` holds the embedded self-contained lazer offset
//! generator (installed and run through `memory::lazer_loader`'s
//! `OffsetGenerator` seam); its platform-selected bundle is embedded by this
//! crate's own `build.rs` into `assets::DaemonAssets`.

pub mod assets;
pub mod discovery;
pub mod memory;
pub mod offset_generator;
pub mod process;
