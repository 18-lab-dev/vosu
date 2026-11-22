//! Timers, CBOR (`docs/v2/plugin-abi.md` §6.11).

use serde::{Deserialize, Serialize};

/// `VOSU_OP_TIMER_SET` (plugin -> host). `interval_ms: None` requests a
/// one-shot fire after the host's minimum granularity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct TimerSet {
    pub timer: u64,
    #[serde(default)]
    pub interval_ms: Option<u32>,
}

/// `VOSU_OP_TIMER_FIRED` (host -> plugin, no reply).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct TimerFired {
    pub timer: u64,
}

/// `VOSU_OP_TIMER_CANCEL` (plugin -> host, no reply): stops a periodic
/// timer; a cancelled one-shot timer that already fired is a no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
pub struct TimerCancel {
    pub timer: u64,
}
