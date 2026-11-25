//! P1's production arm, plugin side: the frozen prototype plans submitted
//! through the real SDK and `vosu plugin-host` relay to core's real broker.
//!
//! - `sync` (`production_sync`): [`PlanMemory::read_flat`], the SDK's
//!   zero-allocation `VOSU_READ_PLAN_SYNC` path every reader uses; the
//!   result is dispatched on this submitting thread before `send` returns.
//! - `async` (`production_async`): one `send_flat` without `SYNC` (it
//!   returns after core's `STATUS`), then the `READ_PLAN_RESULT` core
//!   delivers to this instance's dispatch thread, handed to this thread.
//!
//! The plans are `benches/support/brokered/abi.rs`'s ABI stages of the
//! frozen prototype plans: same reads, bytes, order and one submission
//! (precise's 8 prototype stages are 25 ABI stages; regular's 5 are 5).

use std::collections::HashMap;
use std::sync::{Condvar, Mutex};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};
use vosu_plugin_api::{
    HostServices, Message, PlanBase, PlanMemory, ReadPlanResult, ReadResult, StageRef,
    SyncReadBuffers, VtableHostServices, vosu_opcode, vosu_read_result_kind, vosu_status,
};

use crate::census::{Census, Rings};
use crate::os::Usage;
use crate::plans::{precise_plan, regular_plan};
use crate::protocol::Fields;

/// The asynchronous results this instance's dispatch thread received, by
/// plan id, for the submitting thread waiting on them.
#[derive(Default)]
pub struct Results {
    ready: Mutex<HashMap<u64, ReadPlanResult>>,
    arrived: Condvar,
}

impl Results {
    pub fn deliver(&self, result: ReadPlanResult) {
        self.ready.lock().unwrap().insert(result.plan, result);
        self.arrived.notify_all();
    }

    fn take(&self, plan: u64) -> ReadPlanResult {
        let mut ready = self.ready.lock().unwrap();
        loop {
            if let Some(result) = ready.remove(&plan) {
                return result;
            }
            let (next, timeout) = self
                .arrived
                .wait_timeout(ready, Duration::from_secs(10))
                .unwrap();
            assert!(!timeout.timed_out(), "plan {plan}'s result never arrived");
            ready = next;
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Sync,
    Async,
}

/// One shape and mode's session: its plan, buffers and measured totals.
struct Run {
    mode: Mode,
    handle: u64,
    root: u64,
    stages: Vec<StageRef<'static>>,
    plan: u64,
    buffers: SyncReadBuffers,
    submit: Vec<u8>,
    /// The first response's bytes, every later response's reference.
    expected: Vec<u8>,
    recorded: bool,
    first_sha: String,
    mismatches: u64,
    submissions: u64,
    measured: Usage,
    measured_census: Census,
    measured_round_trips: u64,
}

/// `stages` as borrowed SDK stage references; leaked once per run so they
/// are `'static` (a bench plugin starts a handful of runs).
fn stage_refs(stages: &[vosu_plugin_api::Stage]) -> Vec<StageRef<'static>> {
    stages
        .iter()
        .map(|stage| StageRef {
            base: stage.base,
            base_offset: stage.base_offset,
            ops: stage.ops.clone().leak(),
        })
        .collect()
}

impl Run {
    fn new(mode: Mode, shape: &str, handle: u64, root: u64) -> Result<Self, String> {
        let plan = match shape {
            "precise" => precise_plan(),
            "regular" => regular_plan(),
            other => return Err(format!("unknown shape {other}")),
        };
        let submission = crate::abi::submission(&plan, 0, handle, root, false);
        let stages = stage_refs(&submission.stages);
        let submit = Message::ReadPlanSubmit(submission)
            .encode_payload()
            .map_err(|error| format!("encoding the plan: {error:?}"))?;
        Ok(Self {
            mode,
            handle,
            root,
            stages,
            plan: 0,
            buffers: SyncReadBuffers::new(),
            submit,
            expected: Vec::new(),
            recorded: false,
            first_sha: String::new(),
            mismatches: 0,
            submissions: 0,
            measured: Usage::default(),
            measured_census: Census::default(),
            measured_round_trips: 0,
        })
    }

