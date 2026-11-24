use super::*;
use crate::actions::{JobOwner, ToolAuthority};
use std::sync::atomic::{AtomicBool, Ordering};
use vosu_plugin_api::{TimerFired, VOSU_FRAME_MAX_BYTES};

#[test]
fn actions_terminal_survives_full_queue_and_releases_only_after_delivery() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let callee = instance(&host, 9131, "tests.actions.pressurecallee", "");
    let (release, gate) = mpsc::sync_channel(1);
    GATES
        .lock()
        .unwrap()
        .insert("tests.actions.pressurecaller".into(), gate);
    let caller = instance(
        &host,
        9132,
        "tests.actions.pressurecaller",
        "[[requires]]\nid = \"action:tests.actions.pressurecallee.nested.echo\"\nversion = \"^1.0\"",
    );
    host.dispatch_to_instance(
        PluginInstanceId(9132),
        &Message::TimerFired(TimerFired { timer: 999 }),
        0,
    )
    .unwrap();
    assert!(matches!(caller.recv_timeout(WAIT).unwrap(), Message::TimerFired(t) if t.timer == 999));
    let mut queued = 0;
    for _ in 0..1024 {
        if host
            .dispatch_to_instance(
                PluginInstanceId(9132),
                &Message::TimerFired(TimerFired { timer: 1 }),
                0,
            )
            .is_err()
        {
            break;
        }
        queued += 1;
    }
    assert_eq!(queued, crate::plugin::instance::DEFAULT_QUEUE_CAPACITY);
    assert_eq!(
        host.handle(
            PluginInstanceId(9132),
            Message::ActionCall(ActionCall {
                invocation: 1,
                action: "tests.actions.pressurecallee.nested.echo".into(),
                session: None,
                args: CborValue::Text("input".into()),
            }),
            0
        ),
        vosu_status::VOSU_OK
    );
    let invoked = invoke_message(&callee);
    assert_eq!(
        reply(
            &host,
            9131,
            invoked.invocation,
            CborValue::Text("terminal".into())
        ),
        vosu_status::VOSU_OK
    );
    assert!(
        host.action_state
            .lock()
            .unwrap()
            .relays
            .contains_key(&(PluginInstanceId(9132), 1))
    );
    release.try_send(()).unwrap();
    assert!(
        matches!(caller.recv_timeout(WAIT).unwrap(), Message::ActionResult(r) if r.invocation == 1 && r.value == Some(CborValue::Text("terminal".into())))
    );
    // FIFO runner has completed this callback before processing ordinary work.
    assert!(matches!(caller.recv_timeout(WAIT).unwrap(), Message::TimerFired(t) if t.timer == 1));
    host.destroy_all(WAIT);
    assert!(host.action_state.lock().unwrap().relays.is_empty());
}

struct Authority(AtomicBool);
impl ToolAuthority for Authority {
    fn authorizes(&self, owner: &JobOwner, _: &ActionDescriptor) -> bool {
        owner.tool_id() == Some("tests.live-tool") && self.0.load(Ordering::Acquire)
    }
}

#[test]
fn actions_live_authority_real_caller_and_preflight_release() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let events = instance(&host, 9141, "tests.actions.authority", "");
    let action = host.get("tests.actions.authority.nested.echo").unwrap();
    let owner = JobOwner::for_tool(Arc::from("tests.live-tool"));
    let (sink, results) = sink();
    assert_eq!(
        host.invocation_executor().invoke(
            Principal::Tool(owner.clone()),
            action.clone(),
            None,
            json!("x"),
            sink.clone()
        ),
        Err(InvocationError::PermissionDenied)
    );
    let authority = Arc::new(Authority(AtomicBool::new(true)));
    let executor = host
        .invocation_executor()
        .with_tool_authority(authority.clone());
    let prepared = executor
        .prepare(
            Principal::Tool(owner.clone()),
            action.clone(),
            None,
            json!("input"),
        )
        .unwrap();
    assert_eq!(host.action_state.lock().unwrap().pending.len(), 1);
    drop(prepared);
    assert!(host.action_state.lock().unwrap().pending.is_empty());
    let id = executor
        .invoke(
            Principal::Tool(owner.clone()),
            action.clone(),
            None,
            json!("input"),
            sink.clone(),
        )
        .unwrap();
    let invocation = invoke_message(&events);
    assert_eq!(invocation.caller.id, "tests.live-tool");
    reply(&host, 9141, id, CborValue::Text("done".into()));
    assert!(matches!(
        results.recv_timeout(WAIT).unwrap(),
        TerminalOutcome::Succeeded(_)
    ));
    let prepared = executor
        .prepare(
            Principal::Tool(owner.clone()),
            action.clone(),
            None,
            json!("delayed"),
        )
        .unwrap();
    authority.0.store(false, Ordering::Release);
    assert_eq!(
        prepared.invoke(sink.clone()),
        Err(InvocationError::PermissionDenied)
    );
    assert!(host.action_state.lock().unwrap().pending.is_empty());
    assert_eq!(
        executor.invoke(
            Principal::Tool(owner),
            action.clone(),
            None,
            json!("x"),
            sink
        ),
        Err(InvocationError::PermissionDenied)
    );
    let huge = Value::String("x".repeat(VOSU_FRAME_MAX_BYTES as usize));
    assert!(matches!(
        host.invocation_executor()
            .prepare(Principal::Broker, action, None, huge),
        Err(InvocationError::ArgumentsTooLarge)
    ));
    assert!(host.action_state.lock().unwrap().pending.is_empty());
    host.destroy_all(WAIT);
}

#[test]
fn actions_job_cancel_before_start_is_forwarded_after_invoke_and_success_wins() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let events = instance(&host, 9171, "tests.actions.jobcancel", "");
    let executor = host.invocation_executor();
    let registry = crate::actions::JobRegistry::new();
    let action = host.get("tests.actions.jobcancel.nested.echo").unwrap();
    let prepared = executor
        .prepare(Principal::Broker, action.clone(), None, json!("input"))
        .unwrap();
    let job = registry
        .create(Principal::Broker, action.id.clone(), None)
        .unwrap();
    registry
        .bind_invocation(&job.id, &executor, prepared.id())
        .unwrap();
    let cancelled = registry.cancel(&Principal::Broker, &job.id).unwrap();
    assert_eq!(cancelled.state, crate::actions::JobState::Running);
    let id = prepared
        .invoke(Arc::new(crate::actions::JobSink::new(
            registry.clone(),
            job.id.clone(),
        )))
        .unwrap();
    assert_eq!(invoke_message(&events).invocation, id);
    assert!(
        matches!(events.recv_timeout(WAIT).unwrap(), Message::ActionCancel(c) if c.invocation == id)
    );
    reply(
        &host,
        9171,
        id,
        CborValue::Text("success after advisory cancellation".into()),
    );
    let current = registry.get(&Principal::Broker, &job.id).unwrap();
    assert_eq!(current.state, crate::actions::JobState::Succeeded);
    assert_eq!(job.state, crate::actions::JobState::Running);
    let waited = runtime.block_on(async {
        tokio::time::timeout(WAIT, registry.wait(&Principal::Broker, &job.id, 0, WAIT))
            .await
            .unwrap()
            .unwrap()
    });
    assert_eq!(waited, current);
    host.destroy_all(WAIT);
}
