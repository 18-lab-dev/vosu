use std::sync::{
    Arc, Barrier, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::watch;

use super::*;

#[test]
fn pointer_syntax_and_boundaries_follow_rfc6901() {
    assert_eq!(decode_pointer(""), Some(vec![]));
    assert_eq!(decode_pointer("/"), Some(vec![String::new()]));
    assert_eq!(
        decode_pointer("/日本語/~0/~1"),
        Some(vec!["日本語".into(), "~".into(), "/".into()])
    );
    assert!(decode_pointer("x").is_none());
    assert!(decode_pointer("/~").is_none());
    assert!(decode_pointer("/~2").is_none());

    assert!(compile_pointers(vec!["/a".into()]).is_ok());
    assert!(compile_pointers((0..256).map(|id| format!("/{id}")).collect()).is_ok());
    assert_eq!(
        compile_pointers(Vec::new()),
        Err(SubscriptionRequestError::InvalidRequest)
    );
    assert_eq!(
        compile_pointers((0..257).map(|id| format!("/{id}")).collect()),
        Err(SubscriptionRequestError::InvalidRequest)
    );
    assert!(compile_pointers(vec![format!("/{}", "a".repeat(511))]).is_ok());
    assert_eq!(
        compile_pointers(vec![format!("/{}", "a".repeat(512))]),
        Err(SubscriptionRequestError::InvalidRequest)
    );
    assert!(compile_pointers(vec![format!("/{}", vec!["a"; 32].join("/"))]).is_ok());
    assert_eq!(
        compile_pointers(vec![format!("/{}", vec!["a"; 33].join("/"))]),
        Err(SubscriptionRequestError::InvalidRequest)
    );
}

#[test]
fn decoded_duplicates_and_ancestor_overlaps_are_rejected() {
    for pointers in [
        vec!["/a".into(), "/a".into()],
        vec!["/a~1b".into(), "/a~1b/c".into()],
        vec!["".into(), "/a".into()],
        vec!["/a/b".into(), "/a".into()],
    ] {
        assert_eq!(
            compile_pointers(pointers),
            Err(SubscriptionRequestError::InvalidPointer)
        );
    }
}

#[test]
fn traversal_distinguishes_object_keys_array_indices_null_and_absence() {
    let document = json!({"": 1, "0": "object", "array": [null, "one"], "01": 4, "-": 5});
    assert_eq!(traverse(&document, &[String::new()]), Some(&json!(1)));
    assert_eq!(traverse(&document, &["0".into()]), Some(&json!("object")));
    assert_eq!(traverse(&document, &["01".into()]), Some(&json!(4)));
    assert_eq!(traverse(&document, &["-".into()]), Some(&json!(5)));
    assert_eq!(
        traverse(&document, &["array".into(), "0".into()]),
        Some(&Value::Null)
    );
    assert_eq!(
        traverse(&document, &["array".into(), "1".into()]),
        Some(&json!("one"))
    );
    assert_eq!(traverse(&document, &["array".into(), "01".into()]), None);
    assert_eq!(traverse(&document, &["array".into(), "-".into()]), None);
    assert_eq!(parse_array_index("184467440737095516160000"), None);
}

#[test]
fn object_lookup_agrees_below_and_above_the_linear_search_size() {
    for size in 0..=LINEAR_KEY_SEARCH_MAX + 4 {
        let mut object = serde_json::Map::new();
        for id in 0..size {
            // Same-length keys and a shared prefix make every miss reach the byte comparison.
            let key = if id == 0 {
                String::new()
            } else {
                format!("k{id:02}")
            };
            object.insert(key, json!({"id": id}));
        }
        let document = Value::Object(object);
        for id in 0..size {
            let key = if id == 0 {
                String::new()
            } else {
                format!("k{id:02}")
            };
            assert_eq!(
                traverse(&document, &[key.clone(), "id".into()]),
                Some(&json!(id)),
                "size {size} key {key:?}"
            );
        }
        for missing in ["k99", "k0", "K01", "k01 ", "id"] {
            assert_eq!(
                traverse(&document, &[missing.into()]),
                None,
                "size {size} {missing:?}"
            );
        }
    }
}

#[test]
fn request_shape_and_frozen_compact_frames_are_exact() {
    assert_eq!(
        parse_subscription_request(br#"["/a","/b"]"#),
        Ok(vec!["/a".into(), "/b".into()])
    );
    assert_eq!(
        parse_subscription_request(br#"{"a":1}"#),
        Err(SubscriptionRequestError::InvalidRequest)
    );
    assert_eq!(
        parse_subscription_request(br#"[1]"#),
        Err(SubscriptionRequestError::InvalidRequest)
    );

    let initial = initial_frame(7, &[Some(json!(null)), None, Some(json!(-0.0))]).unwrap();
    assert_eq!(initial.as_ref(), br#"[0,7,[[0,null],[2,0]]]"#);
    let numeric = initial_frame(
        8,
        &[
            Some(json!(1e20)),
            Some(json!(1e21)),
            Some(json!(1e-6)),
            Some(json!(1e-7)),
            Some(json!(f64::INFINITY)),
        ],
    )
    .unwrap();
    assert_eq!(
        numeric.as_ref(),
        br#"[0,8,[[0,100000000000000000000],[1,1e+21],[2,0.000001],[3,1e-7],[4,null]]]"#
    );
    let update = update_frame(
        9,
        &[Some(json!(1)), Some(json!(2)), None],
        &[Some(json!(3)), None, Some(json!(null))],
    )
    .unwrap();
    assert_eq!(update.as_ref(), br#"[1,9,[[0,3],[2,null]],[1]]"#);
    assert_eq!(
        initial_frame(0, &[None, None]).unwrap().as_ref(),
        br#"[0,0,[]]"#
    );
    assert_eq!(
        update_frame(10, &[Some(json!(1))], &[None])
            .unwrap()
            .as_ref(),
        br#"[1,10,[],[0]]"#
    );
    assert_eq!(
        update_frame(11, &[Some(json!(1)), None], &[Some(json!(1)), None])
            .unwrap()
            .as_ref(),
        br#"[1,11,[],[]]"#
    );
    assert_eq!(
        update_frame(
            12,
            &[Some(json!(1)), None, Some(json!(3))],
            &[None, None, None],
        )
        .unwrap()
        .as_ref(),
        br#"[1,12,[],[0,2]]"#
    );
    assert_eq!(
        error_frame(INVALID_REQUEST, INVALID_REQUEST_MESSAGE).as_ref(),
        br#"[2,1,"invalid subscription request"]"#
    );
    assert_eq!(
        error_frame(INVALID_POINTER, INVALID_POINTER_MESSAGE).as_ref(),
        br#"[2,2,"invalid JSON Pointer"]"#
    );
    assert_eq!(
        error_frame(SUBSCRIPTION_TIMEOUT, SUBSCRIPTION_TIMEOUT_MESSAGE).as_ref(),
        br#"[2,3,"subscription timed out"]"#
    );
    assert_eq!(
        error_frame(SERIALIZATION_FAILED, SERIALIZATION_FAILED_MESSAGE).as_ref(),
        br#"[2,4,"state serialization failed"]"#
    );
    assert_eq!(
        error_frame(ALREADY_SUBSCRIBED, ALREADY_SUBSCRIBED_MESSAGE).as_ref(),
        br#"[2,5,"subscription already established"]"#
    );
}

#[test]
fn arena_frames_match_api_encoding_across_chunk_turnover_and_oversized_frames() {
    let values = [
        json!(null),
        json!(true),
        json!(-0.0),
        json!(0.1 + 0.2),
        json!(1e21),
        json!(1e-7),
        json!(f64::NEG_INFINITY),
        json!(u64::MAX),
        json!(i64::MIN),
        json!("\"q\\b\u{1}\u{7f}\u{2028}日本語🎵"),
        json!({"z": [1.5, {"a": null}], "a": []}),
    ];
    let oversized = json!("x".repeat(3 * FRAME_ARENA_CAPACITY));
    let pairs = |state: &[Option<Value>]| -> Vec<(u16, Value)> {
        state
            .iter()
            .enumerate()
            .filter_map(|(id, value)| value.clone().map(|value| (id as u16, value)))
            .collect()
    };

    let mut arena = FrameArena::default();
    let mut frames = Vec::new();
    let mut previous: Vec<Option<Value>> = Vec::new();
    for revision in 0..800_u64 {
        let len = (revision / 2) as usize % values.len() + 1;
        let current: Vec<Option<Value>> = (0..len)
            .map(|id| {
                if revision == 400 && id == 0 {
                    Some(oversized.clone())
                } else {
                    (!(id as u64 + revision).is_multiple_of(3))
                        .then(|| values[(id + revision as usize) % values.len()].clone())
                }
            })
            .collect();
        let (frame, expected) = if revision.is_multiple_of(2) || previous.len() != len {
            (
                arena.initial(revision, &current).unwrap(),
                crate::api::v2::encode_dto(&(0_u8, revision, pairs(&current))).unwrap(),
            )
        } else {
            let changed: Vec<(u16, Value)> = pairs(&current)
                .into_iter()
                .filter(|(id, value)| previous[*id as usize].as_ref() != Some(value))
                .collect();
            let removed: Vec<u16> = (0..len)
                .filter(|&id| previous[id].is_some() && current[id].is_none())
                .map(|id| id as u16)
                .collect();
            (
                arena
                    .update(revision, &current, &{
                        let mut ids = Vec::new();
                        changes(&previous, &current, &mut ids);
                        ids
                    })
                    .unwrap(),
                crate::api::v2::encode_dto(&(1_u8, revision, changed, removed)).unwrap(),
            )
        };
        frames.push((frame, expected));
        previous = current;
    }
    // Compare only after every frame is encoded: later writes and chunk turnover must not
    // disturb frames already handed out.
    for (frame, expected) in &frames {
        assert_eq!(frame, expected);
    }
}

#[test]
fn pong_queue_backpressure_is_nonblocking_and_bounded() {
    let provider = FakeProvider::new(1, None);
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let (connection, mut receiver) = hub.connect();

    assert!(connection.send_pong(Bytes::from_static(b"first")));
    assert!(!connection.send_pong(Bytes::from_static(b"second")));
    assert_eq!(
        receiver.try_recv(),
        Some(V2SubscriptionOutbound::Pong(Bytes::from_static(b"first")))
    );
}

#[tokio::test]
async fn mailbox_wakes_a_waiting_receiver_and_drains_before_end_of_stream() {
    let text = |bytes: &'static [u8]| V2SubscriptionOutbound::Text(Bytes::from_static(bytes));
    let provider = FakeProvider::new(1, None);
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let (connection, receiver) = hub.connect();
    let mailbox = Arc::clone(&connection.client.mailbox);

    let waiting = tokio::spawn(async move {
        let mut receiver = receiver;
        let frame = receiver.recv().await;
        (frame, receiver)
    });
    tokio::task::yield_now().await;
    mailbox.try_send_data(text(b"first")).unwrap();
    let (frame, mut receiver) = waiting.await.unwrap();
    assert_eq!(frame, Some(text(b"first")));

    mailbox.try_send_data(text(b"second")).unwrap();
    assert_eq!(mailbox.try_send_data(text(b"third")), Err(text(b"third")));

    // Dropping the connection drops the client and with it every sender; the frame still
    // queued is delivered before the end of the stream.
    drop(connection);
    drop(hub);
    assert_eq!(receiver.recv().await, Some(text(b"second")));
    assert_eq!(receiver.recv().await, None);
}

struct FakeSnapshot {
    revision: u64,
    document: Result<Option<Value>, ApiError>,
    builds: Arc<AtomicUsize>,
}

impl V2SubscriptionSnapshot for FakeSnapshot {
    fn revision(&self) -> u64 {
        self.revision
    }
    fn document(&self) -> Result<Option<Value>, ApiError> {
        self.builds.fetch_add(1, Ordering::Relaxed);
        self.document.clone()
    }
}

struct FakeProvider {
    tx: watch::Sender<u64>,
    state: Mutex<(u64, Result<Option<Value>, ApiError>)>,
    captures: AtomicUsize,
    builds: Arc<AtomicUsize>,
}

impl FakeProvider {
    fn new(revision: u64, document: Option<Value>) -> Arc<Self> {
        let (tx, _) = watch::channel(revision);
        Arc::new(Self {
            tx,
            state: Mutex::new((revision, Ok(document))),
            captures: AtomicUsize::new(0),
            builds: Arc::new(AtomicUsize::new(0)),
        })
    }

    fn publish(&self, revision: u64, document: Option<Value>) {
        *self.state.lock().unwrap() = (revision, Ok(document));
        self.tx.send_replace(revision);
    }

    fn fail(&self, revision: u64) {
        *self.state.lock().unwrap() = (revision, Err(ApiError::serialization("fake")));
        self.tx.send_replace(revision);
    }
}

impl V2SubscriptionProvider for FakeProvider {
    fn subscribe(&self) -> watch::Receiver<u64> {
        self.tx.subscribe()
    }
    fn capture(&self) -> Arc<dyn V2SubscriptionSnapshot> {
        self.captures.fetch_add(1, Ordering::Relaxed);
        let (revision, document) = self.state.lock().unwrap().clone();
        Arc::new(FakeSnapshot {
            revision,
            document,
            builds: Arc::clone(&self.builds),
        })
    }
}

struct BlockingSnapshot {
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
}

impl V2SubscriptionSnapshot for BlockingSnapshot {
    fn revision(&self) -> u64 {
        1
    }

    fn document(&self) -> Result<Option<Value>, ApiError> {
        self.entered.wait();
        self.release.wait();
        Ok(Some(json!({"a": 1})))
    }
}

struct BlockingProvider {
    tx: watch::Sender<u64>,
    entered: Arc<Barrier>,
    release: Arc<Barrier>,
}

impl BlockingProvider {
    fn new() -> Arc<Self> {
        let (tx, _) = watch::channel(1);
        Arc::new(Self {
            tx,
            entered: Arc::new(Barrier::new(2)),
            release: Arc::new(Barrier::new(2)),
        })
    }
}

impl V2SubscriptionProvider for BlockingProvider {
    fn subscribe(&self) -> watch::Receiver<u64> {
        self.tx.subscribe()
    }

    fn capture(&self) -> Arc<dyn V2SubscriptionSnapshot> {
        Arc::new(BlockingSnapshot {
            entered: Arc::clone(&self.entered),
            release: Arc::clone(&self.release),
        })
    }
}

fn text(frame: V2SubscriptionOutbound) -> Value {
    let V2SubscriptionOutbound::Text(bytes) = frame else {
        panic!("expected text frame")
    };
    serde_json::from_slice(&bytes).unwrap()
}

fn client_plan(client: &Client) -> Option<Arc<Plan>> {
    lock(&client.subscription)
        .as_ref()
        .map(|subscription| Arc::clone(&subscription.plan))
}

fn client_baseline(client: &Client) -> Option<Arc<[Option<Value>]>> {
    lock(&client.subscription)
        .as_ref()
        .and_then(|subscription| subscription.baseline.clone())
}

#[tokio::test]
async fn initial_absent_to_present_null_and_focus_loss_are_delivered() {
    let provider = FakeProvider::new(1, None);
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    assert_eq!(text(receiver.recv().await.unwrap()), json!([0, 1, []]));
    connection.write_completed();

    provider.publish(2, Some(json!({"a": null})));
    assert_eq!(
        text(receiver.recv().await.unwrap()),
        json!([1, 2, [[0, null]], []])
    );
    connection.write_completed();
    provider.publish(3, None);
    assert_eq!(text(receiver.recv().await.unwrap()), json!([1, 3, [], [0]]));
    hub.shutdown();
    runner.await.unwrap();
}

#[test]
fn unshared_baseline_tracks_partial_changes_removals_and_reappearance() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1, "b": "x", "c": [1]})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let (connection, mut receiver) = hub.connect();
    connection
        .subscribe(vec!["/a".into(), "/b".into(), "/c".into()])
        .unwrap();
    let baseline = |connection: &V2SubscriptionConnection| {
        client_baseline(&connection.client).map(|values| values.to_vec())
    };

    hub.dispatch(1);
    assert_eq!(
        text(receiver.try_recv().unwrap()),
        json!([0, 1, [[0, 1], [1, "x"], [2, [1]]]])
    );
    connection.write_completed();

    let steps = [
        (
            json!({"a": 2, "b": "x", "c": [1]}),
            Some(json!([1, 2, [[0, 2]], []])),
        ),
        (
            json!({"a": 2, "c": [1, 2]}),
            Some(json!([1, 3, [[2, [1, 2]]], [1]])),
        ),
        (
            json!({"a": 2, "b": "y", "c": [1, 2]}),
            Some(json!([1, 4, [[1, "y"]], []])),
        ),
        (json!({"a": 2, "b": "y", "c": [1, 2], "d": 0}), None),
    ];
    for (revision, (document, expected)) in (2..).zip(steps) {
        let selected = ["a", "b", "c"].map(|key| document.get(key).cloned());
        provider.publish(revision, Some(document));
        hub.dispatch(revision);
        assert_eq!(receiver.try_recv().map(text), expected);
        assert_eq!(baseline(&connection).as_deref(), Some(selected.as_slice()));
        assert_eq!(
            connection.client.checked_revision.load(Ordering::Acquire),
            revision
        );
        connection.write_completed();
    }
}

#[test]
fn baseline_survives_a_plan_becoming_shared_and_unshared_again() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1, "b": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let (first, mut first_rx) = hub.connect();
    first.subscribe(vec!["/a".into(), "/b".into()]).unwrap();
    hub.dispatch(1);
    assert_eq!(
        text(first_rx.try_recv().unwrap()),
        json!([0, 1, [[0, 1], [1, 1]]])
    );
    first.write_completed();

    // An equal selection makes the plan shared: the first client's update is still computed
    // against the values it last received while its plan was unshared.
    let (second, mut second_rx) = hub.connect();
    second.subscribe(vec!["/a".into(), "/b".into()]).unwrap();
    provider.publish(2, Some(json!({"a": 2, "b": 1})));
    hub.dispatch(2);
    assert_eq!(
        text(first_rx.try_recv().unwrap()),
        json!([1, 2, [[0, 2]], []])
    );
    assert_eq!(
        text(second_rx.try_recv().unwrap()),
        json!([0, 2, [[0, 2], [1, 1]]])
    );
    first.write_completed();
    second.write_completed();

    // Unshared again: the first client now solely holds the extraction it shared, and its
    // updates are computed against, and recorded into, those values.
    second.close(1000);
    drop(second_rx);
    provider.publish(3, Some(json!({"a": 2, "b": 3})));
    hub.dispatch(3);
    assert_eq!(
        text(first_rx.try_recv().unwrap()),
        json!([1, 3, [[1, 3]], []])
    );
    first.write_completed();
    provider.publish(4, Some(json!({"a": 4})));
    hub.dispatch(4);
    assert_eq!(
        text(first_rx.try_recv().unwrap()),
        json!([1, 4, [[0, 4]], [1]])
    );
    assert_eq!(
        client_baseline(&first.client).as_deref(),
        Some([Some(json!(4)), None].as_slice())
    );
}

#[tokio::test]
async fn demand_tracks_established_selections_only() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let mut demand = hub.demand();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });

    let (connection, _receiver) = hub.connect();
    tokio::task::yield_now().await;
    assert!(
        !*demand.borrow(),
        "a connection without a selection is not demand"
    );

    connection.subscribe(vec!["/a".into()]).unwrap();
    demand.wait_for(|active| *active).await.unwrap();

    connection.close(1000);
    demand.wait_for(|active| !*active).await.unwrap();

    let (second, _second_receiver) = hub.connect();
    second.subscribe(vec!["/a".into()]).unwrap();
    demand.wait_for(|active| *active).await.unwrap();
    hub.shutdown();
    runner.await.unwrap();
    assert!(!*demand.borrow(), "a stopped hub releases demand");
}

