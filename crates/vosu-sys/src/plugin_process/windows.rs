//! Windows objects behind one ring direction: an unnamed pagefile-backed
//! section view and an unnamed auto-reset event doorbell
//! (`docs/v2/plugin-abi.md` §7.2); and the plugin host's race-free launch
//! into a strict kill-on-close job with exactly those objects inherited
//! (§7.4).

mod channel;
mod job;
mod objects;
mod spawn;

pub use channel::{EndpointError, EndpointValues, InheritedEndpoints, adopt_inherited};
pub use job::{CurrentJob, JobProcessIds, PLUGIN_JOB_LIMIT_FLAGS, PluginJob, current_job};
pub use objects::Doorbell;
pub(super) use objects::Mapping;
pub(super) use objects::{adopt_mapping, create_mapping};
pub use spawn::{
    KILL_EXIT_CODE, PluginHostProcess, SpawnError, SpawnStep, UNWIND_EXIT_CODE, UNWIND_REAP_BOUND,
    UnwoundChild, spawn_plugin_host,
};

#[cfg(test)]
mod tests;
