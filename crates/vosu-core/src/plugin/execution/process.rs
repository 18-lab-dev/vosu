//! The `runtime = "process"` side of the execution seam
//! (`docs/v2/plugin-abi.md` §7.2): validated package metadata, the launcher
//! that turns it into one child per instance, the endpoint core dispatches
//! through, and the two directions back into core — plugin → host messages
//! ([`PluginIngress`]) and instance loss ([`InstanceLoss`]).

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::time::Duration;

use vosu_plugin_api::{PluginManifest, vosu_status};

use crate::identity::PluginInstanceId;
use crate::plugin::error::HostError;
use crate::plugin::host_services::{self, PluginHostHandler};

/// A `runtime = "process"` package's executable part: the confined,
/// canonical library path for this host's target. Core never maps it; the
/// child loads it with `descriptor::load_library` after its platform
/// boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPackage {
    root: PathBuf,
    library: PathBuf,
}

impl ProcessPackage {
    pub(crate) fn new(root: PathBuf, library: PathBuf) -> Self {
        Self { root, library }
    }

    /// The canonical package directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The canonical library path, already confined to [`Self::root`].
    pub fn library(&self) -> &Path {
        &self.library
    }
}

/// One host ↔ plugin message as borrowed bytes: the `vosu_message` fields
/// without its pointer, so a transport relays the payload unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MessageRef<'a> {
    pub opcode: u32,
    pub flags: u32,
    pub correlation_id: u64,
    pub payload: &'a [u8],
}

/// Core's half of one process instance's channel.
///
/// Core calls `dispatch` from this instance's dispatch worker (or the
/// shared `concurrent_dispatch` pool), never while holding a host lock,
/// and treats it exactly like a local plugin's `dispatch`: it returns the
/// status the child's plugin reported for this message, or a non-OK
/// status once the channel is lost. A lost endpoint must return promptly
/// rather than block, so a stuck child can never hold core workers.
pub trait ProcessEndpoint: Send + Sync {
    fn dispatch(&self, message: MessageRef<'_>) -> vosu_status;

    /// Controlled shutdown (§4, A5.8): the plugin's `destroy` runs in the
    /// child within `grace`; past it the endpoint kills and reaps the
    /// child and reports `HostError::DestroyTimedOut`. Called at most
    /// once, after core stopped dispatching.
    fn destroy(&self, grace: Duration) -> Result<(), HostError>;

    /// Ends the child immediately (kill and reap) and fails every waiter,
    /// without the plugin's `destroy`: used after loss and when queued
    /// dispatch could not drain within its grace. Idempotent.
    fn terminate(&self);
}

/// Everything one process launch receives. The child gets the package
/// and `init`; core keeps `ingress` and `loss` on the parent side of the
/// channel.
pub struct ProcessLaunch<'a> {
    pub instance: PluginInstanceId,
    pub manifest: &'a PluginManifest,
    pub package: &'a ProcessPackage,
    pub init: &'a [u8],
    pub ingress: PluginIngress,
    pub loss: InstanceLoss,
}

/// Starts one `vosu plugin-host` child per instance (§7.2).
pub trait ProcessLauncher: Send + Sync {
    /// Returns only once the child reported its plugin's `create`
    /// succeeded, or an error after reaping a child that did not, within
    /// the bounded startup deadline (A5.8). Core holds no host lock across
    /// this call. Every call creates a distinct child and endpoint;
    /// nothing is shared between instances.
    fn launch(&self, launch: ProcessLaunch<'_>) -> Result<Arc<dyn ProcessEndpoint>, HostError>;
}

/// The plugin → host direction for one process instance: the same
/// validation and routing the in-process `send` trampoline applies (§3
/// frame cap, opcode and flag policy, synchronous read-plan and
/// publication fast paths).
#[derive(Clone)]
pub struct PluginIngress {
    instance: PluginInstanceId,
    handler: Arc<dyn PluginHostHandler>,
}

impl PluginIngress {
    pub(crate) fn new(instance: PluginInstanceId, handler: Arc<dyn PluginHostHandler>) -> Self {
        Self { instance, handler }
    }

