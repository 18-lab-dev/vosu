use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use vosu_proptest::bounded;
pub(super) use vosu_proptest::wait::Barrier;

use serde_json::{Value, json};
use tokio::sync::watch;

use super::*;

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
        let frame = bounded!(receiver.recv());
        (frame, receiver)
    });
    tokio::task::yield_now().await;
    mailbox.try_send_data(text(b"first")).unwrap();
    let (frame, mut receiver) = bounded!(waiting).unwrap();
    assert_eq!(frame, Some(text(b"first")));

    mailbox.try_send_data(text(b"second")).unwrap();
    assert_eq!(mailbox.try_send_data(text(b"third")), Err(text(b"third")));

    // Dropping the connection drops the client and with it every sender; the frame still
    // queued is delivered before the end of the stream.
    drop(connection);
    drop(hub);
    assert_eq!(bounded!(receiver.recv()), Some(text(b"second")));
    assert_eq!(bounded!(receiver.recv()), None);
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
    fn document(&self, _pointers: &[&[String]]) -> Result<Option<Value>, ApiError> {
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

    fn document(&self, _pointers: &[&[String]]) -> Result<Option<Value>, ApiError> {
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
        async move { bounded!(hub.run()) }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    assert_eq!(text(bounded!(receiver.recv()).unwrap()), json!([0, 1, []]));
    connection.write_completed();

    provider.publish(2, Some(json!({"a": null})));
    assert_eq!(
        text(bounded!(receiver.recv()).unwrap()),
        json!([1, 2, [[0, null]], []])
    );
    connection.write_completed();
    provider.publish(3, None);
    assert_eq!(
        text(bounded!(receiver.recv()).unwrap()),
        json!([1, 3, [], [0]])
    );
    hub.shutdown();
    bounded!(runner).unwrap();
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
async fn pending_client_coalesces_directly_from_queued_a_to_latest_c() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    assert_eq!(
        text(bounded!(receiver.recv()).unwrap()),
        json!([0, 1, [[0, 1]]])
    );
    provider.publish(2, Some(json!({"a": 2})));
    provider.publish(3, Some(json!({"a": 3})));
    tokio::task::yield_now().await;
    assert!(receiver.try_recv().is_none());
    connection.write_completed();
    assert_eq!(
        text(bounded!(receiver.recv()).unwrap()),
        json!([1, 3, [[0, 3]], []])
    );
    hub.shutdown();
    bounded!(runner).unwrap();
}

#[tokio::test(start_paused = true)]
async fn debounce_is_per_client_reloadable_fractional_and_coalesces_bursts() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::from_micros(1500));
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    let _ = bounded!(receiver.recv()).unwrap();
    connection.write_completed();
    provider.publish(2, Some(json!({"a": 2})));
    provider.publish(3, Some(json!({"a": 3})));
    tokio::time::advance(Duration::from_micros(1499)).await;
    tokio::task::yield_now().await;
    assert!(receiver.try_recv().is_none());
    hub.set_debounce(Duration::from_micros(1000));
    assert_eq!(
        text(bounded!(receiver.recv()).unwrap()),
        json!([1, 3, [[0, 3]], []])
    );
    hub.shutdown();
    bounded!(runner).unwrap();
}

#[tokio::test]
async fn identical_selections_share_one_build_and_emit_no_updates() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1, "other": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });
    let (first, mut first_rx) = hub.connect();
    let (second, mut second_rx) = hub.connect();
    first.subscribe(vec!["/a".into()]).unwrap();
    second.subscribe(vec!["/a".into()]).unwrap();
    assert_eq!(bounded!(first_rx.recv()), bounded!(second_rx.recv()));
    assert_eq!(provider.builds.load(Ordering::Relaxed), 1);
    first.write_completed();
    second.write_completed();
    provider.publish(2, Some(json!({"a": 1, "other": 2})));
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert!(first_rx.try_recv().is_none());
    assert!(second_rx.try_recv().is_none());
    hub.shutdown();
    bounded!(runner).unwrap();
}

#[tokio::test]
async fn terminal_errors_are_ordered_and_pending_clients_are_closed() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let (connection, mut receiver) = hub.connect();
    connection.invalid_request();
    assert_eq!(
        bounded!(receiver.recv()),
        Some(V2SubscriptionOutbound::Error(Bytes::from_static(
            br#"[2,1,"invalid subscription request"]"#
        )))
    );
    assert_eq!(
        bounded!(receiver.recv()),
        Some(V2SubscriptionOutbound::Close { code: 1008 })
    );

    let (_pending, mut pending_rx) = hub.connect();
    hub.close_all(1001);
    assert_eq!(
        bounded!(pending_rx.recv()),
        Some(V2SubscriptionOutbound::Close { code: 1001 })
    );
}

