//! A5.4 action deadline grace (`docs/v2/plugin-abi.md` §7.6) and the
//! per-instance teardown seam for an in-process plugin: a timed-out process
//! callee gets `ACTION_CANCEL` and 5 seconds (shortened here) before core
//! kills it as one loss; the timed-out job stays `timed_out`, the callee's
//! other running jobs fail `plugin_crashed`, another plugin's job is
//! untouched, an answer inside the grace keeps the callee, and an
//! in-process callee is never killed.

use std::time::Instant;

use super::*;
use crate::plugin::execution::LossCause;
use crate::plugin::process::supervisor::{FailureReason, ProcessPhase};
use crate::plugin::test_support::{LaunchBehavior, RecordingLauncher, write_development_package};

fn process(host: &Arc<PluginHost>, id: u64, plugin_id: &str) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    write_development_package(
        dir.path(),
        plugin_id,
        &format!("{plugin_id}-process"),
        "process",
        "plugin.bin",
        b"never mapped by core",
    )
    .unwrap();
    let loaded = registry::load_development(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        dir.path(),
        true,
    )
    .unwrap();
    host.create_instance(PluginInstanceId(id), &loaded, dir.path().into(), None)
        .unwrap();
    dir
}

fn policy(deadline_ms: u64, grace_ms: u64) -> InvocationPolicy {
    InvocationPolicy {
        deadline: Duration::from_millis(deadline_ms),
        grace: Duration::from_millis(grace_ms),
    }
}

fn failed(outcome: TerminalOutcome) -> String {
    match outcome {
        TerminalOutcome::Failed(failure) => failure.code,
        other => panic!("expected a failure, got {other:?}"),
    }
}

fn invoke(
    host: &PluginHost,
    policy: InvocationPolicy,
    action: &str,
    arguments: serde_json::Value,
) -> (InvocationId, mpsc::Receiver<TerminalOutcome>) {
    let (sink, outcome) = sink();
    let id = host
        .invocation_executor_with_policy(policy)
        .invoke(
            Principal::Broker,
            host.get(action).unwrap(),
            None,
            arguments,
            sink,
        )
        .unwrap();
    (id, outcome)
}

