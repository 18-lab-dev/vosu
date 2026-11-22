//! Core plugin host: the explicit `BUNDLED` array and external `dlopen`
//! registry, instance lifecycle, dispatch, process matching, session
//! lifecycle, focus, and the read-plan executor/scanning primitives
//! (`docs/v2/implementation-plan.md` M1 "Core plugin host";
//! `docs/v2/plugin-abi.md` §1, §2, §4, §5, §6.1, §6.2, §6.3, §6.4, §7.1), plus the M1
//! host services: settings (§6.9), logging with the 200/s rate limit
//! (§6.10), and timers (§6.11), wired into one `PluginHostHandler`
//! (`host.rs`'s `PluginHost`) that owns every live instance.
//!
//! Out of scope for this phase (later milestones): wiring `PluginHost` to
//! real OS discovery (phase (d)), session (§6.2)/focus delivery through
//! it, and routing read plans and scans into `PluginHostHandler::handle`
//! (`read_plan`/`scan` are standalone modules a later handler calls into).

mod dispatch;
mod error;
mod focus;
mod host;
mod host_services;
mod instance;
pub mod logging;
mod matcher;
mod read_plan;
mod registry;
mod scan;
mod session;
pub mod settings;
pub mod timers;

pub use dispatch::{OwnedMessage, WorkerPool};
pub use error::HostError;
pub use focus::{
    FocusCandidate, FocusEvent, FocusKey, FocusTracker, ForegroundSource, SyntheticForegroundSource,
};
pub use host::PluginHost;
pub use host_services::{PluginHostHandler, PluginInstanceId};
pub use instance::{DEFAULT_QUEUE_CAPACITY, PluginInstance};
pub use matcher::{
    CandidateProcess, CompiledMatcher, MatchOutcome, MatchPlatform, MatcherEngine, MatcherError,
    OverlapError, PluginId, RegisterError, overlaps,
};
pub use read_plan::{
    DispatchThreadScope, ExecuteBytesError, FinishOutcome, MAX_OUTSTANDING_ASYNC_PLANS,
    ModuleResolver, OutstandingPlans, OutstandingPlansError, ReadPlanError, SyncReadPlanError,
    deliver_sync, execute, execute_bytes, on_dispatch_thread,
};
pub use registry::{
    BundledPlugin, LoadedPlugin, development_roots, load_bundled, load_development,
};
#[cfg(windows)]
pub use scan::WindowsModuleTable;
pub use scan::{
    MAX_OUTSTANDING_SCANS, ModuleRegion, ModuleRegions, OutstandingScans, OutstandingScansError,
    QueueFullError, ScanSlot, ScanSubmitError, ScanWorkerPool, execute as execute_scan,
    submit as submit_scan,
};
pub use session::{
    AttachedSession, DeclineOutcome, DiscoveredProcess, Generation, IDENTIFICATION_TIMEOUT,
    IdentifyOutcome, SessionError, SessionHandle, SessionRegistry,
};

#[cfg(test)]
mod tests;