#[tokio::test]
async fn pending_client_coalesces_directly_from_queued_a_to_latest_c() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    assert_eq!(
        text(receiver.recv().await.unwrap()),
        json!([0, 1, [[0, 1]]])
    );
    provider.publish(2, Some(json!({"a": 2})));
    provider.publish(3, Some(json!({"a": 3})));
    tokio::task::yield_now().await;
    assert!(receiver.try_recv().is_none());
    connection.write_completed();
    assert_eq!(
        text(receiver.recv().await.unwrap()),
        json!([1, 3, [[0, 3]], []])
    );
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn debounce_is_per_client_reloadable_fractional_and_coalesces_bursts() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::from_micros(1500));
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    let _ = receiver.recv().await.unwrap();
    connection.write_completed();
    provider.publish(2, Some(json!({"a": 2})));
    provider.publish(3, Some(json!({"a": 3})));
    tokio::time::advance(Duration::from_micros(1499)).await;
    tokio::task::yield_now().await;
    assert!(receiver.try_recv().is_none());
    hub.set_debounce(Duration::from_micros(1000));
    assert_eq!(
        text(receiver.recv().await.unwrap()),
        json!([1, 3, [[0, 3]], []])
    );
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn identical_selections_share_one_build_and_emit_no_updates() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1, "other": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let (first, mut first_rx) = hub.connect();
    let (second, mut second_rx) = hub.connect();
    first.subscribe(vec!["/a".into()]).unwrap();
    second.subscribe(vec!["/a".into()]).unwrap();
    assert_eq!(first_rx.recv().await, second_rx.recv().await);
    assert_eq!(provider.builds.load(Ordering::Relaxed), 1);
    first.write_completed();
    second.write_completed();
    provider.publish(2, Some(json!({"a": 1, "other": 2})));
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert!(first_rx.try_recv().is_none());
    assert!(second_rx.try_recv().is_none());
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn terminal_errors_are_ordered_and_pending_clients_are_closed() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let (connection, mut receiver) = hub.connect();
    connection.invalid_request();
    assert_eq!(
        receiver.recv().await,
        Some(V2SubscriptionOutbound::Error(Bytes::from_static(
            br#"[2,1,"invalid subscription request"]"#
        )))
    );
    assert_eq!(
        receiver.recv().await,
        Some(V2SubscriptionOutbound::Close { code: 1008 })
    );

    let (_pending, mut pending_rx) = hub.connect();
    hub.close_all(1001);
    assert_eq!(
        pending_rx.recv().await,
        Some(V2SubscriptionOutbound::Close { code: 1001 })
    );
}

