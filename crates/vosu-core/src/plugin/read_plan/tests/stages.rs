//! `execute`/`execute_bytes` over absolute and module-relative roots,
//! single- and multi-stage/dependent plans, and their per-op error
//! reporting (ref-op width, memory errors, address overflow, deadline).
//! Split out of `tests.rs` so that module stays under the maintained-file
//! line limit (`docs/architecture/source-map.md`).

use std::time::{Duration, Instant};

use vosu_plugin_api::Message;

use super::helpers::{
    FakeMemory, Modules, NoModules, PermissionDeniedMemory, SlowMemory, far_deadline, op,
};
use super::*;

#[test]
fn single_stage_absolute_root_reads_every_op() {
    let memory = FakeMemory::new(vec![(0x1000, vec![1, 2, 3, 4, 5, 6, 7, 8])]);
    let submit = ReadPlanSubmit {
        plan: 1,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 4), op(4, 4)],
        }],
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

    assert_eq!(result.plan, 1);
    assert_eq!(
        result.results,
        vec![
            ReadResult::Ok(vec![1, 2, 3, 4]),
            ReadResult::Ok(vec![5, 6, 7, 8]),
        ]
    );
}

#[test]
fn module_root_resolves_through_module_resolver() {
    let memory = FakeMemory::new(vec![(0x2010, vec![0xaa])]);
    let submit = ReadPlanSubmit {
        plan: 2,
        handle: 1,
        root: PlanBase::Module {
            name: "osu!.exe".to_owned(),
            offset: 0x10,
        },
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 1)],
        }],
        sync: false,
    };

    let result = execute(
        &submit,
        &memory,
        ProcessBitness::X64,
        &Modules(vec![("osu!.exe", 0x2000)]),
        far_deadline(),
    )
    .unwrap();

    assert_eq!(result.results, vec![ReadResult::Ok(vec![0xaa])]);
}

#[test]
fn unresolved_module_reports_not_ready_for_every_dependent_op() {
    let memory = FakeMemory::default();
    let submit = ReadPlanSubmit {
        plan: 3,
        handle: 1,
        root: PlanBase::Module {
            name: "missing.exe".to_owned(),
            offset: 0,
        },
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
                ops: vec![op(0, 4)],
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

    assert_eq!(
        result.results,
        vec![ReadResult::NotReady, ReadResult::NotReady]
    );
}

#[test]
fn dependent_stage_chases_the_pointer_an_earlier_stage_read() {
    // stage 0 reads an 8-byte pointer at 0x1000 pointing at 0x3000; stage 1
    // reads 4 bytes at 0x3000 + 8.
    let mut pointer_bytes = 0x3000u64.to_le_bytes().to_vec();
    pointer_bytes.extend([0u8; 8]);
    let memory = FakeMemory::new(vec![
        (0x1000, pointer_bytes),
        (0x3000, vec![0; 8].into_iter().chain([9, 9, 9, 9]).collect()),
    ]);
    let submit = ReadPlanSubmit {
        plan: 4,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
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
                ops: vec![op(8, 4)],
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

    assert_eq!(
        result.results,
        vec![
            ReadResult::Ok(0x3000u64.to_le_bytes().to_vec()),
            ReadResult::Ok(vec![9, 9, 9, 9]),
        ]
    );
}

#[test]
fn ref_op_wrong_width_is_rejected_before_any_read() {
    let memory = FakeMemory::new(vec![(0x1000, vec![0; 8])]);
    let submit = ReadPlanSubmit {
        plan: 5,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![
            Stage {
                base: StageBase::Root,
                base_offset: 0,
                // X64 pointer width is 8; this op only reads 4 bytes.
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
        &memory,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap_err();

    assert_eq!(error, ReadPlanError::RefOpWrongWidth);
    assert_eq!(
        vosu_status::from(error),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
}

#[test]
fn memory_errors_map_to_their_read_error_code() {
    let memory = FakeMemory::default();
    let submit = ReadPlanSubmit {
        plan: 6,
        handle: 1,
        root: PlanBase::Absolute(0x9000),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 4)],
        }],
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

    assert!(matches!(
        &result.results[0],
        ReadResult::Error {
            code: ReadErrorCode::ReadFailed,
            ..
        }
    ));

    let denied = PermissionDeniedMemory;
    let result = execute(
        &submit,
        &denied,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap();
    assert!(matches!(
        &result.results[0],
        ReadResult::Error {
            code: ReadErrorCode::PermissionDenied,
            ..
        }
    ));
}

#[test]
fn address_overflow_is_reported_per_op_not_as_a_call_rejection() {
    let memory = FakeMemory::default();
    let submit = ReadPlanSubmit {
        plan: 7,
        handle: 1,
        root: PlanBase::Absolute(u64::MAX - 1),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(10, 4)],
        }],
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

    assert!(matches!(
        &result.results[0],
        ReadResult::Error {
            code: ReadErrorCode::AddressOverflow,
            ..
        }
    ));
}

/// P1-3: a deadline exceeded mid-plan does not reject the whole call —
/// the stage that already finished keeps its result, and the stage that
/// did not get to run reports `VOSU_READ_ERROR`/`VOSU_READ_ERROR_BACKEND`
/// ("plan deadline exceeded") per op instead, since an async plan has no
/// synchronous caller left to report a whole-call rejection to by the
/// time a worker discovers this.
#[test]
fn execute_times_out_before_the_next_read() {
    // The deadline is checked once per stage, not once per op: two
    // single-op stages mean the first stage's slow read consumes the
    // whole deadline, and the second stage's check (before any read of
    // its own) is where the timeout is observed.
    let memory = SlowMemory;
    let submit = ReadPlanSubmit {
        plan: 8,
        handle: 1,
        root: PlanBase::Absolute(0),
        stages: vec![
            Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops: vec![op(0, 1)],
            },
            Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops: vec![op(0, 1)],
            },
        ],
        sync: false,
    };

    let deadline = Instant::now() + Duration::from_millis(10);
    let result = execute(&submit, &memory, ProcessBitness::X64, &NoModules, deadline).unwrap();

    assert_eq!(result.results.len(), 2);
    assert!(matches!(result.results[0], ReadResult::Ok(_)));
    assert_eq!(
        result.results[1],
        ReadResult::Error {
            code: ReadErrorCode::Backend,
            message: "plan deadline exceeded".to_owned(),
        }
    );
}

#[test]
fn execute_bytes_round_trips_through_message_decode_and_encode() {
    let memory = FakeMemory::new(vec![(0x1000, vec![1, 2, 3, 4])]);
    let submit = ReadPlanSubmit {
        plan: 9,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 4)],
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

    let decoded = Message::decode(vosu_opcode::VOSU_OP_READ_PLAN_RESULT, 0, &result_bytes).unwrap();
    let Message::ReadPlanResult(result) = decoded else {
        panic!("expected a decoded ReadPlanResult");
    };
    assert_eq!(result.plan, 9);
    assert_eq!(result.results, vec![ReadResult::Ok(vec![1, 2, 3, 4])]);
}
