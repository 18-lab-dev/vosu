use super::*;
use crate::plugin::{focus::SyntheticForegroundSource, host::InstanceSlot};
use crate::state::SessionKey;

fn owned_subscriber(
    host: &Host,
    n: u64,
    pool: Option<Arc<WorkerPool>>,
) -> (Receiver<SubscriberEvent>, u64) {
    let id = format!("test.k6.review.owner{n}");
    let (events, _commands) = subscriber::register(&id);
    let options = if pool.is_some() {
        "concurrent_dispatch=true\n"
    } else {
        ""
    };
    let manifest = session_manifest(&id, &[&id]).replace(
        "[match]",
        &format!("{options}covers=['field:vosu.play.combo']\n[match]"),
    );
    let loaded = registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest: &manifest,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: subscriber::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    host.create_instance(
        PluginInstanceId::new(n),
        &loaded[0],
        std::env::temp_dir(),
        pool,
    )
    .unwrap();
    host.process_appeared(test_process(n as u32, &id));
    let SubscriberEvent::Attached(attached) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("owner attach")
    };
    assert_eq!(
        host.handle(
            PluginInstanceId::new(n),
            Message::SessionIdentify(vosu_plugin_api::SessionIdentify {
                handle: attached.handle,
                game: "testkit".into(),
                variant: "review".into(),
                name: "Owner subscriber".into(),
                version: "1".into(),
                focus_priority: false,
            }),
            0
        ),
        vosu_status::VOSU_OK
    );
    assert!(
        matches!(events.recv_timeout(RECV_TIMEOUT).unwrap(), SubscriberEvent::Focus(f) if f.handle == attached.handle)
    );
    (events, attached.handle)
}

fn publish_owned(host: &Host, who: u64, handle: u64, value: u64) {
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    writer.push("vosu.play", "/combo", PublishValueRef::U64(value));
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    assert_eq!(
        host.handle_publish_raw(
            PluginInstanceId::new(who),
            vosu_opcode::VOSU_OP_PUBLISH_BATCH,
            &bytes
        ),
        vosu_status::VOSU_OK
    );
}

