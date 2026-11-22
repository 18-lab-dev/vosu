//! Opaque handle aliases (`include/vosu_plugin.h:130-137`).
//!
//! Each is an integer scoped to one plugin instance; it is never a pointer
//! and carries no meaning outside the vtable call pair that minted and
//! consumed it.

/// A session's `handle`; see "Session identity" in the spec.
pub type vosu_session_id = u64;
pub type vosu_plan_id = u64;
pub type vosu_timer_id = u64;
/// Caller-assigned (`ACTION_CALL`) or host-assigned (`ACTION_INVOKE`);
/// never reused within one plugin instance's lifetime.
pub type vosu_invocation_id = u64;
/// An open bounded byte stream, minted by the plugin.
pub type vosu_resource_id = u64;
