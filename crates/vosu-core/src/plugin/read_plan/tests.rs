use std::sync::Mutex;
use std::thread;
use std::time::{Duration, Instant};

use vosu_plugin_api::Message;

use super::*;

#[derive(Debug, Default)]
struct FakeMemory {
    regions: Mutex<Vec<(usize, Vec<u8>)>>,
}

impl FakeMemory {
    fn new(regions: Vec<(usize, Vec<u8>)>) -> Self {
        Self {
            regions: Mutex::new(regions),
        }
    }
}

impl ProcessMemory for FakeMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let regions = self.regions.lock().unwrap();
        for (base, bytes) in regions.iter() {
            let Some(offset) = address.checked_sub(*base) else {
                continue;
            };
            if offset <= bytes.len() && dst.len() <= bytes.len() - offset {
                dst.copy_from_slice(&bytes[offset..offset + dst.len()]);
                return Ok(());
            }
        }
        Err(MemoryError::Read {
            address,
            len: dst.len(),
            message: "unmapped test address".to_owned(),
        })
    }

    fn read_many(
        &self,
        reads: &mut [vosu_sys::process::ReadRequest<'_>],
    ) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<vosu_sys::process::MemoryRegion>, MemoryError> {
        Ok(self
            .regions
            .lock()
            .unwrap()
            .iter()
            .map(|(address, bytes)| vosu_sys::process::MemoryRegion {
                address: *address,
                size: bytes.len(),
            })
            .collect())
    }
}

struct PermissionDeniedMemory;

impl ProcessMemory for PermissionDeniedMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        Err(MemoryError::PermissionDenied {
            address,
            len: dst.len(),
            message: "denied".to_owned(),
        })
    }

    fn read_many(
        &self,
        reads: &mut [vosu_sys::process::ReadRequest<'_>],
    ) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<vosu_sys::process::MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

/// A single-threaded read that sleeps past the deadline before returning,
/// so [`execute`] observes a timeout on its next deadline check.
struct SlowMemory;

impl ProcessMemory for SlowMemory {
    fn read_exact(&self, _address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        std::thread::sleep(Duration::from_millis(20));
        dst.fill(0);
        Ok(())
    }

    fn read_many(
        &self,
        reads: &mut [vosu_sys::process::ReadRequest<'_>],
    ) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<vosu_sys::process::MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

struct Modules(Vec<(&'static str, u64)>);

impl ModuleResolver for Modules {
    fn resolve(&self, module: &str) -> Option<u64> {
        self.0
            .iter()
            .find(|(name, _)| *name == module)
            .map(|(_, base)| *base)
    }
}

struct NoModules;

impl ModuleResolver for NoModules {
    fn resolve(&self, _module: &str) -> Option<u64> {
        None
    }
}

fn op(offset: i64, length: u32) -> PlanOp {
    PlanOp { offset, length }
}

fn far_deadline() -> Instant {
    Instant::now() + Duration::from_secs(60)
}

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
    let error = execute(&submit, &memory, ProcessBitness::X64, &NoModules, deadline).unwrap_err();

    assert_eq!(error, ReadPlanError::TimedOut);
    assert_eq!(vosu_status::from(error), vosu_status::VOSU_ERR_TIMED_OUT);
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

#[test]
fn deliver_sync_refuses_on_a_dispatch_thread() {
    let memory = FakeMemory::new(vec![(0x1000, vec![0; 4])]);
    let submit = ReadPlanSubmit {
        plan: 10,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 4)],
        }],
        sync: true,
    };

    let _scope = DispatchThreadScope::enter();
    assert!(on_dispatch_thread());
    let error = deliver_sync(
        &submit,
        &memory,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap_err();
    assert_eq!(error, SyncReadPlanError::OnDispatchThread);
    assert_eq!(
        vosu_status::from(error),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
}

#[test]
fn deliver_sync_succeeds_off_the_dispatch_thread() {
    assert!(!on_dispatch_thread());
    let memory = FakeMemory::new(vec![(0x1000, vec![7, 7, 7, 7])]);
    let submit = ReadPlanSubmit {
        plan: 11,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 4)],
        }],
        sync: true,
    };

    let result = deliver_sync(
        &submit,
        &memory,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap();
    assert_eq!(result.results, vec![ReadResult::Ok(vec![7, 7, 7, 7])]);
}

#[test]
fn dispatch_thread_scope_is_thread_local_and_times_out_on_the_caller() {
    // A sync submission on a worker thread not wearing the scope still
    // honors its own deadline and reports VOSU_ERR_TIMED_OUT rather than
    // blocking forever, even while some other thread on this process is
    // inside a DispatchThreadScope of its own.
    let handle = thread::spawn(|| {
        let _scope = DispatchThreadScope::enter();
        thread::sleep(Duration::from_millis(50));
    });

    let memory = SlowMemory;
    let submit = ReadPlanSubmit {
        plan: 12,
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
        sync: true,
    };
    assert!(!on_dispatch_thread());
    let deadline = Instant::now() + Duration::from_millis(5);
    let error =
        deliver_sync(&submit, &memory, ProcessBitness::X64, &NoModules, deadline).unwrap_err();
    assert_eq!(error, SyncReadPlanError::Execute(ReadPlanError::TimedOut));

    handle.join().unwrap();
}

