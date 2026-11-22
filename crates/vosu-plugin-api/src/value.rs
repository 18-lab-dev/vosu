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

impl TryFrom<u32> for vosu_value_kind {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::VOSU_VALUE_NULL,
            1 => Self::VOSU_VALUE_BOOL,
            2 => Self::VOSU_VALUE_I64,
            3 => Self::VOSU_VALUE_U64,
            4 => Self::VOSU_VALUE_F64,
            5 => Self::VOSU_VALUE_STRING,
            6 => Self::VOSU_VALUE_BYTES,
            7 => Self::VOSU_VALUE_CBOR,
            8 => Self::VOSU_VALUE_RESOURCE_REF,
            9 => Self::VOSU_VALUE_TREE_REF,
            _ => return Err(()),
        })
    }
}
