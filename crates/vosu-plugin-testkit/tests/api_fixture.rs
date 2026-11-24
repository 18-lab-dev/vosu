//! Exercise the actual development fixture implementation through ABI host sends.
#![cfg(feature = "test-harness")]
use std::ffi::c_void;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::time::Duration;
use vosu_plugin_api::*;
use vosu_plugin_testkit::{
    fixture::{ApiFixturePlugin, STREAM_BYTES},
    resources::Envelope,
};

unsafe extern "C" fn record(ctx: *mut c_void, raw: *const vosu_message) -> u32 {
    // SAFETY: the local sender and raw payload outlive this synchronous callback.
    let sender = unsafe { &*ctx.cast::<SyncSender<Envelope>>() };
    // SAFETY: SDK provides a live message and payload for this callback.
    let raw = unsafe { &*raw };
    // SAFETY: SDK owns exactly payload.len initialized bytes until we return.
    let bytes = unsafe { std::slice::from_raw_parts(raw.payload.data, raw.payload.len as usize) };
    let message =
        Message::decode(vosu_opcode::try_from(raw.opcode).unwrap(), raw.flags, bytes).unwrap();
    match sender.try_send(Envelope {
        message,
        correlation: raw.correlation_id,
    }) {
        Ok(()) => vosu_status::VOSU_OK as u32,
        Err(_) => vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32,
    }
}

