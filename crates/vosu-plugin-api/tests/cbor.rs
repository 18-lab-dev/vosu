//! CBOR codec tests (`src/cbor/*`, D13, `docs/v2/plugin-abi.md` §6.2-§6.12):
//! encode/decode round trips for every typed message, the universal
//! `VOSU_FRAME_MAX_BYTES` cap, unknown-field tolerance (additive growth,
//! §1), and ill-typed-field rejection.

use vosu_plugin_api::{
    ActionCall, ActionCancel, ActionInvoke, ActionProgress, ActionResult, ActionStatus, Bitness,
    Caller, CallerKind, CborValue, DetachReason, ErrorInfo, FocusChanged, LogLevel, LogRecord,
    ReadPlanCancel, ResourceRef, ResourceResolve, ResourceResolved, RootDeclare, ScanRequest,
    ScanResult, Selector, SessionAttached, SessionDecline, SessionDetached, SessionIdentify,
    SettingsChanged, StateEntry, StateSubscribe, StateUpdate, StreamChunk, StreamClose, StreamRead,
    TimerCancel, TimerFired, TimerSet, TreeEntry, TreeList, TreeLookup, VOSU_FRAME_MAX_BYTES,
    decode, encode,
};

/// Round-trips `value` through `encode`/`decode` and asserts the decoded
/// value equals the original.
fn roundtrip<T>(value: T)
where
    T: std::fmt::Debug + PartialEq + serde::Serialize + serde::de::DeserializeOwned,
{
    let bytes = encode(&value).expect("encode");
    let decoded: T = decode(&bytes).expect("decode");
    assert_eq!(decoded, value);
}

#[test]
fn roundtrip_session_attached() {
    roundtrip(SessionAttached {
        session: "s-1".into(),
        handle: 7,
        generation: 2,
        pid: 1234,
        bitness: Bitness::X64,
        name: Some("osu!.exe".into()),
        path: Some("C:/osu/osu!.exe".into()),
        command_line: Some("osu!.exe".into()),
        argv: None,
    });
    roundtrip(SessionAttached {
        session: "s-2".into(),
        handle: 8,
        generation: 1,
        pid: 1,
        bitness: Bitness::X86,
        name: Some("osu!".into()),
        path: Some("/home/user/osu!".into()),
        command_line: Some("osu! --tournament".into()),
        argv: Some(vec!["osu!".into(), "--tournament".into()]),
    });
}

/// A1b/DA: an older-shape `SESSION_ATTACHED` (minted before the amendment,
/// no `name`/`path`/`command_line`/`argv` keys at all) still decodes,
/// with the four additive fields defaulted to `None` — absent means
/// unknown, never the empty string (§6.2 amended): a missing key must
/// not collapse to `""`.
#[test]
fn session_attached_decodes_pre_amendment_shape() {
    let bytes = encode(&CborValue::Map(vec![
        (
            CborValue::Text("session".into()),
            CborValue::Text("s-1".into()),
        ),
        (
            CborValue::Text("handle".into()),
            CborValue::Integer(7.into()),
        ),
        (
            CborValue::Text("generation".into()),
            CborValue::Integer(2.into()),
        ),
        (
            CborValue::Text("pid".into()),
            CborValue::Integer(1234.into()),
        ),
        (
            CborValue::Text("bitness".into()),
            CborValue::Text("x64".into()),
        ),
    ]))
    .expect("encode");
    let decoded: SessionAttached = decode(&bytes).expect("decode pre-amendment shape");
    assert_eq!(
        decoded,
        SessionAttached {
            session: "s-1".into(),
            handle: 7,
            generation: 2,
            pid: 1234,
            bitness: Bitness::X64,
            name: None,
            path: None,
            command_line: None,
            argv: None,
        }
    );
}

/// §1/§6.2: an absent `argv` is omitted from the encoded map entirely,
/// never sent as an explicit CBOR `null` — `skip_serializing_if` must
/// actually drop the key, not just let a reader tolerate it either way.
#[test]
fn session_attached_omits_absent_argv_key_entirely() {
    let bytes = encode(&SessionAttached {
        session: "s-1".into(),
        handle: 7,
        generation: 2,
        pid: 1234,
        bitness: Bitness::X64,
        name: None,
        path: None,
        command_line: None,
        argv: None,
    })
    .expect("encode");
    let decoded: CborValue = decode(&bytes).expect("decode as a generic CBOR value");
    let CborValue::Map(entries) = decoded else {
        panic!("expected a CBOR map, got {decoded:?}");
    };
    assert!(
        !entries
            .iter()
            .any(|(key, _)| matches!(key, CborValue::Text(text) if text == "argv")),
        "an absent argv must omit the key entirely, not send it as null: {entries:?}"
    );
}

