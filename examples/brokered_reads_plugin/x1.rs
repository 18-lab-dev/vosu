//! X1's process-runtime captures, plugin side: the actual stable and lazer
//! readers decoding through `vosu_osu::memory::PlanProcessMemory` (the
//! process-runtime osu! producer's own backend) over this plugin host's
//! real relay to core's broker, which executes against the synthetic
//! session's fixture memory. The capture calls are the `plan_memory`
//! cases' own (`benches/support/plan_process_cases.rs`).
//!
//! Each case keeps two readers on the same session: `timed` submits
//! through the plain SDK host services, exactly as the producer does;
//! `counted` goes through [`Counted`], which parses every submitted plan
//! before forwarding it unchanged, for the per-capture census. Frames are
//! counted from the rings themselves ([`crate::census`]).

use std::collections::HashMap;
use std::hint::black_box;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use vosu_osu::memory::PlanProcessMemory;
use vosu_osu_reader::memory::lazer_offsets::LazerOffsets;
use vosu_osu_reader::memory::{LazerMemory, LazerPattern, ReaderSynchronization, StableMemory};
use vosu_osu_reader::process::{ProcessHandle, ScanBackend};
use vosu_plugin_api::memory::{
    BatchScanResult, MemoryError, MemoryRegion, Pattern, ProcessBitness, ProcessMemory, ReadRequest,
};
use vosu_plugin_api::{HostServices, Message, VtableHostServices, vosu_opcode, vosu_status};

use crate::cases::{self, Case};
use crate::census::{Census, Rings};
use crate::protocol::Fields;

/// What one reader's submissions carried.
#[derive(Default)]
pub struct Counters {
    submissions: AtomicU64,
    stages: AtomicU64,
    reads: AtomicU64,
    read_bytes: AtomicU64,
    other_sends: AtomicU64,
    scans: AtomicU64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Snapshot {
    submissions: u64,
    stages: u64,
    reads: u64,
    read_bytes: u64,
    other_sends: u64,
    scans: u64,
}

impl Counters {
    fn snapshot(&self) -> Snapshot {
        Snapshot {
            submissions: self.submissions.load(Ordering::Relaxed),
            stages: self.stages.load(Ordering::Relaxed),
            reads: self.reads.load(Ordering::Relaxed),
            read_bytes: self.read_bytes.load(Ordering::Relaxed),
            other_sends: self.other_sends.load(Ordering::Relaxed),
            scans: self.scans.load(Ordering::Relaxed),
        }
    }

    /// Adds one `VOSU_OP_READ_PLAN_SUBMIT` payload's shape
    /// (`vosu_read_plan_submit_header`: `stages_offset` at 48,
    /// `stage_count` at 52; `vosu_stage_header`: 32 bytes, `ops_offset` at
    /// 24, `op_count` at 28; `vosu_plan_op`: 16 bytes, `length` at 8).
    fn plan(&self, payload: &[u8]) {
        let word = |at: usize| {
            u32::from_le_bytes(payload[at..at + 4].try_into().expect("in-bounds word")) as usize
        };
        let (stages_offset, stage_count) = (word(48), word(52));
        let mut reads = 0;
        let mut bytes = 0;
        for stage in 0..stage_count {
            let header = stages_offset + stage * 32;
            let (ops_offset, op_count) = (word(header + 24), word(header + 28));
            reads += op_count;
            for op in 0..op_count {
                bytes += word(ops_offset + op * 16 + 8);
            }
        }
        self.submissions.fetch_add(1, Ordering::Relaxed);
        self.stages.fetch_add(stage_count as u64, Ordering::Relaxed);
        self.reads.fetch_add(reads as u64, Ordering::Relaxed);
        self.read_bytes.fetch_add(bytes as u64, Ordering::Relaxed);
    }
}

/// The SDK host services, counting what passes through unchanged.
pub struct Counted {
    host: VtableHostServices,
    counters: Arc<Counters>,
}

impl HostServices for Counted {
    fn send(&self, message: &Message, correlation_id: u64) -> vosu_status {
        self.counters.other_sends.fetch_add(1, Ordering::Relaxed);
        self.host.send(message, correlation_id)
    }

