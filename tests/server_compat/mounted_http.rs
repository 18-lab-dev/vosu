//! HTTP and upgrade matrix over the real mounted adapter providers.
use super::mounted::*;
use super::*;

const STATE: [(&str, &str); 4] = [
    ("/json", "v1"),
    ("/json/sc", "sc"),
    ("/json/v2", "v2"),
    ("/json/v2/precise", "v2-precise"),
];
const FEEDS: [(&str, &str); 4] = [
    ("/ws", "v1"),
    ("/tokens", "sc"),
    ("/websocket/v2", "v2"),
    ("/websocket/v2/precise", "v2-precise"),
];
const NEAR_MISSES: [&str; 16] = [
    "/json/",
    "/JSON",
    "/Json/v2",
    "/json/v2/",
    "/json/V2",
    "/json/v2/precise/",
    "/json/v2/Precise",
    "/json/v2/precisex",
    "/json/v2/precise/extra",
    "/json/sc/",
    "/json/v3",
    "/json%2Fv2",
    "/json/v2%2Fprecise",
    "/json/v2;x",
    "//json/v2",
    "/api/calculate/pp/",
];
const FEED_NEAR_MISSES: [&str; 12] = [
    "/ws/",
    "/WS",
    "/tokens/",
    "/tokens/x",
    "/websocket",
    "/websocket/",
    "/websocket/v2/",
    "/websocket/V2",
    "/websocket/v2/precise/",
    "/websocket/v2/precise/x",
    "/websocket/v2/subscribe",
    "/websocket/v3",
];

#[test]
fn mounted_state_routes_serve_exact_bytes_and_reject_methods_near_misses_and_upgrades() {
    let m = Mounted::new(Some(seeded_play()));
    let expected: Vec<Bytes> = STATE.iter().map(|(_, schema)| m.expected(schema)).collect();
    // The seeded producer's distinct path facts reach the wire (positive control).
    assert!(
        String::from_utf8_lossy(&expected[2])
            .contains("/compat-private/stable-play-osu-fl/install")
    );
    m.block_on(async {
        for ((path, schema), expected) in STATE.iter().zip(&expected) {
            let url = format!("{}{path}", m.base);
            for query in ["", "?x=1&l=counter", "?a=%ZZ"] {
                let response = m.client.get(format!("{url}{query}")).send().await.unwrap();
                assert_eq!(response.status(), StatusCode::OK, "{path}{query}");
                assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
                assert!(
                    response.headers()[header::CONTENT_TYPE]
                        .to_str()
                        .unwrap()
                        .starts_with("application/json"),
                    "{path}"
                );
                assert!(!response.headers().contains_key(header::UPGRADE));
                assert_eq!(
                    &response.bytes().await.unwrap(),
                    expected,
                    "{schema}{query}"
                );
            }
            for method in [
                Method::HEAD,
                Method::POST,
                Method::PUT,
                Method::DELETE,
                Method::PATCH,
                Method::OPTIONS,
            ] {
                let response = m.client.request(method.clone(), &url).send().await.unwrap();
                assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
                assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
                assert_ne!(
                    &response.bytes().await.unwrap(),
                    expected,
                    "{method} {path}"
                );
            }
            // An HTTP state route is never an upgrade path.
            assert_eq!(
                m.ws(path, None).await.err(),
                Some(StatusCode::NOT_FOUND),
                "{path}"
            );
        }
        for path in NEAR_MISSES {
            let response = m
                .client
                .get(format!("{}{path}", m.base))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            let body = response.bytes().await.unwrap();
            assert!(!expected.contains(&body), "{path} served adapter state");
        }
        for ((path, schema), bytes) in FEEDS.iter().zip(&expected) {
            let mut socket = m.ws(path, None).await.unwrap();
            assert_eq!(text(&mut socket).await.as_bytes(), bytes, "{schema} feed");
            socket.close(None).await.unwrap();
            // Without an upgrade the feed path serves no state document.
            let plain = m
                .client
                .get(format!("{}{path}", m.base))
                .send()
                .await
                .unwrap();
            assert_ne!(plain.status(), StatusCode::OK, "{path}");
            assert!(!expected.contains(&plain.bytes().await.unwrap()));
            for method in [Method::POST, Method::PUT, Method::OPTIONS] {
                let response = m
                    .client
                    .request(method.clone(), format!("{}{path}", m.base))
                    .header(header::CONNECTION, "upgrade")
                    .header(header::UPGRADE, "websocket")
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::NOT_FOUND, "{method} {path}");
            }
        }
        for path in FEED_NEAR_MISSES {
            assert_eq!(
                m.ws(path, None).await.err(),
                Some(StatusCode::NOT_FOUND),
                "{path}"
            );
        }
    });
    m.shutdown();
}

