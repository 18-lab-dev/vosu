use super::*;
use axum::{extract::Request, response::Response};
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::Notify;

struct Provider {
    focused: AtomicBool,
    generation: Arc<AtomicU64>,
    encoded: Arc<AtomicUsize>,
    filtered: Arc<AtomicUsize>,
    events: watch::Sender<u64>,
    poll_rate: Duration,
}

struct Captured {
    generation: u64,
    live_generation: Arc<AtomicU64>,
    encoded: Arc<AtomicUsize>,
    filtered: Arc<AtomicUsize>,
}

impl StateSnapshot for Captured {
    fn generation(&self) -> u64 {
        self.generation
    }

    fn encode_unfiltered(&self) -> Result<Bytes, String> {
        self.encoded.fetch_add(1, Ordering::Relaxed);
        Ok(Bytes::from(format!(
            "{{\"generation\":{}}}",
            self.generation
        )))
    }

    fn encode_filtered(&self, _filters: &[Filter]) -> Result<Bytes, String> {
        let call = self.filtered.fetch_add(1, Ordering::Relaxed);
        if call == 0 {
            self.live_generation.fetch_add(1, Ordering::AcqRel);
        }
        Ok(Bytes::from(format!(
            "{{\"generation\":{},\"filtered\":{call}}}",
            self.generation
        )))
    }
}

impl Provider {
    fn new(focused: bool) -> Arc<Self> {
        Self::new_with_poll_rate(focused, Duration::from_millis(10))
    }

    fn new_with_poll_rate(focused: bool, poll_rate: Duration) -> Arc<Self> {
        let (events, _) = watch::channel(0);
        Arc::new(Self {
            focused: AtomicBool::new(focused),
            generation: Arc::new(AtomicU64::new(1)),
            encoded: Arc::new(AtomicUsize::new(0)),
            filtered: Arc::new(AtomicUsize::new(0)),
            events,
            poll_rate,
        })
    }
}

impl StateProvider for Provider {
    fn poll_rate(&self, _: Schema) -> Duration {
        self.poll_rate
    }

    fn subscribe(&self, _: Schema) -> watch::Receiver<u64> {
        self.events.subscribe()
    }

    fn capture(&self, _: Schema) -> Option<Arc<dyn StateSnapshot>> {
        self.focused.load(Ordering::Acquire).then(|| {
            Arc::new(Captured {
                generation: self.generation.load(Ordering::Acquire),
                live_generation: Arc::clone(&self.generation),
                encoded: Arc::clone(&self.encoded),
                filtered: Arc::clone(&self.filtered),
            }) as Arc<dyn StateSnapshot>
        })
    }
}

#[tokio::test]
async fn callback_tracker_seals_cancels_and_joins_live_tasks() {
    let tracker = Arc::new(UpgradeCallbackTracker::new());
    let registration = tracker.register().unwrap();
    let task = tokio::spawn(registration.run(std::future::pending()));
    assert_eq!(tracker.active(), 1);
    tracker.seal();
    assert!(tracker.register().is_none());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), tracker.wait_drained())
            .await
            .is_err()
    );
    tracker.cancel();
    tokio::time::timeout(Duration::from_millis(100), tracker.wait_drained())
        .await
        .expect("cancellation must drain callback");
    task.await.unwrap();
    assert_eq!(tracker.active(), 0);
}

#[tokio::test]
async fn sealing_after_response_creation_owns_unresolved_upgrade() {
    use axum::Router;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let tracker = Arc::new(UpgradeCallbackTracker::new());
    let release_response = Arc::new(Notify::new());
    let (response_created_tx, mut response_created_rx) = mpsc::channel(1);
    let (handler_entered_tx, mut handler_entered_rx) = mpsc::channel(1);

    let app = Router::new().fallback({
        let tracker = Arc::clone(&tracker);
        let release_response = Arc::clone(&release_response);
        move |request: Request| {
            let tracker = Arc::clone(&tracker);
            let release_response = Arc::clone(&release_response);
            let response_created_tx = response_created_tx.clone();
            let handler_entered_tx = handler_entered_tx.clone();
            async move {
                let response =
                    upgrade_response(Endpoint::V1, request, tracker, move |_, _| async move {
                        let _ = handler_entered_tx.send(()).await;
                        std::future::pending::<()>().await;
                    })
                    .await;
                let _ = response_created_tx.send(()).await;
                release_response.notified().await;
                response
            }
        }
    });
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    });
    let client = tokio::spawn(async move {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        stream
                .write_all(
                    b"GET /ws HTTP/1.1\r\nHost: localhost\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
                )
                .await
                .unwrap();
        let mut response = [0_u8; 256];
        let read = stream.read(&mut response).await.unwrap();
        assert!(response[..read].starts_with(b"HTTP/1.1 101"));
    });

    response_created_rx.recv().await.unwrap();
    tracker.seal();
    assert_eq!(tracker.active(), 1);
    assert!(
        tokio::time::timeout(Duration::from_millis(10), tracker.wait_drained())
            .await
            .is_err()
    );

    release_response.notify_one();
    handler_entered_rx.recv().await.unwrap();
    tracker.cancel();
    tokio::time::timeout(Duration::from_millis(100), tracker.wait_drained())
        .await
        .expect("cancellation must drain the upgraded handler");
    client.await.unwrap();
    server.abort();
}

