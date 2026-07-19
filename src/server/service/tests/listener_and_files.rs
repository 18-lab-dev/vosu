    use std::sync::atomic::{AtomicBool, Ordering};
    use tokio::io::AsyncWriteExt as _;

    struct TransportSubscriptionState {
        events: tokio::sync::watch::Receiver<u64>,
        fail: AtomicBool,
    }

    impl crate::server::v2_subscription::V2SubscriptionSnapshot for TransportSubscriptionState {
        fn revision(&self) -> u64 {
            *self.events.borrow()
        }

        fn document(&self) -> Result<Option<Value>, ApiError> {
            if self.fail.load(Ordering::Acquire) {
                Err(ApiError::Serialization("injected transport failure".into()))
            } else {
                Ok(None)
            }
        }
    }

    impl crate::server::v2_subscription::V2SubscriptionProvider for TransportSubscriptionState {
        fn subscribe(&self) -> tokio::sync::watch::Receiver<u64> {
            self.events.clone()
        }

        fn capture(
            &self,
        ) -> Arc<dyn crate::server::v2_subscription::V2SubscriptionSnapshot> {
            Arc::new(Self {
                events: self.events.clone(),
                fail: AtomicBool::new(self.fail.load(Ordering::Acquire)),
            })
        }
    }

    #[tokio::test(start_paused = true)]
    async fn v2_subscription_terminal_writer_drain_is_bounded() {
        let mut writer = Box::pin(std::future::pending());
        let started = tokio::time::Instant::now();

        drain_v2_subscription_writer(writer.as_mut()).await;

        assert_eq!(started.elapsed(), V2_SUBSCRIPTION_TERMINAL_DRAIN_TIMEOUT);
    }

    async fn subscription_client(
        address: std::net::SocketAddr,
    ) -> (tokio::net::TcpStream, Vec<u8>) {
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(
                b"GET /websocket/v2/subscribe HTTP/1.1\r\n\
Host: localhost\r\n\
Connection: Upgrade\r\n\
Upgrade: websocket\r\n\
Sec-WebSocket-Version: 13\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        let header_end = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                if let Some(offset) = response
                    .windows(4)
                    .position(|window| window == b"\r\n\r\n")
                {
                    break offset + 4;
                }
                let mut chunk = [0_u8; 1024];
                let read = client.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0);
                response.extend_from_slice(&chunk[..read]);
            }
        })
        .await
        .expect("subscription upgrade must complete");
        assert!(response[..header_end].starts_with(b"HTTP/1.1 101"));
        (client, response.split_off(header_end))
    }

    fn masked_client_frame_header(opcode: u8, payload_len: usize) -> Vec<u8> {
        let mask = [0x12, 0x34, 0x56, 0x78];
        let mut frame = vec![0x80 | opcode];
        match payload_len {
            len @ 0..=125 => frame.push(0x80 | len as u8),
            len @ 126..=65535 => {
                frame.push(0x80 | 126);
                frame.extend_from_slice(&(len as u16).to_be_bytes());
            }
            len => {
                frame.push(0x80 | 127);
                frame.extend_from_slice(&(len as u64).to_be_bytes());
            }
        }
        frame.extend_from_slice(&mask);
        frame
    }

    fn masked_client_frame(opcode: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [0x12, 0x34, 0x56, 0x78];
        let mut frame = masked_client_frame_header(opcode, payload.len());
        frame.extend(
            payload
                .iter()
                .enumerate()
                .map(|(index, byte)| byte ^ mask[index % mask.len()]),
        );
        frame
    }

    async fn server_frame(
        client: &mut tokio::net::TcpStream,
        buffered: &mut Vec<u8>,
    ) -> (u8, Vec<u8>) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while buffered.len() < 2 {
                let mut chunk = [0_u8; 1024];
                let read = client.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0);
                buffered.extend_from_slice(&chunk[..read]);
            }
            assert_eq!(buffered[1] & 0x80, 0);
            let short_len = buffered[1] & 0x7f;
            let header_len = match short_len {
                126 => 4,
                127 => 10,
                _ => 2,
            };
            while buffered.len() < header_len {
                let mut chunk = [0_u8; 1024];
                let read = client.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0);
                buffered.extend_from_slice(&chunk[..read]);
            }
            let payload_len = match short_len {
                126 => u16::from_be_bytes([buffered[2], buffered[3]]) as usize,
                127 => u64::from_be_bytes(buffered[2..10].try_into().unwrap()) as usize,
                len => len as usize,
            };
            while buffered.len() < header_len + payload_len {
                let mut chunk = [0_u8; 1024];
                let read = client.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0);
                buffered.extend_from_slice(&chunk[..read]);
            }
            let frame: Vec<_> = buffered.drain(..header_len + payload_len).collect();
            let opcode = frame[0] & 0x0f;
            let payload = frame[header_len..].to_vec();
            if opcode == 0x8 {
                client
                    .write_all(&masked_client_frame(0x8, &payload))
                    .await
                    .unwrap();
            }
            (opcode, payload)
        })
        .await
        .expect("server frame must arrive")
    }

    fn close_code(payload: &[u8]) -> u16 {
        u16::from_be_bytes(payload[..2].try_into().unwrap())
    }

    #[tokio::test]
    async fn forced_listener_drain_drops_idle_production_bridge_children() {
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
        let processor = Arc::new(CommandProcessor::new(command_providers, relays.clone()));
        let unused = Arc::new(UnusedServices);
        let delegate = Arc::new(ProductionRouteDelegate::new(
            ProductionServices::with_native_report_probe(
                PageService,
                unused.clone(),
                counters,
                Arc::new(Current::default()),
                unused.clone(),
                unused.clone(),
                unused,
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
                b"GET /websocket/commands?l=counter HTTP/1.1\r\n\
Host: localhost\r\n\
Connection: Upgrade\r\n\
Upgrade: websocket\r\n\
Sec-WebSocket-Version: 13\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            )
            .await
            .unwrap();
        let mut response = Vec::new();
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while !response.windows(4).any(|window| window == b"\r\n\r\n") {
                let mut chunk = [0_u8; 512];
                let read = client.read(&mut chunk).await.unwrap();
                assert_ne!(read, 0);
                response.extend_from_slice(&chunk[..read]);
            }
        })
        .await
        .expect("WebSocket upgrade must complete");
        assert!(response.starts_with(b"HTTP/1.1 101"));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while commands.client_count() == 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .unwrap();

        listener.begin_drain();
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), listener.drained())
                .await
                .is_err()
        );
        listener.cancel_survivors();
        tokio::time::timeout(std::time::Duration::from_secs(1), listener.drained())
            .await
            .expect("forced drain must release callback and tracked transport");
        let mut byte = [0_u8; 1];
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(1), client.read(&mut byte))
                .await
                .expect("client must observe shutdown")
                .unwrap(),
            0,
        );
        assert_eq!(commands.client_count(), 0);
        relays.shutdown().await;
    }

    #[tokio::test]
    async fn v2_subscription_transport_preserves_terminal_order_and_lifecycle() {
        use crate::server::runtime::{
            AxumListenerFactory, ListenTarget, ListenerBinder, TokioBinder,
        };

        let directory = tempfile::tempdir().unwrap();
        let counters = Arc::new(ReloadableCounterService::production(
            directory.path().join("static"),
            directory.path().join("settings"),
            directory.path().join("cache"),
        ));
        let (_, relay_events) = tokio::sync::watch::channel(0);
        let relay_state = Arc::new(NoState {
            events: relay_events,
        });
        let (_subscription_tx, subscription_events) = tokio::sync::watch::channel(1);
        let subscription_state = Arc::new(TransportSubscriptionState {
            events: subscription_events,
            fail: AtomicBool::new(false),
        });
        let subscriptions =
            V2SubscriptionHub::new(subscription_state.clone(), std::time::Duration::ZERO);
        let subscription_runner = tokio::spawn({
            let subscriptions = subscriptions.clone();
            async move { subscriptions.run().await }
        });
        let relays = Arc::new(WebSocketRelaySink::new());
        let command_providers = Arc::new(CommandProviders);
        let processor = Arc::new(CommandProcessor::new(command_providers, relays.clone()));
        let unused = Arc::new(UnusedServices);
        let delegate = Arc::new(ProductionRouteDelegate::new(
            ProductionServices::with_native_report_probe(
                PageService,
                unused.clone(),
                counters,
                Arc::new(Current::default()),
                unused.clone(),
                unused.clone(),
                unused,
                WebSocketEndpoints {
                    v1: WebSocketHub::new(websocket::Schema::V1, relay_state.clone()),
                    sc: WebSocketHub::new(
                        websocket::Schema::StreamCompanion,
                        relay_state.clone(),
                    ),
                    v2: WebSocketHub::new(websocket::Schema::V2, relay_state.clone()),
                    precise: WebSocketHub::new(
                        websocket::Schema::V2Precise,
                        relay_state.clone(),
                    ),
                    commands: WebSocketHub::new(websocket::Schema::V1, relay_state),
                    subscriptions: subscriptions.clone(),
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

        let (mut second_text, mut buffered) = subscription_client(listener.address()).await;
        second_text
            .write_all(&masked_client_frame(0x1, br#"["/play/score"]"#))
            .await
            .unwrap();
        assert_eq!(server_frame(&mut second_text, &mut buffered).await.0, 0x1);
        second_text
            .write_all(&masked_client_frame(0x1, br#"["/play/name"]"#))
            .await
            .unwrap();
        let (opcode, payload) = server_frame(&mut second_text, &mut buffered).await;
        assert_eq!(opcode, 0x1);
        assert_eq!(
            payload,
            br#"[2,5,"subscription already established"]"#.as_slice()
        );
        let (opcode, payload) = server_frame(&mut second_text, &mut buffered).await;
        assert_eq!((opcode, close_code(&payload)), (0x8, 1008));

        let (mut binary, mut buffered) = subscription_client(listener.address()).await;
        binary
            .write_all(&masked_client_frame(0x2, b"binary"))
            .await
            .unwrap();
        let (opcode, payload) = server_frame(&mut binary, &mut buffered).await;
        assert_eq!((opcode, close_code(&payload)), (0x8, 1003));

        let (mut malformed, mut buffered) = subscription_client(listener.address()).await;
        malformed.write_all(&[0x81, 0x00]).await.unwrap();
        let (opcode, payload) = server_frame(&mut malformed, &mut buffered).await;
        assert_eq!((opcode, close_code(&payload)), (0x8, 1002));

        let (mut oversized, mut buffered) = subscription_client(listener.address()).await;
        // Declaring a frame above the configured bound is sufficient for tungstenite to
        // reject it. Omitting the body leaves no unread TCP data that could turn the
        // server's graceful close into an abortive reset on Windows.
        oversized
            .write_all(&masked_client_frame_header(0x1, 1_048_577))
            .await
            .unwrap();
        let (opcode, payload) = server_frame(&mut oversized, &mut buffered).await;
        assert_eq!((opcode, close_code(&payload)), (0x8, 1009));

        let (mut timeout_client, mut buffered) = subscription_client(listener.address()).await;
        for ping in 0..4_u8 {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            timeout_client
                .write_all(&masked_client_frame(0x9, &[ping]))
                .await
                .unwrap();
        }
        let mut pong_seen = [false; 4];
        let timeout_payload = loop {
            let (opcode, payload) = server_frame(&mut timeout_client, &mut buffered).await;
            match opcode {
                0xa => pong_seen[payload[0] as usize] = true,
                0x1 => break payload,
                other => panic!("unexpected opcode before subscription timeout: {other}"),
            }
        };
        assert!(pong_seen.into_iter().all(|seen| seen));
        assert_eq!(
            timeout_payload,
            br#"[2,3,"subscription timed out"]"#.as_slice()
        );
        let (opcode, payload) = server_frame(&mut timeout_client, &mut buffered).await;
        assert_eq!((opcode, close_code(&payload)), (0x8, 1008));

        subscription_state.fail.store(true, Ordering::Release);
        let (mut failed, mut buffered) = subscription_client(listener.address()).await;
        failed
            .write_all(&masked_client_frame(0x1, br#"["/play/score"]"#))
            .await
            .unwrap();
        let (opcode, payload) = server_frame(&mut failed, &mut buffered).await;
        assert_eq!(opcode, 0x1);
        assert_eq!(payload, br#"[2,4,"state serialization failed"]"#.as_slice());
        let (opcode, payload) = server_frame(&mut failed, &mut buffered).await;
        assert_eq!((opcode, close_code(&payload)), (0x8, 1011));
        subscription_state.fail.store(false, Ordering::Release);

        let (mut pending, mut pending_buffered) = subscription_client(listener.address()).await;
        let (mut active, mut active_buffered) = subscription_client(listener.address()).await;
        active
            .write_all(&masked_client_frame(0x1, br#"["/play/score"]"#))
            .await
            .unwrap();
        assert_eq!(
            server_frame(&mut active, &mut active_buffered).await.0,
            0x1
        );
        subscriptions.close_all(1001);
        let (opcode, payload) = server_frame(&mut pending, &mut pending_buffered).await;
        assert_eq!((opcode, close_code(&payload)), (0x8, 1001));
        let (opcode, payload) = server_frame(&mut active, &mut active_buffered).await;
        assert_eq!((opcode, close_code(&payload)), (0x8, 1001));
        tokio::time::timeout(std::time::Duration::from_secs(1), async {
            while subscriptions.client_count() != 0 {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("transport cleanup must unregister every client exactly once");

        listener.begin_drain();
        listener.cancel_survivors();
        listener.drained().await;
        subscriptions.shutdown();
        subscription_runner.await.unwrap();
        relays.shutdown().await;
    }

    #[test]
    fn websocket_uses_exact_per_listener_tracker() {
        let first = Arc::new(UpgradeCallbackTracker::new());
        let second = Arc::new(UpgradeCallbackTracker::new());
        let mut first_request = Request::new(Body::empty());
        first_request.extensions_mut().insert(Arc::clone(&first));
        let mut second_request = Request::new(Body::empty());
        second_request.extensions_mut().insert(Arc::clone(&second));

        let selected_first = listener_tracker(&first_request).unwrap();
        let selected_second = listener_tracker(&second_request).unwrap();
        assert!(Arc::ptr_eq(&selected_first, &first));
        assert!(Arc::ptr_eq(&selected_second, &second));
        assert!(!Arc::ptr_eq(&selected_first, &selected_second));

        first.seal();
        assert_eq!(first.active(), 0);
        assert_eq!(second.active(), 0);
        assert!(listener_tracker(&Request::new(Body::empty())).is_none());
    }
    #[test]
    fn production_file_adapter_roots_generic_beatmap_at_songs() {
        use std::io::Read as _;

        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(root.path().join("set")).unwrap();
        std::fs::write(root.path().join("set/map.osu"), b"songs-root").unwrap();
        let selected = BeatmapFiles {
            songs_root: Some(root.path().to_owned()),
            ready: true,
            ..BeatmapFiles::default()
        };

        let ResourceOutcome::File(mut target) = resolve_file_endpoint(
            Endpoint::BeatmapPath,
            &selected,
            "/files/beatmap/set/map.osu",
            "/files/beatmap/set/map.osu",
        )
        .unwrap() else {
            panic!("generic beatmap path must resolve below Songs");
        };
        let mut body = String::new();
        target.file.read_to_string(&mut body).unwrap();
        assert_eq!(body, "songs-root");
    }

    #[test]
    fn production_file_adapter_rejects_current_file_traversal() {
        let root = tempfile::tempdir().unwrap();
        let selected = BeatmapFiles {
            beatmap_file: Some(PathBuf::from("../outside.osu")),
            songs_root: Some(root.path().join("Songs")),
            ready: true,
            ..BeatmapFiles::default()
        };
        assert!(matches!(
            resolve_file_endpoint(
                Endpoint::BeatmapFile,
                &selected,
                "/files/beatmap/file",
                "/files/beatmap/file",
            ),
            Ok(ResourceOutcome::Missing(MissingKind::NotFound))
        ));
    }

    #[test]
    fn production_exact_file_endpoint_rejects_directories() {
        let root = tempfile::tempdir().unwrap();
        std::fs::create_dir(root.path().join("selected.osu")).unwrap();
        let selected = BeatmapFiles {
            beatmap_file: Some(PathBuf::from("selected.osu")),
            songs_root: Some(root.path().to_owned()),
            ready: true,
            ..BeatmapFiles::default()
        };
        assert!(matches!(
            resolve_file_endpoint(
                Endpoint::BeatmapFile,
                &selected,
                "/files/beatmap/file",
                "/files/beatmap/file",
            ),
            Ok(ResourceOutcome::Missing(MissingKind::NotFound))
        ));
    }

    #[tokio::test]
    async fn focused_provider_without_songs_root_keeps_song_routes_not_ready() {
        let selected = BeatmapFiles {
            beatmap_file: Some(PathBuf::from("set/map.osu")),
            ready: true,
            ..BeatmapFiles::default()
        };
        for (endpoint, path) in [
            (Endpoint::SongsFile, "/Songs/set/map.osu"),
            (Endpoint::BeatmapPath, "/files/beatmap/set/map.osu"),
        ] {
            let outcome = resolve_file_endpoint(endpoint, &selected, path, path).unwrap();
            assert!(matches!(
                outcome,
                ResourceOutcome::Missing(MissingKind::NotReady)
            ));
            let response = resource_response(outcome, None).await;
            assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
            let body = axum::body::to_bytes(response.into_body(), usize::MAX)
                .await
                .unwrap();
            assert_eq!(
                serde_json::from_slice::<Value>(&body).unwrap(),
                json!({"error": "osu is not ready/running"})
            );
        }
    }

    #[tokio::test]
    async fn production_lazer_skin_keeps_explicit_rejection() {
        let selected = BeatmapFiles {
            lazer: true,
            ready: true,
            ..BeatmapFiles::default()
        };
        let error = resolve_file_endpoint(
            Endpoint::SkinFile,
            &selected,
            "/files/skin/cursor.png",
            "/files/skin/cursor.png",
        )
        .unwrap_err();
        assert!(matches!(error, FileRouteError::LazerSkinUnavailable));
        let response = file_route_error_response(error);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            serde_json::from_slice::<Value>(&body).unwrap(),
            json!({"error": "This endpoint is unavailable for the lazer client."})
        );
    }
