//! `vosu_status` (`include/vosu_plugin.h:107-121`).

/// Every `dispatch`/`send` call returns a `vosu_status`. `VOSU_OK` means the
/// message was accepted for processing, not that any requested effect
/// already completed; see "Error model" (`docs/v2/plugin-abi.md` §5) for
/// the full per-variant meaning.
#[repr(u32)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum vosu_status {
    VOSU_OK = 0,
    VOSU_ERR_UNSUPPORTED = 1,
    VOSU_ERR_INVALID_ARGUMENT = 2,
    VOSU_ERR_NOT_READY = 3,
    VOSU_ERR_PERMISSION_DENIED = 4,
    VOSU_ERR_TARGET_GONE = 5,
    VOSU_ERR_BACKEND = 6,
    /// The call answers an invocation/stream/plan that was already
    /// cancelled or closed from the other side.
    VOSU_ERR_CANCELLED = 7,
    /// The call exceeded a host-enforced deadline (an action invocation, a
    /// stream read awaiting a slow generator, or a plan execution).
    VOSU_ERR_TIMED_OUT = 8,
}

impl TryFrom<u32> for vosu_status {
    type Error = ();

    fn try_from(value: u32) -> Result<Self, Self::Error> {
        Ok(match value {
            0 => Self::VOSU_OK,
            1 => Self::VOSU_ERR_UNSUPPORTED,
            2 => Self::VOSU_ERR_INVALID_ARGUMENT,
            3 => Self::VOSU_ERR_NOT_READY,
            4 => Self::VOSU_ERR_PERMISSION_DENIED,
            5 => Self::VOSU_ERR_TARGET_GONE,
            6 => Self::VOSU_ERR_BACKEND,
            7 => Self::VOSU_ERR_CANCELLED,
            8 => Self::VOSU_ERR_TIMED_OUT,
            _ => return Err(()),
        })
    }
}
