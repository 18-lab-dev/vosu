#![forbid(unsafe_op_in_unsafe_fn)]
#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "unit-test fixtures write scratch files, sleep to sequence threads, and use unbounded test channels"
    )
)]

//! Native boundary: platform filesystem/process integration, the target
//! process memory boundary, and process-exit-event backends. All platform
//! `unsafe` lives here.

#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
pub mod events;
pub mod platform;
pub mod process;
