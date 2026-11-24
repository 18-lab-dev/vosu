#![cfg_attr(
    test,
    expect(
        clippy::disallowed_methods,
        reason = "Tests write only isolated resource fixtures"
    )
)]

use super::*;
use crate::resources::publication::Publication;
use std::sync::atomic::AtomicU64;
use vosu_plugin_api::{ResourceRef, ResourceResolve, ResourceResolved};

#[derive(Debug)]
enum Event {
    Root(String),
    Batch(Vec<(String, Option<ResourceRef>)>),
    Reply(Message),
}
struct ResourceHost(mpsc::SyncSender<Event>);
impl HostServices for ResourceHost {
    fn send(&self, message: &Message, _: u64) -> vosu_status {
        match message {
            Message::RootDeclare(root) => self.0.try_send(Event::Root(root.name.clone())).unwrap(),
            Message::ResourceResolved(_) | Message::StreamChunk(_) => {
                self.0.try_send(Event::Reply(message.clone())).unwrap()
            }
            Message::LogRecord(_) => (),
            _ => panic!("unexpected resource message"),
        }
        vosu_status::VOSU_OK
    }
    fn send_flat(&self, _: vosu_opcode, _: u32, _: u64, bytes: &[u8]) -> vosu_status {
        let batch = decode_publish_batch_ref(bytes).unwrap();
        let fields = batch
            .entries()
            .filter_map(|field| {
                if !(field.ns == "vosu.chart" && field.pointer.starts_with("/resources/")
                    || field.ns == "vosu.skin" && field.pointer == "/tree")
                {
                    return None;
                }
                let reference = match field.value {
                    PublishValueRef::ResourceRef(bytes) | PublishValueRef::TreeRef(bytes) => {
                        Some(vosu_plugin_api::decode(bytes).unwrap())
                    }
                    PublishValueRef::Null => None,
                    _ => panic!("not a typed reference"),
                };
                Some((field.pointer.to_owned(), reference))
            })
            .collect();
        self.0.try_send(Event::Batch(fields)).unwrap();
        vosu_status::VOSU_OK
    }
}
fn receive(
    receiver: &mpsc::Receiver<Event>,
    mut predicate: impl FnMut(Event) -> Option<ResourceRef>,
) -> ResourceRef {
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let event = receiver
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap();
        if let Some(reference) = predicate(event) {
            return reference;
        }
    }
}
fn chart(event: Event) -> Option<ResourceRef> {
    if let Event::Batch(fields) = event {
        fields
            .into_iter()
            .find(|(p, r)| p == "/resources/file" && r.is_some())
            .and_then(|(_, r)| r)
    } else {
        None
    }
}
#[test]
fn production_publisher_prepares_references_and_republishes_after_gone_and_generation() {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir(root.path().join("set")).unwrap();
    std::fs::write(
        root.path().join("set/chart.osu"),
        b"first cold representation",
    )
    .unwrap();
    let (sender, receiver) = mpsc::sync_channel(128);
    let host: Arc<dyn HostServices + Send + Sync> = Arc::new(ResourceHost(sender));
    let bus = Arc::new(BusPublisher::new(
        SessionHost(host.clone()),
        812,
        Arc::new(AtomicBool::new(true)),
        SessionFacts {
            client: ClientType::Stable,
            settings: InstanceSettings {
                tournament_spectator: false,
                tournament_ipc_id: None,
                devserver_endpoint: None,
            },
            foreground: None,
        },
    ));
    let publisher: Arc<dyn crate::resources::publication::RefreshPublication> = bus.clone();
    let resources = Publication::start(
        Arc::downgrade(&publisher),
        host,
        812,
        Arc::new(AtomicU64::new(1)),
    );
    assert!(bus.resources.set(resources.clone()).is_ok());
    let mut snapshot = sample(29);
    let regular = Arc::make_mut(&mut Arc::make_mut(&mut snapshot).regular);
    regular.global.songs_folder = root.path().to_str().unwrap().into();
    // A deliberately different raw memory path must not override the selected root.
    regular.global.memory_songs_folder = root.path().join("wrong-root").to_str().unwrap().into();
    regular.menu.folder = Some("set".into());
    regular.menu.filename = Some("chart.osu".into());
    bus.publish(snapshot.clone(), Delivery::All).unwrap();
    let mut declared = false;
    let first = receive(&receiver, |event| match event {
        Event::Root(name) => {
            assert_eq!(name, "songs");
            declared = true;
            None
        }
        event => {
            let reference = chart(event);
            if reference.is_some() {
                assert!(declared, "roots precede production references");
            }
            reference
        }
    });
    assert_eq!(first.name, "chart.osu");
    assert_eq!(first.kind, "file");
    std::fs::write(
        root.path().join("set/chart.osu"),
        b"changed representation with distinct length",
    )
    .unwrap();
    resources
        .request(
            Message::ResourceResolve(ResourceResolve {
                handle: 812,
                kind: first.kind.clone(),
                key: first.key.clone(),
            }),
            73,
        )
        .unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Event::Reply(Message::ResourceResolved(ResourceResolved::Error { error })) = receiver
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap()
        {
            assert_eq!(error.code, "gone");
            break;
        }
    }
    let second = receive(&receiver, chart);
    assert_ne!(second.content_id, first.content_id);
    assert_ne!(second.key, first.key);
    Arc::make_mut(&mut snapshot).generation = 30;
    bus.publish(snapshot, Delivery::All).unwrap();
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        if let Event::Batch(fields) = receiver
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap()
            && fields
                .iter()
                .any(|(path, reference)| path == "/resources/file" && reference.is_none())
        {
            assert_eq!(fields.len(), 5);
            assert!(fields.iter().all(|(_, reference)| reference.is_none()));
            break;
        }
    }
    assert_eq!(receive(&receiver, chart), second);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), resources.shutdown())
            .await
            .unwrap();
    });
}
