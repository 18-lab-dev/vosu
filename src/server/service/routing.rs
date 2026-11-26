/// Core's exact tool registry routes (A7.2), so this module does not depend
/// on `crate::tools` directly: the overlay-listener launch-session exchange
/// and the dashboard-listener `/api/tools` management routes.
pub trait ToolRegistryRoutes: Send + Sync {
    /// `POST /tools/_host/v1/session`.
    fn session(self: Arc<Self>, body: Bytes) -> ProviderFuture<'static, Response>;
    /// `/api/tools...`: the 2.0 tool views, enable/disable/trust, launch and settings.
    fn dashboard(
        self: Arc<Self>,
        request: Request<Body>,
        body: Bytes,
    ) -> ProviderFuture<'static, Response>;
}

/// The dashboard's process-plugin management routes (catalog-v2.md A5.5
/// "Management routes"), so this module does not depend on the runtime that
/// owns the plugin host. Routing has already enforced the dashboard
/// listener, dashboard-origin provenance and `X-Vosu-CSRF` for mutations.
pub trait PluginManagementRoutes: Send + Sync {
    fn handle(self: Arc<Self>, request: Request<Body>, body: Bytes)
    -> ProviderFuture<'static, Response>;
}

#[derive(Clone)]
pub struct ProductionRouteDelegate {
    services: ProductionServices,
    api_keys: Option<Arc<super::api_keys::ApiKeys>>,
    connections: OverlayConnections,
    /// Core's one D20 recorder, owned by the runtime for the process lifetime.
    removed: Arc<vosu_core::removed_routes::RemovedRoutes>,
    tools: Option<Arc<dyn ToolRegistryRoutes>>,
    plugins: Option<Arc<dyn PluginManagementRoutes>>,
    catalog: Option<crate::server::catalog_api::CatalogApi>,
    api_v1: Option<crate::server::api_v1::router::ApiV1Router>,
    surface: Option<SurfaceRouter>,
    /// The overlay host's static provider (A7.1): core keeps the overlay
    /// listener's fallback classification and hands it this request.
    overlay_static: Option<Arc<vosu_overlay_host::OverlayStatic>>,
    opener: Arc<dyn super::overlay_folders::FolderOpener>,
    #[cfg(feature = "test-harness")]
    adapter_capture: Option<Arc<crate::server::adapter_capture::endpoint::AdapterCaptureEndpoint>>,
}

