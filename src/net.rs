//! Shared hardened outbound networking primitives.

pub mod streaming;

pub use streaming::{
    AddressResolver, Deadlines, DestinationPolicy, DownloadLimits, NetworkError, NetworkResponse,
    ResolverFuture, SleepFuture, StreamingClient, SystemResolver, TimeSource, TokioTime, ZipLimits,
    extract_zip_atomic, is_global_unicast,
};

pub use streaming::AddressResolver as Resolver;

#[cfg(feature = "test-harness")]
pub use streaming::{TestNetworkConfig, TestNetworkEndpoints};
