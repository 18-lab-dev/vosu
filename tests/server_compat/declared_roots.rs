use super::*;
#[path = "../../crates/vosu-adapter-upstream/tests/support/mod.rs"]
mod support;
use support::Case;
use vosu_core::runtime::ListenerOwner;
use vosu_core::server::service::SurfaceRouter;
use vosu_surface::{requester::BundledRequester, routes::RegistrationLease};

struct NoState;
impl StateJsonProvider for NoState {
    fn cached_json(&self, _: StateRoute) -> ProviderFuture<'_, CachedJson> {
        Box::pin(async { CachedJson::NotReady })
    }
}
impl StateProvider for NoState {
    fn poll_rate(&self, _: Schema) -> Duration {
        Duration::from_millis(100)
    }
    fn subscribe(&self, _: Schema) -> watch::Receiver<u64> {
        watch::channel(0).1
    }
    fn capture(&self, _: Schema) -> Option<Arc<dyn StateSnapshot>> {
        None
    }
}

fn providers(
    case: &Case,
    upstream: Arc<dyn BundledRequester>,
) -> (RouteProviders, RegistrationLease) {
    let state = Arc::new(NoState);
    let [entries, _, _] = vosu::BUNDLE.hosts.adapter_routes(
        std::array::from_fn(|_| state.clone() as Arc<dyn StateJsonProvider>),
        [
            Schema::V1,
            Schema::StreamCompanion,
            Schema::V2,
            Schema::V2Precise,
        ]
        .map(|schema| WebSocketHub::new(schema, state.clone())),
        upstream,
    );
    let lease = case.requester.register_routes(entries).unwrap();
    (
        RouteProviders {
            authorization: Arc::new(AuthorizationPolicy::new("", "127.0.0.1")),
            delegate: Arc::new(SurfaceRouter::new(
                case.host.clone(),
                case.requester.clone(),
            )),
        },
        lease,
    )
}
fn request(method: Method, path: &str, headers: &[(&str, &str)]) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header("Host", "127.0.0.1");
    for (name, value) in headers {
        builder = builder.header(*name, *value);
    }
    let mut request = builder.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(PeerAddress("127.0.0.1:12345".parse().unwrap())));
    request
}
fn dispatch(case: &Case, method: Method, path: &str, headers: &[(&str, &str)]) -> Response {
    let (providers, _lease) = providers(case, case.requester.clone());
    case.run(routes::dispatch(
        State(providers),
        request(method, path, headers),
    ))
}
fn body(case: &Case, response: Response) -> Bytes {
    case.run(to_bytes(response.into_body(), 1024 * 1024))
        .unwrap()
}
#[test]
fn declared_root_http_routes_stream_exact_bytes_with_legacy_headers_and_errors() {
    let case = Case::new(false);
    for (path, expected) in [
        ("/Songs/Set/bg.jpg", b"stable-bg".as_slice()),
        ("/files/beatmap/Set/bg.jpg", b"stable-bg"),
        ("/backgroundImage", b"stable-bg"),
        ("/files/beatmap/background", b"stable-bg"),
        ("/files/beatmap/audio", b"0123456789"),
        ("/files/beatmap/file", support::MAP),
        ("/files/skin/subdir/mixed.png", b"skin-exact"),
    ] {
        let response = dispatch(&case, Method::GET, path, &[]);
        assert_eq!(response.status(), StatusCode::OK, "{path}");
        assert_eq!(
            response.headers()[header::CONTENT_SECURITY_POLICY],
            "sandbox"
        );
        assert_eq!(body(&case, response), expected, "{path}");
    }
    for path in [
        "/files/skin/",
        "/files/beatmap/../outside",
        "/Songs/%2e%2e/outside",
        "/Songs/%",
        "/Songs/C:/outside",
        "/Songs/missing",
    ] {
        let response = dispatch(&case, Method::GET, path, &[]);
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert_eq!(body(&case, response), "Not Found", "{path}");
    }
    assert_eq!(
        dispatch(&case, Method::POST, "/Songs/Set/bg.jpg", &[]).status(),
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        dispatch(&case, Method::GET, "/Song/Set/bg.jpg", &[]).status(),
        StatusCode::NOT_FOUND
    );
}
#[test]
fn declared_root_http_range_head_validators_and_directory_contract() {
    let case = Case::new(false);
    let response = dispatch(
        &case,
        Method::GET,
        "/files/beatmap/audio",
        &[("Range", "bytes=2-5")],
    );
    assert_eq!(response.status(), StatusCode::PARTIAL_CONTENT);
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes 2-5/10");
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "4");
    assert_eq!(body(&case, response), "2345");
    let response = dispatch(
        &case,
        Method::GET,
        "/files/beatmap/audio",
        &[("Range", "bytes=20-30")],
    );
    assert_eq!(response.status(), StatusCode::RANGE_NOT_SATISFIABLE);
    assert_eq!(response.headers()[header::CONTENT_RANGE], "bytes */10");
    assert!(body(&case, response).is_empty());
    let response = dispatch(
        &case,
        Method::HEAD,
        "/files/beatmap/audio",
        &[("Range", "bytes=2-5")],
    );
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::CONTENT_LENGTH], "10");
    let etag = response.headers()[header::ETAG]
        .to_str()
        .unwrap()
        .to_owned();
    let modified = response.headers()[header::LAST_MODIFIED]
        .to_str()
        .unwrap()
        .to_owned();
    assert!(body(&case, response).is_empty());
    for headers in [
        [("If-None-Match", etag.as_str())],
        [("If-Modified-Since", modified.as_str())],
    ] {
        let response = dispatch(&case, Method::GET, "/files/beatmap/audio", &headers);
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert!(body(&case, response).is_empty());
    }
    assert!(!etag.starts_with("W/"));
    for condition in ["*".to_owned(), format!("\"other\", W/{etag}")] {
        let response = dispatch(
            &case,
            Method::HEAD,
            "/files/beatmap/audio",
            &[("If-None-Match", &condition)],
        );
        assert_eq!(response.status(), StatusCode::NOT_MODIFIED);
        assert_eq!(response.headers()[header::ETAG], etag);
        assert!(body(&case, response).is_empty());
    }
    let response = dispatch(
        &case,
        Method::GET,
        "/files/beatmap/audio",
        &[
            ("If-None-Match", "\"different\""),
            ("If-Modified-Since", &modified),
        ],
    );
    assert_eq!(
        response.status(),
        StatusCode::OK,
        "ETag condition takes precedence"
    );
    assert_eq!(response.headers()[header::CACHE_CONTROL], "no-cache");
    assert_eq!(body(&case, response), "0123456789");
    let response = dispatch(&case, Method::GET, "/Songs/Set?x=1", &[]);
    assert_eq!(response.status(), StatusCode::MOVED_PERMANENTLY);
    assert_eq!(response.headers()[header::LOCATION], "/Songs/Set/?x=1");
    let response = dispatch(&case, Method::GET, "/Songs/Set/", &[]);
    assert_eq!(response.status(), StatusCode::OK);
    assert!(
        String::from_utf8(body(&case, response).to_vec())
            .unwrap()
            .contains("map.osu")
    );
}
#[test]
fn declared_root_http_lazer_skin_404_and_calculation_security_divergence() {
    let case = Case::new(true);
    let response = dispatch(&case, Method::GET, "/files/skin/SubDir/MiXeD.PNG", &[]);
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    assert_eq!(body(&case, response), "Not Found");
    let response = dispatch(
        &case,
        Method::GET,
        "/api/calculate/pp?path=Set%2Fmap.osu&mods=HD",
        &[],
    );
    assert_eq!(response.status(), StatusCode::OK);
    let outside = tempfile::tempdir().unwrap();
    let path = outside.path().join("secret.osu");
    std::fs::write(&path, support::MAP).unwrap();
    let query = url::form_urlencoded::Serializer::new(String::new())
        .append_pair("path", path.to_str().unwrap())
        .finish();
    let response = dispatch(
        &case,
        Method::GET,
        &format!("/api/calculate/pp?{query}"),
        &[],
    );
    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let json: Value = serde_json::from_slice(&body(&case, response)).unwrap();
    assert!(json["error"].as_str().unwrap().contains("declared roots"));
}

