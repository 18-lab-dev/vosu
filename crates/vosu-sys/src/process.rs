//! Native target-process boundary: OS handle management and syscalls that
//! implement `vosu_plugin_api::memory::ProcessMemory`, process enumeration and
//! metadata, module tables, and the signature-scan region walk.
//!
//! The read vocabulary itself (requests, errors, statuses, bitness, the
//! object-safe trait and its ordered defaults, signature patterns) is
//! platform-neutral and lives in `vosu_plugin_api::memory`. Decoding stays
//! above that object-safe boundary so generic collection callbacks remain
//! monomorphized.

mod clock;
pub use clock::coarse_monotonic_millis;
mod launch_context;
pub use launch_context::{LaunchContext, launch_context};
pub mod scanner;

#[cfg(feature = "test-harness")]
mod liveness;
#[cfg(feature = "test-harness")]
pub use liveness::ProcessLiveness;

#[cfg(windows)]
pub mod windows;
#[cfg(windows)]
pub use windows::{
    ModuleEntry, OwnedHandle, ProcessEntry, ThreadEntry, TimerResolutionGuard,
    WindowsProcessMemory, enumerate_modules, enumerate_processes, enumerate_threads,
    foreground_process_id, is_process_alive, process_bitness, process_command_line,
    process_image_path,
};
#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "linux")]
pub use linux::{
    LinuxProcessMemory, LinuxProcessMetadata, ModuleEntry, enumerate_modules, enumerate_processes,
    enumerate_processes_matching, is_process_alive, open_process, process_bitness,
    process_command_line, process_cwd, process_executable, process_metadata_matching, process_name,
};

#[cfg(all(test, any(windows, target_os = "linux")))]
mod tests;
