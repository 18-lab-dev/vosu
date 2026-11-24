use super::*;
use crate::api::{ApiError, ApiPlan, ApiSubscription, SessionTarget};
use crate::state::{InternalGrant, PublisherGrant};
use vosu_plugin_api::Selector;
use vosu_surface::state::SessionTarget as SurfaceTarget;

fn plan(pointers: &[&str]) -> ApiPlan {
    ApiPlan::new(pointers).unwrap()
}
fn changed(rt: &tokio::runtime::Runtime, subscription: &mut ApiSubscription) {
    rt.block_on(async {
        tokio::time::timeout(RECV_TIMEOUT, subscription.changed())
            .await
            .unwrap()
            .unwrap();
    });
}
fn object(value: &StateValue) -> &crate::state::StateObject {
    let StateValue::Object(object) = value else {
        panic!("expected object")
    };
    object
}
fn summary_id(value: &StateValue) -> String {
    let Some(StateValue::String(id)) = value.get(&["session"]) else {
        panic!("missing session")
    };
    id.to_string()
}

#[test]
fn api_reads_hide_pending_sessions_and_never_create_demand() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let pending = create_session_instance(
        &host,
        PluginInstanceId::new(9401),
        "test.s0.hidden",
        &["s0-hidden"],
        SessionPluginConfig {
            on_attach: OnAttach::Ignore,
            ..Default::default()
        },
    );
    host.process_appeared(test_process(9401, "s0-hidden"));
    let SessionEvent::Attached(attached) = pending.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("expected attachment")
    };
    let source = host.api_state_source();
    assert!(source.sessions().unwrap().is_empty());
    assert!(matches!(
        source.session(&SessionTarget::Id(attached.session)),
        Err(ApiError::UnknownSession)
    ));
    assert!(matches!(
        source.read(&SessionTarget::Active, &plan(&[""])),
        Err(ApiError::UnknownSession)
    ));
    let handle = publisher_instance(&host, "test.s0.read", 9402, 9402);
    let summaries = source.sessions().unwrap();
    assert_eq!(summaries.len(), 1);
    assert_eq!(object(&summaries[0]).len(), 8);
    let summary = source.session(&SessionTarget::Active).unwrap();
    assert_eq!(summary, summaries[0]);
    assert_eq!(
        source
            .read(&SessionTarget::Active, &plan(&["/vosu.client/pid"]))
            .unwrap()
            .values,
        vec![Some(StateValue::U64(9402))]
    );
    assert!(
        host.state
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(handle), Some("play"))
            .selectors
            .is_empty()
    );
    host.destroy_all(RECV_TIMEOUT);
    assert!(matches!(source.sessions(), Err(ApiError::Closed)));
}

#[test]
fn api_watch_is_a_full_snapshot_and_raii_preserves_surface_demand() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.s0.snapshot";
    let handle = publisher_instance_covering(&host, id, 9403, 9403, "field:vosu.play");
    let source = host.api_state_source();
    let mut subscription = source
        .subscribe(
            SessionTarget::Active,
            plan(&[
                "/vosu.play/combo",
                "/vosu.play/accuracy",
                "/vosu.play/score",
            ]),
        )
        .unwrap();
    let surface = host
        .surface_host()
        .subscribe(
            id,
            SurfaceTarget::Focused,
            vec![Selector {
                ns: "vosu.client".into(),
                pointer: "/pid".into(),
            }],
        )
        .unwrap();
    for (pointer, value) in [("/combo", 13), ("/accuracy", 27), ("/score", 42)] {
        assert_eq!(
            send(
                id,
                &batch(
                    handle,
                    &[("vosu.play", pointer, PublishValueRef::U64(value))]
                )
            ),
            vosu_status::VOSU_OK
        );
    }
    // No consumer poll between publications: all selected keys remain intact.
    changed(&rt, &mut subscription);
    assert_eq!(
        subscription.updates.borrow().values[0],
        Some(StateValue::U64(13))
    );
    assert_eq!(
        subscription.updates.borrow().values[1],
        Some(StateValue::U64(27))
    );
    assert_eq!(
        subscription.updates.borrow().values[2],
        Some(StateValue::U64(42))
    );
    let demand = host
        .state
        .subscriptions
        .lock()
        .unwrap()
        .demand(SessionKey(handle), Some("play"));
    assert_eq!(demand.selectors.len(), 4);
    assert!(demand.regular);
    drop(subscription);
    let demand = host
        .state
        .subscriptions
        .lock()
        .unwrap()
        .demand(SessionKey(handle), Some("play"));
    assert_eq!(demand.selectors.len(), 1);
    assert_eq!(demand.selectors[0].0.as_ref(), "vosu.client");
    assert!(!demand.regular);
    drop(surface);
    assert!(
        host.state
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(handle), Some("play"))
            .selectors
            .is_empty()
    );
    host.destroy_all(RECV_TIMEOUT);
}

