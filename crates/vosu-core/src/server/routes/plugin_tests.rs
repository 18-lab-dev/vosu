//! `/api/plugins` placement (catalog-v2.md A5.5 "Management routes", "API
//! placement"): exact methods, the dashboard listener only, and
//! dashboard-origin provenance plus `X-Vosu-CSRF` on every `POST` and
//! `DELETE`. A near miss never reaches the management handler.

use super::*;
use vosu_proptest::bounded;

struct Reached;
impl RouteDelegate for Reached {
    fn handle(&self, endpoint: Endpoint, _: Request, _: Bytes) -> ProviderFuture<'_, Response> {
        assert_eq!(endpoint, Endpoint::PluginManagement);
        Box::pin(async {
            Response::builder()
                .status(StatusCode::IM_A_TEAPOT)
                .body(Body::empty())
                .unwrap()
        })
    }
}

const OVERLAY: &str = "http://127.0.0.1:24870";
const DASHBOARD: &str = "http://127.0.0.1:24871";

/// Providers for the overlay listener (`Some(false)`), the dashboard
/// listener (`Some(true)`) or a listener without origins (`None`).
fn providers(
    listener: Option<bool>,
) -> (RouteProviders, Option<Arc<super::super::origins::Origins>>) {
    let policy: Arc<dyn RequestAuthorizer> =
        Arc::new(AuthorizationPolicy::new("127.0.0.1,localhost", "127.0.0.1"));
    let mut providers = RouteProviders {
        authorization: Arc::clone(&policy),
        delegate: Arc::new(Reached),
    };
    let Some(dashboard) = listener else {
        return (providers, None);
    };
    let origins =
        Arc::new(super::super::origins::Origins::new(OVERLAY.into(), DASHBOARD.into()).unwrap());
    providers.authorization = Arc::new(super::super::origins::OriginAuthorizer {
        inner: policy,
        origins: Arc::clone(&origins),
        dashboard,
    });
    (providers, Some(origins))
}

fn request(
    method: Method,
    path: &str,
    host: &str,
    origin: Option<&str>,
    csrf: Option<&str>,
) -> Request {
    let mut builder = Request::builder()
        .method(method)
        .uri(path)
        .header(header::HOST, host.trim_start_matches("http://"))
        .extension(ConnectInfo(
            "127.0.0.1:41234".parse::<SocketAddr>().unwrap(),
        ));
    if let Some(origin) = origin {
        builder = builder.header(header::ORIGIN, origin);
    }
    if let Some(csrf) = csrf {
        builder = builder.header("x-vosu-csrf", csrf);
    }
    builder.body(Body::from("{}")).unwrap()
}

#[test]
fn plugin_routes_have_exact_methods_and_shapes() {
    let pm = Some(Endpoint::PluginManagement);
    for (method, path, expected) in [
        (Method::GET, "/api/plugins", pm),
        (Method::POST, "/api/plugins", None),
        (Method::DELETE, "/api/plugins", None),
        (Method::POST, "/api/plugins/test.u0/approval", pm),
        (Method::DELETE, "/api/plugins/test.u0/approval", pm),
        (Method::POST, "/api/plugins/test%2Eu0/restart", pm),
        (Method::POST, "/api/plugins/test.u0/stop", pm),
        (Method::GET, "/api/plugins/test.u0/settings", pm),
        (Method::POST, "/api/plugins/test.u0/settings", pm),
        (Method::PUT, "/api/plugins/test.u0/settings", None),
        (Method::DELETE, "/api/plugins/test.u0/settings", None),
        (Method::GET, "/api/plugins/test.u0/settings/", None),
        (Method::GET, "/api/plugins//settings", None),
        (Method::GET, "/api/plugins/test.u0/approval", None),
        (Method::PUT, "/api/plugins/test.u0/approval", None),
        (Method::DELETE, "/api/plugins/test.u0/restart", None),
        (Method::DELETE, "/api/plugins/test.u0/stop", None),
        (Method::POST, "/api/plugins//approval", None),
        (Method::POST, "/api/plugins/test.u0/approval/", None),
        (Method::POST, "/api/plugins/test.u0/other/approval", None),
        (Method::POST, "/api/plugins/test.u0/enable", None),
        (Method::HEAD, "/api/plugins", None),
        (Method::OPTIONS, "/api/plugins", None),
    ] {
        assert_eq!(classify(&method, path), expected, "{method} {path}");
    }
}

#[tokio::test]
async fn plugin_mutations_need_the_dashboard_origin_and_csrf_token() {
    for (method, path) in [
        (Method::POST, "/api/plugins/test.u0/approval"),
        (Method::DELETE, "/api/plugins/test.u0/approval"),
        (Method::POST, "/api/plugins/test.u0/restart"),
        (Method::POST, "/api/plugins/test.u0/stop"),
        (Method::POST, "/api/plugins/test.u0/settings"),
    ] {
        // (Origin, whether the launch's token is sent, else a wrong one).
        for (origin, valid_token, expected) in [
            (Some(DASHBOARD), Some(true), StatusCode::IM_A_TEAPOT),
            (Some(DASHBOARD), None, StatusCode::FORBIDDEN),
            (Some(DASHBOARD), Some(false), StatusCode::FORBIDDEN),
            (None, Some(true), StatusCode::FORBIDDEN),
            (Some(OVERLAY), Some(true), StatusCode::FORBIDDEN),
            (
                Some("http://attacker.example"),
                Some(true),
                StatusCode::FORBIDDEN,
            ),
        ] {
            let (providers, origins) = providers(Some(true));
            let token = valid_token.map(|valid| {
                if valid {
                    origins.as_ref().unwrap().csrf.clone()
                } else {
                    "not-the-token".to_owned()
                }
            });
            let response = bounded!(dispatch(
                State(providers),
                request(method.clone(), path, DASHBOARD, origin, token.as_deref())
            ));
            assert_eq!(
                response.status(),
                expected,
                "{method} {path} {origin:?} {valid_token:?}"
            );
        }
    }
}

#[tokio::test]
async fn plugin_routes_exist_only_on_the_dashboard_listener() {
    // The listing needs no token, but only the dashboard listener serves it.
    let (providers_dashboard, _) = providers(Some(true));
    let listed = bounded!(dispatch(
        State(providers_dashboard),
        request(Method::GET, "/api/plugins", DASHBOARD, None, None)
    ));
    assert_eq!(listed.status(), StatusCode::IM_A_TEAPOT);
    for (method, path) in [
        (Method::GET, "/api/plugins"),
        (Method::POST, "/api/plugins/test.u0/approval"),
        (Method::DELETE, "/api/plugins/test.u0/approval"),
        (Method::POST, "/api/plugins/test.u0/restart"),
        (Method::GET, "/api/plugins/test.u0/settings"),
        (Method::POST, "/api/plugins/test.u0/settings"),
    ] {
        let (overlay, origins) = providers(Some(false));
        let csrf = origins.unwrap().csrf.clone();
        let response = bounded!(dispatch(
            State(overlay),
            request(method.clone(), path, OVERLAY, Some(OVERLAY), Some(&csrf))
        ));
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "overlay {method} {path}"
        );
        let (bare, _) = providers(None);
        let response = bounded!(dispatch(
            State(bare),
            request(method.clone(), path, OVERLAY, None, None)
        ));
        assert_eq!(
            response.status(),
            StatusCode::NOT_FOUND,
            "no origins {method} {path}"
        );
    }
}
