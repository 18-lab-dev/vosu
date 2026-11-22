//! `VOSU_OP_PUBLISH_FIELD` / `VOSU_OP_PUBLISH_BATCH` flat payloads
//! (`include/vosu_plugin.h:241-288`).

use core::fmt;

/// The scalar payload of a `vosu_field_publish_header`, meaningful only
/// when `value_kind` is `VOSU_VALUE_BOOL`/`I64`/`U64`/`F64`
/// (`include/vosu_plugin.h:249-254`).
#[repr(C)]
#[derive(Clone, Copy)]
pub union vosu_scalar {
    pub b: u8,
    pub i64: i64,
    pub u64: u64,
    pub f64: f64,
}

impl fmt::Debug for vosu_scalar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // SAFETY: every bit pattern is a valid `u64`; this reads the union's
        // raw bits for display only, never as a typed scalar.
        let bits = unsafe { self.u64 };
        write!(f, "vosu_scalar {{ bits: {bits:#018x} }}")
    }
}

/// `VOSU_OP_PUBLISH_FIELD`: flat payload. `ns` plus `pointer` name the
/// capability id `field:<ns>.<dotted form of pointer>`; `pointer` is an
/// RFC 6901 JSON Pointer relative to the namespace root. 48 bytes
/// (`docs/v2/plugin-abi.md` §3).
#[repr(C)]
#[derive(Clone, Copy)]
pub struct vosu_field_publish_header {
    pub handle: u64,
    pub ns_offset: u32,
    pub ns_len: u32,
    pub pointer_offset: u32,
    pub pointer_len: u32,
    /// a `vosu_value_kind` value
    pub value_kind: u32,
    /// keeps `scalar` 8-byte aligned; always 0
    pub reserved: u32,
    /// meaningful for `VOSU_VALUE_BOOL`/`I64`/`U64`/`F64` only
    pub scalar: vosu_scalar,
    /// STRING/BYTES/CBOR/RESOURCE_REF/TREE_REF: trailing bytes
    pub data_offset: u32,
    pub data_len: u32,
}

impl fmt::Debug for vosu_field_publish_header {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("vosu_field_publish_header")
            .field("handle", &self.handle)
            .field("ns_offset", &self.ns_offset)
            .field("ns_len", &self.ns_len)
            .field("pointer_offset", &self.pointer_offset)
            .field("pointer_len", &self.pointer_len)
            .field("value_kind", &self.value_kind)
            .field("scalar", &self.scalar)
            .field("data_offset", &self.data_offset)
            .field("data_len", &self.data_len)
            .finish_non_exhaustive()
    }
}

/// `VOSU_OP_PUBLISH_BATCH`: flat payload (D4). One handle, a count, and
/// `entry_count` [`vosu_field_publish_header`] entries plus their trailing
/// variable-length data. 16 bytes (`docs/v2/plugin-abi.md` §3).
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct vosu_publish_batch_header {
    pub handle: u64,
    /// `vosu_field_publish_header[entry_count]`
    pub entries_offset: u32,
    pub entry_count: u32,
}
