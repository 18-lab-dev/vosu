//! Optional instrumentation of the real reader threads in the M5 process example.
use std::{
    cell::Cell,
    sync::{Mutex, OnceLock},
    time::Instant,
};

use serde::{Deserialize, Serialize};
use vosu_osu_reader::memory::{
    ReaderSynchronization, StablePatterns,
    snapshot::{RawPreciseSnapshot, RawRegularSnapshot},
};
use vosu_osu_state::{
    PreciseSnapshotReader, ReadContext, RegularCaptureRequest, RegularSnapshotReader,
    StateWorkerError,
};
use vosu_plugin_api::{StageRef, memory::ReadStatus};

static PROBE: OnceLock<Probe> = OnceLock::new();

struct Probe {
    cpu: fn() -> (u64, u64),
    fixture_patterns: Option<StablePatterns>,
    lanes: [Mutex<Lane>; 2],
}

/// Enabled only by the separately built benchmark example's constructor.
pub fn install(cpu: fn() -> (u64, u64), fixture_patterns: Option<StablePatterns>) {
    assert!(
        PROBE
            .set(Probe {
                cpu,
                fixture_patterns,
                lanes: Default::default()
            })
            .is_ok(),
        "one benchmark instance per child"
    );
}

pub(crate) fn fixture_patterns() -> Option<StablePatterns> {
    PROBE.get().and_then(|probe| probe.fixture_patterns)
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Counts {
    pub submissions: u64,
    pub stages: u64,
    pub reads: u64,
    pub read_bytes: u64,
}

impl Counts {
    fn add(&mut self, other: Self) {
        self.submissions += other.submissions;
        self.stages += other.stages;
        self.reads += other.reads;
        self.read_bytes += other.read_bytes;
    }

    fn since(self, before: Self) -> Self {
        Self {
            submissions: self.submissions - before.submissions,
            stages: self.stages - before.stages,
            reads: self.reads - before.reads,
            read_bytes: self.read_bytes - before.read_bytes,
        }
    }
}

thread_local! {
    static COUNTS: Cell<Counts> = const { Cell::new(Counts { submissions: 0, stages: 0, reads: 0, read_bytes: 0 }) };
}

pub(crate) fn record_plan(stages: &[StageRef<'_>]) {
    if PROBE.get().is_some() {
        COUNTS.with(|counts| {
            let mut next = counts.get();
            next.submissions += 1;
            next.stages += stages.len() as u64;
            for stage in stages {
                next.reads += stage.ops.len() as u64;
                next.read_bytes += stage.ops.iter().map(|op| u64::from(op.length)).sum::<u64>();
            }
            counts.set(next);
        });
    }
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
pub struct Lane {
    pub ticks: u64,
    pub ready: u64,
    pub errors: u64,
    pub cpu_ns: u64,
    pub cpu_cycles: u64,
    pub wall_ns: u64,
    pub counts: Counts,
}

impl Lane {
    pub fn since(self, before: Self) -> Self {
        Self {
            ticks: self.ticks - before.ticks,
            ready: self.ready - before.ready,
            errors: self.errors - before.errors,
            cpu_ns: self.cpu_ns - before.cpu_ns,
            cpu_cycles: self.cpu_cycles - before.cpu_cycles,
            wall_ns: self.wall_ns - before.wall_ns,
            counts: self.counts.since(before.counts),
        }
    }
}

pub fn snapshot() -> [Lane; 2] {
    PROBE.get().map_or([Lane::default(); 2], |probe| {
        probe.lanes.each_ref().map(|lane| *lane.lock().unwrap())
    })
}

pub(crate) fn readers(
    regular: impl RegularSnapshotReader,
    precise: impl PreciseSnapshotReader,
) -> (impl RegularSnapshotReader, impl PreciseSnapshotReader) {
    (Observed(regular, 0), Observed(precise, 1))
}

struct Observed<T>(T, usize);

impl<T> Observed<T> {
    fn measure<V>(
        &mut self,
        work: impl FnOnce(&mut T) -> Result<V, StateWorkerError>,
        ready: impl FnOnce(&V) -> (bool, bool),
    ) -> Result<V, StateWorkerError> {
        let Some(probe) = PROBE.get() else {
            return work(&mut self.0);
        };
        COUNTS.set(Counts::default());
        let wall = Instant::now();
        let cpu = (probe.cpu)();
        let result = work(&mut self.0);
        let end_cpu = (probe.cpu)();
        let wall = wall.elapsed().as_nanos() as u64;
        let (ready, error) = result.as_ref().map_or((false, true), ready);
        let mut lane = probe.lanes[self.1].lock().unwrap();
        lane.ticks += 1;
        lane.ready += u64::from(ready);
        lane.errors += u64::from(error);
        lane.cpu_ns += end_cpu.0 - cpu.0;
        lane.cpu_cycles += end_cpu.1 - cpu.1;
        lane.wall_ns += wall;
        lane.counts.add(COUNTS.get());
        result
    }
}

impl<R: RegularSnapshotReader> RegularSnapshotReader for Observed<R> {
    fn capture_regular(
        &mut self,
        request: &RegularCaptureRequest,
    ) -> Result<std::sync::Arc<RawRegularSnapshot>, StateWorkerError> {
        self.measure(
            |reader| reader.capture_regular(request),
            |raw| {
                let global = raw.global();
                (matches!(global, Ok(ReadStatus::Ready(_))), global.is_err())
            },
        )
    }
}

impl<R: PreciseSnapshotReader> PreciseSnapshotReader for Observed<R> {
    fn capture_precise(
        &mut self,
        context: &ReadContext,
        produced_sequence: u64,
        synchronization: ReaderSynchronization,
    ) -> Result<std::sync::Arc<RawPreciseSnapshot>, StateWorkerError> {
        self.measure(
            |reader| reader.capture_precise(context, produced_sequence, synchronization),
            |raw| {
                let global = raw.global();
                (matches!(global, Ok(ReadStatus::Ready(_))), global.is_err())
            },
        )
    }
}
