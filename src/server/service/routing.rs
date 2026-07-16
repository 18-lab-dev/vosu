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
            Endpoint::CounterDelete => self.counter_delete(&request),
            Endpoint::CounterSettings => self.counter_settings(request, body).await,
            Endpoint::SettingsSave => self.settings_save(body).await,
            Endpoint::RunUpdates => self.run_updates(response_flush_barrier(&request)).await,
            Endpoint::GenerateReport => self.generate_report().await,
            Endpoint::Ingame
            | Endpoint::Asset
            | Endpoint::Favicon
            | Endpoint::Root
            | Endpoint::Settings
            | Endpoint::LocalOverlays
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

    async fn counter_search(&self, request: Request<Body>) -> Response {
        let raw = request
            .uri()
            .path()
            .strip_prefix("/api/counters/search/")
            .unwrap_or_default();
        let query = percent_decode_once(raw).unwrap_or_default();
        let authority = request
            .headers()
            .get(header::HOST)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned);
        let external = request
            .headers()
            .get(header::REFERER)
            .and_then(|value| value.to_str().ok())
            .and_then(|value| url::Url::parse(value).ok())
            .is_some_and(|url| url.path() == "/available");
        match self
            .services
            .page_data
            .search_counters(&query, external, authority)
            .await
        {
            Ok(body) => response(StatusCode::OK, "text/html; charset=utf-8", body),
            Err(error) => response(
                StatusCode::OK,
                "text/html; charset=utf-8",
                Bytes::from(error),
            ),
        }
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
                Ok(()) => {
                    self.services.websocket.processor.refresh_overlays();
                    json_response(StatusCode::OK, json!({ "status": "Finished" }))
                }
                Err(error) => json_response(StatusCode::OK, json!({ "error": error.to_string() })),
            },
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
                Ok(()) => {
                    self.services.websocket.processor.refresh_overlays();
                    json_response(StatusCode::OK, json!({ "status": "deleted" }))
                }
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
                Ok((settings, _)) => json_response(
                    StatusCode::OK,
                    serde_json::to_value(settings).unwrap_or(Value::Null),
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
        let input = match self.services.report_input.capture() {
            Ok(input) => input,
            Err(error) => return internal_error(error),
        };
        match generate_report_document(input, self.services.report_probe.as_ref()).await {
            Ok(document) => document.into_response(),
            Err(error) => internal_error(error.to_string()),
        }
    }

    async fn page(&self, endpoint: Endpoint, request: Request<Body>) -> Response {
        let ingame = self.services.page_data.ingame_counters().ok();
        let lookup = {
            let path = request.uri().path();
            match endpoint {
                Endpoint::Ingame => self.services.pages.lookup_get(path, false, ingame.as_ref()),
                Endpoint::Asset => PageLookup::Embedded(
                    self.services
                        .pages
                        .asset(path.strip_prefix("/assets/").unwrap_or_default()),
                ),
                Endpoint::Favicon => PageLookup::Embedded(self.services.pages.favicon()),
                Endpoint::Root
                | Endpoint::Settings
                | Endpoint::LocalOverlays
                | Endpoint::Available => self.services.pages.lookup_get(
                    path,
                    request
                        .uri()
                        .query()
                        .and_then(|query| decoded_query_value(query, "overlay"))
                        .is_some_and(|value| !value.is_empty()),
                    ingame.as_ref(),
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
        resource_response(outcome, range).await
    }

    async fn static_fallback(&self, fallback: StaticFallback, request: Request<Body>) -> Response {
        match fallback {
            StaticFallback::RedirectTrailingSlash { location } => redirect(location),
            StaticFallback::CounterPath { index_path, .. } => {
                let current = self.services.beatmap.files();
                let target = request.uri().to_string();
                let outcome = files::resolve_encoded_resource(
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
                    Ok(value) => resource_response(value, range).await,
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
        let (hub, commands, client_namespace) = self.services.websocket.hub(endpoint);
        let hub = hub.clone();
        let processor = Arc::clone(&self.services.websocket.processor);
        let relays = Arc::clone(&self.services.websocket.relays);
        let query = request.uri().query().map(str::to_owned);
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
                    query.as_deref(),
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
        Endpoint::WsCommands => websocket::Endpoint::Commands,
        _ => unreachable!(),
    }
}
