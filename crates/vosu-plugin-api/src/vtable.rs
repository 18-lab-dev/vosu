//! Plugin/host vtables and the external entry point
//! (`include/vosu_plugin.h:453-527`).

use core::ffi::c_void;

use crate::envelope::{vosu_bytes, vosu_message};

/// A plugin's implementation of the ABI. `struct_size` and `abi_version`
/// are always the first two fields of any future minor revision, so a host
/// can safely read up to `struct_size` bytes even when it was built against
/// an older or newer minor version; new function pointers are appended only
/// at the end and are treated as absent when a producer predates them
/// (`docs/v2/plugin-abi.md` §1).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct vosu_plugin_vtable {
    /// `sizeof(vosu_plugin_vtable)` as built
    pub struct_size: u32,
    /// must equal `VOSU_PLUGIN_ABI_MAJOR`
    pub abi_version: u32,

    /// Called once after the host validates the plugin's manifest. `init`
    /// is CBOR `{"plugin_id": string, "settings": <CBOR>, "data_dir":
    /// string}`. Returns an opaque plugin-owned context passed back on
    /// every later call, or `NULL` to fail plugin load.
    pub create: Option<
        unsafe extern "C" fn(
            host: *const vosu_host_services,
            host_ctx: *mut c_void,
            init: vosu_bytes,
        ) -> *mut c_void,
    >,

    /// The single entry point for every host -> plugin message. The return
    /// value only accepts or rejects the message itself; any business
    /// reply is a later, separate message sent through
    /// `vosu_host_services::send`. Returns the raw `vosu_status` numeric
    /// value, not the typed enum: a foreign (C) implementation of this
    /// function pointer could otherwise return a value outside
    /// `vosu_status`'s defined range, which would be immediate undefined
    /// behavior for a typed enum return. A caller decodes it with
    /// `vosu_status::try_from`, treating an unrecognized value as an
    /// error rather than UB (M4).
    pub dispatch: Option<
        unsafe extern "C" fn(plugin_ctx: *mut c_void, message: *const vosu_message) -> u32,
    >,

    /// The host is tearing this instance down. Must release every resource
    /// and return within the host's configured shutdown grace period.
    pub destroy: Option<unsafe extern "C" fn(plugin_ctx: *mut c_void)>,
}

/// Host-provided services, passed to `vosu_plugin_vtable::create`. Grows
/// the same way `vosu_plugin_vtable` does.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct vosu_host_services {
    pub struct_size: u32,
    pub abi_version: u32,

    /// Sends one plugin -> host message. `message` is borrowed for the
    /// duration of this call. A reply, when the opcode has one, arrives
    /// later through the plugin's own `dispatch`, carrying the same
    /// `correlation_id` (or, for read plans, the same `plan`) — except a
    /// `VOSU_OP_READ_PLAN_SUBMIT` carrying `VOSU_READ_PLAN_SYNC` in
    /// `vosu_message.flags` (D5), whose matching `VOSU_OP_READ_PLAN_RESULT`
    /// this call delivers itself, on this same thread, before returning.
    /// Returns the raw `vosu_status` numeric value, not the typed enum,
    /// for the same reason `dispatch` does (M4): the host implementing
    /// this may be foreign code.
    pub send: Option<
        unsafe extern "C" fn(host_ctx: *mut c_void, message: *const vosu_message) -> u32,
    >,
}

/// Entry point every external plugin dynamic library exports under exactly
/// `vosu_plugin_entry_v<VOSU_PLUGIN_ABI_MAJOR>`. A major ABI bump changes
/// the exported name, so a host never loads a struct layout it cannot
/// interpret. Bundled plugins produce the same `vosu_plugin_vtable` through
/// the Rust SDK's registration macro and export no dynamic symbol
/// (`docs/v2/plugin-abi.md` §2, §9).
pub type VosuPluginEntryFn = unsafe extern "C" fn() -> *const vosu_plugin_vtable;
