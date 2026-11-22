//! `Plugin`/`ConcurrentPlugin`, `Init`, `PluginError`, and
//! `VtableHostServices` (`docs/v2/plugin-abi.md` §9, D22).

use std::ffi::c_void;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::cbor::CborValue;
use crate::message::{HostServices, Message};
use crate::{vosu_host_services, vosu_message, vosu_status};

/// `create`'s `init` parameter, decoded (`include/vosu_plugin.h:463`: CBOR
/// `{"plugin_id", "settings", "data_dir"}`).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Init {
    pub plugin_id: String,
    pub settings: CborValue,
    pub data_dir: PathBuf,
}

/// The error a `Plugin`/`ConcurrentPlugin` method returns; converts to the
/// `vosu_status` the ABI's `create`/`dispatch` actually reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PluginError(pub vosu_status);

impl From<vosu_status> for PluginError {
    fn from(status: vosu_status) -> Self {
        Self(status)
    }
}

/// A plugin whose `on_message` the SDK serializes behind a lock
/// (`docs/v2/plugin-abi.md` §9), safe to implement regardless of what the
/// manifest's `concurrent_dispatch` declares.
pub trait Plugin: Sized + Send {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError>;

    /// A host -> plugin message this plugin's `dispatch` received, and the
    /// `vosu_message.correlation_id` it carried (`0` when the opcode has
    /// no reply): echo it back in `HostServices::send`'s own
    /// `correlation_id` argument to answer it (B1).
    fn on_message(&mut self, message: Message, correlation_id: u64) -> Result<(), PluginError>;
}

/// A plugin whose manifest sets `concurrent_dispatch = true` (D22): the
/// host dispatches from its own worker pool instead of one dedicated
/// thread, so `on_message` takes `&self`, and this plugin's own state must
/// be `Send + Sync` and internally synchronized. A plugin implements
/// exactly one of `Plugin`/`ConcurrentPlugin`, never both.
pub trait ConcurrentPlugin: Sized + Send + Sync {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError>;

    fn on_message(&self, message: Message, correlation_id: u64) -> Result<(), PluginError>;
}

/// The one real [`HostServices`] implementation: wraps the raw
/// `*const vosu_host_services` and its `host_ctx` a `create` call received,
/// routing [`HostServices::send`] to `vosu_host_services::send` with every
/// bounds check the ABI's own growth story requires (`docs/v2/plugin-abi.md`
/// §1: never touch a field beyond the producer's own `struct_size`).
///
/// Borrowed for the lifetime of the plugin instance exactly as
/// `vosu_plugin_vtable::create`'s doc comment describes
/// (`include/vosu_plugin.h:467`): `host`/`host_ctx` outlive every call made
/// through them until `destroy` returns, so this type is `Copy` and safe to
/// store inside a plugin's own state for that whole lifetime.
#[derive(Clone, Copy)]
pub struct VtableHostServices {
    host: *const vosu_host_services,
    host_ctx: *mut c_void,
}

impl VtableHostServices {
    /// # Safety
    ///
    /// `host` must be non-null and point to a `vosu_host_services` that
    /// outlives every call made through the returned value until the
    /// plugin instance's `destroy` returns (`include/vosu_plugin.h:467`),
    /// and `host_ctx` must be the matching context `create` received
    /// alongside it.
    pub unsafe fn new(host: *const vosu_host_services, host_ctx: *mut c_void) -> Self {
        Self { host, host_ctx }
    }
}

// SAFETY: `vosu_host_services::send` is documented safe to call from any
// thread for the instance's whole lifetime (`docs/v2/plugin-abi.md` §4's
// `concurrent_dispatch` depends on exactly this); the raw pointers
// themselves carry no thread affinity.
unsafe impl Send for VtableHostServices {}
// SAFETY: see `Send` above; `send` takes `&self` on the C side (a borrowed
// `vosu_host_services`), so concurrent callers are exactly what the ABI's
// own worker-pool dispatch already requires a host to support.
unsafe impl Sync for VtableHostServices {}

impl HostServices for VtableHostServices {
    fn send(&self, message: &Message, correlation_id: u64) -> vosu_status {
        let opcode = message.opcode();
        let flags = message.flags();
        let payload = match message.encode_payload() {
            Ok(bytes) => bytes,
            Err(_) => return vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        };
        let raw = vosu_message {
            opcode: opcode as u32,
            flags,
            correlation_id,
            payload: crate::vosu_bytes {
                data: payload.as_ptr(),
                len: payload.len() as u32,
            },
        };
        // SAFETY: `self.host` was constructed by `VtableHostServices::new`
        // under that function's safety contract: non-null, outliving this
        // call. `struct_size`/`abi_version` are always the vtable's first
        // two fields regardless of minor-version growth
        // (`docs/v2/plugin-abi.md` §1), so this read is always valid for
        // any producer built against this major version; `send` is read
        // only when `struct_size` covers its offset, treating a smaller
        // producer's absent function pointer as "no services available"
        // rather than reading past what it initialized.
        unsafe {
            let host = &*self.host;
            let send_offset = core::mem::offset_of!(vosu_host_services, send);
            if (host.struct_size as usize) < send_offset + core::mem::size_of::<usize>() {
                return vosu_status::VOSU_ERR_UNSUPPORTED;
            }
            match host.send {
                // `send` returns the raw status code, not the typed enum
                // (M4): the host may be foreign (C) code, so an
                // out-of-range value must become a detectable error
                // rather than undefined behavior from reading an invalid
                // enum discriminant.
                Some(send) => {
                    vosu_status::try_from(send(self.host_ctx, &raw as *const vosu_message))
                        .unwrap_or(vosu_status::VOSU_ERR_BACKEND)
                }
                None => vosu_status::VOSU_ERR_UNSUPPORTED,
            }
        }
    }
}
