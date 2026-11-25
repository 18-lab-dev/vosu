//! Core's [`ProcessLauncher`] for `runtime = "process"` instances
//! (`docs/v2/plugin-abi.md` §7.2, §7.6 "Startup", "Destroy and shutdown").
//!
//! One launch starts one `vosu plugin-host` child of this exact executable
//! generation, sends `CREATE` and returns only after `READY`, within the
//! 5-second startup deadline measured from spawn (Linux) or resume
//! (Windows). Any startup failure kills and reaps the child, closes its
//! endpoints and returns [`HostError::ProcessStartupFailed`] naming the
//! §7.6 stage; nothing was published, so nothing needs rolling back here.
//!
//! Each instance then has two supervision threads, never holding a host
//! lock: an **exit watcher**, the one thread that waits on and reaps the
//! process, and a **ring reader**, which handles lifecycle frames and
//! answers each acknowledged plugin → host frame with core's `STATUS`
//! (§7.6 A5.2). An exit or channel failure that core did not request is
//! reported once through [`InstanceLoss`]. Controlled destroy sends
//! `DESTROY`, waits up to its grace (5 s) for the OS exit, then kills and
//! reaps the child.
//!
//! **Installation.** Production composition does not install this launcher
//! yet: process plugins stay unavailable until the approval flow (A5.5, M5
//! U0) gates them. Tests and fixtures install it through
//! `PluginHost::install_process_launcher`.
//!
//! **Interim until M5 B0/T1.** The ring reader delivers each plugin → host
//! message to core's handler inline, and `VOSU_READ_PLAN_SYNC` submissions
//! are refused by the child, so no `REPLY_NEXT` is ever sent.

mod platform;
mod supervision;

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use vosu_plugin_api::{Init, decode};

use super::control::{Create, StartupFailure, StartupStage};
use crate::plugin::error::HostError;
use crate::plugin::execution::{ProcessEndpoint, ProcessLaunch, ProcessLauncher};

pub use platform::LaunchTarget;

/// From spawn (Linux) or resume (Windows) to `READY` (A5.8).
pub const STARTUP_DEADLINE: Duration = Duration::from_secs(5);
/// The `SYNC` read-plan budget sent in `CREATE`: the same 2-second budget
/// the in-process read-plan path applies (`host::read_plans`).
const SYNC_BUDGET_MS: u32 = 2_000;

/// Starts plugin hosts of this executable generation.
pub struct ProcessHostLauncher {
    target: LaunchTarget,
    /// The last generation handed out; never reused while core runs.
    generation: AtomicU64,
    startup_deadline: Duration,
}

impl std::fmt::Debug for ProcessHostLauncher {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProcessHostLauncher")
            .field("generation", &self.generation)
            .finish_non_exhaustive()
    }
}

impl ProcessHostLauncher {
    /// A launcher for this running executable. `build_identity` is the
    /// executable's compile-time identity, which each child compares with
    /// its own (§7.6 `generation`). Call once at startup: on Windows the
    /// image path is captured now, and on Linux the dedicated launcher
    /// thread starts now.
    pub fn start(build_identity: &str) -> std::io::Result<Self> {
        Ok(Self::new(LaunchTarget::current(build_identity)?))
    }

    /// A launcher for `executable` instead of this one: tests start the
    /// real `vosu` binary from a test process.
    #[cfg(feature = "test-harness")]
    pub fn with_executable(executable: &Path, build_identity: &str) -> std::io::Result<Self> {
        Ok(Self::new(LaunchTarget::executable(
            executable,
            build_identity,
        )?))
    }

    fn new(target: LaunchTarget) -> Self {
        Self {
            target,
            generation: AtomicU64::new(0),
            startup_deadline: STARTUP_DEADLINE,
        }
    }

    fn launch_child(
        &self,
        launch: ProcessLaunch<'_>,
    ) -> Result<Arc<dyn ProcessEndpoint>, StartupFailure> {
        let generation = self.generation.fetch_add(1, Ordering::Relaxed) + 1;
        let create = Create {
            generation,
            plugin_id: launch.manifest.id.clone(),
            library: launch.package.library().to_owned(),
            library_sha256: library_digest(launch.package.library())?,
            concurrent_dispatch: launch.manifest.concurrent_dispatch,
            sync_budget_ms: SYNC_BUDGET_MS,
            init: decode::<Init>(launch.init).map_err(|error| {
                StartupFailure::new(StartupStage::Create, "init_invalid", error.to_string())
            })?,
        }
        .encode()
        .map_err(|error| {
            StartupFailure::new(StartupStage::Create, "init_too_large", error.to_string())
        })?;
        let started = self.target.spawn()?;
        let deadline = Instant::now() + self.startup_deadline;
        supervision::start(
            started,
            supervision::Start {
                generation,
                plugin_id: launch.manifest.id.clone(),
                create,
                deadline,
                ingress: launch.ingress,
                loss: launch.loss,
            },
        )
    }
}

impl ProcessLauncher for ProcessHostLauncher {
    fn launch(&self, launch: ProcessLaunch<'_>) -> Result<Arc<dyn ProcessEndpoint>, HostError> {
        let plugin = launch.manifest.id.clone();
        self.launch_child(launch).map_err(|failure| {
            tracing::warn!(%plugin, %failure, "plugin host did not start");
            HostError::ProcessStartupFailed(failure.to_string())
        })
    }
}

/// The SHA-256 the child must find in the library it opens. Core reads the
/// bytes only to hash them, never maps them.
fn library_digest(path: &Path) -> Result<[u8; 32], StartupFailure> {
    let failure = |error: std::io::Error| StartupFailure {
        stage: StartupStage::Library,
        code: "read_failed".to_owned(),
        errno: error.raw_os_error(),
        message: format!("cannot read {}: {error}", path.display()),
    };
    let mut file = std::fs::File::open(path).map_err(failure)?;
    super::bootstrap::sha256(&mut file).map_err(failure)
}

#[cfg(test)]
mod tests;
