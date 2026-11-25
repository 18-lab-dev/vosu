//! Windows endpoint integration for one `vosu plugin-host` generation
//! (`docs/v2/plugin-abi.md` §7.2, §7.4, §7.6).
//!
//! [`launch`] is core's side: it creates both ring directions at the
//! trusted [`REGION_BYTES`], initializes their headers, starts the child
//! through `vosu_sys::plugin_process::spawn_plugin_host` (strict job
//! assigned before resume, exactly the four ring objects inherited) and
//! attaches core's channel end. [`attach_inherited`] is the child's side:
//! it adopts the four handles its endpoint argument names and attaches the
//! swapped end. The protocol above the channel (startup, control frames,
//! supervision) belongs to the units that consume these two functions.

use std::ffi::OsStr;
use std::io;
use std::path::Path;

use vosu_sys::plugin_process::{
    EndpointError, PluginHostProcess, RingObjects, SpawnError, adopt_inherited, spawn_plugin_host,
};

use super::channel::{Endpoint, connect};
use super::frame::{Direction, DirectionPolicy};
use super::ring::{REGION_BYTES, RingError, initialize};

/// Core's handles on one launched plugin host: its channel end and its
/// confined process. Dropping `process` kills the plugin host's job.
#[derive(Debug)]
pub struct Launched {
    pub endpoint: Endpoint,
    pub process: PluginHostProcess,
}

/// Why a launch failed. Every created object is closed and any started
/// process terminated before this is returned.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("creating ring objects failed: {0}")]
    Objects(#[source] io::Error),
    #[error(transparent)]
    Spawn(#[from] SpawnError),
    #[error("attaching core's channel failed: {0}")]
    Ring(#[from] RingError),
}

impl LaunchError {
    /// The §7.6 startup stage: `job` when the job could not be created,
    /// assigned or verified, `spawn` otherwise. Neither is retried.
    pub fn startup_stage(&self) -> &'static str {
        match self {
            Self::Spawn(error) => error.step.startup_stage(),
            Self::Objects(_) | Self::Ring(_) => "spawn",
        }
    }
}

/// Starts `program` (the image path captured at startup) with
/// `leading_args`, followed by the endpoint argument, and returns core's
/// channel end with the running, confined child.
pub fn launch(program: &Path, leading_args: &[&OsStr]) -> Result<Launched, LaunchError> {
    let host_to_plugin = RingObjects::create(REGION_BYTES).map_err(LaunchError::Objects)?;
    let plugin_to_host = RingObjects::create(REGION_BYTES).map_err(LaunchError::Objects)?;
    initialize(host_to_plugin.region())?;
    initialize(plugin_to_host.region())?;
    let process = spawn_plugin_host(program, leading_args, &host_to_plugin, &plugin_to_host)?;
    // A failure here drops `process`, whose job closes and kills the child.
    let endpoint = connect(
        host_to_plugin.into(),
        plugin_to_host.into(),
        Box::new(DirectionPolicy::with_ledger(Direction::PluginToHost)),
    )?;
    Ok(Launched { endpoint, process })
}

/// Why the child could not attach its inherited endpoints. It exits with
/// the §7.6 `endpoints` stage before running anything else.
#[derive(Debug, thiserror::Error)]
pub enum AttachError {
    #[error(transparent)]
    Endpoints(#[from] EndpointError),
    #[error("attaching the plugin host's channel failed: {0}")]
    Ring(#[from] RingError),
}

/// The plugin host's side: adopts the endpoints `endpoint_arg` names
/// (validated kinds, exact sizes, no longer inheritable) and attaches the
/// channel with the directions swapped.
pub fn attach_inherited(endpoint_arg: &str) -> Result<Endpoint, AttachError> {
    let endpoints = adopt_inherited(endpoint_arg, REGION_BYTES)?;
    Ok(connect(
        endpoints.plugin_to_host.into(),
        endpoints.host_to_plugin.into(),
        Box::new(DirectionPolicy::with_ledger(Direction::HostToPlugin)),
    )?)
}

#[cfg(test)]
mod tests;
