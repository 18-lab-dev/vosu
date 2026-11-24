//! Host vtable `send` entry: routes every plugin -> host message
//! (`vosu_host_services::send`) into host-owned Rust code
//! (`docs/v2/plugin-abi.md` §1, §3, §5).

use std::ffi::c_void;
use std::sync::Arc;

use vosu_plugin_api::{
    Message, VOSU_FRAME_MAX_BYTES, VOSU_PLUGIN_ABI_MAJOR, VOSU_READ_PLAN_SYNC, vosu_host_services,
    vosu_message, vosu_opcode, vosu_status,
};

/// Instance-scoped id a [`PluginHostHandler`] receives with every message,
/// so one handler implementation can serve every loaded instance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PluginInstanceId(pub(crate) u64);

impl PluginInstanceId {
    pub fn new(raw: u64) -> Self {
        Self(raw)
    }
}

/// Routes every plugin -> host `send` call into host-owned Rust code: the
/// host-side analog of `vosu_plugin_api::HostServices` on the plugin side.
pub trait PluginHostHandler: Send + Sync {
    /// Handles one decoded message from `instance`, plus the
    /// `vosu_message.correlation_id` it carried (`0` when the opcode has
    /// no reply). Returns the `vosu_status` the ABI reports back across
    /// the vtable boundary (§5).
    fn handle(
        &self,
        instance: PluginInstanceId,
        message: Message,
        correlation_id: u64,
    ) -> vosu_status;

    /// Fast path for `VOSU_OP_READ_PLAN_SUBMIT` with `VOSU_READ_PLAN_SYNC`
    /// set (D5): `payload`/`flags` are still raw, undecoded bytes.
    /// `decode_and_handle` tries this before `Message::decode` so a
    /// steady-state synchronous read plan allocates nothing on the host
    /// side either (D17; `crate::plugin::read_plan::execute_into`'s own
    /// doc comment). `None` falls back to the ordinary decode-and-`handle`
    /// path, for a handler (e.g. a test double) with no faster path of
    /// its own.
    fn handle_read_plan_sync(
        &self,
        instance: PluginInstanceId,
        payload: &[u8],
        flags: u32,
    ) -> Option<vosu_status> {
        let _ = (instance, payload, flags);
        None
    }

    /// Borrowed publication fast path; None requests ordinary owning decode.
    fn handle_publish(
        &self,
        instance: PluginInstanceId,
        opcode: vosu_opcode,
        payload: &[u8],
    ) -> Option<vosu_status> {
        let _ = (instance, opcode, payload);
        None
    }
}

/// `host_ctx` passed to `create`: outlives the instance exactly as
/// `vosu_plugin_vtable::create`'s doc comment requires, so
/// `super::instance::PluginInstance` keeps it boxed until `destroy`.
pub(crate) struct HostContext {
    instance: PluginInstanceId,
    handler: Arc<dyn PluginHostHandler>,
}

impl HostContext {
    pub(crate) fn new(instance: PluginInstanceId, handler: Arc<dyn PluginHostHandler>) -> Self {
        Self { instance, handler }
    }
}

/// The `vosu_host_services` every instance's `create` call receives: a
/// process-wide `static`, not a per-call local. `include/vosu_plugin.h`
/// documents `host` as outliving every call made through it until
/// `destroy` returns — for the instance's whole lifetime, not just
/// `create`'s own stack frame. The table's contents never change (only
/// `send`'s target, `host_ctx`, varies per instance), so one instance
/// safely serves every plugin instance ever created in this process.
pub(crate) static HOST_SERVICES: vosu_host_services = vosu_host_services {
    struct_size: core::mem::size_of::<vosu_host_services>() as u32,
    abi_version: VOSU_PLUGIN_ABI_MAJOR,
    send: Some(send_trampoline),
};

unsafe extern "C" fn send_trampoline(host_ctx: *mut c_void, message: *const vosu_message) -> u32 {
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        if host_ctx.is_null() || message.is_null() {
            return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
        }
        // SAFETY: `host_ctx` was boxed by `PluginInstance::create` and
        // stays alive until `destroy` returns, per `vosu_host_services`'s
        // own lifetime contract (`include/vosu_plugin.h`).
        let ctx = unsafe { &*host_ctx.cast::<HostContext>() };
        // SAFETY: the ABI borrows `message` for the duration of this call
        // only (`docs/v2/plugin-abi.md` §3's envelope).
        let message = unsafe { &*message };
        decode_and_handle(ctx, message)
    }));
    outcome.unwrap_or(vosu_status::VOSU_ERR_BACKEND) as u32
}

fn decode_and_handle(ctx: &HostContext, raw: &vosu_message) -> vosu_status {
    if raw.payload.len > VOSU_FRAME_MAX_BYTES {
        return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
    }
    let payload: &[u8] = if raw.payload.len == 0 {
        &[]
    } else if raw.payload.data.is_null() {
        return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
    } else {
        // SAFETY: the ABI requires `payload.data` to be valid for
        // `payload.len` bytes for the duration of this call.
        unsafe { std::slice::from_raw_parts(raw.payload.data, raw.payload.len as usize) }
    };
    let Ok(opcode) = vosu_opcode::try_from(raw.opcode) else {
        return vosu_status::VOSU_ERR_UNSUPPORTED;
    };
    if opcode == vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT
        && raw.flags & VOSU_READ_PLAN_SYNC != 0
        && let Some(status) = ctx
            .handler
            .handle_read_plan_sync(ctx.instance, payload, raw.flags)
    {
        return status;
    }
    if matches!(
        opcode,
        vosu_opcode::VOSU_OP_PUBLISH_FIELD | vosu_opcode::VOSU_OP_PUBLISH_BATCH
    ) && let Some(status) = ctx.handler.handle_publish(ctx.instance, opcode, payload)
    {
        return status;
    }
    match Message::decode(opcode, raw.flags, payload) {
        Ok(message) => ctx
            .handler
            .handle(ctx.instance, message, raw.correlation_id),
        Err(_) => vosu_status::VOSU_ERR_INVALID_ARGUMENT,
    }
}
