//! Counterexamples: shared PID/global state, released barriers, wrong request
//! keys, queue overflow consuming steps, duplicate terminals and lost cancellation.
#![cfg(feature = "test-harness")]
use std::time::Duration;
use vosu_plugin_api::*;
use vosu_plugin_testkit::{actions::*, resources::*};
const WAIT: Duration = Duration::from_secs(2);

fn resolve(handle: u64) -> Message {
    Message::ResourceResolve(ResourceResolve {
        handle,
        kind: "fixture.file".into(),
        key: CborValue::Text("key-7913".into()),
    })
}
fn path(root: &str) -> Message {
    Message::ResourceResolved(ResourceResolved::Path {
        root: root.into(),
        path: "different-8927.osu".into(),
    })
}
fn call(action: &str, invocation: u64) -> ActionInvoke {
    ActionInvoke {
        invocation,
        action: action.into(),
        session: Some(715),
        caller: Caller {
            kind: CallerKind::Tool,
            id: "owner-937".into(),
        },
        args: CborValue::Integer(7913.into()),
    }
}
fn success() -> ActionScript {
    ActionScript {
        steps: vec![
            ActionStep::Progress {
                fraction: 0.375,
                message: "progress-8927".into(),
            },
            ActionStep::Success(CborValue::Integer(937.into())),
        ]
        .into(),
        ignores_cancel: false,
    }
}

#[test]
fn resources_hold_late_wrong_correlation_and_independent_roots() {
    // Same session handle (and hence no dependence on PID uniqueness).
    let (mut first, events1) = ResourceFixture::new();
    let (mut second, events2) = ResourceFixture::new();
    first
        .push(ResourceStep {
            request: resolve(715),
            replies: vec![Reply {
                message: path("root-first"),
                correlation: Some(999),
                held: true,
            }],
        })
        .unwrap();
    second
        .push(ResourceStep {
            request: resolve(715),
            replies: vec![Reply::immediate(path("root-second"))],
        })
        .unwrap();
    assert!(first.dispatch(resolve(715), 77).unwrap().is_empty());
    assert_eq!(first.pending(), 1);
    assert_eq!(
        second.dispatch(resolve(715), 88).unwrap(),
        vec![Envelope {
            message: path("root-second"),
            correlation: 88
        }]
    );
    assert_eq!(events1.recv_timeout(WAIT).unwrap().correlation, 77);
    assert_eq!(events2.recv_timeout(WAIT).unwrap().correlation, 88);
    assert_eq!(
        first.release(),
        Some(Envelope {
            message: path("root-first"),
            correlation: 999
        })
    );
    assert!(second.release().is_none());
    first.shutdown();
    assert_eq!(first.pending(), 0);
    assert_eq!(second.counters().resolve, 1);
}

#[test]
fn resource_event_pressure_refuses_without_consuming_script() {
    let (mut fixture, receiver) = ResourceFixture::new();
    for _ in 0..CAPACITY {
        fixture
            .push(ResourceStep {
                request: resolve(715),
                replies: vec![],
            })
            .unwrap();
    }
    assert!(
        fixture
            .push(ResourceStep {
                request: resolve(715),
                replies: vec![]
            })
            .is_err()
    );
    for id in 0..CAPACITY {
        fixture.dispatch(resolve(715), id as u64).unwrap();
    }
    fixture
        .push(ResourceStep {
            request: resolve(715),
            replies: vec![Reply::immediate(path("root-third"))],
        })
        .unwrap();
    assert_eq!(
        fixture.dispatch(resolve(715), 99),
        Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT)
    );
    assert_eq!(fixture.remaining(), 1);
    assert_eq!(fixture.counters().resolve, CAPACITY);
    receiver.recv_timeout(WAIT).unwrap();
    assert_eq!(
        fixture.dispatch(resolve(715), 99).unwrap()[0].correlation,
        99
    );
    assert_eq!(fixture.remaining(), 0);
}

