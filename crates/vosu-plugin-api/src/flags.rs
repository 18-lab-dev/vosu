//! `vosu_message::flags` bit values.

/// Set in `vosu_message.flags` on a `VOSU_OP_READ_PLAN_SUBMIT`: guarantees
/// the matching `VOSU_OP_READ_PLAN_RESULT` is dispatched back on this same
/// submitting thread before `send` returns, under a host-enforced deadline
/// (D5, `include/vosu_plugin.h:387-402`). Every other flag bit on any
/// opcode is unrecognized and rejected per `vosu_message::flags`'s rule
/// (D4, `include/vosu_plugin.h:210`).
pub const VOSU_READ_PLAN_SYNC: u32 = 1 << 0;
