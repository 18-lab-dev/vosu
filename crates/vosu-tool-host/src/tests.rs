use super::*;
use bytes::Bytes;
use std::collections::HashMap;
use vosu_surface::{
    files::{
        DirectoryReader, FileError, FileMetadata, FileReader, FilesRequester, OpenedFile,
        Operation, RelativeChild, RootLease, RootOperations,
    },
    resources::{PublishedResource, ResourceRequester},
    routes::{ProviderResponse, ResponseBody, SurfaceRequest, SurfaceResponse},
    state::{
        CapturedSelection, IdentifiedRoster, IdentifiedSession, SessionTarget, SurfaceError,
        SurfaceSubscription,
    },
    tool_view::{self, ToolHostView, ToolHostViewSender, ToolPresentation},
};

#[derive(Clone, Default)]
struct Files(Arc<HashMap<String, Bytes>>);
struct Reader(Bytes);
impl FileReader for Reader {
    fn read(&self, offset: u64, len: u32) -> Operation<'_, Vec<u8>> {
        let start = (offset as usize).min(self.0.len());
        let end = (start + len as usize).min(self.0.len());
        let bytes = self.0[start..end].to_vec();
        Box::pin(async move { Ok(bytes) })
    }
}
impl RootOperations for Files {
    fn open(&self, path: &RelativeChild) -> Operation<'_, OpenedFile> {
        let found = self.0.get(path.as_str()).cloned();
        let directory = self
            .0
            .keys()
            .any(|key| key.starts_with(&format!("{}/", path.as_str())));
        Box::pin(async move {
            match found {
                Some(bytes) => Ok(OpenedFile::new(
                    FileMetadata {
                        size: bytes.len() as u64,
                        modified: None,
                        identity: Arc::from(&b"id"[..]),
                    },
                    Reader(bytes),
                )),
                None if directory => Err(FileError::InvalidPath),
                None => Err(FileError::NotFound),
            }
        })
    }
    fn directory(&self, _: &RelativeChild) -> Operation<'_, Box<dyn DirectoryReader>> {
        Box::pin(async { Err(FileError::PermissionDenied) })
    }
}

fn tool(id: &str, servable: bool) -> Arc<ToolPresentation> {
    let files = [
        ("web/index.html", "<h1>entry</h1>"),
        ("web/app.js", "export {}"),
        ("web/sub/index.html", "sub"),
        ("web/api/x", "must never be served"),
        ("art/icon.svg", "<svg/>"),
    ]
    .into_iter()
    .map(|(path, body)| (path.to_owned(), Bytes::from(body)))
    .collect();
    Arc::new(ToolPresentation {
        id: id.into(),
        name: format!("Tool \"{id}\""),
        version: "1.2.3".into(),
        servable,
        ui_root: "web".into(),
        ui_entry: "index.html".into(),
        icon: Some("art/icon.svg".into()),
        banner: None,
        settings: Some(Bytes::from_static(br#"{"label":"hi"}"#)),
        root: RootLease::new(Files(Arc::new(files))),
    })
}

fn publish(sender: &ToolHostViewSender, generation: u64, tools: Vec<Arc<ToolPresentation>>) {
    sender.send_replace(Arc::new(ToolHostView {
        generation,
        dashboard_origin: Some("http://127.0.0.1:25151".into()),
        tools,
    }));
}

fn request(method: &str, path: &str) -> SurfaceRequest {
    SurfaceRequest {
        method: method.into(),
        path: path.into(),
        query: Vec::new(),
        headers: Vec::new(),
        body: Arc::from([]),
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
        management: None,
    }
}

async fn respond(routes: &routes::Routes, method: &str, path: &str) -> (SurfaceResponse, Bytes) {
    let ProviderResponse::Http(response) = routes.respond(request(method, path)).await else {
        panic!("tool host never returns a feed");
    };
    assert!(response.valid_metadata(), "{path}");
    let body = match &response.body {
        ResponseBody::Empty => Bytes::new(),
        ResponseBody::Bytes(bytes) => bytes.clone(),
        ResponseBody::File {
            file,
            offset,
            length,
        } => Bytes::from(file.read(*offset, *length as u32).await.unwrap()),
    };
    (response, body)
}
fn header<'a>(response: &'a SurfaceResponse, name: &str) -> Option<&'a str> {
    response
        .headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
}

