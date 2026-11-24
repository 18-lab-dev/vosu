use super::*;
use vosu_core::state::StateValue;

const WAIT: Duration = Duration::from_secs(2);

fn client() -> (
    ApiV1SubscriptionConnection,
    Receiver,
    oneshot::Receiver<ApiPlan>,
) {
    let (send, request) = oneshot::channel();
    let mailbox = Arc::new(Mailbox::new());
    (
        ApiV1SubscriptionConnection {
            client: Arc::new(Client {
                request: Mutex::new(Some(send)),
                established: AtomicBool::new(false),
                writing: AtomicBool::new(false),
                mailbox: Arc::clone(&mailbox),
                wake: Notify::new(),
                closed: CancellationToken::new(),
            }),
        },
        Receiver::new(mailbox),
        request,
    )
}

fn update(binding: u64, values: Vec<Option<StateValue>>) -> Arc<ApiUpdate> {
    Arc::new(ApiUpdate {
        session: Some("s1".into()),
        revision: 13,
        binding,
        values,
        lanes: LaneBits::default(),
        ended: false,
        error: None,
    })
}

fn running(
    initial: Arc<ApiUpdate>,
    debounce: Duration,
) -> (
    ApiV1SubscriptionConnection,
    Receiver,
    watch::Sender<Arc<ApiUpdate>>,
    tokio::task::JoinHandle<()>,
) {
    let (connection, receiver, _) = client();
    let (send, mut updates) = watch::channel(initial);
    let (config, debounce) = watch::channel(debounce);
    let weak = Arc::downgrade(&connection.client);
    let task = tokio::spawn(async move {
        // Keep the configuration sender live just as the hub does.
        let _config = config;
        stream(weak, &mut updates, debounce, CancellationToken::new()).await;
    });
    (connection, receiver, send, task)
}

async fn text(receiver: &mut Receiver) -> serde_json::Value {
    let frame = tokio::time::timeout(WAIT, receiver.recv())
        .await
        .unwrap()
        .unwrap();
    let Outbound::Text(bytes) = frame else {
        panic!("expected text, got {frame:?}");
    };
    serde_json::from_slice(&bytes).unwrap()
}
async fn stopped(task: tokio::task::JoinHandle<()>) {
    tokio::time::timeout(WAIT, task).await.unwrap().unwrap();
}

#[tokio::test]
async fn slow_receiver_skips_intermediate_and_preserves_distinct_keys() {
    let (c, mut rx, tx, task) = running(
        update(1, vec![Some(StateValue::I64(7)), Some(StateValue::I64(8))]),
        Duration::ZERO,
    );
    assert_eq!(
        text(&mut rx).await,
        serde_json::json!([0, 1, [[0, 7], [1, 8]]])
    );
    tx.send_replace(update(
        1,
        vec![Some(StateValue::I64(9)), Some(StateValue::I64(8))],
    ));
    // Give the producer a turn while the first Text is still in flight.
    tokio::time::timeout(WAIT, tokio::task::yield_now())
        .await
        .unwrap();
    assert!(
        rx.try_recv().is_none(),
        "dequeue alone must not release the write slot"
    );
    tx.send_replace(update(
        1,
        vec![Some(StateValue::I64(9)), Some(StateValue::I64(12))],
    ));
    c.write_completed();
    assert_eq!(
        text(&mut rx).await,
        serde_json::json!([1, 2, [[0, 9], [1, 12]], []])
    );
    drop(c);
    stopped(task).await;
}

#[tokio::test]
async fn focus_binding_resets_equal_values_without_resetting_revision() {
    let (c, mut rx, tx, task) = running(update(1, vec![Some(StateValue::I64(31))]), Duration::ZERO);
    assert_eq!(text(&mut rx).await[1], 1);
    c.write_completed();
    // Same session id and values: binding epoch catches skipped away/back switches.
    tx.send_replace(update(3, vec![Some(StateValue::I64(31))]));
    assert_eq!(text(&mut rx).await, serde_json::json!([0, 2, [[0, 31]]]));
    drop(c);
    stopped(task).await;
}

