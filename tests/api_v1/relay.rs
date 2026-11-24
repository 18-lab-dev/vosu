//! J3: production ABI ingress, independent caller IDs, bounded relay delivery.
use super::{job_fixture as fixture, support::Harness};
use vosu_core::{identity::PluginInstanceId, plugin::PluginHostHandler};
use vosu_plugin_api::{
    ActionCall, ActionCancel, ActionResult, ActionStatus, CborValue, Message, TimerFired,
    vosu_status,
};

#[test]
fn failed_relay_delivery_and_cancel_do_not_release_any_of_64_slots() {
    let h = Harness::new();
    let (callee, _) = fixture::load(&h, 783311, "fixture.jthree.callee", "");
    let (caller, release) = fixture::load(
        &h,
        783312,
        "fixture.jthree.caller",
        "\n[[requires]]\nid='action:fixture.jthree.callee.nested.echo'\nversion='^1.0'\n",
    );
    let provider = PluginInstanceId::new(783311);
    let owner = PluginInstanceId::new(783312);
    fixture::timer(&h, 783312, 999);
    fixture::next(
        &caller,
        |m| matches!(m, Message::TimerFired(t) if t.timer == 999),
    );
    let mut queued = 0;
    for _ in 0..1024 {
        if h.host
            .dispatch_to_instance(owner, &Message::TimerFired(TimerFired { timer: 2 }), 0)
            .is_err()
        {
            break;
        }
        queued += 1;
    }
    assert_eq!(queued, vosu_core::plugin::DEFAULT_QUEUE_CAPACITY);
    let call = |invocation| {
        Message::ActionCall(ActionCall {
            invocation,
            action: "fixture.jthree.callee.nested.echo".into(),
            session: None,
            args: CborValue::Text("relay sentinel".into()),
        })
    };
    for invocation in 1..=64 {
        assert_eq!(
            h.host.handle(owner, call(invocation), 0),
            vosu_status::VOSU_OK
        );
    }
    let Message::ActionInvoke(first) =
        fixture::next(&callee, |m| matches!(m, Message::ActionInvoke(_)))
    else {
        unreachable!()
    };
    // The host-generated invocation must not be used as the caller correlation ID.
    let Message::ActionInvoke(second) =
        fixture::next(&callee, |m| matches!(m, Message::ActionInvoke(_)))
    else {
        unreachable!()
    };
    assert_ne!(first.invocation, second.invocation);
    assert_eq!(
        h.host.handle(
            owner,
            Message::ActionCancel(ActionCancel { invocation: 2 }),
            0
        ),
        vosu_status::VOSU_OK
    );
    let terminal = Message::ActionResult(ActionResult {
        invocation: first.invocation,
        status: ActionStatus::Ok,
        value: Some(CborValue::Text("retained terminal".into())),
        code: None,
        message: None,
    });
    assert_ne!(
        h.host.handle(owner, terminal.clone(), 0),
        vosu_status::VOSU_OK
    );
    assert_eq!(h.host.handle(provider, terminal, 0), vosu_status::VOSU_OK);
    // Neither cancellation nor failed immediate delivery frees capacity.
    assert_ne!(h.host.handle(owner, call(65), 0), vosu_status::VOSU_OK);
    release.try_send(()).unwrap();
    let result = fixture::next(&caller, |m| matches!(m, Message::ActionResult(_)));
    assert!(
        matches!(result, Message::ActionResult(r) if r.invocation == 1 && r.value == Some(CborValue::Text("retained terminal".into())))
    );
    // Callback completion wakes a runtime task to reclaim the slot. Observe an
    // ordinary callback, then bound the asynchronous acknowledgement, not its
    // scheduler ordering against this test thread.
    fixture::next(
        &caller,
        |m| matches!(m, Message::TimerFired(t) if t.timer == 2),
    );
    h.runtime.block_on(super::support::bounded(async {
        loop {
            if h.host.handle(owner, call(65), 0) == vosu_status::VOSU_OK {
                break;
            }
            tokio::task::yield_now().await;
        }
    }));
    h.host.action_instance_lost(owner);
    h.host.action_instance_lost(provider);
    h.host.destroy_all(super::support::WAIT);
}

#[test]
fn maximum_caller_id_remaps_full_size_real_abi_result_to_bounded_error() {
    let h = Harness::new();
    let (callee, _) = fixture::load(&h, 783351, "fixture.jthree.largecallee", "");
    let (caller, _) = fixture::load(
        &h,
        783352,
        "fixture.jthree.largecaller",
        "\n[[requires]]\nid='action:fixture.jthree.largecallee.nested.echo'\nversion='^1.0'\n",
    );
    assert_eq!(
        h.host.handle(
            PluginInstanceId::new(783352),
            Message::ActionCall(ActionCall {
                invocation: u64::MAX,
                action: "fixture.jthree.largecallee.nested.echo".into(),
                session: None,
                args: CborValue::Text("large relay boundary".into()),
            }),
            0
        ),
        vosu_status::VOSU_OK
    );
    let Message::ActionInvoke(invoked) =
        fixture::next(&callee, |m| matches!(m, Message::ActionInvoke(_)))
    else {
        unreachable!()
    };
    assert!(invoked.invocation < u64::MAX);
    // Timer=4 emits an exactly 1-MiB result through the provider's real SDK
    // HostServices. Only remapping its small host id to u64::MAX exceeds the cap.
    fixture::timer(&h, 783351, 4);
    let Message::ActionResult(result) =
        fixture::next(&caller, |m| matches!(m, Message::ActionResult(_)))
    else {
        unreachable!()
    };
    assert_eq!(result.invocation, u64::MAX);
    assert_eq!(result.status, ActionStatus::Error);
    assert_eq!(result.code.as_deref(), Some("result_too_large"));
    assert!(result.value.is_none());
    h.host.destroy_all(super::support::WAIT);
}