#[test]
fn exact_endpoint_classification_rejects_near_misses() {
    assert_eq!(Endpoint::classify("/ws"), Some(Endpoint::V1));
    assert_eq!(
        Endpoint::classify("/tokens"),
        Some(Endpoint::StreamCompanion)
    );
    assert_eq!(Endpoint::classify("/websocket/v2"), Some(Endpoint::V2));
    assert_eq!(
        Endpoint::classify("/websocket/v2/precise"),
        Some(Endpoint::V2Precise)
    );
    assert_eq!(
        Endpoint::classify("/websocket/v2/subscribe"),
        Some(Endpoint::V2Subscribe)
    );
    assert_eq!(
        Endpoint::classify("/websocket/commands"),
        Some(Endpoint::Commands)
    );
    for rejected in ["/ws/", "/WS", "/websocket", "/websocket/v2?x=1", "/unknown"] {
        assert_eq!(Endpoint::classify(rejected), None);
    }
}

#[test]
fn handshake_gate_matrix_never_upgrades_denied_or_unknown_requests() {
    let endpoints = [
        ("/ws", Endpoint::V1),
        ("/tokens", Endpoint::StreamCompanion),
        ("/websocket/v2", Endpoint::V2),
        ("/websocket/v2/precise", Endpoint::V2Precise),
        ("/websocket/v2/subscribe", Endpoint::V2Subscribe),
        ("/websocket/commands", Endpoint::Commands),
    ];
    for (path, endpoint) in endpoints {
        assert_eq!(
            decide_upgrade(path, true),
            UpgradeDecision::Upgrade(endpoint)
        );
        assert_eq!(
            decide_upgrade(path, false),
            UpgradeDecision::Reject {
                status: 403,
                body: "Not Found"
            }
        );
    }
    for path in ["/ws/", "/websocket/v3", "/unknown"] {
        assert_eq!(
            decide_upgrade(path, true),
            UpgradeDecision::Reject {
                status: 404,
                body: "Not Found"
            }
        );
        assert_eq!(
            decide_upgrade(path, false),
            UpgradeDecision::Reject {
                status: 403,
                body: "Not Found"
            }
        );
    }
}

#[tokio::test]
async fn real_axum_handshake_returns_101_only_for_exact_six_paths() {
    use axum::{Router, body::Body, http::StatusCode};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    async fn handshake(request: Request) -> Response {
        match decide_upgrade(request.uri().path(), true) {
            UpgradeDecision::Upgrade(endpoint) => {
                upgrade_response(
                    endpoint,
                    request,
                    Arc::new(UpgradeCallbackTracker::new()),
                    |_endpoint, _socket| async {},
                )
                .await
            }
            UpgradeDecision::Reject { status, body } => Response::builder()
                .status(status)
                .body(Body::from(body))
                .unwrap(),
        }
    }

    async fn request_status(address: std::net::SocketAddr, path: &str) -> StatusCode {
        let mut stream = tokio::net::TcpStream::connect(address).await.unwrap();
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: Upgrade\r\nUpgrade: websocket\r\nSec-WebSocket-Version: 13\r\nSec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n"
        );
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        let mut chunk = [0_u8; 512];
        loop {
            let count = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut chunk))
                .await
                .unwrap()
                .unwrap();
            assert_ne!(count, 0, "connection closed before response headers");
            response.extend_from_slice(&chunk[..count]);
            if response.windows(4).any(|window| window == b"\r\n\r\n") {
                break;
            }
        }
        let status = std::str::from_utf8(&response)
            .unwrap()
            .lines()
            .next()
            .unwrap()
            .split_ascii_whitespace()
            .nth(1)
            .unwrap()
            .parse::<u16>()
            .unwrap();
        StatusCode::from_u16(status).unwrap()
    }

    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        axum::serve(listener, Router::new().fallback(handshake))
            .await
            .unwrap();
    });

    for path in [
        "/ws",
        "/tokens",
        "/websocket/v2",
        "/websocket/v2/precise",
        "/websocket/v2/subscribe",
        "/websocket/commands",
    ] {
        assert_eq!(
            request_status(address, path).await,
            StatusCode::SWITCHING_PROTOCOLS
        );
    }
    for path in [
        "/ws/",
        "/websocket/v2/precise/",
        "/websocket/v2/subscribe/",
        "/websocket/unknown",
    ] {
        assert_eq!(request_status(address, path).await, StatusCode::NOT_FOUND);
    }
    server.abort();
}

