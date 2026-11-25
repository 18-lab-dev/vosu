//! Level batching in the raw executor: every stage whose base is already
//! resolved is read in one native batch per dependency level, while each
//! op keeps exactly the status, bytes, order and error message the owned
//! stage-by-stage [`execute`] reports for it.

use std::sync::Mutex;

use vosu_plugin_api::Message;
use vosu_plugin_api::memory::ReadRequest;

use super::helpers::{FakeMemory, NoModules, far_deadline, op};
use super::*;

const ROOT: u64 = 0x1_0000;
const PAGE: u64 = 0x2_0000;
const HOLE: i64 = 0x10_0000;

/// [`FakeMemory`] that records every native call: the request count of
/// each `read_many`, and each `read_exact` address.
struct RecordingMemory {
    inner: FakeMemory,
    batches: Mutex<Vec<usize>>,
    singles: Mutex<Vec<usize>>,
}

impl RecordingMemory {
    fn new(regions: Vec<(usize, Vec<u8>)>) -> Self {
        Self {
            inner: FakeMemory::new(regions),
            batches: Mutex::new(Vec::new()),
            singles: Mutex::new(Vec::new()),
        }
    }

    fn batches(&self) -> Vec<usize> {
        std::mem::take(&mut *self.batches.lock().unwrap())
    }

    fn singles(&self) -> Vec<usize> {
        std::mem::take(&mut *self.singles.lock().unwrap())
    }
}

impl ProcessMemory for RecordingMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.singles.lock().unwrap().push(address);
        self.inner.read_exact(address, dst)
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        self.batches.lock().unwrap().push(reads.len());
        for read in reads {
            self.inner.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<vosu_plugin_api::memory::MemoryRegion>, MemoryError> {
        self.inner.readable_regions()
    }
}

fn root_stage(ops: Vec<PlanOp>) -> Stage {
    Stage {
        base: StageBase::Root,
        base_offset: 0,
        ops,
    }
}

fn dependent_stage(ref_stage: u32, ref_op: u32, ops: Vec<PlanOp>) -> Stage {
    Stage {
        base: StageBase::Dependent { ref_stage, ref_op },
        base_offset: 0,
        ops,
    }
}

fn submission(stages: Vec<Stage>) -> ReadPlanSubmit {
    ReadPlanSubmit {
        plan: 9,
        handle: 1,
        root: PlanBase::Absolute(ROOT),
        stages,
        sync: false,
    }
}

/// The root region holds two pointers, to `PAGE` and `PAGE + 0x100`; the
/// page holds `0, 1, 2, …` except for a pointer to `PAGE + 0x40` at 0x100.
fn memory() -> RecordingMemory {
    let mut root = vec![0u8; 0x40];
    root[..8].copy_from_slice(&PAGE.to_le_bytes());
    root[8..16].copy_from_slice(&(PAGE + 0x100).to_le_bytes());
    let mut page: Vec<u8> = (0..0x200u32).map(|byte| byte as u8).collect();
    page[0x100..0x108].copy_from_slice(&(PAGE + 0x40).to_le_bytes());
    RecordingMemory::new(vec![(ROOT as usize, root), (PAGE as usize, page)])
}