impl ProductionRouteDelegate {
    pub fn new(services: ProductionServices) -> Self {
        services.websocket.processor.bind_profiles(services.counters.settings_store());
        services
            .websocket
            .relays
            .bind_processor(&services.websocket.processor);
        Self { services, api_keys: None, connections: OverlayConnections::default(), removed: Arc::default(), tools: None, plugins: None, catalog: None, api_v1: None, surface: None, overlay_static: None, opener: Arc::new(super::overlay_folders::SystemOpener), #[cfg(feature = "test-harness")] adapter_capture: None }
    }
    pub fn with_overlay_static(mut self, serving: Arc<vosu_overlay_host::OverlayStatic>) -> Self {
        self.overlay_static = Some(serving);
        self
    }
    /// Core desktop integration for the open-root/open routes.
    pub fn with_opener(mut self, opener: Arc<dyn super::overlay_folders::FolderOpener>) -> Self {
        self.opener = opener;
        self
    }
    /// Shares the runtime's recorder, so recomposing routes keeps live notices.
    pub fn with_removed_routes(mut self, removed: Arc<vosu_core::removed_routes::RemovedRoutes>) -> Self {
        self.removed = removed;
        self
    }
    /// D20 consumes only bounded notices, revalidating identities before actionable links.
    pub fn removed_notices(&self) -> Vec<vosu_core::removed_routes::RemovedNotice> {
        self.removed_snapshot(std::time::Instant::now()).0
    }
    /// The A7.4 read: expiry, then each known identity revalidated against the
    /// overlays installed now (read once, only when a known notice exists),
    /// then the bounded snapshot with each identity's display name.
    fn removed_snapshot(&self, now: std::time::Instant) -> (Vec<vosu_core::removed_routes::RemovedNotice>, HashMap<String, String>) {
        let installed = std::cell::OnceCell::new();
        let names = || installed.get_or_init(|| installed_overlay_names(&self.services.counters));
        let notices = self.removed.snapshot(now, |overlay| names().contains_key(overlay));
        (notices, installed.into_inner().unwrap_or_default())
    }
    fn removed_routes(&self) -> Value {
        let now = std::time::Instant::now();
        let (notices, names) = self.removed_snapshot(now);
        removed_routes_view(&notices, &names, now)
    }

    pub fn with_api_keys(mut self, keys: Arc<super::api_keys::ApiKeys>) -> Self {
        self.api_keys = Some(keys);
        self
    }
    pub fn with_tools(mut self, tools: Arc<dyn ToolRegistryRoutes>) -> Self {
        self.tools = Some(tools);
        self
    }
    pub fn with_plugins(mut self, plugins: Arc<dyn PluginManagementRoutes>) -> Self {
        self.plugins = Some(plugins);
        self
    }
    /// M6b H0 dashboard-only catalog management facade.
    pub fn with_catalog(mut self, catalog: crate::server::catalog_api::CatalogApi) -> Self {
        self.catalog = Some(catalog);
        self
    }
    pub fn with_api_v1(mut self, router: crate::server::api_v1::router::ApiV1Router) -> Self {
        self.api_v1 = Some(router);
        self
    }
    pub fn with_surface(mut self, router: SurfaceRouter) -> Self {
        self.surface = Some(router);
        self
    }
    /// Mounts the test-harness adapter capture endpoint for this launch.
    #[cfg(feature = "test-harness")]
    pub fn with_adapter_capture(mut self, endpoint: crate::server::adapter_capture::endpoint::AdapterCaptureEndpoint) -> Self {
        self.adapter_capture = Some(Arc::new(endpoint));
        self
    }
    pub fn services(&self) -> &ProductionServices {
        &self.services
    }

    async fn dispatch(&self, endpoint: Endpoint, request: Request<Body>, body: Bytes) -> Response {
        match endpoint {
            Endpoint::Removed(feature) => {
                self.removed.admitted(feature);
                // A7.4: the dashboard's own previews are never recorded.
                if !is_preview_source(&request) {
                    let overlay = socket_overlay(&request).filter(|overlay| {
                        self.services.counters.settings_store().counter_values(overlay).is_ok()
                    });
                    self.removed.record(overlay, feature, std::time::Instant::now());
                }
                not_found()
            }
            Endpoint::ApiV1 => match &self.api_v1 {
                Some(router) => router.handle(request).await,
                None => crate::server::api_v1::error::error(StatusCode::NOT_FOUND, "invalid_request", "Unknown API route"),
            },
            Endpoint::ToolSession => match &self.tools {
                Some(tools) => Arc::clone(tools).session(body).await,
                None => not_found(),
            },
            // The bundled tool host's exact registrations; core adds nosniff
            // to every response it serves on the overlay origin.
            Endpoint::ToolHost => {
                let mut response = match &self.surface {
                    Some(surface) => surface.http(&request).await,
                    None => not_found(),
                };
                response.headers_mut().insert(
                    header::X_CONTENT_TYPE_OPTIONS,
                    axum::http::HeaderValue::from_static("nosniff"),
                );
                response
            }
            Endpoint::ToolManagement => match &self.tools {
                Some(tools) => Arc::clone(tools).dashboard(request, body).await,
                None => not_found(),
            },
            Endpoint::PluginManagement => match &self.plugins {
                Some(plugins) => Arc::clone(plugins).handle(request, body).await,
                None => not_found(),
            },
            Endpoint::Catalog => match &self.catalog {
                Some(catalog) => catalog.handle(request, body).await,
                None => not_found(),
            },
            Endpoint::State(_) | Endpoint::CalculatePp => {
                internal_error("route provider dispatch invariant violated")
            }
            // `vosu.overlayhost`'s registrations (A7); core already applied
            // the listener, key, broker and dashboard rules above.
            Endpoint::CounterSearch
            | Endpoint::CounterDownload
            | Endpoint::CounterDelete
            | Endpoint::CounterDetails
            | Endpoint::CounterNames
            | Endpoint::CounterUpdates
            | Endpoint::CounterSettings => match &self.surface {
                Some(surface) => surface.overlay_host(&request, body).await,
                None => not_found(),
            },
            Endpoint::CounterOpen => self.counter_open(&request),
            Endpoint::CounterOpenRoot => self.counter_open_root(),
            Endpoint::DashboardStatus => {
                let mut status = self.services.report_input.dashboard_status();
                status["connections"] = json!(self.connections.counts());
                status["removed_routes"] = self.removed_routes();
                json_response(StatusCode::OK, status)
            }
            Endpoint::ApiKeys => {
                let Some(keys) = &self.api_keys else { return internal_error("API key store unavailable"); };
                if request.method() == axum::http::Method::GET {
                    json_response(StatusCode::OK, json!({"keys": keys.list()}))
                } else {
                    match serde_json::from_slice::<Value>(&body).map_err(|_| "invalid JSON".to_owned()).and_then(|v| keys.manage(&v)) {
                        Ok(value) => json_response(StatusCode::OK, value),
                        Err(error) => json_response(StatusCode::BAD_REQUEST, json!({"error": error})),
                    }
                }
            }
            Endpoint::SettingsSave => self.settings_save(request.headers(), body).await,
            Endpoint::SettingsView => json_response(StatusCode::OK, self.services.config.view()),
            Endpoint::RunUpdates => self.run_updates(body, response_flush_barrier(&request)).await,
            Endpoint::GenerateReport => self.generate_report().await,
            Endpoint::Asset
            | Endpoint::Favicon
            | Endpoint::Root
            | Endpoint::Settings
            | Endpoint::Tools
            | Endpoint::Available => self.page(endpoint, request).await,
            Endpoint::BackgroundImage
            | Endpoint::BeatmapBackground
            | Endpoint::BeatmapAudio
            | Endpoint::BeatmapFile
            | Endpoint::SongsFile
            | Endpoint::BeatmapPath
            | Endpoint::SkinFile
            | Endpoint::CounterStatic => self.file(endpoint, request).await,
            Endpoint::WsV1
            | Endpoint::WsTokens
            | Endpoint::WsV2
            | Endpoint::WsV2Precise
            | Endpoint::WsCommands => self.websocket(endpoint, request).await,
        }
    }

    fn counter_open_root(&self) -> Response {
        match super::overlay_folders::open_root(self.opener.as_ref(), &self.services.counters.current_root()) {
            Ok(()) => json_response(StatusCode::OK, json!({ "status": "opened" })),
            Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
        }
    }

    fn counter_open(&self, request: &Request<Body>) -> Response {
        match decoded_capture(request, "/api/counters/open/")
            .as_deref()
            .filter(|name| !name.is_empty())
        {
            None => json_response(StatusCode::OK, json!({ "error": "no folder name" })),
            Some(name) => match super::overlay_folders::open_folder(
                self.opener.as_ref(),
                &self.services.counters.current_root(),
                name,
            ) {
                Ok(()) => json_response(StatusCode::OK, json!({ "status": "opened" })),
                Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
            },
        }
    }

    async fn settings_save(&self, headers: &axum::http::HeaderMap, body: Bytes) -> Response {
        let mut values: Value = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(error) => {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": error.to_string() }),
                );
            }
        };
        // A7.3: the dashboard switch lives only in vosu.toml. Nothing the
        // dashboard serves may turn it off, so the save publishes nothing.
        if values.get("ENABLE_DASHBOARD").is_some() {
            return json_response(
                StatusCode::BAD_REQUEST,
                json!({ "error": "ENABLE_DASHBOARD can only be changed in vosu.toml" }),
            );
        }
        if let Some(object) = values.as_object_mut() {
            object.insert("_request_origin".into(), headers.get(header::HOST).and_then(|v| v.to_str().ok()).map_or(Value::Null, |host| Value::String(format!("http://{host}"))));
        }
        match self.services.config.save(values).await {
            Ok(()) => json_response(StatusCode::OK, json!({ "status": "updated" })),
            Err(error) => {
                json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": error }))
            }
        }
    }

    async fn run_updates(&self, body: Bytes, barrier: Option<Arc<ResponseFlushBarrier>>) -> Response {
        let consent = if body.is_empty() {
            false
        } else {
            match serde_json::from_slice::<Value>(&body) {
                Ok(value) => value
                    .get("consent")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
                Err(error) => {
                    return json_response(
                        StatusCode::INTERNAL_SERVER_ERROR,
                        json!({ "error": error.to_string() }),
                    );
                }
            }
        };
        update_response(self.services.updater.run_update(consent).await, barrier)
    }

    async fn generate_report(&self) -> Response {
        use axum::response::IntoResponse;
        static REPORTS: tokio::sync::Semaphore = tokio::sync::Semaphore::const_new(1);
        let permit = REPORTS.acquire().await.expect("report semaphore is never closed");
        let provider = Arc::clone(&self.services.report_input);
        let probe = Arc::clone(&self.services.report_probe);
        let runtime = tokio::runtime::Handle::current();
        match tokio::task::spawn_blocking(move || {
            // Keep admission through all capture/probe/log work, even if the
            // request future is cancelled while a filesystem worker is running.
            let _permit = permit;
            let input = provider.capture()?;
            runtime.block_on(generate_report_document(input, probe.as_ref()))
                .map_err(|error| error.to_string())
        }).await {
            Ok(Ok(document)) => document.into_response(),
            Ok(Err(error)) => internal_error(error),
            Err(error) => internal_error(error.to_string()),
        }
    }

    /// The page shells, `/assets/<name>` and `/favicon.ico` are bundled hosts'
    /// exact registrations on the listener the request arrived on: the tool
    /// host's design-system set on both roles, the dashboard host's shells
    /// and assets on the dashboard role only (A7). Core embeds and serves none
    /// of them; an unregistered path is a 404.
    async fn page(&self, endpoint: Endpoint, request: Request<Body>) -> Response {
        let listener = request
            .extensions()
            .get::<crate::server::routes::SurfaceAdmission>()
            .map_or(vosu_surface::listener::ListenerRole::Overlay, |admission| {
                admission.listener
            });
        let Some(surface) = &self.surface else {
            return not_found();
        };
        let Some(mut response) = surface.listener_route(listener, &request).await else {
            return not_found();
        };
        // The second-launch probe (`vosu_core::desktop::existing_dashboard`)
        // recognises a running vosu by this header on a 200 favicon.
        if endpoint == Endpoint::Favicon && response.status() == StatusCode::OK {
            response.headers_mut().insert(
                "x-vosu-identity",
                axum::http::HeaderValue::from_static("vosu"),
            );
        }
        response
    }

    async fn file(&self, endpoint: Endpoint, request: Request<Body>) -> Response {
        if endpoint == Endpoint::CounterStatic {
            return self.counter_static(&request).await;
        }
        match &self.surface {
            Some(surface) => surface.http(&request).await,
            None => surface_http_response(surface_not_ready()),
        }
    }

    /// Overlay static serving, alias redirects and `COUNTER_PATH` belong to
    /// the overlay host and read only through its `OverlayRootView` lease.
    fn counter_static(&self, request: &Request<Body>) -> impl Future<Output = Response> + Send + '_ {
        let surface = self.overlay_static.as_ref().zip(surface_request(request, Arc::new(empty_selection())));
        let head = request.method() == Method::HEAD;
        async move {
            let Some((serving, surface)) = surface else {
                return not_found();
            };
            let mut response = surface_http_response(serving.serve(&surface).await);
            if head {
                *response.body_mut() = Body::empty();
            }
            response
        }
    }

    async fn websocket(&self, endpoint: Endpoint, request: Request<Body>) -> Response {
        let Some(tracker) = listener_tracker(&request) else {
            return response(
                StatusCode::SERVICE_UNAVAILABLE,
                "text/plain; charset=utf-8",
                Bytes::from_static(NOT_FOUND.as_bytes()),
            );
        };
        let connections = self.connections.clone();
        let overlay = socket_overlay(&request);
        let (hub, commands, client_namespace) = self.services.websocket.hub(endpoint);
        // Every feed, `/websocket/commands` included, is a bundled host's
        // registration; the commands hub only frames the overlay host's feed.
        let feed = match &self.surface {
            Some(surface) => match surface.dispatch(&request, true).await {
                Some(ProviderResponse::Feed(feed)) => feed,
                _ => return not_found(),
            },
            None => hub.surface_feed(commands),
        };
        let processor = Arc::clone(&self.services.websocket.processor);
        let relays = Arc::clone(&self.services.websocket.relays);
        let query = request.uri().query().map(str::to_owned);
        let mut mutation = request.extensions()
            .get::<super::auth::MutationAuthorization>()
            .copied()
            .unwrap_or_default();
        let key_grant = request.extensions().get::<super::api_keys::KeyGrant>().cloned();
        let key_protocol = request.extensions().get::<super::routes::KeyProtocol>().is_some();
        if key_grant.is_none() && endpoint == Endpoint::WsCommands
            && request.headers().get(header::UPGRADE)
                .is_some_and(|value| value.as_bytes().eq_ignore_ascii_case(b"websocket"))
            && request.headers().get_all(header::CONNECTION).iter().any(|value|
                value.to_str().is_ok_and(|value| value.split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))))
            && let Some(Some(broker)) = request.extensions()
                .get::<Option<Arc<super::broker::BrokerToken>>>()
        {
            let peer = request.extensions().get::<ConnectInfo<PeerAddress>>()
                .map(|peer| peer.0.0.ip());
            match broker.authorize(request.headers(), peer) {
                Ok(valid) => mutation.0 |= valid,
                Err(()) => return response(StatusCode::UNAUTHORIZED, "text/plain; charset=utf-8",
                    Bytes::from_static(b"Invalid broker credential")),
            }
        }
        let mut result = websocket::upgrade_response(
            websocket_endpoint(endpoint),
            request,
            tracker,
            move |_, socket| async move {
                let _registration = connections.register(overlay);
                bridge_socket(
                    socket,
                    feed,
                    processor,
                    client_namespace,
                    relays,
                    (query.as_deref(), mutation, key_grant),
                )
                .await;
            },
        )
        .await;
        if key_protocol && result.status() == StatusCode::SWITCHING_PROTOCOLS {
            result.headers_mut().insert(header::SEC_WEBSOCKET_PROTOCOL, axum::http::HeaderValue::from_static("vosu.key"));
        }
        result
    }
}