#[test]
fn roundtrip_session_detached() {
    for reason in [
        DetachReason::Exited,
        DetachReason::Lost,
        DetachReason::Superseded,
        DetachReason::Declined,
    ] {
        roundtrip(SessionDetached { handle: 7, reason });
    }
}

#[test]
fn roundtrip_session_identify_focus_priority_omitted_on_false() {
    let identify = SessionIdentify {
        handle: 7,
        game: "osu".into(),
        variant: "stable".into(),
        name: "name".into(),
        version: "1.0".into(),
        focus_priority: false,
    };
    let bytes = encode(&identify).expect("encode");
    let value: CborValue = decode(&bytes).expect("decode as value");
    let CborValue::Map(entries) = value else {
        panic!("expected a map");
    };
    assert!(
        entries
            .iter()
            .all(|(key, _)| key.as_text() != Some("focus_priority")),
        "omitting focus_priority: false must not write the key at all"
    );
    roundtrip(identify);

    roundtrip(SessionIdentify {
        focus_priority: true,
        ..SessionIdentify {
            handle: 1,
            game: "g".into(),
            variant: "v".into(),
            name: "n".into(),
            version: "1".into(),
            focus_priority: false,
        }
    });
}

#[test]
fn roundtrip_session_decline_and_focus_changed() {
    roundtrip(SessionDecline { handle: 42 });
    roundtrip(FocusChanged {
        handle: 42,
        focused: true,
        foreground: None,
    });
    roundtrip(FocusChanged {
        handle: 42,
        focused: true,
        foreground: Some(true),
    });
}

/// An older-shape `FOCUS_CHANGED` message (pre-ME, no `foreground` key at
/// all) still decodes, with `foreground` defaulting to `None` rather than
/// collapsing to `Some(false)` (ME's additive, backward-compatible
/// amendment).
#[test]
fn focus_changed_decodes_without_foreground() {
    let bytes = encode(&CborValue::Map(vec![
        (
            CborValue::Text("handle".into()),
            CborValue::Integer(7.into()),
        ),
        (CborValue::Text("focused".into()), CborValue::Bool(true)),
    ]))
    .expect("encode");
    let decoded: FocusChanged = decode(&bytes).expect("decode despite missing foreground key");
    assert_eq!(
        decoded,
        FocusChanged {
            handle: 7,
            focused: true,
            foreground: None,
        }
    );
}

#[test]
fn roundtrip_scan() {
    roundtrip(ScanRequest {
        session: 1,
        module: Some("osu!.exe".into()),
        pattern: "AA BB ?? CC".into(),
        non_zero_mask: true,
        all_matches: false,
    });
    roundtrip(ScanRequest {
        session: 1,
        module: None,
        pattern: "AA".into(),
        non_zero_mask: false,
        all_matches: true,
    });
    roundtrip(ScanResult {
        addresses: vec![1, 2, 3],
        error: None,
    });
    roundtrip(ScanResult {
        addresses: vec![],
        error: Some(ErrorInfo {
            code: "backend".into(),
            message: "boom".into(),
        }),
    });
}

#[test]
fn roundtrip_resource_ref_and_root_declare() {
    roundtrip(ResourceRef {
        kind: "beatmap-background".into(),
        key: CborValue::Text("key".into()),
        name: "bg.jpg".into(),
        content_id: CborValue::Integer(7.into()),
    });
    roundtrip(RootDeclare {
        handle: Some(1),
        name: "songs".into(),
        path: "/home/user/osu/Songs".into(),
    });
    roundtrip(RootDeclare {
        handle: None,
        name: "songs".into(),
        path: "/home/user/osu/Songs".into(),
    });
}

#[test]
fn roundtrip_resource_resolve_family() {
    roundtrip(ResourceResolve {
        handle: 1,
        kind: "file".into(),
        key: CborValue::Text("k".into()),
    });
    roundtrip(TreeList {
        handle: 1,
        kind: "set".into(),
        key: CborValue::Null,
    });
    roundtrip(TreeLookup {
        handle: 1,
        kind: "set".into(),
        key: CborValue::Null,
        path: "a/b.osu".into(),
    });
}

