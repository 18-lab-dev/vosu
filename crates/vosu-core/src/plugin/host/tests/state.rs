use super::helpers::{
    RECV_TIMEOUT, create_session_instance, session_manifest, test_host, test_host_with,
    test_process,
};
use crate::plugin::{
    dispatch::WorkerPool,
    focus::SyntheticForegroundSource,
    host_services::{PluginHostHandler, PluginInstanceId},
    registry,
};
use crate::state::{Pointer, SessionKey, StateValue};
use std::sync::Arc;
use std::time::Duration;
use vosu_plugin_api::{
    HostServices, PublishBatchWriter, PublishValueRef, vosu_opcode, vosu_status,
};
use vosu_plugin_testkit::{
    publisher,
    session::{OnAttach, SessionEvent, SessionPluginConfig},
};

mod api;
mod surface;

fn publisher_instance(
    host: &Arc<super::super::PluginHost>,
    id: &str,
    instance: u64,
    pid: u32,
) -> u64 {
    publisher_instance_covering(host, id, instance, pid, "field:vosu.play.combo")
}

fn publisher_instance_covering(
    host: &Arc<super::super::PluginHost>,
    id: &str,
    instance: u64,
    pid: u32,
    coverage: &str,
) -> u64 {
    let manifest = session_manifest(id, &[id]).replace(
        "[match]",
        &format!("concurrent_dispatch=true\ncovers=['{coverage}']\n[match]"),
    );
    publisher::register_manifest(id, &manifest);
    let events = publisher::register(id);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &publisher::manifest(id),
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: publisher::vosu_plugin_vtable,
    }])
    .unwrap();
    host.create_instance(
        PluginInstanceId::new(instance),
        &loaded[0],
        std::env::temp_dir(),
        Some(WorkerPool::new(2, 64)),
    )
    .unwrap();
    host.process_appeared(test_process(pid, id));
    // Identify may create surface demand on another concurrent dispatch worker
    // before the publisher records its own attach event.
    let deadline = std::time::Instant::now() + RECV_TIMEOUT;
    loop {
        match events
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap()
        {
            publisher::PublisherEvent::Attached { handle } => break handle,
            publisher::PublisherEvent::Demand(_) => {}
            other => panic!("expected attach, got {other:?}"),
        }
    }
}
fn batch(handle: u64, fields: &[(&str, &str, PublishValueRef<'_>)]) -> Vec<u8> {
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    for &(ns, pointer, value) in fields {
        writer.push(ns, pointer, value);
    }
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    bytes
}
fn send(id: &str, bytes: &[u8]) -> vosu_status {
    publisher::host_services(id).send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, bytes)
}

#[test]
fn publish_before_identify_refused() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = PluginInstanceId::new(8401);
    let events = create_session_instance(
        &host,
        id,
        "test.k4.pending",
        &["k4-pending"],
        SessionPluginConfig {
            on_attach: OnAttach::Ignore,
            ..Default::default()
        },
    );
    host.process_appeared(test_process(8401, "k4-pending"));
    let SessionEvent::Attached(attached) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("expected attach")
    };
    let bytes = batch(
        attached.handle,
        &[("vosu.play", "/combo", PublishValueRef::U64(9))],
    );
    assert_eq!(
        host.handle_publish(id, vosu_opcode::VOSU_OP_PUBLISH_BATCH, &bytes),
        Some(vosu_status::VOSU_ERR_INVALID_ARGUMENT)
    );
    assert!(
        host.state()
            .store
            .current(SessionKey(attached.handle))
            .is_none()
    );
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn foreign_handle_permission_denied() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let owner = "test.k4.owner";
    let other = "test.k4.foreign";
    let h = publisher_instance(&host, owner, 8402, 8402);
    publisher_instance(&host, other, 8403, 8403);
    let before = host.state().store.current(SessionKey(h)).unwrap();
    assert_eq!(
        send(
            other,
            &batch(h, &[("vosu.play", "/combo", PublishValueRef::U64(9))])
        ),
        vosu_status::VOSU_ERR_PERMISSION_DENIED
    );
    assert!(Arc::ptr_eq(
        &before,
        &host.state().store.current(SessionKey(h)).unwrap()
    ));
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn detached_handle_target_gone() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.k4.detached";
    let h = publisher_instance(&host, id, 8404, 8404);
    host.process_exited(8404);
    assert!(host.state().store.current(SessionKey(h)).is_none());
    assert_eq!(
        send(
            id,
            &batch(h, &[("vosu.play", "/combo", PublishValueRef::U64(9))])
        ),
        vosu_status::VOSU_ERR_TARGET_GONE
    );
    assert_eq!(
        send(id, &batch(u64::MAX, &[])),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn uncovered_entry_rejects_whole_batch() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.k4.atomic";
    let h = publisher_instance(&host, id, 8405, 8405);
    let before = host.state().store.current(SessionKey(h)).unwrap();
    let bytes = batch(
        h,
        &[
            ("vosu.play", "/combo", PublishValueRef::U64(9)),
            ("vosu.play", "/accuracy", PublishValueRef::F64(0.9)),
        ],
    );
    assert_eq!(send(id, &bytes), vosu_status::VOSU_ERR_INVALID_ARGUMENT);
    assert!(Arc::ptr_eq(
        &before,
        &host.state().store.current(SessionKey(h)).unwrap()
    ));
    assert_eq!(
        send(
            id,
            &batch(h, &[("vosu.play", "/combo", PublishValueRef::U64(12))])
        ),
        vosu_status::VOSU_OK
    );
    assert_eq!(
        host.state()
            .store
            .current(SessionKey(h))
            .unwrap()
            .get("vosu.play", &Pointer::parse("/combo").unwrap()),
        Some(&StateValue::U64(12))
    );
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn vosu_client_filled_on_identify_and_focus_updates_it() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let fg = Arc::new(SyntheticForegroundSource::new(Some(8406)));
    let (_logs, host) = test_host_with(rt.handle().clone(), fg.clone(), Duration::from_secs(5));
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(8406),
        "test.k4.client",
        &["k4-client"],
        SessionPluginConfig::default(),
    );
    host.process_appeared(test_process(8406, "k4-client"));
    let SessionEvent::Attached(attached) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("attach")
    };
    let SessionEvent::Focus(first) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("focus")
    };
    assert!(first.focused);
    assert_eq!(first.foreground, Some(true));
    let state = host
        .state()
        .store
        .current(SessionKey(attached.handle))
        .unwrap();
    let json: serde_json::Value =
        serde_json::from_slice(state.api_json(&host.state().registry.read().unwrap())).unwrap();
    assert_eq!(
        json["vosu.client"],
        serde_json::json!({"game":"testkit","variant":"session","name":"Session Testkit","version":"0.1.0","pid":8406,"session":attached.session,"plugin":{"id":"test.k4.client","version":"0.1.0"},"focused":true})
    );
    // Another identified session wins focus when its pid enters foreground.
    let other = create_session_instance(
        &host,
        PluginInstanceId::new(8407),
        "test.k4.client-other",
        &["k4-client-other"],
        SessionPluginConfig::default(),
    );
    fg.set(Some(8407));
    host.process_appeared(test_process(8407, "k4-client-other"));
    other.recv_timeout(RECV_TIMEOUT).unwrap();
    other.recv_timeout(RECV_TIMEOUT).unwrap();
    let SessionEvent::Focus(loss) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("loss")
    };
    assert!(!loss.focused);
    assert_eq!(
        host.state()
            .store
            .current(SessionKey(attached.handle))
            .unwrap()
            .get("vosu.client", &Pointer::parse("/focused").unwrap()),
        Some(&StateValue::Bool(false))
    );
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn foreground_only_change_notifies_owner() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let fg = Arc::new(SyntheticForegroundSource::new(Some(8408)));
    let (_logs, host) = test_host_with(rt.handle().clone(), fg.clone(), Duration::from_secs(5));
    let events = create_session_instance(
        &host,
        PluginInstanceId::new(8408),
        "test.k4.foreground",
        &["k4-foreground"],
        SessionPluginConfig {
            focus_priority: true,
            ..Default::default()
        },
    );
    host.process_appeared(test_process(8408, "k4-foreground"));
    let SessionEvent::Attached(attached) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("attach")
    };
    events.recv_timeout(RECV_TIMEOUT).unwrap();
    let before = host
        .state()
        .store
        .current(SessionKey(attached.handle))
        .unwrap();
    fg.set(None);
    host.foreground_changed();
    let SessionEvent::Focus(event) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("focus")
    };
    assert!(event.focused);
    assert_eq!(event.foreground, Some(false));
    assert!(Arc::ptr_eq(
        &before,
        &host
            .state()
            .store
            .current(SessionKey(attached.handle))
            .unwrap()
    ));
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn namespace_schema_registration_refuses_visibility_and_unregisters_on_destroy() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.k4.schemas";
    let manifest = session_manifest(id, &["k4-schemas"]).replace(
        "[match]",
        "[[provides]]\nid='field:test.k4.schemas'\nversion='1.0.0'\nschema='schema.json'\n[match]",
    );
    let invalid = r#"{"title":"test.k4.schemas","version":"1.0.0","type":"object","properties":{"internal":{"type":"object","x-visibility":"plugins"}}}"#;
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest,
        settings_schema: None,
        namespace_schemas: &[invalid],
        action_schemas: &[],
        vtable: vosu_plugin_testkit::session::vosu_plugin_vtable,
    }])
    .unwrap();
    assert!(matches!(
        host.create_instance(
            PluginInstanceId::new(8409),
            &loaded[0],
            std::env::temp_dir(),
            None
        ),
        Err(crate::plugin::HostError::State(_))
    ));
    assert!(host.state().registry.read().unwrap().get(id).is_none());
    let valid = r#"{"title":"test.k4.schemas","version":"1.0.0","type":"object","properties":{}}"#;
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest,
        settings_schema: None,
        namespace_schemas: &[valid],
        action_schemas: &[],
        vtable: vosu_plugin_testkit::session::vosu_plugin_vtable,
    }])
    .unwrap();
    let _events = vosu_plugin_testkit::session::register(id);
    host.create_instance(
        PluginInstanceId::new(8409),
        &loaded[0],
        std::env::temp_dir(),
        None,
    )
    .unwrap();
    assert!(host.state().registry.read().unwrap().get(id).is_some());
    host.destroy_all(Duration::from_secs(5));
    assert!(host.state().registry.read().unwrap().get(id).is_none());
}

