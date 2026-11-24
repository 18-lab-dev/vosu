use super::helpers::{RECV_TIMEOUT, session_manifest, test_host, test_process};
use crate::plugin::{
    dispatch::WorkerPool,
    host_services::{PluginHostHandler, PluginInstanceId},
    registry,
};
use std::sync::{
    Arc,
    mpsc::{Receiver, sync_channel},
};
use std::time::{Duration, Instant};
use vosu_plugin_api::{
    HostServices, Message, PublishBatchWriter, PublishValueRef, Selector, StateEntry,
    StateSubscribe, vosu_opcode, vosu_status,
};
use vosu_plugin_testkit::{
    publisher::{self, PublisherEvent},
    subscriber::{self, SubscriberEvent},
};

type Host = Arc<super::super::PluginHost>;

fn publisher(host: &Host, n: u64) -> (String, u64, Receiver<PublisherEvent>) {
    let id = format!("test.k6.publisher{n}");
    let manifest = session_manifest(&id, &[&id]).replace("[match]", "concurrent_dispatch=true\ncovers=['field:vosu.play.combo','field:vosu.play.accuracy','field:vosu.status.status']\n[match]");
    publisher::register_manifest(&id, &manifest);
    let events = publisher::register(&id);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest,
        settings_schema: None,
        namespace_schemas: &[],
        vtable: publisher::vosu_plugin_vtable,
    }])
    .unwrap();
    host.create_instance(
        PluginInstanceId::new(n),
        &loaded[0],
        std::env::temp_dir(),
        Some(WorkerPool::new(2, 64)),
    )
    .unwrap();
    host.process_appeared(test_process(n as u32, &id));
    let PublisherEvent::Attached { handle } = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("expected publisher attachment")
    };
    (id, handle, events)
}

