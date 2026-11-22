//! `vosu_value_kind` (`include/vosu_plugin.h:219-230`).

/// The typed-value kind carried by `vosu_field_publish_header::value_kind`
/// (`docs/v2/plugin-abi.md` §6.6).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum vosu_value_kind {
    VOSU_VALUE_NULL = 0,
    VOSU_VALUE_BOOL = 1,
    VOSU_VALUE_I64 = 2,
    VOSU_VALUE_U64 = 3,
    VOSU_VALUE_F64 = 4,
    /// UTF-8, trailing bytes
    VOSU_VALUE_STRING = 5,
    /// opaque, trailing bytes
    VOSU_VALUE_BYTES = 6,
    /// a CBOR-encoded subtree that replaces the whole value at the
    /// pointer; the only way to publish an array or object
    VOSU_VALUE_CBOR = 7,
    /// trailing bytes are CBOR `{"kind", "key", "name", "content_id"}`
    VOSU_VALUE_RESOURCE_REF = 8,
    /// trailing bytes are CBOR `{"kind", "key", "name", "content_id"}`,
    /// same shape, resolved via `TREE_LIST`/`TREE_LOOKUP`
    VOSU_VALUE_TREE_REF = 9,
}
