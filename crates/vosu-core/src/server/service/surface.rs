// Permanent surface dispatch after ordinary transport admission.
use vosu_surface::routes::{
    ProviderResponse, ResponseBody, ResponseFuture, SurfaceRequest, SurfaceResponse,
};

/// Core's own routes (A7 route inventory), reserved so no bundled host can
/// register them. Every row is exact, a template or a `Children` matcher
/// except `/api/v1` on the overlay role, which core's API-v1 router serves.
/// The page shells, every `/assets/<name>` and `/favicon.ico` are host
/// registrations (the dashboard host's set on the dashboard role, the tool
/// host's design-system set on both), so core reserves none of them.
///
/// With `dashboard` off (A7.3) core reserves nothing on the dashboard role:
/// no dashboard listener binds, so no route of its own may exist there.
pub fn reserve_core_routes(
    host: &Arc<crate::plugin::PluginHost>,
    dashboard: bool,
) -> Result<vosu_surface::routes::RegistrationLease, vosu_surface::routes::RegistrationError> {
    use vosu_surface::{
        listener::ListenerRole::{self, Dashboard, Overlay},
        routes::{RouteDescriptor, RoutePath},
    };
    let mut descriptors = Vec::new();
    let mut reserve = |listener: ListenerRole, method: &str, path: RoutePath| {
        descriptors.push(RouteDescriptor {
            listener,
            method: method.into(),
            path,
            websocket: false,
        });
    };
    let roles: &[ListenerRole] = if dashboard {
        &[Overlay, Dashboard]
    } else {
        &[Overlay]
    };
    // The O→D page redirects (404 while the dashboard is off).
    for path in ["/", "/settings", "/available", "/tools"] {
        reserve(Overlay, "GET", RoutePath::Exact(path.into()));
    }
    for &listener in roles {
        for (method, path) in [
            ("GET", "/api/dashboard"),
            ("GET", "/api/dashboard/status"),
            ("GET", "/api/keys"),
            ("POST", "/api/keys"),
            ("POST", "/api/settingsSave"),
            ("POST", "/api/runUpdates"),
            ("POST", "/api/generateReport"),
        ] {
            reserve(listener, method, RoutePath::Exact(path.into()));
        }
    }
    for method in ["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"] {
        reserve(Overlay, method, RoutePath::Prefix("/api/v1".into()));
    }
    // Core's exact tool routes (A7.2); no `/tools` or `/api/tools` prefix
    // reservation survives, so a deleted route has no owner at all.
    reserve(
        Overlay,
        "POST",
        RoutePath::Exact("/tools/_host/v1/session".into()),
    );
    if dashboard {
        reserve(Dashboard, "GET", RoutePath::Exact("/api/settings".into()));
        reserve(Dashboard, "GET", RoutePath::Exact("/api/tools".into()));
        for (method, path) in [
            ("GET", "/api/tools/{id}"),
            ("GET", "/api/tools/{id}/settings"),
            ("POST", "/api/tools/{id}/settings"),
            ("POST", "/api/tools/{id}/enable"),
            ("POST", "/api/tools/{id}/disable"),
            ("POST", "/api/tools/{id}/trust"),
            ("POST", "/api/tools/{id}/launch"),
        ] {
            reserve(Dashboard, method, RoutePath::Template(path.into()));
        }
        // Desktop integration stays in core (A7 route inventory): opening the
        // overlay root or one folder, dashboard only. Every other counter
        // route and the `/websocket/commands` feed are `vosu.overlayhost`'s.
        reserve(
            Dashboard,
            "POST",
            RoutePath::Exact("/api/counters/open-root".into()),
        );
        reserve(
            Dashboard,
            "POST",
            RoutePath::Children("/api/counters/open".into()),
        );
    }
    host.reserve_core_surface_routes(descriptors)
}

#[derive(Clone)]
pub struct SurfaceRouter {
    host: Arc<crate::plugin::PluginHost>,
    requester: Arc<dyn vosu_surface::requester::BundledRequester>,
}
impl SurfaceRouter {
    pub fn new(
        host: Arc<crate::plugin::PluginHost>,
        requester: Arc<dyn vosu_surface::requester::BundledRequester>,
    ) -> Self {
        Self { host, requester }
    }
    pub fn dispatch(
        &self,
        request: &Request<Body>,
        websocket: bool,
    ) -> ResponseFuture<'_, Option<ProviderResponse>> {
        let selected = self
            .requester
            .capture(vosu_surface::state::SessionTarget::Focused, &[])
            .ok()
            .and_then(|context| surface_request(request, Arc::new(context)));
        Box::pin(async move {
            self.host
                .dispatch_surface_route(
                    vosu_surface::listener::ListenerRole::Overlay,
                    websocket,
                    selected?,
                )
                .await
        })
    }
    pub fn http(&self, request: &Request<Body>) -> ProviderFuture<'_, Response> {
        let response = self.dispatch(request, false);
        Box::pin(async move {
            match response.await {
                Some(ProviderResponse::Http(response)) => surface_http_response(response),
                _ => not_found(),
            }
        })
    }

    /// One admitted counter API request for the overlay host's registrations
    /// on the listener it arrived on (A7), with its bounded body and, for an
    /// admitted dashboard mutation, core's management witness. Counter
    /// routes read no game state.
    pub fn overlay_host(&self, request: &Request<Body>, body: Bytes) -> ProviderFuture<'_, Response> {
        let admission = request
            .extensions()
            .get::<crate::server::routes::SurfaceAdmission>()
            .cloned();
        let listener = admission
            .as_ref()
            .map_or(vosu_surface::listener::ListenerRole::Overlay, |admission| admission.listener);
        let management = admission.and_then(|admission| {
            let expected = admission.csrf?;
            let supplied = request
                .headers()
                .get("x-vosu-csrf")
                .and_then(|value| value.to_str().ok());
            self.host
                .surface_management_provenance(listener, supplied, &expected)
        });
        let surface = surface_request(request, Arc::new(empty_selection())).map(|mut surface| {
            surface.body = Arc::from(&body[..]);
            surface.management = management;
            surface
        });
        Box::pin(async move {
            let Some(surface) = surface else {
                return not_found();
            };
            match self
                .host
                .dispatch_surface_route(listener, false, surface)
                .await
            {
                Some(ProviderResponse::Http(response)) => surface_http_response(response),
                _ => not_found(),
            }
        })
    }

    /// A stateless route registered on `listener`: the dashboard host's page
    /// shells and assets, or the tool host's design-system set. These
    /// providers read no state, so the request carries an empty selection;
    /// `None` when no provider serves the path on that listener.
    pub fn listener_route(
        &self,
        listener: vosu_surface::listener::ListenerRole,
        request: &Request<Body>,
    ) -> ProviderFuture<'_, Option<Response>> {
        let selected = surface_request(request, Arc::new(empty_selection()));
        Box::pin(async move {
            match self
                .host
                .dispatch_surface_route(listener, false, selected?)
                .await
            {
                Some(ProviderResponse::Http(response)) => Some(surface_http_response(response)),
                _ => None,
            }
        })
    }
}