#[test]
fn development_schema_with_x_visibility_refuses_instance() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let dir = tempfile::tempdir().unwrap();
    let library = if cfg!(windows) {
        "vosu_plugin_testkit.dll"
    } else {
        "libvosu_plugin_testkit.so"
    };
    let source = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join(library);
    assert!(
        source.is_file(),
        "testkit cdylib must be built beside this test binary"
    );
    std::fs::copy(source, dir.path().join(library)).unwrap();
    let target = if cfg!(windows) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    };
    let manifest = format!(
        "kind='plugin'\nmanifest_version=1\nid='test.k4.development'\nname='Development'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n[[provides]]\nid='field:test.k4.development'\nversion='1.0.0'\nschema='schema.json'\n[targets.{target}]\nlibrary='{library}'\n"
    );
    std::fs::write(dir.path().join("manifest.toml"), manifest).unwrap();
    std::fs::write(dir.path().join("schema.json"), r#"{"title":"test.k4.development","version":"1.0.0","type":"object","properties":{"internal":{"type":"object","x-visibility":"plugins"}}}"#).unwrap();
    let loaded = registry::load_development(dir.path(), true).unwrap();
    assert_eq!(loaded.namespace_schemas().len(), 1);
    assert!(matches!(
        host.create_instance(
            PluginInstanceId::new(8411),
            &loaded,
            dir.path().to_owned(),
            None
        ),
        Err(crate::plugin::HostError::State(_))
    ));
    assert!(
        host.state()
            .registry
            .read()
            .unwrap()
            .get("test.k4.development")
            .is_none()
    );
}