#[tokio::test]
async fn command_transport_replies_without_state_publication() {
    let provider = Provider::new(true);
    let hub = WebSocketHub::new(Schema::V1, provider.clone());
    let (connection, mut frames) = hub.connect_commands();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });

    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(provider.encoded.load(Ordering::Relaxed), 0);
    assert!(frames.try_recv().is_none());

    let reply = Bytes::from_static(b"{\"command\":\"ok\"}");
    assert!(connection.send_text(reply.clone()).await);
    assert_eq!(frames.recv().await, Some(Outbound::Reply(reply)));

    connection.cleanup();
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn close_all_emits_going_away_clears_registry_and_seals_connects() {
    let provider = Provider::new(true);
    let hub = WebSocketHub::new(Schema::V1, provider);
    let (state_connection, mut state_frames) = hub.connect();
    let (command_connection, mut command_frames) = hub.connect_commands();
    assert_eq!(hub.client_count(), 2);

    // Even queued application traffic is discarded in favor of the
    // terminal shutdown close.
    assert!(
        command_connection
            .send_text(Bytes::from_static(b"queued"))
            .await
    );
    hub.close_all(1001).await;
    assert_eq!(hub.client_count(), 0);
    let expected = Some(Outbound::Close {
        code: 1001,
        reason: "Server shutting down",
    });
    assert_eq!(state_frames.recv().await, expected);
    assert_eq!(
        command_frames.recv().await,
        Some(Outbound::Close {
            code: 1001,
            reason: "Server shutting down",
        })
    );
    assert_eq!(state_frames.recv().await, None);
    assert_eq!(command_frames.recv().await, None);

    let (late_connection, mut late_frames) = hub.connect();
    assert_eq!(hub.client_count(), 0);
    assert_eq!(
        late_frames.recv().await,
        Some(Outbound::Close {
            code: 1001,
            reason: "Server shutting down",
        })
    );
    assert_eq!(late_frames.recv().await, None);
    drop((state_connection, command_connection, late_connection));
}

