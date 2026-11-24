//! Opt-in local diagnostics, not part of the wire ABI. Build with
//! `read-plan-profile` and set `VOSU_READ_PLAN_PROFILE=1` before starting.
//! Default builds compile spans out entirely. Enabled-feature, disabled-env spans
//! make no clock reads, allocations, or thread-local writes. Profiling itself
//! uses two precise reads per span, counted separately from application clocks.

#[derive(Clone, Copy, Debug)]
#[repr(usize)]
pub enum Phase {
    SdkEncode,
    SessionMemory,
    Validation,
    Backend,
    RootResolution,
    ResultDelivery,
}
impl Phase {
    pub const ALL: [Self; 6] = [
        Self::SdkEncode,
        Self::SessionMemory,
        Self::Validation,
        Self::Backend,
        Self::RootResolution,
        Self::ResultDelivery,
    ];
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Timing {
    pub calls: u64,
    pub nanos: u128,
    /// Requested backend ops, not the number of merged OS syscalls.
    pub ops: u64,
}
#[derive(Clone, Copy, Debug, Default)]
pub struct Snapshot {
    pub phases: [Timing; 6],
    pub precise_clock_reads: u64,
    pub coarse_clock_reads: u64,
    pub profiler_clock_reads: u64,
}
pub const COMPILED_IN: bool = cfg!(feature = "read-plan-profile");

#[cfg(feature = "read-plan-profile")]
mod enabled {
    use super::{Phase, Snapshot};
    use std::{cell::Cell, sync::LazyLock, time::Instant};

    static ENABLED: LazyLock<bool> = LazyLock::new(|| {
        std::env::var_os("VOSU_READ_PLAN_PROFILE").is_some_and(|value| value == "1")
    });
    thread_local! {
        static COUNTERS: Cell<Snapshot> = Cell::new(Snapshot::default());
    }
    fn update(f: impl FnOnce(&mut Snapshot)) {
        COUNTERS.with(|cell| {
            let mut snapshot = cell.get();
            f(&mut snapshot);
            cell.set(snapshot);
        });
    }
    pub fn precise_clock_read() {
        if *ENABLED {
            update(|snapshot| snapshot.precise_clock_reads += 1);
        }
    }
    pub fn coarse_clock_read() {
        if *ENABLED {
            update(|snapshot| snapshot.coarse_clock_reads += 1);
        }
    }
    pub fn take_thread_snapshot() -> Snapshot {
        COUNTERS.with(|cell| cell.replace(Snapshot::default()))
    }

    #[must_use]
    pub struct Span {
        phase: Phase,
        start: Option<Instant>,
        ops: u64,
    }
    impl Span {
        pub fn new(phase: Phase) -> Self {
            Self::start(phase, *ENABLED)
        }
        fn start(phase: Phase, enabled: bool) -> Self {
            let start = enabled.then(Instant::now);
            Self {
                phase,
                start,
                ops: 0,
            }
        }
        pub fn ops(mut self, count: usize) -> Self {
            self.ops = count as u64;
            self
        }
    }
    impl Drop for Span {
        fn drop(&mut self) {
            if let Some(start) = self.start {
                let nanos = start.elapsed().as_nanos();
                update(|snapshot| {
                    let phase = &mut snapshot.phases[self.phase as usize];
                    phase.calls += 1;
                    phase.nanos += nanos;
                    phase.ops += self.ops;
                    snapshot.profiler_clock_reads += 2;
                });
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn disabled_profile_has_no_clock_or_phase_records() {
            take_thread_snapshot();
            drop(Span::start(Phase::Backend, false).ops(3));
            let snapshot = take_thread_snapshot();
            assert_eq!(snapshot.phases[Phase::Backend as usize].calls, 0);
            assert_eq!(snapshot.profiler_clock_reads, 0);
        }
        #[test]
        fn enabled_profile_counts_ops_and_its_own_clock_cost() {
            take_thread_snapshot();
            drop(Span::start(Phase::Backend, true).ops(3));
            let snapshot = take_thread_snapshot();
            assert_eq!(snapshot.phases[Phase::Backend as usize].calls, 1);
            assert_eq!(snapshot.phases[Phase::Backend as usize].ops, 3);
            assert_eq!(snapshot.profiler_clock_reads, 2);
        }
    }
}

#[cfg(feature = "read-plan-profile")]
pub use enabled::{Span, coarse_clock_read, precise_clock_read, take_thread_snapshot};

#[cfg(not(feature = "read-plan-profile"))]
mod disabled {
    use super::{Phase, Snapshot};
    #[must_use]
    pub struct Span;
    impl Span {
        #[inline]
        pub fn new(_: Phase) -> Self {
            Self
        }
        #[inline]
        pub fn ops(self, _: usize) -> Self {
            self
        }
    }
    // Preserve the guard's explicit-drop API while optimizing to no instructions.
    impl Drop for Span {
        #[inline]
        fn drop(&mut self) {}
    }
    #[inline]
    pub fn precise_clock_read() {}
    #[inline]
    pub fn coarse_clock_read() {}
    #[inline]
    pub fn take_thread_snapshot() -> Snapshot {
        Snapshot::default()
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        #[test]
        fn production_spans_are_zero_sized_and_record_nothing() {
            let _span = Span::new(Phase::Backend).ops(3);
            precise_clock_read();
            coarse_clock_read();
            assert_eq!(std::mem::size_of::<Span>(), 0);
            let snapshot = take_thread_snapshot();
            assert_eq!(snapshot.precise_clock_reads, 0);
            assert_eq!(snapshot.coarse_clock_reads, 0);
            assert_eq!(snapshot.profiler_clock_reads, 0);
            assert_eq!(snapshot.phases[Phase::Backend as usize].calls, 0);
        }
    }
}
#[cfg(not(feature = "read-plan-profile"))]
pub use disabled::{Span, coarse_clock_read, precise_clock_read, take_thread_snapshot};
