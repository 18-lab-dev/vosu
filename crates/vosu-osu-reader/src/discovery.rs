//! osu!-specific discovery inputs for the generic process discoverer: the
//! client's own startup argv grammar and, on Linux, Wine data-path
//! resolution. Both stay here (not in `vosu_sys`) because they decode
//! osu!-specific conventions rather than OS process primitives.

mod startup_args;
pub use startup_args::{StartupArgs, StartupValue};

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::resolve_osu_path;