    /// One round trip; returns its wall time. Its bytes are compared with
    /// the first response after the clock stopped.
    fn round_trip(&mut self, host: &VtableHostServices, results: &Results) -> u128 {
        self.plan += 1;
        self.submissions += 1;
        let mut check = Check::default();
        let elapsed = match self.mode {
            Mode::Sync => {
                let start = Instant::now();
                let flat = PlanMemory::new()
                    .read_flat(
                        host,
                        self.plan,
                        self.handle,
                        PlanBase::Absolute(self.root),
                        &self.stages,
                        &mut self.buffers,
                    )
                    .expect("SYNC read plan");
                let elapsed = start.elapsed().as_nanos();
                for index in 0..flat.len() {
                    let (kind, bytes) = flat.get(index);
                    assert_eq!(kind, vosu_read_result_kind::VOSU_READ_OK, "read {index}");
                    check.read(&mut self.expected, self.recorded, bytes);
                }
                elapsed
            }
            Mode::Async => {
                // The plan id is the payload's first field.
                self.submit[..8].copy_from_slice(&self.plan.to_le_bytes());
                let start = Instant::now();
                let status =
                    host.send_flat(vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT, 0, 0, &self.submit);
                assert_eq!(status, vosu_status::VOSU_OK, "async submission refused");
                let result = results.take(self.plan);
                let elapsed = start.elapsed().as_nanos();
                for (index, read) in result.results.iter().enumerate() {
                    let ReadResult::Ok(bytes) = read else {
                        panic!("read {index}: {read:?}");
                    };
                    check.read(&mut self.expected, self.recorded, bytes);
                }
                elapsed
            }
        };
        if !self.recorded {
            self.recorded = true;
            self.first_sha = Sha256::digest(&self.expected)
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect();
        } else if check.mismatched || check.offset != self.expected.len() {
            self.mismatches += 1;
        }
        elapsed
    }
}

/// One response's comparison with (or, the first time, recording of) the
/// reference bytes, read by read.
#[derive(Default)]
struct Check {
    offset: usize,
    mismatched: bool,
}

impl Check {
    fn read(&mut self, expected: &mut Vec<u8>, recorded: bool, bytes: &[u8]) {
        if !recorded {
            expected.extend_from_slice(bytes);
        } else if !self.mismatched {
            self.mismatched = expected.get(self.offset..self.offset + bytes.len()) != Some(bytes);
        }
        self.offset += bytes.len();
    }
}

/// Every P1 run of this instance, by `<shape>/<mode>`.
#[derive(Default)]
pub struct P1 {
    runs: HashMap<String, Run>,
}

impl P1 {
    /// `p1`: a fresh run's reference response, warm-up and individually
    /// timed samples.
    pub fn samples(
        &mut self,
        host: &VtableHostServices,
        results: &Results,
        rings: &Rings,
        command: &Fields,
        handle: u64,
    ) -> Result<Fields, String> {
        let shape = command.text("shape");
        let mode = match command.text("mode") {
            "sync" => Mode::Sync,
            "async" => Mode::Async,
            other => return Err(format!("unknown mode {other}")),
        };
        let root = u64::from_str_radix(command.text("root"), 16).map_err(|e| e.to_string())?;
        let key = format!("{shape}/{}", command.text("mode"));
        let mut run = Run::new(mode, shape, handle, root)?;
        run.round_trip(host, results);
        for _ in 0..command.number("warmup") {
            run.round_trip(host, results);
        }
        let count = command.number("samples") as usize;
        let mut samples = Vec::with_capacity(count);
        let usage = Usage::begin();
        let census = chunked(rings, count as u64, || {
            samples.push(run.round_trip(host, results));
        })?;
        let window = Usage::end().since(usage);
        run.measured.add(window);
        run.measured_census.add(census);
        run.measured_round_trips += count as u64;
        samples.sort_unstable();
        let mean = samples.iter().sum::<u128>() as f64 / count as f64;
        let reply = Fields::new()
            .with("p50_ns", samples[count / 2])
            .with("p99_ns", samples[(count * 99 / 100).min(count - 1)])
            .with("mean_ns", format_args!("{mean:.1}"))
            .with("abi_stages", run.stages.len())
            .with(
                "reads",
                run.stages
                    .iter()
                    .map(|stage| stage.ops.len())
                    .sum::<usize>(),
            )
            .with(
                "bytes",
                run.stages
                    .iter()
                    .flat_map(|stage| stage.ops)
                    .map(|op| op.length as usize)
                    .sum::<usize>(),
            )
            .with("submissions", run.submissions)
            .with("window_round_trips", count)
            .with("window_cpu_ns", window.cpu_ns)
            .with("window_cpu_cycles", window.cpu_cycles)
            .with("window_switches", window.context_switches)
            .with("window_allocations", window.allocations);
        let reply = census_fields(reply, "window", &census);
        self.runs.insert(key, run);
        Ok(reply)
    }