impl RouteDelegate for ProductionRouteDelegate {
    fn api_keys(&self) -> Option<Arc<super::api_keys::ApiKeys>> { self.api_keys.clone() }
    #[cfg(feature = "test-harness")]
    fn adapter_capture(&self) -> Option<&crate::server::adapter_capture::endpoint::AdapterCaptureEndpoint> { self.adapter_capture.as_deref() }
    fn registered_http<'a>(&'a self, request: &'a Request<Body>) -> Option<ProviderFuture<'a, Response>> {
        self.surface.as_ref().map(|surface| Box::pin(surface.http(request)) as ProviderFuture<'a, Response>)
    }
    fn handle(
        &self,
        endpoint: Endpoint,
        request: Request<Body>,
        body: Bytes,
    ) -> ProviderFuture<'_, Response> {
        let wants_page = crate::server::pages::wants_page(request.method(), request.headers());
        Box::pin(async move {
            let response = self.dispatch(endpoint, request, body).await;
            // A missing overlay file or page opened in a browser gets the
            // styled 404 page instead of the plain body scripts receive.
            let plain = response
                .headers()
                .get(header::CONTENT_TYPE)
                .is_some_and(|value| value.as_bytes().starts_with(b"text/plain"));
            if wants_page && response.status() == StatusCode::NOT_FOUND && plain {
                embedded_response(EmbeddedResponse::not_found())
            } else {
                response
            }
        })
    }
}

fn response_flush_barrier(request: &Request<Body>) -> Option<Arc<ResponseFlushBarrier>> {
    let peer = request.extensions().get::<ConnectInfo<PeerAddress>>()?.0.0;
    let barrier = request
        .extensions()
        .get::<Arc<ResponseFlushRegistry>>()?
        .barrier(peer)?;
    Some(barrier)
}

fn listener_tracker(request: &Request<Body>) -> Option<Arc<UpgradeCallbackTracker>> {
    request
        .extensions()
        .get::<Arc<UpgradeCallbackTracker>>()
        .cloned()
}

fn websocket_endpoint(endpoint: Endpoint) -> websocket::Endpoint {
    match endpoint {
        Endpoint::WsV1 => websocket::Endpoint::V1,
        Endpoint::WsTokens => websocket::Endpoint::StreamCompanion,
        Endpoint::WsV2 => websocket::Endpoint::V2,
        Endpoint::WsV2Precise => websocket::Endpoint::V2Precise,
        Endpoint::WsCommands => websocket::Endpoint::Commands,
        _ => unreachable!(),
    }
}
