use super::*;
use crate::server::{
    adapter_capture::DerivedStatus,
    auth::AuthorizationPolicy,
    origins::{OriginAuthorizer, Origins},
    routes::{
        CachedJson, CalculatePpProvider, CalculateResult, Endpoint, RouteDelegate, RouteProviders,
        StateJsonProvider, StateRoute, dispatch,
    },
};
use axum::{
    body::to_bytes,
    extract::{ConnectInfo, State},
};
use std::{net::SocketAddr, sync::Mutex};
use vosu_adapter_upstream::calculate::PpQuery;
use vosu_proptest::bounded;
use vosu_surface::state::{CapturedSelection, IdentifiedRoster};

const CAPABILITY: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeeff";
const OTHER: &str = "00112233445566778899aabbccddeeff00112233445566778899aabbccddeefe";
const SECRET: &str = "requester failed for /home/private/osu! pid 4242";

type Capture = dyn Fn(Schema) -> Result<AdapterCaptureSample, String> + Send + Sync;
struct Source {
    capture: Box<Capture>,
    calls: Mutex<Vec<Schema>>,
}
impl CaptureSource for Source {
    fn capture(&self, schema: Schema) -> Result<AdapterCaptureSample, String> {
        self.calls.lock().unwrap().push(schema);
        (self.capture)(schema)
    }
}

fn sample(schema: Schema, http: CachedJson) -> AdapterCaptureSample {
    AdapterCaptureSample {
        schema,
        provider_generation: 7,
        capture: CapturedSelection {
            session: None,
            focus_revision: 3,
            roster: IdentifiedRoster {
                revision: 2,
                sessions: Arc::from(Vec::new()),
            },
            revision: 5,
            namespace_revisions: Arc::from(Vec::new()),
            values: Arc::from(Vec::new()),
        },
        derived_revision: 0,
        derived: DerivedStatus::None,
        http,
    }
}
fn ready(body: &[u8]) -> CachedJson {
    CachedJson::Ready(Bytes::copy_from_slice(body))
}

/// The ordinary routing a request reaches when the endpoint ignores it.
struct Delegate {
    endpoint: Option<AdapterCaptureEndpoint>,
    handled: Mutex<Vec<Endpoint>>,
}
impl RouteDelegate for Delegate {
    fn adapter_capture(&self) -> Option<&AdapterCaptureEndpoint> {
        self.endpoint.as_ref()
    }
    fn handle(&self, endpoint: Endpoint, _: Request, _: Bytes) -> ProviderFuture<'_, Response> {
        self.handled.lock().unwrap().push(endpoint);
        Box::pin(async {
            Response::builder()
                .status(StatusCode::NOT_FOUND)
                .body(Body::from("ordinary"))
                .unwrap()
        })
    }
}
struct Unused;
impl StateJsonProvider for Unused {
    fn cached_json(&self, _: StateRoute) -> ProviderFuture<'_, CachedJson> {
        Box::pin(async { CachedJson::NotReady })
    }
}
impl CalculatePpProvider for Unused {
    fn calculate_pp(&self, _: PpQuery) -> ProviderFuture<'_, CalculateResult> {
        Box::pin(async { CalculateResult::NotReady })
    }
}

struct Harness {
    source: Arc<Source>,
    delegate: Arc<Delegate>,
    providers: RouteProviders,
}
impl Harness {
    fn new(
        capture: impl Fn(Schema) -> Result<AdapterCaptureSample, String> + Send + Sync + 'static,
    ) -> Self {
        Self::mounted(capture, true)
    }
    fn mounted(
        capture: impl Fn(Schema) -> Result<AdapterCaptureSample, String> + Send + Sync + 'static,
        mounted: bool,
    ) -> Self {
        let source = Arc::new(Source {
            capture: Box::new(capture),
            calls: Mutex::new(Vec::new()),
        });
        let delegate = Arc::new(Delegate {
            endpoint: mounted.then(|| {
                AdapterCaptureEndpoint::new(
                    Capability::parse(CAPABILITY.as_bytes()).unwrap(),
                    source.clone(),
                )
            }),
            handled: Mutex::new(Vec::new()),
        });
        let providers = RouteProviders {
            authorization: Arc::new(AuthorizationPolicy::new(
                "198.51.100.7,localhost,absolute",
                "127.0.0.1",
            )),
            state: Arc::new(Unused),
            calculation: Arc::new(Unused),
            delegate: delegate.clone(),
        };
        Self {
            source,
            delegate,
            providers,
        }
    }
    fn calls(&self) -> Vec<Schema> {
        self.source.calls.lock().unwrap().clone()
    }
    fn handled(&self) -> Vec<Endpoint> {
        self.delegate.handled.lock().unwrap().clone()
    }
}