#[test]
fn roundtrip_resource_resolved_shapes() {
    roundtrip(ResourceResolved::Path {
        root: "songs".into(),
        path: "a/b.jpg".into(),
    });
    roundtrip(ResourceResolved::Stream {
        stream: 9,
        size: 1024,
    });
    roundtrip(ResourceResolved::Tree {
        entries: vec![
            TreeEntry {
                path: "a.osu".into(),
                size: 10,
                hash: Some("abc".into()),
            },
            TreeEntry {
                path: "b.osu".into(),
                size: 20,
                hash: None,
            },
        ],
        truncated: true,
        revision: "r1".into(),
    });
    roundtrip(ResourceResolved::Error {
        error: ErrorInfo {
            code: "not_found".into(),
            message: "missing".into(),
        },
    });
}

#[test]
fn tree_entry_omits_hash_entirely_when_unknown() {
    let entry = TreeEntry {
        path: "a.osu".into(),
        size: 1,
        hash: None,
    };
    let bytes = encode(&entry).expect("encode");
    let value: CborValue = decode(&bytes).expect("decode as value");
    let CborValue::Map(entries) = value else {
        panic!("expected a map");
    };
    assert!(
        entries.iter().all(|(key, _)| key.as_text() != Some("hash")),
        "an entry with unknown hashing omits `hash` entirely rather than sending it null"
    );
}

#[test]
fn roundtrip_stream_messages() {
    roundtrip(StreamRead {
        stream: 1,
        offset: 0,
        len: 4096,
    });
    roundtrip(StreamChunk::Data {
        data: vosu_plugin_api::CborBytes(vec![1, 2, 3]),
    });
    roundtrip(StreamChunk::Error {
        error: ErrorInfo {
            code: "cancelled".into(),
            message: "closed".into(),
        },
    });
    roundtrip(StreamClose { stream: 1 });
}

#[test]
fn stream_chunk_data_is_a_cbor_byte_string_not_an_array() {
    let bytes = encode(&StreamChunk::Data {
        data: vosu_plugin_api::CborBytes(vec![9, 8, 7]),
    })
    .expect("encode");
    let value: CborValue = decode(&bytes).expect("decode as value");
    let CborValue::Map(entries) = value else {
        panic!("expected a map");
    };
    let (_, data_value) = entries
        .into_iter()
        .find(|(k, _)| k.as_text() == Some("data"))
        .expect("data key present");
    assert_eq!(data_value, CborValue::Bytes(vec![9, 8, 7]));
}

#[test]
fn roundtrip_action_messages() {
    roundtrip(ActionInvoke {
        invocation: 1,
        session: Some(2),
        caller: Caller {
            kind: CallerKind::Tool,
            id: "tool-id".into(),
        },
        args: CborValue::Null,
    });
    roundtrip(ActionInvoke {
        invocation: 1,
        session: None,
        caller: Caller {
            kind: CallerKind::Cli,
            id: "cli".into(),
        },
        args: CborValue::Map(vec![(
            CborValue::Text("a".into()),
            CborValue::Integer(1.into()),
        )]),
    });
    roundtrip(ActionProgress {
        invocation: 1,
        fraction: Some(0.5),
        message: Some("halfway".into()),
    });
    roundtrip(ActionProgress {
        invocation: 1,
        fraction: None,
        message: None,
    });
    roundtrip(ActionResult {
        invocation: 1,
        status: ActionStatus::Ok,
        value: Some(CborValue::Bool(true)),
        code: None,
        message: None,
    });
    roundtrip(ActionResult {
        invocation: 1,
        status: ActionStatus::Error,
        value: None,
        code: Some("cancelled".into()),
        message: Some("stopped".into()),
    });
    roundtrip(ActionCancel { invocation: 1 });
    roundtrip(ActionCall {
        invocation: 1,
        action: "vosu.osu.beatmap.import".into(),
        session: Some(1),
        args: CborValue::Null,
    });
}

#[test]
fn roundtrip_settings_log_timers() {
    roundtrip(SettingsChanged {
        values: CborValue::Map(vec![(
            CborValue::Text("poll_rate".into()),
            CborValue::Integer(60.into()),
        )]),
    });
    for level in [
        LogLevel::Trace,
        LogLevel::Debug,
        LogLevel::Info,
        LogLevel::Warn,
        LogLevel::Error,
    ] {
        roundtrip(LogRecord {
            level,
            message: "hello".into(),
            fields: CborValue::Map(vec![]),
        });
    }
    roundtrip(TimerSet {
        timer: 1,
        interval_ms: Some(16),
    });
    roundtrip(TimerSet {
        timer: 1,
        interval_ms: None,
    });
    roundtrip(TimerFired { timer: 1 });
    roundtrip(TimerCancel { timer: 1 });
}

