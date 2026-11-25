
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
                    processor,
                    relays: relays.clone(),
                },
            ),
        ));
        let providers = super::super::routes::RouteProviders {
            authorization: Arc::new(AllowAll),
            state: Arc::new(NoState { events: tokio::sync::watch::channel(0).1 }),
            calculation: Arc::new(DeclaredCalculateProvider::default()),
            delegate,
        };
        let binder = TokioBinder(AxumListenerFactory::new(providers));
        let listener = bounded!(binder
            .bind(
                &ListenTarget::new("127.0.0.1", 0),
                "127.0.0.1:0".parse().unwrap(),
            ))
            .unwrap();
        assert_eq!(
            bounded!(vosu_core::desktop::existing_dashboard("127.0.0.1", listener.address().port())),
            Some(vosu_core::desktop::dashboard_url("127.0.0.1", listener.address().port())),
        );

        let mut client = bounded!(tokio::net::TcpStream::connect(listener.address()))
            .unwrap();
        bounded!(client
            .write_all(
                b"GET /websocket/commands?l=counter HTTP/1.1\r\n\
Host: localhost\r\n\
Connection: Upgrade\r\n\
Upgrade: websocket\r\n\
Sec-WebSocket-Version: 13\r\n\
Sec-WebSocket-Key: dGhlIHNhbXBsZSBub25jZQ==\r\n\r\n",
            ))
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
        bounded!(relays.shutdown());
    }