    fn send_flat(
        &self,
        opcode: vosu_opcode,
        flags: u32,
        correlation_id: u64,
        payload: &[u8],
    ) -> vosu_status {
        if opcode == vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT {
            self.counters.plan(payload);
        } else {
            self.counters.other_sends.fetch_add(1, Ordering::Relaxed);
        }
        self.host.send_flat(opcode, flags, correlation_id, payload)
    }
}

/// The plugin's own zero-filled memory for the region-walk scan backend
/// the `_plan` cases use (`benches/support/region_walk_scans.rs`): scans
/// are not brokered in these cases, in any transport.
struct Zeros(Vec<u8>);

impl ProcessMemory for Zeros {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let source = address
            .checked_add(dst.len())
            .and_then(|end| self.0.get(address..end))
            .ok_or_else(|| MemoryError::Backend {
                operation: "bench",
                message: "outside fixture".into(),
            })?;
        dst.copy_from_slice(source);
        Ok(())
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0,
            size: self.0.len(),
        }])
    }
}

struct FixtureScan {
    handle: ProcessHandle,
    counters: Arc<Counters>,
}

impl ScanBackend for FixtureScan {
    fn scan_all(&self, pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
        self.counters.scans.fetch_add(1, Ordering::Relaxed);
        self.handle.scan_all(pattern)
    }

    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        self.counters.scans.fetch_add(1, Ordering::Relaxed);
        self.handle.scan_batch(patterns)
    }
}

enum Reader {
    Stable(Box<StableMemory>),
    Lazer(Box<LazerMemory>),
}

struct CaseState {
    case: Case,
    timed: Reader,
    counted: Reader,
    counters: Arc<Counters>,
    synchronization: ReaderSynchronization,
}

impl CaseState {
    /// One capture of `case` with `reader`; its digest when `digest`.
    fn capture(
        case: Case,
        reader: &mut Reader,
        synchronization: ReaderSynchronization,
        digest: bool,
    ) -> Result<Option<String>, String> {
        let finish = |capture: &dyn std::fmt::Debug| digest.then(|| cases::digest(&capture));
        Ok(match (case, reader) {
            (Case::StablePrecise, Reader::Stable(reader)) => {
                finish(&black_box(cases::stable_precise(reader)?))
            }
            (Case::StableRegularFields, Reader::Stable(reader)) => {
                finish(&black_box(cases::stable_regular_fields(reader)))
            }
            (Case::LazerPrecise, Reader::Lazer(reader)) => {
                finish(&black_box(cases::lazer_precise(reader, synchronization)?))
            }
            (Case::LazerRegularFields, Reader::Lazer(reader)) => {
                finish(&black_box(cases::lazer_regular_fields(reader)))
            }
            _ => unreachable!("each case is built with its own reader kind"),
        })
    }
}

/// The process-runtime capture cases of this instance.
#[derive(Default)]
pub struct X1 {
    cases: HashMap<Case, CaseState>,
}

fn case(command: &Fields) -> Result<Case, String> {
    let name = command.text("case");
    Case::parse(name).ok_or_else(|| format!("unknown case {name}"))
}

fn address(command: &Fields, key: &str) -> Result<usize, String> {
    usize::from_str_radix(command.text(key), 16).map_err(|error| format!("{key}: {error}"))
}