#[tokio::test]
async fn serialization_failure_terminates_active_subscriber_with_1011() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    let _ = receiver.recv().await;
    connection.write_completed();
    provider.fail(2);
    assert_eq!(
        receiver.recv().await,
        Some(V2SubscriptionOutbound::Error(Bytes::from_static(
            br#"[2,4,"state serialization failed"]"#
        )))
    );
    assert_eq!(
        receiver.recv().await,
        Some(V2SubscriptionOutbound::Close { code: 1011 })
    );
    hub.shutdown();
    runner.await.unwrap();
    assert_eq!(hub.client_count(), 0);
    assert!(lock(&hub.0.document).is_none());
    let plans = lock(&hub.0.plans);
    assert!(plans.is_empty());
}

#[test]
fn close_during_document_build_cannot_restore_retained_state() {
    for close_all in [false, true] {
        let provider = BlockingProvider::new();
        let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
        let (connection, _receiver) = hub.connect();
        connection.subscribe(vec!["/a".into()]).unwrap();

        let dispatch_hub = hub.clone();
        let worker = std::thread::spawn(move || dispatch_hub.dispatch(1));
        provider.entered.wait();
        if close_all {
            hub.close_all(1001);
        } else {
            connection.cleanup();
        }
        provider.release.wait();
        worker.join().unwrap();

        assert_eq!(hub.client_count(), 0);
        assert!(lock(&hub.0.document).is_none());
        let plans = lock(&hub.0.plans);
        assert!(plans.is_empty());
    }
}

