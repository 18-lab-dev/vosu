//! Shared hardened outbound networking primitives.

#![forbid(unsafe_op_in_unsafe_fn)]
#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "unit-test fixtures write scratch files, sleep to sequence threads, and use unbounded test channels"
    )
)]

#[cfg(all(feature = "test-harness", not(debug_assertions)))]
compile_error!("the test-harness feature is forbidden in release builds");

pub mod streaming;

pub use streaming::{
    AddressResolver, Deadlines, DestinationPolicy, DownloadLimits, NetworkError, NetworkResponse,
    ResolverFuture, SleepFuture, StreamingClient, SystemResolver, TimeSource, TokioTime, ZipLimits,
    extract_zip_atomic, is_global_unicast, normalize_ip,
};

pub use streaming::AddressResolver as Resolver;

#[cfg(feature = "test-harness")]
pub use streaming::{TestNetworkConfig, TestNetworkEndpoints};
