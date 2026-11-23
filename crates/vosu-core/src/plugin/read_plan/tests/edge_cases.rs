//! Precedence and scale edge cases: a dead target reported as
//! `NotReady`/`TargetGone` rather than a whole-call rejection, ref-op
//! width validation outranking target liveness, dependent-chain failures
//! staying message-less once copied, and the `READ_PLAN_MAX_BYTES` cap
//! under many failing ops. Split out of `tests.rs` so that module stays
//! under the maintained-file line limit (`docs/architecture/source-map.md`).

use vosu_plugin_api::Message;

use super::helpers::{FakeMemory, NoModules, far_deadline, op};
use super::*;

/// P1-3/P1-1: `execute` (the production-wired path, `host/read_plans.rs`)
/// reports a dead target as a per-read `VOSU_READ_NOT_READY`, never a
/// whole-call rejection — an async plan has no synchronous caller left to
/// report `VOSU_ERR_TARGET_GONE` to by the time a worker discovers this.
/// `execute_bytes` (the raw, not-yet-wired-into-production entry point)
/// still rejects outright, which this test documents as an intentional
/// divergence between the two rather than letting it bit-rot unnoticed.
#[test]
fn target_gone_is_reported_before_any_read_when_the_process_is_already_dead() {
    struct DeadMemory;
    impl ProcessMemory for DeadMemory {
        fn read_exact(&self, _address: usize, _dst: &mut [u8]) -> Result<(), MemoryError> {
            panic!("must never read a dead target")
        }
        fn read_many(
            &self,
            _reads: &mut [vosu_sys::process::ReadRequest<'_>],
        ) -> Result<(), MemoryError> {
            panic!("must never read a dead target")
        }
        fn readable_regions(&self) -> Result<Vec<vosu_sys::process::MemoryRegion>, MemoryError> {
            Ok(Vec::new())
        }
        fn is_alive(&self) -> bool {
            false
        }
    }

    let submit = ReadPlanSubmit {
        plan: 40,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 4)],
        }],
        sync: false,
    };

    let result = execute(
        &submit,
        &DeadMemory,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap();
    assert_eq!(result.plan, 40);
    assert_eq!(result.results, vec![ReadResult::NotReady]);

    let message = Message::ReadPlanSubmit(submit);
    let flags = message.flags();
    let payload = message.encode_payload().unwrap();
    let error = execute_bytes(
        &payload,
        flags,
        &DeadMemory,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ExecuteBytesError::Execute(ReadPlanError::TargetGone)
    ));
}

#[test]
fn ref_op_wrong_width_outranks_target_gone_in_both_execution_paths() {
    // A dependent stage whose `ref_op` is the wrong width *and* a dead
    // target: both `execute` and `execute_bytes` must reject with the
    // same variant. P1-3 made `ref_op` width a pure plan-validation
    // error, checked before any runtime/target-state concern (including
    // liveness) in both entry points, reversing this phase's earlier
    // precedence (F2, where liveness used to win).
    struct DeadMemory;
    impl ProcessMemory for DeadMemory {
        fn read_exact(&self, _address: usize, _dst: &mut [u8]) -> Result<(), MemoryError> {
            panic!("must never read a dead target")
        }
        fn read_many(
            &self,
            _reads: &mut [vosu_sys::process::ReadRequest<'_>],
        ) -> Result<(), MemoryError> {
            panic!("must never read a dead target")
        }
        fn readable_regions(&self) -> Result<Vec<vosu_sys::process::MemoryRegion>, MemoryError> {
            Ok(Vec::new())
        }
        fn is_alive(&self) -> bool {
            false
        }
    }

    let submit = ReadPlanSubmit {
        plan: 41,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![
            Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops: vec![op(0, 4)],
            },
            Stage {
                base: StageBase::Dependent {
                    ref_stage: 0,
                    ref_op: 0,
                },
                base_offset: 0,
                ops: vec![op(0, 4)],
            },
        ],
        sync: false,
    };

    let error = execute(
        &submit,
        &DeadMemory,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap_err();
    assert_eq!(error, ReadPlanError::RefOpWrongWidth);

    let message = Message::ReadPlanSubmit(submit);
    let flags = message.flags();
    let payload = message.encode_payload().unwrap();
    let error = execute_bytes(
        &payload,
        flags,
        &DeadMemory,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        ExecuteBytesError::Execute(ReadPlanError::RefOpWrongWidth)
    ));
}

#[test]
fn a_blocked_dependent_stage_reports_every_op_message_less() {
    // Stage 0's own read genuinely fails against an unmapped address, so
    // it carries a message; stage 1's three ops are all blocked by that
    // same failure and must each report it without repeating the message
    // (`docs/v2/plugin-abi.md` §6.3: a copied dependent-chain failure
    // "carries no message").
    let memory = FakeMemory::default();
    let submit = ReadPlanSubmit {
        plan: 60,
        handle: 1,
        root: PlanBase::Absolute(0x9000),
        stages: vec![
            Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops: vec![op(0, 8)],
            },
            Stage {
                base: StageBase::Dependent {
                    ref_stage: 0,
                    ref_op: 0,
                },
                base_offset: 0,
                ops: vec![op(0, 1), op(1, 1), op(2, 1)],
            },
        ],
        sync: false,
    };

    let result = execute(
        &submit,
        &memory,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap();

    match &result.results[0] {
        ReadResult::Error {
            code: ReadErrorCode::ReadFailed,
            message,
        } => assert!(!message.is_empty(), "the genuine failure keeps its message"),
        other => panic!("expected a ReadFailed error, got {other:?}"),
    }
    for blocked in &result.results[1..] {
        match blocked {
            ReadResult::Error {
                code: ReadErrorCode::ReadFailed,
                message,
            } => assert!(message.is_empty(), "a copied failure must carry no message"),
            other => panic!("expected a message-less ReadFailed error, got {other:?}"),
        }
    }
}

#[test]
fn many_failing_ops_stay_within_the_read_plan_max_bytes_cap() {
    const OP_COUNT: usize = 2500;
    let memory = FakeMemory::default();
    let submit = ReadPlanSubmit {
        plan: 70,
        handle: 1,
        root: PlanBase::Absolute(0x9000),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: (0..OP_COUNT as i64).map(|offset| op(offset, 1)).collect(),
        }],
        sync: false,
    };
    let message = Message::ReadPlanSubmit(submit);
    let flags = message.flags();
    let payload = message.encode_payload().unwrap();

    let result_bytes = execute_bytes(
        &payload,
        flags,
        &memory,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap();

    assert!(result_bytes.len() <= vosu_plugin_api::VOSU_READ_PLAN_MAX_BYTES as usize);
    vosu_plugin_api::validate_read_plan_result(&result_bytes).unwrap();

    let decoded = Message::decode(vosu_opcode::VOSU_OP_READ_PLAN_RESULT, 0, &result_bytes).unwrap();
    let Message::ReadPlanResult(result) = decoded else {
        panic!("expected a decoded ReadPlanResult");
    };
    assert_eq!(result.results.len(), OP_COUNT);
    assert!(result.results.iter().all(|r| matches!(
        r,
        ReadResult::Error {
            code: ReadErrorCode::ReadFailed,
            ..
        }
    )));
}
