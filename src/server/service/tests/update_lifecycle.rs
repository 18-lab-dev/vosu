    #[tokio::test]
    async fn update_outcomes_are_terminal_status_200_envelopes() {
        let cases = [
            (Ok(UpdateRouteResult::Latest), json!({ "status": "latest" })),
            (
                Ok(UpdateRouteResult::NoFiles),
                json!({ "status": "noFiles" }),
            ),
            (
                Err("update already in progress".into()),
                json!({ "error": "update already in progress" }),
            ),
        ];
        for (result, expected) in cases {
            let response = update_response(result, None);
            assert_eq!(response.status(), StatusCode::OK);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(serde_json::from_slice::<Value>(&body).unwrap(), expected);
        }
    }

    #[tokio::test]
    async fn updated_handoff_waits_for_terminal_body_poll() {
        let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::clone(&fired);
        let trigger = DeferredUpdateTrigger::new(move || {
            observed.store(true, std::sync::atomic::Ordering::SeqCst);
        });
        let mut body =
            DeferredUpdateBody::new(json_bytes(json!({ "status": "updated" })), trigger, None);

        assert!(!fired.load(std::sync::atomic::Ordering::SeqCst));
        let payload = body.next().await.unwrap().unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&payload).unwrap(),
            json!({ "status": "updated" })
        );
        assert!(!fired.load(std::sync::atomic::Ordering::SeqCst));
        assert!(body.next().await.is_none());
        assert!(fired.load(std::sync::atomic::Ordering::SeqCst));

        let disconnected = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let observed = Arc::clone(&disconnected);
        let body = DeferredUpdateBody::new(
            Bytes::from_static(b"payload"),
            DeferredUpdateTrigger::new(move || {
                observed.store(true, std::sync::atomic::Ordering::SeqCst);
            }),
            None,
        );
        drop(body);
        assert!(disconnected.load(std::sync::atomic::Ordering::SeqCst));
    }

    #[tokio::test]
    async fn production_update_flush_precedes_websocket_shutdown_and_listener_drain() {
        use crate::server::runtime::{
            AxumListenerFactory, ListenTarget, ListenerBinder, TokioBinder,
        };
        use tokio::io::AsyncWriteExt as _;

        let directory = tempfile::tempdir().unwrap();
        let counters = Arc::new(ReloadableCounterService::production(
            directory.path().join("static"),
            directory.path().join("settings"),
            directory.path().join("cache"),
        ));
        let (_, events) = tokio::sync::watch::channel(0);
        let state = Arc::new(NoState { events });
        let v1 = WebSocketHub::new(websocket::Schema::V1, state.clone());
        let sc = WebSocketHub::new(websocket::Schema::StreamCompanion, state.clone());
        let v2 = WebSocketHub::new(websocket::Schema::V2, state.clone());
        let precise = WebSocketHub::new(websocket::Schema::V2Precise, state.clone());
        let commands = WebSocketHub::new(websocket::Schema::V1, state.clone());
        let subscriptions = V2SubscriptionHub::new(state.clone(), std::time::Duration::ZERO);
        let relays = Arc::new(WebSocketRelaySink::new());
        let command_providers = Arc::new(CommandProviders);
        let processor = Arc::new(CommandProcessor::new(
            command_providers.clone(),
            command_providers,
            relays.clone(),
        ));
        let unused = Arc::new(UnusedServices);
        let update_started = CancellationToken::new();
        let updater = Arc::new(UpdatedServices(update_started.clone()));
        let delegate = Arc::new(ProductionRouteDelegate::new(
            ProductionServices::with_native_report_probe(
                PageService,
                unused.clone(),
                counters,
                Arc::new(Current::default()),
                unused.clone(),
                unused.clone(),
                updater,
                WebSocketEndpoints {
                    v1,
                    sc,
                    v2,
                    precise,
                    commands: commands.clone(),
                    subscriptions,
                    processor,
                    relays: relays.clone(),
                },
            ),
        ));
        let snapshots = Arc::new(SnapshotManager::new(Arc::new(ManagerSnapshot::not_ready(
            1,
            ApiRuntimeSettings::default(),
        ))));
        let providers = super::super::routes::RouteProviders {
            authorization: Arc::new(AllowAll),
            state: Arc::new(SnapshotStateProvider::new(snapshots)),
            calculation: Arc::new(LegacyCalculateProvider::new(Arc::new(Current::default()))),
            delegate,
        };
        let binder = TokioBinder(AxumListenerFactory::new(providers));
        let listener = binder
            .bind(
                &ListenTarget::new("127.0.0.1", 0),
                "127.0.0.1:0".parse().unwrap(),
            )
            .await
            .unwrap();

        let mut client = tokio::net::TcpStream::connect(listener.address())
            .await
            .unwrap();
        client
            .write_all(
                b"GET /websocket/commands?l=__ingame__ HTTP/1.1\r\n\
Host: localhost\r\n\
Connection: Upgrade\r\n\
Upgrade: websocket\r\n\
Sec-WebSocket-Version: 13\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        while !response.windows(4).any(|window| window == b"\r\n\r\n") {
            let mut chunk = [0_u8; 512];
            let read =
                tokio::time::timeout(std::time::Duration::from_secs(1), client.read(&mut chunk))
                    .await
                    .unwrap()
                    .unwrap();
            assert_ne!(read, 0);
            response.extend_from_slice(&chunk[..read]);
        }
        assert!(response.starts_with(b"HTTP/1.1 101"));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while commands.client_count() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        let mut closing_client = tokio::net::TcpStream::connect(listener.address())
            .await
            .unwrap();
        closing_client
            .write_all(
                b"GET /websocket/commands?l=__ingame__ HTTP/1.1\r\n\
Host: localhost\r\n\
Connection: Upgrade\r\n\
Upgrade: websocket\r\n\
Sec-WebSocket-Version: 13\r\n\
Sec-WebSocket-Key: MDEyMzQ1Njc4OWFiY2RlZg==\r\n\r\n",
            )
            .await
            .unwrap();
        let mut closing_response = Vec::new();
        while !closing_response
            .windows(4)
            .any(|window| window == b"\r\n\r\n")
        {
            let mut chunk = [0_u8; 512];
            let read = closing_client.read(&mut chunk).await.unwrap();
            assert_ne!(read, 0);
            closing_response.extend_from_slice(&chunk[..read]);
        }
        assert!(closing_response.starts_with(b"HTTP/1.1 101"));
        let mask = [1_u8, 2, 3, 4];
        let mut backpressure = Vec::with_capacity(131 * 50_000 + 8);
        for _ in 0..50_000 {
            backpressure.extend_from_slice(&[0x89, 0xfd]);
            backpressure.extend_from_slice(&mask);
            for index in 0..125 {
                backpressure.push(mask[index % mask.len()]);
            }
        }
        backpressure.extend_from_slice(&[0x88, 0x82]);
        backpressure.extend_from_slice(&mask);
        backpressure.extend_from_slice(&[0x03 ^ mask[0], 0xe8 ^ mask[1]]);
        let (mut closing_read, mut closing_write) = closing_client.into_split();
        let websocket_bytes = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            let writer = async {
                closing_write.write_all(&backpressure).await.unwrap();
            };
            let reader = async {
                tokio::time::sleep(std::time::Duration::from_millis(25)).await;
                let mut websocket_bytes = Vec::new();
                while !websocket_bytes.contains(&0x88) {
                    let mut chunk = [0_u8; 16 * 1024];
                    let read = closing_read.read(&mut chunk).await.unwrap();
                    assert_ne!(read, 0, "peer close reply must be flushed before EOF");
                    websocket_bytes.extend_from_slice(&chunk[..read]);
                }
                websocket_bytes
            };
            let ((), websocket_bytes) = tokio::join!(writer, reader);
            websocket_bytes
        })
        .await
        .expect("backpressured peer close handshake must complete");
        assert!(websocket_bytes.contains(&0x88));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while commands.client_count() != 1 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();
        let owner_listener = Arc::clone(&listener);
        let owner_commands = commands.clone();
        let update_owner = tokio::spawn(async move {
            update_started.cancelled().await;
            owner_listener.begin_drain();
            owner_commands.close_all(1001).await;
            owner_commands.shutdown();
        });
        let mut update_client = tokio::net::TcpStream::connect(listener.address())
            .await
            .unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(1),
            update_client.write_all(
                b"GET /api/runUpdates HTTP/1.1\r\n\
Host: localhost\r\n\
Connection: close\r\n\r\n",
            ),
        )
        .await
        .expect("update request write must complete")
        .unwrap();
        let mut update_response = Vec::new();
        let mut premature_websocket_close = [0_u8; 16];
        let update_read = update_client.read_to_end(&mut update_response);
        tokio::pin!(update_read);
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            tokio::select! {
                biased;
                result = &mut update_read => result.unwrap(),
                result = client.read(&mut premature_websocket_close) => {
                    panic!(
                        "WebSocket shutdown became observable before terminal update response: {result:?}"
                    );
                }
            }
        })
        .await
        .expect("graceful shutdown must flush the terminal update response first");
        let body_offset = update_response
            .windows(4)
            .position(|window| window == b"\r\n\r\n")
            .map(|position| position + 4)
            .expect("update response headers");
        assert!(update_response.starts_with(b"HTTP/1.1 200"));
        assert_eq!(&update_response[body_offset..], br#"{"status":"updated"}"#);

        tokio::time::timeout(std::time::Duration::from_secs(1), update_owner)
            .await
            .expect("update trigger must reach the production-equivalent owner")
            .unwrap();
        let mut close_frame = [0_u8; 16];
        let close_len = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            client.read(&mut close_frame),
        )
        .await
        .expect("WebSocket close must follow the update response")
        .unwrap();
        assert!(close_len >= 4);
        assert_eq!(close_frame[0] & 0x0f, 0x08);
        assert_eq!(&close_frame[2..4], &1001_u16.to_be_bytes());

        tokio::time::timeout(std::time::Duration::from_secs(1), listener.drained())
            .await
            .expect("listener must drain after response and WebSocket close");
        assert_eq!(commands.client_count(), 0);
        relays.shutdown().await;
    }

