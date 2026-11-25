//! Support for `benches/brokered_reads_bench.rs`; see its module doc.

pub mod candidate;
pub mod os;
#[cfg(windows)]
pub mod pipe_windows;
pub mod plans;
#[cfg(target_os = "linux")]
pub mod prototype_linux;
pub mod ring;
pub mod stats;
pub mod target;
pub mod wire;
