//! Buffer-size and version constants (`include/vosu_plugin.h:51-101`).

/// Major ABI version this crate implements; the symbol a plugin exports is
/// `vosu_plugin_entry_v<VOSU_PLUGIN_ABI_MAJOR>` (`include/vosu_plugin.h:47-51`).
pub const VOSU_PLUGIN_ABI_MAJOR: u32 = 1;

/// Upper bound on the encoded size of a read-plan submission and of its
/// result, in bytes (`include/vosu_plugin.h:53-60`).
pub const VOSU_READ_PLAN_MAX_BYTES: u32 = 64 * 1024;

/// Upper bound on the encoded payload size of any single `vosu_message`, in
/// either direction, for any opcode (`include/vosu_plugin.h:62-87`).
pub const VOSU_FRAME_MAX_BYTES: u32 = 1024 * 1024;

/// Upper bound on the encoded size of a `VOSU_OP_PUBLISH_BATCH` payload
/// specifically; an alias of [`VOSU_FRAME_MAX_BYTES`], not a cap of its own
/// (`include/vosu_plugin.h:90-93`, D4).
pub const VOSU_PUBLISH_BATCH_MAX_BYTES: u32 = VOSU_FRAME_MAX_BYTES;

/// Upper bound on `len` in a `VOSU_OP_STREAM_READ` request, and so on the
/// `data` a matching `VOSU_OP_STREAM_CHUNK` reply may return: strictly below
/// [`VOSU_FRAME_MAX_BYTES`] so the reply's own CBOR overhead can never push
/// the message over the universal cap (`include/vosu_plugin.h:95-101`).
pub const VOSU_STREAM_CHUNK_MAX_BYTES: u32 = VOSU_FRAME_MAX_BYTES - 64;
