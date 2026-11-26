//! M7 N1: the `/api/ingame` tombstone (M7-G01, M7-G02) and the D20 status
//! read over core's recorder, through the real mounted overlay listener.
use super::mounted::*;
use super::*;
use std::time::Instant;
use vosu_core::removed_routes::{INGAME_WARNING, RemovedFeature, TTL};

const SENTINEL: &str = "n1-sentinel-7c4f";

/// `GET /api/dashboard/status` as core's handler answers it.
fn status(m: &Mounted) -> Value {
    m.block_on(async {
        let response = m
            .delegate
            .handle(
                Endpoint::DashboardStatus,
                Request::builder()
                    .uri("/api/dashboard/status")
                    .body(Body::empty())
                    .unwrap(),
                Bytes::new(),
            )
            .await;
        assert_eq!(response.status(), StatusCode::OK);
        serde_json::from_slice(&to_bytes(response.into_body(), 1 << 20).await.unwrap()).unwrap()
    })
}

fn notices<'a>(status: &'a Value, feature: &str) -> Vec<&'a Value> {
    status["removed_routes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|notice| notice["feature"] == feature)
        .collect()
}

fn ingame_attempts(m: &Mounted, overlay: Option<&str>) -> u64 {
    notices(&status(m), "ingame-page")
        .iter()
        .filter(|notice| notice["overlay"]["id"].as_str() == overlay)
        .map(|notice| notice["attempts"].as_u64().unwrap())
        .sum()
}

/// A request as the overlay listener receives it, dispatched on this thread.
fn request(
    m: &Mounted,
    method: Method,
    uri: &str,
    headers: &[(header::HeaderName, &str)],
) -> Request<Body> {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, format!("127.0.0.1:{}", m.port));
    for (name, value) in headers {
        builder = builder.header(name, *value);
    }
    let mut request = builder.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(PeerAddress(SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        )))));
    request
}

fn comparable(response: &reqwest::Response) -> Vec<(String, String)> {
    let mut headers: Vec<_> = response
        .headers()
        .iter()
        .filter(|(name, _)| *name != header::DATE)
        .map(|(name, value)| (name.to_string(), value.to_str().unwrap().to_owned()))
        .collect();
    headers.sort();
    headers
}

