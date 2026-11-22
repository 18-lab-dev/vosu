//! `vosu_pointer_ref` (`include/vosu_plugin.h:305-310`).

/// A namespace plus an RFC 6901 JSON Pointer below it, both named by
/// `{offset, len}` pairs into the owning payload buffer. 16 bytes
/// (`docs/v2/plugin-abi.md` §3).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vosu_pointer_ref {
    pub ns_offset: u32,
    pub ns_len: u32,
    /// empty selects the whole namespace
    pub pointer_offset: u32,
    pub pointer_len: u32,
}
