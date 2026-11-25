use vosu_plugin_api::{
    ActionCall, CborValue, Message, PlanBase, ReadPlanCancel, ReadPlanSubmit, ScanRequest,
    Selector, SessionDecline, StateSubscribe, TimerCancel, TimerSet, vosu_status,
};

use super::*;

const OK: vosu_status = vosu_status::VOSU_OK;
const REFUSED_BY_CORE: vosu_status = vosu_status::VOSU_ERR_PERMISSION_DENIED;

fn plan(plan: u64, handle: u64) -> Message {
    Message::ReadPlanSubmit(ReadPlanSubmit {
        plan,
        handle,
        root: PlanBase::Absolute(0),
        stages: Vec::new(),
        sync: false,
    })
}

fn scan(session: u64) -> Message {
    Message::ScanRequest(ScanRequest {
        session,
        module: None,
        pattern: "AA".into(),
        non_zero_mask: false,
        all_matches: false,
    })
}

fn call(invocation: u64) -> Message {
    Message::ActionCall(ActionCall {
        invocation,
        action: "acme.other.run".into(),
        session: None,
        args: CborValue::Null,
    })
}

fn timer(timer: u64, interval_ms: Option<u32>) -> Message {
    Message::TimerSet(TimerSet { timer, interval_ms })
}

/// Reserves and settles one send as core answered it.
fn send(
    admission: &Admission,
    message: &Message,
    correlation: u64,
    status: vosu_status,
) -> vosu_status {
    match admission.reserve(message, correlation) {
        Ok(settle) => {
            admission.settle(&settle, status);
            status
        }
        Err(local) => local,
    }
}

/// T1 counterexample: a 65th asynchronous plan is refused locally (it
/// never reaches the ring) and each release path frees exactly one slot.
#[test]
fn the_sixty_fifth_async_plan_is_refused_locally() {
    let admission = Admission::default();
    for id in 0..64 {
        assert_eq!(send(&admission, &plan(id, 1 + id % 2), 0, OK), OK);
    }
    assert_eq!(admission.counts().0, 64);
    assert_eq!(
        admission.reserve(&plan(64, 1), 0),
        Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT)
    );
    // A result frees one.
    admission.observe(Release::Plan(5));
    assert_eq!(send(&admission, &plan(64, 1), 0, OK), OK);
    // An accepted cancel frees one at once (§6.3), a refused one does not.
    assert_eq!(
        send(
            &admission,
            &Message::ReadPlanCancel(ReadPlanCancel { plan: 7 }),
            0,
            REFUSED_BY_CORE
        ),
        REFUSED_BY_CORE
    );
    assert_eq!(admission.counts().0, 64);
    assert_eq!(
        send(
            &admission,
            &Message::ReadPlanCancel(ReadPlanCancel { plan: 7 }),
            0,
            OK
        ),
        OK
    );
    assert_eq!(admission.counts().0, 63);
    // Its session's detach frees every plan of that session.
    admission.observe(Release::Session(2));
    assert_eq!(admission.counts().0, 33);
    // A decline core accepted does the same.
    assert_eq!(
        send(
            &admission,
            &Message::SessionDecline(SessionDecline { handle: 1 }),
            0,
            OK
        ),
        OK
    );
    assert_eq!(admission.counts().0, 0);
}

/// T1 counterexample: a refused send leaves nothing counted, and a late
/// result for an id that was never admitted frees nothing.
#[test]
fn a_refused_request_releases_its_slot() {
    let admission = Admission::default();
    assert_eq!(
        send(&admission, &plan(1, 1), 0, REFUSED_BY_CORE),
        REFUSED_BY_CORE
    );
    assert_eq!(
        send(&admission, &scan(1), 9, REFUSED_BY_CORE),
        REFUSED_BY_CORE
    );
    assert_eq!(
        send(&admission, &call(4), 0, REFUSED_BY_CORE),
        REFUSED_BY_CORE
    );
    assert_eq!(
        send(&admission, &timer(3, None), 0, REFUSED_BY_CORE),
        REFUSED_BY_CORE
    );
    assert_eq!(admission.counts(), (0, 0, 0, 0));
    admission.observe(Release::Plan(1));
    admission.observe(Release::Scan(9));
    assert_eq!(admission.counts(), (0, 0, 0, 0));
}

