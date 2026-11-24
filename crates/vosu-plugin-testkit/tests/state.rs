//! State fixtures exercise real SDK host sends and lifecycle cleanup.
#![cfg(feature = "test-harness")]

use std::ffi::c_void;
use std::sync::Mutex;
use std::sync::mpsc::{TryRecvError, sync_channel};
use std::time::Duration;
use vosu_plugin_api::*;
use vosu_plugin_testkit::{publisher, subscriber};

const TIMEOUT: Duration = Duration::from_secs(5);

fn init(id: &str) -> Init {
    Init {
        plugin_id: id.into(),
        settings: CborValue::Map(vec![]),
        data_dir: std::env::temp_dir(),
    }
}

unsafe extern "C" fn record(ctx: *mut c_void, message: *const vosu_message) -> u32 {
    // SAFETY: each test keeps its message log and SDK payload live during sends.
    let messages = unsafe { &*ctx.cast::<Mutex<Vec<Message>>>() };
    // SAFETY: the SDK supplies a valid message and payload for this call.
    let raw = unsafe { &*message };
    // SAFETY: the SDK's payload allocation spans payload.len bytes.
    let bytes = unsafe { std::slice::from_raw_parts(raw.payload.data, raw.payload.len as usize) };
    messages.lock().unwrap().push(
        Message::decode(vosu_opcode::try_from(raw.opcode).unwrap(), raw.flags, bytes).unwrap(),
    );
    vosu_status::VOSU_OK as u32
}

fn services() -> vosu_host_services {
    vosu_host_services {
        struct_size: std::mem::size_of::<vosu_host_services>() as u32,
        abi_version: VOSU_PLUGIN_ABI_MAJOR,
        send: Some(record),
    }
}

fn entry(pointer: &str) -> StateEntry {
    StateEntry {
        ns: "vosu.play".into(),
        pointer: pointer.into(),
        value: Some(CborValue::Bool(true)),
        error: None,
    }
}

#[test]
fn subscriber_reassembles_parts_and_joins_command_thread() {
    let id = "vosu.kt.reassembly";
    let (events, commands) = subscriber::register(id);
    let messages = Mutex::new(Vec::new());
    let services = services();
    // SAFETY: services and messages outlive the plugin and its joined command thread.
    let host = unsafe {
        VtableHostServices::new(
            &services,
            (&messages as *const Mutex<Vec<Message>>).cast_mut().cast(),
        )
    };
    let mut plugin = subscriber::SubscriberPlugin::create(host, init(id)).unwrap();
    plugin
        .on_message(
            Message::StateUpdate(StateUpdate {
                session: 71,
                values: vec![entry("/score")],
                more: true,
            }),
            0,
        )
        .unwrap();
    assert_eq!(events.try_recv(), Err(TryRecvError::Empty));
    plugin
        .on_message(
            Message::StateUpdate(StateUpdate {
                session: 72,
                values: vec![entry("/health")],
                more: false,
            }),
            0,
        )
        .unwrap();
    assert_eq!(
        events.recv_timeout(TIMEOUT).unwrap(),
        subscriber::SubscriberEvent::Update {
            session: 72,
            values: vec![entry("/health")]
        }
    );
    plugin
        .on_message(
            Message::StateUpdate(StateUpdate {
                session: 71,
                values: vec![entry("/combo")],
                more: false,
            }),
            0,
        )
        .unwrap();
    assert_eq!(
        events.recv_timeout(TIMEOUT).unwrap(),
        subscriber::SubscriberEvent::Update {
            session: 71,
            values: vec![entry("/score"), entry("/combo")]
        }
    );
    commands
        .send(subscriber::SubscribeCommand {
            session: Some(71),
            selectors: vec![],
        })
        .unwrap();
    assert_eq!(
        events.recv_timeout(TIMEOUT).unwrap(),
        subscriber::SubscriberEvent::SubscribeStatus(vosu_status::VOSU_OK)
    );
    // Fill the event channel without draining it: dispatch must not block.
    for _ in 0..100 {
        plugin
            .on_message(
                Message::FocusChanged(FocusChanged {
                    handle: 71,
                    focused: true,
                    foreground: None,
                }),
                0,
            )
            .unwrap();
    }
    drop(plugin);
    assert!(
        commands
            .send(subscriber::SubscribeCommand {
                session: None,
                selectors: vec![]
            })
            .is_err()
    );
    assert_eq!(messages.lock().unwrap().len(), 1);
    let _replacement = subscriber::register(id);
}