#[cfg(unix)]
#[test]
fn development_schema_symlink_cannot_escape_plugin_directory() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::NamedTempFile::new().unwrap();
    std::os::unix::fs::symlink(outside.path(), dir.path().join("schema.json")).unwrap();
    std::fs::write(dir.path().join("manifest.toml"), "kind='plugin'\nmanifest_version=1\nid='test.k4.confine'\nname='Confine'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n[[provides]]\nid='field:test.k4.confine'\nversion='1.0.0'\nschema='schema.json'\n").unwrap();
    assert!(
        matches!(registry::load_development(dir.path(), true), Err(crate::plugin::HostError::Io(error)) if error.kind() == std::io::ErrorKind::PermissionDenied)
    );
}

#[test]
fn destroying_instance_closes_its_bus_sessions() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let h = publisher_instance(&host, "test.k4.shutdown", 8412, 8412);
    assert!(host.state().store.current(SessionKey(h)).is_some());
    host.destroy_all(Duration::from_secs(5));
    assert!(host.state().store.current(SessionKey(h)).is_none());
}

#[test]
fn publish_field_and_owning_fallback_share_commit_path() {
    use vosu_plugin_api::{Message, PublishBatch, PublishField, PublishValue};
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.k4.field";
    let h = publisher_instance(&host, id, 8413, 8413);
    let mut field = PublishField {
        handle: h,
        ns: "vosu.play".into(),
        pointer: "/combo".into(),
        value: PublishValue::U64(17),
    };
    assert_eq!(
        publisher::host_services(id).send(&Message::PublishField(field.clone()), 0),
        vosu_status::VOSU_OK
    );
    let key = SessionKey(h);
    let pointer = Pointer::parse("/combo").unwrap();
    assert_eq!(
        host.state()
            .store
            .current(key)
            .unwrap()
            .get("vosu.play", &pointer),
        Some(&StateValue::U64(17))
    );
    field.value = PublishValue::U64(18);
    assert_eq!(
        host.handle(
            PluginInstanceId::new(8413),
            Message::PublishBatch(PublishBatch {
                handle: h,
                entries: vec![field.clone()]
            }),
            0
        ),
        vosu_status::VOSU_OK
    );
    assert_eq!(
        host.state()
            .store
            .current(key)
            .unwrap()
            .get("vosu.play", &pointer),
        Some(&StateValue::U64(18))
    );
    field.value = PublishValue::U64(19);
    assert_eq!(
        host.handle(PluginInstanceId::new(8413), Message::PublishField(field), 0),
        vosu_status::VOSU_OK
    );
    assert_eq!(
        host.state()
            .store
            .current(key)
            .unwrap()
            .get("vosu.play", &pointer),
        Some(&StateValue::U64(19))
    );
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn unknown_publish_flags_are_refused_before_commit() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.k4.flags";
    let handle = publisher_instance(&host, id, 8420, 8420);
    let before = host.state().store.current(SessionKey(handle)).unwrap();
    let field = vosu_plugin_api::Message::PublishField(vosu_plugin_api::PublishField {
        handle,
        ns: "vosu.play".into(),
        pointer: "/combo".into(),
        value: vosu_plugin_api::PublishValue::U64(42),
    })
    .encode_payload()
    .unwrap();
    let batch = batch(handle, &[("vosu.play", "/combo", PublishValueRef::U64(42))]);
    for (opcode, payload) in [
        (vosu_opcode::VOSU_OP_PUBLISH_FIELD, field),
        (vosu_opcode::VOSU_OP_PUBLISH_BATCH, batch),
    ] {
        assert_eq!(
            publisher::host_services(id).send_flat(opcode, 1 << 31, 0, &payload),
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        );
        assert!(Arc::ptr_eq(
            &before,
            &host.state().store.current(SessionKey(handle)).unwrap()
        ));
    }
    host.destroy_all(Duration::from_secs(5));
}