/// Plan ids are plugin-chosen: a resubmitted id counts again, and each of
/// its results frees one submission.
#[test]
fn a_reused_plan_id_counts_once_per_submission() {
    let admission = Admission::default();
    assert_eq!(send(&admission, &plan(3, 1), 0, OK), OK);
    assert_eq!(send(&admission, &plan(3, 1), 0, OK), OK);
    assert_eq!(admission.counts().0, 2);
    admission.observe(Release::Plan(3));
    assert_eq!(admission.counts().0, 1);
    admission.observe(Release::Plan(3));
    admission.observe(Release::Plan(3));
    assert_eq!(admission.counts().0, 0, "never below zero");
}

#[test]
fn scans_are_capped_and_freed_by_result_or_session() {
    let admission = Admission::default();
    for correlation in 0..64 {
        assert_eq!(
            send(&admission, &scan(correlation % 4), correlation, OK),
            OK
        );
    }
    assert_eq!(
        admission.reserve(&scan(0), 64),
        Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT)
    );
    admission.observe(Release::Scan(10));
    assert_eq!(admission.counts().1, 63);
    admission.observe(Release::Session(0));
    assert_eq!(admission.counts().1, 47);
}

/// §6.12: a call's slot is freed only by its relayed ACTION_RESULT.
#[test]
fn action_calls_are_capped_and_freed_only_by_their_result() {
    let admission = Admission::default();
    for invocation in 0..64 {
        assert_eq!(send(&admission, &call(invocation), 0, OK), OK);
    }
    assert_eq!(
        admission.reserve(&call(64), 0),
        Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT)
    );
    admission.observe(Release::Session(0));
    admission.observe(Release::Plan(0));
    assert_eq!(admission.counts().2, 64);
    admission.observe(Release::Call(12));
    assert_eq!(send(&admission, &call(64), 0, OK), OK);
    // A reused invocation id is core's refusal to make; the existing slot
    // survives that refusal.
    assert_eq!(
        send(
            &admission,
            &call(64),
            0,
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        ),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    assert_eq!(admission.counts().2, 64);
}

#[test]
fn timers_count_distinct_ids_and_free_one_shots_when_they_fire() {
    let admission = Admission::default();
    for id in 0..64 {
        let interval = if id % 2 == 0 { None } else { Some(100) };
        assert_eq!(send(&admission, &timer(id, interval), 0, OK), OK);
    }
    assert_eq!(
        admission.reserve(&timer(64, None), 0),
        Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT)
    );
    // Re-arming a live id needs no new slot, and a refused re-arm restores
    // the old timer.
    assert_eq!(
        send(&admission, &timer(1, None), 0, REFUSED_BY_CORE),
        REFUSED_BY_CORE
    );
    assert_eq!(admission.counts().3, 64);
    admission.observe(Release::Timer(1));
    assert_eq!(admission.counts().3, 64, "a periodic timer survives firing");
    admission.observe(Release::Timer(0));
    assert_eq!(
        admission.counts().3,
        63,
        "a one-shot timer is gone once fired"
    );
    assert_eq!(
        send(
            &admission,
            &Message::TimerCancel(TimerCancel { timer: 3 }),
            0,
            OK
        ),
        OK
    );
    assert_eq!(admission.counts().3, 62);
}

#[test]
fn a_session_subscription_settles_its_delivery_reset() {
    let admission = Admission::default();
    let subscribe = |session| {
        Message::StateSubscribe(StateSubscribe {
            session,
            selectors: vec![Selector {
                ns: "vosu.sessions".into(),
                pointer: String::new(),
            }],
        })
    };
    assert_eq!(
        admission.reserve(&subscribe(Some(8)), 0),
        Ok(Settle::Resubscribe(8))
    );
    assert_eq!(admission.reserve(&subscribe(None), 0), Ok(Settle::Nothing));
}

#[test]
fn only_log_records_go_unacknowledged() {
    use vosu_plugin_api::vosu_opcode::*;
    assert_eq!(route(VOSU_OP_LOG_RECORD), Route::Unacknowledged);
    assert_eq!(route(VOSU_OP_PUBLISH_BATCH), Route::Acknowledged);
    assert_eq!(route(VOSU_OP_STATE_SUBSCRIBE), Route::Acknowledged);
    assert_eq!(route(VOSU_OP_STATE_UPDATE), Route::NoEffect);
}
