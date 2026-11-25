//! Execution backends for a validated plugin package
//! (`docs/v2/plugin-abi.md` §1, §2, §7).
//!
//! The registry validates a package once — manifest, schemas, grants and
//! action assets — and then attaches exactly one [`Execution`]:
//!
//! - [`Execution::Local`]: code this process may call directly. A bundled
//!   plugin's linked vtable, or a debug development library the local
//!   in-process grant (§7.1) already `dlopen`ed through
//!   [`descriptor::load_library`].
//! - [`Execution::Process`]: a `runtime = "process"` grant. Core records
//!   the confined library path ([`ProcessPackage`]) and never maps, resolves
//!   or calls that library: its static constructors can run code at load
//!   time, so only the confined `vosu plugin-host` child may load it, after
//!   its platform boundary (§7.2–§7.4), with the same
//!   [`descriptor::load_library`] checks.
//!
//! A created instance dispatches through one [`DispatchTarget`]: the local
//! vtable, or a [`ProcessEndpoint`] that a [`ProcessLauncher`] returned.
//! Core's dispatch queues, outboxes, state delivery and host services are
//! backend-neutral and reused for both.

pub(crate) mod descriptor;
mod process;

use std::sync::Arc;

use libloading::Library;
use vosu_plugin_api::{vosu_bytes, vosu_message, vosu_plugin_vtable, vosu_status};

use super::dispatch::PluginCtx;

pub(crate) use process::LossState;
pub use process::{
    InstanceLoss, LossCause, MessageRef, PluginIngress, ProcessEndpoint, ProcessLaunch,
    ProcessLauncher, ProcessPackage,
};

/// Where a validated package came from: the explicit provenance that
/// decides schema trust and bundled-only grants. Never inferred from the
/// execution backend — a process package has no local library either,
/// and must not be mistaken for a bundled one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PackageOrigin {
    /// The root crate's explicit `BUNDLED` array (D13).
    Bundled,
    /// A `--plugin <dir>` development package (§7.1).
    Development,
    /// A catalog-installed package. `official` is current §7.1 policy's
    /// `TrustTier::Official` decision for its exact installed source
    /// identity; a trusted user source is `false`.
    Installed { official: bool },
}

/// The one way a validated package is allowed to run. Cloning shares a
/// local library mapping; supervision keeps a clone of a process package
/// to start its next generation.
#[derive(Clone)]
pub enum Execution {
    /// In-process code: bundled, or granted the local development trust.
    Local(LocalCode),
    /// `runtime = "process"`: never entered in core.
    Process(ProcessPackage),
}

/// A validated local vtable. `library` keeps a `dlopen`ed development
/// library mapped for as long as any instance still calls into it; `None`
/// for a bundled plugin linked into this executable.
#[derive(Clone)]
pub struct LocalCode {
    pub(crate) vtable: vosu_plugin_vtable,
    pub(crate) library: Option<Arc<Library>>,
}

/// Where one instance's host → plugin messages go: its local vtable or its
/// process endpoint. Every dispatch worker, outbox drain, state delivery
/// and synchronous read-plan result calls through this one seam.
#[derive(Clone)]
pub(crate) enum DispatchTarget {
    Local {
        vtable: vosu_plugin_vtable,
        ctx: PluginCtx,
    },
    Process(Arc<dyn ProcessEndpoint>),
}

impl DispatchTarget {
    /// Delivers one message and returns the plugin's dispatch status. The
    /// local path borrows `payload` for the call and allocates nothing
    /// (D17's synchronous read-plan result relies on this); the process
    /// path hands the same bytes to the endpoint unchanged.
    pub(crate) fn dispatch(
        &self,
        opcode: u32,
        flags: u32,
        correlation_id: u64,
        payload: &[u8],
    ) -> vosu_status {
        match self {
            Self::Local { vtable, ctx } => {
                let message = vosu_message {
                    opcode,
                    flags,
                    correlation_id,
                    payload: vosu_bytes {
                        data: payload.as_ptr(),
                        len: payload.len() as u32,
                    },
                };
                let dispatch = vtable
                    .dispatch
                    .expect("descriptor::check_descriptor verifies dispatch is Some before load");
                // SAFETY: `ctx` is this instance's live `plugin_ctx`;
                // `message`'s `payload` points at `payload`, which
                // outlives this call.
                let raw = unsafe { dispatch(ctx.raw(), &message as *const vosu_message) };
                vosu_status::try_from(raw).unwrap_or(vosu_status::VOSU_ERR_BACKEND)
            }
            Self::Process(endpoint) => endpoint.dispatch(MessageRef {
                opcode,
                flags,
                correlation_id,
                payload,
            }),
        }
    }
}

#[cfg(test)]
mod tests;