#[test]
fn actions_an_unanswered_grace_kills_only_the_process_callee_once() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let launcher = RecordingLauncher::new(LaunchBehavior::Ready);
    host.install_process_launcher(launcher.clone()).unwrap();
    let _callee = process(&host, 9_701, "test.c0.grace");
    let _other = process(&host, 9_702, "test.c0.unrelated");
    let events = instance(&host, 9_703, "tests.actions.inprocess", "");
    let endpoint = Arc::clone(&launcher.launches()[0].endpoint);

    let (timed_out, timed_out_outcome) =
        invoke(&host, policy(150, 400), "test.c0.grace.run", json!({}));
    let (_, collateral) = invoke(&host, policy(30_000, 5_000), "test.c0.grace.run", json!({}));
    let (_, unrelated) = invoke(
        &host,
        policy(30_000, 5_000),
        "test.c0.unrelated.run",
        json!({}),
    );
    let (_, in_process) = invoke(
        &host,
        policy(150, 100),
        "tests.actions.inprocess.nested.echo",
        json!("x"),
    );
    invoke_message(&events);

    let started = Instant::now();
    assert_eq!(
        failed(timed_out_outcome.recv_timeout(WAIT).unwrap()),
        "timed_out"
    );
    assert_eq!(failed(in_process.recv_timeout(WAIT).unwrap()), "timed_out");
    let cancel = endpoint
        .wait_for_frame(
            vosu_plugin_api::vosu_opcode::VOSU_OP_ACTION_CANCEL as u32,
            WAIT,
        )
        .expect("ACTION_CANCEL to the timed-out process callee");
    assert_eq!(
        vosu_plugin_api::decode::<ActionCancel>(&cancel.payload)
            .unwrap()
            .invocation,
        timed_out
    );

    // The grace elapses unanswered: the callee is killed, and its other
    // running job fails with `plugin_crashed`.
    assert_eq!(
        failed(collateral.recv_timeout(WAIT).unwrap()),
        "plugin_crashed"
    );
    assert!(
        started.elapsed() >= Duration::from_millis(350),
        "{:?}",
        started.elapsed()
    );
    // Supervision records the loss first; retirement then kills the child
    // on its own thread.
    let deadline = Instant::now() + WAIT;
    while endpoint.terminate_calls() == 0 {
        assert!(Instant::now() < deadline, "the grace kill never happened");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(endpoint.terminate_calls(), 1);
    assert_eq!(endpoint.destroy_calls(), 0);
    let status = host.process_status(PluginInstanceId(9_701)).unwrap();
    assert_eq!(
        status.last_failure,
        Some(FailureReason::Loss(LossCause::ActionGraceExpired))
    );
    assert_eq!(status.crash_events, 1);
    // The timed-out job never changes, and nothing else was killed.
    assert!(
        timed_out_outcome
            .recv_timeout(Duration::from_millis(100))
            .is_err()
    );
    assert!(unrelated.recv_timeout(Duration::from_millis(100)).is_err());
    assert_eq!(launcher.launches()[1].endpoint.terminate_calls(), 0);
    assert!(
        host.dispatch_to_instance(
            PluginInstanceId(9_703),
            &Message::ActionCancel(ActionCancel { invocation: 1 }),
            0
        )
        .is_ok(),
        "an in-process callee was killed"
    );
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn actions_an_answer_inside_the_grace_keeps_the_process_callee() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let launcher = RecordingLauncher::new(LaunchBehavior::Ready);
    host.install_process_launcher(launcher.clone()).unwrap();
    let _callee = process(&host, 9_711, "test.c0.answers");
    let endpoint = Arc::clone(&launcher.launches()[0].endpoint);

    let (id, outcome) = invoke(&host, policy(150, 500), "test.c0.answers.run", json!({}));
    assert_eq!(failed(outcome.recv_timeout(WAIT).unwrap()), "timed_out");
    endpoint
        .wait_for_frame(
            vosu_plugin_api::vosu_opcode::VOSU_OP_ACTION_CANCEL as u32,
            WAIT,
        )
        .unwrap();
    // The late result is discarded with the in-process status, and proves
    // the callee responsive.
    assert_eq!(
        reply(&host, 9_711, id, CborValue::Bool(true)),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    std::thread::sleep(Duration::from_millis(800));
    assert_eq!(
        endpoint.terminate_calls(),
        0,
        "a responsive callee was killed"
    );
    assert!(outcome.try_recv().is_err(), "the timed-out job changed");
    let status = host.process_status(PluginInstanceId(9_711)).unwrap();
    assert_eq!(
        (status.phase, status.crash_events),
        (ProcessPhase::Running, 0)
    );
    host.destroy_all(Duration::from_secs(5));
}

/// The per-instance teardown seam (M6b uninstall, manual stop) for an
/// in-process instance: destroyed, its actions gone, other instances kept.
#[test]
fn actions_teardown_instance_destroys_one_in_process_instance() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let (_dir, host) = test_host(runtime.handle().clone());
    let _gone = instance(&host, 9_721, "tests.actions.teardown", "");
    let _kept = instance(&host, 9_722, "tests.actions.kept", "");
    let (_, running) = invoke(
        &host,
        InvocationPolicy::default(),
        "tests.actions.teardown.nested.echo",
        json!("x"),
    );
    host.teardown_instance(PluginInstanceId(9_721)).unwrap();
    assert_eq!(
        failed(running.recv_timeout(WAIT).unwrap()),
        "plugin_crashed"
    );
    assert!(host.get("tests.actions.teardown.nested.echo").is_none());
    assert!(host.get("tests.actions.kept.nested.echo").is_some());
    assert!(host.process_status(PluginInstanceId(9_721)).is_none());
    assert!(matches!(
        host.dispatch_to_instance(
            PluginInstanceId(9_721),
            &Message::ActionCancel(ActionCancel { invocation: 1 }),
            0
        ),
        Err(crate::plugin::HostError::UnknownInstance)
    ));
    assert!(matches!(
        host.teardown_instance(PluginInstanceId(9_721)),
        Err(crate::plugin::HostError::UnknownInstance)
    ));
    host.destroy_all(Duration::from_secs(5));
}