#[tokio::test]
async fn no_clients_or_focus_produces_no_encoding() {
    let provider = Provider::new(false);
    let hub = WebSocketHub::new(Schema::V1, provider.clone());
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert_eq!(provider.encoded.load(Ordering::Relaxed), 0);
    let (_connection, mut frames) = hub.connect();
    tokio::time::sleep(Duration::from_millis(30)).await;
    assert!(frames.try_recv().is_none());
    assert_eq!(provider.encoded.load(Ordering::Relaxed), 0);
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn cadence_shares_encoding_drops_backpressure_and_filters_independently() {
    let provider = Provider::new(true);
    let hub = WebSocketHub::new(Schema::V2, provider.clone());
    let (plain_a, mut a) = hub.connect();
    let (plain_b, mut b) = hub.connect();
    let (filtered_a, mut fa) = hub.connect();
    let (filtered_b, mut fb) = hub.connect();
    filtered_a.set_filters(vec![Filter::Field("one".into())]);
    filtered_b.set_filters(vec![Filter::Field("two".into())]);
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });

    let a_first = tokio::time::timeout(Duration::from_millis(100), a.recv())
        .await
        .unwrap()
        .unwrap();
    let b_first = b.recv().await.unwrap();
    assert_eq!(a_first, b_first);
    assert_eq!(provider.encoded.load(Ordering::Relaxed), 1);
    let Outbound::Text(filtered_first) = fa.recv().await.unwrap() else {
        panic!("expected filtered text");
    };
    let Outbound::Text(filtered_second) = fb.recv().await.unwrap() else {
        panic!("expected filtered text");
    };
    assert_ne!(filtered_first, filtered_second);
    assert!(filtered_first.starts_with(b"{\"generation\":1,"));
    assert!(filtered_second.starts_with(b"{\"generation\":1,"));
    assert_eq!(provider.filtered.load(Ordering::Relaxed), 2);

    // Only A is made writable. B retains its pending send, so subsequent
    // cadence ticks drop B rather than queueing another state frame.
    plain_a.write_completed();
    tokio::time::timeout(Duration::from_secs(1), a.recv())
        .await
        .expect("writable client should receive the next cadence frame")
        .expect("writable client channel should remain open");
    assert!(b.try_recv().is_none());
    assert_eq!(provider.encoded.load(Ordering::Relaxed), 2);

    drop((plain_b, filtered_a, filtered_b));
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn rapid_state_events_do_not_bypass_poll_rate() {
    let poll_rate = Duration::from_millis(100);
    let provider = Provider::new_with_poll_rate(true, poll_rate);
    let hub = WebSocketHub::new(Schema::V2, provider.clone());
    let (connection, mut frames) = hub.connect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });

    tokio::time::timeout(Duration::from_millis(100), frames.recv())
        .await
        .expect("initial frame should be immediate")
        .expect("initial frame should exist");
    connection.write_completed();
    for generation in 2..=32 {
        provider.generation.store(generation, Ordering::Release);
        provider.events.send_replace(generation);
    }

    assert!(
        tokio::time::timeout(Duration::from_millis(40), frames.recv())
            .await
            .is_err(),
        "state events must not publish before the poll deadline"
    );
    tokio::time::timeout(Duration::from_millis(200), frames.recv())
        .await
        .expect("next cadence frame should arrive")
        .expect("next cadence frame should exist");

    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn staggered_write_completions_do_not_postpone_cadence() {
    let poll_rate = Duration::from_millis(100);
    let provider = Provider::new_with_poll_rate(true, poll_rate);
    let hub = WebSocketHub::new(Schema::V2, provider);
    let (connection_a, mut frames_a) = hub.connect();
    let (connection_b, mut frames_b) = hub.connect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });

    frames_a
        .recv()
        .await
        .expect("A should receive initial frame");
    frames_b
        .recv()
        .await
        .expect("B should receive initial frame");

    tokio::time::advance(Duration::from_millis(30)).await;
    connection_a.write_completed();
    tokio::task::yield_now().await;

    tokio::time::advance(Duration::from_millis(30)).await;
    connection_b.write_completed();
    tokio::task::yield_now().await;

    assert!(frames_a.try_recv().is_none());
    assert!(frames_b.try_recv().is_none());

    tokio::time::advance(Duration::from_millis(40)).await;
    tokio::task::yield_now().await;

    assert!(matches!(frames_a.try_recv(), Some(Outbound::Text(_))));
    assert!(matches!(frames_b.try_recv(), Some(Outbound::Text(_))));

    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test(start_paused = true)]
