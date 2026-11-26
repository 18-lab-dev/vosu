//! Non-owner own-namespace publication (`docs/v2/plugin-abi.md` §6.6/§6.7,
//! 2026-11-26) through a real owning game plugin and a real non-owning
//! provider that learns the session from `vosu.sessions` and publishes
//! through its own host services.
use super::helpers::{RECV_TIMEOUT, session_manifest, test_host, test_process};
use crate::api::{ApiPlan, ApiSubscription, SessionTarget};
use crate::plugin::{
    PluginInstanceId, dispatch::WorkerPool, host_services::PluginHostHandler, registry,
};
use crate::state::{Pointer, SessionKey, StateValue};
use std::sync::{Arc, mpsc::Receiver};
use std::time::Instant;
use vosu_plugin_api::{
    DemandChanged, FieldSelector, HostServices, Message, PublishBatchWriter, PublishValueRef,
    Selector, StateSubscribe, vosu_opcode, vosu_status,
};
use vosu_plugin_testkit::publisher::{self, PublisherEvent};

type Host = Arc<super::super::PluginHost>;

fn load(
    host: &Host,
    n: u64,
    id: &str,
    manifest: &str,
    schemas: &[&str],
) -> Receiver<PublisherEvent> {
    publisher::register_manifest(id, manifest);
    let events = publisher::register(id);
    let loaded = registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest,
            settings_schema: None,
            namespace_schemas: schemas,
            action_schemas: &[],
            vtable: publisher::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    host.create_instance(
        PluginInstanceId::new(n),
        &loaded[0],
        std::env::temp_dir(),
        Some(WorkerPool::new(2, 64)),
    )
    .unwrap();
    events
}

fn attached(events: &Receiver<PublisherEvent>) -> u64 {
    let deadline = Instant::now() + RECV_TIMEOUT;
    loop {
        if let PublisherEvent::Attached { handle } = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap()
        {
            return handle;
        }
    }
}

/// The owning game plugin; returns its id and the identified handle.
fn owner(host: &Host, n: u64) -> (String, u64) {
    let id = format!("test.guest.owner{n}");
    let manifest = session_manifest(&id, &[&id]).replace(
        "[match]",
        "concurrent_dispatch=true\ncovers=['field:vosu.play.combo']\n[match]",
    );
    let events = load(host, n, &id, &manifest, &[]);
    host.process_appeared(test_process(n as u32, &id));
    (id, attached(&events))
}

/// A tool plugin that owns no session and provides its own namespace,
/// whose `/value` is precise/always.
fn guest(host: &Host, n: u64) -> (String, Receiver<PublisherEvent>) {
    let id = format!("test.guest.tool{n}");
    let manifest = format!(
        "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='{id}'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\nconcurrent_dispatch=true\n[[provides]]\nid='field:{id}'\nversion='1.0.0'\nschema='state.json'\n"
    );
    let schema = format!(
        r#"{{"title":"{id}","version":"1.0.0","type":"object","properties":{{"value":{{"type":"integer","x-sampling-lane":{{"lane":"precise","activity":"always"}}}}}}}}"#
    );
    let events = load(host, n, &id, &manifest, &[&schema]);
    (id, events)
}

/// Subscribes instance `n` to session events and waits for `handle`.
fn attach(host: &Host, n: u64, events: &Receiver<PublisherEvent>, handle: u64) {
    assert_eq!(
        host.handle(
            PluginInstanceId::new(n),
            Message::StateSubscribe(StateSubscribe {
                session: None,
                selectors: vec![Selector {
                    ns: "vosu.sessions".into(),
                    pointer: String::new(),
                }],
            }),
            0,
        ),
        vosu_status::VOSU_OK
    );
    assert_eq!(attached(events), handle);
}

fn batch(handle: u64, ns: &str, pointer: &str, value: u64) -> Vec<u8> {
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    writer.push(ns, pointer, PublishValueRef::U64(value));
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    bytes
}

/// Publishes through plugin `id`'s own host services.
fn publish(id: &str, handle: u64, ns: &str, pointer: &str, value: u64) -> vosu_status {
    publisher::host_services(id).send_flat(
        vosu_opcode::VOSU_OP_PUBLISH_BATCH,
        0,
        0,
        &batch(handle, ns, pointer, value),
    )
}

fn next_demand(events: &Receiver<PublisherEvent>) -> DemandChanged {
    let deadline = Instant::now() + RECV_TIMEOUT;
    loop {
        if let PublisherEvent::Demand(demand) = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap()
        {
            return demand;
        }
    }
}

fn api_value(host: &Host, path: &str) -> Option<StateValue> {
    host.api_state_source()
        .read(&SessionTarget::Active, &ApiPlan::new(&[path]).unwrap())
        .unwrap()
        .values
        .remove(0)
}

fn await_api_value(
    rt: &tokio::runtime::Runtime,
    subscription: &mut ApiSubscription,
    expected: Option<StateValue>,
) {
    let deadline = Instant::now() + RECV_TIMEOUT;
    while subscription.updates.borrow().values[0] != expected {
        rt.block_on(tokio::time::timeout(
            deadline.saturating_duration_since(Instant::now()),
            subscription.changed(),
        ))
        .unwrap()
        .unwrap();
    }
}