#[test]
fn duplicate_ids_panic_without_overwriting_live_registrations() {
    let id = "vosu.kt.duplicate";
    let _events = publisher::register(id);
    assert!(std::panic::catch_unwind(|| publisher::register(id)).is_err());
    publisher::register_manifest(id, "first");
    assert!(std::panic::catch_unwind(|| publisher::register_manifest(id, "second")).is_err());
    assert_eq!(publisher::manifest(id), "first");
    let messages = Mutex::new(Vec::new());
    let services = services();
    // SAFETY: host data outlives both create attempts and the plugin.
    let host = unsafe {
        VtableHostServices::new(
            &services,
            (&messages as *const Mutex<Vec<Message>>).cast_mut().cast(),
        )
    };
    let plugin = publisher::PublisherPlugin::create(host, init(id)).unwrap();
    assert!(
        std::panic::catch_unwind(|| publisher::PublisherPlugin::create(host, init(id))).is_err()
    );
    drop(plugin);
    let _replacement = publisher::register(id);
    let sid = "vosu.kt.duplicate-subscriber";
    let _registration = subscriber::register(sid);
    assert!(std::panic::catch_unwind(|| subscriber::register(sid)).is_err());
}

#[test]
fn writers_publish_many_batches_and_retained_host_refuses_after_destroy() {
    let id = "vosu.kt.multibatch";
    let _events = publisher::register(id);
    let messages = Mutex::new(Vec::new());
    let services = services();
    // SAFETY: host data outlives the plugin and all writer calls.
    let host = unsafe {
        VtableHostServices::new(
            &services,
            (&messages as *const Mutex<Vec<Message>>).cast_mut().cast(),
        )
    };
    let plugin = publisher::PublisherPlugin::create(host, init(id)).unwrap();
    let retained = publisher::host_services(id);
    let (done_tx, done_rx) = sync_channel(2);
    let writers: Vec<publisher::Writer> = (0..2)
        .map(|thread| {
            let done = done_tx.clone();
            let mut count = 0;
            Box::new(move |batch: &mut PublishBatchWriter| {
                if count == 10_000 {
                    done.send(()).unwrap();
                    return false;
                }
                batch.push(
                    "vosu.play",
                    "/score",
                    PublishValueRef::U64(thread * 10_000 + count),
                );
                count += 1;
                true
            }) as publisher::Writer
        })
        .collect();
    let threads = publisher::spawn_writers(id, 81, writers);
    for _ in 0..2 {
        done_rx.recv_timeout(TIMEOUT).unwrap();
    }
    for thread in threads {
        assert_eq!(thread.join().unwrap(), vosu_status::VOSU_OK);
    }
    let mut scores: Vec<u64> = messages
        .lock()
        .unwrap()
        .iter()
        .map(|message| {
            let Message::PublishBatch(batch) = message else {
                panic!("expected batch")
            };
            assert_eq!(batch.handle, 81);
            assert_eq!(batch.entries.len(), 1);
            let PublishValue::U64(score) = batch.entries[0].value else {
                panic!("expected score")
            };
            score
        })
        .collect();
    scores.sort_unstable();
    assert_eq!(scores, (0..20_000).collect::<Vec<_>>());
    drop(plugin);
    assert_eq!(
        retained.publish(81, "vosu.play", "/score", PublishValue::U64(1)),
        vosu_status::VOSU_ERR_TARGET_GONE
    );
    assert_eq!(messages.lock().unwrap().len(), 20_000);
    let _replacement = publisher::register(id);
}
