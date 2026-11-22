//! Core plugin host: the explicit `BUNDLED` array and external `dlopen`
//! registry, instance lifecycle, dispatch, and process matching
//! (`docs/v2/implementation-plan.md` M1 "Core plugin host";
//! `docs/v2/plugin-abi.md` §1, §2, §4, §5, §6.1, §7.1).
//!
//! Out of scope for this phase (later milestones): wiring these
//! mechanisms to real OS discovery (phase (d)), session lifecycle, focus,
//! read plans, scanning, settings, logging rate limits, and timers.

mod dispatch;
mod error;
mod host_services;
mod instance;
mod matcher;
mod registry;

pub use dispatch::{OwnedMessage, WorkerPool};
pub use error::HostError;
pub use host_services::{PluginHostHandler, PluginInstanceId};
pub use instance::{DEFAULT_QUEUE_CAPACITY, PluginInstance};
pub use matcher::{
    CandidateProcess, CompiledMatcher, MatchOutcome, MatchPlatform, MatcherEngine, MatcherError,
    OverlapError, PluginId, RegisterError, overlaps,
};
pub use registry::{
    BundledPlugin, LoadedPlugin, development_roots, load_bundled, load_development,
};

#[cfg(test)]
mod tests;