#[test]
fn daemon_fixture_same_pid_instances_keep_streams_actions_roots_and_shutdown_independent() {
    let host = vosu_host_services {
        struct_size: size_of::<vosu_host_services>() as u32,
        abi_version: VOSU_PLUGIN_ABI_MAJOR,
        send: Some(record),
    };
    let (mut tx1, rx1) = sync_channel(32);
    let (mut tx2, rx2) = sync_channel(32);
    // SAFETY: host and both senders remain live until both plugins drop below.
    let host1 = unsafe { VtableHostServices::new(&host, (&raw mut tx1).cast()) };
    // SAFETY: same lifetime guarantee, separate context for the second instance.
    let host2 = unsafe { VtableHostServices::new(&host, (&raw mut tx2).cast()) };
    let init = |dir| Init {
        plugin_id: "test.api.fixture".into(),
        settings: CborValue::Null,
        data_dir: std::env::temp_dir().join(dir),
    };
    let mut first = ApiFixturePlugin::create(host1, init("fixture-first-7913")).unwrap();
    let mut second = ApiFixturePlugin::create(host2, init("fixture-second-8927")).unwrap();
    let attach = Message::SessionAttached(SessionAttached {
        session: "fixture-opaque-937".into(),
        handle: 715,
        generation: 19,
        pid: 424242,
        bitness: Bitness::X64,
        name: None,
        path: None,
        command_line: None,
        argv: None,
    });
    first.on_message(attach.clone(), 0).unwrap();
    second.on_message(attach, 0).unwrap();
    let recv =
        |rx: &std::sync::mpsc::Receiver<Envelope>| rx.recv_timeout(Duration::from_secs(2)).unwrap();
    assert!(matches!(recv(&rx1).message, Message::SessionIdentify(_)));
    assert!(matches!(recv(&rx2).message, Message::SessionIdentify(_)));
    let root1 = recv(&rx1);
    let root2 = recv(&rx2);
    assert!(
        matches!(root1.message, Message::RootDeclare(RootDeclare { path, .. }) if path.ends_with("fixture-first-7913"))
    );
    assert!(
        matches!(root2.message, Message::RootDeclare(RootDeclare { path, .. }) if path.ends_with("fixture-second-8927"))
    );
    assert!(matches!(recv(&rx1).message, Message::PublishBatch(_)));
    assert!(matches!(recv(&rx2).message, Message::PublishBatch(_)));
    let resolve = Message::ResourceResolve(ResourceResolve {
        handle: 715,
        kind: "fixture-stream".into(),
        key: CborValue::Text("stream".into()),
    });
    first.on_message(resolve.clone(), 77).unwrap();
    second.on_message(resolve, 88).unwrap();
    let one = recv(&rx1);
    let two = recv(&rx2);
    assert_eq!((one.correlation, two.correlation), (77, 88));
    let Message::ResourceResolved(ResourceResolved::Stream { stream, size }) = one.message else {
        panic!("expected generated stream");
    };
    assert_eq!(size, STREAM_BYTES.len() as u64);
    assert!(
        matches!(two.message, Message::ResourceResolved(ResourceResolved::Stream { stream: other, .. }) if other == stream)
    );
    first
        .on_message(Message::StreamClose(StreamClose { stream }), 0)
        .unwrap();
    let read = Message::StreamRead(StreamRead {
        stream,
        offset: 3,
        len: 7,
    });
    assert_eq!(
        first.on_message(read.clone(), 99),
        Err(PluginError(vosu_status::VOSU_ERR_CANCELLED))
    );
    second.on_message(read, 199).unwrap();
    assert_eq!(
        recv(&rx2),
        Envelope {
            correlation: 199,
            message: Message::StreamChunk(StreamChunk::Data {
                data: CborBytes(STREAM_BYTES[3..10].to_vec())
            })
        }
    );
    // Any range order, including beyond-EOF, keeps the exact correlation.
    second
        .on_message(
            Message::StreamRead(StreamRead {
                stream,
                offset: u64::MAX,
                len: 17,
            }),
            299,
        )
        .unwrap();
    assert_eq!(
        recv(&rx2),
        Envelope {
            correlation: 299,
            message: Message::StreamChunk(StreamChunk::Data {
                data: CborBytes(vec![])
            })
        }
    );
    second
        .on_message(
            Message::ResourceResolve(ResourceResolve {
                handle: 715,
                kind: "fixture-file".into(),
                key: CborValue::Text("file".into()),
            }),
            399,
        )
        .unwrap();
    assert_eq!(
        recv(&rx2),
        Envelope {
            correlation: 399,
            message: Message::ResourceResolved(ResourceResolved::Path {
                root: "fixture".into(),
                path: "fixture.osu".into()
            })
        }
    );
    second
        .on_message(
            Message::TreeList(TreeList {
                handle: 715,
                kind: "fixture-tree".into(),
                key: CborValue::Text("tree".into()),
            }),
            499,
        )
        .unwrap();
    assert!(
        matches!(recv(&rx2).message, Message::ResourceResolved(ResourceResolved::Tree { entries, truncated: false, .. }) if entries[0].path == "fixture.osu" && entries[0].size == 36)
    );
    second
        .on_message(
            Message::TreeLookup(TreeLookup {
                handle: 715,
                kind: "fixture-tree".into(),
                key: CborValue::Text("tree".into()),
                path: "alias.osu".into(),
            }),
            599,
        )
        .unwrap();
    assert!(
        matches!(recv(&rx2).message, Message::ResourceResolved(ResourceResolved::TreePath { entry_path, .. }) if entry_path == "fixture.osu")
    );
    for (action, invocation) in [("run", 39), ("fail", 49)] {
        second
            .on_message(
                Message::ActionInvoke(ActionInvoke {
                    invocation,
                    action: format!("test.api.fixture.{action}"),
                    session: None,
                    caller: Caller {
                        kind: CallerKind::Cli,
                        id: "cli-937".into(),
                    },
                    args: CborValue::Map(vec![]),
                }),
                0,
            )
            .unwrap();
        if action == "run" {
            assert!(matches!(
                recv(&rx2).message,
                Message::ActionProgress(ActionProgress {
                    invocation: 39,
                    fraction: Some(0.375),
                    ..
                })
            ));
            assert!(matches!(
                recv(&rx2).message,
                Message::ActionResult(ActionResult {
                    invocation: 39,
                    status: ActionStatus::Ok,
                    ..
                })
            ));
        } else {
            assert!(
                matches!(recv(&rx2).message, Message::ActionResult(ActionResult { invocation: 49, status: ActionStatus::Error, code: Some(code), .. }) if code == "fixture_failed")
            );
        }
    }
    let invoke = Message::ActionInvoke(ActionInvoke {
        invocation: 29,
        action: "test.api.fixture.run".into(),
        session: None,
        caller: Caller {
            kind: CallerKind::Tool,
            id: "owner-937".into(),
        },
        args: CborValue::Map(vec![(
            CborValue::Text("mode".into()),
            CborValue::Text("wait".into()),
        )]),
    });
    first.on_message(invoke.clone(), 0).unwrap();
    second.on_message(invoke, 0).unwrap();
    first
        .on_message(Message::ActionCancel(ActionCancel { invocation: 29 }), 0)
        .unwrap();
    assert!(
        matches!(recv(&rx1).message, Message::ActionResult(ActionResult { invocation: 29, code: Some(code), .. }) if code == "cancelled")
    );
    drop(first);
    second
        .on_message(Message::ActionCancel(ActionCancel { invocation: 29 }), 0)
        .unwrap();
    assert!(
        matches!(recv(&rx2).message, Message::ActionResult(ActionResult { invocation: 29, code: Some(code), .. }) if code == "cancelled")
    );
    drop(second);
}