#[test]
fn close_after_pending_reservation_cannot_restore_delivery_state() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let (connection, _receiver) = hub.connect();
    connection.subscribe(vec!["".into()]).unwrap();
    let client = Arc::clone(&connection.client);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    *lock(&hub.0.pending_gate) = Some((Arc::clone(&entered), Arc::clone(&release)));

    let dispatch_hub = hub.clone();
    let worker = std::thread::spawn(move || dispatch_hub.dispatch(1));
    entered.wait();
    let close_started = Arc::new(Barrier::new(2));
    let closer = std::thread::spawn({
        let close_started = Arc::clone(&close_started);
        move || {
            close_started.wait();
            connection.cleanup();
        }
    });
    close_started.wait();
    release.wait();
    worker.join().unwrap();
    closer.join().unwrap();

    // Plan and baseline are released together with the subscription.
    assert!(lock(&client.subscription).is_none());
    assert!(lock(&hub.0.document).is_none());
    let plans = lock(&hub.0.plans);
    assert!(plans.is_empty());
}

#[test]
fn closed_connection_rejects_late_subscription_without_retaining_plan() {
    let provider = FakeProvider::new(1, None);
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let (connection, _receiver) = hub.connect();

    connection.close(1001);
    assert_eq!(
        connection.subscribe(vec!["/a".into()]),
        Err(SubscriptionRequestError::AlreadySubscribed)
    );
    assert!(lock(&connection.client.subscription).is_none());
    let plans = lock(&hub.0.plans);
    assert!(plans.is_empty());
}

