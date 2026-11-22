//! `VOSU_OP_DEMAND_CHANGED` flat payload (`include/vosu_plugin.h:291-319`).

/// Sent whenever any demand for this session changes; a snapshot of the
/// session's complete current demand, never a single field's transition
/// (`docs/v2/plugin-abi.md` §6.7). 24 bytes (`docs/v2/plugin-abi.md` §3).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vosu_demand_changed_header {
    pub handle: u64,
    pub regular: u8,
    pub precise: u8,
    /// always 0
    pub reserved: [u8; 6],
    /// `vosu_pointer_ref[selector_count]`
    /// ([`crate::vosu_pointer_ref`])
    pub selectors_offset: u32,
    pub selector_count: u32,
}
