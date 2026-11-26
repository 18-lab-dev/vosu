use super::*;
use crate::{
    catalog::CatalogService,
    commands::{Relay, RelaySink, SettingsProvider},
};
use tokio_util::sync::CancellationToken;
use vosu_surface::{
    management::{ManagementAuthority, ManagementProvenance},
    state::{CapturedSelection, IdentifiedRoster},
};

struct NoSettings;
impl SettingsProvider for NoSettings {
    fn get(&self, _: &str) -> Result<Value, String> {
        Ok(json!({}))
    }
    fn save(&self, _: &str, _: Value) -> Result<(), String> {
        Ok(())
    }
}
impl RelaySink for NoSettings {
    fn relay(&self, _: Relay) {}
}

struct Fixture {
    root: tempfile::TempDir,
    routes: OverlayRoutes,
    processor: Arc<CommandProcessor>,
}

fn fixture() -> Fixture {
    let root = tempfile::tempdir().unwrap();
    std::fs::create_dir_all(root.path().join("static")).unwrap();
    let counters = Arc::new(ReloadableCounterService::production(
        root.path().join("static"),
        root.path().join("config/settings"),
        root.path().join("cache"),
    ));
    let processor = Arc::new(CommandProcessor::new(
        Arc::new(NoSettings),
        Arc::new(NoSettings),
    ));
    let presenter = OverlayPresenter::new(
        Arc::clone(&counters),
        CatalogService::production("2.0.0"),
        CancellationToken::new(),
    );
    let routes = OverlayRoutes::new(
        counters,
        presenter,
        Arc::clone(&processor),
        Arc::new(|| unreachable!("no command feed in these tests")),
    );
    Fixture {
        root,
        routes,
        processor,
    }
}

fn witness() -> ManagementProvenance {
    ManagementAuthority::default()
        .authorize(ListenerRole::Dashboard, Some("csrf"), "csrf")
        .unwrap()
}

fn request(
    method: &str,
    path: &str,
    body: &[u8],
    management: Option<ManagementProvenance>,
) -> SurfaceRequest {
    let (path, query) = path.split_once('?').unwrap_or((path, ""));
    SurfaceRequest {
        method: method.into(),
        path: path.into(),
        query: url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect(),
        headers: Vec::new(),
        body: Arc::from(body),
        context: Arc::new(CapturedSelection {
            session: None,
            focus_revision: 0,
            roster: IdentifiedRoster {
                revision: 0,
                sessions: Arc::from([]),
            },
            revision: 0,
            namespace_revisions: Arc::from([]),
            values: Arc::from([]),
        }),
        management,
    }
}

async fn send(routes: &OverlayRoutes, request: SurfaceRequest) -> (u16, Value) {
    let ProviderResponse::Http(response) = routes.respond(request).await else {
        panic!("an HTTP route answered with a feed");
    };
    let ResponseBody::Bytes(body) = response.body else {
        panic!("counter routes answer with bytes");
    };
    (
        response.status,
        serde_json::from_slice(&body).unwrap_or(Value::Null),
    )
}

#[test]
fn registrations_follow_the_a7_listener_roles() {
    let routes = OverlayRoutes::descriptors();
    let roles = |path: &str, method: &str| {
        routes
            .iter()
            .filter(|route| {
                route.method == method
                    && match &route.path {
                        RoutePath::Exact(exact) => exact == path,
                        RoutePath::Children(base) => path
                            .strip_prefix(base.as_str())
                            .is_some_and(|rest| rest.len() > 1 && rest.starts_with('/')),
                        _ => false,
                    }
            })
            .map(|route| (route.listener, route.websocket))
            .collect::<Vec<_>>()
    };
    use ListenerRole::{Dashboard, Overlay};
    for (method, path) in [
        ("GET", "/api/counters/search/x"),
        ("GET", "/api/counters/details/x"),
        ("GET", "/api/counters/updates"),
        ("POST", "/api/counters/names/x"),
        ("POST", "/api/counters/download/x"),
        ("POST", "/api/counters/delete/x"),
    ] {
        assert_eq!(roles(path, method), [(Dashboard, false)], "{method} {path}");
    }
    for method in ["GET", "POST"] {
        for path in ["/api/counters/settings/x", "/api/counters/profiles"] {
            assert_eq!(
                roles(path, method),
                [(Overlay, false), (Dashboard, false)],
                "{method} {path}"
            );
        }
    }
    assert_eq!(roles(COMMANDS, "GET"), [(Overlay, true)]);
    // Folder opening stays in core (desktop integration).
    for path in ["/api/counters/open-root", "/api/counters/open/x"] {
        assert!(roles(path, "POST").is_empty(), "{path}");
    }
    assert_eq!(routes.len(), 15);
}

