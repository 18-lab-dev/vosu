//! `deliver_sync`/`DispatchThreadScope`: the dispatch-thread refusal, the
//! off-thread success path, and the thread-local scope's own deadline
//! behavior for a caller not wearing it. Split out of `tests.rs` so that
//! module stays under the maintained-file line limit
//! (`docs/architecture/source-map.md`).

use std::thread;
use std::time::{Duration, Instant};

use super::helpers::{FakeMemory, NoModules, SlowMemory, far_deadline, op};
use super::*;

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
    // inside a DispatchThreadScope of its own. F2: the SYNC path rejects
    // the whole call once its deadline passes mid-plan (D5/§6.3's
    // whole-call error), unlike the async path's per-read conversion —
    // so this never sees stage 0's already-successful read, only the
    // rejection.
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
    let error = deliver_sync(&submit, &memory, ProcessBitness::X64, &NoModules, deadline)
        .expect_err("stage 0's own read sleeps past the 5ms deadline");
    assert_eq!(error, SyncReadPlanError::Execute(ReadPlanError::TimedOut));

    handle.join().unwrap();
}
