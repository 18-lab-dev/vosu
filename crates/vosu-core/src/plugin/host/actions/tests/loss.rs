use super::*;

#[test]
fn actions_relay_timeout_and_provider_loss_deliver_synthetic_terminals() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let callee = instance(&host, 9201, "tests.actions.losscallee", "");
    let caller = instance(
        &host,
        9202,
        "tests.actions.losscaller",
        "[[requires]]\nid = \"action:tests.actions.losscallee.nested.echo\"\nversion = \"^1.0\"",
    );
    host.set_invocation_policy_for_tests(InvocationPolicy {
        deadline: Duration::from_secs(1),
        ..InvocationPolicy::default()
    });
    for original in [31, 32] {
        if original == 32 {
            host.set_invocation_policy_for_tests(InvocationPolicy::default());
        }
        assert_eq!(
            host.handle(
                PluginInstanceId(9202),
                Message::ActionCall(ActionCall {
                    invocation: original,
                    action: "tests.actions.losscallee.nested.echo".into(),
                    session: None,
                    args: CborValue::Text("input".into()),
                }),
                0
            ),
            vosu_status::VOSU_OK
        );
        let invoked = invoke_message(&callee);
        let expected = if original == 31 {
            assert_eq!(
                host.handle(
                    PluginInstanceId(9202),
                    Message::ActionCancel(ActionCancel {
                        invocation: original
                    }),
                    0
                ),
                vosu_status::VOSU_OK
            );
            assert!(
                matches!(callee.recv_timeout(WAIT).unwrap(), Message::ActionCancel(c) if c.invocation == invoked.invocation)
            );
            "timed_out"
        } else {
            host.action_instance_lost(PluginInstanceId(9201));
            "plugin_crashed"
        };
        assert!(
            matches!(caller.recv_timeout(WAIT).unwrap(), Message::ActionResult(r)
            if r.invocation == original && r.code.as_deref() == Some(expected))
        );
        assert_eq!(
            reply(
                &host,
                9201,
                invoked.invocation,
                CborValue::Text("late".into())
            ),
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        );
    }
    assert!(host.action_state.lock().unwrap().pending.is_empty());
    host.destroy_all(WAIT);
}

#[test]
fn actions_caller_loss_releases_invocations_and_requests_callee_cancellation() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let callee = instance(&host, 9211, "tests.actions.callergonecallee", "");
    let _caller = instance(
        &host,
        9212,
        "tests.actions.callergone",
        "[[requires]]\nid = \"action:tests.actions.callergonecallee.nested.echo\"\nversion = \"^1.0\"",
    );
    assert_eq!(
        host.handle(
            PluginInstanceId(9212),
            Message::ActionCall(ActionCall {
                invocation: 9,
                action: "tests.actions.callergonecallee.nested.echo".into(),
                session: None,
                args: CborValue::Text("input".into()),
            }),
            0
        ),
        vosu_status::VOSU_OK
    );
    let invoked = invoke_message(&callee);
    host.action_instance_lost(PluginInstanceId(9212));
    assert!(
        matches!(callee.recv_timeout(WAIT).unwrap(), Message::ActionCancel(c) if c.invocation == invoked.invocation)
    );
    assert!(host.action_state.lock().unwrap().pending.is_empty());
    assert!(host.action_state.lock().unwrap().relays.is_empty());
    host.destroy_all(WAIT);
}