#[test]
fn outstanding_plans_enforces_the_cap_and_cancel_finish_lifecycle() {
    let outstanding = OutstandingPlans::new();
    for plan in 0..MAX_OUTSTANDING_ASYNC_PLANS as u64 {
        outstanding.begin(plan, 1).unwrap();
    }
    assert_eq!(outstanding.len(), MAX_OUTSTANDING_ASYNC_PLANS);
    assert_eq!(
        outstanding.begin(MAX_OUTSTANDING_ASYNC_PLANS as u64, 1),
        Err(OutstandingPlansError::CapExceeded)
    );

    assert_eq!(outstanding.finish(0), FinishOutcome::Deliver);
    assert_eq!(outstanding.len(), MAX_OUTSTANDING_ASYNC_PLANS - 1);
    // Room again after one finishes.
    outstanding
        .begin(MAX_OUTSTANDING_ASYNC_PLANS as u64, 1)
        .unwrap();

    outstanding.cancel(1);
    assert_eq!(outstanding.finish(1), FinishOutcome::Cancelled);
    // A plan id never admitted (already finished, or unknown) is a no-op.
    outstanding.cancel(999);
    assert_eq!(outstanding.finish(999), FinishOutcome::Deliver);
}

#[test]
fn cancel_frees_the_slot_immediately_rather_than_waiting_for_finish() {
    let outstanding = OutstandingPlans::new();
    for plan in 0..MAX_OUTSTANDING_ASYNC_PLANS as u64 {
        outstanding.begin(plan, 1).unwrap();
    }
    assert_eq!(outstanding.len(), MAX_OUTSTANDING_ASYNC_PLANS);

    // Cancelling must free the slot right away, before `finish` ever runs
    // for it (a plugin that cancels slow plans is not still refused).
    outstanding.cancel(0);
    assert_eq!(outstanding.len(), MAX_OUTSTANDING_ASYNC_PLANS - 1);
    outstanding
        .begin(MAX_OUTSTANDING_ASYNC_PLANS as u64, 1)
        .unwrap();
    assert_eq!(outstanding.len(), MAX_OUTSTANDING_ASYNC_PLANS);

    // The cancelled id's late result is still reported Cancelled, not
    // silently treated as never-admitted.
    assert_eq!(outstanding.finish(0), FinishOutcome::Cancelled);
}

#[test]
fn begin_refuses_an_id_already_outstanding() {
    let outstanding = OutstandingPlans::new();
    outstanding.begin(5, 1).unwrap();
    assert_eq!(
        outstanding.begin(5, 1),
        Err(OutstandingPlansError::AlreadyOutstanding)
    );
    assert_eq!(
        vosu_status::from(OutstandingPlansError::AlreadyOutstanding),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    assert_eq!(outstanding.len(), 1);

    // Still refused while cancelled-but-not-yet-finished: re-submitting a
    // cancelled id's old id must not resurrect its stale result.
    outstanding.cancel(5);
    assert_eq!(
        outstanding.begin(5, 1),
        Err(OutstandingPlansError::AlreadyOutstanding)
    );
    outstanding.finish(5);
    outstanding.begin(5, 1).unwrap();
}

#[test]
fn cancel_session_cancels_only_that_sessions_plans() {
    let outstanding = OutstandingPlans::new();
    outstanding.begin(1, 10).unwrap();
    outstanding.begin(2, 10).unwrap();
    outstanding.begin(3, 20).unwrap();
    assert_eq!(outstanding.len(), 3);

    outstanding.cancel_session(10);
    assert_eq!(outstanding.len(), 1);
    assert_eq!(outstanding.finish(1), FinishOutcome::Cancelled);
    assert_eq!(outstanding.finish(2), FinishOutcome::Cancelled);
    assert_eq!(outstanding.finish(3), FinishOutcome::Deliver);
}

#[cfg(target_os = "linux")]
#[test]
fn execute_reads_this_process_own_memory_through_the_native_backend() {
    use vosu_sys::process::{open_process, process_bitness};

    let marker: Box<u64> = Box::new(0x1122_3344_5566_7788);
    let address = std::ptr::from_ref::<u64>(&marker) as u64;
    let pid = std::process::id();
    let memory = open_process(pid).unwrap();
    let bitness = process_bitness(pid).unwrap();

    let submit = ReadPlanSubmit {
        plan: 13,
        handle: 1,
        root: PlanBase::Absolute(address),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 8)],
        }],
        sync: false,
    };

    let result = execute(&submit, &memory, bitness, &NoModules, far_deadline()).unwrap();
    assert_eq!(
        result.results,
        vec![ReadResult::Ok(marker.to_le_bytes().to_vec())]
    );
}

#[cfg(windows)]
#[test]
fn execute_reads_this_process_own_memory_through_the_native_backend() {
    use vosu_sys::process::WindowsProcessMemory;

    let marker: Box<u64> = Box::new(0x1122_3344_5566_7788);
    let address = std::ptr::from_ref::<u64>(&marker) as u64;
    let pid = std::process::id();
    let memory = WindowsProcessMemory::open(pid).unwrap();
    let bitness = memory.bitness().unwrap();

    let submit = ReadPlanSubmit {
        plan: 13,
        handle: 1,
        root: PlanBase::Absolute(address),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0,
            ops: vec![op(0, 8)],
        }],
        sync: false,
    };

    let result = execute(&submit, &memory, bitness, &NoModules, far_deadline()).unwrap();
    assert_eq!(
        result.results,
        vec![ReadResult::Ok(marker.to_le_bytes().to_vec())]
    );
}

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

    let error = execute(
        &submit,
        &DeadMemory,
        ProcessBitness::X64,
        &NoModules,
        far_deadline(),
    )
    .unwrap_err();
    assert_eq!(error, ReadPlanError::TargetGone);
    assert_eq!(vosu_status::from(error), vosu_status::VOSU_ERR_TARGET_GONE);

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

/// See `tests::zero_alloc` for the D17 M1 zero-allocation proof (split
/// into its own file so this module stays under the maintained-file line
/// limit, `docs/architecture/source-map.md`).
mod zero_alloc;
