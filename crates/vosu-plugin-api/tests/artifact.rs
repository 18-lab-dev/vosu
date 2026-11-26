//! Invocation-owned artifact codecs (`docs/v2/plugin-abi.md` §6.13, AF.12):
//! the four additive opcodes round-trip through both the typed CBOR codec
//! and the `Message` API, a declaration carries no path or session field,
//! a reply mixing both `ARTIFACT_DECLARED` shapes is rejected, the name
//! rule matches the header's bound, and `declare_artifact` refuses before
//! sending anything a host would refuse.

use std::cell::RefCell;

use vosu_plugin_api::{
    ArtifactDeclare, ArtifactDeclared, ArtifactOpen, ArtifactRelease, CborValue, ErrorInfo,
    HostServices, Message, ResourceResolved, VOSU_ARTIFACT_NAME_MAX_BYTES, decode, encode,
    is_artifact_name, vosu_opcode, vosu_status,
};

fn declaration(name: &str) -> ArtifactDeclare {
    ArtifactDeclare {
        invocation: 41,
        key: CborValue::Array(vec![
            CborValue::Text("render".into()),
            CborValue::Integer(7.into()),
        ]),
        name: name.into(),
        content_id: CborValue::Bytes(vec![0xab; 32]),
        size: 2 * 1024 * 1024 * 1024,
    }
}

fn roundtrip(message: Message) {
    let payload = message.encode_payload().expect("encode");
    let decoded = Message::decode(message.opcode(), message.flags(), &payload).expect("decode");
    assert_eq!(decoded, message);
}

#[test]
fn artifact_opcodes_are_additive_and_round_trip() {
    for (value, opcode) in [
        (80, vosu_opcode::VOSU_OP_ARTIFACT_DECLARE),
        (81, vosu_opcode::VOSU_OP_ARTIFACT_DECLARED),
        (82, vosu_opcode::VOSU_OP_ARTIFACT_OPEN),
        (83, vosu_opcode::VOSU_OP_ARTIFACT_RELEASE),
    ] {
        assert_eq!(opcode as u32, value);
        assert_eq!(vosu_opcode::try_from(value), Ok(opcode));
    }
    for unassigned in [72, 79, 84, 90] {
        assert!(vosu_opcode::try_from(unassigned).is_err(), "{unassigned}");
    }
    roundtrip(Message::ArtifactDeclare(declaration("replay.osr")));
    roundtrip(Message::ArtifactDeclared(ArtifactDeclared::Artifact {
        artifact: "a-opaque".into(),
    }));
    roundtrip(Message::ArtifactDeclared(ArtifactDeclared::Error {
        error: ErrorInfo {
            code: "too_many_artifacts".into(),
            message: "four per job".into(),
        },
    }));
    roundtrip(Message::ArtifactOpen(ArtifactOpen {
        artifact: "a-opaque".into(),
        key: CborValue::Text("render-7".into()),
    }));
    roundtrip(Message::ArtifactRelease(ArtifactRelease {
        artifact: "a-opaque".into(),
        key: CborValue::Text("render-7".into()),
    }));
    // The open reply is the ordinary resolve reply, never a new shape.
    roundtrip(Message::ResourceResolved(ResourceResolved::Stream {
        stream: 3,
        size: 2 * 1024 * 1024 * 1024,
    }));
}

/// The declaration's exact wire keys: no path, root, session or handle.
#[test]
fn declare_wire_shape_names_no_path_or_session() {
    let bytes = encode(&declaration("out.mp4")).unwrap();
    let CborValue::Map(entries) = decode::<CborValue>(&bytes).unwrap() else {
        panic!("a declaration is a map");
    };
    let mut keys: Vec<_> = entries
        .iter()
        .map(|(key, _)| key.as_text().unwrap().to_owned())
        .collect();
    keys.sort();
    assert_eq!(keys, ["content_id", "invocation", "key", "name", "size"]);
}

#[test]
fn declared_rejects_a_reply_mixing_both_shapes() {
    let bytes = encode(&CborValue::Map(vec![
        (
            CborValue::Text("artifact".into()),
            CborValue::Text("a".into()),
        ),
        (
            CborValue::Text("error".into()),
            CborValue::Map(vec![
                (
                    CborValue::Text("code".into()),
                    CborValue::Text("invalid_name".into()),
                ),
                (
                    CborValue::Text("message".into()),
                    CborValue::Text("x".into()),
                ),
            ]),
        ),
    ]))
    .unwrap();
    assert!(decode::<ArtifactDeclared>(&bytes).is_err());
}

#[test]
fn artifact_names_are_one_safe_component() {
    let longest = "a".repeat(VOSU_ARTIFACT_NAME_MAX_BYTES as usize);
    for valid in ["replay.osr", "Skin export (1).osk", "動画.mp4", longest.as_str()] {
        assert!(is_artifact_name(valid), "{valid:?}");
    }
    let too_long = format!("{longest}a");
    for invalid in [
        "",
        ".",
        "..",
        "a/b.osr",
        "..\\up.osr",
        "nul\0.osr",
        "line\nbreak.mp4",
        "\u{7f}.png",
        too_long.as_str(),
    ] {
        assert!(!is_artifact_name(invalid), "{invalid:?}");
    }
}

struct RecordingHost(RefCell<Vec<(Message, u64)>>);

impl HostServices for RecordingHost {
    fn send(&self, message: &Message, correlation_id: u64) -> vosu_status {
        self.0.borrow_mut().push((message.clone(), correlation_id));
        vosu_status::VOSU_OK
    }
}

#[test]
fn declare_artifact_requires_a_correlation_and_a_safe_name() {
    let host = RecordingHost(RefCell::new(Vec::new()));
    assert_eq!(
        host.declare_artifact(0, declaration("ok.osr")),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    assert_eq!(
        host.declare_artifact(9, declaration("../escape.osr")),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    assert!(host.0.borrow().is_empty());
    assert_eq!(
        host.declare_artifact(9, declaration("ok.osr")),
        vosu_status::VOSU_OK
    );
    let sent = host.0.into_inner();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].1, 9);
    assert_eq!(sent[0].0, Message::ArtifactDeclare(declaration("ok.osr")));
}
