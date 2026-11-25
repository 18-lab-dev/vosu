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
//! Compiled under `#[cfg(any(test, feature = "test-harness"))]`: always
//! available to `vosu-core`'s own tests, and to any other crate that opts
//! into `test-harness` (forwarded from the root `test-harness` feature,
//! guarded by `lib.rs`'s own release `compile_error!`).

use std::sync::Arc;

use vosu_sys::process::{ProcessBitness, ProcessMemory};

use super::host::read_plans;

mod process;
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

/// Explicit teardown for synchronous tests owning a runtime, config, and host.
///
/// A live instance retains its host handler, whose settings can retain the
/// config store. Runtime drop cannot cancel that store's `spawn_blocking`
/// worker, so dropping these owners without shutdown can wait forever.
/// Declare this guard before creating instances; it also runs on assertion
/// failures. It must be dropped outside an async runtime context.
pub struct ConfigHostShutdown<'a> {
    runtime: &'a tokio::runtime::Runtime,
    config: &'a crate::config::ConfigStore,
    host: &'a super::PluginHost,
    watcher: Option<tokio::task::JoinHandle<()>>,
}

impl<'a> ConfigHostShutdown<'a> {
    /// Retains the services until they have been explicitly shut down.
    pub fn new(
        runtime: &'a tokio::runtime::Runtime,
        config: &'a crate::config::ConfigStore,
        host: &'a super::PluginHost,
    ) -> Self {
        Self {
            runtime,
            config,
            host,
            watcher: None,
        }
    }

    /// Registers this fixture's B2 watcher for cancellation before destruction.
    pub fn set_watcher(&mut self, watcher: tokio::task::JoinHandle<()>) {
        self.watcher = Some(watcher);
    }
}

impl Drop for ConfigHostShutdown<'_> {
    fn drop(&mut self) {
        if let Some(watcher) = self.watcher.take() {
            watcher.abort();
            let _ = self.runtime.block_on(watcher);
        }
        self.host.destroy_all(std::time::Duration::from_secs(5));
        // Await even on assertion failure: abort cannot cancel blocking work.
        let _ = self.runtime.block_on(self.config.shutdown());
    }
}
