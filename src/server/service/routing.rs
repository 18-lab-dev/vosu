#[derive(Clone)]
pub struct ProductionRouteDelegate {
    services: ProductionServices,
    api_keys: Option<Arc<super::api_keys::ApiKeys>>,
}

impl ProductionRouteDelegate {
    pub fn new(services: ProductionServices) -> Self {
        services.websocket.processor.bind_profiles(services.counters.settings_store());
        services
            .websocket
            .relays
            .bind_processor(&services.websocket.processor);
        Self { services, api_keys: None }
    }
    pub fn with_api_keys(mut self, keys: Arc<super::api_keys::ApiKeys>) -> Self {
        self.api_keys = Some(keys);
        self
    }
    pub fn services(&self) -> &ProductionServices {
        &self.services
    }

    async fn dispatch(&self, endpoint: Endpoint, request: Request<Body>, body: Bytes) -> Response {
        match endpoint {
            Endpoint::State(_) | Endpoint::CalculatePp => {
                internal_error("route provider dispatch invariant violated")
            }
            Endpoint::CounterSearch => self.counter_search(request).await,
            Endpoint::CounterDownload => self.counter_download(request).await,
            Endpoint::CounterOpen => self.counter_open(&request),
            Endpoint::CounterOpenRoot => self.counter_open_root(),
            Endpoint::CounterDelete => self.counter_delete(&request),
            Endpoint::CounterSettings => self.counter_settings(request, body).await,
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
            Endpoint::SettingsSave => self.settings_save(body).await,
            Endpoint::RunUpdates => self.run_updates(response_flush_barrier(&request)).await,
            Endpoint::GenerateReport => self.generate_report().await,
            Endpoint::Asset
            | Endpoint::Favicon
            | Endpoint::Root
            | Endpoint::Settings
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
            | Endpoint::WsV2Subscribe
            | Endpoint::WsCommands => self.websocket(endpoint, request).await,
        }
    }

    async fn counter_search(&self, request: Request<Body>) -> Response {
        let raw = request
            .uri()
            .path()
            .strip_prefix("/api/counters/search/")
            .unwrap_or_default();
        let query = percent_decode_once(raw).unwrap_or_default();
        let Some(external) = counter_search_source(request.uri().query()) else {
            return response(
                StatusCode::BAD_REQUEST,
                "text/html; charset=utf-8",
                Bytes::from_static(b"Error: unknown search source"),
            );
        };
        let authority = request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        counter_search_response(
            self.services
                .page_data
                .search_counters(&query, external, authority)
                .await,
        )
    }

    async fn counter_download(&self, request: Request<Body>) -> Response {
        let raw = request
            .uri()
            .path()
            .strip_prefix("/api/counters/download/")
            .unwrap_or_default()
            .to_owned();
        let query = query_pairs(&request);
        let parsed = CounterService::parse_download_request(
            &raw,
            query_value(&query, "name"),
            query_value(&query, "update"),
        );
        match parsed {
            Ok(download) => match self.services.counters.download(&download).await {
                Ok(()) => json_response(StatusCode::OK, json!({ "status": "Finished" })),
                Err(CounterError::InstallBusy) => json_response(
                    StatusCode::TOO_MANY_REQUESTS,
                    json!({ "error": "counter install capacity is full" }),
                ),
                Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
            },
            Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
        }

    }
    fn counter_open_root(&self) -> Response {
        match self.services.counters.open_root() {
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
            Some(name) => match self.services.counters.open(name) {
                Ok(()) => json_response(StatusCode::OK, json!({ "status": "opened" })),
                Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
            },
        }
    }

    fn counter_delete(&self, request: &Request<Body>) -> Response {
        match decoded_capture(request, "/api/counters/delete/")
            .as_deref()
            .filter(|name| !name.is_empty())
        {
            None => json_response(StatusCode::OK, json!({ "error": "no folder name" })),
            Some(name) => match self.services.counters.delete(name) {
                Ok(()) => json_response(StatusCode::OK, json!({ "status": "deleted" })),
                Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
            },
        }
    }