    /// `p1-batch`: `iterations` back-to-back round trips, each timed alone
    /// and summed so no response check is inside the total.
    pub fn batch(
        &mut self,
        host: &VtableHostServices,
        results: &Results,
        rings: &Rings,
        command: &Fields,
    ) -> Result<Fields, String> {
        let run = self
            .runs
            .get_mut(command.text("key"))
            .ok_or("no such run")?;
        let iterations = command.number("iterations");
        let usage = Usage::begin();
        let mut elapsed = 0u128;
        let census = chunked(rings, iterations, || {
            elapsed += run.round_trip(host, results);
        })?;
        run.measured.add(Usage::end().since(usage));
        run.measured_census.add(census);
        run.measured_round_trips += iterations;
        Ok(Fields::new().with("elapsed_ns", elapsed))
    }

    /// `p1-report`: the run's totals over every measured loop (timed
    /// samples and Criterion batches, never warm-up).
    pub fn report(&self, command: &Fields) -> Result<Fields, String> {
        let run = self.runs.get(command.text("key")).ok_or("no such run")?;
        let reply = Fields::new()
            .with("sha256", &run.first_sha)
            .with("mismatches", run.mismatches)
            .with("submissions", run.submissions)
            .with("loop_round_trips", run.measured_round_trips)
            .with("loop_cpu_ns", run.measured.cpu_ns)
            .with("loop_cpu_cycles", run.measured.cpu_cycles)
            .with("loop_switches", run.measured.context_switches)
            .with("loop_allocations", run.measured.allocations);
        Ok(census_fields(reply, "loop", &run.measured_census))
    }
}

/// Round trips per census window: a ring holds 2 MiB, and one round trip
/// writes at most a few KiB, so a window never wraps past bytes the census
/// still has to read.
const CENSUS_CHUNK: u64 = 256;

/// Runs `round_trip` `count` times, counting the rings' frames every
/// [`CENSUS_CHUNK`] round trips (between round trips, outside their own
/// timing).
fn chunked(rings: &Rings, count: u64, mut round_trip: impl FnMut()) -> Result<Census, String> {
    let mut census = Census::default();
    let mut done = 0;
    while done < count {
        let cursors = rings.cursors();
        let chunk = CENSUS_CHUNK.min(count - done);
        for _ in 0..chunk {
            round_trip();
        }
        census.add(rings.since(cursors)?);
        done += chunk;
    }
    Ok(census)
}

pub fn census_fields(fields: Fields, prefix: &str, census: &Census) -> Fields {
    fields
        .with(&format!("{prefix}_wire_frames"), census.frames())
        .with(
            &format!("{prefix}_plugin_to_host_frames"),
            census.plugin_to_host,
        )
        .with(
            &format!("{prefix}_host_to_plugin_frames"),
            census.host_to_plugin,
        )
        .with(&format!("{prefix}_control_frames"), census.private)
        .with(&format!("{prefix}_submit_frames"), census.submits)
        .with(&format!("{prefix}_result_frames"), census.results)
}
