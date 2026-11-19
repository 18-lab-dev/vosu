#![forbid(unsafe_op_in_unsafe_fn)]
#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "unit-test fixtures write scratch files, sleep to sequence threads, and use unbounded test channels"
    )
)]

pub mod api;
pub mod app;
pub mod assets;
pub mod calculation;
pub mod config;
pub mod counters;
pub mod desktop;
pub mod instance;
pub mod logging;
pub mod memory;
pub mod model;
pub mod net;
pub mod platform;
pub mod process;
pub mod report;
pub mod runtime;
pub mod server;
pub mod state;
pub mod tools;
pub mod updater;
pub mod vosux;

#[cfg(test)]
mod proptest_support;
