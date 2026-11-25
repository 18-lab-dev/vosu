//! M5 I0: actions between two real `vosu plugin-host` children through
//! core's existing invocation services (`docs/v2/plugin-abi.md` §6.8,
//! §6.12, §7.6 A5.4; `docs/v2/api-v1.md` jobs): `ACTION_CALL`
//! invocation ids remapped in both directions for concurrent calls,
//! argument and result schemas and capability grants enforced before the
//! callee, and a callee's death failing its waiting caller relays and
//! jobs while the caller lives on.

use std::collections::HashMap;

use serde_json::json;
use vosu_core::actions::TerminalOutcome;
use vosu_core::plugin::process::supervisor::ProcessPhase;

use super::support::fixture::Receipt;
use super::support::host::{Identity, wait_until};
use super::support::services::{BOUND, Services, Spec, failure, kill, op};

const CALLER: &str = "test.i0.act.caller";

/// A caller granted the `work` action of the callee named `callee`;
/// returns the callee's plugin id. The callee is created first, so the
/// caller's `requires` validates against its registered action; index 0
/// is the caller and index 1 the callee.
fn pair(callee: &'static str) -> (Services, String) {
    let callee_id = format!("test.i0.{callee}");
    let mut run = Services::empty();
    run.add(&Spec {
        name: callee,
        ..Spec::default()
    });
    run.add(&Spec {
        name: "act.caller",
        requires: vec![format!(
            "[[requires]]\nid = 'action:{callee_id}.work'\nversion = '^1.0'"
        )],
        ..Spec::default()
    });
    run.plugins.swap(0, 1);
    (run, callee_id)
}

/// `key:value` pairs of a rendered result map such as
/// `{n:3,invocation:17,caller:test.i0.act.caller}`.
fn fields(rendered: &str) -> HashMap<String, String> {
    rendered
        .trim_start_matches('{')
        .trim_end_matches('}')
        .split(',')
        .filter_map(|pair| pair.split_once(':'))
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
}

/// The caller's relayed results, by its own call id.
fn results(run: &Services) -> HashMap<u64, Receipt> {
    run.plugin(0)
        .fixture
        .receipts("relay_result")
        .into_iter()
        .map(|result| (result.get("invocation").parse().unwrap(), result))
        .collect()
}

/// Sixteen concurrent `ACTION_CALL`s from one child to another: each
/// reaches the callee under core's own invocation id with the calling
/// plugin as caller, all sixteen are outstanding at once, and each
/// result and progress comes back under the caller's own id, matched to
/// exactly the invocation that carried its arguments.
#[test]
fn actions_process_calls_remap_ids_across_two_children_concurrently() {
    let (run, callee) = pair("act.remap");
    let drive = run.drive(
        0,
        op(
            "call",
            json!({"action": format!("{callee}.work"), "count": 16, "args": {"n": 100, "hold": true}}),
        ),
    );
    let fixture = &run.plugin(1).fixture;
    assert!(
        wait_until(BOUND, || fixture.receipts("work").len() == 16),
        "the callee never held all sixteen calls"
    );
    assert!(
        run.plugin(0).fixture.receipts("relay_result").is_empty(),
        "a result arrived before the callee answered"
    );
    let invoked: HashMap<u64, String> = fixture
        .receipts("work")
        .iter()
        .map(|work| {
            assert_eq!(work.get("caller_kind"), "Plugin");
            assert_eq!(work.get("caller"), CALLER);
            (
                work.get("invocation").parse().unwrap(),
                work.get("n").to_owned(),
            )
        })
        .collect();
    assert_eq!(invoked.len(), 16, "sixteen distinct callee invocations");

    assert_eq!(
        run.drive(1, op("answer", json!({"progress": 3}))).status(),
        0
    );
    assert!(
        matches!(drive.outcome(), TerminalOutcome::Succeeded(_)),
        "the caller saw every result"
    );
    let calls: HashMap<u64, u64> = run
        .plugin(0)
        .fixture
        .receipts("call")
        .iter()
        .map(|call| {
            assert_eq!(call.get("status"), "0", "{call:?}");
            (
                call.get("call").parse().unwrap(),
                call.get("index").parse().unwrap(),
            )
        })
        .collect();
    let results = results(&run);
    assert_eq!(results.len(), 16, "exactly one result per call");
    let mut seen = std::collections::HashSet::new();
    for (call, result) in &results {
        let index = calls[call];
        assert_eq!(result.get("status"), "Ok", "{result:?}");
        let value = fields(result.get("value"));
        assert_eq!(
            value["n"],
            (100 + index).to_string(),
            "call {call}'s own arguments"
        );
        assert_eq!(value["caller"], CALLER);
        let carried: u64 = value["invocation"].parse().unwrap();
        assert_eq!(
            invoked.get(&carried).map(String::as_str),
            Some(value["n"].as_str()),
            "call {call} came back from the invocation that carried its arguments"
        );
        assert!(seen.insert(carried), "two calls answered by one invocation");
    }
    for progress in run.plugin(0).fixture.receipts("relay_progress") {
        let call: u64 = progress.get("invocation").parse().unwrap();
        assert!(
            calls.contains_key(&call),
            "progress under a foreign id: {progress:?}"
        );
    }
}