    async fn counter_settings(&self, request: Request<Body>, body: Bytes) -> Response {
        if request.uri().path() == "/api/counters/profiles" {
            return self.counter_profiles(&request, &body);
        }
        let query = query_pairs(&request);
        let overlay = decoded_capture(&request, "/api/counters/settings/").unwrap_or_default();
        let name = match query_value(&query, "profile") {
            Some(profile) => crate::counters::qualified_counter(&overlay, profile),
            None => overlay.clone(),
        };
        if overlay.is_empty() {
            return json_response(StatusCode::OK, json!({ "error": "no folder name" }));
        }
        if request.method() == axum::http::Method::GET {
            return match self.services.counters.get_settings(&name) {
                Ok((settings, values)) => json_response(
                    StatusCode::OK,
                    json!({ "settings": settings, "values": values }),
                ),
                Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
            };
        }
        if query_value(&query, "update") == Some("yes") {
            let parsed = serde_json::from_slice::<Vec<CounterSetting>>(&body);
            return match parsed {
                Ok(settings) => match self.services.counters.save_developer_settings(
                    &name,
                    settings,
                    Some("yes"),
                ) {
                    Ok(_) => json_response(StatusCode::OK, json!({ "result": "success" })),
                    Err(error) => {
                        json_response(StatusCode::OK, json!({ "error": error.to_string() }))
                    }
                },
                Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
            };
        }
        let resolved_profile = match self.services.counters.settings_store().resolve_profile(query_value(&query, "profile")) {
            Ok(profile) => profile,
            Err(error) => return json_response(StatusCode::OK, json!({ "error": error.to_string() })),
        };
        let name = crate::counters::qualified_counter(&overlay, &resolved_profile);
        match serde_json::from_slice::<Vec<SettingInput>>(&body) {
            Ok(values) => match self
                .services
                .counters
                .save_user_settings_debounced(name, values)
                .await
            {
                Ok(saved) => {
                    if saved {
                        self.refresh_profile(&overlay, &resolved_profile);
                    }
                    json_response(StatusCode::OK, json!({ "result": "success" }))
                }
                Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
            },
            Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
        }
    }

    fn counter_profiles(&self, request: &Request<Body>, body: &[u8]) -> Response {
        let store = self.services.counters.settings_store();
        let result = (|| -> Result<crate::counters::Profiles, String> {
            if request.method() == axum::http::Method::GET {
                return store.profiles().map_err(|e| e.to_string());
            }
            let value: Value = serde_json::from_slice(body).map_err(|e| e.to_string())?;
            let field = |key| value.get(key).and_then(Value::as_str);
            let action = field("action").ok_or("missing action")?;
            let name = field("name").ok_or("missing name")?;
            let before = store.profiles().map_err(|e| e.to_string())?;
            let registry = if action == "copyOverlay" {
                store.copy_overlay(name, field("source").ok_or("missing source")?,
                    field("target").ok_or("missing target")?).map_err(|e| e.to_string())?;
                self.refresh_profile(name, field("target").unwrap_or_default());
                store.profiles().map_err(|e| e.to_string())?
            } else {
                store.manage_profile(action, name, field(if action == "create" { "copyFrom" } else { "target" }))
                    .map_err(|e| e.to_string())?
            };
            if before.active != registry.active {
                self.services.websocket.processor.refresh_active_settings();
            }
            Ok(registry)
        })();
        match result {
            Ok(registry) => json_response(StatusCode::OK, json!(registry)),
            Err(error) => json_response(StatusCode::OK, json!({ "error": error })),
        }
    }

    fn refresh_profile(&self, overlay: &str, profile: &str) {
        self.services.websocket.processor.refresh_settings(crate::counters::qualified_counter(overlay, profile));
        if self.services.counters.settings_store().resolve_profile(None).is_ok_and(|p| p == profile) {
            self.services.websocket.processor.refresh_settings(overlay);
        }
    }

