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
async fn real_axum_handshake_returns_101_only_for_exact_five_paths() {
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
        "/websocket/commands",
    ] {
        assert_eq!(
            request_status(address, path).await,
            StatusCode::SWITCHING_PROTOCOLS
        );
    }
    for path in ["/ws/", "/websocket/v2/precise/", "/websocket/unknown"] {
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