fn subscriber(host: &Host, n: u64, pool: Option<Arc<WorkerPool>>) -> Receiver<SubscriberEvent> {
    let id = format!("test.k6.subscriber{n}");
    let (events, _commands) = subscriber::register(&id);
    let manifest = session_manifest(&id, &[&id]).replace(
        "[match]",
        if pool.is_some() {
            "concurrent_dispatch=true\n[match]"
        } else {
            "[match]"
        },
    );
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &manifest,
        settings_schema: None,
        namespace_schemas: &[],
        vtable: subscriber::vosu_plugin_vtable,
    }])
    .unwrap();
    host.create_instance(
        PluginInstanceId::new(n),
        &loaded[0],
        std::env::temp_dir(),
        pool,
    )
    .unwrap();
    events
}
fn select(ns: &str, pointer: &str) -> Selector {
    Selector {
        ns: ns.into(),
        pointer: pointer.into(),
    }
}
fn subscribe(host: &Host, n: u64, session: Option<u64>, selectors: Vec<Selector>) -> vosu_status {
    host.handle(
        PluginInstanceId::new(n),
        Message::StateSubscribe(StateSubscribe { session, selectors }),
        0,
    )
}
fn next_update(events: &Receiver<SubscriberEvent>) -> Vec<StateEntry> {
    let deadline = Instant::now() + RECV_TIMEOUT;
    loop {
        let event = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap();
        if let SubscriberEvent::Update { values, .. } = event {
            return values;
        }
    }
}
fn observe(host: &Host, n: u64, events: &Receiver<SubscriberEvent>, handle: u64) {
    assert_eq!(
        subscribe(host, n, None, vec![select("vosu.sessions", "")]),
        vosu_status::VOSU_OK
    );
    let SubscriberEvent::Attached(attached) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("expected session attachment")
    };
    assert_eq!(attached.handle, handle);
    assert!(attached.name.is_none());
    assert!(attached.path.is_none());
    assert!(attached.command_line.is_none());
    assert!(attached.argv.is_none());
    assert!(
        matches!(events.recv_timeout(RECV_TIMEOUT).unwrap(), SubscriberEvent::Focus(f) if f.handle == handle)
    );
}
fn publish(id: &str, handle: u64, fields: &[(&str, &str, PublishValueRef<'_>)]) {
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    for &(ns, pointer, value) in fields {
        writer.push(ns, pointer, value);
    }
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    assert_eq!(
        publisher::host_services(id).send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &bytes),
        vosu_status::VOSU_OK
    );
}
fn demand(events: &Receiver<PublisherEvent>) -> vosu_plugin_api::DemandChanged {
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

#[test]
fn subscribe_gets_immediate_baseline_then_changes_only() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_, host) = test_host(rt.handle().clone());
    let (id, handle, _) = publisher(&host, 8601);
    publish(&id, handle, &[("vosu.play", "/combo", PublishValueRef::U64(0))]);
    let events = subscriber(&host, 8602, None);
    observe(&host, 8602, &events, handle);
    assert_eq!(
        subscribe(
            &host,
            8602,
            Some(handle),
            vec![select("vosu.play", "/combo")]
        ),
        vosu_status::VOSU_OK
    );
    assert_eq!(
        next_update(&events)[0].value,
        Some(0u64.into())
    );
    publish(
        &id,
        handle,
        &[("vosu.play", "/combo", PublishValueRef::U64(17))],
    );
    let values = next_update(&events);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].value, Some(17u64.into()));
    publish(
        &id,
        handle,
        &[("vosu.play", "/accuracy", PublishValueRef::F64(0.9))],
    );
    // A queued worker fence proves the preceding commit has completed delivery
    // processing without relying on a sleep to infer the absence of an update.
    let (tx, rx) = sync_channel(1);
    host.instances
        .lock()
        .unwrap()
        .get(&PluginInstanceId::new(8602))
        .and_then(|slot| match slot {
            super::super::InstanceSlot::Live(e) => Some(
                e.instance
                    .dispatch_task(Box::new(move |_, _| {
                        tx.send(()).unwrap();
                    }))
                    .unwrap(),
            ),
            _ => None,
        })
        .unwrap();
    rx.recv_timeout(RECV_TIMEOUT).unwrap();
    assert!(events.try_recv().is_err());
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn sessions_subscriber_learns_identified_sessions_only_and_without_process_facts() {
    use vosu_plugin_testkit::session::{OnAttach, SessionEvent, SessionPluginConfig};
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_, host) = test_host(rt.handle().clone());
    let pending = super::helpers::create_session_instance(
        &host,
        PluginInstanceId::new(8611),
        "test.k6.pending",
        &["k6-pending"],
        SessionPluginConfig {
            on_attach: OnAttach::Ignore,
            ..Default::default()
        },
    );
    host.process_appeared(test_process(8611, "k6-pending"));
    assert!(matches!(
        pending.recv_timeout(RECV_TIMEOUT).unwrap(),
        SessionEvent::Attached(_)
    ));
    let events = subscriber(&host, 8612, None);
    assert_eq!(
        subscribe(&host, 8612, None, vec![select("vosu.sessions", "")]),
        vosu_status::VOSU_OK
    );
    assert!(events.try_recv().is_err());
    let (_, handle, _) = publisher(&host, 8613);
    let SubscriberEvent::Attached(attached) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("expected attachment")
    };
    assert_eq!(attached.handle, handle);
    assert!(
        attached.name.is_none()
            && attached.path.is_none()
            && attached.command_line.is_none()
            && attached.argv.is_none()
    );
    assert!(
        matches!(events.recv_timeout(RECV_TIMEOUT).unwrap(), SubscriberEvent::Focus(f) if f.handle == handle)
    );
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn unknown_handle_and_bad_null_session_selector_refused() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_, host) = test_host(rt.handle().clone());
    let (_, handle, _) = publisher(&host, 8621);
    let _events = subscriber(&host, 8622, None);
    for (session, selectors) in [
        (Some(handle), vec![select("vosu.play", "/combo")]),
        (Some(u64::MAX), vec![]),
        (None, vec![]),
        (None, vec![select("vosu.sessions", "/x")]),
        (None, vec![select("vosu.play", "")]),
    ] {
        assert_eq!(
            subscribe(&host, 8622, session, selectors),
            vosu_status::VOSU_ERR_INVALID_ARGUMENT
        );
    }
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn demand_changed_on_subscribe_unsubscribe_and_status_change() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_, host) = test_host(rt.handle().clone());
    let (id, handle, publisher_events) = publisher(&host, 8631);
    let events = subscriber(&host, 8632, None);
    observe(&host, 8632, &events, handle);
    assert_eq!(
        subscribe(
            &host,
            8632,
            Some(handle),
            vec![select("vosu.play", "/accuracy")]
        ),
        vosu_status::VOSU_OK
    );
    let before = demand(&publisher_events);
    assert_eq!(before.selectors.len(), 1);
    assert!(!before.regular && !before.precise);
    publish(
        &id,
        handle,
        &[("vosu.status", "/status", PublishValueRef::String("play"))],
    );
    let playing = demand(&publisher_events);
    assert!(playing.regular || playing.precise);
    publish(
        &id,
        handle,
        &[("vosu.status", "/status", PublishValueRef::String("pause"))],
    );
    publish(
        &id,
        handle,
        &[("vosu.status", "/status", PublishValueRef::String("results"))],
    );
    let results = demand(&publisher_events);
    assert!(!results.regular && !results.precise);
    assert_eq!(
        subscribe(&host, 8632, Some(handle), vec![]),
        vosu_status::VOSU_OK
    );
    assert!(demand(&publisher_events).selectors.is_empty());
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn full_queue_never_loses_the_latest_value() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_, host) = test_host(rt.handle().clone());
    let (id, handle, _) = publisher(&host, 8641);
    let events = subscriber(&host, 8642, Some(WorkerPool::new(1, 1)));
    observe(&host, 8642, &events, handle);
    assert_eq!(
        subscribe(
            &host,
            8642,
            Some(handle),
            vec![select("vosu.play", "/combo")]
        ),
        vosu_status::VOSU_OK
    );
    next_update(&events);
    let (entered_tx, entered_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel(1);
    {
        let instances = host.instances.lock().unwrap();
        let super::super::InstanceSlot::Live(entry) =
            instances.get(&PluginInstanceId::new(8642)).unwrap()
        else {
            panic!("live subscriber")
        };
        entry
            .instance
            .dispatch_task(Box::new(move |_, _| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(RECV_TIMEOUT).unwrap();
            }))
            .unwrap();
    }
    entered_rx.recv_timeout(RECV_TIMEOUT).unwrap();
    {
        let instances = host.instances.lock().unwrap();
        let super::super::InstanceSlot::Live(entry) =
            instances.get(&PluginInstanceId::new(8642)).unwrap()
        else {
            panic!("live subscriber")
        };
        entry.instance.dispatch_task(Box::new(|_, _| {})).unwrap();
    }
    for value in 1..=100 {
        publish(
            &id,
            handle,
            &[("vosu.play", "/combo", PublishValueRef::U64(value))],
        );
    }
    release_tx.send(()).unwrap();
    assert_eq!(next_update(&events)[0].value, Some(100u64.into()));
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn detached_session_flushes_then_detaches() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_, host) = test_host(rt.handle().clone());
    let (id, handle, _) = publisher(&host, 8651);
    let events = subscriber(&host, 8652, None);
    observe(&host, 8652, &events, handle);
    assert_eq!(
        subscribe(
            &host,
            8652,
            Some(handle),
            vec![select("vosu.play", "/combo")]
        ),
        vosu_status::VOSU_OK
    );
    next_update(&events);
    publish(
        &id,
        handle,
        &[("vosu.play", "/combo", PublishValueRef::U64(42))],
    );
    host.process_exited(8651);
    assert_eq!(next_update(&events)[0].value, Some(42u64.into()));
    assert!(
        matches!(events.recv_timeout(RECV_TIMEOUT).unwrap(), SubscriberEvent::Detached(d) if d.handle == handle)
    );
    assert!(
        host.state()
            .store
            .current(crate::state::SessionKey(handle))
            .is_none()
    );
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn destroyed_subscriber_drops_demand() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_, host) = test_host(rt.handle().clone());
    let (_, handle, publisher_events) = publisher(&host, 8661);
    let events = subscriber(&host, 8662, None);
    observe(&host, 8662, &events, handle);
    assert_eq!(
        subscribe(
            &host,
            8662,
            Some(handle),
            vec![select("vosu.play", "/combo")]
        ),
        vosu_status::VOSU_OK
    );
    assert_eq!(demand(&publisher_events).selectors.len(), 1);
    let super::super::InstanceSlot::Live(entry) = host
        .instances
        .lock()
        .unwrap()
        .remove(&PluginInstanceId::new(8662))
        .unwrap()
    else {
        panic!("live subscriber")
    };
    host.state_subscriber_removed(PluginInstanceId::new(8662));
    entry.instance.destroy(Duration::from_secs(5)).unwrap();
    assert!(demand(&publisher_events).selectors.is_empty());
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn subscription_replacement_clears_queued_old_keys() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_, host) = test_host(rt.handle().clone());
    let (id, handle, _) = publisher(&host, 8671);
    let events = subscriber(&host, 8672, None);
    observe(&host, 8672, &events, handle);
    assert_eq!(
        subscribe(
            &host,
            8672,
            Some(handle),
            vec![select("vosu.play", "/combo")]
        ),
        vosu_status::VOSU_OK
    );
    next_update(&events);
    let (entered_tx, entered_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel(1);
    {
        let instances = host.instances.lock().unwrap();
        let super::super::InstanceSlot::Live(entry) =
            instances.get(&PluginInstanceId::new(8672)).unwrap()
        else {
            panic!("live subscriber")
        };
        entry
            .instance
            .dispatch_task(Box::new(move |_, _| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(RECV_TIMEOUT).unwrap();
            }))
            .unwrap();
    }
    entered_rx.recv_timeout(RECV_TIMEOUT).unwrap();
    publish(
        &id,
        handle,
        &[("vosu.play", "/combo", PublishValueRef::U64(31))],
    );
    assert_eq!(
        subscribe(
            &host,
            8672,
            Some(handle),
            vec![select("vosu.play", "/accuracy")]
        ),
        vosu_status::VOSU_OK
    );
    release_tx.send(()).unwrap();
    let values = next_update(&events);
    assert_eq!(values.len(), 1);
    assert_eq!(values[0].pointer, "/accuracy");
    host.destroy_all(Duration::from_secs(5));
}
