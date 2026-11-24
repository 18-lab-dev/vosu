//! M2 K8: real host vtables, KT publishers/subscribers and the D3 surface.
#![cfg(feature = "test-harness")]

#[path = "state_bus/support.rs"]
mod support;
use std::time::{Duration, Instant};
use support::*;
use vosu_core::state::{
    Audience, Pointer, SessionKey, StateValue, project_document, project_namespace, project_select,
};
use vosu_plugin_api::{HostServices, Message, PublishValueRef, StateEntry, vosu_status};
use vosu_plugin_testkit::{publisher, subscriber::SubscriberEvent};
use vosu_surface::state::SessionTarget;

fn value(entry: &StateEntry) -> serde_json::Value {
    assert!(entry.error.is_none());
    entry.value.as_ref().map_or(serde_json::Value::Null, |v| {
        serde_json::to_value(v).unwrap()
    })
}
fn pair_check(entries: &[StateEntry]) -> bool {
    let doc = value(&entries[0]);
    for lane in ["regular", "precise"] {
        if let Some(pair) = doc.get(lane) {
            assert_eq!(pair["a"], pair["b"], "torn {lane} batch");
        }
    }
    doc["regular"]["a"] == 10_000 && doc["precise"]["a"] == 10_000
}
fn midpoint(entries: &[StateEntry]) -> bool {
    let doc = value(&entries[0]);
    doc["regular"]["a"] == 5_000 && doc["precise"]["a"] == 5_000
}

#[test]
fn concurrent_regular_precise_batches_are_atomic_on_both_read_paths() {
    let mut h = Harness::new();
    let id = "test.k8.concurrent";
    let (handle, _) = h.publisher(id, 880_001);
    let observer = h.subscriber("test.k8.concurrent.reader", "");
    assert_eq!(observer.learn().handle, handle);
    observer.subscribe(Some(handle), vec![select_ns(id, "")]);
    pair_check(&observer.update());
    let surface = h.host.surface_host();
    let mut subscription = surface
        .subscribe(
            "test.k8.concurrent.reader",
            SessionTarget::Focused,
            vec![select_ns(id, "")],
        )
        .unwrap();
    let mut releases = Vec::new();
    let writers = ["regular", "precise"]
        .into_iter()
        .map(|lane| {
            let (release, resume) = std::sync::mpsc::sync_channel(1);
            releases.push(release);
            let mut n = 0;
            Box::new(move |batch: &mut vosu_plugin_api::PublishBatchWriter| {
                if n == 10_000 {
                    return false;
                }
                // Both readers must observe the stable midpoint before either writer
                // continues. This bounds the handshake and prevents scheduler luck
                // from making the in-flight observation assertion vacuous.
                if n == 5_000 {
                    resume.recv_timeout(WAIT).unwrap();
                }
                n += 1;
                batch.push(id, &format!("/{lane}/a"), PublishValueRef::U64(n));
                batch.push(id, &format!("/{lane}/b"), PublishValueRef::U64(n));
                true
            }) as publisher::Writer
        })
        .collect();
    let threads = publisher::spawn_writers(id, handle, writers);
    let deadline = Instant::now() + WAIT;
    let (mut kt_final, mut surface_final) = (false, false);
    let (mut kt_samples, mut surface_samples) = (0, 0);
    let (mut kt_mid, mut surface_mid) = (false, false);
    while !(kt_final && surface_final && threads.iter().all(std::thread::JoinHandle::is_finished)) {
        assert!(
            Instant::now() < deadline,
            "writers/readers must reach both final batches"
        );
        for event in observer.events.try_iter() {
            if let SubscriberEvent::Update { values, .. } = event {
                kt_samples += 1;
                kt_final |= pair_check(&values);
                kt_mid |= midpoint(&values);
            }
        }
        if subscription.updates.has_changed().unwrap() {
            surface_samples += 1;
            let update = subscription.updates.borrow_and_update();
            surface_final |= pair_check(&update.values);
            surface_mid |= midpoint(&update.values);
        }
        if kt_mid && surface_mid {
            for release in releases.drain(..) {
                release.try_send(()).unwrap();
            }
        }
        std::thread::yield_now();
    }
    assert!(
        kt_samples > 1 && surface_samples > 1,
        "both readers observed publication in flight"
    );
    for thread in threads {
        assert_eq!(thread.join().unwrap(), vosu_status::VOSU_OK);
    }
    assert!(pair_check(&[surface
        .read(
            "test.k8.concurrent.reader",
            SessionTarget::Focused,
            &select_ns(id, "")
        )
        .unwrap()]));
}