#[tokio::test]
async fn serialization_failure_terminates_active_subscriber_with_1011() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    let _ = bounded!(receiver.recv());
    connection.write_completed();
    provider.fail(2);
    assert_eq!(
        bounded!(receiver.recv()),
        Some(V2SubscriptionOutbound::Error(Bytes::from_static(
            br#"[2,4,"state serialization failed"]"#
        )))
    );
    assert_eq!(
        bounded!(receiver.recv()),
        Some(V2SubscriptionOutbound::Close { code: 1011 })
    );
    hub.shutdown();
    bounded!(runner).unwrap();
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
        vosu_proptest::wait::JoinBounded::join_bounded(worker).unwrap();

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
    vosu_proptest::wait::JoinBounded::join_bounded(worker).unwrap();
    vosu_proptest::wait::JoinBounded::join_bounded(closer).unwrap();

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
        async move { bounded!(hub.run()) }
    });
    let (pending, mut pending_rx) = hub.connect();
    let (open, mut open_rx) = hub.connect();
    pending.subscribe(vec!["/a".into()]).unwrap();
    open.subscribe(vec!["/b".into()]).unwrap();
    let _ = bounded!(pending_rx.recv()).unwrap();
    let _ = bounded!(open_rx.recv()).unwrap();
    open.write_completed();

    provider.fail(2);
    for receiver in [&mut pending_rx, &mut open_rx] {
        assert_eq!(
            bounded!(receiver.recv()),
            Some(V2SubscriptionOutbound::Error(Bytes::from_static(
                br#"[2,4,"state serialization failed"]"#
            )))
        );
        assert_eq!(
            bounded!(receiver.recv()),
            Some(V2SubscriptionOutbound::Close { code: 1011 })
        );
    }
    assert_eq!(hub.client_count(), 0);
    hub.shutdown();
    bounded!(runner).unwrap();
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
        async move { bounded!(hub.run()) }
    });
    let (first, mut first_rx) = hub.connect();
    let (second, mut second_rx) = hub.connect();
    first.subscribe(vec!["/a".into()]).unwrap();
    second.subscribe(vec!["/a".into()]).unwrap();
    assert_eq!(bounded!(first_rx.recv()), bounded!(second_rx.recv()));
    assert_eq!(provider.captures.load(Ordering::Relaxed), 1);
    assert_eq!(provider.builds.load(Ordering::Relaxed), 1);
    assert_eq!(hub.0.extractions.load(Ordering::Relaxed), 1);
    assert_eq!(hub.0.encodings.load(Ordering::Relaxed), 1);

    provider.publish(2, Some(json!({"a": 2, "other": 2})));
    tokio::task::yield_now().await;
    assert_eq!(provider.captures.load(Ordering::Relaxed), 1);
    first.write_completed();
    second.write_completed();
    assert_eq!(bounded!(first_rx.recv()), bounded!(second_rx.recv()));
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
    bounded!(runner).unwrap();
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
        async move { bounded!(hub.run()) }
    });
    let (first, mut first_rx) = hub.connect();
    first.subscribe(vec!["/a".into()]).unwrap();
    let _ = bounded!(first_rx.recv()).unwrap();
    first.write_completed();

    tokio::time::advance(Duration::from_millis(1)).await;
    let (second, mut second_rx) = hub.connect();
    second.subscribe(vec!["/a".into()]).unwrap();
    let _ = bounded!(second_rx.recv()).unwrap();
    second.write_completed();
    provider.publish(2, Some(json!({"a": 2})));

    tokio::time::advance(Duration::from_micros(999)).await;
    tokio::task::yield_now().await;
    assert!(first_rx.try_recv().is_none());
    assert!(second_rx.try_recv().is_none());
    tokio::time::advance(Duration::from_micros(1)).await;
    assert_eq!(
        text(bounded!(first_rx.recv()).unwrap()),
        json!([1, 2, [[0, 2]], []])
    );
    assert!(second_rx.try_recv().is_none());
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(
        text(bounded!(second_rx.recv()).unwrap()),
        json!([1, 2, [[0, 2]], []])
    );
    assert_eq!(provider.builds.load(Ordering::Relaxed), 2);
    hub.shutdown();
    bounded!(runner).unwrap();
}

#[tokio::test(start_paused = true)]
async fn debounce_increase_recomputes_from_the_original_queue_instant() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::from_millis(1));
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    let _ = bounded!(receiver.recv()).unwrap();
    connection.write_completed();
    provider.publish(2, Some(json!({"a": 2})));
    hub.set_debounce(Duration::from_millis(2));

    tokio::time::advance(Duration::from_micros(1999)).await;
    tokio::task::yield_now().await;
    assert!(receiver.try_recv().is_none());
    tokio::time::advance(Duration::from_micros(1)).await;
    assert_eq!(
        text(bounded!(receiver.recv()).unwrap()),
        json!([1, 2, [[0, 2]], []])
    );
    hub.shutdown();
    bounded!(runner).unwrap();
}

