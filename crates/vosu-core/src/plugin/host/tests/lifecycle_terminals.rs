//! Lifecycle terminals are reserved (`docs/v2/plugin-abi.md` §7.6): a
//! `SESSION_DETACHED` queued while the instance's dispatch queue is full
//! still reaches the owning plugin and every `vosu.sessions` watcher, after
//! everything queued before it, exactly once, even when the owner leaves
//! service right after the detach (teardown or restart).
//!
//! Each instance here uses its own serial worker. [`fill`] blocks it on a
//! first gate, queues a second gate, then queues no-ops until the ordinary
//! queue refuses one. Once released, the second gate runs only after
//! everything queued before it, so a detach seen before the second gate is
//! entered would have overtaken queued work.

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::{Duration, Instant};

use vosu_plugin_api::{
    DetachReason, HostServices, Message, PublishBatchWriter, PublishValueRef, Selector,
    SessionAttached, SessionDetached, SessionIdentify, StateSubscribe, decode, vosu_opcode,
    vosu_status,
};
use vosu_plugin_testkit::{
    publisher::{self, PublisherEvent},
    subscriber::{self, SubscriberEvent},
};

use super::super::{InstanceSlot, PluginHost};
use super::helpers::{RECV_TIMEOUT, session_manifest, test_host, test_process};
use crate::plugin::host_services::PluginHostHandler;
use crate::plugin::instance::{DEFAULT_QUEUE_CAPACITY, PluginInstance};
use crate::plugin::test_support::{LaunchBehavior, RecordingLauncher, write_development_package};
use crate::plugin::{HostError, PluginInstanceId, registry};

type Host = Arc<PluginHost>;

/// How long a gate job waits for its release before giving up.
const GATE_HOLD: Duration = Duration::from_secs(30);

fn load(
    manifest: &str,
    vtable: fn() -> vosu_plugin_api::vosu_plugin_vtable,
) -> registry::LoadedPlugin {
    registry::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        &[registry::BundledPlugin {
            manifest,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable,
        }],
    )
    .unwrap()
    .remove(0)
}

/// An owning game plugin on its own serial worker; returns its first
/// session's handle.
fn owner(host: &Host, n: u64) -> (String, u64, Receiver<PublisherEvent>) {
    let id = format!("test.m5d.owner{n}");
    let manifest = session_manifest(&id, &[&id])
        .replace("[match]", "covers=['field:vosu.play.combo']\n[match]");
    publisher::register_manifest(&id, &manifest);
    let events = publisher::register(&id);
    let loaded = load(&manifest, publisher::vosu_plugin_vtable);
    host.create_instance(
        PluginInstanceId::new(n),
        &loaded,
        std::env::temp_dir(),
        None,
    )
    .unwrap();
    let handle = attach(host, &id, &events, n as u32);
    (id, handle, events)
}

fn attach(host: &Host, owner: &str, events: &Receiver<PublisherEvent>, pid: u32) -> u64 {
    host.process_appeared(test_process(pid, owner));
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

/// A non-owning plugin on its own serial worker.
fn watcher(host: &Host, n: u64) -> Receiver<SubscriberEvent> {
    let id = format!("test.m5d.watcher{n}");
    let (events, _commands) = subscriber::register(&id);
    let manifest = session_manifest(&id, &[&id]);
    let loaded = load(&manifest, subscriber::vosu_plugin_vtable);
    host.create_instance(
        PluginInstanceId::new(n),
        &loaded,
        std::env::temp_dir(),
        None,
    )
    .unwrap();
    events
}

fn subscribe(host: &Host, n: u64, session: Option<u64>, ns: &str, pointer: &str) {
    let status = host.handle(
        PluginInstanceId::new(n),
        Message::StateSubscribe(StateSubscribe {
            session,
            selectors: vec![Selector {
                ns: ns.into(),
                pointer: pointer.into(),
            }],
        }),
        0,
    );
    assert_eq!(status, vosu_status::VOSU_OK);
}

/// Subscribes `n` to `vosu.sessions` and returns every handle announced,
/// once each announcement's first `FOCUS_CHANGED` arrived too.
fn watch_sessions(
    host: &Host,
    n: u64,
    events: &Receiver<SubscriberEvent>,
    count: usize,
) -> Vec<u64> {
    subscribe(host, n, None, "vosu.sessions", "");
    let mut handles = Vec::new();
    let mut focus = 0;
    while handles.len() < count || focus < count {
        match events.recv_timeout(RECV_TIMEOUT).unwrap() {
            SubscriberEvent::Attached(attached) => handles.push(attached.handle),
            SubscriberEvent::Focus(_) => focus += 1,
            event => panic!("expected the session list, got {event:?}"),
        }
    }
    handles
}

fn publish(id: &str, handle: u64, combo: u64) {
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    writer.push("vosu.play", "/combo", PublishValueRef::U64(combo));
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    assert_eq!(
        publisher::host_services(id).send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &bytes),
        vosu_status::VOSU_OK
    );
}