#[test]
fn identical_active_plans_are_weakly_interned_and_released() {
    let provider = FakeProvider::new(1, None);
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let (first, _first_rx) = hub.connect();
    let (second, _second_rx) = hub.connect();
    first.subscribe(vec!["/a".into(), "/b".into()]).unwrap();
    second.subscribe(vec!["/a".into(), "/b".into()]).unwrap();

    let first_plan = client_plan(&first.client).unwrap();
    let second_plan = client_plan(&second.client).unwrap();
    assert!(Arc::ptr_eq(&first_plan, &second_plan));
    let released = Arc::downgrade(&first_plan);
    drop(first_plan);
    drop(second_plan);
    drop(first);
    drop(second);
    assert!(released.upgrade().is_none());
    {
        let plans = lock(&hub.0.plans);
        assert!(plans.is_empty());
    }
    assert!(lock(&hub.0.document).is_none());

    let (third, _third_rx) = hub.connect();
    third.subscribe(vec!["/different".into()]).unwrap();
    let plans = lock(&hub.0.plans);
    assert_eq!(plans.len(), 1);
    assert!(plans[0].upgrade().is_some());
}

#[test]
fn failed_cas_and_full_queue_do_not_advance_baseline() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();

    connection.client.state.store(PENDING, Ordering::Release);
    hub.dispatch(1);
    assert!(client_baseline(&connection.client).is_none());

    connection.client.state.store(OPEN, Ordering::Release);
    connection
        .client
        .mailbox
        .try_send_data(V2SubscriptionOutbound::Text(Bytes::from_static(
            b"occupied",
        )))
        .unwrap();
    hub.dispatch(1);
    assert!(client_baseline(&connection.client).is_none());
    assert_eq!(
        receiver.try_recv(),
        Some(V2SubscriptionOutbound::Text(Bytes::from_static(
            b"occupied"
        )))
    );

    let (closed_connection, closed_receiver) = hub.connect();
    closed_connection.subscribe(vec!["/a".into()]).unwrap();
    drop(closed_receiver);
    hub.dispatch(1);
    assert!(client_baseline(&closed_connection.client).is_none());
    assert_eq!(closed_connection.client.state.load(Ordering::Acquire), OPEN);

    hub.dispatch(1);
    assert_eq!(text(receiver.try_recv().unwrap()), json!([0, 1, [[0, 1]]]));
    assert_eq!(
        client_baseline(&connection.client).as_deref(),
        Some([Some(json!(1))].as_slice())
    );
}

