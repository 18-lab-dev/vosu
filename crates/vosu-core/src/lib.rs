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

pub mod app;
pub mod config;
pub mod desktop;
pub mod logging;
pub mod report;
pub mod updater;
