//! Message envelope (`include/vosu_plugin.h:123-213`).

/// Borrowed read-only byte view. Call-scoped: never retained past the call
/// that receives it.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vosu_bytes {
    pub data: *const u8,
    pub len: u32,
}

/// Every call across the plugin/host vtable boundary —
/// `vosu_host_services::send` (plugin -> host) and
/// `vosu_plugin_vtable::dispatch` (host -> plugin) — carries one
/// `vosu_message` (`docs/v2/plugin-abi.md` §3).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vosu_message {
    /// a `vosu_opcode` ([`crate::vosu_opcode`]) value
    pub opcode: u32,
    /// opcode-specific; 0 unless the spec says otherwise. A receiver
    /// rejects a message whose flags set any bit it does not recognize
    /// with `VOSU_ERR_INVALID_ARGUMENT` rather than ignoring it (D4).
    pub flags: u32,
    /// 0 for messages with no reply; see per-opcode notes
    pub correlation_id: u64,
    pub payload: vosu_bytes,
}