#[test]
fn mounted_origin_peer_limits_and_api_v1_privacy_are_enforced_on_adapter_routes() {
    let m = Mounted::new(Some(seeded_play()));
    let v2 = m.expected("v2");
    let private = "/compat-private/stable-play-osu-fl";
    assert!(
        String::from_utf8_lossy(&v2).contains(private),
        "positive control"
    );
    m.block_on(async {
        for (path, _) in STATE {
            for origin in [
                "http://evil.invalid",
                "https://evil.invalid",
                "http://192.0.2.1:24050",
            ] {
                let response = m
                    .client
                    .get(format!("{}{path}", m.base))
                    .header(header::ORIGIN, origin)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(response.status(), StatusCode::FORBIDDEN, "{origin} {path}");
                assert_eq!(response.text().await.unwrap(), "Not Found");
            }
            // Same-origin pages and opaque (`null`) browser-source origins read.
            for origin in [m.origin.as_str(), "null"] {
                let allowed = m
                    .client
                    .get(format!("{}{path}", m.base))
                    .header(header::ORIGIN, origin)
                    .send()
                    .await
                    .unwrap();
                assert_eq!(allowed.status(), StatusCode::OK, "{origin} {path}");
                let headers = allowed.headers();
                assert_eq!(headers[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
                assert_eq!(
                    headers[header::ACCESS_CONTROL_ALLOW_HEADERS],
                    "Origin, X-Requested-With, Content-Type, Accept"
                );
                assert_eq!(
                    headers[header::ACCESS_CONTROL_ALLOW_METHODS],
                    "POST, GET, PUT, DELETE, OPTIONS"
                );
                assert_eq!(headers["access-control-allow-private-network"], "true");
                assert!(!headers.contains_key(header::ACCESS_CONTROL_ALLOW_CREDENTIALS));
            }
        }
        for (path, _) in FEEDS {
            assert_eq!(
                m.ws(path, Some("http://evil.invalid")).await.err(),
                Some(StatusCode::FORBIDDEN),
                "{path}"
            );
            assert!(m.ws(path, Some(&m.origin)).await.is_ok(), "{path}");
            assert!(m.ws(path, Some("null")).await.is_ok(), "{path}");
        }
        // A peer outside the allow-list is refused before any provider runs,
        // for state reads and upgrades alike (in-process: same dispatcher).
        for (path, upgrade) in [("/json/v2", false), ("/websocket/v2", true)] {
            let mut request = Request::builder()
                .uri(path)
                .header(header::HOST, format!("127.0.0.1:{}", m.port));
            if upgrade {
                request = request
                    .header(header::CONNECTION, "upgrade")
                    .header(header::UPGRADE, "websocket")
                    .header("Sec-WebSocket-Version", "13")
                    .header("Sec-WebSocket-Key", "dGhlIHNhbXBsZSBub25jZQ==");
            }
            let mut request = request.body(Body::empty()).unwrap();
            request
                .extensions_mut()
                .insert(ConnectInfo(PeerAddress("192.0.2.9:4000".parse().unwrap())));
            let response = routes::dispatch(State(m.providers.clone()), request).await;
            assert_eq!(response.status(), StatusCode::FORBIDDEN, "{path}");
            assert!(!response.headers().contains_key(header::UPGRADE));
        }
        // Surface metadata budget: an over-limit query never reaches a provider.
        let pairs: String = (0..=vosu_surface::routes::MAX_QUERY_PAIRS)
            .map(|index| format!("k{index}=v&"))
            .collect();
        let response = m
            .client
            .get(format!("{}/json/v2?{pairs}", m.base))
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_ne!(response.bytes().await.unwrap(), v2);
        // Oversized client messages close with 1009 on a mounted adapter feed.
        // Fragments keep the client from writing after the server's close.
        use tokio_tungstenite::tungstenite::protocol::frame::{
            Frame,
            coding::{Data, OpCode},
        };
        let mut socket = m.ws("/websocket/v2", None).await.unwrap();
        text(&mut socket).await;
        for (data, opcode, last) in [
            (vec![b'x'; MAX_MESSAGE_BYTES], Data::Text, false),
            (vec![b'x'], Data::Continue, true),
        ] {
            socket
                .send(WsMessage::Frame(Frame::message(
                    data,
                    OpCode::Data(opcode),
                    last,
                )))
                .await
                .unwrap();
        }
        let code = loop {
            match socket.next().await {
                Some(Ok(WsMessage::Close(Some(frame)))) => break u16::from(frame.code),
                Some(Ok(WsMessage::Text(_) | WsMessage::Ping(_) | WsMessage::Pong(_))) => {}
                other => panic!("expected a 1009 close, got {other:?}"),
            }
        };
        assert_eq!(code, MESSAGE_TOO_BIG);
        // API v1 on the same listener never exposes internal path echoes.
        let active: Value = m
            .client
            .get(format!("{}/api/v1/sessions/active", m.base))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        let id = active["session"].as_str().unwrap();
        let state = m
            .client
            .get(format!("{}/api/v1/sessions/{id}/state", m.base))
            .send()
            .await
            .unwrap();
        assert_eq!(state.status(), StatusCode::OK);
        let state = state.text().await.unwrap();
        assert!(state.contains("\"vosu.osu\""), "{state}");
        assert!(!state.contains(private), "API v1 leaked a private path");
        let internal = m
            .client
            .get(format!(
                "{}/api/v1/sessions/{id}/state/vosu.osu/internal",
                m.base
            ))
            .send()
            .await
            .unwrap();
        assert_ne!(internal.status(), StatusCode::OK);
        assert!(!internal.text().await.unwrap().contains(private));
    });
    m.shutdown();
}

#[test]
fn mounted_no_client_errors_calculation_and_removed_route_attribution() {
    let m = Mounted::new(None);
    m.block_on(async {
        for (path, _) in STATE {
            let response = m
                .client
                .get(format!("{}{path}", m.base))
                .send()
                .await
                .unwrap();
            assert_eq!(
                response.status(),
                StatusCode::INTERNAL_SERVER_ERROR,
                "{path}"
            );
            assert_eq!(response.headers()[header::ACCESS_CONTROL_ALLOW_ORIGIN], "*");
            assert_eq!(
                response.text().await.unwrap(),
                r#"{"error":"osu is not ready/running"}"#,
                "{path}"
            );
        }
        // Feeds still accept overlays before osu! starts, and no state
        // document precedes control traffic for the absent client.
        for (path, _) in FEEDS {
            let mut socket = m
                .ws(path, None)
                .await
                .unwrap_or_else(|status| panic!("{path}: no-client upgrade refused with {status}"));
            socket.send(WsMessage::Ping("p".into())).await.unwrap();
            match socket.next().await {
                Some(Ok(WsMessage::Pong(payload))) => assert_eq!(payload.as_ref(), b"p"),
                other => panic!("{path}: expected only a pong, got {other:?}"),
            }
            socket.close(None).await.unwrap();
        }
        let calculation = m
            .client
            .get(format!("{}/api/calculate/pp?mode=0", m.base))
            .send()
            .await
            .unwrap();
        assert_eq!(calculation.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let length = calculation.headers()[header::CONTENT_LENGTH].clone();
        assert_eq!(
            calculation.text().await.unwrap(),
            r#"{"error":"osu is not ready/running"}"#
        );
        assert_eq!(
            length,
            r#"{"error":"osu is not ready/running"}"#.len().to_string()
        );
        let invalid = m
            .client
            .get(format!("{}/api/calculate/pp?combo=-1", m.base))
            .send()
            .await
            .unwrap();
        assert_eq!(invalid.status(), StatusCode::BAD_REQUEST);
        assert!(invalid.text().await.unwrap().starts_with(r#"{"error":"#));
        for path in ["/backgroundImage", "/files/beatmap/file", "/Songs/x/y.osu"] {
            let response = m
                .client
                .get(format!("{}{path}", m.base))
                .send()
                .await
                .unwrap();
            assert_ne!(response.status(), StatusCode::OK, "{path}");
        }
        // Lazer-only and removed legacy skin index paths never succeed.
        for (path, overlay) in [
            ("/websocket/v2/subscribe?l=counter", Some("counter")),
            ("/websocket/v2/subscribe?l=uninstalled", None),
            ("/files/skin/?l=counter", Some("counter")),
        ] {
            let before = attempts(&m, overlay);
            assert_eq!(m.ws(path, None).await.err(), Some(StatusCode::NOT_FOUND));
            let response = m
                .client
                .get(format!("{}{path}", m.base))
                .send()
                .await
                .unwrap();
            assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
            assert_eq!(response.text().await.unwrap(), "Not Found");
            assert_eq!(attempts(&m, overlay), before + 2, "{path}");
        }
        // Origin alone never identifies an overlay.
        let before = attempts(&m, None);
        let response = m
            .client
            .get(format!("{}/websocket/v2/subscribe", m.base))
            .header(header::ORIGIN, &m.origin)
            .send()
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        assert_eq!(attempts(&m, None), before + 1);
        assert!(
            m.delegate
                .removed_notices()
                .iter()
                .all(|notice| notice.overlay.as_deref() != Some("uninstalled"))
        );
    });
    m.shutdown();
}

fn attempts(m: &Mounted, overlay: Option<&str>) -> u64 {
    m.delegate
        .removed_notices()
        .iter()
        .filter(|notice| notice.overlay.as_deref() == overlay)
        .map(|notice| notice.attempts)
        .sum()
}
