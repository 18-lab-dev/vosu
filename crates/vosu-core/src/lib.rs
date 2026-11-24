#![forbid(unsafe_op_in_unsafe_fn)]
#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "unit-test fixtures write scratch files, sleep to sequence threads, and use unbounded test channels"
    )
)]

//! Config, logging, diagnostics report, self-update, desktop integration, and
//! startup decisions: the daemon-wide operational services shared by every
//! runtime composition.

#[cfg(all(feature = "test-harness", not(debug_assertions)))]
compile_error!("the test-harness feature is forbidden in release builds");

pub mod app;
pub mod config;
pub mod desktop;
pub mod logging;
pub mod plugin;
pub mod report;
pub mod state;
pub mod updater;