impl X1 {
    /// `x1-setup`: builds both readers of `case` on the session `handle`
    /// and returns the digest of each reader's first capture, which must
    /// agree.
    pub fn setup(
        &mut self,
        host: VtableHostServices,
        command: &Fields,
        handle: u64,
        pid: u32,
        bitness: ProcessBitness,
    ) -> Result<Fields, String> {
        let case = case(command)?;
        let counters = Arc::new(Counters::default());
        let synchronization = if case == Case::LazerPrecise {
            ReaderSynchronization {
                game_base: Some(address(command, "game_base")?),
                current_screen: Some(address(command, "player")?),
            }
        } else {
            ReaderSynchronization::default()
        };
        let offsets = || {
            LazerOffsets::embedded()
                .map(Arc::new)
                .map_err(|error| format!("embedded lazer offsets: {error}"))
        };
        let scans = |counters: &Arc<Counters>| {
            Arc::new(FixtureScan {
                handle: crate::region_walk_scans::scanning_handle(
                    Arc::new(Zeros(vec![0; 64 * 1024])),
                    bitness,
                ),
                counters: Arc::clone(counters),
            })
        };
        let build = |memory: Arc<dyn ProcessMemory>| -> Result<Reader, String> {
            Ok(match case {
                Case::StablePrecise | Case::StableRegularFields => {
                    Reader::Stable(Box::new(StableMemory::new(
                        ProcessHandle::with_scan_backend(memory, bitness, scans(&counters)),
                        crate::stable_fixture::patterns(),
                    )))
                }
                Case::LazerPrecise => Reader::Lazer(Box::new(LazerMemory::new(
                    ProcessHandle::new(memory, bitness),
                    offsets()?,
                    LazerPattern::source_default(),
                ))),
                Case::LazerRegularFields => {
                    let mut reader = LazerMemory::new(
                        ProcessHandle::with_scan_backend(memory, bitness, scans(&counters)),
                        offsets()?,
                        LazerPattern::source_default(),
                    );
                    reader.set_resolved_state(0x1000, 0x2000);
                    Reader::Lazer(Box::new(reader))
                }
            })
        };
        let mut timed = build(Arc::new(PlanProcessMemory::new(host, handle, pid, bitness)))?;
        let mut counted = build(Arc::new(PlanProcessMemory::new(
            Counted {
                host,
                counters: Arc::clone(&counters),
            },
            handle,
            pid,
            bitness,
        )))?;
        let timed_digest = CaseState::capture(case, &mut timed, synchronization, true)?;
        let counted_digest = CaseState::capture(case, &mut counted, synchronization, true)?;
        if timed_digest != counted_digest {
            return Err(format!(
                "{}: the counted reader decoded differently",
                case.name()
            ));
        }
        self.cases.insert(
            case,
            CaseState {
                case,
                timed,
                counted,
                counters,
                synchronization,
            },
        );
        Ok(Fields::new().with("digest", timed_digest.expect("digest requested")))
    }

    /// `x1-census`: `captures` captures of the counted reader, each with
    /// its own counter and ring-cursor window; totals plus the per-capture
    /// minimum and maximum of every count.
    pub fn census(&mut self, rings: &Rings, command: &Fields) -> Result<Fields, String> {
        let state = self
            .cases
            .get_mut(&case(command)?)
            .ok_or("case not set up")?;
        let captures = command.number("captures");
        let mut per_capture = Vec::new();
        for _ in 0..captures {
            let before = state.counters.snapshot();
            let cursors = rings.cursors();
            CaseState::capture(state.case, &mut state.counted, state.synchronization, false)?;
            let census = rings.since(cursors)?;
            let after = state.counters.snapshot();
            per_capture.push((
                Snapshot {
                    submissions: after.submissions - before.submissions,
                    stages: after.stages - before.stages,
                    reads: after.reads - before.reads,
                    read_bytes: after.read_bytes - before.read_bytes,
                    other_sends: after.other_sends - before.other_sends,
                    scans: after.scans - before.scans,
                },
                census,
            ));
        }
        let mut reply = Fields::new().with("captures", captures);
        let mut metric = |name: &str, value: fn(&(Snapshot, Census)) -> u64| {
            let values = per_capture.iter().map(value);
            reply.set(&format!("{name}_total"), values.clone().sum::<u64>());
            reply.set(&format!("{name}_min"), values.clone().min().unwrap_or(0));
            reply.set(&format!("{name}_max"), values.max().unwrap_or(0));
        };
        metric("submissions", |(counts, _)| counts.submissions);
        metric("stages", |(counts, _)| counts.stages);
        metric("reads", |(counts, _)| counts.reads);
        metric("read_bytes", |(counts, _)| counts.read_bytes);
        metric("other_sends", |(counts, _)| counts.other_sends);
        metric("scans", |(counts, _)| counts.scans);
        metric("wire_frames", |(_, census)| census.frames());
        metric("control_frames", |(_, census)| census.private);
        metric("submit_frames", |(_, census)| census.submits);
        metric("result_frames", |(_, census)| census.results);
        Ok(reply)
    }

    /// `x1-batch`: `iterations` captures of the timed reader, timed
    /// together (one Criterion `iter_custom` batch).
    pub fn batch(&mut self, command: &Fields) -> Result<Fields, String> {
        let state = self
            .cases
            .get_mut(&case(command)?)
            .ok_or("case not set up")?;
        let iterations = command.number("iterations");
        let started = Instant::now();
        for _ in 0..iterations {
            CaseState::capture(state.case, &mut state.timed, state.synchronization, false)?;
        }
        Ok(Fields::new().with("elapsed_ns", started.elapsed().as_nanos()))
    }
}
