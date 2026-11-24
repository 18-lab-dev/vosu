//! Per-instance raw capture workers and serialized transition reducer.

#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "unit-test fixtures write scratch files, sleep to sequence threads, and use unbounded test channels"
    )
)]

#[cfg(all(feature = "test-harness", not(debug_assertions)))]
compile_error!("the test-harness feature is forbidden in release builds");

mod bundle;
mod context;
mod reader_settings;
mod readers;
pub mod state_backend;
#[cfg(feature = "test-harness")]
pub mod test_fixtures;
#[cfg(windows)]
mod windows_timer;
mod worker;

pub use bundle::*;
pub use context::*;
pub use reader_settings::ReaderSettings;
pub use readers::*;
pub use worker::*;

#[cfg(test)]
mod test_logging;
#[cfg(test)]
mod tests;