#[tokio::test]
async fn details_report_safe_folder_facts_and_reject_traversal() {
    let fixture = fixture();
    let folder = fixture.root.path().join("static/counter");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("index.html"), b"12345").unwrap();
    let (status, details) = send(
        &fixture.routes,
        request("GET", "/api/counters/details/counter", b"", None),
    )
    .await;
    assert_eq!(status, 200);
    assert_eq!(details["bytes"], 5);
    assert_eq!(details["complete"], true);
    // Compare canonical forms: the reported path is canonical, while `folder` may
    // contain an 8.3 short name (hosted Windows TEMP is `C:\Users\RUNNER~1\...`).
    let reported = details["path"].as_str().unwrap();
    assert!(
        !reported.starts_with(r"\\?\"),
        "verbatim prefix leaked: {reported}"
    );
    assert_eq!(
        std::fs::canonicalize(reported).unwrap(),
        std::fs::canonicalize(&folder).unwrap()
    );
    let (status, _) = send(
        &fixture.routes,
        request("GET", "/api/counters/details/%2E%2E", b"", None),
    )
    .await;
    assert_eq!(status, 400);
    fixture.processor.shutdown().await;
}

#[tokio::test]
async fn names_save_beside_the_settings_only_with_the_core_witness() {
    let fixture = fixture();
    std::fs::create_dir_all(fixture.root.path().join("static/accuracy by sxlvia")).unwrap();
    let path = "/api/counters/names/accuracy%20by%20sxlvia";
    let body = br#"{"nickname":"Acc","alias":"acc"}"#;
    // Without core's dashboard witness nothing is written.
    assert_eq!(
        send(&fixture.routes, request("POST", path, body, None))
            .await
            .0,
        403
    );
    assert!(
        !fixture
            .root
            .path()
            .join("config/overlay-names.json")
            .exists()
    );
    let reserved = br#"{"nickname":"Acc","alias":"tools"}"#;
    assert_eq!(
        send(
            &fixture.routes,
            request("POST", path, reserved, Some(witness()))
        )
        .await
        .0,
        400
    );
    assert_eq!(
        send(
            &fixture.routes,
            request("POST", path, body, Some(witness()))
        )
        .await
        .0,
        200
    );
    assert!(
        fixture
            .root
            .path()
            .join("config/overlay-names.json")
            .is_file()
    );
    fixture.processor.shutdown().await;
}

#[tokio::test]
async fn dashboard_mutations_without_the_witness_change_nothing() {
    let fixture = fixture();
    let folder = fixture.root.path().join("static/expendable");
    std::fs::create_dir_all(&folder).unwrap();
    std::fs::write(folder.join("index.html"), b"x").unwrap();
    for (path, body) in [
        ("/api/counters/delete/expendable", &b""[..]),
        (
            "/api/counters/download/https%3A%2F%2Fexample.com%2Fa.zip?name=fresh&update=false",
            b"",
        ),
        ("/api/counters/settings/expendable?update=yes", b"[]"),
    ] {
        assert_eq!(
            send(&fixture.routes, request("POST", path, body, None))
                .await
                .0,
            403,
            "{path}"
        );
    }
    assert!(folder.join("index.html").is_file());
    assert!(!fixture.root.path().join("static/fresh").exists());
    fixture.processor.shutdown().await;
}

#[test]
fn search_source_is_explicit_and_errors_are_escaped_non_200() {
    let pairs = |query: &str| {
        url::form_urlencoded::parse(query.as_bytes())
            .into_owned()
            .collect::<Vec<_>>()
    };
    assert_eq!(search_source(&pairs("")), Some(false));
    assert_eq!(search_source(&pairs("source=installed")), Some(false));
    assert_eq!(search_source(&pairs("x=1&source=available")), Some(true));
    assert_eq!(search_source(&pairs("source=Available")), None);
    assert_eq!(search_source(&pairs("source=")), None);

    let failed = search_response(Err(r#"<img src=x onerror="alert(1)">"#.into()));
    assert_eq!(failed.status, 502);
    let ResponseBody::Bytes(body) = failed.body else {
        panic!("search errors are bytes");
    };
    assert_eq!(
        &body[..],
        b"Error: &lt;img src=x onerror=&quot;alert(1)&quot;&gt;"
    );
    assert_eq!(
        search_response(Ok(Bytes::from_static(b"<article></article>"))).status,
        200
    );
}
