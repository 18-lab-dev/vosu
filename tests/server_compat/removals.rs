use super::*;

#[tokio::test]
async fn removed_routes_are_exact_404_without_upgrade_or_static_fallthrough() {
    for path in ["/websocket/v2/subscribe", "/files/skin/"] {
        for method in [
            Method::GET,
            Method::HEAD,
            Method::POST,
            Method::PUT,
            Method::OPTIONS,
        ] {
            for upgrade in [false, true] {
                let mut request = Request::builder()
                    .method(method.clone())
                    .uri(format!("{path}?l=unknown&secret=never-retain"))
                    .header(header::HOST, "127.0.0.1:24050")
                    .body(Body::empty())
                    .unwrap();
                request
                    .extensions_mut()
                    .insert(ConnectInfo(SocketAddr::from(([127, 0, 0, 1], 24000))));
                if upgrade {
                    request
                        .headers_mut()
                        .insert(header::CONNECTION, "upgrade".parse().unwrap());
                    request
                        .headers_mut()
                        .insert(header::UPGRADE, "websocket".parse().unwrap());
                }
                let response = bounded!(routes::dispatch(State(providers()), request));
                assert_eq!(
                    response.status(),
                    StatusCode::NOT_FOUND,
                    "{method} {path} upgrade={upgrade}"
                );
                assert!(!response.headers().contains_key(header::UPGRADE));
                assert_eq!(
                    bounded!(to_bytes(response.into_body(), 1024)).unwrap(),
                    "Not Found"
                );
            }
        }
    }
    for path in [
        "/websocket/v2/subscribe/",
        "/websocket/v2/Subscribe",
        "/websocket/v2/subscribe/extra",
        "/files/skin",
    ] {
        let response = bounded!(dispatch(Method::GET, path, None, Body::empty()));
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(!response.headers().contains_key(header::UPGRADE));
    }
}
#[tokio::test]
async fn rejected_origin_is_403_not_found_and_never_reaches_upgrade() {
    let response = bounded!(dispatch(
        Method::GET,
        "/websocket/v2/subscribe",
        Some("http://evil.invalid"),
        Body::empty(),
    ));
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(
        bounded!(to_bytes(response.into_body(), 1024)).unwrap(),
        "Not Found"
    );
}
