//! `Message` decode/encode round trips (`src/message.rs`), spanning both
//! the flat hot-path opcodes and a representative CBOR opcode, plus
//! `HostServices`'s default methods over a mock `send`.

use std::cell::RefCell;

use vosu_plugin_api::{
    Bitness, CborValue, DemandChanged, FieldSelector, FocusChanged, HostServices, Message,
    PlanBase, PlanOp, PublishBatch, PublishField, PublishValue, ReadPlanResult, ReadPlanSubmit,
    ReadResult, SessionAttached, Stage, StageBase, VOSU_READ_PLAN_SYNC, vosu_opcode,
};

/// Round-trips `message` through `encode_payload`/`decode` with `message`'s
/// own opcode and flags, and asserts the decoded value equals the original.
fn roundtrip(message: Message) {
    let opcode = message.opcode();
    let flags = message.flags();
    let payload = message.encode_payload().expect("encode");
    let decoded = Message::decode(opcode, flags, &payload).expect("decode");
    assert_eq!(decoded, message);
}

#[test]
fn roundtrip_publish_field_every_value_kind() {
    for value in [
        PublishValue::Null,
        PublishValue::Bool(true),
        PublishValue::I64(-7),
        PublishValue::U64(7),
        PublishValue::F64(1.5),
        PublishValue::String("hello".into()),
        PublishValue::Bytes(vec![1, 2, 3]),
        PublishValue::Cbor(CborValue::Array(vec![CborValue::Integer(1.into())])),
    ] {
        roundtrip(Message::PublishField(PublishField {
            handle: 1,
            ns: "vosu.osu".into(),
            pointer: "/play/pp/current".into(),
            value,
        }));
    }
}

#[test]
fn roundtrip_publish_batch() {
    roundtrip(Message::PublishBatch(PublishBatch {
        handle: 1,
        entries: vec![
            PublishField {
                handle: 1,
                ns: "vosu.osu".into(),
                pointer: "/play/combo".into(),
                value: PublishValue::U64(42),
            },
            PublishField {
                handle: 1,
                ns: "vosu.osu".into(),
                pointer: "/play/pp/current".into(),
                value: PublishValue::F64(123.4),
            },
        ],
    }));
    roundtrip(Message::PublishBatch(PublishBatch {
        handle: 1,
        entries: vec![],
    }));
}

#[test]
fn roundtrip_demand_changed() {
    roundtrip(Message::DemandChanged(DemandChanged {
        handle: 1,
        regular: true,
        precise: false,
        selectors: vec![
            FieldSelector {
                ns: "vosu.osu".into(),
                pointer: "/play/pp/current".into(),
            },
            FieldSelector {
                ns: "vosu.play".into(),
                pointer: String::new(),
            },
        ],
    }));
}

#[test]
fn roundtrip_read_plan_submit_absolute_and_module_base() {
    roundtrip(Message::ReadPlanSubmit(ReadPlanSubmit {
        plan: 1,
        handle: 1,
        root: PlanBase::Absolute(0x1000),
        stages: vec![Stage {
            base: StageBase::Root,
            base_offset: 0x10,
            ops: vec![PlanOp {
                offset: 0,
                length: 8,
            }],
        }],
        sync: false,
    }));

    roundtrip(Message::ReadPlanSubmit(ReadPlanSubmit {
        plan: 2,
        handle: 1,
        root: PlanBase::Module {
            name: "osu!.exe".into(),
            offset: 0x2000,
        },
        stages: vec![
            Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops: vec![
                    PlanOp {
                        offset: 0,
                        length: 8,
                    },
                    PlanOp {
                        offset: 8,
                        length: 4,
                    },
                ],
            },
            Stage {
                base: StageBase::Dependent {
                    ref_stage: 0,
                    ref_op: 0,
                },
                base_offset: 0x18,
                ops: vec![PlanOp {
                    offset: 0,
                    length: 4,
                }],
            },
        ],
        sync: true,
    }));
}

#[test]
fn read_plan_submit_sync_flag_round_trips_through_vosu_message_flags() {
    let message = Message::ReadPlanSubmit(ReadPlanSubmit {
        plan: 1,
        handle: 1,
        root: PlanBase::Absolute(0),
        stages: vec![],
        sync: true,
    });
    assert_eq!(message.flags(), VOSU_READ_PLAN_SYNC);
    let payload = message.encode_payload().expect("encode");
    let decoded = Message::decode(
        vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT,
        VOSU_READ_PLAN_SYNC,
        &payload,
    )
    .expect("decode");
    assert_eq!(decoded, message);
}

