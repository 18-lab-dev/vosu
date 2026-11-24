use std::collections::HashMap;
use std::sync::{Arc, LazyLock, Mutex, mpsc};
use std::time::Duration;

use serde_json::json;
use vosu_plugin_api::{ActionCall, ActionCancel, ActionResult, ActionStatus, CborValue, Message, vosu_status};

use super::*;
use crate::plugin::{host::tests::test_host, host_services::PluginHostHandler, registry};

const WAIT: Duration = Duration::from_secs(5);
static EVENTS: LazyLock<Mutex<HashMap<String, mpsc::SyncSender<Message>>>> = LazyLock::new(Mutex::default);

mod fixture {
    use vosu_plugin_api::{Init, Message, Plugin, PluginError, VtableHostServices};
    pub struct ActionPlugin(std::sync::mpsc::SyncSender<Message>);
    impl Plugin for ActionPlugin {
        fn create(_: VtableHostServices, init: Init) -> Result<Self, PluginError> {
            Ok(Self(super::EVENTS.lock().unwrap().remove(&init.plugin_id).unwrap()))
        }
        fn on_message(&mut self, message: Message, _: u64) -> Result<(), PluginError> {
            self.0.try_send(message).unwrap();
            Ok(())
        }
    }
    vosu_plugin_api::export_plugin!(bundled ActionPlugin);
}

fn instance(host: &Arc<PluginHost>, id: u64, name: &str, grants: &str) -> mpsc::Receiver<Message> {
    let (tx, rx) = mpsc::sync_channel(256);
    EVENTS.lock().unwrap().insert(name.into(), tx);
    let manifest = format!(r#"
kind = "plugin"
manifest_version = 2
id = "{name}"
name = "Actions"
version = "1.0.0"
host = ">=2.0.0-alpha.1"
abi_version = 1
runtime = "in-process"
[[actions]]
name = "nested.echo"
version = "1.0.0"
title = "Echo"
args_schema = "args.json"
result_schema = "result.json"
session = "none"
cli = true
[[provides]]
id = "action:{name}.nested.echo"
version = "1.0.0"
{grants}
"#);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest, settings_schema: None, namespace_schemas: &[],
        action_schemas: &[("args.json", "{\"type\":\"string\"}"), ("result.json", "{\"type\":\"string\"}")],
        vtable: fixture::vosu_plugin_vtable,
    }]).unwrap();
    let dir = tempfile::tempdir().unwrap();
    host.create_instance(PluginInstanceId(id), &loaded[0], dir.path().into(), None).unwrap();
    rx
}

struct Sink(mpsc::SyncSender<TerminalOutcome>);
impl InvocationSink for Sink {
    fn progress(&self, _: ActionProgress) {}
    fn finish(&self, result: TerminalOutcome) { self.0.try_send(result).unwrap(); }
}
fn sink() -> (Arc<dyn InvocationSink>, mpsc::Receiver<TerminalOutcome>) {
    let (tx, rx) = mpsc::sync_channel(4);
    (Arc::new(Sink(tx)), rx)
}
fn reply(host: &PluginHost, provider: u64, id: u64, value: CborValue) -> vosu_status {
    host.handle(PluginInstanceId(provider), Message::ActionResult(ActionResult {
        invocation: id, status: ActionStatus::Ok, value: Some(value), code: None, message: None,
    }), 0)
}
fn invoke_message(rx: &mpsc::Receiver<Message>) -> vosu_plugin_api::ActionInvoke {
    match rx.recv_timeout(WAIT).unwrap() { Message::ActionInvoke(v) => v, other => panic!("{other:?}") }
}