#[test]
fn api_active_rebinds_equal_values_and_sessions_flip_coherently() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let foreground = Arc::new(SyntheticForegroundSource::new(Some(9404)));
    let (_logs, host) = test_host_with(rt.handle().clone(), foreground.clone(), RECV_TIMEOUT);
    let first = publisher_instance(&host, "test.s0.focus-a", 9404, 9404);
    let source = host.api_state_source();
    let mut active = source
        .subscribe(SessionTarget::Active, plan(&["/vosu.play/combo"]))
        .unwrap();
    let old = active.updates.borrow().session.clone().unwrap();
    let mut fixed = source
        .subscribe(SessionTarget::Id(old.clone()), plan(&["/vosu.play/combo"]))
        .unwrap();
    let mut sessions = source.subscribe_sessions(plan(&[""])).unwrap();
    foreground.set(Some(9405));
    let second = publisher_instance(&host, "test.s0.focus-b", 9405, 9405);
    changed(&rt, &mut active);
    changed(&rt, &mut sessions);
    assert_ne!(
        active.updates.borrow().session.as_deref(),
        Some(old.as_str())
    );
    assert_eq!(
        active.updates.borrow().values,
        fixed.updates.borrow().values
    );
    assert!(active.updates.borrow().revision > fixed.updates.borrow().revision);
    let snapshot = sessions.updates.borrow().values[0].clone().unwrap();
    assert_eq!(object(&snapshot).len(), 2);
    assert_eq!(
        object(&snapshot)
            .iter()
            .filter(|(_, s)| s.get(&["focused"]) == Some(&StateValue::Bool(true)))
            .count(),
        1
    );
    assert_eq!(
        snapshot.get(&[old.as_str(), "focused"]),
        Some(&StateValue::Bool(false))
    );
    assert!(
        host.state
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(first), Some("play"))
            .regular
    ); // fixed still owns demand
    // A slow consumer may skip both focus transitions. The final session id
    // equals its previous one, but the binding epoch must still force a reset.
    let binding = active.updates.borrow().binding;
    foreground.set(Some(9404));
    host.foreground_changed();
    foreground.set(Some(9405));
    host.foreground_changed();
    changed(&rt, &mut active);
    assert!(active.updates.borrow().binding > binding);
    drop(active);
    assert!(
        !host
            .state
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(second), Some("play"))
            .regular
    );
    host.process_exited(9404);
    changed(&rt, &mut fixed);
    let terminal = fixed.updates.borrow().clone();
    assert!(terminal.ended);
    assert_eq!(terminal.values, vec![None]);
    foreground.set(Some(9405));
    host.foreground_changed();
    assert!(Arc::ptr_eq(&terminal, &fixed.updates.borrow()));
    assert!(
        host.state
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(first), Some("play"))
            .selectors
            .is_empty()
    );
    host.destroy_all(RECV_TIMEOUT);
    assert!(sessions.updates.borrow().ended);
}

#[test]
fn api_root_expands_new_namespaces_without_spending_external_selectors() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.s0.root";
    let handle = publisher_instance(&host, id, 9406, 9406);
    let subscription = host
        .api_state_source()
        .subscribe(SessionTarget::Active, plan(&[""]))
        .unwrap();
    assert!(
        host.state
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(handle), Some("play"))
            .precise
    );
    for n in 0..260 {
        let ns = if n == 259 {
            id.to_owned()
        } else {
            format!("test.s0.ns{n}")
        };
        let schema = format!(
            r#"{{"title":"{ns}","version":"1.0.0","type":"object","properties":{{"value":{{"type":"integer","x-sampling-lane":{{"lane":"precise","activity":"always"}}}}}}}}"#
        );
        host.state
            .registry
            .write()
            .unwrap()
            .register(id, &ns, &schema)
            .unwrap();
    }
    assert_eq!(
        send(
            id,
            &batch(handle, &[("vosu.play", "/combo", PublishValueRef::U64(54))])
        ),
        vosu_status::VOSU_OK
    );
    let demand = host
        .state
        .subscriptions
        .lock()
        .unwrap()
        .demand(SessionKey(handle), Some("play"));
    assert_eq!(demand.selectors.len(), 265);
    assert!(
        demand
            .selectors
            .iter()
            .any(|(ns, p)| ns.as_ref() == id && p.is_empty())
    );
    assert!(!subscription.updates.borrow().ended);
    let manifest = vosu_plugin_api::manifest::PackageManifest::parse(&format!(
        "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='root'\nversion='1.0.0'\nhost='>=1.0.0'\nabi_version=1\nruntime='in-process'\n[[provides]]\nid='field:{id}'\nversion='1.0.0'\nschema='new.json'\n"
    )).and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin).unwrap();
    let grant = PublisherGrant::from_manifest(&manifest).unwrap();
    let bytes = batch(handle, &[(id, "/value", PublishValueRef::U64(92))]);
    let published = vosu_plugin_api::decode_publish_batch_ref(&bytes).unwrap();
    let outcome = host
        .state
        .store
        .commit(&grant, SessionKey(handle), published.entries())
        .unwrap();
    host.api_committed(&outcome);
    assert_eq!(
        subscription.updates.borrow().values[0]
            .as_ref()
            .unwrap()
            .get(&[id, "value"]),
        Some(&StateValue::U64(92))
    );
    drop(subscription);
    assert!(
        !host
            .state
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(handle), Some("play"))
            .precise
    );
    host.destroy_all(RECV_TIMEOUT);
}

