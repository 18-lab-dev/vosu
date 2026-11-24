//! `vosu_opcode` (`include/vosu_plugin.h:150-206`).

/// Every opcode exchanged across the plugin/host vtable boundary. The
/// payload for opcodes 1-4 and 6 is the corresponding flat `*_header`
/// struct from `crate::flat` followed by its trailing variable-length
/// data; every other opcode's payload is CBOR
/// (`docs/v2/plugin-abi.md` §3).
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum vosu_opcode {
    /// plugin -> host, flat
    VOSU_OP_PUBLISH_FIELD = 1,
    /// host -> plugin, flat
    VOSU_OP_DEMAND_CHANGED = 2,
    /// plugin -> host, flat; `VOSU_READ_PLAN_SYNC` may be set in
    /// `vosu_message.flags` (D5)
    VOSU_OP_READ_PLAN_SUBMIT = 3,
    /// host -> plugin, flat
    VOSU_OP_READ_PLAN_RESULT = 4,
    /// plugin -> host, CBOR: abandon a submitted plan before its result
    /// arrives
    VOSU_OP_READ_PLAN_CANCEL = 5,
    /// plugin -> host, flat: atomic multi-field publish (D4)
    VOSU_OP_PUBLISH_BATCH = 6,

    /// host -> plugin, CBOR
    VOSU_OP_SESSION_ATTACHED = 10,
    /// host -> plugin, CBOR
    VOSU_OP_SESSION_DETACHED = 11,
    /// plugin -> host, CBOR
    VOSU_OP_SESSION_IDENTIFY = 12,
    /// host -> plugin, CBOR
    VOSU_OP_FOCUS_CHANGED = 13,
    /// plugin -> host, CBOR: `{"handle"}`; valid only before this plugin's
    /// own `SESSION_IDENTIFY` for the handle (D6)
    VOSU_OP_SESSION_DECLINE = 14,

    /// plugin -> host, CBOR
    VOSU_OP_SCAN_REQUEST = 20,
    /// host -> plugin, CBOR, correlated
    VOSU_OP_SCAN_RESULT = 21,

    /// host -> plugin, CBOR
    VOSU_OP_RESOURCE_RESOLVE = 30,
    /// plugin -> host, CBOR, correlated: answers `RESOURCE_RESOLVE`,
    /// `TREE_LIST`, or `TREE_LOOKUP`
    VOSU_OP_RESOURCE_RESOLVED = 31,
    /// host -> plugin, CBOR
    VOSU_OP_TREE_LIST = 32,
    /// host -> plugin, CBOR
    VOSU_OP_TREE_LOOKUP = 33,
    /// host -> plugin, CBOR
    VOSU_OP_STREAM_READ = 34,
    /// plugin -> host, CBOR, correlated
    VOSU_OP_STREAM_CHUNK = 35,
    /// plugin -> host, CBOR
    VOSU_OP_ROOT_DECLARE = 36,
    /// host -> plugin, CBOR: host no longer needs this stream
    VOSU_OP_STREAM_CLOSE = 37,

    /// host -> plugin, CBOR
    VOSU_OP_ACTION_INVOKE = 40,
    /// bidirectional, CBOR
    VOSU_OP_ACTION_RESULT = 41,
    /// bidirectional, CBOR
    VOSU_OP_ACTION_CANCEL = 42,
    /// bidirectional, CBOR
    VOSU_OP_ACTION_PROGRESS = 43,
    /// plugin -> host, CBOR: this plugin invokes another plugin's action
    VOSU_OP_ACTION_CALL = 44,

    /// host -> plugin, CBOR
    VOSU_OP_SETTINGS_CHANGED = 50,
    /// plugin -> host, CBOR
    VOSU_OP_LOG_RECORD = 51,

    /// plugin -> host, CBOR
    VOSU_OP_TIMER_SET = 60,
    /// plugin -> host, CBOR
    VOSU_OP_TIMER_CANCEL = 61,
    /// host -> plugin, CBOR
    VOSU_OP_TIMER_FIRED = 62,

    /// plugin -> host, CBOR: replaces this plugin's previous selection for
    /// the named session
    VOSU_OP_STATE_SUBSCRIBE = 70,
    /// host -> plugin, CBOR
    VOSU_OP_STATE_UPDATE = 71,
}

impl TryFrom<u32> for vosu_opcode {
    type Error = ();

    /// An opcode a receiver's minor version predates is
    /// `VOSU_ERR_UNSUPPORTED` (`docs/v2/plugin-abi.md` §5), not a decode
    /// failure; this `TryFrom` lets a dispatch loop distinguish the two.
    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(match value {
            1 => Self::VOSU_OP_PUBLISH_FIELD,
            2 => Self::VOSU_OP_DEMAND_CHANGED,
            3 => Self::VOSU_OP_READ_PLAN_SUBMIT,
            4 => Self::VOSU_OP_READ_PLAN_RESULT,
            5 => Self::VOSU_OP_READ_PLAN_CANCEL,
            6 => Self::VOSU_OP_PUBLISH_BATCH,
            10 => Self::VOSU_OP_SESSION_ATTACHED,
            11 => Self::VOSU_OP_SESSION_DETACHED,
            12 => Self::VOSU_OP_SESSION_IDENTIFY,
            13 => Self::VOSU_OP_FOCUS_CHANGED,
            14 => Self::VOSU_OP_SESSION_DECLINE,
            20 => Self::VOSU_OP_SCAN_REQUEST,
            21 => Self::VOSU_OP_SCAN_RESULT,
            30 => Self::VOSU_OP_RESOURCE_RESOLVE,
            31 => Self::VOSU_OP_RESOURCE_RESOLVED,
            32 => Self::VOSU_OP_TREE_LIST,
            33 => Self::VOSU_OP_TREE_LOOKUP,
            34 => Self::VOSU_OP_STREAM_READ,
            35 => Self::VOSU_OP_STREAM_CHUNK,
            36 => Self::VOSU_OP_ROOT_DECLARE,
            37 => Self::VOSU_OP_STREAM_CLOSE,
            40 => Self::VOSU_OP_ACTION_INVOKE,
            41 => Self::VOSU_OP_ACTION_RESULT,
            42 => Self::VOSU_OP_ACTION_CANCEL,
            43 => Self::VOSU_OP_ACTION_PROGRESS,
            44 => Self::VOSU_OP_ACTION_CALL,
            50 => Self::VOSU_OP_SETTINGS_CHANGED,
            51 => Self::VOSU_OP_LOG_RECORD,
            60 => Self::VOSU_OP_TIMER_SET,
            61 => Self::VOSU_OP_TIMER_CANCEL,
            62 => Self::VOSU_OP_TIMER_FIRED,
            70 => Self::VOSU_OP_STATE_SUBSCRIBE,
            71 => Self::VOSU_OP_STATE_UPDATE,
            _ => return Err(()),
        })
    }
}
