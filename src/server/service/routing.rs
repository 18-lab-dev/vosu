#[derive(Clone)]
pub struct ProductionRouteDelegate {
    services: ProductionServices,
}

impl ProductionRouteDelegate {
    pub fn new(services: ProductionServices) -> Self {
        services
            .websocket
            .relays
            .bind_processor(&services.websocket.processor);
        Self { services }
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
        let name = decoded_capture(&request, "/api/counters/settings/").unwrap_or_default();
        if name.is_empty() {
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
        let query = query_pairs(&request);
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
        let relay_name = name.clone();
        match serde_json::from_slice::<Vec<SettingInput>>(&body) {
            Ok(values) => match self
                .services
                .counters
                .save_user_settings_debounced(name, values)
                .await
            {
                Ok(saved) => {
                    if saved {
                        self.services
                            .websocket
                            .processor
                            .refresh_settings(relay_name);
                    }
                    json_response(StatusCode::OK, json!({ "result": "success" }))
                }
                Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
            },
            Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
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
                Endpoint::Favicon => PageLookup::Embedded(self.services.pages.favicon()),
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
            counter_resource_response(outcome, range, &encoded_html_path).await
        } else {
            resource_response(outcome, range).await
        }
    }

    async fn static_fallback(&self, fallback: StaticFallback, request: Request<Body>) -> Response {
        match fallback {
            StaticFallback::RedirectTrailingSlash { location } => redirect(location),
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
                    Ok(value) => counter_resource_response(value, range, &index_path).await,
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
        let mutation = request.extensions()
            .get::<super::auth::MutationAuthorization>()
            .copied()
            .unwrap_or_default();
        websocket::upgrade_response(
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
                    (query.as_deref(), mutation),
                )
                .await;
            },
        )
        .await
    }
}

impl RouteDelegate for ProductionRouteDelegate {
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