    pub fn instance(&self) -> PluginInstanceId {
        self.instance
    }

    /// Routes one message the child's plugin sent through `send`, returning
    /// the status that `send` must report back to it.
    pub fn deliver(&self, message: MessageRef<'_>) -> vosu_status {
        host_services::route_message(self.handler.as_ref(), self.instance, message)
    }

    /// Queues `job` on the host's bounded broker workers under this
    /// instance ([`PluginHostHandler::submit_broker_work`]); `Err` returns
    /// it unrun when the host has none.
    pub(crate) fn submit_broker_work(
        &self,
        job: host_services::BrokerJob,
    ) -> Result<(), host_services::BrokerJob> {
        self.handler.submit_broker_work(self.instance, job)
    }
}

/// Why a process instance was lost.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub enum LossCause {
    /// The OS reported the child exited (any exit core did not request,
    /// including status 0).
    ProcessExited { status: Option<i64> },
    /// The channel failed: a peer protocol violation or a broken doorbell.
    ChannelFailed,
    /// A legal frame core wrote waited the whole 2-second backpressure
    /// window for ring space (§7.2 "Pressure, loss and restart").
    Stalled,
    /// Core killed the instance: an action it timed out stayed unanswered
    /// past its 5-second cancellation grace (§7.6 A5.4).
    ActionGraceExpired,
}

impl LossCause {
    /// The §7.6 loss reason, as supervision and the management surface
    /// name it. A child exits with
    /// [`EXIT_TRANSPORT_TIMEOUT`](crate::plugin::process::control::EXIT_TRANSPORT_TIMEOUT)
    /// only when one of its waiter backstops expired: core stopped serving
    /// the channel, recorded as one `transport_timeout` loss.
    pub fn reason(&self) -> &'static str {
        match self {
            Self::ProcessExited {
                status: Some(status),
            } if *status == i64::from(crate::plugin::process::control::EXIT_TRANSPORT_TIMEOUT) => {
                "transport_timeout"
            }
            Self::ProcessExited { .. } => "exited",
            Self::ChannelFailed => "protocol_violation",
            Self::Stalled => "pressure_timeout",
            Self::ActionGraceExpired => "action_grace_expired",
        }
    }
}

/// The one-shot loss flag shared by an instance and its [`InstanceLoss`].
pub(crate) struct LossState {
    lost: AtomicBool,
}

impl LossState {
    pub(crate) fn new() -> Arc<Self> {
        Arc::new(Self {
            lost: AtomicBool::new(false),
        })
    }

    pub(crate) fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    /// Marks the instance lost; `true` only for the first mark across
    /// this flag and every [`InstanceLoss`] sharing it.
    pub(crate) fn mark(&self) -> bool {
        !self.lost.swap(true, Ordering::AcqRel)
    }
}

/// Reports one process instance lost. Clones share one flag, so the exit
/// watcher and the channel reader can each hold one and only the first
/// report reaches the host.
#[derive(Clone)]
pub struct InstanceLoss {
    instance: PluginInstanceId,
    state: Arc<LossState>,
    // Weak: a loss reporter must never keep the host alive.
    handler: Weak<dyn PluginHostHandler>,
}

impl InstanceLoss {
    pub(crate) fn new(
        instance: PluginInstanceId,
        state: Arc<LossState>,
        handler: &Arc<dyn PluginHostHandler>,
    ) -> Self {
        Self {
            instance,
            state,
            handler: Arc::downgrade(handler),
        }
    }

    pub fn instance(&self) -> PluginInstanceId {
        self.instance
    }

    /// Marks the instance lost and notifies the host. Returns `true` only
    /// for the first report across every clone; later reports change
    /// nothing. Safe to call from a thread the endpoint itself owns: the
    /// host retires the instance on a separate thread.
    pub fn report(&self, cause: LossCause) -> bool {
        if self.state.lost.swap(true, Ordering::AcqRel) {
            return false;
        }
        if let Some(handler) = self.handler.upgrade() {
            handler.instance_lost(self.instance, cause);
        }
        true
    }
}