#[test]
fn roundtrip_state_messages() {
    roundtrip(StateSubscribe {
        session: Some(1),
        selectors: vec![Selector {
            ns: "vosu.osu".into(),
            pointer: "/play/pp/current".into(),
        }],
    });
    roundtrip(StateSubscribe {
        session: None,
        selectors: vec![],
    });
    roundtrip(StateUpdate {
        session: 1,
        values: vec![
            StateEntry {
                ns: "vosu.osu".into(),
                pointer: "/play/pp/current".into(),
                value: Some(CborValue::Float(123.4)),
                error: None,
            },
            StateEntry {
                ns: "vosu.osu".into(),
                pointer: "/play/combo".into(),
                value: None,
                error: Some(ErrorInfo {
                    code: "value_too_large".into(),
                    message: "nope".into(),
                }),
            },
        ],
        more: false,
    });
}

#[test]
fn roundtrip_read_plan_cancel() {
    roundtrip(ReadPlanCancel { plan: 1 });
}

/// §6.12's absence rule: a `STATE_UPDATE` entry missing `value`/`error`
/// keys entirely decodes exactly as one carrying them explicitly `null`.
#[test]
fn state_entry_missing_keys_equal_explicit_null() {
    let explicit = CborValue::Map(vec![
        (
            CborValue::Text("ns".into()),
            CborValue::Text("vosu.osu".into()),
        ),
        (
            CborValue::Text("pointer".into()),
            CborValue::Text("/play/combo".into()),
        ),
        (CborValue::Text("value".into()), CborValue::Null),
        (CborValue::Text("error".into()), CborValue::Null),
    ]);
    let omitted = CborValue::Map(vec![
        (
            CborValue::Text("ns".into()),
            CborValue::Text("vosu.osu".into()),
        ),
        (
            CborValue::Text("pointer".into()),
            CborValue::Text("/play/combo".into()),
        ),
    ]);
    let explicit_bytes = encode(&explicit).expect("encode explicit");
    let omitted_bytes = encode(&omitted).expect("encode omitted");
    let from_explicit: StateEntry = decode(&explicit_bytes).expect("decode explicit");
    let from_omitted: StateEntry = decode(&omitted_bytes).expect("decode omitted");
    assert_eq!(from_explicit, from_omitted);
    assert_eq!(from_explicit.value, None);
    assert_eq!(from_explicit.error, None);
}

/// Additive, backward-compatible growth (§1) extends to a CBOR message's
/// own map: an unrecognized key is accepted and ignored, never rejected.
#[test]
fn unknown_field_is_accepted_and_ignored() {
    let mut entries = vec![
        (
            CborValue::Text("handle".into()),
            CborValue::Integer(1.into()),
        ),
        (CborValue::Text("focused".into()), CborValue::Bool(true)),
    ];
    entries.push((
        CborValue::Text("a_future_minor_revision_field".into()),
        CborValue::Text("ignored".into()),
    ));
    let bytes = encode(&CborValue::Map(entries)).expect("encode");
    let decoded: FocusChanged = decode(&bytes).expect("decode despite unknown key");
    assert_eq!(
        decoded,
        FocusChanged {
            handle: 1,
            focused: true,
            foreground: None,
        }
    );
}

/// An ill-typed field — here `bitness` outside its `"x86"|"x64"` literals —
/// is rejected, not silently coerced.
#[test]
fn ill_typed_enum_field_is_rejected() {
    let bytes = encode(&CborValue::Map(vec![
        (
            CborValue::Text("session".into()),
            CborValue::Text("s".into()),
        ),
        (
            CborValue::Text("handle".into()),
            CborValue::Integer(1.into()),
        ),
        (
            CborValue::Text("generation".into()),
            CborValue::Integer(1.into()),
        ),
        (CborValue::Text("pid".into()), CborValue::Integer(1.into())),
        (
            CborValue::Text("bitness".into()),
            CborValue::Text("arm64".into()),
        ),
    ]))
    .expect("encode");
    let result: Result<SessionAttached, _> = decode(&bytes);
    assert!(
        result.is_err(),
        "an unrecognized bitness literal must be rejected"
    );
}

