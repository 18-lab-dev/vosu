//! [`OutstandingPlans`] admission cap, cancel/finish lifecycle, and
//! per-session cancellation. Split out of `tests.rs` so that module stays
//! under the maintained-file line limit
//! (`docs/architecture/source-map.md`).

use super::*;

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