fn with_live<R>(host: &Host, n: u64, f: impl FnOnce(&PluginInstance) -> R) -> R {
    let instances = host.instances.lock().unwrap();
    let Some(InstanceSlot::Live(entry)) = instances.get(&PluginInstanceId::new(n)) else {
        panic!("instance {n} is not live")
    };
    f(&entry.instance)
}

/// Waits until instance `n` is out of service and still draining.
fn wait_destroying(host: &Host, n: u64) {
    let deadline = Instant::now() + RECV_TIMEOUT;
    while !matches!(
        host.instances
            .lock()
            .unwrap()
            .get(&PluginInstanceId::new(n)),
        Some(InstanceSlot::Destroying { .. })
    ) {
        assert!(Instant::now() < deadline, "instance {n} never left service");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn gate() -> (
    Receiver<()>,
    SyncSender<()>,
    crate::plugin::dispatch::DispatchTask,
) {
    let (entered_tx, entered_rx) = sync_channel(1);
    let (release_tx, release_rx) = sync_channel::<()>(1);
    let task = Box::new(move |_: &crate::plugin::execution::DispatchTarget| {
        let _ = entered_tx.send(());
        let _ = release_rx.recv_timeout(GATE_HOLD);
    });
    (entered_rx, release_tx, task)
}

struct Gates {
    first: SyncSender<()>,
    second_entered: Receiver<()>,
    second: SyncSender<()>,
}

impl Gates {
    /// Runs everything queued before the second gate, which then holds.
    fn run_to_second(&self) {
        self.first.send(()).unwrap();
        self.second_entered.recv_timeout(RECV_TIMEOUT).unwrap();
    }
}

/// Blocks `n`'s worker and fills its ordinary queue (module docs).
fn fill(host: &Host, n: u64) -> Gates {
    let (first_entered, first, task) = gate();
    with_live(host, n, |instance| instance.dispatch_task(task).unwrap());
    first_entered.recv_timeout(RECV_TIMEOUT).unwrap();
    let (second_entered, second, task) = gate();
    with_live(host, n, |instance| {
        instance.dispatch_task(task).unwrap();
        let mut queued = 1;
        while instance.dispatch_task(Box::new(|_| {})).is_ok() {
            queued += 1;
        }
        assert_eq!(queued, DEFAULT_QUEUE_CAPACITY);
        assert!(matches!(
            instance.dispatch_task(Box::new(|_| {})),
            Err(HostError::DispatchQueueFull)
        ));
    });
    Gates {
        first,
        second_entered,
        second,
    }
}

/// Every job queued so far on `n` has run.
fn fence(host: &Host, n: u64) {
    let (tx, rx) = sync_channel(1);
    with_live(host, n, |instance| {
        instance
            .dispatch_task(Box::new(move |_| {
                let _ = tx.send(());
            }))
            .unwrap();
    });
    rx.recv_timeout(RECV_TIMEOUT).unwrap();
}

fn owner_detaches(events: &Receiver<PublisherEvent>) -> Vec<u64> {
    events
        .try_iter()
        .filter_map(|event| match event {
            PublisherEvent::Detached { handle } => Some(handle),
            _ => None,
        })
        .collect()
}

fn next_owner_detach(events: &Receiver<PublisherEvent>) -> u64 {
    let deadline = Instant::now() + RECV_TIMEOUT;
    loop {
        if let PublisherEvent::Detached { handle } = events
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .unwrap()
        {
            return handle;
        }
    }
}

fn next_watcher_detach(events: &Receiver<SubscriberEvent>) -> SessionDetached {
    match events.recv_timeout(RECV_TIMEOUT).unwrap() {
        SubscriberEvent::Detached(detached) => detached,
        event => panic!("expected SESSION_DETACHED, got {event:?}"),
    }
}

#[test]
fn a_detach_behind_a_full_queue_reaches_owner_and_watcher_in_order() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let (owner_id, handle, owner_events) = owner(&host, 87_101);
    let watcher_events = watcher(&host, 87_102);
    assert_eq!(watch_sessions(&host, 87_102, &watcher_events, 1), [handle]);
    subscribe(&host, 87_102, Some(handle), "vosu.play", "/combo");
    assert!(matches!(
        watcher_events.recv_timeout(RECV_TIMEOUT).unwrap(),
        SubscriberEvent::Update { .. }
    ));
    let owner_gates = fill(&host, 87_101);
    let watcher_gates = fill(&host, 87_102);
    // Refused by the watcher's full queue: its delivery waits to retry.
    publish(&owner_id, handle, 42);

    // The teardown detaches the owner's session while both queues are full,
    // then takes the owner out of service at once.
    let teardown = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || host.teardown_instance(PluginInstanceId::new(87_101)))
    };
    wait_destroying(&host, 87_101);
    owner_gates.run_to_second();
    assert!(
        owner_detaches(&owner_events).is_empty(),
        "the detach overtook queued work"
    );
    owner_gates.second.send(()).unwrap();
    assert_eq!(next_owner_detach(&owner_events), handle);
    teardown.join().unwrap().unwrap();
    assert!(
        owner_detaches(&owner_events).is_empty(),
        "detach delivered twice"
    );

    watcher_gates.run_to_second();
    assert!(
        watcher_events.try_recv().is_err(),
        "the final state or detach overtook queued work"
    );
    watcher_gates.second.send(()).unwrap();
    // The session's final state precedes its detach.
    match watcher_events.recv_timeout(RECV_TIMEOUT).unwrap() {
        SubscriberEvent::Update { session, values } => {
            assert_eq!(session, handle);
            assert_eq!(values[0].value, Some(42u64.into()));
        }
        event => panic!("expected the final state, got {event:?}"),
    }
    let detached = next_watcher_detach(&watcher_events);
    assert_eq!(
        (detached.handle, detached.reason),
        (handle, DetachReason::Lost)
    );
    fence(&host, 87_102);
    assert!(watcher_events.try_recv().is_err(), "detach delivered twice");
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn many_sessions_detaching_behind_full_queues_each_take_one_lifecycle_entry() {
    const SESSIONS: usize = 16;
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let (owner_id, first, owner_events) = owner(&host, 87_201);
    let mut handles = vec![first];
    for pid in 0..SESSIONS as u32 - 1 {
        handles.push(attach(&host, &owner_id, &owner_events, 872_100 + pid));
    }
    let watcher_events = watcher(&host, 87_202);
    let announced = watch_sessions(&host, 87_202, &watcher_events, SESSIONS);
    let expected: HashSet<u64> = handles.iter().copied().collect();
    assert_eq!(announced.into_iter().collect::<HashSet<_>>(), expected);
    let owner_gates = fill(&host, 87_201);
    let watcher_gates = fill(&host, 87_202);

    let teardown = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || host.teardown_instance(PluginInstanceId::new(87_201)))
    };
    wait_destroying(&host, 87_201);
    // One lifecycle entry per detached session, past the full queue.
    with_live(&host, 87_202, |instance| {
        assert_eq!(instance.lifecycle_pending(), SESSIONS);
    });
    // A duplicate detach and a superseded one take no further entry.
    host.state_session_detaching(
        &SessionDetached {
            handle: first,
            reason: DetachReason::Exited,
        },
        None,
    );
    for pid in 0..SESSIONS as u32 - 1 {
        host.process_exited(872_100 + pid);
    }
    host.process_exited(87_201);
    with_live(&host, 87_202, |instance| {
        assert_eq!(instance.lifecycle_pending(), SESSIONS);
    });

    owner_gates.run_to_second();
    assert!(owner_detaches(&owner_events).is_empty());
    owner_gates.second.send(()).unwrap();
    let mut seen = HashSet::new();
    for _ in 0..SESSIONS {
        assert!(
            seen.insert(next_owner_detach(&owner_events)),
            "a detach was delivered twice"
        );
    }
    assert_eq!(seen, expected);
    teardown.join().unwrap().unwrap();
    assert!(owner_detaches(&owner_events).is_empty());

    watcher_gates.run_to_second();
    assert!(watcher_events.try_recv().is_err());
    watcher_gates.second.send(()).unwrap();
    let mut seen = HashSet::new();
    for _ in 0..SESSIONS {
        let detached = next_watcher_detach(&watcher_events);
        // The first detach stands; the later `exited` one superseded nothing.
        assert_eq!(detached.reason, DetachReason::Lost);
        assert!(seen.insert(detached.handle), "a detach was delivered twice");
    }
    assert_eq!(seen, expected);
    fence(&host, 87_202);
    assert!(watcher_events.try_recv().is_err());
    with_live(&host, 87_202, |instance| {
        assert_eq!(instance.lifecycle_pending(), 0);
    });
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn a_restart_behind_a_full_queue_detaches_the_old_generation_exactly_once() {
    let rt = tokio::runtime::Runtime::new().unwrap();
    let _enter = rt.enter();
    let (_logs, host) = test_host(rt.handle().clone());
    let launcher = RecordingLauncher::new(LaunchBehavior::Ready);
    host.install_process_launcher(launcher.clone()).unwrap();
    let package = tempfile::tempdir().unwrap();
    write_development_package(
        package.path(),
        "test.m5d.process",
        "m5d-process",
        "process",
        "plugin.bin",
        b"never mapped by core",
    )
    .unwrap();
    let loaded = registry::load_development(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &crate::plugin::ValidationSnapshot::default(),
        package.path(),
        true,
    )
    .unwrap();
    let process = PluginInstanceId::new(87_301);
    host.create_instance(process, &loaded, std::env::temp_dir(), None)
        .unwrap();
    host.process_appeared(test_process(873_100, "m5d-process"));
    let old = Arc::clone(&launcher.launches()[0].endpoint);
    let attached = old
        .wait_for_frame(vosu_opcode::VOSU_OP_SESSION_ATTACHED as u32, RECV_TIMEOUT)
        .unwrap();
    let handle = decode::<SessionAttached>(&attached.payload).unwrap().handle;
    assert_eq!(
        host.handle(
            process,
            Message::SessionIdentify(SessionIdentify {
                handle,
                game: "fixture".into(),
                variant: "process".into(),
                name: "Process fixture".into(),
                version: "1".into(),
                focus_priority: false,
            }),
            0,
        ),
        vosu_status::VOSU_OK
    );
    let watcher_events = watcher(&host, 87_302);
    assert_eq!(watch_sessions(&host, 87_302, &watcher_events, 1), [handle]);
    let owner_gates = fill(&host, 87_301);
    let watcher_gates = fill(&host, 87_302);

    let restart = {
        let host = Arc::clone(&host);
        std::thread::spawn(move || host.restart_process_instance(process))
    };
    wait_destroying(&host, 87_301);
    // The process exit discovery reports during the restart finds the
    // session already detached.
    host.process_exited(873_100);
    owner_gates.run_to_second();
    let detaches = |endpoint: &crate::plugin::test_support::RecordingEndpoint| {
        endpoint
            .frames()
            .into_iter()
            .filter(|frame| frame.opcode == vosu_opcode::VOSU_OP_SESSION_DETACHED as u32)
            .map(|frame| decode::<SessionDetached>(&frame.payload).unwrap())
            .collect::<Vec<_>>()
    };
    assert!(detaches(&old).is_empty(), "the detach overtook queued work");
    owner_gates.second.send(()).unwrap();
    restart.join().unwrap().unwrap();
    assert_eq!(
        detaches(&old),
        [SessionDetached {
            handle,
            reason: DetachReason::Lost,
        }]
    );
    assert_eq!(old.destroy_calls(), 1);
    assert_eq!(launcher.wait_launches(2, RECV_TIMEOUT), 2);
    let new = Arc::clone(&launcher.launches()[1].endpoint);
    assert!(detaches(&new).is_empty());

    watcher_gates.run_to_second();
    assert!(watcher_events.try_recv().is_err());
    watcher_gates.second.send(()).unwrap();
    let detached = next_watcher_detach(&watcher_events);
    assert_eq!(
        (detached.handle, detached.reason),
        (handle, DetachReason::Lost)
    );
    fence(&host, 87_302);
    assert!(watcher_events.try_recv().is_err(), "detach delivered twice");
    host.destroy_all(Duration::from_secs(5));
}
