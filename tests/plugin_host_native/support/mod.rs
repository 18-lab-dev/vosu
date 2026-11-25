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

#[path = "artifacts.rs"]
pub mod artifacts;
#[path = "fixture.rs"]
pub mod fixture;
#[path = "host.rs"]
pub mod host;
#[path = "smoke.rs"]
mod smoke;