#[test]
fn unchecked_sentinel_revision_still_delivers_the_initial_frame() {
    let provider = FakeProvider::new(UNCHECKED, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();

    hub.dispatch(UNCHECKED);
    assert_eq!(
        text(receiver.try_recv().unwrap()),
        json!([0, UNCHECKED, [[0, 1]]])
    );
    connection.write_completed();
    hub.dispatch(UNCHECKED);
    assert!(receiver.try_recv().is_none());
}

#[tokio::test]
async fn document_failure_terminates_every_active_plan_including_pending_clients() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1, "b": 2})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let (pending, mut pending_rx) = hub.connect();
    let (open, mut open_rx) = hub.connect();
    pending.subscribe(vec!["/a".into()]).unwrap();
    open.subscribe(vec!["/b".into()]).unwrap();
    let _ = pending_rx.recv().await.unwrap();
    let _ = open_rx.recv().await.unwrap();
    open.write_completed();

    provider.fail(2);
    for receiver in [&mut pending_rx, &mut open_rx] {
        assert_eq!(
            receiver.recv().await,
            Some(V2SubscriptionOutbound::Error(Bytes::from_static(
                br#"[2,4,"state serialization failed"]"#
            )))
        );
        assert_eq!(
            receiver.recv().await,
            Some(V2SubscriptionOutbound::Close { code: 1011 })
        );
    }
    assert_eq!(hub.client_count(), 0);
    hub.shutdown();
    runner.await.unwrap();
}

