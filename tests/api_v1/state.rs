use super::support::*;
use serde_json::json;
use vosu_plugin_api::PublishValueRef;

#[test]
fn all_state_routes_methods_queries_and_near_misses_use_api_envelopes() {
    let mut h = Harness::new();
    let ns = "test.s4.routes";
    let (handle, _) = h.publisher(ns, 894_010);
    publish(
        ns,
        handle,
        &[
            ("/score", PublishValueRef::I64(7913)),
            ("/a~1b/~0", PublishValueRef::String("escaped")),
            ("/nil", PublishValueRef::Null),
        ],
    );
    let id = h.id();
    h.runtime.block_on(bounded(async {
        let server = Server::new(&h, false).await;
        for target in [&id, "active"] {
            let base = format!("/sessions/{target}");
            let summary = server.get(&base).await.1;
            let (status, full) = server.get(&format!("{base}/state")).await;
            assert_eq!(status, 200);
            assert_eq!(full["vosu.client"], summary);
            assert_eq!(full[ns]["score"], 7913);
            let (status, namespace) = server.get(&format!("{base}/state/{ns}")).await;
            assert_eq!(status, 200);
            assert_eq!(namespace, full[ns]);
            for (route, pointer, expected) in [
                (format!("{base}/state"), format!("/{ns}/score"), json!(7913)),
                (format!("{base}/state/{ns}"), "/score".into(), json!(7913)),
                (
                    format!("{base}/state/{ns}"),
                    "/a~1b/~0".into(),
                    json!("escaped"),
                ),
                (format!("{base}/state/{ns}"), "/nil".into(), json!(null)),
                (
                    format!("{base}/state/{ns}"),
                    "/score/child".into(),
                    json!(null),
                ),
                (format!("{base}/state/{ns}"), "/absent".into(), json!(null)),
            ] {
                let query: String =
                    url::form_urlencoded::byte_serialize(pointer.as_bytes()).collect();
                let response = server
                    .client
                    .get(format!("{}{route}?pointer={query}", server.base))
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), 200);
                assert_eq!(
                    response.json::<serde_json::Value>().await.unwrap(),
                    json!({"value": expected})
                );
            }
            for route in [
                &base,
                &format!("{base}/state"),
                &format!("{base}/state/{ns}"),
                &format!("{base}/subscribe"),
            ] {
                for method in [
                    reqwest::Method::POST,
                    reqwest::Method::PUT,
                    reqwest::Method::DELETE,
                    reqwest::Method::PATCH,
                ] {
                    let response = server
                        .client
                        .request(method, format!("{}{route}", server.base))
                        .send()
                        .await
                        .unwrap();
                    assert_eq!(response.status(), 405, "{route}");
                    assert_eq!(response.headers()["allow"], "GET");
                    assert_eq!(
                        response.json::<serde_json::Value>().await.unwrap()["error"]["code"],
                        "method_not_allowed"
                    );
                }
            }
        }
        for (path, status, code) in [
            ("/sessions/no-such-id", 404, "unknown_session"),
            ("/sessions/no-such-id/state", 404, "unknown_session"),
            ("/sessions/active/state/unknown", 404, "unknown_namespace"),
            ("/sessions/active/state?pointer=bad", 400, "invalid_pointer"),
            (
                "/sessions/active/state?pointer=%2Fbad~2",
                400,
                "invalid_pointer",
            ),
            ("/sessions/active/state?pointer=%FF", 400, "invalid_request"),
            (
                "/sessions/active/state?pointer=&pointer=",
                400,
                "invalid_request",
            ),
            (
                "/sessions/active/state?unknown=value",
                400,
                "invalid_request",
            ),
            (
                "/sessions/active/subscribe?pointer=",
                400,
                "invalid_request",
            ),
            ("/sessions/active/subscribe/precise", 404, "invalid_request"),
            (
                "/sessions/active/state/test.s4.routes/extra",
                404,
                "invalid_request",
            ),
            ("/sessions/", 404, "invalid_request"),
            ("/sessions/active/", 404, "invalid_request"),
        ] {
            let response = server.get(path).await;
            assert_eq!(response.0, status, "{path}: {}", response.1);
            assert_eq!(response.1["error"]["code"], code, "{path}");
        }
        server.shutdown().await;
    }));
}
