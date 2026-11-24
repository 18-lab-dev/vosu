//! The test-harness adapter capture over the mounted production stack. The
//! osu! plugin publishes through its production projection and the ABI, and
//! the dump must be the bus selection each real adapter decoded, with an HTTP
//! result byte-identical to what the production listener serves for it.
use super::mounted::*;
use super::*;
use base64::{Engine, engine::general_purpose::STANDARD};
use vosu::server::adapter_capture::{AdapterCaptureSample, DerivedStatus, FORMAT, schema_name};
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