#[test]
fn outer_encoding_failure_only_removes_the_failed_plan_group() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1, "b": 2})));
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let (failed, mut failed_rx) = hub.connect();
    let (_healthy, mut healthy_rx) = hub.connect();
    failed.subscribe(vec!["/a".into()]).unwrap();
    _healthy.subscribe(vec!["/b".into()]).unwrap();
    *lock(&hub.0.failed_encoding_plan) = client_plan(&failed.client);

    hub.dispatch(1);
    assert_eq!(
        failed_rx.try_recv(),
        Some(V2SubscriptionOutbound::Error(Bytes::from_static(
            br#"[2,4,"state serialization failed"]"#
        )))
    );
    assert_eq!(
        failed_rx.try_recv(),
        Some(V2SubscriptionOutbound::Close { code: 1011 })
    );
    assert_eq!(
        text(healthy_rx.try_recv().unwrap()),
        json!([0, 1, [[0, 2]]])
    );
    assert_eq!(hub.client_count(), 1);
}

#[tokio::test]
async fn shared_eligible_group_builds_extracts_and_encodes_once() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1, "other": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let (first, mut first_rx) = hub.connect();
    let (second, mut second_rx) = hub.connect();
    first.subscribe(vec!["/a".into()]).unwrap();
    second.subscribe(vec!["/a".into()]).unwrap();
    assert_eq!(first_rx.recv().await, second_rx.recv().await);
    assert_eq!(provider.captures.load(Ordering::Relaxed), 1);
    assert_eq!(provider.builds.load(Ordering::Relaxed), 1);
    assert_eq!(hub.0.extractions.load(Ordering::Relaxed), 1);
    assert_eq!(hub.0.encodings.load(Ordering::Relaxed), 1);

    provider.publish(2, Some(json!({"a": 2, "other": 2})));
    tokio::task::yield_now().await;
    assert_eq!(provider.captures.load(Ordering::Relaxed), 1);
    first.write_completed();
    second.write_completed();
    assert_eq!(first_rx.recv().await, second_rx.recv().await);
    assert_eq!(provider.captures.load(Ordering::Relaxed), 2);
    assert_eq!(provider.builds.load(Ordering::Relaxed), 2);
    assert_eq!(hub.0.extractions.load(Ordering::Relaxed), 2);
    assert_eq!(hub.0.encodings.load(Ordering::Relaxed), 2);

    first.write_completed();
    second.write_completed();
    provider.publish(3, Some(json!({"a": 2, "other": 3})));
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert!(first_rx.try_recv().is_none());
    assert!(second_rx.try_recv().is_none());
    assert_eq!(provider.builds.load(Ordering::Relaxed), 3);
    assert_eq!(hub.0.extractions.load(Ordering::Relaxed), 3);
    assert_eq!(hub.0.encodings.load(Ordering::Relaxed), 2);
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn clients_use_independent_debounce_start_instants() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::from_millis(2));
    hub.dispatch(1);
    assert_eq!(provider.captures.load(Ordering::Relaxed), 0);
    assert_eq!(provider.builds.load(Ordering::Relaxed), 0);
    assert_eq!(hub.0.extractions.load(Ordering::Relaxed), 0);
    assert_eq!(hub.0.encodings.load(Ordering::Relaxed), 0);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let (first, mut first_rx) = hub.connect();
    first.subscribe(vec!["/a".into()]).unwrap();
    let _ = first_rx.recv().await.unwrap();
    first.write_completed();

    tokio::time::advance(Duration::from_millis(1)).await;
    let (second, mut second_rx) = hub.connect();
    second.subscribe(vec!["/a".into()]).unwrap();
    let _ = second_rx.recv().await.unwrap();
    second.write_completed();
    provider.publish(2, Some(json!({"a": 2})));

    tokio::time::advance(Duration::from_micros(999)).await;
    tokio::task::yield_now().await;
    assert!(first_rx.try_recv().is_none());
    assert!(second_rx.try_recv().is_none());
    tokio::time::advance(Duration::from_micros(1)).await;
    assert_eq!(
        text(first_rx.recv().await.unwrap()),
        json!([1, 2, [[0, 2]], []])
    );
    assert!(second_rx.try_recv().is_none());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(
        text(second_rx.recv().await.unwrap()),
        json!([1, 2, [[0, 2]], []])
    );
    assert_eq!(provider.builds.load(Ordering::Relaxed), 2);
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn debounce_increase_recomputes_from_the_original_queue_instant() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::from_millis(1));
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    let _ = receiver.recv().await.unwrap();
    connection.write_completed();
    provider.publish(2, Some(json!({"a": 2})));
    hub.set_debounce(Duration::from_millis(2));

    tokio::time::advance(Duration::from_micros(1999)).await;
    tokio::task::yield_now().await;
    assert!(receiver.try_recv().is_none());
    tokio::time::advance(Duration::from_micros(1)).await;
    assert_eq!(
        text(receiver.recv().await.unwrap()),
        json!([1, 2, [[0, 2]], []])
    );
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn zero_to_nonzero_debounce_uses_the_original_queue_instant() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    let _ = receiver.recv().await.unwrap();
    connection.write_completed();
    provider.publish(2, Some(json!({"a": 2})));
    hub.set_debounce(Duration::from_millis(2));

    tokio::time::advance(Duration::from_micros(1999)).await;
    tokio::task::yield_now().await;
    assert!(receiver.try_recv().is_none());
    tokio::time::advance(Duration::from_micros(1)).await;
    assert_eq!(
        text(receiver.recv().await.unwrap()),
        json!([1, 2, [[0, 2]], []])
    );
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn zero_debounce_stays_event_driven_without_rebuilding_or_spinning() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    let _ = receiver.recv().await.unwrap();
    connection.write_completed();
    let captures = provider.captures.load(Ordering::Relaxed);
    let builds = provider.builds.load(Ordering::Relaxed);
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
    assert_eq!(provider.captures.load(Ordering::Relaxed), captures);
    assert_eq!(provider.builds.load(Ordering::Relaxed), builds);
    assert!(receiver.try_recv().is_none());
    hub.shutdown();
    runner.await.unwrap();
}