#[test]
fn resource_tree_stream_error_and_exact_read_close_counters() {
    let (mut fixture, events) = ResourceFixture::new();
    let list = Message::TreeList(TreeList {
        handle: 715,
        kind: "fixture.tree".into(),
        key: CborValue::Bool(true),
    });
    let lookup = Message::TreeLookup(TreeLookup {
        handle: 715,
        kind: "fixture.tree".into(),
        key: CborValue::Bool(true),
        path: "alias.osu".into(),
    });
    let read = Message::StreamRead(StreamRead {
        stream: 917,
        offset: 3,
        len: 7,
    });
    let close = Message::StreamClose(StreamClose { stream: 917 });
    for (request, reply) in [
        (
            list,
            Some(Message::ResourceResolved(ResourceResolved::Tree {
                entries: vec![TreeEntry {
                    path: "canonical.osu".into(),
                    size: 19,
                    hash: None,
                }],
                truncated: true,
                revision: "rev-937".into(),
            })),
        ),
        (
            lookup,
            Some(Message::ResourceResolved(ResourceResolved::TreeStream {
                stream: 917,
                size: 19,
                entry_path: "canonical.osu".into(),
            })),
        ),
        (
            read,
            Some(Message::StreamChunk(StreamChunk::Error {
                error: ErrorInfo {
                    code: "fixture_error".into(),
                    message: "nonempty sentinel".into(),
                },
            })),
        ),
        (close, None),
    ] {
        fixture
            .push(ResourceStep {
                request: request.clone(),
                replies: reply.clone().map(Reply::immediate).into_iter().collect(),
            })
            .unwrap();
        let actual = fixture.dispatch(request.clone(), 123).unwrap();
        assert_eq!(
            actual.into_iter().map(|e| e.message).collect::<Vec<_>>(),
            reply.into_iter().collect::<Vec<_>>()
        );
        assert_eq!(events.recv_timeout(WAIT).unwrap().message, request);
    }
    assert_eq!(
        fixture.counters(),
        ResourceCounters {
            resolve: 0,
            list: 1,
            lookup: 1,
            read: 1,
            close: 1
        }
    );
    fixture
        .push(ResourceStep {
            request: resolve(715),
            replies: vec![],
        })
        .unwrap();
    assert!(fixture.dispatch(resolve(716), 1).is_err());
    assert_eq!(fixture.remaining(), 1);
}

#[test]
fn actions_exact_discriminator_barriers_failure_and_terminal_once() {
    let (mut fixture, events) = ActionFixture::new();
    fixture
        .script("fixture.first.run".into(), success())
        .unwrap();
    fixture
        .script(
            "fixture.second.run".into(),
            ActionScript {
                steps: vec![ActionStep::Failure {
                    code: "failed-937".into(),
                    message: "distinct failure".into(),
                }]
                .into(),
                ignores_cancel: false,
            },
        )
        .unwrap();
    let first = call("fixture.first.run", 19);
    fixture.invoke(first.clone()).unwrap();
    fixture.invoke(call("fixture.second.run", 29)).unwrap();
    assert_eq!(fixture.pending(), 2);
    assert_eq!(
        events.recv_timeout(WAIT).unwrap(),
        ActionEvent::Invoked(first)
    );
    assert!(
        matches!(fixture.release(29), Some(Message::ActionResult(ActionResult { status: ActionStatus::Error, code: Some(code), .. })) if code == "failed-937")
    );
    assert!(fixture.release(29).is_none());
    assert!(matches!(
        fixture.release(19),
        Some(Message::ActionProgress(ActionProgress {
            fraction: Some(0.375),
            ..
        }))
    ));
    assert_eq!(fixture.pending(), 1);
    assert!(
        matches!(fixture.release(19), Some(Message::ActionResult(ActionResult { value: Some(CborValue::Integer(value)), .. })) if value == 937.into())
    );
    assert!(fixture.release(19).is_none());
    assert!(fixture.invoke(call("fixture.unknown", 31)).is_err());
    assert_eq!(fixture.pending(), 0);
}