#[test]
fn mounted_calculation_preserves_number_spelling_mod_inputs_and_client_errors() {
    let case = Case::new(false);
    let (providers, _lease) = providers(&case, case.requester.clone());
    case.run(async {
        let server = ListenerOwner::start(
            providers,
            ListenTarget {
                host: "127.0.0.1".into(),
                port: 0,
            },
        )
        .await
        .unwrap();
        let address = server.state().effective.unwrap().address;
        let base = format!("http://{address}/api/calculate/pp");
        let client = reqwest::Client::builder().no_proxy().build().unwrap();
        let get = |query, expected_status| {
            let request = client.get(format!("{base}?{query}"));
            async move {
                let response = request.send().await.unwrap();
                assert_eq!(response.status(), expected_status, "{query}");
                let length = response.headers()[header::CONTENT_LENGTH].clone();
                let bytes = response.bytes().await.unwrap();
                assert_eq!(length, bytes.len().to_string(), "{query}");
                bytes
            }
        };
        let ok = StatusCode::OK;
        let attributes = get("ar=9.5", ok).await;
        let text = std::str::from_utf8(&attributes).unwrap();
        assert!(text.contains(r#""hp":5,"#), "{text}");
        assert!(text.contains(r#""ar":9.5,"#), "{text}");

        assert_eq!(get("mods=HDDT", ok).await, get("mods=72", ok).await);
        let settings = get(
            "mods=%5B%7B%22acronym%22%3A%22DT%22%2C%22settings%22%3A%7B%22speed_change%22%3A1.25%7D%7D%5D",
            ok,
        )
        .await;
        assert_eq!(settings, get("mods=64&clockRate=1.25", ok).await);
        assert_ne!(settings, get("mods=64", ok).await);
        get("acc=98.5&clockRate=1&combo=2&mode=0", ok).await;
        for (query, expected) in [
            ("acc=abc", r#"{"error":"invalid acc"}"#),
            (
                "clockRate=0",
                r#"{"error":"clockRate must be between 0.01 and 100"}"#,
            ),
            ("mods=H", r#"{"error":"invalid mods acronyms"}"#),
            ("combo=-1", r#"{"error":"invalid combo"}"#),
            ("mode=9", r#"{"error":"mode must be 0, 1, 2, or 3"}"#),
        ] {
            assert_eq!(get(query, StatusCode::BAD_REQUEST).await, expected);
        }
        let post = client.post(&base).send().await.unwrap();
        assert_eq!(post.status(), StatusCode::NOT_FOUND);
        assert_eq!(
            post.headers()[header::CONTENT_TYPE],
            "text/plain; charset=utf-8"
        );
        assert_eq!(post.text().await.unwrap(), "Not Found");
        drop(client);
        server.shutdown().await;
    });
}

#[test]
fn unprovisioned_declared_provider_fails_closed_with_legacy_not_ready_envelope() {
    let case = Case::new(false);
    for path in ["/Songs/Set/bg.jpg", "/api/calculate/pp?path=/outside.osu"] {
        // The game fixture is not granted the upstream adapter's file selectors.
        let upstream = case
            .host
            .surface_requester(vosu_core::plugin::PluginInstanceId::new(1))
            .unwrap();
        let (providers, _lease) = providers(&case, upstream);
        let response = case.run(routes::dispatch(
            State(providers),
            request(Method::GET, path, &[]),
        ));
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let json: Value = serde_json::from_slice(&body(&case, response)).unwrap();
        assert_eq!(json, json!({"error":"osu is not ready/running"}));
    }
}

#[test]
fn registered_game_html_is_sandboxed_for_get_head_and_directory_responses() {
    let case = Case::new(false);
    let html = b"<html><script>parent.alert(1)</script></html>";
    for relative in ["songs/Set/page.html", "skins/Skin/page.html"] {
        std::fs::write(case.root.path().join(relative), html).unwrap();
    }
    let response = dispatch(&case, Method::GET, "/Songs/Set/page.html", &[]);
    assert_eq!(
        response.headers()[header::CONTENT_SECURITY_POLICY],
        "sandbox"
    );
    assert_eq!(body(&case, response), html.as_slice());
    for path in [
        "/files/beatmap/Set/page.html",
        "/files/skin/page.html",
        "/files/beatmap/Set/",
    ] {
        for method in [Method::GET, Method::HEAD] {
            let response = dispatch(&case, method.clone(), path, &[]);
            assert_eq!(response.status(), StatusCode::OK, "{method} {path}");
            assert_eq!(
                response.headers()[header::CONTENT_SECURITY_POLICY],
                "sandbox"
            );
            assert_eq!(
                response.headers()[header::CONTENT_TYPE],
                "text/html; charset=utf-8"
            );
            if method == Method::HEAD {
                assert!(body(&case, response).is_empty());
            }
        }
    }
}

#[test]
fn registered_lazer_files_keep_the_store_hash_as_the_strong_validator() {
    let case = Case::new(true);
    let hash = "b".repeat(64);
    let path = format!("b/bb/{hash}");
    std::fs::create_dir_all(case.root.path().join("files/b/bb")).unwrap();
    std::fs::write(case.root.path().join("files").join(&path), b"data").unwrap();
    let response = dispatch(&case, Method::GET, &format!("/files/beatmap/{path}"), &[]);
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[header::ETAG], format!("\"{hash}\""));
    assert_eq!(body(&case, response), "data");
}
