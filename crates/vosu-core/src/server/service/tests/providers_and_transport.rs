    #[tokio::test]
    async fn dashboard_status_reports_overlay_connection_counts() {
        let (_, events) = tokio::sync::watch::channel(0);
        let state = Arc::new(NoState { events });
        let unused = Arc::new(UnusedServices);
        let delegate = ProductionRouteDelegate::new(ProductionServices::with_native_report_probe(
            Arc::new(TestOverlay::default()),
            unused.clone(), unused.clone(), unused,
            WebSocketEndpoints {
                v1: WebSocketHub::new(websocket::Schema::V1, state.clone()),
                sc: WebSocketHub::new(websocket::Schema::StreamCompanion, state.clone()),
                v2: WebSocketHub::new(websocket::Schema::V2, state.clone()),
                precise: WebSocketHub::new(websocket::Schema::V2Precise, state.clone()),
                commands: WebSocketHub::new(websocket::Schema::V1, state.clone()),
            },
        ));
        let _first = delegate.connections.register(Some("counter".into()));
        let _second = delegate.connections.register(Some("counter".into()));
        let status = bounded!(delegate.dispatch(Endpoint::DashboardStatus, Request::builder().uri("/api/dashboard/status").body(Body::empty()).unwrap(), Bytes::new()));
        assert_eq!(status.status(), StatusCode::OK);
        let status: Value = serde_json::from_slice(&bounded!(axum::body::to_bytes(status.into_body(), 16384)).unwrap()).unwrap();
        assert_eq!(status["connections"]["counter"], 2);
        assert_eq!(status["instances"], json!([]));
    }

    /// A7.3: nothing the dashboard serves can turn the dashboard off.
    #[tokio::test]
    async fn settings_save_rejects_the_dashboard_switch_before_saving() {
        let (_, events) = tokio::sync::watch::channel(0);
        let state = Arc::new(NoState { events });
        let unused = Arc::new(UnusedServices);
        let delegate = ProductionRouteDelegate::new(ProductionServices::with_native_report_probe(
            Arc::new(TestOverlay::default()),
            unused.clone(), unused.clone(), unused,
            WebSocketEndpoints {
                v1: WebSocketHub::new(websocket::Schema::V1, state.clone()),
                sc: WebSocketHub::new(websocket::Schema::StreamCompanion, state.clone()),
                v2: WebSocketHub::new(websocket::Schema::V2, state.clone()),
                precise: WebSocketHub::new(websocket::Schema::V2Precise, state.clone()),
                commands: WebSocketHub::new(websocket::Schema::V1, state.clone()),
            },
        ));
        let save = |body: &'static str| delegate.dispatch(
            Endpoint::SettingsSave,
            Request::builder().method("POST").uri("/api/settingsSave").body(Body::empty()).unwrap(),
            Bytes::from_static(body.as_bytes()),
        );
        // The fixture's save always succeeds, so a 400 means it never ran.
        assert_eq!(bounded!(save(r#"{"DEBUG_LOG":true}"#)).status(), StatusCode::OK);
        for body in [r#"{"ENABLE_DASHBOARD":false}"#, r#"{"ENABLE_DASHBOARD":true}"#, r#"{"DEBUG_LOG":true,"ENABLE_DASHBOARD":null}"#] {
            let response = bounded!(save(body));
            assert_eq!(response.status(), StatusCode::BAD_REQUEST, "{body}");
            let error: Value = serde_json::from_slice(&bounded!(axum::body::to_bytes(response.into_body(), 1024)).unwrap()).unwrap();
            assert_eq!(error, json!({"error": "ENABLE_DASHBOARD can only be changed in vosu.toml"}));
        }
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
        assert_eq!(plus, "counter name");

        let encoded_space = decoded_query_value("l=counter%2520name", "l").unwrap();
        assert_eq!(encoded_space, "counter%20name");

        let local_id = 1;
        let ids = (1..=5)
            .map(|namespace| namespaced_client_id(namespace, local_id).unwrap())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(ids.len(), 5);
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
