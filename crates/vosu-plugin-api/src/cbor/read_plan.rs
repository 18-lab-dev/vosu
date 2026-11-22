//! `VOSU_OP_READ_PLAN_CANCEL`, CBOR (`docs/v2/plugin-abi.md` §6.3). The
//! submission and result themselves are flat (`crate::flat::read_plan`);
//! only cancellation is CBOR.

use serde::{Deserialize, Serialize};

/// `VOSU_OP_READ_PLAN_CANCEL` (plugin -> host, no reply): abandon a
/// submitted plan before its result arrives.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct ReadPlanCancel {
    pub plan: u64,
}