#[test]
fn roundtrip_read_plan_result() {
    roundtrip(Message::ReadPlanResult(ReadPlanResult {
        plan: 1,
        results: vec![
            ReadResult::Ok(vec![1, 2, 3, 4]),
            ReadResult::NotReady,
            ReadResult::Unsupported,
            ReadResult::Error {
                code: vosu_plugin_api::ReadErrorCode::ReadFailed,
                message: "boom".into(),
            },
        ],
    }));
    roundtrip(Message::ReadPlanResult(ReadPlanResult {
        plan: 1,
        results: vec![],
    }));
}

#[test]
fn roundtrip_a_cbor_opcode_through_the_same_message_api() {
    roundtrip(Message::SessionAttached(SessionAttached {
        session: "s-1".into(),
        handle: 1,
        generation: 1,
        pid: 1234,
        bitness: Bitness::X64,
        name: Some("osu!.exe".into()),
        path: Some("C:/osu/osu!.exe".into()),
        command_line: Some("osu!.exe".into()),
        argv: None,
    }));
    roundtrip(Message::FocusChanged(FocusChanged {
        handle: 1,
        focused: true,
        foreground: None,
    }));
}

/// A mock `HostServices` recording every message it was asked to send,
/// alongside the `correlation_id` each call carried (B1).
struct RecordingHost {
    sent: RefCell<Vec<(Message, u64)>>,
}

impl HostServices for RecordingHost {
    fn send(&self, message: &Message, correlation_id: u64) -> vosu_plugin_api::vosu_status {
        self.sent
            .borrow_mut()
            .push((message.clone(), correlation_id));
        vosu_plugin_api::vosu_status::VOSU_OK
    }
}

#[test]
fn host_services_default_methods_build_the_right_message() {
    let host = RecordingHost {
        sent: RefCell::new(Vec::new()),
    };
    host.publish(1, "vosu.osu", "/play/combo", PublishValue::U64(1));
    host.identify_session(1, "osu", "stable", "name", "1.0", true);
    host.log(
        vosu_plugin_api::LogLevel::Info,
        "hello",
        CborValue::Map(vec![]),
    );
    host.cancel_timer(9);

    let sent = host.sent.into_inner();
    assert_eq!(sent.len(), 4);
    assert_eq!(sent[0].0.opcode(), vosu_opcode::VOSU_OP_PUBLISH_FIELD);
    assert_eq!(sent[1].0.opcode(), vosu_opcode::VOSU_OP_SESSION_IDENTIFY);
    assert_eq!(sent[2].0.opcode(), vosu_opcode::VOSU_OP_LOG_RECORD);
    assert_eq!(sent[3].0.opcode(), vosu_opcode::VOSU_OP_TIMER_CANCEL);
}

/// minor 6: a non-zero sub-millisecond interval is rejected before
/// anything is sent, rather than silently truncated to `interval_ms: 0`.
#[test]
fn set_timer_rejects_a_sub_millisecond_nonzero_interval() {
    let host = RecordingHost {
        sent: RefCell::new(Vec::new()),
    };
    let status = host.set_timer(1, Some(std::time::Duration::from_micros(500)));
    assert_eq!(
        status,
        vosu_plugin_api::vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    assert!(host.sent.into_inner().is_empty());
}

/// minor 6: an interval that does not fit in `u32` milliseconds saturates
/// instead of wrapping.
#[test]
fn set_timer_saturates_an_overlong_interval() {
    let host = RecordingHost {
        sent: RefCell::new(Vec::new()),
    };
    let status = host.set_timer(1, Some(std::time::Duration::from_secs(u64::from(u32::MAX))));
    assert_eq!(status, vosu_plugin_api::vosu_status::VOSU_OK);
    let Message::TimerSet(timer_set) = &host.sent.into_inner()[0].0 else {
        panic!("expected a TimerSet message");
    };
    assert_eq!(timer_set.interval_ms, Some(u32::MAX));
}

/// minor 6: a non-UTF-8 path is rejected rather than lossily substituted.
#[cfg(unix)]
#[test]
fn declare_root_rejects_a_non_utf8_path() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let host = RecordingHost {
        sent: RefCell::new(Vec::new()),
    };
    let path = std::path::Path::new(OsStr::from_bytes(b"/tmp/not-\xFF-utf8"));
    let status = host.declare_root(None, "root", path);
    assert_eq!(
        status,
        vosu_plugin_api::vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    assert!(host.sent.into_inner().is_empty());
}
