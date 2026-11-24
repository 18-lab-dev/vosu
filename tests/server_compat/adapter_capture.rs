//! The test-harness adapter capture over the mounted production stack. The
//! osu! plugin publishes through its production projection and the ABI, and
//! the dump must be the bus selection each real adapter decoded, with an HTTP
//! result byte-identical to what the production listener serves for it.
use super::mounted::*;
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use vosu::{
    runtime::ListenerOwner,
    server::adapter_capture::{
        AdapterCaptureSample, DerivedStatus, FORMAT,
        endpoint::{
            AdapterCaptureEndpoint, BAD_METHOD_BODY, BAD_QUERY_BODY, CAPABILITY_HEADER, Capability,
            CaptureSource, PATH,
        },
        schema_name,
    },
};
use vosu_plugin_api::CborValue;

const ROUTES: [(StateRoute, &str); 4] = [
    (StateRoute::V1, "/json"),
    (StateRoute::StreamCompanion, "/json/sc"),
    (StateRoute::V2, "/json/v2"),
    (StateRoute::V2Precise, "/json/v2/precise"),
];

async fn get(m: &Mounted, path: &str) -> (StatusCode, Bytes) {
    let response = m
        .client
        .get(format!("{}{path}", m.base))
        .send()
        .await
        .unwrap();
    (response.status(), response.bytes().await.unwrap())
}

fn dump(m: &Mounted, route: StateRoute) -> (AdapterCaptureSample, Value) {
    let schemas = Schemas(m.fixture.providers.clone());
    let sample = schemas.get(schema_of(route)).capture_for_harness().unwrap();
    let envelope = serde_json::from_slice(&sample.envelope().unwrap()).unwrap();
    (sample, envelope)
}

#[test]
fn adapter_capture_sandwich_equals_production_route_bytes() {
    let m = Mounted::new(Some(seeded_play()));
    let demand = m.demand();
    m.block_on(async {
        for (route, path) in ROUTES {
            let (before, before_json) = dump(&m, route);
            let (status, body) = get(&m, path).await;
            let (after, after_json) = dump(&m, route);
            assert_eq!(status, StatusCode::OK, "{path}");
            let CachedJson::Ready(dumped) = &after.http else {
                panic!("{path}: dump not ready: {:?}", after.http);
            };
            assert_eq!(dumped, &body, "{path}: route bytes differ from the dump");
            assert_eq!(before.http, after.http, "{path}");
            assert_eq!(
                after_json["http"]["body_utf8"].as_str().unwrap().as_bytes(),
                &body[..]
            );
            assert_eq!(before_json["digest"], after_json["digest"], "{path}");
            assert_eq!(after_json["format"], FORMAT);
            assert_eq!(after_json["schema"], schema_name(schema_of(route)));
            assert_eq!(after_json["session"]["ordinal"], 0);
            if route == StateRoute::StreamCompanion {
                // The settled chart is retained exactly as by the route read.
                assert_eq!(after.derived, DerivedStatus::Ready, "{path}");
                assert!(after.derived_revision > 0);
            } else {
                assert_eq!(after.derived, DerivedStatus::None, "{path}");
                assert_eq!(before.provider_generation, after.provider_generation);
            }
            // Every selected entry is the committed bus value's canonical CBOR.
            let entries = after_json["entries"].as_array().unwrap();
            assert_eq!(entries.len(), after.capture.values.len());
            assert!(!entries.is_empty(), "{path}");
            let mut values = 0;
            for (entry, json) in after.capture.values.iter().zip(entries) {
                assert_eq!(json["ns"], entry.ns.as_str());
                assert_eq!(json["pointer"], entry.pointer.as_str());
                match (&entry.value, json["value_cbor_b64"].as_str()) {
                    (Some(value), Some(encoded)) => {
                        let bytes = STANDARD.decode(encoded).unwrap();
                        assert_eq!(bytes, vosu_plugin_api::encode(value).unwrap());
                        let decoded: CborValue = vosu_plugin_api::decode(&bytes).unwrap();
                        assert_eq!(vosu_plugin_api::encode(&decoded).unwrap(), bytes);
                        values += 1;
                    }
                    (None, None) => assert_eq!(json["value_cbor_b64"], Value::Null),
                    other => panic!("{path} {}: {other:?}", entry.pointer),
                }
            }
            assert!(values > 0, "{path}: no published values");
        }
    });
    assert_eq!(m.demand(), demand, "a dump registered sampling demand");
    m.shutdown();
}

#[test]
fn adapter_capture_without_a_client_is_the_production_not_ready_response() {
    let m = Mounted::new(None);
    m.block_on(async {
        for (route, path) in ROUTES {
            let (status, body) = get(&m, path).await;
            let (sample, envelope) = dump(&m, route);
            assert_eq!(sample.http, CachedJson::NotReady, "{path}");
            assert_eq!(envelope["session"], Value::Null);
            assert_eq!(envelope["http"]["kind"], "not_ready");
            assert_eq!(envelope["http"]["status"], status.as_u16(), "{path}");
            assert_eq!(
                envelope["http"]["body_utf8"].as_str().unwrap().as_bytes(),
                &body[..],
                "{path}"
            );
        }
    });
    m.shutdown();
}

const CAPABILITY: &str = "5f0e7c1d9a2b3c4d5e6f708192a3b4c5d6e7f8091a2b3c4d5e6f708192a3b4c5";

impl CaptureSource for Schemas {
    fn capture(&self, schema: Schema) -> Result<AdapterCaptureSample, String> {
        self.get(schema).capture_for_harness()
    }
}