#[test]
fn internal_sentinel_never_reaches_public_projections_events_or_tracing() {
    let _lease = osu_lease();
    // A global subscriber also captures the host runtime and ABI worker threads.
    let capture = Capture::default();
    tracing::subscriber::set_global_default(
        tracing_subscriber::fmt()
            .with_max_level(tracing::Level::DEBUG)
            .with_ansi(false)
            .with_writer(capture.clone())
            .finish(),
    )
    .unwrap();
    let mut h = Harness::new();
    let (handle, _) = h.publisher(NS, 880_002);
    let observer = h.subscriber("test.k8.redaction.reader", "");
    let attached = observer.learn();
    assert_eq!(attached.handle, handle);
    assert!(
        attached.name.is_none()
            && attached.path.is_none()
            && attached.command_line.is_none()
            && attached.argv.is_none()
    );
    assert_eq!(
        publish(
            NS,
            handle,
            &[
                (
                    "/internal/paths/stable/install",
                    PublishValueRef::String(SENTINEL)
                ),
                ("/public", PublishValueRef::String("visible"))
            ]
        ),
        vosu_status::VOSU_OK
    );
    // Exercise a refusal carrying sensitive input, plus subscription/demand paths.
    assert_eq!(
        publish(
            NS,
            handle,
            &[
                ("/public", PublishValueRef::String(SENTINEL)),
                ("/public/nested", PublishValueRef::String(SENTINEL))
            ]
        ),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    observer.subscribe(Some(handle), vec![select("")]);
    let update = observer.update();
    clean(&update);
    assert_eq!(value(&update[0])["public"], "visible");
    observer.subscribe(Some(handle), vec![select("/")]);
    let update = observer.update();
    clean(&update);
    assert!(update[0].value.is_none());
    let state = h.host.state().store.current(SessionKey(handle)).unwrap();
    assert!(
        format!("{:?}", state.namespace(NS).unwrap()).contains(SENTINEL),
        "sentinel really reached private storage"
    );
    let registry = h.host.state().registry.read().unwrap();
    clean(project_document(&registry, &state, Audience::Api));
    clean(project_namespace(
        &registry,
        NS,
        state.namespace(NS).unwrap(),
        Audience::Api,
    ));
    // Whole-document, namespace and hidden selections in document-relative form.
    for path in ["", "/vosu.osu", "/vosu.osu/internal"] {
        let pointer = path.strip_prefix("/vosu.osu").unwrap_or(path);
        let projected = project_select(
            &registry,
            &state,
            NS,
            &Pointer::parse(pointer).unwrap(),
            Audience::Api,
        );
        clean(&projected);
        if path.ends_with("/internal") {
            assert!(projected.is_none());
        }
    }
    for _ in 0..2 {
        let bytes = state.api_json(&registry);
        assert!(!String::from_utf8_lossy(bytes).contains(SENTINEL));
    }
    drop(registry);
    h.host.destroy_all(WAIT);
    let log = String::from_utf8(capture.0.lock().unwrap().clone()).unwrap();
    assert!(!log.contains(SENTINEL));
    // Prove capture is active even when a healthy host has no diagnostic to emit.
    tracing::debug!("K8 capture control");
    assert!(String::from_utf8_lossy(&capture.0.lock().unwrap()).contains("K8 capture control"));
}

#[test]
fn internal_requires_and_optional_grants_match_for_abi_and_surface() {
    let _lease = osu_lease();
    let mut h = Harness::new();
    let publisher_id = NS;
    let (handle, _) = h.publisher(publisher_id, 880_003);
    assert_eq!(
        publish(
            publisher_id,
            handle,
            &[
                (
                    "/internal/paths/stable/install",
                    PublishValueRef::String("stable")
                ),
                (
                    "/internal/paths/lazer/install",
                    PublishValueRef::String("lazer")
                )
            ]
        ),
        vosu_status::VOSU_OK
    );
    for (suffix, kind, capability, stable, lazer) in [
        ("whole", "requires", "vosu.osu", true, true),
        (
            "stable",
            "requires",
            "vosu.osu.internal.paths.stable",
            false,
            true,
        ),
        ("all", "optional", "vosu.osu.internal", false, false),
    ] {
        let id = format!("test.k8.gating.{suffix}");
        let observer = h.subscriber(
            &id,
            &format!("\n[[{kind}]]\nid='field:{capability}'\nversion='*'\n"),
        );
        observer.learn();
        for (path, absent) in [
            ("/internal/paths/stable/install", stable),
            ("/internal/paths/lazer/install", lazer),
        ] {
            observer.subscribe(Some(handle), vec![select(path)]);
            let entries = observer.update();
            assert_eq!(
                entries[0].value.is_none(),
                absent,
                "ABI {capability} {path}"
            );
            let surface = h.host.surface_host();
            let entry = surface
                .read(&id, SessionTarget::Focused, &select(path))
                .unwrap();
            assert_eq!(entry, entries[0], "surface uses identical grants");
            if !absent {
                assert_eq!(
                    value(&entry),
                    if path.contains("/stable/") {
                        "stable"
                    } else {
                        "lazer"
                    }
                );
            }
        }
        observer.subscribe(Some(handle), vec![select("")]);
        let entries = observer.update();
        let whole = value(&entries[0]);
        assert_eq!(
            whole.pointer("/internal/paths/stable/install").is_none(),
            stable
        );
        assert_eq!(
            whole.pointer("/internal/paths/lazer/install").is_none(),
            lazer
        );
        assert_eq!(
            h.host
                .surface_host()
                .read(&id, SessionTarget::Focused, &select(""))
                .unwrap(),
            entries[0]
        );
    }
}

#[test]
fn invalid_batch_changes_neither_revision_nor_any_field() {
    let mut h = Harness::new();
    let id = "test.k8.invalid";
    let (handle, _) = h.publisher(id, 880_004);
    assert_eq!(
        publish(id, handle, &[("/valid", PublishValueRef::U64(1))]),
        vosu_status::VOSU_OK
    );
    let before = h.host.state().store.current(SessionKey(handle)).unwrap();
    assert_eq!(
        publish(
            id,
            handle,
            &[
                ("/valid", PublishValueRef::U64(2)),
                ("/valid/child", PublishValueRef::U64(3))
            ]
        ),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
    let after = h.host.state().store.current(SessionKey(handle)).unwrap();
    assert!(std::sync::Arc::ptr_eq(&before, &after));
    assert_eq!(after.revision, before.revision);
    assert_eq!(
        after.get(id, &Pointer::parse("/valid").unwrap()),
        Some(&StateValue::U64(1))
    );
}

#[test]
fn identification_after_five_seconds_opens_the_bus_only_then() {
    let mut h = Harness::new();
    let id = "test.k8.late";
    let events = delayed::register();
    h.load(
        &format!(
            "{}concurrent_dispatch=true\n[match]\nprocess_names=['{id}']\n",
            manifest(id)
        ),
        delayed::vosu_plugin_vtable,
        None,
    );
    let observer = h.subscriber("test.k8.late.reader", "");
    observer.subscribe(
        None,
        vec![vosu_plugin_api::Selector {
            ns: "vosu.sessions".into(),
            pointer: String::new(),
        }],
    );
    h.host.process_appeared(record(880_005, id));
    let (handle, abi) = events.recv_timeout(WAIT).unwrap();
    assert!(h.host.state().store.current(SessionKey(handle)).is_none());
    let start = Instant::now();
    // This is the actual spec window, not a sleep used to order worker threads.
    h.runtime.block_on(async {
        tokio::time::timeout(WAIT, tokio::time::sleep(Duration::from_millis(5_100)))
            .await
            .unwrap();
    });
    assert!(start.elapsed() >= Duration::from_secs(5));
    assert!(h.host.state().store.current(SessionKey(handle)).is_none());
    assert!(
        !observer
            .events
            .try_iter()
            .any(|event| matches!(event, SubscriberEvent::Attached(_)))
    );
    assert_eq!(
        abi.send(
            &Message::SessionIdentify(vosu_plugin_api::SessionIdentify {
                handle,
                game: "testkit".into(),
                variant: "late".into(),
                name: "late".into(),
                version: "1".into(),
                focus_priority: false
            }),
            0
        ),
        vosu_status::VOSU_OK
    );
    let attached = until(&observer.events, |event| {
        if let SubscriberEvent::Attached(a) = event {
            Some(a)
        } else {
            None
        }
    });
    assert_eq!(attached.handle, handle);
    assert!(
        h.host
            .state()
            .store
            .current(SessionKey(handle))
            .unwrap()
            .namespace("vosu.client")
            .is_some()
    );
}

#[test]
fn reused_pid_closes_old_bus_before_opening_a_fresh_generation() {
    let mut h = Harness::new();
    let id = "test.k8.reuse";
    let (old, events) = h.publisher(id, 880_006);
    let observer = h.subscriber("test.k8.reuse.reader", "");
    let first = observer.learn();
    assert_eq!(
        publish(id, old, &[("/old", PublishValueRef::U64(99))]),
        vosu_status::VOSU_OK
    );
    h.host.process_exited(880_006);
    assert!(h.host.state().store.current(SessionKey(old)).is_none());
    until(&observer.events, |event| {
        if let SubscriberEvent::Detached(d) = event {
            assert_eq!(d.handle, old);
            Some(())
        } else {
            None
        }
    });
    h.host.process_appeared(record(880_006, id));
    let new = until(&events, |event| {
        if let publisher::PublisherEvent::Attached { handle } = event {
            Some(handle)
        } else {
            None
        }
    });
    let second = until(&observer.events, |event| {
        if let SubscriberEvent::Attached(a) = event {
            Some(a)
        } else {
            None
        }
    });
    assert_ne!(old, new);
    assert_eq!(new, second.handle);
    assert_ne!(first.session, second.session);
    assert!(second.generation > first.generation);
    let state = h.host.state().store.current(SessionKey(new)).unwrap();
    assert_eq!(state.revision, 0);
    assert!(state.namespace(id).is_none());
    assert_eq!(
        publish(id, old, &[("/old", PublishValueRef::U64(100))]),
        vosu_status::VOSU_ERR_TARGET_GONE
    );
    assert_eq!(
        publish(id, new, &[("/new", PublishValueRef::U64(1))]),
        vosu_status::VOSU_OK
    );
}
