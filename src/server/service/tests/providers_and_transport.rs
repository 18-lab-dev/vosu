    #[test]
    fn pp_query_maps_every_documented_knob() {
        let query = PpQuery {
            path: None,
            lazer: Some(true),
            mode: Some(3),
            ar: Some(9.1),
            cs: Some(4.2),
            hp: Some(6.3),
            od: Some(8.4),
            clock_rate: Some(1.25),
            passed_objects: Some(99),
            combo: Some(88),
            n_misses: Some(1),
            n100: Some(2),
            n300: Some(3),
            n50: Some(4),
            n_geki: Some(5),
            n_katu: Some(6),
            mods: Some(ModsInput::Acronyms("HD, DT".into())),
            accuracy: Some(97.5),
            slider_end_hits: Some(7),
            small_tick_hits: Some(8),
            large_tick_hits: Some(9),
            hitresult_priority: Some(1),
        };
        let request = performance_request(&query).unwrap();
        assert_eq!(
            request.mode,
            Some(crate::calculation::engine::Ruleset::Mania)
        );
        assert_eq!(request.lazer, Some(true));
        assert_eq!(request.clock_rate, Some(1.25));
        assert_eq!(
            (request.ar, request.cs, request.hp, request.od),
            (Some(9.1), Some(4.2), Some(6.3), Some(8.4))
        );
        assert_eq!(
            (request.passed_objects, request.combo),
            (Some(99), Some(88))
        );
        assert_eq!(
            (request.n_misses, request.n100, request.n300, request.n50),
            (Some(1), Some(2), Some(3), Some(4))
        );
        assert_eq!((request.n_geki, request.n_katu), (Some(5), Some(6)));
        assert_eq!(
            (
                request.slider_end_hits,
                request.small_tick_hits,
                request.large_tick_hits
            ),
            (Some(7), Some(8), Some(9))
        );
        assert_eq!(request.accuracy, Some(97.5));
        assert_eq!(request.hitresult_priority, HitResultPriority::WorstCase);
        let LegacyMods::Structured(mods) = request.mods else {
            panic!("expected structured acronyms")
        };
        assert_eq!(
            mods.iter()
                .map(|item| item.acronym.as_str())
                .collect::<Vec<_>>(),
            ["HD", "DT"]
        );
    }

    #[tokio::test]
    async fn pp_selects_current_or_safe_canonical_regular_osu_file() {
        let provider = LegacyCalculateProvider::new(Arc::new(Current {
            bytes: Some(Bytes::from_static(b"current")),
            files: None,
        }));
        assert_eq!(
            provider.select_beatmap(&PpQuery::default()).await.unwrap(),
            Some(Bytes::from_static(b"current")),
        );

        let directory = tempfile::tempdir().unwrap();
        let osu = directory.path().join("map.osu");
        tokio::fs::write(&osu, b"path").await.unwrap();
        let selected = provider
            .select_beatmap(&PpQuery {
                path: Some(osu.to_string_lossy().into_owned()),
                ..PpQuery::default()
            })
            .await
            .unwrap();
        assert_eq!(selected, Some(Bytes::from_static(b"path")));

        let wrong_extension = directory.path().join("map.txt");
        tokio::fs::write(&wrong_extension, b"path").await.unwrap();
        assert!(
            provider
                .select_beatmap(&PpQuery {
                    path: Some(wrong_extension.to_string_lossy().into_owned()),
                    ..PpQuery::default()
                })
                .await
                .is_err()
        );

        let directory_named_osu = directory.path().join("folder.osu");
        tokio::fs::create_dir(&directory_named_osu).await.unwrap();
        assert!(
            provider
                .select_beatmap(&PpQuery {
                    path: Some(directory_named_osu.to_string_lossy().into_owned()),
                    ..PpQuery::default()
                })
                .await
                .is_err()
        );
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

    #[tokio::test]
    async fn counter_search_source_is_explicit_and_errors_are_escaped_non_200() {
        assert_eq!(counter_search_source(None), Some(false));
        assert_eq!(counter_search_source(Some("source=installed")), Some(false));
        assert_eq!(counter_search_source(Some("x=1&source=available")), Some(true));
        assert_eq!(counter_search_source(Some("source=Available")), None);
        assert_eq!(counter_search_source(Some("source=")), None);

        let failed = counter_search_response(Err(r#"<img src=x onerror="alert(1)">"#.into()));
        assert_eq!(failed.status(), StatusCode::BAD_GATEWAY);
        let body = axum::body::to_bytes(failed.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            &body[..],
            b"Error: &lt;img src=x onerror=&quot;alert(1)&quot;&gt;"
        );
        assert_eq!(
            counter_search_response(Ok(Bytes::from_static(b"<article></article>"))).status(),
            StatusCode::OK
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
            sink.register(COMMANDS_CHANNEL, client, connection.lock().await.control_handle());
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

        connection.lock().await.cleanup();
        sink.shutdown().await;
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
            sink.register(COMMANDS_CHANNEL, client, connection.lock().await.control_handle());
        let healthy_client = CommandClient::from_query(healthy_id, Some("counter")).unwrap();
        let _healthy_registration = sink.register(
            COMMANDS_CHANNEL,
            healthy_client,
            healthy.lock().await.control_handle(),
        );


        let guard = connection.lock().await;
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

        connection.lock().await.cleanup();
        healthy.lock().await.cleanup();
        sink.shutdown().await;
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
            stalled.lock().await.control_handle(),
        );
        let _healthy_registration = sink.register(
            COMMANDS_CHANNEL,
            healthy_client,
            healthy.lock().await.control_handle(),
        );

        // Saturate control traffic without draining this recipient.
        for _ in 0..16 {
            assert!(stalled.lock().await.try_send_text(Bytes::from_static(b"occupied")));
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
        assert!(!stalled.lock().await.try_send_text(Bytes::from_static(b"still-open")));
        stalled.lock().await.cleanup();
        healthy.lock().await.cleanup();
        sink.shutdown().await;
    }

    #[tokio::test]
    async fn relay_worker_shutdown_is_joined() {
        let sink = WebSocketRelaySink::new();
        assert!(!sink.is_shutdown());
        sink.shutdown().await;
        assert!(sink.is_shutdown());
    }

    #[tokio::test]
    async fn file_response_emits_partial_and_unsatisfiable_ranges() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("data.mp3");
        std::fs::write(&path, b"abcdef").unwrap();
        let partial_target = resolve_exact(
            Some(directory.path()),
            Some(Path::new("data.mp3")),
            MissingKind::NotFound,
        )
        .unwrap();
        let response = resource_response(partial_target, Some("bytes=1-3".into())).await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 1-3/6");

        let invalid_target = resolve_exact(
            Some(directory.path()),
            Some(Path::new("data.mp3")),
            MissingKind::NotFound,
        )
        .unwrap();
        let response = resource_response(invalid_target, Some("bytes=8-9".into())).await;
        assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */6");
    }

    #[tokio::test]
    async fn full_counter_html_appends_decoded_logical_parent_without_changing_original_body() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.html");
        let original = b"<main>original counter</main>";
        std::fs::write(&path, original).unwrap();
        let outcome = resolve_exact(
            Some(directory.path()),
            Some(Path::new("index.html")),
            MissingKind::NotFound,
        )
        .unwrap();

        let response =
            counter_resource_response(outcome, None, "nested%20counter/index.html").await;
        assert_eq!(response.status(), StatusCode::OK);
        let declared_length = response.headers()[header::CONTENT_LENGTH]
            .to_str()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(declared_length, body.len());
        assert!(body.starts_with(original));
        let appended = std::str::from_utf8(&body[original.len()..]).unwrap();
        assert!(appended.contains(r#"window.COUNTER_PATH="nested counter""#));
    }

    #[tokio::test]
    async fn counter_path_script_escapes_html_terminators_and_javascript_line_separators() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.html");
        let original = b"<!doctype html><title>counter</title>";
        std::fs::write(&path, original).unwrap();
        let outcome = resolve_exact(
            Some(directory.path()),
            Some(Path::new("index.html")),
            MissingKind::NotFound,
        )
        .unwrap();

        let response = counter_resource_response(
            outcome,
            None,
            "evil%3C%2Fscript%3E%26%E2%80%A8%E2%80%A9/index.html",
        )
        .await;
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert!(body.starts_with(original));
        let appended = std::str::from_utf8(&body[original.len()..]).unwrap();
        assert!(appended.contains(
            r#"window.COUNTER_PATH="evil\u003C/script\u003E\u0026\u2028\u2029""#
        ));
        assert_eq!(appended.matches("</script>").count(), 1);
    }

    #[tokio::test]
    async fn partial_counter_html_preserves_range_body_and_headers_without_metadata() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("index.html");
        std::fs::write(&path, b"abcdef").unwrap();
        let outcome = resolve_exact(
            Some(directory.path()),
            Some(Path::new("index.html")),
            MissingKind::NotFound,
        )
        .unwrap();

        let response =
            counter_resource_response(outcome, Some("bytes=1-3".into()), "counter/index.html")
                .await;
        assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
        assert_eq!(response.headers()[header::CONTENT_LENGTH], "3");
        assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 1-3/6");
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(body, Bytes::from_static(b"bcd"));
    }

    #[tokio::test]
    async fn unavailable_current_file_uses_exact_not_ready_envelope() {
        let response =
            resource_response(ResourceOutcome::Missing(MissingKind::NotReady), None).await;
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers()[header::CONTENT_TYPE], JSON);
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        assert_eq!(
            body,
            Bytes::from_static(br#"{"error":"osu is not ready/running"}"#)
        );
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