#[test]
fn api_projection_never_uses_plugin_internal_grants() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.s0.privacy";
    let handle = publisher_instance(&host, id, 9407, 9407);
    host.state
        .registry
        .write()
        .unwrap()
        .register_versioned(
            "vosu.osu",
            "vosu.osu",
            &semver::Version::parse("1.2.0").unwrap(),
            vosu_plugin_api::manifest::schema::SchemaProvenance::TrustedOfficial,
            include_str!(concat!(
                env!("CARGO_MANIFEST_DIR"),
                "/../../docs/v2/schemas/vosu.osu.schema.json"
            )),
        )
        .unwrap();
    let manifest = vosu_plugin_api::manifest::PackageManifest::parse("kind='plugin'\nmanifest_version=1\nid='vosu.osu'\nname='osu'\nversion='1.0.0'\nhost='>=1.0.0'\nabi_version=1\nruntime='in-process'\n[[provides]]\nid='field:vosu.osu'\nversion='1.2.0'\nschema='osu.json'\n").and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin).unwrap();
    let grant = PublisherGrant::from_manifest(&manifest).unwrap();
    let public_id = summary_id(&host.api_state_source().sessions().unwrap().remove(0));
    host.state.store.close_session(SessionKey(handle));
    host.state.store.open_session(
        SessionKey(handle),
        &grant,
        crate::state::ClientFacts {
            game: "osu".into(),
            variant: "stable".into(),
            name: "privacy".into(),
            version: "42".into(),
            pid: 9407,
            session: public_id,
            plugin_id: "vosu.osu".into(),
            plugin_version: "1.0.0".into(),
        },
        true,
    );
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    writer.push(
        "vosu.osu",
        "/internal/paths/install",
        PublishValueRef::String("/private/s0-sentinel"),
    );
    writer.push("vosu.osu", "/visible", PublishValueRef::U64(71));
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    let batch = vosu_plugin_api::decode_publish_batch_ref(&bytes).unwrap();
    let outcome = host
        .state
        .store
        .commit(&grant, SessionKey(handle), batch.entries())
        .unwrap();
    let registry = host.state.registry.read().unwrap();
    let internal = Pointer::parse("/internal/paths/install").unwrap();
    assert!(
        crate::state::project_select(
            &registry,
            &outcome.current,
            "vosu.osu",
            &internal,
            crate::state::Audience::Plugin(&InternalGrant::from_subtrees(vec![
                Pointer::parse("/internal/paths").unwrap()
            ]))
        )
        .is_some()
    );
    drop(registry);
    let source = host.api_state_source();
    let read = source
        .read(
            &SessionTarget::Active,
            &plan(&["/vosu.osu/visible", "/vosu.osu/internal/paths/install"]),
        )
        .unwrap();
    assert_eq!(read.values, vec![Some(StateValue::U64(71)), None]);
    let subscription = source
        .subscribe(SessionTarget::Active, plan(&[""]))
        .unwrap();
    assert!(
        subscription.updates.borrow().values[0]
            .as_ref()
            .unwrap()
            .get(&["vosu.osu", "internal"])
            .is_none()
    );
    host.destroy_all(RECV_TIMEOUT);
}

#[test]
fn api_plans_enforce_wire_bounds_and_unknown_namespace_is_absent() {
    for invalid in [vec![], vec!["/a", "/a/b"], vec!["/~2"], vec!["/a"; 257]] {
        assert!(ApiPlan::new(&invalid).is_err());
    }
    assert!(ApiPlan::new(&["/a~1b/~0", "/missing"]).is_ok());
}

#[test]
fn api_capacity_is_released_and_host_is_not_kept_alive() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let source = host.api_state_source();
    let mut subscriptions = (0..crate::state::MAX_SUBSCRIPTIONS)
        .map(|_| {
            source
                .subscribe_sessions(plan(&["/not-yet-attached"]))
                .unwrap()
        })
        .collect::<Vec<_>>();
    assert!(matches!(
        source.subscribe_sessions(plan(&[""])),
        Err(ApiError::Capacity)
    ));
    subscriptions.pop();
    let replacement = source.subscribe_sessions(plan(&[""])).unwrap();
    let weak = Arc::downgrade(&host);
    host.destroy_all(RECV_TIMEOUT);
    drop(host);
    assert!(weak.upgrade().is_none());
    assert!(replacement.updates.borrow().ended);
    assert!(matches!(source.sessions(), Err(ApiError::Closed)));
}

