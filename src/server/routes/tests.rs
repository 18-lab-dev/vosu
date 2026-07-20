use super::*;

#[test]
fn exact_methods_never_add_head_or_options() {
    for path in ["/json", "/api/calculate/pp", "/", "/ws"] {
        assert!(classify(&Method::GET, path).is_some());
        assert_eq!(classify(&Method::HEAD, path), None);
        assert_eq!(classify(&Method::OPTIONS, path), None);
        assert_eq!(classify(&Method::PUT, path), None);
    }
    assert_eq!(classify(&Method::GET, "/api/settingsSave"), None);
    assert_eq!(
        classify(&Method::POST, "/api/settingsSave"),
        Some(Endpoint::SettingsSave)
    );
}
#[test]
fn counter_root_open_is_an_exact_get_route() {
    assert_eq!(
        classify(&Method::GET, "/api/counters/open-root"),
        Some(Endpoint::CounterOpenRoot)
    );
    assert_eq!(classify(&Method::POST, "/api/counters/open-root"), None);
    assert_eq!(classify(&Method::GET, "/api/counters/open-root/"), None);
    assert_eq!(
        classify(&Method::GET, "/api/counters/open/counter"),
        Some(Endpoint::CounterOpen)
    );
}

#[test]
fn only_six_exact_websocket_paths_reach_upgrade_delegate() {
    let paths = [
        ("/ws", Endpoint::WsV1),
        ("/tokens", Endpoint::WsTokens),
        ("/websocket/v2", Endpoint::WsV2),
        ("/websocket/v2/precise", Endpoint::WsV2Precise),
        ("/websocket/v2/subscribe", Endpoint::WsV2Subscribe),
        ("/websocket/commands", Endpoint::WsCommands),
    ];
    for (path, endpoint) in paths {
        assert_eq!(classify(&Method::GET, path), Some(endpoint));
        assert_eq!(classify(&Method::HEAD, path), None);
    }
    for path in [
        "/ws/",
        "/ws/extra",
        "/tokens/",
        "/websocket",
        "/websocket/",
        "/websocket/v2/near-miss",
        "/websocket/v2/subscribe/",
        "/websocket/commands/extra",
    ] {
        assert_eq!(classify(&Method::GET, path), None, "{path}");
    }
}

#[tokio::test]
async fn websocket_upgrade_headers_never_fall_through_to_http_routes() {
    for path in [
        "/",
        "/available",
        "/assets/app.js",
        "/api/counters/search/foo",
    ] {
        let mut request = make_request(Method::GET, path, Body::empty());
        request.headers_mut().insert(
            header::CONNECTION,
            HeaderValue::from_static("keep-alive, Upgrade"),
        );
        request
            .headers_mut()
            .insert(header::UPGRADE, HeaderValue::from_static("websocket"));
        let response = dispatch(
            State(providers(CachedJson::Ready(Bytes::from_static(b"{}")))),
            request,
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body(response).await, "Not Found", "{path}");
    }
}

#[test]
fn parses_every_pp_input_and_both_structured_mod_shapes() {
    let raw = "path=%2Ftmp%2Fa.osu&lazer=true&mode=3&ar=9.1&cs=4&hp=6&od=8&clockRate=1.5&passedObjects=10&combo=20&nMisses=1&n100=2&n300=3&n50=4&nGeki=5&nKatu=6&mods=%5B%7B%22acronym%22%3A%22DT%22%7D%5D&acc=98.5&sliderEndHits=7&smallTickHits=8&largeTickHits=9&hitresultPriority=1";
    let q = PpQuery::parse(Some(raw)).unwrap();
    assert_eq!(q.path.as_deref(), Some("/tmp/a.osu"));
    assert_eq!(q.lazer, Some(true));
    assert_eq!(q.mode, Some(3));
    assert_eq!(
        (q.ar, q.cs, q.hp, q.od, q.clock_rate),
        (Some(9.1), Some(4.0), Some(6.0), Some(8.0), Some(1.5))
    );
    assert_eq!(
        (
            q.passed_objects,
            q.combo,
            q.n_misses,
            q.n100,
            q.n300,
            q.n50,
            q.n_geki,
            q.n_katu
        ),
        (
            Some(10),
            Some(20),
            Some(1),
            Some(2),
            Some(3),
            Some(4),
            Some(5),
            Some(6)
        )
    );
    assert!(matches!(q.mods, Some(ModsInput::Json(Value::Array(_)))));
    assert_eq!(q.accuracy, Some(98.5));
    assert_eq!(
        (q.slider_end_hits, q.small_tick_hits, q.large_tick_hits),
        (Some(7), Some(8), Some(9))
    );
    assert_eq!(q.hitresult_priority, Some(1));
    assert!(matches!(
        PpQuery::parse(Some("mods=%7B%22acronym%22%3A%22HD%22%7D"))
            .unwrap()
            .mods,
        Some(ModsInput::Json(Value::Object(_)))
    ));
}

