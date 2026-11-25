use std::path::PathBuf;

use vosu_plugin_api::{CborBytes, CborValue, Init, encode};

use super::*;

fn create() -> Create {
    Create {
        generation: 7,
        plugin_id: "acme.fixture".to_owned(),
        library: PathBuf::from("/opt/plugins/acme/lib/libfixture.so"),
        library_sha256: [0xab; 32],
        concurrent_dispatch: true,
        sync_budget_ms: 2_000,
        init: Init {
            plugin_id: "acme.fixture".to_owned(),
            settings: CborValue::Map(vec![(
                CborValue::Text("level".into()),
                CborValue::Integer(3.into()),
            )]),
            data_dir: PathBuf::from("/var/lib/vosu/acme"),
        },
    }
}

#[test]
fn create_round_trips_and_reencodes_init_byte_for_byte() {
    let original = create();
    let decoded = Create::decode(&original.encode().unwrap()).unwrap();
    assert_eq!(decoded.generation, 7);
    assert_eq!(decoded.plugin_id, "acme.fixture");
    assert_eq!(decoded.library, original.library);
    assert_eq!(decoded.library_sha256, [0xab; 32]);
    assert!(decoded.concurrent_dispatch);
    assert_eq!(decoded.sync_budget_ms, 2_000);
    // The child passes `encode(&init)` to `create`: it must equal the bytes
    // the in-process path would pass.
    assert_eq!(
        encode(&decoded.init).unwrap(),
        encode(&original.init).unwrap()
    );
}

#[derive(serde::Serialize)]
struct LooseCreate {
    generation: u64,
    plugin_id: String,
    library: PathBuf,
    library_sha256: CborBytes,
    concurrent_dispatch: bool,
    sync_budget_ms: u32,
    init: Init,
    #[serde(skip_serializing_if = "Option::is_none")]
    extra: Option<u32>,
}

fn loose(digest: Vec<u8>, extra: Option<u32>) -> Vec<u8> {
    let base = create();
    encode(&LooseCreate {
        generation: base.generation,
        plugin_id: base.plugin_id,
        library: base.library,
        library_sha256: CborBytes(digest),
        concurrent_dispatch: base.concurrent_dispatch,
        sync_budget_ms: base.sync_budget_ms,
        init: base.init,
        extra,
    })
    .unwrap()
}

#[test]
fn create_refuses_a_short_digest_and_an_unknown_key() {
    assert!(Create::decode(&loose(vec![0; 32], None)).is_ok());
    assert!(matches!(
        Create::decode(&loose(vec![0; 31], None)),
        Err(ControlError::DigestLength(31))
    ));
    assert!(matches!(
        Create::decode(&loose(vec![0; 33], None)),
        Err(ControlError::DigestLength(33))
    ));
    assert!(matches!(
        Create::decode(&loose(vec![0; 32], Some(1))),
        Err(ControlError::Cbor(_))
    ));
    assert!(Create::decode(b"\xff").is_err());
}

#[test]
fn create_over_the_frame_cap_is_refused_at_encode() {
    let mut oversize = create();
    oversize.init.settings =
        CborValue::Bytes(vec![0; vosu_plugin_api::VOSU_FRAME_MAX_BYTES as usize]);
    assert!(matches!(oversize.encode(), Err(ControlError::Cbor(_))));
}

#[test]
fn ready_round_trips() {
    let ready = Ready {
        generation: 3,
        struct_size: 32,
        abi_version: 1,
    };
    assert_eq!(Ready::decode(&ready.encode()).unwrap(), ready);
    assert!(Ready::decode(b"").is_err());
}

#[test]
fn failed_round_trips_and_truncates_its_message_at_a_char_boundary() {
    let failed = Failed {
        generation: 0,
        stage: StartupStage::Security,
        code: "drop_failed".to_owned(),
        errno: Some(1),
        message: "é".repeat(400),
    };
    let decoded = Failed::decode(&failed.encode()).unwrap();
    assert_eq!(decoded.stage, StartupStage::Security);
    assert_eq!(decoded.errno, Some(1));
    assert!(decoded.message.len() <= MAX_FAILED_MESSAGE);
    assert_eq!(decoded.message.len(), MAX_FAILED_MESSAGE);
    assert!(failed.message.starts_with(&decoded.message));

    let no_errno = Failed {
        errno: None,
        ..failed
    };
    assert_eq!(Failed::decode(&no_errno.encode()).unwrap().errno, None);
}

#[derive(serde::Serialize)]
struct RawFailed<'a> {
    generation: u64,
    stage: &'a str,
    code: &'a str,
    message: &'a str,
}

fn raw_failed(stage: &str, code: &str, message: &str) -> Vec<u8> {
    encode(&RawFailed {
        generation: 1,
        stage,
        code,
        message,
    })
    .unwrap()
}

#[test]
fn failed_is_an_untrusted_claim() {
    for stage in ["security", "approval", "library", "abi", "create"] {
        assert!(
            Failed::decode(&raw_failed(stage, "x", "")).is_ok(),
            "{stage}"
        );
    }
    // Stages only core detects cannot be claimed by a child.
    for stage in [
        "spawn",
        "job",
        "arguments",
        "generation",
        "parent",
        "endpoints",
        "timeout",
        "crash",
        "",
    ] {
        assert!(
            matches!(
                Failed::decode(&raw_failed(stage, "x", "")),
                Err(ControlError::Stage(_))
            ),
            "{stage}"
        );
    }
    assert!(matches!(
        Failed::decode(&raw_failed(
            "create",
            "x",
            &"m".repeat(MAX_FAILED_MESSAGE + 1)
        )),
        Err(ControlError::MessageLength(_))
    ));
    assert!(Failed::decode(&raw_failed("create", "x", &"m".repeat(MAX_FAILED_MESSAGE))).is_ok());
    assert!(matches!(
        Failed::decode(&raw_failed("create", &"c".repeat(MAX_FAILED_CODE + 1), "")),
        Err(ControlError::CodeLength(_))
    ));
}

#[test]
fn pre_channel_exit_statuses_name_their_stage() {
    assert_eq!(
        StartupStage::from_exit_status(EXIT_ARGUMENTS),
        Some(StartupStage::Arguments)
    );
    assert_eq!(
        StartupStage::from_exit_status(EXIT_GENERATION),
        Some(StartupStage::Generation)
    );
    assert_eq!(
        StartupStage::from_exit_status(EXIT_PARENT),
        Some(StartupStage::Parent)
    );
    assert_eq!(
        StartupStage::from_exit_status(EXIT_ENDPOINTS),
        Some(StartupStage::Endpoints)
    );
    for other in [
        0,
        1,
        EXIT_REPORTED,
        EXIT_CHANNEL_LOST,
        EXIT_TRANSPORT_TIMEOUT,
        -9,
    ] {
        assert_eq!(StartupStage::from_exit_status(other), None, "{other}");
    }
}

#[test]
fn only_create_timeout_and_crash_count_as_restart_crashes() {
    use StartupStage::*;
    for stage in [Create, Timeout, Crash] {
        assert!(stage.is_crash_class(), "{stage:?}");
    }
    for stage in [
        Spawn, Job, Arguments, Generation, Parent, Endpoints, Security, Approval, Library, Abi,
    ] {
        assert!(!stage.is_crash_class(), "{stage:?}");
    }
}