#[tokio::test]
async fn selected_removals_distinguish_absence_and_null_and_do_not_repeat() {
    let (c, mut rx, tx, task) = running(
        update(
            1,
            vec![None, Some(StateValue::Null), Some(StateValue::I64(43))],
        ),
        Duration::ZERO,
    );
    assert_eq!(
        text(&mut rx).await,
        serde_json::json!([0, 1, [[1, null], [2, 43]]])
    );
    c.write_completed();
    tx.send_replace(update(1, vec![None, None, Some(StateValue::I64(44))]));
    assert_eq!(
        text(&mut rx).await,
        serde_json::json!([1, 2, [[2, 44]], [1]])
    );
    c.write_completed();
    tx.send_replace(update(1, vec![None, None, Some(StateValue::I64(45))]));
    assert_eq!(
        text(&mut rx).await,
        serde_json::json!([1, 3, [[2, 45]], []])
    );
    drop(c);
    stopped(task).await;
}

#[tokio::test]
async fn detach_drains_final_removals_before_normal_close() {
    let (c, mut rx, tx, task) = running(update(1, vec![Some(StateValue::I64(53))]), Duration::ZERO);
    text(&mut rx).await;
    let mut terminal = (*update(2, vec![None])).clone();
    terminal.ended = true;
    tx.send_replace(Arc::new(terminal));
    // Core removes terminal registrations immediately; channel closure must not
    // overtake the pending final removal while the previous frame is in flight.
    drop(tx);
    c.write_completed();
    assert_eq!(text(&mut rx).await, serde_json::json!([1, 2, [], [0]]));
    assert!(rx.try_recv().is_none());
    c.write_completed();
    assert_eq!(
        tokio::time::timeout(WAIT, rx.recv()).await.unwrap(),
        Some(Outbound::Close { code: 1000 })
    );
    stopped(task).await;
}