#[test]
fn shared_plan_preserves_unchanged_stale_and_initial_delivery_outcomes() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let (current, mut current_rx) = hub.connect();
    let (stale, mut stale_rx) = hub.connect();
    current.subscribe(vec!["/a".into()]).unwrap();
    stale.subscribe(vec!["/a".into()]).unwrap();
    hub.dispatch(1);
    assert_eq!(
        text(current_rx.try_recv().unwrap()),
        json!([0, 1, [[0, 1]]])
    );
    assert_eq!(text(stale_rx.try_recv().unwrap()), json!([0, 1, [[0, 1]]]));

    current.write_completed();
    provider.publish(2, Some(json!({"a": 2})));
    hub.dispatch(2);
    assert_eq!(
        text(current_rx.try_recv().unwrap()),
        json!([1, 2, [[0, 2]], []])
    );
    assert!(stale_rx.try_recv().is_none());
    current.write_completed();
    stale.write_completed();

    let (initial, mut initial_rx) = hub.connect();
    initial.subscribe(vec!["/a".into()]).unwrap();
    provider.publish(3, Some(json!({"a": 2})));
    hub.dispatch(3);
    assert!(current_rx.try_recv().is_none());
    assert_eq!(
        text(stale_rx.try_recv().unwrap()),
        json!([1, 3, [[0, 2]], []])
    );
    assert_eq!(
        text(initial_rx.try_recv().unwrap()),
        json!([0, 3, [[0, 2]]])
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn subscription_during_dispatch_is_delivered_without_another_publication() {
    let provider = BlockingProvider::new();
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let (first, _first_rx) = hub.connect();
    first.subscribe(vec!["/a".into()]).unwrap();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    provider.entered.wait();
    let (second, mut second_rx) = hub.connect();
    second.subscribe(vec!["/a".into()]).unwrap();
    provider.release.wait();

    let delivered = tokio::time::timeout(Duration::from_secs(2), second_rx.recv()).await;
    hub.shutdown();
    runner.await.unwrap();
    assert_eq!(text(delivered.unwrap().unwrap()), json!([0, 1, [[0, 1]]]));
}