#[test]
fn m7_g01_api_ingame_is_the_ordinary_404_and_only_the_exact_path_is_recorded() {
    let m = Mounted::new(None);
    let mut expected = 0;
    m.block_on(async {
        for method in [
            Method::GET,
            Method::HEAD,
            Method::POST,
            Method::PUT,
            Method::DELETE,
            Method::OPTIONS,
            Method::PATCH,
        ] {
            for accept in [None, Some("text/html")] {
                let send = |path: &str| {
                    let mut builder = m
                        .client
                        .request(method.clone(), format!("{}{path}", m.base));
                    if let Some(accept) = accept {
                        builder = builder.header(header::ACCEPT, accept);
                    }
                    builder.send()
                };
                let removed = send(&format!("/api/ingame?{SENTINEL}=1")).await.unwrap();
                let unknown = send("/api/definitely-unknown").await.unwrap();
                assert_eq!(
                    removed.status(),
                    StatusCode::NOT_FOUND,
                    "{method} {accept:?}"
                );
                assert_eq!(
                    comparable(&removed),
                    comparable(&unknown),
                    "{method} {accept:?}"
                );
                for name in ["deprecation", "link", "sunset", "location", "upgrade"] {
                    assert!(!removed.headers().contains_key(name), "{method} {name}");
                }
                assert_eq!(
                    removed.bytes().await.unwrap(),
                    unknown.bytes().await.unwrap()
                );
                expected += 1;
            }
        }
        // An upgrade request is refused with the same 404 and never upgrades.
        assert_eq!(
            m.ws("/api/ingame", None).await.err(),
            Some(StatusCode::NOT_FOUND)
        );
        expected += 1;
    });
    // Positive control: every exact-path request above was counted.
    assert_eq!(ingame_attempts(&m, None), expected);
    m.block_on(async {
        for path in [
            "/api/ingame/",
            "/API/ingame",
            "/api/Ingame",
            "//api//ingame",
            "/api/ingame%2F",
            "/api/ingamex",
            "/api/ingame/x",
            "/ingame",
        ] {
            let response = m
                .client
                .get(format!("{}{path}", m.base))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    });
    assert_eq!(
        ingame_attempts(&m, None),
        expected,
        "near misses are not recorded"
    );

    // The dashboard listener has no tombstone: an unknown-route 404 that
    // records nothing.
    let origins = Arc::new(
        vosu::server::origins::Origins::new(m.origin.clone(), "http://127.0.0.1:24051".into())
            .unwrap(),
    );
    let mut providers = m.providers.clone();
    providers.authorization = Arc::new(vosu::server::origins::OriginAuthorizer {
        inner: Arc::new(AuthorizationPolicy::new("127.0.0.1,localhost", "127.0.0.1")),
        origins,
        dashboard: true,
    });
    m.block_on(async {
        for path in ["/api/ingame", "/api/definitely-unknown"] {
            let mut request = request(&m, Method::GET, path, &[]);
            request
                .headers_mut()
                .insert(header::HOST, "127.0.0.1:24051".parse().unwrap());
            let response = routes::dispatch(State(providers.clone()), request).await;
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            assert_eq!(
                to_bytes(response.into_body(), 1024).await.unwrap(),
                "Not Found"
            );
        }
    });
    assert_eq!(
        ingame_attempts(&m, None),
        expected,
        "the dashboard listener records nothing"
    );
    m.shutdown();
}

#[derive(Clone, Default)]
struct Logs(Arc<std::sync::Mutex<Vec<u8>>>);
impl Write for Logs {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Logs {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

#[test]
fn m7_g02_one_warning_unknown_bucket_attribution_privacy_and_expiry() {
    let m = Mounted::new(None);
    let logs = Logs::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    // Every dispatch below runs on this thread, inside `block_on`.
    let _logging = tracing::subscriber::set_default(subscriber);
    let first = status(&m);
    assert!(
        first.get("ingame").is_none(),
        "DashboardStatus has no ingame field"
    );
    assert!(
        notices(&first, "ingame-page").is_empty(),
        "no notice before any request"
    );

    let uri = format!("/api/ingame?{SENTINEL}=1");
    // A same-origin Referer naming no installed overlay stays unknown.
    let referer = format!("{}/not-installed/page?ref={SENTINEL}", m.base);
    let concurrent = m.block_on(futures_util::future::join_all((0..8).map(|_| {
        routes::dispatch(
            State(m.providers.clone()),
            request(
                &m,
                Method::GET,
                &uri,
                &[(header::REFERER, referer.as_str())],
            ),
        )
    })));
    assert!(
        concurrent
            .iter()
            .all(|response| response.status() == StatusCode::NOT_FOUND)
    );
    for _ in 0..3 {
        let response = m.block_on(routes::dispatch(
            State(m.providers.clone()),
            request(&m, Method::POST, &uri, &[]),
        ));
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert_eq!(text.matches(INGAME_WARNING).count(), 1, "{text}");
    assert!(
        text.lines()
            .any(|line| line.contains("WARN") && line.contains(INGAME_WARNING)),
        "{text}"
    );
    assert!(
        !text.contains(SENTINEL) && !text.contains("not-installed"),
        "{text}"
    );

    let read = status(&m);
    let bucket = notices(&read, "ingame-page");
    assert_eq!(bucket.len(), 1);
    assert_eq!(bucket[0]["overlay"], Value::Null);
    assert_eq!(bucket[0]["attempts"], 11);
    assert!(
        bucket[0]["last_seen_seconds_ago"].as_u64().unwrap()
            <= bucket[0]["first_seen_seconds_ago"].as_u64().unwrap()
    );
    let serialized = read.to_string();
    assert!(
        !serialized.contains(SENTINEL)
            && !serialized.contains("not-installed")
            && !serialized.contains("/api/ingame")
    );

    // A validated `l` identity is attributed; a same-origin Origin alone is not.
    for (uri, origin) in [
        ("/api/ingame?l=counter", None),
        ("/api/ingame", Some(m.origin.as_str())),
    ] {
        let headers: Vec<_> = origin
            .map(|origin| (header::ORIGIN, origin))
            .into_iter()
            .collect();
        let response = m.block_on(routes::dispatch(
            State(m.providers.clone()),
            request(&m, Method::GET, uri, &headers),
        ));
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }
    assert_eq!(ingame_attempts(&m, Some("counter")), 1);
    assert_eq!(ingame_attempts(&m, None), 12);
    let read = status(&m);
    let known = notices(&read, "ingame-page")
        .into_iter()
        .find(|notice| !notice["overlay"].is_null())
        .unwrap();
    assert_eq!(known["overlay"]["id"], "counter");
    assert!(known["overlay"]["name"].is_string());
    drop(_logging);
    assert_eq!(
        String::from_utf8(logs.0.lock().unwrap().clone())
            .unwrap()
            .matches(INGAME_WARNING)
            .count(),
        1
    );

    // The recorder's own inactivity TTL, read with an injected clock.
    let now = Instant::now();
    let installed = |_: &str| true;
    assert!(
        m.removed
            .snapshot(now + TTL - Duration::from_secs(60), installed)
            .iter()
            .any(|notice| notice.feature == RemovedFeature::IngamePage)
    );
    assert!(m.removed.snapshot(now + TTL, installed).is_empty());
    assert!(
        notices(&status(&m), "ingame-page").is_empty(),
        "expired notices are gone from the status read"
    );
    m.shutdown();
}

/// N1 counterexamples: an uninstalled overlay leaves no actionable identity,
/// a dashboard preview is not recorded while the genuine request beside it
/// is, and every removed feature (the skin index too) reaches the status read.
#[test]
fn n1_status_drops_uninstalled_identities_ignores_previews_and_reports_every_feature() {
    let m = Mounted::new(None);
    let preview_page = format!("{}/counter/index.html?vosu-preview=1", m.base);
    m.block_on(async {
        for (path, referer) in [
            ("/websocket/v2/subscribe?l=counter", None),
            ("/files/skin/?l=counter", None),
            ("/api/ingame?l=counter", None),
            ("/api/ingame?l=counter&vosu-preview=1", None),
            ("/websocket/v2/subscribe?l=counter&vosu-preview=1", None),
            ("/api/ingame", Some(preview_page.as_str())),
        ] {
            let mut builder = m.client.get(format!("{}{path}", m.base));
            if let Some(referer) = referer {
                builder = builder.header(header::REFERER, referer);
            }
            let response = builder.send().await.unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        }
    });
    let read = status(&m);
    for feature in ["selected-v2-feed", "legacy-skin-index", "ingame-page"] {
        let bucket = notices(&read, feature);
        assert_eq!(bucket.len(), 1, "{feature}: {read}");
        assert_eq!(bucket[0]["overlay"]["id"], "counter", "{feature}");
        assert_eq!(
            bucket[0]["attempts"], 1,
            "{feature}: previews are not recorded"
        );
    }
    std::fs::remove_dir_all(m.static_root().join("counter")).unwrap();
    let read = status(&m);
    assert!(
        read["removed_routes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|notice| notice["overlay"].is_null()),
        "an uninstalled overlay is never named: {read}"
    );
    m.shutdown();
}