/// The admitted request as a surface provider sees it: decoded query pairs,
/// only the conditional and range headers and no credentials. Only
/// [`SurfaceRouter::overlay_host`] adds the bounded body and core's management
/// witness. `None` when a forwarded header is not text or metadata exceeds
/// its bounds.
pub(super) fn surface_request(
    request: &Request<Body>,
    context: Arc<vosu_surface::state::CapturedSelection>,
) -> Option<SurfaceRequest> {
    let query: Vec<_> =
        url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
            .take(vosu_surface::routes::MAX_QUERY_PAIRS + 1)
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect();
    let headers = request
        .headers()
        .iter()
        .filter(|(name, _)| {
            matches!(
                name.as_str(),
                "range" | "if-none-match" | "if-modified-since" | "if-range"
            )
        })
        .map(|(k, v)| Some((k.to_string(), v.to_str().ok()?.to_owned())))
        .collect::<Option<Vec<_>>>()?;
    let request = SurfaceRequest {
        method: request.method().to_string(),
        path: request.uri().path().to_owned(),
        query,
        headers,
        body: Arc::from([]),
        context,
        management: None,
    };
    request.valid_metadata().then_some(request)
}

/// Overlay static serving reads no game state.
pub(super) fn empty_selection() -> vosu_surface::state::CapturedSelection {
    vosu_surface::state::CapturedSelection {
        session: None,
        focus_revision: 0,
        roster: vosu_surface::state::IdentifiedRoster {
            revision: 0,
            sessions: Arc::from([]),
        },
        revision: 0,
        namespace_revisions: Arc::from([]),
        values: Arc::from([]),
    }
}
impl RouteDelegate for SurfaceRouter {
    fn registered_http<'a>(
        &'a self,
        request: &'a Request<Body>,
    ) -> Option<ProviderFuture<'a, Response>> {
        Some(self.http(request))
    }
    fn handle(
        &self,
        _: Endpoint,
        request: Request<Body>,
        _: Bytes,
    ) -> ProviderFuture<'_, Response> {
        self.http(&request)
    }
}

/// A bytes body with its `content-type`.
pub fn surface_bytes(status: u16, content_type: &str, body: Bytes) -> SurfaceResponse {
    SurfaceResponse {
        status,
        headers: vec![("content-type".into(), content_type.into())],
        body: ResponseBody::Bytes(body),
    }
}
/// The production not-ready envelope of every state or calculation read.
pub fn surface_not_ready() -> SurfaceResponse {
    let (status, body) = CachedJson::NotReady.status_body();
    surface_bytes(status.as_u16(), JSON, body)
}
fn surface_http_response(response: SurfaceResponse) -> Response {
    let body = match response.body {
        ResponseBody::Empty => Body::empty(),
        ResponseBody::Bytes(bytes) => Body::from(bytes),
        ResponseBody::File {
            file,
            offset,
            length,
        } => Body::from_stream(futures_util::stream::try_unfold(
            (file, offset, length),
            |(file, offset, remaining)| async move {
                if remaining == 0 {
                    return Ok::<_, std::io::Error>(None);
                }
                let len = remaining.min(u64::from(vosu_surface::resources::MAX_READ_BYTES)) as u32;
                let bytes = file
                    .read(offset, len)
                    .await
                    .map_err(std::io::Error::other)?;
                if bytes.is_empty() || bytes.len() > len as usize {
                    return Err(std::io::Error::other("invalid retained file read"));
                }
                let count = bytes.len() as u64;
                Ok(Some((
                    Bytes::from(bytes),
                    (file, offset + count, remaining - count),
                )))
            },
        )),
    };
    let mut builder = Response::builder().status(response.status);
    for (name, value) in response.headers {
        // The sandboxed game-file surface serves untrusted client files.
        // Keep this browser policy host-owned, not a provider header grant.
        if name == "content-security-policy" && value == "sandbox" {
            builder = builder.header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
        }
        builder = builder.header(name, value);
    }
    builder.body(body).expect("core-validated surface response")
}
