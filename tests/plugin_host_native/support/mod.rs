//! M5 P0 fixture support for the native plugin-host target
//! (`docs/testing/verification.md`, "M5 test and fixture conventions").
//!
//! - [`artifacts`] finds the two prepared `cdylib` fixtures through
//!   `VOSU_PLUGIN_HOST_TESTKIT_LIBRARY`/`VOSU_PLUGIN_HOST_OSU_LIBRARY`
//!   (`scripts/plugin-host-fixtures.py` builds and stages them; this target
//!   never runs Cargo) and stages a fresh package per test with a
//!   `RECEIPT` line.
//! - [`fixture`] owns one run's control file, nonce, receipts and barriers.
//! - [`host`] drives a real `PluginHost` whose launcher starts the real
//!   `vosu` executable, and identifies child processes without trusting PID
//!   absence.
//!
//! `smoke` proves each testkit fault mode actually triggers under the
//! production launcher. The crash, stall and loss end-to-end assertions
//! belong to the X0/C0 modules.
//!
//! M5 N0's core-death trees: [`tree`] (a core of either kind, its plugin
//! host, descendant and outside control), [`daemon`] (the real daemon as
//! core), [`tracked`] (pidfd and handle identities) and, on Windows,
//! [`jobs`] (job, token and handle-table readbacks).

#[path = "artifacts.rs"]
pub mod artifacts;
#[path = "daemon.rs"]
pub mod daemon;
#[path = "fixture.rs"]
pub mod fixture;
#[path = "host.rs"]
pub mod host;
#[cfg(windows)]
#[path = "jobs.rs"]
pub mod jobs;
#[cfg(target_os = "linux")]
#[path = "refusal.rs"]
pub mod refusal;
#[path = "smoke.rs"]
mod smoke;
#[path = "tracked.rs"]
pub mod tracked;
#[path = "tree.rs"]
pub mod tree;

/// A fresh per-test scratch directory for staged packages, receipts and
/// instance data. `VOSU_PLUGIN_HOST_SCRATCH_DIR` places it under a chosen
/// root: the Windows VM gates point it at a directory Defender excludes,
/// because an on-access scan of each freshly staged library (measured
/// 1.6–3 s for 194 MB, 0.2 s for 0.8 MB, against about 50 ms excluded)
/// otherwise lands inside the 5-second startup deadline under parallel load.
/// Unset, it is the system temporary directory.
pub fn scratch() -> tempfile::TempDir {
    match std::env::var_os("VOSU_PLUGIN_HOST_SCRATCH_DIR") {
        Some(root) => {
            std::fs::create_dir_all(&root).unwrap();
            tempfile::tempdir_in(root).unwrap()
        }
        None => tempfile::tempdir().unwrap(),
    }
}