async fn lagging_client_catch_up_does_not_postpone_shared_cadence() {
    let poll_rate = Duration::from_millis(100);
    let provider = Provider::new_with_poll_rate(true, poll_rate);
    let hub = WebSocketHub::new(Schema::V2, provider.clone());
    let (fast, mut fast_frames) = hub.connect();
    let (slow, mut slow_frames) = hub.connect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });

    fast_frames
        .recv()
        .await
        .expect("fast client should receive generation 1");
    slow_frames
        .recv()
        .await
        .expect("slow client should receive generation 1");

    tokio::time::advance(Duration::from_millis(10)).await;
    fast.write_completed();
    tokio::task::yield_now().await;

    provider.generation.store(2, Ordering::Release);
    provider.events.send_replace(2);
    tokio::task::yield_now().await;

    tokio::time::advance(Duration::from_millis(90)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        fast_frames.try_recv(),
        Some(Outbound::Text(Bytes::from_static(b"{\"generation\":2}")))
    );
    assert!(slow_frames.try_recv().is_none());

    fast.write_completed();
    tokio::task::yield_now().await;

    tokio::time::advance(Duration::from_millis(10)).await;
    slow.write_completed();
    tokio::task::yield_now().await;
    tokio::task::yield_now().await;
    assert_eq!(
        slow_frames.try_recv(),
        Some(Outbound::Text(Bytes::from_static(b"{\"generation\":2}")))
    );

    tokio::time::advance(Duration::from_millis(89)).await;
    tokio::task::yield_now().await;
    assert!(fast_frames.try_recv().is_none());

    tokio::time::advance(Duration::from_millis(1)).await;
    tokio::task::yield_now().await;
    assert_eq!(
        fast_frames.try_recv(),
        Some(Outbound::Text(Bytes::from_static(b"{\"generation\":2}")))
    );

    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn control_frames_bypass_a_full_data_slot() {
    let provider = Provider::new(true);
    let hub = WebSocketHub::new(Schema::V1, provider);
    let (mut connection, mut frames) = hub.connect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    assert_eq!(
        connection
            .receive(Inbound::Ping(Bytes::from_static(b"urgent")))
            .await,
        None
    );
    assert_eq!(
        frames.recv().await,
        Some(Outbound::Pong(Bytes::from_static(b"urgent")))
    );
    assert!(matches!(frames.recv().await, Some(Outbound::Text(_))));
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn protocol_close_preempts_full_data_and_control_slots() {
    let provider = Provider::new(true);
    let hub = WebSocketHub::new(Schema::V1, provider);
    let (mut connection, mut frames) = hub.connect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    connection
        .receive(Inbound::Ping(Bytes::from_static(b"queued pong")))
        .await;
    tokio::time::timeout(
        Duration::from_millis(100),
        connection.receive(Inbound::Continuation {
            data: Bytes::from_static(b"orphan"),
            finished: true,
        }),
    )
    .await
    .expect("protocol close must not wait behind queued pong");
    assert_eq!(hub.client_count(), 0);
    assert_eq!(
        frames.recv().await,
        Some(Outbound::Close {
            code: PROTOCOL_ERROR,
            reason: "Protocol error"
        })
    );
    assert_eq!(frames.recv().await, None);
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn reader_cleanup_cancels_writer_and_discards_queued_state() {
    let provider = Provider::new(true);
    let hub = WebSocketHub::new(Schema::V1, provider);
    let (mut connection, mut frames) = hub.connect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    tokio::time::sleep(Duration::from_millis(10)).await;
    connection.receive(Inbound::Error).await;
    assert_eq!(hub.client_count(), 0);
    assert_eq!(frames.recv().await, None);
    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn sequencing_errors_are_1002_not_1009() {
    let provider = Provider::new(true);
    let hub = WebSocketHub::new(Schema::V1, provider);
    let (mut orphan, mut orphan_frames) = hub.connect();
    orphan
        .receive(Inbound::Continuation {
            data: Bytes::from_static(b"orphan"),
            finished: true,
        })
        .await;
    assert_eq!(
        orphan_frames.recv().await,
        Some(Outbound::Close {
            code: PROTOCOL_ERROR,
            reason: "Protocol error"
        })
    );

    let (mut interleaved, mut interleaved_frames) = hub.connect();
    interleaved
        .receive(Inbound::Text {
            data: Bytes::from_static(b"first"),
            finished: false,
        })
        .await;
    interleaved
        .receive(Inbound::Binary {
            data: Bytes::from_static(b"second"),
            finished: true,
        })
        .await;
    assert_eq!(
        interleaved_frames.recv().await,
        Some(Outbound::Close {
            code: PROTOCOL_ERROR,
            reason: "Protocol error"
        })
    );
}

#[tokio::test]
async fn fragmented_limit_ping_pong_and_cleanup_are_exact() {
    let provider = Provider::new(true);
    let hub = WebSocketHub::new(Schema::V1, provider);
    let (mut connection, mut frames) = hub.connect();
    assert_eq!(
        connection
            .receive(Inbound::Ping(Bytes::from_static(b"x")))
            .await,
        None
    );
    assert_eq!(
        frames.recv().await,
        Some(Outbound::Pong(Bytes::from_static(b"x")))
    );

    assert!(
        connection
            .receive(Inbound::Text {
                data: Bytes::from(vec![b'a'; MAX_MESSAGE_BYTES]),
                finished: false,
            })
            .await
            .is_none()
    );
    assert!(
        connection
            .receive(Inbound::Continuation {
                data: Bytes::from_static(b"x"),
                finished: true,
            })
            .await
            .is_none()
    );
    assert_eq!(
        frames.recv().await,
        Some(Outbound::Close {
            code: MESSAGE_TOO_BIG,
            reason: "Message too big"
        })
    );
    assert_eq!(hub.client_count(), 0);
    connection.cleanup();
    connection.receive(Inbound::Error).await;
    assert_eq!(hub.client_count(), 0);
}

#[test]
fn concurrent_close_error_cleanup_unregisters_once() {
    let provider = Provider::new(true);
    let hub = WebSocketHub::new(Schema::V1, provider);
    let (connection, _frames) = hub.connect();
    let connection = Arc::new(connection);
    std::thread::scope(|scope| {
        for _ in 0..16 {
            let connection = Arc::clone(&connection);
            scope.spawn(move || connection.cleanup());
        }
    });
    assert_eq!(hub.client_count(), 0);
    drop(connection);
    assert_eq!(hub.client_count(), 0);
}

#[tokio::test]
async fn valid_fragments_assemble_once() {
    let provider = Provider::new(true);
    let hub = WebSocketHub::new(Schema::V1, provider);
    let (mut connection, _frames) = hub.connect();
    assert_eq!(
        connection
            .receive(Inbound::Text {
                data: Bytes::from_static(b"ab"),
                finished: false,
            })
            .await,
        None
    );
    assert_eq!(
        connection
            .receive(Inbound::Continuation {
                data: Bytes::from_static(b"cd"),
                finished: true,
            })
            .await,
        Some(Bytes::from_static(b"abcd"))
    );
}

/// Encodes the generation and the exact requested filters, so a payload reused
/// for the wrong plan or generation is visible. `pad:N` appends N bytes.
struct EchoSnapshot {
    generation: u64,
    calls: Arc<AtomicUsize>,
}

impl StateSnapshot for EchoSnapshot {
    fn generation(&self) -> u64 {
        self.generation
    }

    fn encode_unfiltered(&self) -> Result<Bytes, String> {
        Ok(Bytes::from(self.generation.to_string()))
    }

    fn encode_filtered(&self, filters: &[Filter]) -> Result<Bytes, String> {
        self.calls.fetch_add(1, Ordering::Relaxed);
        let mut payload = echo(self.generation, filters);
        if let Some(Filter::Field(field)) = filters.first()
            && let Some(size) = field.strip_prefix("pad:")
        {
            payload.push_str(&" ".repeat(size.parse().unwrap()));
        }
        Ok(Bytes::from(payload))
    }
}

fn echo(generation: u64, filters: &[Filter]) -> String {
    format!("{generation}:{}", serde_json::to_string(filters).unwrap())
}

struct EchoProvider {
    generation: AtomicU64,
    calls: Arc<AtomicUsize>,
    events: watch::Sender<u64>,
}

impl EchoProvider {
    fn new() -> Arc<Self> {
        let (events, _) = watch::channel(1);
        Arc::new(Self {
            generation: AtomicU64::new(1),
            calls: Arc::new(AtomicUsize::new(0)),
            events,
        })
    }

    fn publish(&self) {
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.events.send_replace(generation);
    }

    fn calls(&self) -> usize {
        self.calls.load(Ordering::Relaxed)
    }
}

impl StateProvider for EchoProvider {
    // A long cadence leaves delivery to the explicit generation/writability wakes.
    fn poll_rate(&self, _: Schema) -> Duration {
        Duration::from_secs(60)
    }

    fn subscribe(&self, _: Schema) -> watch::Receiver<u64> {
        self.events.subscribe()
    }

    fn capture(&self, _: Schema) -> Option<Arc<dyn StateSnapshot>> {
        Some(Arc::new(EchoSnapshot {
            generation: self.generation.load(Ordering::Acquire),
            calls: Arc::clone(&self.calls),
        }))
    }
}

async fn echo_text(receiver: &mut OutboundReceiver) -> Bytes {
    match tokio::time::timeout(Duration::from_secs(1), receiver.recv()).await {
        Ok(Some(Outbound::Text(bytes))) => bytes,
        frame => panic!("expected text frame, got {frame:?}"),
    }
}

#[tokio::test]
async fn identical_filter_plans_share_one_projection_per_generation() {
    let provider = EchoProvider::new();
    let hub = WebSocketHub::new(Schema::V2, provider.clone());
    let plan = || {
        vec![Filter::Nested {
            field: "play".into(),
            keys: vec![Filter::Field("0".into())],
        }]
    };
    let mut clients: Vec<_> = (0..100)
        .map(|_| {
            let (connection, receiver) = hub.connect();
            connection.set_filters(plan());
            (connection, receiver)
        })
        .collect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });

    for (_, receiver) in &mut clients {
        assert_eq!(echo_text(receiver).await, echo(1, &plan()));
    }
    assert_eq!(provider.calls(), 1);

    // Client 0 keeps its write pending; sharing must not deliver to it.
    for (connection, _) in &clients[1..] {
        connection.write_completed();
    }
    provider.publish();
    hub.wake();
    for (_, receiver) in &mut clients[1..] {
        assert_eq!(echo_text(receiver).await, echo(2, &plan()));
    }
    assert!(clients[0].1.try_recv().is_none());
    assert_eq!(provider.calls(), 2);

    // Its own acknowledgement delivers the latest generation to it alone.
    clients[0].0.write_completed();
    assert_eq!(echo_text(&mut clients[0].1).await, echo(2, &plan()));
    for (_, receiver) in &mut clients[1..] {
        assert!(receiver.try_recv().is_none());
    }

    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn unique_filter_plans_beyond_sharing_bound_each_get_their_projection() {
    let provider = EchoProvider::new();
    let hub = WebSocketHub::new(Schema::V1, provider.clone());
    let count = MAX_SHARED_PROJECTIONS * 2 + 3;
    let plan = |index: usize| vec![Filter::Field(format!("u{index}"))];
    let mut clients: Vec<_> = (0..count)
        .map(|index| {
            let (connection, receiver) = hub.connect();
            connection.set_filters(plan(index));
            (connection, receiver)
        })
        .collect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });

    for generation in [1, 2] {
        if generation == 2 {
            for (connection, _) in &clients {
                connection.write_completed();
            }
            provider.publish();
            hub.wake();
        }
        for (index, (_, receiver)) in clients.iter_mut().enumerate() {
            assert_eq!(echo_text(receiver).await, echo(generation, &plan(index)));
        }
        assert_eq!(provider.calls(), count * generation as usize);
    }

    hub.shutdown();
    runner.await.unwrap();
}