/// A field present with the wrong CBOR major type (a string where `handle`
/// needs an integer) is rejected rather than coerced.
#[test]
fn ill_typed_scalar_field_is_rejected() {
    let bytes = encode(&CborValue::Map(vec![
        (
            CborValue::Text("handle".into()),
            CborValue::Text("not-a-number".into()),
        ),
        (CborValue::Text("focused".into()), CborValue::Bool(true)),
    ]))
    .expect("encode");
    let result: Result<FocusChanged, _> = decode(&bytes);
    assert!(result.is_err());
}

/// Every CBOR opcode shares the universal `VOSU_FRAME_MAX_BYTES` cap
/// (§3/§7.2): an encode that would exceed it is rejected before the bytes
/// are ever returned, and a decode of an over-cap buffer is rejected before
/// it is ever parsed.
#[test]
fn frame_cap_is_enforced_on_encode_and_decode() {
    let huge = CborValue::Bytes(vec![0u8; VOSU_FRAME_MAX_BYTES as usize + 1]);
    let oversize = SettingsChanged { values: huge };
    assert!(encode(&oversize).is_err());

    // A buffer already over the cap is rejected before parsing, even if it
    // happens to be valid CBOR that would decode to something small.
    let mut over_cap_bytes = vec![0xf6u8]; // CBOR `null`
    over_cap_bytes.resize(VOSU_FRAME_MAX_BYTES as usize + 1, 0xff);
    let result: Result<CborValue, _> = decode(&over_cap_bytes);
    assert!(result.is_err());
}

/// The spec says to omit `hash` entirely for unknown content hashing,
/// never send it explicit `null` (nits): an entry that does so is
/// rejected, not silently treated the same as an absent key.
#[test]
fn tree_entry_rejects_explicit_null_hash() {
    let bytes = encode(&CborValue::Map(vec![
        (
            CborValue::Text("path".into()),
            CborValue::Text("a.osu".into()),
        ),
        (CborValue::Text("size".into()), CborValue::Integer(1.into())),
        (CborValue::Text("hash".into()), CborValue::Null),
    ]))
    .expect("encode");
    let result: Result<TreeEntry, _> = decode(&bytes);
    assert!(
        result.is_err(),
        "hash: null must be rejected, not accepted as equivalent to omitting the key"
    );
}

/// `ResourceResolved` is `#[serde(untagged)]`: a reply mixing fields from
/// more than one shape (here `path`/`root` and `error`) must be rejected
/// rather than silently accepted as whichever variant needs only a subset
/// of what was sent (nits).
#[test]
fn resource_resolved_rejects_a_reply_mixing_path_and_error_shapes() {
    let bytes = encode(&CborValue::Map(vec![
        (
            CborValue::Text("root".into()),
            CborValue::Text("songs".into()),
        ),
        (
            CborValue::Text("path".into()),
            CborValue::Text("a/b.jpg".into()),
        ),
        (
            CborValue::Text("error".into()),
            CborValue::Map(vec![
                (
                    CborValue::Text("code".into()),
                    CborValue::Text("not_found".into()),
                ),
                (
                    CborValue::Text("message".into()),
                    CborValue::Text("missing".into()),
                ),
            ]),
        ),
    ]))
    .expect("encode");
    let result: Result<ResourceResolved, _> = decode(&bytes);
    assert!(
        result.is_err(),
        "a reply naming fields from both the Path and Error shapes must be rejected"
    );
}

/// `LogRecord.fields` and `SettingsChanged.values` are always CBOR maps
/// (minor 9): any other major type is rejected on decode.
#[test]
fn log_record_fields_and_settings_values_reject_non_map_values() {
    let bytes = encode(&CborValue::Map(vec![
        (
            CborValue::Text("level".into()),
            CborValue::Text("info".into()),
        ),
        (
            CborValue::Text("message".into()),
            CborValue::Text("hello".into()),
        ),
        (
            CborValue::Text("fields".into()),
            CborValue::Array(vec![CborValue::Integer(1.into())]),
        ),
    ]))
    .expect("encode");
    let result: Result<LogRecord, _> = decode(&bytes);
    assert!(result.is_err(), "fields: [1] (an array) must be rejected");

    let bytes = encode(&CborValue::Map(vec![(
        CborValue::Text("values".into()),
        CborValue::Text("not a map".into()),
    )]))
    .expect("encode");
    let result: Result<SettingsChanged, _> = decode(&bytes);
    assert!(result.is_err(), "values: a string must be rejected");
}