#[test]
fn detach_flushes_committed_state_before_its_notification() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_, host) = test_host(rt.handle().clone());
    let (_, handle, _) = publisher(&host, 90_101);
    let events = subscriber(&host, 90_102, None);
    observe(&host, 90_102, &events, handle);
    assert_eq!(
        subscribe(
            &host,
            90_102,
            Some(handle),
            vec![select("vosu.play", "/combo")]
        ),
        vosu_status::VOSU_OK
    );
    next_update(&events);
    let release = block_subscriber(&host, 90_102);
    let publisher = {
        let instances = host.instances.lock().unwrap();
        let InstanceSlot::Live(entry) = instances.get(&PluginInstanceId::new(90_101)).unwrap()
        else {
            panic!("live publisher")
        };
        entry.publisher.clone()
    };
    // Drive precisely the production gap between store.commit and acquiring
    // state_serial for notification; no scheduling race or sleep is needed.
    let outcome = host
        .state()
        .store
        .commit(
            &publisher.grant,
            SessionKey(handle),
            std::iter::once(vosu_plugin_api::PublishFieldRef {
                handle,
                ns: "vosu.play",
                pointer: "/combo",
                value: PublishValueRef::U64(41),
            }),
        )
        .unwrap();
    host.process_exited(90_101);
    host.state_committed(&outcome);
    release.send(()).unwrap();
    let SubscriberEvent::Update { values, .. } = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
        panic!("final committed update")
    };
    assert_eq!(values[0].value, Some(41u64.into()));
    assert!(
        matches!(events.recv_timeout(RECV_TIMEOUT).unwrap(), SubscriberEvent::Detached(d) if d.handle == handle)
    );
    fence(&host, 90_102);
    assert!(events.try_recv().is_err());
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn lifecycle_overflow_preserves_owned_session_subscription() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_, host) = test_host(rt.handle().clone());
    host.set_lifecycle_cap_for_tests(1);
    let (foreign_id, foreign, publisher_events) = publisher(&host, 90_121);
    let (events, owned) = owned_subscriber(&host, 90_122, None);
    observe(&host, 90_122, &events, foreign);
    publish_owned(&host, 90_122, owned, 0);
    assert_eq!(
        subscribe(
            &host,
            90_122,
            Some(owned),
            vec![select("vosu.play", "/combo")]
        ),
        vosu_status::VOSU_OK
    );
    assert_eq!(next_update(&events)[0].value, Some(0u64.into()));
    let release = block_subscriber(&host, 90_122);
    attach_another(&host, &foreign_id, &publisher_events, 901_211);
    attach_another(&host, &foreign_id, &publisher_events, 901_212);
    assert!(
        !host
            .lifecycle_backlog_for_tests(PluginInstanceId::new(90_122))
            .1
    );
    let demand = host
        .state()
        .subscriptions
        .lock()
        .unwrap()
        .demand(SessionKey(owned), None);
    assert_eq!(demand.selectors.len(), 1);
    assert_eq!(demand.selectors[0].1.as_ref(), "/combo");
    publish_owned(&host, 90_122, owned, 88);
    release.send(()).unwrap();
    let mut lost = false;
    let mut updated = false;
    let deadline = Instant::now() + RECV_TIMEOUT;
    while !lost || !updated {
        match events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap()
        {
            SubscriberEvent::Detached(d) => {
                assert_eq!(d.handle, foreign);
                assert_eq!(d.reason, vosu_plugin_api::DetachReason::Lost);
                lost = true;
            }
            SubscriberEvent::Update { session, values } => {
                assert_eq!(session, owned);
                assert_eq!(values[0].value, Some(88u64.into()));
                updated = true;
            }
            event => panic!("unexpected overflow event: {event:?}"),
        }
    }
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn owner_watching_sessions_receives_focus_once() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let foreground = Arc::new(SyntheticForegroundSource::new(None));
    let (_, host) = super::super::helpers::test_host_with(
        rt.handle().clone(),
        foreground.clone(),
        Duration::from_secs(5),
    );
    let (events, owned) = owned_subscriber(&host, 90_131, None);
    assert_eq!(
        subscribe(&host, 90_131, None, vec![select("vosu.sessions", "")]),
        vosu_status::VOSU_OK
    );
    fence(&host, 90_131);
    assert!(events.try_recv().is_err());
    foreground.set(Some(90_131));
    host.foreground_changed();
    fence(&host, 90_131);
    assert!(
        matches!(events.recv_timeout(RECV_TIMEOUT).unwrap(), SubscriberEvent::Focus(f) if f.handle == owned && f.foreground == Some(true))
    );
    assert!(events.try_recv().is_err());
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn owned_publication_flushes_before_detach_with_full_dispatch_queue() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_, host) = test_host(rt.handle().clone());
    let (events, owned) = owned_subscriber(&host, 90_141, Some(WorkerPool::new(1, 1)));
    assert_eq!(
        subscribe(
            &host,
            90_141,
            Some(owned),
            vec![select("vosu.play", "/combo")]
        ),
        vosu_status::VOSU_OK
    );
    next_update(&events);
    // The baseline event is recorded before its drain finishes. Also wait for
    // the owner's coalesced DEMAND_CHANGED so the tiny queue starts empty.
    rt.block_on(async {
        vosu_proptest::bounded!(RECV_TIMEOUT, async {
            while !host
                .state_delivery_idle_for_tests(PluginInstanceId::new(90_141), SessionKey(owned))
            {
                tokio::task::yield_now().await;
            }
        });
    });
    let release = block_subscriber(&host, 90_141);
    {
        let instances = host.instances.lock().unwrap();
        let InstanceSlot::Live(entry) = instances.get(&PluginInstanceId::new(90_141)).unwrap()
        else {
            panic!("live owner")
        };
        entry.instance.dispatch_task(Box::new(|_, _| {})).unwrap();
    }
    publish_owned(&host, 90_141, owned, 99);
    host.process_exited(90_141);
    release.send(()).unwrap();
    let SubscriberEvent::Update { session, values } = events.recv_timeout(RECV_TIMEOUT).unwrap()
    else {
        panic!("owner final update")
    };
    assert_eq!(session, owned);
    assert_eq!(values[0].value, Some(99u64.into()));
    assert!(
        matches!(events.recv_timeout(RECV_TIMEOUT).unwrap(), SubscriberEvent::Detached(d) if d.handle == owned)
    );
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn lifecycle_cap_counts_pending_detaches_not_announced_live_sessions() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_, host) = test_host(rt.handle().clone());
    host.set_lifecycle_cap_for_tests(1);
    let (id, first, publisher_events) = publisher(&host, 90_151);
    let events = subscriber(&host, 90_152, None);
    observe(&host, 90_152, &events, first);
    let mut handles = std::collections::HashSet::from([first]);
    for pid in [901_511, 901_512] {
        let handle = attach_another(&host, &id, &publisher_events, pid);
        assert!(
            matches!(events.recv_timeout(RECV_TIMEOUT).unwrap(), SubscriberEvent::Attached(a) if a.handle == handle)
        );
        assert!(
            matches!(events.recv_timeout(RECV_TIMEOUT).unwrap(), SubscriberEvent::Focus(f) if f.handle == handle)
        );
        handles.insert(handle);
        assert_eq!(
            host.lifecycle_backlog_for_tests(PluginInstanceId::new(90_152)),
            (0, true)
        );
    }
    let release = block_subscriber(&host, 90_152);
    host.process_exited(90_151);
    assert_eq!(
        host.lifecycle_backlog_for_tests(PluginInstanceId::new(90_152)),
        (1, true)
    );
    host.process_exited(901_511);
    assert!(
        !host
            .lifecycle_backlog_for_tests(PluginInstanceId::new(90_152))
            .1
    );
    release.send(()).unwrap();
    for _ in 0..3 {
        let SubscriberEvent::Detached(detached) = events.recv_timeout(RECV_TIMEOUT).unwrap() else {
            panic!("lost announced session")
        };
        assert_eq!(detached.reason, vosu_plugin_api::DetachReason::Lost);
        assert!(handles.remove(&detached.handle));
    }
    assert!(handles.is_empty());
    host.destroy_all(Duration::from_secs(5));
}
