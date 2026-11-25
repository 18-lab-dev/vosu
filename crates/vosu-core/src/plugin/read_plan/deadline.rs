//! Fresh synchronous plans never wait in a queue. Their deadline bounds long
//! dependent chains and per-op retries, not a single non-cancellable native read.
//! Coarse ticks avoid QPC/HPET traps on Windows VMs; public absolute deadlines
//! retain their precise Instant semantics.
use super::ReadPlanError;
use std::{
    cell::Cell,
    time::{Duration, Instant},
};
use vosu_plugin_internal::read_plan_profile;

pub(super) enum RawDeadline {
    Absolute(Instant),
    Coarse {
        budget_ms: u64,
        start: Cell<Option<u64>>,
    },
}

#[cfg(test)]
thread_local! {
    static READS: Cell<(usize, usize)> = const { Cell::new((0, 0)) };
}
fn precise_now() -> Instant {
    #[cfg(test)]
    READS.with(|reads| {
        let (precise, coarse) = reads.get();
        reads.set((precise + 1, coarse));
    });
    read_plan_profile::precise_clock_read();
    Instant::now()
}
fn coarse_now() -> Result<u64, ReadPlanError> {
    #[cfg(test)]
    READS.with(|reads| {
        let (precise, coarse) = reads.get();
        reads.set((precise, coarse + 1));
    });
    read_plan_profile::coarse_clock_read();
    vosu_sys::process::coarse_monotonic_millis().map_err(|_| ReadPlanError::ClockUnavailable)
}

impl RawDeadline {
    pub(super) fn sync(budget: Duration) -> Self {
        // Windows ticks are ~16 ms, Linux coarse ticks typically 1–4 ms.
        // Even a test override gets at least four Windows ticks of budget.
        let budget_ms =
            u64::try_from(budget.max(Duration::from_millis(64)).as_millis()).unwrap_or(u64::MAX);
        Self::Coarse {
            budget_ms,
            start: Cell::new(None),
        }
    }
    pub(super) fn begin(&self, stages: u32) -> Result<(), ReadPlanError> {
        if let Self::Coarse { start, .. } = self
            && stages > 1
        {
            start.set(Some(coarse_now()?));
        }
        Ok(())
    }
    pub(super) fn stage_expired(&self, index: usize) -> Result<bool, ReadPlanError> {
        if index == 0 && matches!(self, Self::Coarse { .. }) {
            return Ok(false);
        }
        self.expired()
    }
    pub(super) fn expired(&self) -> Result<bool, ReadPlanError> {
        match self {
            Self::Absolute(deadline) => Ok(precise_now() >= *deadline),
            Self::Coarse { budget_ms, start } => {
                let now = coarse_now()?;
                match start.get() {
                    Some(start) => Ok(now.saturating_sub(start) >= *budget_ms),
                    None => {
                        // A one-stage success never queries time. If its native
                        // batch fails, start a budget for the retry loop now.
                        start.set(Some(now));
                        Ok(false)
                    }
                }
            }
        }
    }
}

#[cfg(test)]
pub(super) fn take_clock_reads() -> (usize, usize) {
    READS.with(|reads| reads.replace((0, 0)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_single_sync_stage_does_not_query_either_clock() {
        take_clock_reads();
        let deadline = RawDeadline::sync(Duration::from_secs(2));
        assert!(!deadline.stage_expired(0).unwrap());
        assert_eq!(take_clock_reads(), (0, 0));
    }
    #[test]
    fn multi_stage_and_fallback_checks_never_query_the_precise_clock() {
        take_clock_reads();
        let deadline = RawDeadline::sync(Duration::from_secs(2));
        deadline.begin(3).unwrap();
        assert!(!deadline.stage_expired(0).unwrap());
        assert!(!deadline.stage_expired(1).unwrap());
        assert!(!deadline.expired().unwrap());
        assert_eq!(take_clock_reads(), (0, 3));
    }
    #[test]
    fn an_expired_public_deadline_still_uses_precise_time() {
        let deadline = RawDeadline::Absolute(Instant::now() - Duration::from_secs(1));
        take_clock_reads();
        assert!(deadline.stage_expired(0).unwrap());
        assert_eq!(take_clock_reads(), (1, 0));
    }
}
