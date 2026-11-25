//! Linux launch of one `vosu plugin-host` channel and the child's adoption
//! of it (`docs/v2/plugin-abi.md` §7.2, §7.3, §7.6 "Launch", A5.8).
//!
//! Core side: [`HostSpawner`] owns the long-lived launcher thread. Each
//! [`HostSpawner::spawn`] creates and initializes both rings at the trusted
//! [`REGION_BYTES`], starts the child with exactly those four objects, and
//! attaches core's end of the channel under the frozen plugin → host
//! [`DirectionPolicy`]. The child it returns is killed and reaped when
//! dropped.
//!
//! Child side: [`adopt_channel`] takes ownership of the inherited
//! descriptors, validates their kind and sealed size while mapping them, and
//! attaches the child's end with the directions swapped. The child entry
//! runs it between `vosu_sys::plugin_process::arm_parent_death` (§7.3
//! step 1) and `vosu_sys::plugin_process::drop_privileges` (steps 2–6), so
//! a failed drop can still be reported over the channel.

use std::ffi::OsStr;
use std::io;
use std::path::PathBuf;

use vosu_sys::plugin_process::{
    EndpointError, InheritedEndpoints, PluginHostChild, PluginHostCommand, PluginHostLauncher,
    RingObjects, SpawnError,
};

use super::channel::{self, Endpoint};
use super::frame::{Direction, DirectionPolicy};
use super::ring::{self, REGION_BYTES, RingError};

/// Core re-executes its own image, so an updater that replaced the file on
/// disk cannot mix generations (§7.6 "Launch").
const SELF_EXECUTABLE: &str = "/proc/self/exe";

/// Starts plugin hosts from one dedicated launcher thread (A5.8). Create it
/// once and keep it until shutdown; its thread outlives it for as long as
/// any child it spawned is still unreaped.
#[derive(Debug)]
pub struct HostSpawner {
    launcher: PluginHostLauncher,
    executable: PathBuf,
    build_identity: String,
}

/// One started plugin host and core's attached end of its channel.
#[derive(Debug)]
pub struct SpawnedHost {
    pub child: PluginHostChild,
    pub channel: Endpoint,
}

/// Why a plugin host could not be started (startup stage `spawn`). Any
/// child already forked has been killed and reaped.
#[derive(Debug, thiserror::Error)]
pub enum LaunchError {
    #[error("cannot create the plugin-host rings: {0}")]
    Objects(#[source] io::Error),
    #[error("cannot initialize or attach a plugin-host ring: {0}")]
    Ring(#[source] RingError),
    #[error(transparent)]
    Spawn(SpawnError),
}

impl HostSpawner {
    /// Starts the launcher thread. `build_identity` is the compile-time
    /// identity the child compares with its own (§7.6).
    pub fn start(build_identity: impl Into<String>) -> io::Result<Self> {
        Self::with_executable(PathBuf::from(SELF_EXECUTABLE), build_identity.into())
    }

    fn with_executable(executable: PathBuf, build_identity: String) -> io::Result<Self> {
        Ok(Self {
            launcher: PluginHostLauncher::start()?,
            executable,
            build_identity,
        })
    }

    /// Creates one instance's rings, starts its plugin host, and attaches
    /// core's end. Returns once the child's `execve` succeeded; the child's
    /// own startup (§7.6 `CREATE`/`READY`) happens over the channel.
    pub fn spawn(&self) -> Result<SpawnedHost, LaunchError> {
        let to_plugin = RingObjects::create(REGION_BYTES).map_err(LaunchError::Objects)?;
        let to_host = RingObjects::create(REGION_BYTES).map_err(LaunchError::Objects)?;
        ring::initialize(to_plugin.region()).map_err(LaunchError::Ring)?;
        ring::initialize(to_host.region()).map_err(LaunchError::Ring)?;
        let child = self
            .launcher
            .spawn(&PluginHostCommand {
                executable: &self.executable,
                build_identity: &self.build_identity,
                host_to_plugin: &to_plugin,
                plugin_to_host: &to_host,
            })
            .map_err(LaunchError::Spawn)?;
        // On failure `child` drops here, which kills and reaps it.
        let channel = channel::connect(
            to_plugin.into(),
            to_host.into(),
            Box::new(DirectionPolicy::new(Direction::PluginToHost)),
        )
        .map_err(LaunchError::Ring)?;
        Ok(SpawnedHost { child, channel })
    }
}

/// Why the child refused its inherited endpoints (startup stage
/// `endpoints`).
#[derive(Debug, thiserror::Error)]
pub enum AdoptError {
    #[error(transparent)]
    Endpoints(EndpointError),
    #[error("inherited ring objects are invalid: {0}")]
    Map(#[source] io::Error),
    #[error("inherited ring is invalid: {0}")]
    Ring(#[source] RingError),
}

/// The child's half: adopts the descriptors the endpoint `argument` names,
/// maps both regions at exactly [`REGION_BYTES`] after checking their kind
/// and seal, and attaches the channel with directions swapped under the
/// frozen host → plugin [`DirectionPolicy`]. Succeeds at most once per
/// process.
pub fn adopt_channel(argument: &OsStr) -> Result<Endpoint, AdoptError> {
    let endpoints = InheritedEndpoints::adopt(argument).map_err(AdoptError::Endpoints)?;
    let to_plugin = endpoints
        .host_to_plugin
        .into_ring_objects(REGION_BYTES)
        .map_err(AdoptError::Map)?;
    let to_host = endpoints
        .plugin_to_host
        .into_ring_objects(REGION_BYTES)
        .map_err(AdoptError::Map)?;
    channel::connect(
        to_host.into(),
        to_plugin.into(),
        Box::new(DirectionPolicy::new(Direction::HostToPlugin)),
    )
    .map_err(AdoptError::Ring)
}

#[cfg(test)]
mod tests;
