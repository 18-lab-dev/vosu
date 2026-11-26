//! Core plugin host: the explicit `BUNDLED` array and external `dlopen`
//! registry, instance lifecycle, dispatch, process matching, session
//! lifecycle, focus, and the read-plan executor/scanning primitives
//! (`docs/v2/implementation-plan.md` M1 "Core plugin host";
//! `docs/v2/plugin-abi.md` §1, §2, §4, §5, §6.1, §6.2, §6.3, §6.4, §7.1), plus the M1
//! host services: settings (§6.9), logging with the 200/s rate limit
//! (§6.10), and timers (§6.11), wired into one `PluginHostHandler`
//! (`host.rs`'s `PluginHost`) that owns every live instance.
//!
//! The registry validates every package through one path and then
//! attaches its execution backend (`execution.rs`, §7.1–§7.2): local code
//! (bundled vtables, or a debug development library) or a
//! `runtime = "process"` package whose library core never maps; instances
//! dispatch through the same queues to either a local vtable or a process
//! endpoint.
//!
//! `PluginHost` also owns process matching, session lifecycle, and focus
//! (`host.rs`'s `process_appeared`/`process_exited`/`foreground_changed`,
//! `MatcherEngine`, `SessionRegistry`, `FocusTracker`):
//! `process_appeared`/`process_exited` are discovery's own entry points
//! for a process appearing or exiting; `foreground_changed` is that same
//! phase's entry point for an OS foreground-window-change event,
//! re-running the Focus rule.
//!
//! Memory read plans (§6.3, `host/read_plans.rs`) and signature scans
//! (§6.4, `host/scans.rs`) both route through `PluginHost::handle` now,
//! sharing one per-session cached process handle
//! (`PluginHost::session_memory`), the same ownership check
//! (`PluginHost::resolve_owned_session`), and one bounded worker pool
//! (`PluginHost::background_pool`) rather than each opening/spawning its
//! own. `scan`/`read_plan` themselves stay standalone modules (pure
//! executors, no `PluginHost` state) that `host/scans.rs`/
//! `host/read_plans.rs` wire into instance/session state; neither pure
//! module is itself a `PluginHostHandler`.
//!
//! `discovery.rs` (M1 §4, phase d2b) is the generic core discovery
//! service that feeds `process_appeared`/`process_exited` from a real OS
//! process list in production: a pluggable [`discovery::ProcessSource`]
//! seam — the Linux eBPF/CN_PROC/polling backend
//! ([`discovery::LinuxSource`], `linux-process-events` feature) and the
//! Windows polling backend (`discovery::WindowsProcessSource`, with an
//! executable-name pre-filter against the enabled plugin set's
//! `process_names`) both live here too, not the root crate; a synthetic
//! in-memory source in `discovery`'s own tests — one dedicated
//! background thread (never a Tokio worker), periodic full
//! reconciliation as a bounded-loss backstop plus targeted event-driven
//! updates when the source provides them, the Linux comm-hint pre-filter
//! sync (`MatcherEngine::comm_hints`), and PID-generation correctness (a
//! reused pid is reported as an exit immediately followed by a fresh
//! appearance). `DiscoveryService::start` only provides the mechanism;
//! whether to call it at all (zero cost when no enabled plugin has a
//! matcher, `PluginHost::has_enabled_matchers`) is
//! `src/runtime/discovery.rs`'s own concern, alongside picking the
//! platform `ProcessSource` and starting `discovery::ForegroundPoller`
//! (Windows only) under the same gate.
//!
//! State subscriptions deliver latest-only updates and non-owning session/focus
//! events (§6.2, §6.12). Resource/tree/stream operations and action calls remain
//! out of scope until their respective milestones.

pub mod approval;
mod discovery;
mod dispatch;
mod dispatch_queue;
mod error;
mod execution;
mod focus;
mod host;
mod host_services;
mod instance;
pub mod logging;
mod matcher;
pub mod outbox;
pub mod process;
mod read_plan;
mod registry;
pub(crate) mod resource_lease;
mod scan;
pub use resource_lease::StreamLease;
mod session;
pub mod settings;
#[cfg(any(test, feature = "test-harness"))]
pub mod test_support;
pub mod timers;
pub use crate::identity::PluginInstanceId;
#[cfg(all(target_os = "linux", feature = "linux-process-events"))]
pub use discovery::LinuxSource;
#[cfg(target_os = "linux")]
pub use discovery::linux_common;
pub use discovery::{
    DiscoveryCadence, DiscoveryService, FOREGROUND_POLL_INTERVAL, ProcessEventBatch, ProcessSource,
    cadence_interval,
};
#[cfg(windows)]
pub use discovery::{ForegroundPoller, WindowsProcessSource};
pub use dispatch::{OwnedMessage, WorkerPool};
pub use error::HostError;
pub use execution::{
    Execution, InstanceLoss, LocalCode, LossCause, MessageRef, PackageOrigin, PluginIngress,
    ProcessEndpoint, ProcessLaunch, ProcessLauncher, ProcessPackage,
};
pub use focus::{
    FocusCandidate, FocusEvent, FocusKey, FocusTracker, ForegroundSource, OsForegroundSource,
    SyntheticForegroundSource,
};
pub(crate) use host::SurfaceRequester;
pub use host::resource_requests::{
    MAX_PENDING_RESOURCE_REQUESTS, ResourceOwner, ResourceReply, ResourceRequest,
    ResourceRequestError, ResourceRequests,
};
pub use host::resources::{OverlayRootRetirement, RootCapability};
pub use host::{
    DiagnosticSession, DiscoveryObserver, PackageRegistration, PluginHost, ProcessRecord,
    ProviderRegistration, ValidationSnapshot,
};
pub use host_services::PluginHostHandler;
pub use instance::{DEFAULT_QUEUE_CAPACITY, PluginInstance};
pub use matcher::{
    CandidateProcess, CompiledMatcher, MatchOutcome, MatchPlatform, MatcherEngine, MatcherError,
    OBSERVER_PREFILTER_BUDGET, OverlapError, PluginId, Prefilter, RegisterError, host_platform,
    overlaps,
};
pub use read_plan::{
    DispatchThreadScope, ExecuteBytesError, FinishOutcome, MAX_OUTSTANDING_ASYNC_PLANS,
    ModuleResolver, OutstandingPlans, OutstandingPlansError, ReadPlanError, SyncReadPlanError,
    deliver_sync, execute, execute_bytes, execute_into, execute_sync_into, on_dispatch_thread,
};
#[cfg(feature = "test-harness")]
pub use registry::load_official_test_fixture;
pub use registry::{
    BundledPlugin, LoadedPlugin, development_roots, load_bundled, load_development, load_installed,
};
#[cfg(target_os = "linux")]
pub use scan::LinuxModuleTable;
#[cfg(windows)]
pub use scan::WindowsModuleTable;
pub use scan::{
    MAX_OUTSTANDING_SCANS, ModuleRegion, ModuleRegions, OutstandingScans, OutstandingScansError,
    ScanRouting, ScanSlot, ScanSubmitError, ScanWorkerPool, execute as execute_scan,
    submit as submit_scan,
};
pub use session::{
    AttachedSession, DeclineOutcome, DiscoveredProcess, Generation, IDENTIFICATION_TIMEOUT,
    IdentifyOutcome, SessionError, SessionHandle, SessionRegistry,
};

#[cfg(test)]
mod tests;