/// Runs `submit` through `execute_bytes`, asserts its decoded result equals
/// the owned [`execute`]'s, and returns it.
fn run_both(submit: &ReadPlanSubmit, memory: &RecordingMemory) -> Vec<ReadResult> {
    let message = Message::ReadPlanSubmit(submit.clone());
    let payload = message.encode_payload().unwrap();
    let raw = execute_bytes(
        &payload,
        message.flags(),
        memory,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap();
    let Message::ReadPlanResult(raw) =
        Message::decode(vosu_opcode::VOSU_OP_READ_PLAN_RESULT, 0, &raw).unwrap()
    else {
        panic!("expected a decoded ReadPlanResult");
    };
    let batches = memory.batches();
    let singles = memory.singles();
    let owned = execute(
        submit,
        memory,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap();
    assert_eq!(raw.results, owned.results);
    // Put the raw run's calls back for the caller's assertions.
    *memory.batches.lock().unwrap() = batches;
    *memory.singles.lock().unwrap() = singles;
    raw.results
}

/// Stages interleaved in submission order across three dependency levels
/// cost one native batch per level, and results keep (stage, op) order.
#[test]
fn independent_stages_share_one_native_batch_per_level() {
    let memory = memory();
    let submit = submission(vec![
        root_stage(vec![op(0, 8)]),
        dependent_stage(0, 0, vec![op(0x10, 2), op(0x20, 1)]),
        root_stage(vec![op(8, 8), op(0x30, 4)]),
        dependent_stage(2, 0, vec![op(0, 8)]),
        dependent_stage(3, 0, vec![op(4, 1)]),
        dependent_stage(0, 0, vec![op(0x40, 1)]),
    ]);
    let results = run_both(&submit, &memory);
    assert_eq!(memory.batches(), vec![3, 4, 1]);
    assert!(memory.singles().is_empty());
    assert_eq!(
        results,
        vec![
            ReadResult::Ok(PAGE.to_le_bytes().to_vec()),
            ReadResult::Ok(vec![0x10, 0x11]),
            ReadResult::Ok(vec![0x20]),
            ReadResult::Ok((PAGE + 0x100).to_le_bytes().to_vec()),
            ReadResult::Ok(vec![0; 4]),
            ReadResult::Ok((PAGE + 0x40).to_le_bytes().to_vec()),
            ReadResult::Ok(vec![0x44]),
            ReadResult::Ok(vec![0x40]),
        ]
    );
}

/// A hole inside a batched level fails that level's one batch: every op of
/// the level is then re-read on its own, so the hole reports `ReadFailed`
/// with its message, its siblings in other stages still succeed, and a
/// stage reading through the failed pointer reports the failure
/// message-less, exactly as stage-by-stage execution does.
#[test]
fn a_failed_level_batch_falls_back_per_op_keeping_each_status() {
    let memory = memory();
    let submit = submission(vec![
        root_stage(vec![op(0, 8)]),
        root_stage(vec![op(8, 8)]),
        dependent_stage(0, 0, vec![op(0x10, 8), op(HOLE, 8)]),
        dependent_stage(1, 0, vec![op(0, 2)]),
        dependent_stage(2, 1, vec![op(0, 1)]),
        dependent_stage(2, 0, vec![op(0, 1)]),
    ]);
    let results = run_both(&submit, &memory);
    // Stage 2's op 0 reads the page's bytes 0x10..0x18: as a pointer,
    // unmapped.
    let unmapped = u64::from_le_bytes([0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17]);
    // Level 0, level 1 (failed), level 2 (stage 5 only: stage 4's base
    // failed before any read; its one read fails too).
    assert_eq!(memory.batches(), vec![2, 3, 1]);
    assert_eq!(
        memory.singles(),
        vec![
            PAGE as usize + 0x10,
            PAGE as usize + HOLE as usize,
            PAGE as usize + 0x100,
            unmapped as usize,
        ]
    );
    assert_eq!(
        results[2],
        ReadResult::Ok((0x10..0x18).collect::<Vec<u8>>())
    );
    let ReadResult::Error { code, message } = &results[3] else {
        panic!("the hole must fail on its own: {:?}", results[3]);
    };
    assert_eq!(*code, ReadErrorCode::ReadFailed);
    assert!(!message.is_empty());
    assert_eq!(results[4], ReadResult::Ok(vec![0x40, 0x00]));
    assert_eq!(
        results[5],
        ReadResult::Error {
            code: ReadErrorCode::ReadFailed,
            message: String::new(),
        }
    );
    let ReadResult::Error { code, message } = &results[6] else {
        panic!("stage 5's own read must fail: {:?}", results[6]);
    };
    assert_eq!(*code, ReadErrorCode::ReadFailed);
    assert!(!message.is_empty());
}

/// Error messages are charged against the result's byte budget in
/// (stage, op) order even though levels run stages out of that order: with
/// almost no slack left, the earlier stage's message gets the room its own
/// failed read frees, not the stage that happens to run first.
#[test]
fn messages_are_charged_in_stage_order_across_levels() {
    const SLACK: usize = 20;
    let big = vosu_plugin_api::VOSU_READ_PLAN_MAX_BYTES as usize
        - RESULT_HEADER_SIZE
        - 4 * RESULT_ENTRY_SIZE
        - (8 + 1 + 1)
        - SLACK;
    let mut root = vec![0u8; 0x100 + big];
    root[..8].copy_from_slice(&PAGE.to_le_bytes());
    let memory = RecordingMemory::new(vec![(ROOT as usize, root)]);
    let submit = submission(vec![
        root_stage(vec![op(0, 8), op(0x100, big as u32)]),
        // Level 1, but stage 1: its message is charged first.
        dependent_stage(0, 0, vec![op(HOLE, 1)]),
        // Level 0, stage 2.
        root_stage(vec![op(HOLE, 1)]),
    ]);
    let results = run_both(&submit, &memory);
    let message_len = |result: &ReadResult| match result {
        ReadResult::Error { message, .. } => message.len(),
        other => panic!("expected an error, got {other:?}"),
    };
    assert_eq!(message_len(&results[2]), SLACK + 1);
    assert_eq!(message_len(&results[3]), 1);
}