#[tokio::test]
async fn static_ui_art_settings_and_deleted_proxy_paths() {
    let (sender, views) = tool_view::channel(None);
    publish(&sender, 1, vec![tool("demo", true), tool("off", false)]);
    let routes = routes::Routes { views };
    let (response, body) = respond(&routes, "GET", "/tools/demo/").await;
    assert_eq!((response.status, &body[..]), (200, &b"<h1>entry</h1>"[..]));
    assert_eq!(
        header(&response, "content-type"),
        Some("text/html; charset=utf-8")
    );
    let (response, body) = respond(&routes, "HEAD", "/tools/demo/app.js").await;
    assert_eq!(response.status, 200);
    assert!(body.is_empty());
    assert_eq!(header(&response, "content-length"), Some("9"));
    let (response, _) = respond(&routes, "GET", "/tools/demo").await;
    assert_eq!(response.status, 302);
    assert_eq!(header(&response, "location"), Some("/tools/demo/"));
    let (response, _) = respond(&routes, "GET", "/tools/demo/sub").await;
    assert_eq!(header(&response, "location"), Some("/tools/demo/sub/"));
    assert_eq!(respond(&routes, "GET", "/tools/demo/sub/").await.1, "sub");
    // Counterexample: a deleted `/tools/<id>/api/*` proxy route never falls
    // through to a static file of the same name, in any spelling or method.
    for path in [
        "/tools/demo/api/x",
        "/tools/demo/%61pi/x",
        "/tools/demo/api",
        "/tools/demo/api/",
    ] {
        for method in ["GET", "HEAD"] {
            assert_eq!(respond(&routes, method, path).await.0.status, 404, "{path}");
        }
    }
    for path in [
        "/tools/demo/%2e%2e/art/icon.svg",
        "/tools/demo/..%2fart/icon.svg",
        "/tools/demo/%252e%252e/art/icon.svg",
        "/tools/demo/%zz",
        "/tools/demo/missing.js",
        "/tools/unknown/",
        "/tools/_host/v1/self",
        "/tools/_host/v1/bridge.js",
    ] {
        assert_eq!(respond(&routes, "GET", path).await.0.status, 404, "{path}");
    }
    assert_eq!(respond(&routes, "POST", "/tools/demo/").await.0.status, 404);
    let (response, body) = respond(&routes, "GET", "/tools/demo/_settings").await;
    assert_eq!(
        (response.status, &body[..]),
        (200, &br#"{"values":{"label":"hi"}}"#[..])
    );
    assert_eq!(
        respond(&routes, "HEAD", "/tools/demo/_settings")
            .await
            .0
            .status,
        404
    );
    // Not servable: art still identifies the tool; UI and settings are 503.
    for path in ["/tools/off/", "/tools/off/app.js", "/tools/off/_settings"] {
        assert_eq!(respond(&routes, "GET", path).await.0.status, 503, "{path}");
    }
    let (response, body) = respond(&routes, "GET", "/tools/off/_icon").await;
    assert_eq!((response.status, &body[..]), (200, &b"<svg/>"[..]));
    assert!(header(&response, "content-security-policy").is_some_and(|v| v.contains("sandbox")));
    assert_eq!(
        respond(&routes, "GET", "/tools/off/_banner").await.0.status,
        404
    );
}

#[tokio::test]
async fn bridge_theme_design_and_design_system_bytes_are_exact() {
    let (sender, views) = tool_view::channel(None);
    publish(&sender, 1, vec![tool("demo", true)]);
    let routes = routes::Routes { views };
    let (response, body) = respond(&routes, "GET", routes::BRIDGE).await;
    assert_eq!(
        header(&response, "content-type"),
        Some("text/javascript; charset=utf-8")
    );
    let source = std::str::from_utf8(&body).unwrap();
    assert!(source.starts_with(
        "const metadata = [{\"id\":\"demo\",\"name\":\"Tool \\\"demo\\\"\",\"version\":\"1.2.3\"}];\nconst dashboardUrl = \"http://127.0.0.1:25151\";\n"
    ));
    assert!(source.ends_with(include_str!("../../../assets/server/tool-bridge.js")));
    let (_, body) = respond(&routes, "GET", routes::THEME).await;
    assert_eq!(
        body,
        format!(
            "{}\n{}",
            include_str!("../../../assets/server/tokens.css"),
            include_str!("../../../assets/server/system.css")
        )
    );
    let (_, body) = respond(&routes, "GET", routes::DESIGN).await;
    assert_eq!(
        &body[..],
        include_bytes!("../../../assets/server/design.html")
    );
    for (path, bytes) in assets::DESIGN_SYSTEM {
        let (response, body) = respond(&routes, "GET", path).await;
        assert_eq!((response.status, &body[..]), (200, *bytes), "{path}");
    }
    // The latest view replaces the metadata; a dashboard that is off is null.
    publish(&sender, 2, Vec::new());
    let (_, body) = respond(&routes, "GET", routes::BRIDGE).await;
    assert!(
        std::str::from_utf8(&body)
            .unwrap()
            .starts_with("const metadata = [];\n")
    );
    let (off_sender, off_views) = tool_view::channel(None);
    drop(off_sender);
    let (_, body) = respond(&routes::Routes { views: off_views }, "GET", routes::BRIDGE).await;
    assert!(
        std::str::from_utf8(&body)
            .unwrap()
            .contains("const dashboardUrl = null;\n")
    );
    // Exact methods only.
    for method in ["HEAD", "POST"] {
        assert_eq!(respond(&routes, method, routes::BRIDGE).await.0.status, 404);
    }
}

type Registered = (u64, RouteDescriptor);
#[derive(Default)]
struct Requester {
    next: Mutex<u64>,
    routes: Arc<Mutex<Vec<Registered>>>,
}
struct Release(Arc<Mutex<Vec<Registered>>>, u64);
impl Drop for Release {
    fn drop(&mut self) {
        self.0.lock().unwrap().retain(|(owner, _)| *owner != self.1);
    }
}
impl Requester {
    fn paths(&self) -> Vec<(String, String)> {
        let mut paths: Vec<_> = self
            .routes
            .lock()
            .unwrap()
            .iter()
            .map(|(_, route)| {
                let path = match &route.path {
                    RoutePath::Exact(path) => format!("exact {path}"),
                    RoutePath::Prefix(path) => format!("prefix {path}"),
                    other => format!("{other:?}"),
                };
                (route.method.clone(), path)
            })
            .collect();
        paths.sort();
        paths
    }
}
impl FilesRequester for Requester {
    fn declared_root(&self, _: &IdentifiedSession, _: &str) -> Result<RootLease, FileError> {
        Err(FileError::PermissionDenied)
    }
    fn open_calculation_path(&self, _: &IdentifiedSession, _: &str) -> Operation<'_, OpenedFile> {
        Box::pin(async { Err(FileError::PermissionDenied) })
    }
}
impl ResourceRequester for Requester {
    fn open_published_resource(
        &self,
        _: &IdentifiedSession,
        _: &str,
    ) -> Operation<'_, PublishedResource> {
        Box::pin(async { Err(FileError::PermissionDenied) })
    }
}
impl BundledRequester for Requester {
    fn capture(
        &self,
        _: SessionTarget,
        _: &[vosu_plugin_api::Selector],
    ) -> Result<CapturedSelection, SurfaceError> {
        Err(SurfaceError::UnknownReader)
    }
    fn capture_roster(&self, _: &str) -> Result<IdentifiedRoster, SurfaceError> {
        Err(SurfaceError::UnknownReader)
    }
    fn subscribe(
        &self,
        _: SessionTarget,
        _: Vec<vosu_plugin_api::Selector>,
    ) -> Result<SurfaceSubscription, SurfaceError> {
        Err(SurfaceError::UnknownReader)
    }
    fn register_routes(
        &self,
        routes: Vec<(RouteDescriptor, Arc<dyn RouteProvider>)>,
    ) -> Result<RegistrationLease, RegistrationError> {
        let mut registered = self.routes.lock().unwrap();
        if routes
            .iter()
            .any(|(route, _)| registered.iter().any(|(_, other)| other == route))
        {
            return Err(RegistrationError::Collision);
        }
        let mut next = self.next.lock().unwrap();
        *next += 1;
        registered.extend(routes.into_iter().map(|(route, _)| (*next, route)));
        Ok(RegistrationLease::new(Release(self.routes.clone(), *next)))
    }
}