fn request(method: Method, uri: &str, peer: &str, capabilities: &[&str]) -> Request {
    let mut builder = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::HOST, "127.0.0.1:24050")
        .extension(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
    for capability in capabilities {
        builder = builder.header(&CAPABILITY_HEADER, *capability);
    }
    builder.body(Body::empty()).unwrap()
}
async fn send(providers: &RouteProviders, request: Request) -> (StatusCode, HeaderMap, Bytes) {
    let response = dispatch(State(providers.clone()), request).await;
    let (parts, body) = response.into_parts();
    (
        parts.status,
        parts.headers,
        to_bytes(body, usize::MAX).await.unwrap(),
    )
}
fn assert_fixed(response: &(StatusCode, HeaderMap, Bytes), status: StatusCode, body: &[u8]) {
    assert_eq!(response.0, status);
    assert_eq!(response.1[header::CONTENT_TYPE], "application/json");
    assert_eq!(response.1[header::CACHE_CONTROL], "no-store");
    assert!(!response.1.contains_key(header::ACCESS_CONTROL_ALLOW_ORIGIN));
    assert_eq!(response.2, body);
}

#[test]
fn capability_is_exactly_64_lowercase_hex_and_never_formatted() {
    let capability = Capability::parse(CAPABILITY.as_bytes()).unwrap();
    assert_eq!(format!("{capability:?}"), "Capability(<redacted>)");
    for invalid in [
        &CAPABILITY[1..],
        &format!("{CAPABILITY}0"),
        &CAPABILITY.to_ascii_uppercase(),
        &format!(" {}", &CAPABILITY[1..]),
        &format!("{}g", &CAPABILITY[1..]),
        "",
    ] {
        assert!(Capability::parse(invalid.as_bytes()).is_none(), "{invalid}");
    }
    let mut headers = HeaderMap::new();
    assert!(!capability.presented_by(&headers));
    headers.insert(&CAPABILITY_HEADER, HeaderValue::from_static(OTHER));
    assert!(!capability.presented_by(&headers));
    headers.insert(&CAPABILITY_HEADER, HeaderValue::from_static(CAPABILITY));
    assert!(capability.presented_by(&headers));
    headers.append(&CAPABILITY_HEADER, HeaderValue::from_static(CAPABILITY));
    assert!(!capability.presented_by(&headers), "duplicate header");
}