/// Publishes `generation` (the first is delivered on start) to `clients`,
/// whose plans are `[Filter::Field(field)]`, and returns how many
/// projections that took.
async fn deliver_fields(
    provider: &EchoProvider,
    hub: &WebSocketHub,
    clients: &mut [(Connection, OutboundReceiver)],
    generation: u64,
    fields: &[&str],
) -> usize {
    let before = provider.calls();
    if generation > 1 {
        for (connection, _) in clients.iter() {
            connection.write_completed();
        }
        provider.publish();
        hub.wake();
    }
    for ((_, receiver), field) in clients.iter_mut().zip(fields) {
        let plan = [Filter::Field((*field).into())];
        assert_eq!(echo_text(receiver).await, echo(generation, &plan));
    }
    provider.calls() - before
}

#[tokio::test]
async fn plan_changes_and_departures_keep_one_projection_per_distinct_plan() {
    let provider = EchoProvider::new();
    let hub = WebSocketHub::new(Schema::V2, provider.clone());
    let plan = |field: &str| vec![Filter::Field(field.into())];
    let mut clients: Vec<_> = ["a", "b", "c"]
        .into_iter()
        .map(|field| {
            let (connection, receiver) = hub.connect();
            connection.set_filters(plan(field));
            (connection, receiver)
        })
        .collect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });

    let fields = ["a", "b", "c"];
    assert_eq!(
        deliver_fields(&provider, &hub, &mut clients, 1, &fields).await,
        3
    );
    // Once b moves to c's plan, the two share one projection.
    clients[1].0.set_filters(plan("c"));
    let fields = ["a", "c", "c"];
    assert_eq!(
        deliver_fields(&provider, &hub, &mut clients, 2, &fields).await,
        2
    );
    // A newcomer joining a's plan shares it too.
    let (connection, receiver) = hub.connect();
    connection.set_filters(plan("a"));
    clients.push((connection, receiver));
    let fields = ["a", "c", "c", "a"];
    assert_eq!(
        deliver_fields(&provider, &hub, &mut clients, 3, &fields).await,
        2
    );
    // Departures leave every remaining client its own correct frame.
    let (departed, _) = clients.remove(1);
    departed.cleanup();
    drop(departed);
    let fields = ["a", "c", "a"];
    assert_eq!(
        deliver_fields(&provider, &hub, &mut clients, 4, &fields).await,
        2
    );

    hub.shutdown();
    runner.await.unwrap();
}