#[test]
fn non_owner_publishes_only_its_own_namespace_and_receives_its_demand() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let (owner_id, handle) = owner(&host, 9701);
    let (id, events) = guest(&host, 9702);
    // No SESSION_ATTACHED yet: the instance holds no handle for the session.
    assert_eq!(
        publish(&id, handle, &id, "/value", 1),
        vosu_status::VOSU_ERR_PERMISSION_DENIED
    );
    assert!(
        host.state()
            .store
            .current(SessionKey(handle))
            .unwrap()
            .namespace(&id)
            .is_none()
    );
    // An API v1 subscriber already selects the field before the attach.
    let path = format!("/{id}/value");
    let mut subscription = host
        .api_state_source()
        .subscribe(SessionTarget::Active, ApiPlan::new(&[&path]).unwrap())
        .unwrap();
    attach(&host, 9702, &events, handle);
    let demand = next_demand(&events);
    assert_eq!(demand.handle, handle);
    assert_eq!(
        demand.selectors,
        vec![FieldSelector {
            ns: id.clone(),
            pointer: "/value".into(),
        }]
    );
    assert!(demand.precise && !demand.regular);

    assert_eq!(publish(&id, handle, &id, "/value", 7), vosu_status::VOSU_OK);
    await_api_value(&rt, &mut subscription, Some(StateValue::U64(7)));
    assert_eq!(api_value(&host, &path), Some(StateValue::U64(7)));

    let before = host.state().store.current(SessionKey(handle)).unwrap();
    for (ns, pointer) in [
        ("vosu.play", "/combo"),
        ("vosu.client", "/pid"),
        ("vosu.sessions", "/x"),
        (owner_id.as_str(), "/x"),
    ] {
        assert_eq!(
            publish(&id, handle, ns, pointer, 9),
            vosu_status::VOSU_ERR_PERMISSION_DENIED,
            "{ns}{pointer}"
        );
    }
    assert!(Arc::ptr_eq(
        &before,
        &host.state().store.current(SessionKey(handle)).unwrap()
    ));
    // The owner's own rules are unchanged.
    assert_eq!(
        publish(&owner_id, handle, "vosu.play", "/combo", 4),
        vosu_status::VOSU_OK
    );
    assert_eq!(
        publish(&owner_id, handle, &id, "/value", 4),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );

    drop(subscription);
    assert!(next_demand(&events).selectors.is_empty());
    host.destroy_all(RECV_TIMEOUT);
}

#[test]
fn non_owner_publication_answers_target_gone_after_detach() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let (_, handle) = owner(&host, 9711);
    let (id, events) = guest(&host, 9712);
    attach(&host, 9712, &events, handle);
    assert_eq!(publish(&id, handle, &id, "/value", 2), vosu_status::VOSU_OK);
    host.process_exited(9711);
    let deadline = Instant::now() + RECV_TIMEOUT;
    while events
        .recv_timeout(deadline.saturating_duration_since(Instant::now()))
        .unwrap()
        != (PublisherEvent::Detached { handle })
    {}
    assert_eq!(
        publish(&id, handle, &id, "/value", 3),
        vosu_status::VOSU_ERR_TARGET_GONE
    );
    host.destroy_all(RECV_TIMEOUT);
}

#[test]
fn unloading_the_non_owner_removes_its_namespace_from_the_session() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let (owner_id, handle) = owner(&host, 9721);
    let (id, events) = guest(&host, 9722);
    attach(&host, 9722, &events, handle);
    let path = format!("/{id}/value");
    let mut subscription = host
        .api_state_source()
        .subscribe(SessionTarget::Active, ApiPlan::new(&[&path]).unwrap())
        .unwrap();
    assert_eq!(publish(&id, handle, &id, "/value", 5), vosu_status::VOSU_OK);
    await_api_value(&rt, &mut subscription, Some(StateValue::U64(5)));
    assert_eq!(
        publish(&owner_id, handle, "vosu.play", "/combo", 6),
        vosu_status::VOSU_OK
    );

    host.teardown_instance(PluginInstanceId::new(9722)).unwrap();
    let state = host.state().store.current(SessionKey(handle)).unwrap();
    assert!(state.namespace(&id).is_none());
    assert_eq!(
        state.get("vosu.play", &Pointer::parse("/combo").unwrap()),
        Some(&StateValue::U64(6))
    );
    await_api_value(&rt, &mut subscription, None);
    assert_eq!(api_value(&host, &path), None);
    // The retired instance can no longer publish at all.
    assert_eq!(
        host.handle_publish(
            PluginInstanceId::new(9722),
            vosu_opcode::VOSU_OP_PUBLISH_BATCH,
            &batch(handle, &id, "/value", 8),
        ),
        Some(vosu_status::VOSU_ERR_INVALID_ARGUMENT)
    );
    host.destroy_all(RECV_TIMEOUT);
}
