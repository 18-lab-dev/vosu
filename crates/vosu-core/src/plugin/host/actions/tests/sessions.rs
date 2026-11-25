use super::*;
use crate::plugin::host::tests::test_process;
use vosu_plugin_api::SessionIdentify;

#[test]
fn actions_session_ownership_is_fixed_and_detach_only_requests_cancellation() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let events = instance_mode(
        &host,
        9191,
        "tests.actions.session",
        "",
        "required",
        "[match]\nprocess_names = [\"actions-session-process\"]\n[[provides]]\nid='game:fixture'\nversion='*'\n",
        &[],
    );
    host.process_appeared(test_process(91901, "actions-session-process"));
    let attached = match events.recv_timeout(WAIT).unwrap() {
        Message::SessionAttached(attached) => attached,
        other => panic!("{other:?}"),
    };
    let action = host.get("tests.actions.session.nested.echo").unwrap();
    let executor = host.invocation_executor();
    let (sink, results) = sink();
    assert_eq!(
        executor.invoke(
            Principal::Broker,
            action.clone(),
            None,
            json!("x"),
            sink.clone()
        ),
        Err(InvocationError::InvalidSession)
    );
    assert_eq!(
        executor.invoke(
            Principal::Broker,
            action.clone(),
            Some(attached.session.clone()),
            json!("x"),
            sink.clone()
        ),
        Err(InvocationError::InvalidSession)
    );
    assert_eq!(
        host.handle(
            PluginInstanceId(9191),
            Message::SessionIdentify(SessionIdentify {
                handle: attached.handle,
                game: "fixture".into(),
                variant: "fixture".into(),
                name: "Session action".into(),
                version: "1".into(),
                focus_priority: false,
            }),
            0
        ),
        vosu_status::VOSU_OK
    );
    let other = instance_mode(
        &host,
        9192,
        "tests.actions.sessionother",
        "",
        "optional",
        "",
        &[],
    );
    let other_action = host.get("tests.actions.sessionother.nested.echo").unwrap();
    assert_eq!(
        executor.invoke(
            Principal::Broker,
            other_action,
            Some(attached.session.clone()),
            json!("x"),
            sink.clone()
        ),
        Err(InvocationError::InvalidSession)
    );
    let id = executor
        .invoke(
            Principal::Broker,
            action.clone(),
            Some(attached.session.clone()),
            json!("input"),
            sink,
        )
        .unwrap();
    let mut received = false;
    for _ in 0..16 {
        if let Message::ActionInvoke(invoked) = events.recv_timeout(WAIT).unwrap() {
            assert_eq!(invoked.session, Some(attached.handle));
            assert_eq!(invoked.invocation, id);
            received = true;
            break;
        }
    }
    assert!(received, "invocation was not delivered");
    host.process_exited(91901);
    let mut cancelled = false;
    for _ in 0..16 {
        if let Message::ActionCancel(cancel) = events.recv_timeout(WAIT).unwrap() {
            assert_eq!(cancel.invocation, id);
            cancelled = true;
            break;
        }
    }
    assert!(cancelled, "detach did not reach the callee");
    assert!(host.action_state.lock().unwrap().pending.contains_key(&id));
    assert_eq!(
        reply(
            &host,
            9191,
            id,
            CborValue::Text("finished despite detach".into())
        ),
        vosu_status::VOSU_OK
    );
    assert!(matches!(
        results.recv_timeout(WAIT).unwrap(),
        TerminalOutcome::Succeeded(_)
    ));
    drop(other);
    host.destroy_all(WAIT);
}