#[test]
fn count_inputs_use_bounded_javascript_numeric_coercion() {
    let query = PpQuery::parse(Some(
        "passedObjects=10.0&combo=1e2&nMisses=%2B3&n100=0x10&n300=0b11&n50=0o7&nGeki=&nKatu=4.0&sliderEndHits=5e1&smallTickHits=6.0&largeTickHits=7",
    )).unwrap();
    assert_eq!(
        (
            query.passed_objects,
            query.combo,
            query.n_misses,
            query.n100,
            query.n300,
            query.n50
        ),
        (Some(10), Some(100), Some(3), Some(16), Some(3), Some(7)),
    );
    assert_eq!(
        (
            query.n_geki,
            query.n_katu,
            query.slider_end_hits,
            query.small_tick_hits,
            query.large_tick_hits
        ),
        (Some(0), Some(4), Some(50), Some(6), Some(7)),
    );

    for value in ["10.5", "-1", "NaN", "Infinity", "4294967296", "1e100"] {
        assert!(
            PpQuery::parse(Some(&format!("combo={value}"))).is_err(),
            "{value}"
        );
    }
    assert_eq!(
        PpQuery::parse(Some("combo=4294967295")).unwrap().combo,
        Some(u32::MAX),
    );
}

#[test]
fn cors_is_applied_to_error_envelopes() {
    for response in [
        not_found(),
        not_ready(),
        json(
            StatusCode::PAYLOAD_TOO_LARGE,
            error_json("payload too large"),
        ),
    ] {
        let response = cors(response);
        assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    }
}

struct StateProvider(CachedJson);
impl StateJsonProvider for StateProvider {
    fn cached_json(&self, _: StateRoute) -> ProviderFuture<'_, CachedJson> {
        let result = self.0.clone();
        Box::pin(async move { result })
    }
}
struct CalculationProvider;
impl CalculatePpProvider for CalculationProvider {
    fn calculate_pp(&self, _: PpQuery) -> ProviderFuture<'_, CalculateResult> {
        Box::pin(async { CalculateResult::NotReady })
    }
}
struct Delegate;
impl RouteDelegate for Delegate {
    fn handle(&self, _: Endpoint, _: Request, _: Bytes) -> ProviderFuture<'_, Response> {
        Box::pin(async { Response::new(Body::empty()) })
    }
}
fn providers(state: CachedJson) -> RouteProviders {
    RouteProviders {
        authorization: Arc::new(AuthorizationPolicy::new(
            "198.51.100.7,localhost,absolute",
            "dashboard.local",
        )),
        state: Arc::new(StateProvider(state)),
        calculation: Arc::new(CalculationProvider),
        delegate: Arc::new(Delegate),
    }
}
fn make_request(method: Method, uri: &str, body: Body) -> Request {
    Request::builder()
        .method(method)
        .uri(uri)
        .extension(ConnectInfo(
            "127.0.0.1:41234".parse::<SocketAddr>().unwrap(),
        ))
        .body(body)
        .unwrap()
}
async fn body(response: Response) -> Bytes {
    to_bytes(response.into_body(), usize::MAX).await.unwrap()
}

