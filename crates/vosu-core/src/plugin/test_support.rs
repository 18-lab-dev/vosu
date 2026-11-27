//! Development-only memory seam for driving a real [`super::host::PluginHost`]
//! and its read-plan/scan routing against synthetic process memory, without
//! a real OS process (`docs/v2/implementation-plan.md` plan:121-124's
//! per-crate `test-harness` release guard; AGENTS.md "test-harness is
//! development-only"). `vosu-core`'s own unit tests already had this seam
//! through `host::read_plans`'s own hook map; this module exposes the same
//! hook map through a stable, crate-external `pub` path so an external
//! test (e.g. the root `tests/plugin_host.rs`) can inject a session's
//! process memory too, without needing `#[cfg(test)]` visibility into
//! `vosu-core`'s own internals.
//!
//! Compiled under `#[cfg(any(test, feature = "bench-harness"))]`: always
//! available to `vosu-core`'s own tests, and to any other crate that opts
//! into `test-harness` (forwarded from the root `test-harness` feature,
//! guarded by `lib.rs`'s own release `compile_error!`) or `bench-harness`
//! (the root's release benches only, which drive the real process runtime
//! against the same fixtures as their in-process cases; production never
//! enables it). The recording launcher and package writer below stay
//! `test-harness`-only.

use std::sync::Arc;

use vosu_plugin_api::memory::{ProcessBitness, ProcessMemory};

use super::host::read_plans;

#[cfg(any(test, feature = "test-harness"))]
mod process;
#[cfg(any(test, feature = "test-harness"))]
pub use process::{
    LaunchBehavior, LaunchRecord, RecordedFrame, RecordingEndpoint, RecordingLauncher,
    host_target_triple, write_development_package,
};

/// Substitutes the process [`PluginHost::session_memory`] would otherwise
/// open from the real OS for `pid` with an already-built `memory`/`bitness`
/// pair — e.g. a byte buffer wrapped in a test [`ProcessMemory`] impl, with
/// no real process behind `pid` at all. Keyed by `pid`, not a single global
/// override, so independent tests in the same process never collide as
/// long as each picks its own synthetic pid (same discipline the reviewer
/// checklist requires of every test using this seam).
///
/// [`PluginHost::session_memory`]: super::host::PluginHost
pub fn set_session_memory(pid: u32, memory: Arc<dyn ProcessMemory>, bitness: ProcessBitness) {
    read_plans::set_session_memory_hook_for_tests(pid, memory, bitness);
}

/// Removes `pid`'s hook installed by [`set_session_memory`]: lets a test
/// retire a synthetic pid once it is done with it instead of leaking it in
/// the hook registry for the rest of the process's life.
pub fn clear_session_memory(pid: u32) {
    read_plans::clear_session_memory_hook_for_tests(pid);
}

/// Explicit teardown guard for synchronous tests owning a [`PluginHost`].
///
/// Declare this guard before creating instances so `destroy_all` runs even
/// when an assertion fails. Must be dropped outside an async runtime context.
///
/// [`PluginHost`]: super::PluginHost
pub struct HostShutdown<'a> {
    host: &'a super::PluginHost,
}

impl<'a> HostShutdown<'a> {
    pub fn new(host: &'a super::PluginHost) -> Self {
        Self { host }
    }
}

impl Drop for HostShutdown<'_> {
    fn drop(&mut self) {
        self.host.destroy_all(std::time::Duration::from_secs(5));
    }
}