#[test]
fn shared_projections_are_bounded_and_never_cross_plans() {
    let calls = Arc::new(AtomicUsize::new(0));
    let snapshot = EchoSnapshot {
        generation: 1,
        calls: Arc::clone(&calls),
    };
    let plan = |field: String| Arc::new(FilterPlan::new(vec![Filter::Field(field)]));

    // Variant bound: equal plans in distinct allocations share until it is full.
    let mut shared = SharedProjections::default();
    for index in 0..MAX_SHARED_PROJECTIONS {
        shared.encode(&snapshot, &plan(format!("v{index}")));
    }
    assert_eq!(calls.load(Ordering::Relaxed), MAX_SHARED_PROJECTIONS);
    assert_eq!(
        shared.encode(&snapshot, &plan("v0".into())).unwrap(),
        echo(1, &[Filter::Field("v0".into())])
    );
    assert_eq!(calls.load(Ordering::Relaxed), MAX_SHARED_PROJECTIONS);
    for _ in 0..2 {
        assert_eq!(
            shared.encode(&snapshot, &plan("overflow".into())).unwrap(),
            echo(1, &[Filter::Field("overflow".into())])
        );
    }
    assert_eq!(calls.load(Ordering::Relaxed), MAX_SHARED_PROJECTIONS + 2);
    assert_eq!(shared.entries.len(), MAX_SHARED_PROJECTIONS);

    // Byte bound: a payload that would exceed it is never retained.
    calls.store(0, Ordering::Relaxed);
    let mut shared = SharedProjections::default();
    let large = plan(format!("pad:{MAX_SHARED_PROJECTION_BYTES}"));
    for _ in 0..2 {
        assert!(shared.encode(&snapshot, &large).unwrap().len() > MAX_SHARED_PROJECTION_BYTES);
    }
    assert_eq!(calls.load(Ordering::Relaxed), 2);
    assert_eq!((shared.entries.len(), shared.bytes), (0, 0));

    // A hash collision between different plans still compares the filters.
    let colliding = |field: &str| {
        Arc::new(FilterPlan {
            filters: vec![Filter::Field(field.into())],
            hash: 7,
        })
    };
    assert_eq!(
        shared.encode(&snapshot, &colliding("a")).unwrap(),
        echo(1, &[Filter::Field("a".into())])
    );
    assert_eq!(
        shared.encode(&snapshot, &colliding("b")).unwrap(),
        echo(1, &[Filter::Field("b".into())])
    );
    assert_eq!(calls.load(Ordering::Relaxed), 4);
}

