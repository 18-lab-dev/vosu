use super::*;
use vosu_plugin_api::{CborValue, ResourceResolve, StreamRead, TreeList};
use vosu_plugin_testkit::resources::{Reply, ResourceFixture, ResourceStep};

fn request() -> Message {
    Message::ResourceResolve(ResourceResolve {
        handle: 91,
        kind: "file".into(),
        key: CborValue::Text("distinct-key".into()),
    })
}
fn reply() -> ResourceReply {
    ResourceReply::Resolved(ResourceResolved::Path {
        root: "songs".into(),
        path: "chart.osu".into(),
    })
}

#[test]
fn resource_requests_reentrant_reply_is_reserved_before_fixture_dispatch() {
    let mut requests = Requests::default();
    let (mut fixture, events) = ResourceFixture::new();
    fixture
        .push(ResourceStep {
            request: request(),
            replies: vec![Reply::immediate(Message::ResourceResolved(match reply() {
                ResourceReply::Resolved(r) => r,
                _ => unreachable!(),
            }))],
        })
        .unwrap();
    let instance = PluginInstanceId(8101);
    let (id, mut receiver) = requests.reserve(instance, 7, &request()).unwrap();
    for envelope in fixture.dispatch(request(), id).unwrap() {
        let Message::ResourceResolved(result) = envelope.message else {
            panic!("wrong reply")
        };
        requests
            .complete(
                instance,
                7,
                envelope.correlation,
                ResourceReply::Resolved(result),
            )
            .unwrap();
    }
    assert_eq!(
        events
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap()
            .correlation,
        id
    );
    assert_eq!(receiver.try_recv().unwrap(), Ok(reply()));
    assert_eq!(fixture.counters().resolve, 1);
    assert!(requests.entries.is_empty());
}

#[test]
fn resource_requests_wrong_sender_generation_duplicate_and_shape() {
    let mut requests = Requests::default();
    let instance = PluginInstanceId(8102);
    let (id, mut receiver) = requests.reserve(instance, 11, &request()).unwrap();
    assert_eq!(
        requests.complete(PluginInstanceId(8103), 11, id, reply()),
        Err(ResourceRequestError::Cancelled)
    );
    assert_eq!(
        requests.complete(instance, 12, id, reply()),
        Err(ResourceRequestError::Cancelled)
    );
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Empty)
    ));
    requests.complete(instance, 11, id, reply()).unwrap();
    assert_eq!(receiver.try_recv().unwrap(), Ok(reply()));
    assert_eq!(
        requests.complete(instance, 11, id, reply()),
        Err(ResourceRequestError::Cancelled)
    );
    let list = Message::TreeList(TreeList {
        handle: 91,
        kind: "set".into(),
        key: CborValue::Null,
    });
    let (next, mut receiver) = requests.reserve(instance, 11, &list).unwrap();
    assert!(next > id);
    assert_eq!(
        requests.complete(instance, 11, next, reply()),
        Err(ResourceRequestError::InvalidReply)
    );
    assert_eq!(
        receiver.try_recv().unwrap(),
        Err(ResourceRequestError::InvalidReply)
    );
    assert!(requests.entries.is_empty());
}

#[test]
fn resource_requests_real_capacity_and_cleanup() {
    let mut requests = Requests::default();
    let instance = PluginInstanceId(8104);
    let mut receivers = Vec::new();
    for _ in 0..MAX_PENDING_RESOURCE_REQUESTS {
        receivers.push(requests.reserve(instance, 2, &request()).unwrap().1);
    }
    assert!(matches!(
        requests.reserve(instance, 2, &request()),
        Err(ResourceRequestError::Busy)
    ));
    let (other, mut other_receiver) = requests
        .reserve(PluginInstanceId(8105), 1, &request())
        .unwrap();
    requests.cancel(instance, Some(92));
    assert_eq!(requests.entries.len(), MAX_PENDING_RESOURCE_REQUESTS + 1);
    requests.cancel(instance, Some(91));
    for mut receiver in receivers {
        assert!(matches!(
            receiver.try_recv(),
            Err(oneshot::error::TryRecvError::Closed)
        ));
    }
    assert_eq!(requests.entries.len(), 1);
    requests.remove(other);
    assert!(matches!(
        other_receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Closed)
    ));
    let (_, mut receiver) = requests.reserve(instance, 3, &request()).unwrap();
    requests.cancel(instance, None);
    assert!(matches!(
        receiver.try_recv(),
        Err(oneshot::error::TryRecvError::Closed)
    ));
    assert!(requests.entries.is_empty());
}

#[test]
fn resource_requests_stream_chunk_bound_and_eof() {
    let mut requests = Requests::default();
    let instance = PluginInstanceId(8106);
    let read = |len| {
        Message::StreamRead(StreamRead {
            stream: 39,
            offset: 17,
            len,
        })
    };
    assert!(matches!(
        requests.reserve(instance, 4, &read(VOSU_STREAM_CHUNK_MAX_BYTES + 1)),
        Err(ResourceRequestError::InvalidRequest)
    ));
    let (id, mut receiver) = requests.reserve(instance, 4, &read(2)).unwrap();
    let chunk = ResourceReply::Chunk(StreamChunk::Data {
        data: vosu_plugin_api::CborBytes(vec![3, 5, 7]),
    });
    assert_eq!(
        requests.complete(instance, 4, id, chunk),
        Err(ResourceRequestError::InvalidReply)
    );
    assert_eq!(
        receiver.try_recv().unwrap(),
        Err(ResourceRequestError::InvalidReply)
    );
    let (id, mut receiver) = requests.reserve(instance, 4, &read(2)).unwrap();
    let eof = ResourceReply::Chunk(StreamChunk::Data {
        data: vosu_plugin_api::CborBytes(vec![]),
    });
    requests.complete(instance, 4, id, eof.clone()).unwrap();
    assert_eq!(receiver.try_recv().unwrap(), Ok(eof));
}