#[tokio::test]
async fn immutable_first_request_and_full_pong_queue_are_terminal() {
    let (c, mut rx, request) = client();
    c.text(br#"["/vosu.play/combo"]"#);
    assert_eq!(
        tokio::time::timeout(WAIT, request)
            .await
            .unwrap()
            .unwrap()
            .pointers()
            .len(),
        1
    );
    c.text(br#"["/vosu.play/score"]"#);
    assert_eq!(
        rx.try_recv(),
        Some(Outbound::Error(wire::error_frame(
            5,
            wire::ALREADY_SUBSCRIBED_MESSAGE
        )))
    );
    assert_eq!(rx.try_recv(), Some(Outbound::Close { code: 1008 }));
    let (c, mut rx, _) = client();
    c.ping(Bytes::from_static(b"first"));
    c.ping(Bytes::from_static(b"second"));
    assert_eq!(rx.try_recv(), Some(Outbound::Close { code: 1011 }));
    assert_eq!(
        rx.try_recv(),
        Some(Outbound::Pong(Bytes::from_static(b"first")))
    );
}

#[test]
fn request_bounds_and_pointer_errors_use_shared_wire_codes() {
    for (request, code) in [
        (b"[]".as_slice(), 1),
        (br#"["/~2"]"#, 2),
        (br#"["/a","/a/b"]"#, 2),
        (b"{}", 1),
    ] {
        let (c, mut rx, _) = client();
        c.text(request);
        let Some(Outbound::Error(bytes)) = rx.try_recv() else {
            panic!("missing error");
        };
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&bytes).unwrap()[1],
            code
        );
        assert_eq!(rx.try_recv(), Some(Outbound::Close { code: 1008 }));
    }
}

#[tokio::test(start_paused = true)]
async fn producing_lanes_debounce_only_pending_work() {
    let mut initial = (*update(1, vec![Some(StateValue::I64(61))])).clone();
    initial.lanes.precise_always = true;
    let (c, mut rx, tx, task) = running(Arc::new(initial.clone()), Duration::from_millis(100));
    text(&mut rx).await;
    c.write_completed();
    initial.values[0] = Some(StateValue::I64(62));
    tx.send_replace(Arc::new(initial.clone()));
    assert!(
        tokio::time::timeout(Duration::from_millis(99), rx.recv())
            .await
            .is_err(),
        "a producing lane must wait for its debounce deadline"
    );
    tokio::time::advance(Duration::from_millis(1)).await;
    assert_eq!(
        text(&mut rx).await,
        serde_json::json!([1, 2, [[0, 62]], []])
    );
    c.write_completed();
    initial.values[0] = Some(StateValue::I64(63));
    initial.lanes = LaneBits::default();
    tx.send_replace(Arc::new(initial));
    assert_eq!(
        text(&mut rx).await,
        serde_json::json!([1, 3, [[0, 63]], []])
    );
    drop(c);
    stopped(task).await;
}

#[test]
fn bounded_encoder_handles_exact_limit_and_stops_large_values() {
    // [0,1 ,[[0,"..."]]] is 17 bytes apart from the string contents.
    let small = frame(1, None, &[Some(StateValue::String("x".repeat(100).into()))])
        .unwrap()
        .unwrap();
    let overhead = small.len() - 100;
    let exact = vec![Some(StateValue::String(
        "x".repeat(MAX_FRAME - overhead).into(),
    ))];
    assert_eq!(frame(1, None, &exact).unwrap().unwrap().len(), MAX_FRAME);
    let over = vec![Some(StateValue::String(
        "x".repeat(MAX_FRAME - overhead + 1).into(),
    ))];
    assert!(frame(1, None, &over).is_err());
    assert!(
        frame(
            1,
            None,
            &[Some(StateValue::Bytes(vec![7; MAX_FRAME].into()))]
        )
        .is_err()
    );
}

#[tokio::test]
async fn serialization_error_is_followed_by_policy_close() {
    let (c, mut rx, _, task) = running(
        update(
            1,
            vec![Some(StateValue::String("secret".repeat(MAX_FRAME).into()))],
        ),
        Duration::ZERO,
    );
    assert_eq!(
        tokio::time::timeout(WAIT, rx.recv()).await.unwrap(),
        Some(Outbound::Error(wire::error_frame(
            4,
            wire::SERIALIZATION_FAILED_MESSAGE
        )))
    );
    assert_eq!(rx.try_recv(), Some(Outbound::Close { code: 1008 }));
    stopped(task).await;
    drop(c);
}

#[tokio::test(start_paused = true)]
async fn first_message_deadline_is_five_seconds_and_releases_request() {
    let (c, mut rx, request) = client();
    let now = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(6),
        await_plan(
            &Arc::downgrade(&c.client),
            &CancellationToken::new(),
            request,
            now + REQUEST_TIMEOUT,
        ),
    )
    .await
    .unwrap();
    assert!(result.is_none());
    assert_eq!(Instant::now() - now, REQUEST_TIMEOUT);
    assert_eq!(
        rx.try_recv(),
        Some(Outbound::Error(wire::error_frame(
            3,
            wire::SUBSCRIPTION_TIMEOUT_MESSAGE
        )))
    );
    assert_eq!(rx.try_recv(), Some(Outbound::Close { code: 1008 }));
    assert!(lock(&c.client.request).is_none());
}

#[tokio::test]
async fn shutdown_releases_pending_request_without_waiting_for_deadline() {
    let (c, mut rx, request) = client();
    let shutdown = CancellationToken::new();
    shutdown.cancel();
    assert!(
        tokio::time::timeout(
            WAIT,
            await_plan(
                &Arc::downgrade(&c.client),
                &shutdown,
                request,
                Instant::now() + REQUEST_TIMEOUT,
            )
        )
        .await
        .unwrap()
        .is_none()
    );
    assert_eq!(rx.try_recv(), Some(Outbound::Close { code: 1001 }));
    assert!(lock(&c.client.request).is_none());
}

#[tokio::test]
async fn active_focus_loss_removes_values_and_reacquisition_is_initial() {
    let (c, mut rx, tx, task) = running(update(1, vec![Some(StateValue::I64(71))]), Duration::ZERO);
    text(&mut rx).await;
    c.write_completed();
    let mut unfocused = (*update(2, vec![None])).clone();
    unfocused.session = None;
    tx.send_replace(Arc::new(unfocused));
    assert_eq!(text(&mut rx).await, serde_json::json!([1, 2, [], [0]]));
    c.write_completed();
    tx.send_replace(update(3, vec![Some(StateValue::I64(71))]));
    assert_eq!(text(&mut rx).await, serde_json::json!([0, 3, [[0, 71]]]));
    drop(c);
    stopped(task).await;
}

#[tokio::test]
async fn session_event_selection_encodes_both_focus_flips_in_one_frame() {
    let mut initial = (*update(
        0,
        vec![Some(StateValue::Bool(true)), Some(StateValue::Bool(false))],
    ))
    .clone();
    initial.session = None;
    let (c, mut rx, tx, task) = running(Arc::new(initial.clone()), Duration::ZERO);
    assert_eq!(
        text(&mut rx).await,
        serde_json::json!([0, 1, [[0, true], [1, false]]])
    );
    c.write_completed();
    initial.values = vec![Some(StateValue::Bool(false)), Some(StateValue::Bool(true))];
    tx.send_replace(Arc::new(initial));
    assert_eq!(
        text(&mut rx).await,
        serde_json::json!([1, 2, [[0, false], [1, true]], []])
    );
    drop(c);
    stopped(task).await;
}
