use super::*;

#[tokio::test]
async fn api_v1_reads_preserve_peer_origin_and_listener_policy() {
    for credential in [None, Some("Bearer unrelated"), Some("Bearer settings-key")] {
        for (peer, origin, expected) in [
            ("127.0.0.1:43210", "http://localhost:24860", StatusCode::OK),
            (
                "127.0.0.1:43210",
                "http://evil.example",
                StatusCode::FORBIDDEN,
            ),
            (
                "203.0.113.55:43210",
                "http://localhost:24860",
                StatusCode::FORBIDDEN,
            ),
        ] {
            let (providers, _) = split_providers(false);
            let mut request = make_request(Method::GET, "/api/v1/sessions", Body::empty());
            request
                .extensions_mut()
                .insert(ConnectInfo(peer.parse::<SocketAddr>().unwrap()));
            request
                .headers_mut()
                .insert(header::HOST, HeaderValue::from_static("127.0.0.1:24860"));
            request
                .headers_mut()
                .insert(header::ORIGIN, HeaderValue::from_static(origin));
            if let Some(credential) = credential {
                request
                    .headers_mut()
                    .insert(header::AUTHORIZATION, HeaderValue::from_static(credential));
            }
            let response = bounded!(dispatch(State(providers), request));
            assert_eq!(
                response.status(),
                expected,
                "peer={peer}, origin={origin}, credential={credential:?}"
            );
            if expected == StatusCode::FORBIDDEN {
                let value: Value = serde_json::from_slice(&bounded!(body(response))).unwrap();
                assert_eq!(value["error"]["code"], "forbidden");
            }
        }
    }
    let (providers, _) = split_providers(true);
    let response = bounded!(dispatch(
        State(providers),
        make_request(Method::GET, "/api/v1/sessions", Body::empty())
    ));
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let value: Value = serde_json::from_slice(&bounded!(body(response))).unwrap();
    assert!(value["error"]["code"].is_string());
}