#[tokio::test]
async fn each_acknowledgement_of_the_same_generation_gets_its_forced_delivery() {
    let provider = EchoProvider::new();
    let hub = WebSocketHub::new(Schema::V2, provider.clone());
    let plan = || vec![Filter::Field("a".into())];
    let mut clients: Vec<_> = (0..3)
        .map(|_| {
            let (connection, receiver) = hub.connect();
            connection.set_filters(plan());
            (connection, receiver)
        })
        .collect();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { hub.run().await }
    });
    for (_, receiver) in &mut clients {
        assert_eq!(echo_text(receiver).await, echo(1, &plan()));
    }

    // Generation 2 arrives while every client is still writing frame 1.
    provider.publish();
    hub.wake();
    tokio::task::yield_now().await;
    for (_, receiver) in &mut clients {
        assert!(receiver.try_recv().is_none());
    }

    // Each acknowledgement carries the same ceiling after the previous wake
    // was consumed; none may be suppressed until the 60 s cadence.
    for index in 0..clients.len() {
        clients[index].0.write_completed();
        assert_eq!(echo_text(&mut clients[index].1).await, echo(2, &plan()));
        for (_, receiver) in &mut clients[index + 1..] {
            assert!(receiver.try_recv().is_none());
        }
    }

    // Acknowledgements batched before the hub runs coalesce yet all deliver.
    provider.publish();
    hub.wake();
    tokio::task::yield_now().await;
    for (connection, _) in &clients {
        connection.write_completed();
    }
    for (_, receiver) in &mut clients {
        assert_eq!(echo_text(receiver).await, echo(3, &plan()));
    }

    hub.shutdown();
    runner.await.unwrap();
}

#[tokio::test]
async fn a_dropped_receiver_releases_its_frame_and_refuses_state_frames() {
    let hub = WebSocketHub::new(Schema::V2, EchoProvider::new());
    let (connection, mut receiver) = hub.connect();
    let frame = Bytes::from_static(b"frame");
    assert!(connection.client.send_data(frame.clone(), 1, false));
    // The slot holds one frame until it is taken, like a one-slot channel.
    assert!(!connection.client.data.try_send(frame.clone()));
    assert_eq!(receiver.recv().await, Some(Outbound::Text(frame.clone())));

    connection.write_completed();
    assert!(connection.client.send_data(frame.clone(), 2, false));
    drop(receiver);
    assert!(
        lock_unpoisoned(&connection.client.data.state)
            .frame
            .is_none()
    );
    connection.write_completed();
    assert!(!connection.client.send_data(frame, 3, false));
    assert_eq!(
        connection.client.state.load(Ordering::Acquire),
        CLIENT_OPEN,
        "a refused frame leaves the client writable"
    );
}