/// A second production listener over the same mounted stack, its delegate
/// carrying the endpoint exactly as `ProductionRuntime` composes it for a
/// launch that received a capability.
fn harness_listener(m: &Mounted) -> (ListenerOwner, String) {
    let endpoint = AdapterCaptureEndpoint::new(
        Capability::parse(CAPABILITY.as_bytes()).unwrap(),
        Arc::new(Schemas(m.fixture.providers.clone())),
    );
    let mut providers = m.providers.clone();
    providers.delegate = Arc::new(m.delegate.as_ref().clone().with_adapter_capture(endpoint));
    m.block_on(async {
        for _ in 0..16 {
            let port = std::net::TcpListener::bind("127.0.0.1:0")
                .unwrap()
                .local_addr()
                .unwrap()
                .port();
            let target = vosu::server::runtime::ListenTarget {
                host: "127.0.0.1".into(),
                port,
            };
            if let Ok(listener) = ListenerOwner::start(providers.clone(), target).await {
                return (listener, format!("http://127.0.0.1:{port}"));
            }
        }
        panic!("no free loopback port")
    })
}

struct Reply {
    status: StatusCode,
    headers: reqwest::header::HeaderMap,
    body: Bytes,
}
async fn call(m: &Mounted, method: reqwest::Method, url: &str, capability: Option<&str>) -> Reply {
    let mut request = m.client.request(method, url);
    if let Some(capability) = capability {
        request = request.header(CAPABILITY_HEADER.as_str(), capability);
    }
    let response = request.send().await.unwrap();
    Reply {
        status: response.status(),
        headers: response.headers().clone(),
        body: response.bytes().await.unwrap(),
    }
}

#[test]
fn adapter_capture_endpoint_serves_the_provider_retained_sandwich() {
    let m = Mounted::new(Some(seeded_play()));
    let (listener, harness) = harness_listener(&m);
    let demand = m.demand();
    let revision = m.revision();
    m.block_on(async {
        for (route, path) in ROUTES {
            let schema = schema_name(schema_of(route));
            let url = format!("{harness}{PATH}?schema={schema}");
            let before = call(&m, reqwest::Method::GET, &url, Some(CAPABILITY)).await;
            let (status, body) = get(&m, path).await;
            let after = call(&m, reqwest::Method::GET, &url, Some(CAPABILITY)).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            for reply in [&before, &after] {
                assert_eq!(reply.status, StatusCode::OK, "{path}");
                assert_eq!(reply.headers["content-type"], "application/json");
                assert_eq!(reply.headers["cache-control"], "no-store");
                assert!(!reply.headers.contains_key("access-control-allow-origin"));
            }
            let before_json: Value = serde_json::from_slice(&before.body).unwrap();
            let after_json: Value = serde_json::from_slice(&after.body).unwrap();
            assert_eq!(after_json["format"], FORMAT);
            assert_eq!(after_json["schema"], schema);
            assert_eq!(after_json["http"]["kind"], "ready", "{path}");
            assert_eq!(
                after_json["http"]["body_utf8"].as_str().unwrap().as_bytes(),
                &body[..],
                "{path}: route bytes differ from the dump"
            );
            assert_eq!(before_json["digest"], after_json["digest"], "{path}");
            assert!(!after_json["entries"].as_array().unwrap().is_empty());
            if route != StateRoute::StreamCompanion {
                // One retained snapshot: the whole envelope is byte-stable and
                // is exactly the provider sample's canonical envelope.
                assert_eq!(before.body, after.body, "{path}");
                let (sample, _) = dump(&m, route);
                assert_eq!(sample.envelope().unwrap(), &after.body[..], "{path}");
            }
        }
        // The ordinary listener has no endpoint mounted; neither listener
        // serves it without the capability.
        let unmounted = format!("{}{PATH}?schema=v2", m.base);
        let ordinary = call(&m, reqwest::Method::GET, &unmounted, Some(CAPABILITY)).await;
        assert_eq!(ordinary.status, StatusCode::NOT_FOUND);
        let mounted = format!("{harness}{PATH}?schema=v2");
        for capability in [
            None,
            Some(&CAPABILITY[..63]),
            Some(&CAPABILITY.replace('5', "6")[..]),
        ] {
            let reply = call(&m, reqwest::Method::GET, &mounted, capability).await;
            assert_eq!(
                (reply.status, &reply.body),
                (ordinary.status, &ordinary.body),
                "{capability:?}"
            );
        }
        for method in [reqwest::Method::POST, reqwest::Method::HEAD] {
            let reply = call(&m, method.clone(), &mounted, Some(CAPABILITY)).await;
            assert_eq!(reply.status, StatusCode::METHOD_NOT_ALLOWED, "{method}");
            assert_eq!(reply.headers["allow"], "GET");
            if method == reqwest::Method::POST {
                assert_eq!(reply.body, BAD_METHOD_BODY);
            }
        }
        let duplicate = format!("{harness}{PATH}?schema=v2&schema=v2");
        let reply = call(&m, reqwest::Method::GET, &duplicate, Some(CAPABILITY)).await;
        assert_eq!(
            (reply.status, &reply.body[..]),
            (StatusCode::BAD_REQUEST, BAD_QUERY_BODY)
        );
    });
    assert_eq!(
        m.demand(),
        demand,
        "the endpoint registered sampling demand"
    );
    assert_eq!(m.revision(), revision, "the endpoint published state");
    m.block_on(async move {
        listener.stop_accepting();
        listener.join().await;
    });
    m.shutdown();
}