#[test]
fn api_established_active_survives_no_focus_and_reappears() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    publisher_instance(&host, "test.s0.loss", 9408, 9408);
    let source = host.api_state_source();
    let mut active = source
        .subscribe(
            SessionTarget::Active,
            plan(&["/vosu.client/pid", "/future~1namespace/value"]),
        )
        .unwrap();
    let revision = active.updates.borrow().revision;
    host.process_exited(9408);
    changed(&rt, &mut active);
    assert!(!active.updates.borrow().ended);
    assert_eq!(active.updates.borrow().session, None);
    assert_eq!(active.updates.borrow().values, vec![None, None]);
    publisher_instance(&host, "test.s0.return", 9409, 9409);
    changed(&rt, &mut active);
    assert!(active.updates.borrow().revision > revision);
    assert_eq!(
        active.updates.borrow().values,
        vec![Some(StateValue::U64(9409)), None]
    );
    host.destroy_all(RECV_TIMEOUT);
}

#[test]
fn api_active_lost_between_upgrade_and_establishment_starts_absent() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let first = publisher_instance(&host, "test.s0.upgrade-loss", 9410, 9410);
    let source = host.api_state_source();
    let public_id = summary_id(&source.session(&SessionTarget::Active).unwrap());
    // Transport upgrade succeeded, but focus disappeared before the selection.
    host.process_exited(9410);
    let mut subscription = source
        .subscribe(SessionTarget::Active, plan(&["/vosu.client/pid"]))
        .unwrap();
    let initial = subscription.current();
    assert_eq!(initial.session, None);
    assert_eq!(initial.values, vec![None]);
    assert!(!initial.ended);
    assert!(matches!(
        source.subscribe(SessionTarget::Id(public_id), plan(&[""])),
        Err(ApiError::UnknownSession)
    ));
    assert!(matches!(
        source.session(&SessionTarget::Active),
        Err(ApiError::UnknownSession)
    ));
    assert!(
        host.state
            .subscriptions
            .lock()
            .unwrap()
            .demand(SessionKey(first), Some("play"))
            .selectors
            .is_empty()
    );
    publisher_instance(&host, "test.s0.upgrade-return", 9411, 9411);
    changed(&rt, &mut subscription);
    let current = subscription.current();
    assert_eq!(current.values, vec![Some(StateValue::U64(9411))]);
    assert!(current.binding > initial.binding);
    host.destroy_all(RECV_TIMEOUT);
}

#[test]
fn api_session_document_binding_is_constant_after_prior_notifications() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let id = "test.s0.session-binding";
    let handle = publisher_instance(&host, id, 9412, 9412);
    let source = host.api_state_source();
    // Make the publication and notification revision domains deliberately differ.
    for value in [15, 28] {
        assert_eq!(
            send(
                id,
                &batch(
                    handle,
                    &[("vosu.play", "/combo", PublishValueRef::U64(value))]
                )
            ),
            vosu_status::VOSU_OK
        );
    }
    let field = source
        .subscribe(SessionTarget::Active, plan(&["/vosu.play/combo"]))
        .unwrap();
    assert_eq!(
        send(
            id,
            &batch(handle, &[("vosu.play", "/combo", PublishValueRef::U64(39))])
        ),
        vosu_status::VOSU_OK
    );
    let notification = field.current();
    assert!(notification.revision > 0);
    let read = source
        .read(&SessionTarget::Active, &plan(&["/vosu.play/combo"]))
        .unwrap();
    assert_eq!(read.revision, notification.revision);
    assert_ne!(
        read.revision,
        host.state
            .store
            .current(SessionKey(handle))
            .unwrap()
            .revision
    );
    let public_id = notification.session.as_ref().unwrap();
    let mut sessions = source
        .subscribe_sessions(plan(&[&format!("/{public_id}")]))
        .unwrap();
    let initial = sessions.current();
    assert!(initial.revision > 0);
    assert_eq!(initial.binding, 0);
    assert!(initial.values[0].is_some());
    host.process_exited(9412);
    changed(&rt, &mut sessions);
    let removed = sessions.current();
    assert!(removed.revision > initial.revision);
    assert_eq!(removed.binding, initial.binding);
    assert_eq!(removed.values, vec![None]);
    assert!(!removed.ended);
    host.destroy_all(RECV_TIMEOUT);
    assert!(sessions.current().ended);
    assert_eq!(sessions.current().binding, initial.binding);
}
