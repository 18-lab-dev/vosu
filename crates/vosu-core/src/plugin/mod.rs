//! Core plugin host: the explicit `BUNDLED` array and external `dlopen`
//! registry, instance lifecycle, dispatch, process matching, session
//! lifecycle, and focus
//! (`docs/v2/implementation-plan.md` M1 "Core plugin host";
//! `docs/v2/plugin-abi.md` §1, §2, §4, §5, §6.1, §6.2, §7.1), plus the M1
//! host service logging with the 200/s rate limit (§6.10).
//!
//! Out of scope for this phase (later milestones): wiring these
//! mechanisms to real OS discovery (phase (d)), read plans, scanning,
//! settings, and timers.

mod dispatch;
mod error;
mod focus;
mod host_services;
mod instance;
pub mod logging;
mod matcher;
mod registry;
mod session;

pub use dispatch::{OwnedMessage, WorkerPool};
pub use error::HostError;
pub use focus::{
    FocusCandidate, FocusEvent, FocusKey, FocusTracker, ForegroundSource, SyntheticForegroundSource,
};
pub use host_services::{PluginHostHandler, PluginInstanceId};
pub use instance::{DEFAULT_QUEUE_CAPACITY, PluginInstance};
pub use matcher::{
    CandidateProcess, CompiledMatcher, MatchOutcome, MatchPlatform, MatcherEngine, MatcherError,
    OverlapError, PluginId, RegisterError, overlaps,
};
pub use registry::{
    BundledPlugin, LoadedPlugin, development_roots, load_bundled, load_development,
};
pub use session::{
    AttachedSession, DeclineOutcome, DiscoveredProcess, Generation, IDENTIFICATION_TIMEOUT,
    IdentifyOutcome, SessionError, SessionHandle, SessionRegistry,
};

#[cfg(test)]
mod tests;
