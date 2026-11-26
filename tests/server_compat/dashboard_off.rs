//! A7.3 dashboard off at the routing layer: the overlay listener neither
//! advertises nor redirects to a dashboard that nothing serves, and the
//! dashboard origin earns no trust. The dashboard listener never serves
//! API v1 in either mode.
use super::*;

/// A dashboard origin whose host is not in `ALLOWED_IPS`, so only the
/// dashboard-origin rule can make it authoritative.
const DASHBOARD: &str = "http://192.168.1.7:24051";

async fn on_listener(
    dashboard_listener: bool,
    dashboard_enabled: bool,
    uri: &str,
    origin: Option<&str>,
) -> Response {
    let mut providers = providers();
    let origins =
        vosu::server::origins::Origins::new("http://127.0.0.1:24050".into(), DASHBOARD.into())
            .unwrap();
    let origins = if dashboard_enabled {
        origins
    } else {
        origins.without_dashboard()
    };
    providers.authorization = Arc::new(vosu::server::origins::OriginAuthorizer {
        inner: providers.authorization,
        origins: Arc::new(origins),
        dashboard: dashboard_listener,
    });
    let mut request = Request::builder().method(Method::GET).uri(uri).header(
        header::HOST,
        if dashboard_listener {
            "192.168.1.7:24051"
        } else {
            "localhost:24050"
        },
    );
    if let Some(origin) = origin {
        request = request.header(header::ORIGIN, origin);
    }
    let mut request = request.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(ConnectInfo(PeerAddress(SocketAddr::from((
            [127, 0, 0, 1],
            40000,
        )))));
    bounded!(routes::dispatch(State(providers), request))
}

#[tokio::test]
async fn dashboard_off_overlay_listener_has_no_dashboard_discovery_or_redirects() {
    let discovery = bounded!(on_listener(false, true, "/api/dashboard", None));
    assert_eq!(discovery.status(), StatusCode::OK);
    assert_eq!(
        bounded!(to_bytes(discovery.into_body(), 1024)).unwrap(),
        format!(r#"{{"origin":"{DASHBOARD}"}}"#)
    );
    let discovery = bounded!(on_listener(false, false, "/api/dashboard", None));
    assert_eq!(discovery.status(), StatusCode::NOT_FOUND);
    for path in [
        "/",
        "/settings",
        "/settings?overlay=x",
        "/available",
        "/tools",
    ] {
        let redirect = bounded!(on_listener(false, true, path, None));
        assert_eq!(redirect.status(), StatusCode::FOUND, "{path}");
        assert!(
            redirect.headers()[header::LOCATION]
                .to_str()
                .unwrap()
                .starts_with(DASHBOARD),
            "{path}"
        );
        let dead = bounded!(on_listener(false, false, path, None));
        assert_eq!(dead.status(), StatusCode::NOT_FOUND, "{path}");
        assert!(!dead.headers().contains_key(header::LOCATION), "{path}");
    }
    // Overlay and API serving is unaffected.
    let state = bounded!(on_listener(false, false, "/json", None));
    assert_eq!(state.status(), StatusCode::OK);
}

#[tokio::test]
async fn dashboard_off_never_trusts_the_dashboard_origin() {
    let trusted = bounded!(on_listener(false, true, "/json", Some(DASHBOARD)));
    assert_eq!(trusted.status(), StatusCode::OK);
    // Nothing of ours serves that origin when off; another process could.
    let refused = bounded!(on_listener(false, false, "/json", Some(DASHBOARD)));
    assert_eq!(refused.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn dashboard_listener_never_serves_api_v1() {
    for path in ["/api/v1", "/api/v1/sessions", "/api/v1/sessions/x/state"] {
        let response = bounded!(on_listener(true, true, path, Some(DASHBOARD)));
        assert_eq!(response.status(), StatusCode::NOT_FOUND, "{path}");
        let body: serde_json::Value =
            serde_json::from_slice(&bounded!(to_bytes(response.into_body(), 1024)).unwrap())
                .unwrap();
        assert_eq!(body["error"]["code"], "invalid_request", "{path}");
    }
}
