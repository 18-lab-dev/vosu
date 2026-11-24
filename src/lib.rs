#![forbid(unsafe_op_in_unsafe_fn)]
#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "unit-test fixtures write scratch files, sleep to sequence threads, and use unbounded test channels"
    )
)]

#[cfg(test)]
extern crate self as vosu;

pub mod assets;
pub mod counters;
pub mod plugins;
pub mod runtime;
pub mod server;
pub mod tools;
pub mod vosux;