#[tokio::test]
async fn adapter_capture_serves_one_provider_sample_per_exact_schema() {
    let harness = Harness::new(|schema| Ok(sample(schema, ready(br#"{"ok":1}"#))));
    let mut expected_calls = Vec::new();
    for (name, schema) in super::super::SCHEMAS {
        for peer in ["127.0.0.1:41234", "[::1]:41234", "[::ffff:127.0.0.1]:41234"] {
            let response = bounded!(send(
                &harness.providers,
                request(
                    Method::GET,
                    &format!("{PATH}?schema={name}"),
                    peer,
                    &[CAPABILITY]
                ),
            ));
            let envelope = sample(schema, ready(br#"{"ok":1}"#)).envelope().unwrap();
            assert_fixed(&response, StatusCode::OK, &envelope);
            expected_calls.push(schema);
        }
    }
    assert_eq!(harness.calls(), expected_calls);
    assert!(harness.handled().is_empty());
}

#[tokio::test]
async fn unauthorized_or_near_miss_requests_route_as_if_the_endpoint_did_not_exist() {
    let harness = Harness::new(|schema| Ok(sample(schema, CachedJson::NotReady)));
    let upper = CAPABILITY.to_ascii_uppercase();
    let cases: [(&str, &str, &[&str]); 10] = [
        // Allowed by configuration, but not loopback.
        (PATH, "198.51.100.7:41234", &[CAPABILITY]),
        (PATH, "127.0.0.1:41234", &[]),
        (PATH, "127.0.0.1:41234", &[OTHER]),
        (PATH, "127.0.0.1:41234", &[&upper]),
        (PATH, "127.0.0.1:41234", &[CAPABILITY, CAPABILITY]),
        (PATH, "127.0.0.1:41234", &[""]),
        (
            "/__vosu_test/v1/adapter-capture/",
            "127.0.0.1:41234",
            &[CAPABILITY],
        ),
        (
            "/__vosu_test/v1/adapter-capture/v2",
            "127.0.0.1:41234",
            &[CAPABILITY],
        ),
        (
            "/__VOSU_TEST/v1/adapter-capture",
            "127.0.0.1:41234",
            &[CAPABILITY],
        ),
        (
            "/__vosu_test/v2/adapter-capture",
            "127.0.0.1:41234",
            &[CAPABILITY],
        ),
    ];
    for (path, peer, capabilities) in cases {
        let response = bounded!(send(
            &harness.providers,
            request(
                Method::GET,
                &format!("{path}?schema=v2"),
                peer,
                capabilities
            ),
        ));
        assert_eq!(response.0, StatusCode::NOT_FOUND, "{path} {peer}");
        assert_eq!(response.2, "ordinary", "{path} {peer}");
    }
    assert_eq!(
        harness.handled(),
        vec![Endpoint::CounterStatic; cases.len()]
    );
    // A non-GET request without the capability is an ordinary unknown route.
    let response = bounded!(send(
        &harness.providers,
        request(Method::POST, PATH, "127.0.0.1:41234", &[OTHER]),
    ));
    assert_eq!(
        (response.0, &response.2[..]),
        (StatusCode::NOT_FOUND, &b"Not Found"[..])
    );
    assert!(harness.calls().is_empty());
}

#[tokio::test]
async fn unmounted_endpoint_is_an_ordinary_route_even_with_a_capability() {
    let harness = Harness::mounted(|schema| Ok(sample(schema, CachedJson::NotReady)), false);
    let response = bounded!(send(
        &harness.providers,
        request(
            Method::GET,
            &format!("{PATH}?schema=v2"),
            "127.0.0.1:41234",
            &[CAPABILITY]
        ),
    ));
    assert_eq!(
        (response.0, &response.2[..]),
        (StatusCode::NOT_FOUND, &b"ordinary"[..])
    );
    assert_eq!(harness.handled(), vec![Endpoint::CounterStatic]);
    assert!(harness.calls().is_empty());
}

#[tokio::test]
async fn methods_queries_and_upgrades_are_exact_and_never_sample() {
    let harness = Harness::new(|schema| Ok(sample(schema, CachedJson::NotReady)));
    for method in [
        Method::POST,
        Method::HEAD,
        Method::PUT,
        Method::DELETE,
        Method::OPTIONS,
        Method::PATCH,
    ] {
        let response = bounded!(send(
            &harness.providers,
            request(
                method.clone(),
                &format!("{PATH}?schema=v2"),
                "127.0.0.1:41234",
                &[CAPABILITY]
            ),
        ));
        assert_fixed(&response, StatusCode::METHOD_NOT_ALLOWED, BAD_METHOD_BODY);
        assert_eq!(response.1[header::ALLOW], "GET", "{method}");
    }
    for query in [
        "",
        "?",
        "?schema",
        "?schema=",
        "?schema=V2",
        "?schema=v2%20",
        "?schema=v2+",
        "?schema=v2&",
        "?schema=v2&schema=v2",
        "?schema=v2&schema=v1",
        "?schema=v2&extra=1",
        "?extra=1&schema=v2",
        "?schema=v2%2Dprecise",
        "?Schema=v2",
        "?schema=json",
    ] {
        let response = bounded!(send(
            &harness.providers,
            request(
                Method::GET,
                &format!("{PATH}{query}"),
                "127.0.0.1:41234",
                &[CAPABILITY]
            ),
        ));
        assert_fixed(&response, StatusCode::BAD_REQUEST, BAD_QUERY_BODY);
    }
    let mut upgrade = request(
        Method::GET,
        &format!("{PATH}?schema=v2"),
        "127.0.0.1:41234",
        &[CAPABILITY],
    );
    upgrade
        .headers_mut()
        .insert(header::CONNECTION, HeaderValue::from_static("Upgrade"));
    upgrade
        .headers_mut()
        .insert(header::UPGRADE, HeaderValue::from_static("websocket"));
    let response = bounded!(send(&harness.providers, upgrade));
    assert_eq!(
        (response.0, &response.2[..]),
        (StatusCode::NOT_FOUND, &b"Not Found"[..])
    );
    assert!(harness.calls().is_empty());
    assert!(harness.handled().is_empty());
}

#[tokio::test]
async fn capture_and_encoding_failures_have_one_fixed_body() {
    let failing = Harness::new(|_| Err(SECRET.to_owned()));
    let response = bounded!(send(
        &failing.providers,
        request(
            Method::GET,
            &format!("{PATH}?schema=sc"),
            "127.0.0.1:41234",
            &[CAPABILITY]
        ),
    ));
    assert_fixed(&response, StatusCode::INTERNAL_SERVER_ERROR, FAILED_BODY);
    // A body that cannot become an envelope fails the same way.
    let unencodable = Harness::new(|schema| Ok(sample(schema, ready(&[0xff, 0xfe]))));
    assert!(sample(Schema::V1, ready(&[0xff])).envelope().is_err());
    let response = bounded!(send(
        &unencodable.providers,
        request(
            Method::GET,
            &format!("{PATH}?schema=v1"),
            "127.0.0.1:41234",
            &[CAPABILITY]
        ),
    ));
    assert_fixed(&response, StatusCode::INTERNAL_SERVER_ERROR, FAILED_BODY);
    assert_eq!(failing.calls(), vec![Schema::StreamCompanion]);
}

#[tokio::test]
async fn envelope_is_bounded_at_8_mib_and_never_truncated() {
    let overhead = sample(Schema::V2, ready(b"")).envelope().unwrap().len();
    for (extra, status) in [(0, StatusCode::OK), (1, StatusCode::INSUFFICIENT_STORAGE)] {
        let body = vec![b'a'; MAX_RESPONSE_BYTES - overhead + extra];
        let harness = Harness::new(move |schema| Ok(sample(schema, ready(&body))));
        let response = bounded!(send(
            &harness.providers,
            request(
                Method::GET,
                &format!("{PATH}?schema=v2"),
                "127.0.0.1:41234",
                &[CAPABILITY]
            ),
        ));
        if status == StatusCode::OK {
            assert_eq!(response.0, status);
            assert_eq!(response.2.len(), MAX_RESPONSE_BYTES);
        } else {
            assert_fixed(&response, status, TOO_LARGE_BODY);
        }
    }
}

#[tokio::test]
async fn dashboard_listener_never_serves_the_endpoint() {
    let mut harness = Harness::new(|schema| Ok(sample(schema, CachedJson::NotReady)));
    harness.providers.authorization = Arc::new(OriginAuthorizer {
        inner: harness.providers.authorization.clone(),
        origins: Arc::new(
            Origins::new(
                "http://127.0.0.1:24050".into(),
                "http://127.0.0.1:24051".into(),
            )
            .unwrap(),
        ),
        dashboard: true,
    });
    let mut request = request(
        Method::GET,
        &format!("{PATH}?schema=v2"),
        "127.0.0.1:41234",
        &[CAPABILITY],
    );
    request
        .headers_mut()
        .insert(header::HOST, HeaderValue::from_static("127.0.0.1:24051"));
    let response = bounded!(send(&harness.providers, request));
    assert_ne!(response.0, StatusCode::OK);
    assert!(harness.calls().is_empty());
}