    async fn settings_save(&self, body: Bytes) -> Response {
        let values = match serde_json::from_slice(&body) {
            Ok(value) => value,
            Err(error) => {
                return json_response(
                    StatusCode::INTERNAL_SERVER_ERROR,
                    json!({ "error": error.to_string() }),
                );
            }
        };
        match self.services.config.save(values).await {
            Ok(()) => json_response(StatusCode::OK, json!({ "status": "updated" })),
            Err(error) => {
                json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({ "error": error }))
            }
        }
    }

    async fn run_updates(&self, barrier: Option<Arc<ResponseFlushBarrier>>) -> Response {
        update_response(self.services.updater.run_update().await, barrier)
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

    async fn page(&self, endpoint: Endpoint, request: Request<Body>) -> Response {
        let lookup = {
            let path = request.uri().path();
            match endpoint {
                Endpoint::Asset => PageLookup::Embedded(
                    self.services
                        .pages
                        .asset(path.strip_prefix("/assets/").unwrap_or_default()),
                ),
                Endpoint::Favicon => {
                    let mut response = embedded_response(self.services.pages.favicon());
                    response.headers_mut().insert(
                        "x-vosu-identity",
                        axum::http::HeaderValue::from_static("vosu"),
                    );
                    return response;
                }
                Endpoint::Root
                | Endpoint::Settings
                | Endpoint::Available => self.services.pages.lookup_get(
                    path,
                    request
                        .uri()
                        .query()
                        .and_then(|query| decoded_query_value(query, "overlay"))
                        .is_some_and(|value| !value.is_empty()),
                ),
                _ => unreachable!(),
            }
        };
        match lookup {
            PageLookup::Embedded(page) => embedded_response(page),
            PageLookup::Dynamic(page) => {
                let authority = request
                    .headers()
                    .get(header::HOST)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                match self.services.page_data.render(page, authority).await {
                    Ok(page) => embedded_response(page),
                    Err(error) => internal_error(error),
                }
            }
            PageLookup::Static(fallback) => self.static_fallback(fallback, request).await,
            PageLookup::NotFound => not_found(),
        }
    }


    fn profile_html_path(&self, path: &str, request: &Request<Body>) -> String {
        let query = query_pairs(request);
        if let Some(requested) = query_value(&query, "vosuProfile") {
            let store = self.services.counters.settings_store();
            if let Ok(profile) = store.resolve_profile(Some(requested)).or_else(|_| store.resolve_profile(None)) {
                return format!("{path}?vosuProfile={profile}");
            }
        }
        path.to_owned()
    }
    async fn file(&self, endpoint: Endpoint, request: Request<Body>) -> Response {
        let current = self.services.beatmap.files();
        let path = request.uri().path();
        let counter_html_path = (endpoint == Endpoint::CounterStatic).then(|| {
            let relative = path.trim_start_matches('/');
            if path.ends_with('/') {
                format!("{relative}index.html")
            } else {
                relative.to_owned()
            }
        });
        let target = request.uri().to_string();
        let outcome = match resolve_file_endpoint(endpoint, &current, path, &target) {
            Ok(outcome) => outcome,
            Err(error) => return file_route_error_response(error),
        };
        let range = request
            .headers()
            .get(header::RANGE)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        if let Some(encoded_html_path) = counter_html_path {
            counter_resource_response(outcome, range, &self.profile_html_path(&encoded_html_path, &request)).await
        } else {
            resource_response(outcome, range).await
        }
    }

    async fn static_fallback(&self, fallback: StaticFallback, request: Request<Body>) -> Response {
        match fallback {
            StaticFallback::RedirectTrailingSlash { location } => {
                let query = query_pairs(&request);
                let location = if query_value(&query, "vosuProfile").is_some() {
                    format!("{location}?{}", request.uri().query().unwrap_or_default())
                } else { location };
                redirect(location)
            },
            StaticFallback::CounterPath { index_path, .. } => {
                let current = self.services.beatmap.files();
                let target = request.uri().to_string();
                let outcome = files::resolve_encoded_counter_resource(
                    current.static_root.as_deref(),
                    index_path.trim_start_matches('/'),
                    &target,
                    MissingKind::NotFound,
                );
                let range = request
                    .headers()
                    .get(header::RANGE)
                    .and_then(|value| value.to_str().ok())
                    .map(str::to_owned);
                match outcome {
                    Ok(value) => counter_resource_response(value, range, &self.profile_html_path(&index_path, &request)).await,
                    Err(_) => not_found(),
                }
            }
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
        if endpoint == Endpoint::WsV2Subscribe {
            let hub = self.services.websocket.subscriptions.clone();
            return websocket::upgrade_response(
                websocket::Endpoint::V2Subscribe,
                request,
                tracker,
                move |_, socket| async move {
                    bridge_v2_subscription_socket(socket, hub).await;
                },
            )
            .await;
        }
        let (hub, commands, client_namespace) = self.services.websocket.hub(endpoint);
        let hub = hub.clone();
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
                bridge_socket(
                    socket,
                    hub,
                    processor,
                    commands,
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
    fn handle(
        &self,
        endpoint: Endpoint,
        request: Request<Body>,
        body: Bytes,
    ) -> ProviderFuture<'_, Response> {
        Box::pin(async move { self.dispatch(endpoint, request, body).await })
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
        Endpoint::WsV2Subscribe => websocket::Endpoint::V2Subscribe,
        Endpoint::WsCommands => websocket::Endpoint::Commands,
        _ => unreachable!(),
    }
}