fn expected(ids: &[&str]) -> Vec<(String, String)> {
    let mut paths: Vec<_> = [routes::BRIDGE, routes::THEME, routes::DESIGN]
        .iter()
        .map(|path| ("GET".to_owned(), format!("exact {path}")))
        .chain(ids.iter().flat_map(|id| {
            ["GET", "HEAD"].map(|method| (method.to_owned(), format!("prefix /tools/{id}")))
        }))
        .collect();
    paths.sort();
    paths
}

async fn settled(requester: &Requester, ids: &[&str]) {
    for _ in 0..200 {
        if requester.paths() == expected(ids) {
            return;
        }
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(requester.paths(), expected(ids));
}

#[tokio::test]
async fn registrations_are_exact_and_follow_the_latest_view() {
    let requester = Arc::new(Requester::default());
    let (sender, views) = tool_view::channel(None);
    publish(&sender, 1, vec![tool("alpha", true)]);
    let host = ToolHost::mount(requester.clone(), views).unwrap();
    // No `/tools` prefix wildcard: only exact host routes and one prefix per
    // presented tool, so removed `/tools/_host/v1/*` paths have no route.
    assert_eq!(requester.paths(), expected(&["alpha"]));
    publish(&sender, 2, vec![tool("alpha", false), tool("beta", true)]);
    settled(&requester, &["alpha", "beta"]).await;
    publish(&sender, 3, vec![tool("beta", true)]);
    settled(&requester, &["beta"]).await;
    // An invalid route segment is never registered.
    publish(&sender, 4, vec![tool("_host", true), tool("a/b", true)]);
    settled(&requester, &[]).await;
    assert!(host.last_refusal().is_none());
    let both = [ListenerRole::Overlay, ListenerRole::Dashboard];
    host.mount_design_system(&both).unwrap();
    host.mount_design_system(&both).unwrap();
    let design = requester
        .paths()
        .into_iter()
        .filter(|(_, path)| path.contains("/assets/") || path.ends_with("/favicon.ico"))
        .count();
    assert_eq!(design, 2 * assets::DESIGN_SYSTEM.len());
    drop(host);
    assert!(requester.paths().is_empty());
}

/// A7.3: with the dashboard off the design-system set is registered on the
/// overlay role only.
#[tokio::test]
async fn design_system_follows_the_requested_listener_roles() {
    let requester = Arc::new(Requester::default());
    let (_sender, views) = tool_view::channel(None);
    let host = ToolHost::mount(requester.clone(), views).unwrap();
    host.mount_design_system(&[ListenerRole::Overlay]).unwrap();
    let design: Vec<ListenerRole> = requester
        .routes
        .lock()
        .unwrap()
        .iter()
        .filter(|(_, route)| {
            matches!(&route.path, RoutePath::Exact(path)
                if path.starts_with("/assets/") || path == "/favicon.ico")
        })
        .map(|(_, route)| route.listener)
        .collect();
    assert_eq!(design.len(), assets::DESIGN_SYSTEM.len());
    assert!(design.iter().all(|role| *role == ListenerRole::Overlay));
    drop(host);
}