/// Core validates before the callee runs: arguments against the callee's
/// schema, the caller's `requires` grant, and the action's existence;
/// a result breaking the callee's result schema becomes `invalid_result`.
/// Only the two calls core accepted reach the callee.
#[test]
fn actions_process_schemas_and_grants_refuse_before_the_callee() {
    let (run, callee) = pair("act.checks");
    let cases = [
        (
            format!("{callee}.work"),
            json!({"n": "seven"}),
            "invalid_arguments",
        ),
        (
            format!("{callee}.drive"),
            json!({"op": "sessions"}),
            "permission_denied",
        ),
        (
            "test.i0.nobody.work".to_owned(),
            json!({"n": 1}),
            "unknown_action",
        ),
        (
            format!("{callee}.work"),
            json!({"n": 2, "bad_result": true}),
            "invalid_result",
        ),
        (format!("{callee}.work"), json!({"n": 3}), "none"),
    ];
    for (call, (action, args, code)) in cases.iter().enumerate() {
        let drive = run.drive(
            0,
            op("call", json!({"action": action, "count": 1, "args": args})),
        );
        assert!(matches!(drive.outcome(), TerminalOutcome::Succeeded(_)));
        let result = &results(&run)[&(call as u64 + 1)];
        assert_eq!(result.get("code"), *code, "{action} {args}: {result:?}");
        assert_eq!(
            result.get("status"),
            if *code == "none" { "Ok" } else { "Error" }
        );
    }
    let reached: Vec<String> = run
        .plugin(1)
        .fixture
        .receipts("work")
        .iter()
        .map(|work| work.get("n").to_owned())
        .collect();
    assert_eq!(
        reached,
        ["2", "3"],
        "refused calls never reached the callee"
    );
    assert!(
        run.plugin(1).fixture.receipts("drive").is_empty(),
        "an ungranted action ran"
    );
}

/// A callee killed while two relayed calls and a broker job wait on it:
/// the caller receives `plugin_crashed` for exactly its two calls, the
/// job fails `plugin_crashed`, and the caller's own child keeps running
/// with no loss of its own.
#[test]
fn actions_process_callee_death_fails_waiting_calls_and_jobs_not_the_caller() {
    let (run, callee) = pair("act.death");
    let drive = run.drive(
        0,
        op(
            "call",
            json!({"action": format!("{callee}.work"), "count": 2, "args": {"n": 1, "hold": true}}),
        ),
    );
    let job = run.invoke(1, "work", json!({"n": 9, "hold": true}));
    let fixture = &run.plugin(1).fixture;
    assert!(
        wait_until(BOUND, || fixture.receipts("work").len() == 3),
        "the callee never held all three invocations"
    );
    let (caller, victim) = (run.child(0), run.child(1));
    let (caller_identity, victim_identity) = (Identity::of(caller), Identity::of(victim));
    kill(victim);
    assert!(wait_until(BOUND, || !victim_identity.alive()));
    assert_eq!(failure(job.outcome()), "plugin_crashed");
    assert!(matches!(drive.outcome(), TerminalOutcome::Succeeded(_)));
    let results = results(&run);
    assert_eq!(results.len(), 2, "{results:?}");
    for result in results.values() {
        assert_eq!(
            (result.get("status"), result.get("code")),
            ("Error", "plugin_crashed")
        );
    }
    assert!(caller_identity.alive(), "the caller's child died");
    let status = run.host().process_status(run.instance(0)).unwrap();
    assert!(
        status.phase == ProcessPhase::Running && status.crash_events == 0,
        "{status:?}"
    );
    assert_eq!(
        run.drive(0, op("publish", json!({"text": "alive"})))
            .status(),
        vosu_plugin_api::vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32,
        "the caller still answers (it owns no session to publish to)"
    );
}
