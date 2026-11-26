use super::*;
use vosu_proptest::bounded;

#[tokio::test]
async fn tool_routes_are_the_exact_a7_2_inventory_and_mutations_require_dashboard_csrf() {
    for (method, path) in [
        (Method::GET, "/api/tools"),
        (Method::GET, "/api/tools/fixture"),
        (Method::GET, "/api/tools/fixture/settings"),
        (Method::POST, "/api/tools/fixture/settings"),
        (Method::POST, "/api/tools/fixture/enable"),
        (Method::POST, "/api/tools/fixture/disable"),
        (Method::POST, "/api/tools/fixture/trust"),
        (Method::POST, "/api/tools/fixture/launch"),
    ] {
        assert_eq!(
            classify(&method, path),
            Some(Endpoint::ToolManagement),
            "{method} {path}"
        );
    }
    // Deleted service, log, data and autostart routes, near misses and the
    // CLI host and proxy routes are unknown (404), never a prefix match.
    for (method, path) in [
        (Method::PUT, "/api/tools/fixture/autostart"),
        (Method::POST, "/api/tools/fixture/start"),
        (Method::POST, "/api/tools/fixture/stop"),
        (Method::POST, "/api/tools/fixture/restart"),
        (Method::POST, "/api/tools/fixture/open-data"),
        (Method::POST, "/api/tools/fixture/clear-cache"),
        (Method::GET, "/api/tools/fixture/logs"),
        (Method::POST, "/api/tools"),
        (Method::HEAD, "/api/tools"),
        (Method::GET, "/api/tools/"),
        (Method::GET, "/api/tools/fixture/"),
        (Method::POST, "/api/tools/fixture/enable/"),
        (Method::POST, "/api/tools//enable"),
        (Method::POST, "/api/tools/fixture/extra/enable"),
        (Method::DELETE, "/api/tools/fixture"),
        (Method::PUT, "/api/tools/fixture/settings"),
        (Method::POST, "/tools"),
        (Method::POST, "/tools/_host/v1/cli/fixture"),
        (Method::POST, "/tools/_host/v1/cli/fixture/open"),
        (Method::PUT, "/tools/_host/v1/cli/fixture/autostart"),
        (Method::DELETE, "/tools/_host/v1/cli/session/x"),
        (Method::POST, "/tools/fixture/api/x"),
        (Method::PUT, "/tools/fixture/api/x"),
        (Method::DELETE, "/tools/fixture/api/x"),
        (Method::OPTIONS, "/tools/fixture/"),
        (Method::POST, "/tools/_host/v1/session/"),
    ] {
        assert_eq!(classify(&method, path), None, "{method} {path}");
    }
    assert_eq!(classify(&Method::GET, "/tools"), Some(Endpoint::Tools));
    assert_eq!(
        classify(&Method::POST, "/tools/_host/v1/session"),
        Some(Endpoint::ToolSession)
    );
    // Reads under `/tools/` reach only the tool host's exact registrations,
    // which answer 404 for every deleted host endpoint and `api/*` path.
    for path in [
        "/tools/fixture/",
        "/tools/_host/v2/bridge.js",
        "/tools/_host/v1/self",
        "/tools/fixture/api/x",
    ] {
        assert_eq!(
            classify(&Method::GET, path),
            Some(Endpoint::ToolHost),
            "{path}"
        );
        assert_eq!(
            classify(&Method::HEAD, path),
            Some(Endpoint::ToolHost),
            "{path}"
        );
    }
    let path = "/api/tools/fixture/enable";
    for dashboard in [false, true] {
        for csrf in [false, true] {
            for origin in ["http://localhost:24860", "http://localhost:24861"] {
                let (providers, origins) = split_providers(dashboard);
                let mut request = make_request(Method::POST, path, Body::empty());
                request.headers_mut().insert(
                    header::HOST,
                    HeaderValue::from_static(if dashboard {
                        "localhost:24861"
                    } else {
                        "localhost:24860"
                    }),
                );
                request
                    .headers_mut()
                    .insert(header::ORIGIN, HeaderValue::from_static(origin));
                if csrf {
                    request
                        .headers_mut()
                        .insert("x-vosu-csrf", HeaderValue::from_str(&origins.csrf).unwrap());
                }
                let status = bounded!(dispatch(State(providers), request)).status();
                assert_eq!(
                    status,
                    if !dashboard {
                        StatusCode::NOT_FOUND
                    } else if csrf && origin == "http://localhost:24861" {
                        StatusCode::OK
                    } else {
                        StatusCode::FORBIDDEN
                    }
                );
            }
        }
    }
}