/// The iterator rendezvous runs *inside* StateStore validation. Both a raw
/// SYNC read (instances -> sessions) and identify (focus -> instances ->
/// sessions) must finish before publication continues. Holding either global
/// mutex across commit deterministically times out here; no scheduling luck
/// or production-only test hook is required.
#[test]
fn sync_read_and_identify_progress_during_publication() {
    use std::sync::mpsc::sync_channel;
    use vosu_plugin_api::{
        Message, PlanBase, PlanOp, PublishFieldRef, ReadPlanSubmit, SessionIdentify, Stage,
        StageBase, VOSU_READ_PLAN_SYNC,
    };
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = PluginInstanceId::new(8421);
    let plugin_id = "test.k4.lock-order";
    let manifest = session_manifest(plugin_id, &["k4-lock-order"])
        .replace("[match]", "covers=['field:vosu.play.combo']\n[match]");
    let events = super::helpers::create_session_instance_from_manifest(
        &host,
        id,
        plugin_id,
        SessionPluginConfig {
            on_attach: OnAttach::Ignore,
            ..Default::default()
        },
        &manifest,
    );
    let identify = |handle| {
        Message::SessionIdentify(SessionIdentify {
            handle,
            game: "testkit".into(),
            variant: "lock-order".into(),
            name: "Lock order".into(),
            version: "1".into(),
            focus_priority: false,
        })
    };
    host.process_appeared(test_process(930_842_100, "k4-lock-order"));
    let SessionEvent::Attached(first) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("attach")
    };
    assert_eq!(
        host.handle(id, identify(first.handle), 0),
        vosu_status::VOSU_OK
    );
    assert!(matches!(
        events.recv_timeout(RECV_TIMEOUT).unwrap(),
        SessionEvent::Focus(_)
    ));

    let (read_tx, read_rx) = sync_channel::<Vec<u8>>(1);
    let (read_done_tx, read_done_rx) = sync_channel(1);
    let reader_host = host.clone();
    let reader = std::thread::spawn(move || {
        while let Ok(payload) = read_rx.recv_timeout(RECV_TIMEOUT) {
            let status = reader_host.handle_read_plan_sync(id, &payload, VOSU_READ_PLAN_SYNC);
            read_done_tx.send(status).unwrap();
        }
    });
    let (identify_tx, identify_rx) = sync_channel::<u64>(1);
    let (identify_done_tx, identify_done_rx) = sync_channel(1);
    let identify_host = host.clone();
    let identifier = std::thread::spawn(move || {
        while let Ok(handle) = identify_rx.recv_timeout(RECV_TIMEOUT) {
            identify_done_tx
                .send(identify_host.handle(id, identify(handle), 0))
                .unwrap();
        }
    });

    for iteration in 0..64 {
        host.process_appeared(test_process(930_842_101 + iteration, "k4-lock-order"));
        let SessionEvent::Attached(attached) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
            panic!("attach")
        };
        let payload = Message::ReadPlanSubmit(ReadPlanSubmit {
            plan: u64::from(iteration),
            handle: first.handle,
            root: PlanBase::Absolute(0),
            stages: vec![Stage {
                base: StageBase::Root,
                base_offset: 0,
                ops: vec![PlanOp {
                    offset: 0,
                    length: 8,
                }],
            }],
            sync: true,
        })
        .encode_payload()
        .unwrap();
        let entries = std::iter::once(PublishFieldRef {
            handle: first.handle,
            ns: "vosu.play",
            pointer: "/combo",
            value: PublishValueRef::U64(u64::from(iteration)),
        })
        .inspect(|_| {
            read_tx.send(payload.clone()).unwrap();
            identify_tx.send(attached.handle).unwrap();
            assert_eq!(
                read_done_rx
                    .recv_timeout(RECV_TIMEOUT)
                    .expect("SYNC read blocked by publication's global locks"),
                Some(vosu_status::VOSU_ERR_TARGET_GONE)
            );
            assert_eq!(
                identify_done_rx
                    .recv_timeout(RECV_TIMEOUT)
                    .expect("identify blocked by publication's global locks"),
                vosu_status::VOSU_OK
            );
        });
        assert_eq!(
            host.commit_publication(id, first.handle, entries),
            vosu_status::VOSU_OK
        );
        assert!(
            matches!(events.recv_timeout(RECV_TIMEOUT).unwrap(), SessionEvent::Focus(event) if event.handle == attached.handle)
        );
    }
    drop(read_tx);
    drop(identify_tx);
    reader.join().unwrap();
    identifier.join().unwrap();
    host.destroy_all(Duration::from_secs(5));
}