#[tokio::test(start_paused = true)]
async fn zero_to_nonzero_debounce_uses_the_original_queue_instant() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    let _ = bounded!(receiver.recv()).unwrap();
    connection.write_completed();
    provider.publish(2, Some(json!({"a": 2})));
    hub.set_debounce(Duration::from_millis(2));

    tokio::time::advance(Duration::from_micros(1999)).await;
    tokio::task::yield_now().await;
    assert!(receiver.try_recv().is_none());
    tokio::time::advance(Duration::from_micros(1)).await;
    assert_eq!(
        text(bounded!(receiver.recv()).unwrap()),
        json!([1, 2, [[0, 2]], []])
    );
    hub.shutdown();
    bounded!(runner).unwrap();
}

#[tokio::test]
async fn zero_debounce_stays_event_driven_without_rebuilding_or_spinning() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider.clone(), Duration::ZERO);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });
    let (connection, mut receiver) = hub.connect();
    connection.subscribe(vec!["/a".into()]).unwrap();
    let _ = bounded!(receiver.recv()).unwrap();
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
    bounded!(runner).unwrap();
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
        async move { bounded!(hub.run()) }
    });
    provider.entered.wait();
    let (second, mut second_rx) = hub.connect();
    second.subscribe(vec!["/a".into()]).unwrap();
    provider.release.wait();

    let delivered = tokio::time::timeout(Duration::from_secs(2), second_rx.recv()).await;
    hub.shutdown();
    bounded!(runner).unwrap();
    assert_eq!(text(delivered.unwrap().unwrap()), json!([0, 1, [[0, 1]]]));
}

#[test]
fn precise_status_and_gameplay_track_state_use_their_actual_producing_lanes() {
    let status = compile_pointers(vec!["/state/number".into()])
        .unwrap()
        .demand;
    assert!(status.precise);
    assert!(!status.regular && !status.gameplay_regular);
    let track = compile_pointers(vec![
        "/beatmap/time/rate".into(),
        "/beatmap/time/running".into(),
    ])
    .unwrap()
    .demand;
    assert!(track.gameplay_regular);
    assert!(!track.regular && !track.precise && !track.gameplay_precise);
}

#[tokio::test]
async fn selection_added_during_projection_is_delivered_from_a_refreshed_union() {
    struct Projected {
        entered: Arc<Barrier>,
        release: Arc<Barrier>,
        first: AtomicBool,
        event: watch::Sender<u64>,
    }
    impl V2SubscriptionSnapshot for Arc<Projected> {
        fn revision(&self) -> u64 {
            1
        }
        fn document(&self, pointers: &[&[String]]) -> Result<Option<Value>, ApiError> {
            if self.first.swap(false, Ordering::SeqCst) {
                self.entered.wait();
                self.release.wait();
            }
            let full = json!({"a": 1, "b": 2});
            let mut selected = serde_json::Map::new();
            for pointer in pointers {
                if let [field] = *pointer {
                    selected.insert(field.clone(), full[field].clone());
                }
            }
            Ok(Some(Value::Object(selected)))
        }
    }
    impl V2SubscriptionProvider for Arc<Projected> {
        fn subscribe(&self) -> watch::Receiver<u64> {
            self.event.subscribe()
        }
        fn capture(&self) -> Arc<dyn V2SubscriptionSnapshot> {
            Arc::new(self.clone())
        }
    }
    let provider = Arc::new(Projected {
        entered: Arc::new(Barrier::new(2)),
        release: Arc::new(Barrier::new(2)),
        first: AtomicBool::new(true),
        event: watch::channel(1).0,
    });
    let hub = V2SubscriptionHub::new(Arc::new(provider.clone()), Duration::ZERO);
    let (first, mut first_rx) = hub.connect();
    first.subscribe(vec!["/a".into()]).unwrap();
    let (second, mut second_rx) = hub.connect();
    let dispatch = std::thread::spawn({
        let hub = hub.clone();
        move || {
            hub.dispatch(1);
        }
    });
    provider.entered.wait();
    second.subscribe(vec!["/b".into()]).unwrap();
    provider.release.wait();
    vosu_proptest::wait::JoinBounded::join_bounded(dispatch).unwrap();
    assert_eq!(text(first_rx.try_recv().unwrap()), json!([0, 1, [[0, 1]]]));
    assert!(second_rx.try_recv().is_none());
    first.write_completed();
    hub.dispatch(1);
    assert_eq!(text(second_rx.try_recv().unwrap()), json!([0, 1, [[0, 2]]]));
    assert!(first_rx.try_recv().is_none());
    first.cleanup();
    second.cleanup();
    assert!(lock(&hub.0.document).is_none());
    assert!(lock(&hub.0.plans).is_empty());
}

#[path = "tests_pointer.rs"]
mod pointer;

#[path = "tests_demand.rs"]
mod demand;
#[path = "tests_frames.rs"]
mod frames;
#[path = "tests_wire.rs"]
mod wire;
