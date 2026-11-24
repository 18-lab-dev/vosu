use super::*;

#[test]
fn actions_callee_calls_back_into_caller_during_dispatch_without_deadlock() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let caller = instance_mode(
        &host,
        9181,
        "tests.actions.reentrantcaller",
        "[[requires]]\nid = \"action:tests.actions.reentrantcallee.nested.echo\"\nversion = \"^1.0\"",
        "none",
        "",
        &["tests.actions.reentrantcallee"],
    );
    let callee = instance(
        &host,
        9182,
        "tests.actions.reentrantcallee",
        "[[requires]]\nid = \"action:tests.actions.reentrantcaller.nested.echo\"\nversion = \"^1.0\"",
    );
    let action = host
        .get("tests.actions.reentrantcaller.nested.echo")
        .unwrap();
    let (sink, results) = sink();
    host.invocation_executor()
        .invoke(
            Principal::Broker,
            action,
            None,
            json!("call:tests.actions.reentrantcallee.nested.echo"),
            sink,
        )
        .unwrap();
    assert!(
        matches!(results.recv_timeout(WAIT).unwrap(), TerminalOutcome::Succeeded(value)
        if value == json!("reentrant result"))
    );
    let first = invoke_message(&caller);
    let reverse = invoke_message(&caller);
    let forward = invoke_message(&callee);
    assert_eq!(first.caller.kind, CallerKind::Cli);
    assert_eq!(reverse.caller.id, "tests.actions.reentrantcallee");
    assert_eq!(forward.caller.id, "tests.actions.reentrantcaller");
    assert_ne!(first.invocation, reverse.invocation);
    host.destroy_all(WAIT);
}
