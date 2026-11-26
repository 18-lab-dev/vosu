    #[tokio::test]
    async fn dashboard_status_reports_overlay_connection_counts() {
        let root = tempfile::tempdir().unwrap();
        let folder = root.path().join("static/counter");
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("index.html"), b"12345").unwrap();
        let counters = Arc::new(ReloadableCounterService::production(
            root.path().join("static"), root.path().join("settings"), root.path().join("cache"),
        ));
        let (_, events) = tokio::sync::watch::channel(0);
        let state = Arc::new(NoState { events });
        let relays = Arc::new(WebSocketRelaySink::new());
        let processor = Arc::new(CommandProcessor::new(Arc::new(CommandProviders), relays.clone()));
        let unused = Arc::new(UnusedServices);
        let delegate = ProductionRouteDelegate::new(ProductionServices::with_native_report_probe(
            PageService, unused.clone(), counters, Arc::new(Current::default()),
            unused.clone(), unused.clone(), unused,
            WebSocketEndpoints {
                v1: WebSocketHub::new(websocket::Schema::V1, state.clone()),
                sc: WebSocketHub::new(websocket::Schema::StreamCompanion, state.clone()),
                v2: WebSocketHub::new(websocket::Schema::V2, state.clone()),
                precise: WebSocketHub::new(websocket::Schema::V2Precise, state.clone()),
                commands: WebSocketHub::new(websocket::Schema::V1, state.clone()),
                processor: processor.clone(), relays: relays.clone(),
            },
        ));
        let _first = delegate.connections.register(Some("counter".into()));
        let _second = delegate.connections.register(Some("counter".into()));
        let status = bounded!(delegate.dispatch(Endpoint::DashboardStatus, Request::builder().uri("/api/dashboard/status").body(Body::empty()).unwrap(), Bytes::new()));
        assert_eq!(status.status(), StatusCode::OK);
        let status: Value = serde_json::from_slice(&bounded!(axum::body::to_bytes(status.into_body(), 16384)).unwrap()).unwrap();
        assert_eq!(status["connections"]["counter"], 2);
        assert_eq!(status["instances"], json!([]));
        bounded!(processor.shutdown());
        bounded!(relays.shutdown());
    }

    #[test]
    fn names_route_is_post_only() {
        assert_eq!(crate::server::routes::classify(&axum::http::Method::POST, "/api/counters/names/x"), Some(Endpoint::CounterNames));
        assert_eq!(crate::server::routes::classify(&axum::http::Method::GET, "/api/counters/names/x"), None);
    }


    #[test]
    fn raw_splat_decodes_once_and_query_is_separate() {
        let request = Request::builder()
            .uri("/api/counters/download/https%3A%2F%2Fexample.com%2Fa%252Fb.zip%3Fx%3Da%2526b?name=counter&update=true")
            .body(Body::empty())
            .unwrap();
        let raw = request
            .uri()
            .path()
            .strip_prefix("/api/counters/download/")
            .unwrap();
        assert_eq!(
            percent_decode_once(raw).unwrap(),
            "https://example.com/a%2Fb.zip?x=a%26b",
        );
        let query = query_pairs(&request);
        assert_eq!(query_value(&query, "name"), Some("counter"));
        assert_eq!(query_value(&query, "update"), Some("true"));
    }

    #[test]
    fn overlay_page_query_requires_nonempty_exact_parameter_value() {
        assert!(
            decoded_query_value("overlay=counter", "overlay")
                .is_some_and(|value| !value.is_empty())
        );
        assert!(
            decoded_query_value("x=1&overlay=&y=2", "overlay").is_none_or(|value| value.is_empty())
        );
        assert!(
            decoded_query_value("xoverlay=counter", "overlay").is_none_or(|value| value.is_empty())
        );
        assert!(
            decoded_query_value("x=overlay%3Dcounter", "overlay")
                .is_none_or(|value| value.is_empty())
        );
    }

    #[test]
    fn websocket_query_matches_url_search_params_and_client_ids_do_not_collide() {
        let plus = decoded_query_value("x=ignored&l=counter+name", "l").unwrap();
        assert_eq!(
            CommandClient::from_query(9, Some(&plus)).unwrap().overlay,
            "counter name"
        );

        let encoded_space = decoded_query_value("l=counter%2520name", "l").unwrap();
        assert_eq!(
            CommandClient::from_query(9, Some(&encoded_space))
                .unwrap()
                .overlay,
            "counter name"
        );

        let local_id = 1;
        let ids = (1..=5)
            .map(|namespace| namespaced_client_id(namespace, local_id).unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 5);
        for namespace in 1..=5 {
            let id = namespaced_client_id(namespace, local_id).unwrap();
            assert_eq!(client_channel(id), Some(namespace));
        }
        assert_eq!(client_channel(8), None);
    }

    #[tokio::test]
    async fn relays_are_processed_in_order_before_browser_delivery() {
        let (_, events) = tokio::sync::watch::channel(0);
        let provider = Arc::new(NoState { events });
        let hub = WebSocketHub::new(websocket::Schema::V1, provider);
        let (connection, mut outbound) = hub.connect_commands();
        let local_id = connection.id();
        let connection = Arc::new(tokio::sync::Mutex::new(connection));

        let sink = Arc::new(WebSocketRelaySink::new());
        let providers = Arc::new(CommandProviders);
        let processor = Arc::new(CommandProcessor::new(providers, sink.clone()));
        sink.bind_processor(&processor);

        let client_id = namespaced_client_id(COMMANDS_CHANNEL, local_id).unwrap();
        let client = CommandClient::from_query(client_id, Some("counter")).unwrap();
        let _registration =
            sink.register(COMMANDS_CHANNEL, client, bounded!(connection.lock()).control_handle());
        for value in [1, 2] {
            sink.relay(Relay {
                except: None,
                command: "updateSettings".into(),
                overlay: "counter".into(),
                payload: Some(format!(r#"{{"value":{value}}}"#)),
            });
        }

        for value in [1, 2] {
            let frame = tokio::time::timeout(std::time::Duration::from_secs(1), outbound.recv())
                .await
                .unwrap()
                .unwrap();
            let Outbound::Reply(bytes) = frame else {
                panic!("relay must produce a command reply");
            };
            assert_eq!(
                serde_json::from_slice::<Value>(&bytes).unwrap(),
                json!({ "command": "updateSettings", "message": { "value": value } })
            );
        }

        bounded!(connection.lock()).cleanup();
        bounded!(sink.shutdown());
    }
    #[tokio::test]
    async fn relay_reply_survives_connection_mutex_contention() {
        let (_, events) = tokio::sync::watch::channel(0);
        let hub = WebSocketHub::new(websocket::Schema::V1, Arc::new(NoState { events }));
        let (connection, mut outbound) = hub.connect_commands();
        let (healthy, mut healthy_outbound) = hub.connect_commands();
        let local_id = connection.id();
        let healthy_id = namespaced_client_id(COMMANDS_CHANNEL, healthy.id()).unwrap();
        let healthy = Arc::new(tokio::sync::Mutex::new(healthy));
        let connection = Arc::new(tokio::sync::Mutex::new(connection));

        let sink = Arc::new(WebSocketRelaySink::new());
        let processor = Arc::new(CommandProcessor::new(Arc::new(CommandProviders), sink.clone()));
        sink.bind_processor(&processor);
        let client_id = namespaced_client_id(COMMANDS_CHANNEL, local_id).unwrap();
        let client = CommandClient::from_query(client_id, Some("counter")).unwrap();
        let _registration =
            sink.register(COMMANDS_CHANNEL, client, bounded!(connection.lock()).control_handle());
        let healthy_client = CommandClient::from_query(healthy_id, Some("counter")).unwrap();
        let _healthy_registration = sink.register(
            COMMANDS_CHANNEL,
            healthy_client,
            bounded!(healthy.lock()).control_handle(),
        );


        let guard = bounded!(connection.lock());
        sink.relay(Relay {
            except: None,
            command: "updateSettings".into(),
            overlay: "counter".into(),
            payload: Some(r#"{"value":1}"#.into()),
        });
        tokio::task::yield_now().await;

        let Some(Outbound::Reply(healthy_reply)) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            healthy_outbound.recv(),
        )
        .await
        .unwrap()
        else {
            panic!("healthy recipient must progress while another connection mutex is held");
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&healthy_reply).unwrap(),
            json!({ "command": "updateSettings", "message": { "value": 1 } })
        );

        let Some(Outbound::Reply(reply)) = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            outbound.recv(),
        )
        .await
        .unwrap()
        else {
            panic!("relay reply must arrive while the connection mutex remains held");
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&reply).unwrap(),
            json!({ "command": "updateSettings", "message": { "value": 1 } })
        );
        drop(guard);

        bounded!(connection.lock()).cleanup();
        bounded!(healthy.lock()).cleanup();
        bounded!(sink.shutdown());
    }


    #[tokio::test]
    async fn full_relay_recipient_does_not_block_healthy_recipients() {
        let (_, events) = tokio::sync::watch::channel(0);
        let provider = Arc::new(NoState { events });
        let hub = WebSocketHub::new(websocket::Schema::V1, provider);
        let (stalled, _stalled_outbound) = hub.connect_commands();
        let (healthy, mut healthy_outbound) = hub.connect_commands();
        let stalled_id = namespaced_client_id(COMMANDS_CHANNEL, stalled.id()).unwrap();
        let healthy_id = namespaced_client_id(COMMANDS_CHANNEL, healthy.id()).unwrap();
        let stalled = Arc::new(tokio::sync::Mutex::new(stalled));
        let healthy = Arc::new(tokio::sync::Mutex::new(healthy));

        let sink = Arc::new(WebSocketRelaySink::new());
        let processor = Arc::new(CommandProcessor::new(Arc::new(CommandProviders), sink.clone()));
        sink.bind_processor(&processor);
        let stalled_client = CommandClient::from_query(stalled_id, Some("counter")).unwrap();
        let healthy_client = CommandClient::from_query(healthy_id, Some("counter")).unwrap();
        let _stalled_registration = sink.register(
            COMMANDS_CHANNEL,
            stalled_client.clone(),
            bounded!(stalled.lock()).control_handle(),
        );
        let _healthy_registration = sink.register(
            COMMANDS_CHANNEL,
            healthy_client,
            bounded!(healthy.lock()).control_handle(),
        );

        // Saturate control traffic without draining this recipient.
        for _ in 0..16 {
            assert!(bounded!(stalled.lock()).try_send_text(Bytes::from_static(b"occupied")));
        }
        sink.relay(Relay {
            except: None,
            command: "updateSettings".into(),
            overlay: "counter".into(),
            payload: Some(r#"{"value":1}"#.into()),
        });
        let received = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            healthy_outbound.recv(),
        )
        .await
        .unwrap();
        let Some(Outbound::Reply(received)) = received else {
            panic!("healthy recipient must receive the settings update");
        };
        assert_eq!(
            serde_json::from_slice::<Value>(&received).unwrap(),
            json!({ "command": "updateSettings", "message": { "value": 1 } })
        );
        assert!(!bounded!(stalled.lock()).try_send_text(Bytes::from_static(b"still-open")));
        bounded!(stalled.lock()).cleanup();
        bounded!(healthy.lock()).cleanup();
        bounded!(sink.shutdown());
    }

    #[tokio::test]
    async fn relay_worker_shutdown_is_joined() {
        let sink = WebSocketRelaySink::new();
        assert!(!sink.is_shutdown());
        bounded!(sink.shutdown());
        assert!(sink.is_shutdown());
    }

    #[test]
    fn traversal_and_malformed_percent_are_rejected() {
        assert_eq!(
            percent_decode_once("safe%252Fchild").unwrap(),
            "safe%2Fchild"
        );
        assert!(percent_decode_once("%GG").is_err());
        let root = tempfile::tempdir().unwrap();
        let outcome = files::resolve_encoded_resource(
            Some(root.path()),
            "..%2Fsecret",
            "/..%2Fsecret",
            MissingKind::NotFound,
        )
        .unwrap();
        assert!(matches!(
            outcome,
            ResourceOutcome::Missing(MissingKind::NotFound)
        ));
    }