#[tokio::test]
async fn dispatch_has_exact_not_found_and_not_ready_envelopes() {
    for method in [Method::HEAD, Method::OPTIONS, Method::POST] {
        let request = make_request(method, "/json", Body::empty());
        let response = dispatch(State(providers(CachedJson::NotReady)), request).await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        assert_eq!(body(response).await, "Not Found");
    }
    let request = make_request(Method::GET, "/json", Body::empty());
    let response = dispatch(State(providers(CachedJson::NotReady)), request).await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(
        body(response).await,
        r#"{"error":"osu is not ready/running"}"#
    );

    let request = make_request(Method::GET, "/json/v2", Body::empty());
    let response = dispatch(
        State(providers(CachedJson::Failed {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            body: Bytes::from_static(br#"{"error":"builder failed"}"#),
        })),
        request,
    )
    .await;
    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert_eq!(body(response).await, r#"{"error":"builder failed"}"#);
}

#[tokio::test]
async fn post_body_is_rejected_beyond_one_mibibyte() {
    let request = make_request(
        Method::POST,
        "/api/settingsSave",
        Body::from(vec![0_u8; MAX_POST_BODY + 1]),
    );
    let response = dispatch(State(providers(CachedJson::NotReady)), request).await;
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
    assert_eq!(body(response).await, r#"{"error":"payload too large"}"#);
}

#[tokio::test]
async fn real_request_authorization_precedes_http_and_websocket_delegation() {
    struct Case {
        peer: &'static str,
        origin: Option<&'static str>,
        referer: Option<&'static str>,
        host: Option<&'static str>,
        expected: StatusCode,
    }
    let cases = [
        Case {
            peer: "127.0.0.1:1",
            origin: None,
            referer: None,
            host: None,
            expected: StatusCode::OK,
        },
        Case {
            peer: "198.51.100.7:1",
            origin: None,
            referer: None,
            host: None,
            expected: StatusCode::OK,
        },
        Case {
            peer: "127.0.0.1:1",
            origin: Some("http://dashboard.local"),
            referer: None,
            host: Some("dashboard.local"),
            expected: StatusCode::OK,
        },
        Case {
            peer: "127.0.0.1:1",
            origin: Some("http://evil.example"),
            referer: None,
            host: Some("dashboard.local"),
            expected: StatusCode::FORBIDDEN,
        },
        Case {
            peer: "127.0.0.1:1",
            origin: Some("http://localhost:24050"),
            referer: None,
            host: Some("forwarder.internal:8080"),
            expected: StatusCode::OK,
        },
        Case {
            peer: "203.0.113.9:1",
            origin: Some("http://dashboard.local"),
            referer: None,
            host: Some("dashboard.local"),
            expected: StatusCode::FORBIDDEN,
        },
    ];
    for case in cases {
        let mut builder = Request::builder()
            .method(Method::GET)
            .uri("/json")
            .extension(ConnectInfo(case.peer.parse::<SocketAddr>().unwrap()));
        if let Some(value) = case.origin {
            builder = builder.header(header::ORIGIN, value);
        }
        if let Some(value) = case.referer {
            builder = builder.header(header::REFERER, value);
        }
        if let Some(value) = case.host {
            builder = builder.header(header::HOST, value);
        }
        let response = dispatch(
            State(providers(CachedJson::Ready(Bytes::from_static(b"{}")))),
            builder.body(Body::empty()).unwrap(),
        )
        .await;
        assert_eq!(
            response.status(),
            case.expected,
            "{case_peer}",
            case_peer = case.peer
        );
        if case.expected == StatusCode::FORBIDDEN {
            assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
            assert_eq!(body(response).await, "Not Found");
        }
    }

    let response = dispatch(
        State(providers(CachedJson::Ready(Bytes::from_static(b"{}")))),
        Request::builder()
            .method(Method::GET)
            .uri("/websocket/v2")
            .extension(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()))
            .header(header::ORIGIN, "http://evil.example")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");

    for path in ["/ws/", "/tokens/", "/websocket", "/websocket/v2/near-miss"] {
        let response = dispatch(
            State(providers(CachedJson::Ready(Bytes::from_static(b"{}")))),
            Request::builder()
                .method(Method::GET)
                .uri(path)
                .extension(ConnectInfo("127.0.0.1:1".parse::<SocketAddr>().unwrap()))
                .body(Body::empty())
                .unwrap(),
        )
        .await;
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
        assert_eq!(body(response).await, "Not Found");
    }
}

#[test]
fn response_flush_barrier_remembers_disconnect_before_arm() {
    let barrier = ResponseFlushBarrier::default();
    barrier.disconnect();
    let fired = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let observed = Arc::clone(&fired);
    barrier.arm(Box::new(move || {
        observed.store(true, std::sync::atomic::Ordering::SeqCst);
    }));
    assert!(fired.load(std::sync::atomic::Ordering::SeqCst));
}

#[test]
fn response_flush_barrier_preserves_keep_alive_and_pipelined_callbacks() {
    let barrier = ResponseFlushBarrier::default();
    let fired = Arc::new(std::sync::atomic::AtomicUsize::new(0));

    for expected in 1..=2 {
        let observed = Arc::clone(&fired);
        barrier.arm(Box::new(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
        assert_eq!(
            fired.load(std::sync::atomic::Ordering::SeqCst),
            expected - 1
        );
        barrier.flushed();
        assert_eq!(fired.load(std::sync::atomic::Ordering::SeqCst), expected);
    }

    for _ in 0..2 {
        let observed = Arc::clone(&fired);
        barrier.arm(Box::new(move || {
            observed.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }));
    }
    assert_eq!(fired.load(std::sync::atomic::Ordering::SeqCst), 2);
    barrier.flushed();
    assert_eq!(fired.load(std::sync::atomic::Ordering::SeqCst), 4);

    let registry = ResponseFlushRegistry::default();
    let peer: SocketAddr = "127.0.0.1:24050".parse().unwrap();
    let barrier = Arc::new(barrier);
    registry.register(peer, &barrier);
    assert!(Arc::ptr_eq(&registry.barrier(peer).unwrap(), &barrier));
    registry.remove(peer);
    assert!(registry.barrier(peer).is_none());
}
