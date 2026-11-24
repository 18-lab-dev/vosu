#![forbid(unsafe_op_in_unsafe_fn)]
#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "unit-test fixtures write scratch files, sleep to sequence threads, and use unbounded test channels"
    )
)]

pub mod api;
pub mod assets;
pub mod counters;
pub mod model;
pub mod plugins;
pub mod runtime;
pub mod server;
pub mod tools;
pub mod vosux;