#[test]
fn actions_exact_lookup_schema_and_wrong_instance_first_terminal() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let events = instance(&host, 9101, "tests.actions.exact", "");
    assert!(host.get("tests.actions.exact.nested.echo").is_some());
    assert!(host.get("tests.actions.exact.echo").is_none());
    let descriptor = host.get("tests.actions.exact.nested.echo").unwrap();
    let executor = host.invocation_executor();
    let (sink, result) = sink();
    assert_eq!(executor.invoke(Principal::Broker, descriptor.clone(), None, json!(7), sink.clone()), Err(InvocationError::InvalidArguments));
    assert_eq!(executor.invoke(Principal::Broker, descriptor.clone(), Some("session-7".into()), json!("x"), sink.clone()), Err(InvocationError::InvalidSession));
    let id = executor.invoke(Principal::Broker, descriptor, None, json!("input"), sink).unwrap();
    let invoked = invoke_message(&events);
    assert_eq!(invoked.action, "tests.actions.exact.nested.echo");
    assert_eq!(invoked.invocation, id);
    assert_eq!(reply(&host, 9102, id, CborValue::Text("wrong".into())), vosu_status::VOSU_ERR_PERMISSION_DENIED);
    assert_eq!(reply(&host, 9101, id, CborValue::Text("right".into())), vosu_status::VOSU_OK);
    assert!(matches!(result.recv_timeout(WAIT).unwrap(), TerminalOutcome::Succeeded(v) if v == json!("right")));
    assert_eq!(reply(&host, 9101, id, CborValue::Text("late".into())), vosu_status::VOSU_ERR_INVALID_ARGUMENT);
    host.destroy_all(WAIT);
}

#[test]
fn actions_invalid_result_timeout_and_loss_are_terminal() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let events = instance(&host, 9111, "tests.actions.failure", "");
    let descriptor = host.get("tests.actions.failure.nested.echo").unwrap();
    let (sink, result) = sink();
    let executor = host.invocation_executor_with_policy(InvocationPolicy { deadline: Duration::from_millis(50) });
    let id = executor.invoke(Principal::Broker, descriptor.clone(), None, json!("input"), sink.clone()).unwrap();
    invoke_message(&events);
    reply(&host, 9111, id, CborValue::Bool(true));
    assert!(matches!(result.recv_timeout(WAIT).unwrap(), TerminalOutcome::Failed(e) if e.code == "invalid_result"));
    let id = executor.invoke(Principal::Broker, descriptor.clone(), None, json!("input"), sink.clone()).unwrap();
    invoke_message(&events);
    assert!(matches!(result.recv_timeout(WAIT).unwrap(), TerminalOutcome::Failed(e) if e.code == "timed_out"));
    assert_eq!(reply(&host, 9111, id, CborValue::Text("late".into())), vosu_status::VOSU_ERR_INVALID_ARGUMENT);
    host.invocation_executor().invoke(Principal::Broker, descriptor, None, json!("input"), sink).unwrap();
    invoke_message(&events);
    host.action_instance_lost(PluginInstanceId(9111));
    assert!(matches!(result.recv_timeout(WAIT).unwrap(), TerminalOutcome::Failed(e) if e.code == "plugin_crashed"));
    assert!(host.get("tests.actions.failure.nested.echo").is_none());
    host.destroy_all(WAIT);
}

#[test]
fn actions_relay_remaps_ids_and_cancel_keeps_all_64_slots() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let callee = instance(&host, 9121, "tests.actions.callee", "");
    let caller = instance(&host, 9122, "tests.actions.caller", "[[requires]]\nid = \"action:tests.actions.callee.nested.echo\"\nversion = \"^1.0\"");
    host.action_state.lock().unwrap().next_id = 65535;
    for original in 1..=64 {
        assert_eq!(host.handle(PluginInstanceId(9122), Message::ActionCall(ActionCall {
            invocation: original, action: "tests.actions.callee.nested.echo".into(), session: None,
            args: CborValue::Text("input".into()),
        }), 0), vosu_status::VOSU_OK);
        let invoked = invoke_message(&callee);
        assert!(invoked.invocation > 65535);
        assert_ne!(invoked.invocation, original);
        assert_eq!(host.handle(PluginInstanceId(9122), Message::ActionCancel(ActionCancel { invocation: original }), 0), vosu_status::VOSU_OK);
        assert!(matches!(callee.recv_timeout(WAIT).unwrap(), Message::ActionCancel(c) if c.invocation == invoked.invocation));
    }
    assert_eq!(host.action_state.lock().unwrap().relays.len(), 64);
    assert_eq!(host.handle(PluginInstanceId(9122), Message::ActionCall(ActionCall {
        invocation: 65, action: "tests.actions.callee.nested.echo".into(), session: None, args: CborValue::Null,
    }), 0), vosu_status::VOSU_ERR_INVALID_ARGUMENT);
    reply(&host, 9121, 65536, CborValue::Text("done".into()));
    assert!(matches!(caller.recv_timeout(WAIT).unwrap(), Message::ActionResult(r) if r.invocation == 1 && r.status == ActionStatus::Ok));
    host.destroy_all(WAIT);
}