#[test]
fn actions_independent_ownership_cancel_ignore_and_shutdown() {
    let (mut first, events1) = ActionFixture::new();
    let (mut second, events2) = ActionFixture::new();
    first.script("fixture.run".into(), success()).unwrap();
    second
        .script(
            "fixture.run".into(),
            ActionScript {
                steps: Default::default(),
                ignores_cancel: true,
            },
        )
        .unwrap();
    first.invoke(call("fixture.run", 19)).unwrap();
    second.invoke(call("fixture.run", 19)).unwrap();
    events1.recv_timeout(WAIT).unwrap();
    events2.recv_timeout(WAIT).unwrap();
    assert!(
        matches!(first.cancel(19).unwrap(), Some(Message::ActionResult(ActionResult { code: Some(code), .. })) if code == "cancelled")
    );
    assert_eq!(
        events1.recv_timeout(WAIT).unwrap(),
        ActionEvent::Cancelled(19)
    );
    assert_eq!(first.pending(), 0);
    assert_eq!(second.pending(), 1);
    assert!(second.cancel(19).unwrap().is_none());
    assert_eq!(
        events2.recv_timeout(WAIT).unwrap(),
        ActionEvent::Cancelled(19)
    );
    assert!(second.release(19).is_none());
    assert_eq!(second.pending(), 1);
    assert_eq!(
        (
            first.invoked,
            first.cancelled,
            second.invoked,
            second.cancelled
        ),
        (1, 1, 1, 1)
    );
    first.shutdown();
    assert_eq!(second.pending(), 1);
    second.shutdown();
    assert_eq!(second.pending(), 0);
}

#[test]
fn actions_real_queue_and_pending_limits_are_bounded() {
    let (mut fixture, events) = ActionFixture::new();
    fixture.script("fixture.run".into(), success()).unwrap();
    for id in 0..CAPACITY {
        fixture.invoke(call("fixture.run", id as u64)).unwrap();
    }
    assert!(fixture.invoke(call("fixture.run", 1000)).is_err());
    assert!(fixture.invoke(call("fixture.run", 0)).is_err());
    // Free a pending slot without draining the full observation queue.
    fixture.release(0).unwrap();
    fixture.release(0).unwrap();
    assert_eq!(fixture.pending(), CAPACITY - 1);
    assert!(fixture.invoke(call("fixture.run", 1000)).is_err());
    events.recv_timeout(WAIT).unwrap();
    fixture.invoke(call("fixture.run", 1000)).unwrap();
    assert_eq!(fixture.pending(), CAPACITY);
    fixture.shutdown();
    assert_eq!(fixture.pending(), 0);
}

#[test]
fn development_manifest_constructs_both_platforms_and_two_actions() {
    let manifest = PluginManifest::parse(include_str!(
        "../../../tests/fixtures/api-v1-plugin/manifest.toml"
    ))
    .unwrap();
    assert_eq!(manifest.id, "test.api.fixture");
    assert_eq!(manifest.actions.len(), 2);
    assert!(manifest.targets.contains_key("x86_64-unknown-linux-gnu"));
    assert!(manifest.targets.contains_key("x86_64-pc-windows-msvc"));
    assert_eq!(manifest.root_slots, ["fixture"]);
}

#[test]
fn stream_chunk_barrier_is_bounded_and_shutdown_discards_held_replies() {
    let (mut fixture, events) = ResourceFixture::new();
    let read = Message::StreamRead(StreamRead {
        stream: 937,
        offset: 13,
        len: 7,
    });
    let chunk = Message::StreamChunk(StreamChunk::Data {
        data: CborBytes(vec![7, 9, 1, 3]),
    });
    fixture
        .push(ResourceStep {
            request: read.clone(),
            replies: vec![
                Reply {
                    message: chunk.clone(),
                    correlation: None,
                    held: true
                };
                CAPACITY
            ],
        })
        .unwrap();
    assert!(fixture.dispatch(read.clone(), 7913).unwrap().is_empty());
    events.recv_timeout(WAIT).unwrap();
    assert_eq!(fixture.pending(), CAPACITY);
    fixture
        .push(ResourceStep {
            request: read.clone(),
            replies: vec![Reply {
                message: chunk.clone(),
                correlation: None,
                held: true,
            }],
        })
        .unwrap();
    assert!(fixture.dispatch(read.clone(), 8927).is_err());
    assert_eq!(fixture.remaining(), 1);
    assert_eq!(
        fixture.release(),
        Some(Envelope {
            message: chunk,
            correlation: 7913
        })
    );
    assert!(fixture.dispatch(read, 8927).unwrap().is_empty());
    assert_eq!(fixture.pending(), CAPACITY);
    fixture.shutdown();
    assert_eq!(fixture.pending(), 0);
    assert!(fixture.release().is_none());
    assert_eq!(fixture.remaining(), 0);
}
