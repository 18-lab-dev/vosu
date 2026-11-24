use super::*;
use vosu_plugin_api::{ActionProgress, VOSU_FRAME_MAX_BYTES};

fn padding(empty_size: usize) -> String {
    // Text's CBOR length header grows from one byte to five at this size.
    "p".repeat(VOSU_FRAME_MAX_BYTES as usize - empty_size - 4)
}

#[test]
fn actions_remapped_result_and_progress_enforce_exact_frame_limit() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let callee = instance(&host, 9151, "tests.actions.framecallee", "");
    let caller = instance(
        &host,
        9152,
        "tests.actions.framecaller",
        "[[requires]]\nid = \"action:tests.actions.framecallee.nested.echo\"\nversion = \"^1.0\"",
    );
    for original in [u64::MAX, u64::MAX - 1] {
        assert_eq!(
            host.handle(
                PluginInstanceId(9152),
                Message::ActionCall(ActionCall {
                    invocation: original,
                    action: "tests.actions.framecallee.nested.echo".into(),
                    session: None,
                    args: CborValue::Text("input".into()),
                }),
                0
            ),
            vosu_status::VOSU_OK
        );
        let invoked = invoke_message(&callee);
        if original == u64::MAX {
            let mut result = ActionResult {
                invocation: invoked.invocation,
                status: ActionStatus::Ok,
                value: Some(CborValue::Text(String::new())),
                code: None,
                message: None,
            };
            let size = Message::ActionResult(result.clone())
                .encode_payload()
                .unwrap()
                .len();
            result.value = Some(CborValue::Text(padding(size)));
            assert_eq!(
                Message::ActionResult(result.clone())
                    .encode_payload()
                    .unwrap()
                    .len(),
                VOSU_FRAME_MAX_BYTES as usize
            );
            assert_eq!(
                host.handle(PluginInstanceId(9151), Message::ActionResult(result), 0),
                vosu_status::VOSU_OK
            );
            assert!(
                matches!(caller.recv_timeout(WAIT).unwrap(), Message::ActionResult(r)
                if r.invocation == original && r.code.as_deref() == Some("result_too_large"))
            );
        } else {
            let mut progress = ActionProgress {
                invocation: invoked.invocation,
                fraction: None,
                message: Some(String::new()),
            };
            let size = Message::ActionProgress(progress.clone())
                .encode_payload()
                .unwrap()
                .len();
            progress.message = Some(padding(size));
            assert_eq!(
                Message::ActionProgress(progress.clone())
                    .encode_payload()
                    .unwrap()
                    .len(),
                VOSU_FRAME_MAX_BYTES as usize
            );
            assert_eq!(
                host.handle(PluginInstanceId(9151), Message::ActionProgress(progress), 0),
                vosu_status::VOSU_OK
            );
            reply(
                &host,
                9151,
                invoked.invocation,
                CborValue::Text("still alive".into()),
            );
            assert!(
                matches!(caller.recv_timeout(WAIT).unwrap(), Message::ActionResult(r)
                if r.invocation == original && r.status == ActionStatus::Ok)
            );
        }
    }
    host.destroy_all(WAIT);
}

#[test]
fn actions_plugin_arguments_that_grow_during_forwarding_get_terminal_error() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let _callee = instance(&host, 9161, "tests.actions.argcallee", "");
    let caller = instance(
        &host,
        9162,
        "tests.actions.argcaller",
        "[[requires]]\nid = \"action:tests.actions.argcallee.nested.echo\"\nversion = \"^1.0\"",
    );
    let mut call = ActionCall {
        invocation: 1,
        action: "tests.actions.argcallee.nested.echo".into(),
        session: None,
        args: CborValue::Text(String::new()),
    };
    let size = Message::ActionCall(call.clone())
        .encode_payload()
        .unwrap()
        .len();
    call.args = CborValue::Text(padding(size));
    assert_eq!(
        Message::ActionCall(call.clone())
            .encode_payload()
            .unwrap()
            .len(),
        VOSU_FRAME_MAX_BYTES as usize
    );
    assert_eq!(
        host.handle(PluginInstanceId(9162), Message::ActionCall(call), 0),
        vosu_status::VOSU_OK
    );
    assert!(
        matches!(caller.recv_timeout(WAIT).unwrap(), Message::ActionResult(r)
        if r.invocation == 1 && r.code.as_deref() == Some("args_too_large"))
    );
    assert!(host.action_state.lock().unwrap().pending.is_empty());
    host.destroy_all(WAIT);
}
