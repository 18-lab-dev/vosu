// Permanent surface dispatch after ordinary transport admission.
use vosu_surface::routes::{
    ProviderResponse, ResponseBody, ResponseFuture, RouteProvider, SurfaceRequest, SurfaceResponse,
};

pub fn reserve_core_routes(
    host: &Arc<vosu_core::plugin::PluginHost>,
) -> Result<vosu_surface::routes::RegistrationLease, vosu_surface::routes::RegistrationError> {
    use vosu_surface::{
        listener::ListenerRole,
        routes::{RouteDescriptor, RoutePath},
    };
    let mut descriptors = Vec::new();
    for listener in [ListenerRole::Overlay, ListenerRole::Dashboard] {
        for (path, methods, prefix) in [
            ("/", &["GET"][..], false),
            ("/settings", &["GET"][..], false),
            ("/available", &["GET"][..], false),
            ("/favicon.ico", &["GET"][..], false),
            ("/assets", &["GET"][..], true),
            ("/api/dashboard", &["GET"][..], true),
            ("/api/keys", &["GET", "POST"][..], false),
            ("/api/counters", &["GET", "POST"][..], true),
            ("/api/settingsSave", &["POST"][..], false),
            ("/api/runUpdates", &["POST"][..], false),
            ("/api/generateReport", &["POST"][..], false),
            (
                "/tools",
                &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"][..],
                true,
            ),
            (
                "/api/tools",
                &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"][..],
                true,
            ),
            (
                "/api/v1",
                &["GET", "HEAD", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"][..],
                true,
            ),
        ] {
            for method in methods {
                descriptors.push(RouteDescriptor {
                    listener,
                    method: (*method).into(),
                    path: if prefix {
                        RoutePath::Prefix(path.into())
                    } else {
                        RoutePath::Exact(path.into())
                    },
                    websocket: false,
                });
            }
        }
    }
    descriptors.push(RouteDescriptor {
        listener: ListenerRole::Overlay,
        method: "GET".into(),
        path: RoutePath::Exact("/websocket/commands".into()),
        websocket: true,
    });
    host.reserve_core_surface_routes(descriptors)
}

/// One bundled adapter's route registrations.
pub type AdapterRoutes = Vec<(
    vosu_surface::routes::RouteDescriptor,
    Arc<dyn RouteProvider>,
)>;

/// The bundled adapters' overlay route table, grouped by registering owner:
/// `[upstream, gosumemory, streamcompanion]`. `state` and `feeds` are ordered
/// v1, streamCompanion, v2, v2 precise. The production mount and the root
/// compatibility matrix both register exactly this table.
pub fn adapter_routes(
    state: [Arc<dyn StateJsonProvider>; 4],
    feeds: [WebSocketHub; 4],
    files: &vosu_adapter_upstream::files::Provider,
) -> [AdapterRoutes; 3] {
    use vosu_surface::{
        listener::ListenerRole,
        routes::{RouteDescriptor, RoutePath},
    };
    let mut groups: [AdapterRoutes; 3] = Default::default();
    let mut add =
        |owner: usize, path: RoutePath, method: &str, websocket: bool, provider| {
            groups[owner].push((
                RouteDescriptor {
                    listener: ListenerRole::Overlay,
                    method: method.into(),
                    path,
                    websocket,
                },
                Arc::new(provider) as Arc<dyn RouteProvider>,
            ));
        };
    let schemas = [
        (1, StateRoute::V1, "/json", "/ws"),
        (2, StateRoute::StreamCompanion, "/json/sc", "/tokens"),
        (0, StateRoute::V2, "/json/v2", "/websocket/v2"),
        (0, StateRoute::V2Precise, "/json/v2/precise", "/websocket/v2/precise"),
    ];
    for (((owner, route, path, ws), provider), hub) in schemas.into_iter().zip(state).zip(feeds) {
        add(
            owner,
            RoutePath::Exact(path.into()),
            "GET",
            false,
            RegisteredProvider::State { provider, route },
        );
        add(
            owner,
            RoutePath::Exact(ws.into()),
            "GET",
            true,
            RegisteredProvider::Feed(hub),
        );
    }
    add(
        0,
        RoutePath::Exact("/api/calculate/pp".into()),
        "GET",
        false,
        RegisteredProvider::Calculate(files.clone()),
    );
    for (path, methods) in [
        ("/backgroundImage", &["GET"][..]),
        ("/Songs", &["GET"][..]),
        ("/files/beatmap", &["GET", "HEAD"][..]),
        ("/files/skin", &["GET", "HEAD"][..]),
    ] {
        for method in methods {
            let matcher = if path == "/backgroundImage" {
                RoutePath::Exact(path.into())
            } else {
                RoutePath::Children(path.into())
            };
            add(
                0,
                matcher,
                method,
                false,
                RegisteredProvider::File {
                    provider: files.clone(),
                },
            );
        }
    }
    groups
}

#[derive(Clone)]
pub struct SurfaceRouter {
    host: Arc<vosu_core::plugin::PluginHost>,
    requester: Arc<dyn vosu_surface::requester::BundledRequester>,
}
impl SurfaceRouter {
    pub fn new(
        host: Arc<vosu_core::plugin::PluginHost>,
        requester: Arc<dyn vosu_surface::requester::BundledRequester>,
    ) -> Self {
        Self { host, requester }
    }
    pub fn dispatch(
        &self,
        request: &Request<Body>,
        websocket: bool,
    ) -> ResponseFuture<'_, Option<ProviderResponse>> {
        let selected = (|| {
            let context = self
                .requester
                .capture(vosu_surface::state::SessionTarget::Focused, &[])
                .ok()?;
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
            Some(SurfaceRequest {
                method: request.method().to_string(),
                path: request.uri().path().to_owned(),
                query,
                headers,
                body: Arc::from([]),
                context: Arc::new(context),
                management: None,
            })
        })();
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

pub enum RegisteredProvider {
    State {
        provider: Arc<dyn StateJsonProvider>,
        route: StateRoute,
    },
    File {
        provider: vosu_adapter_upstream::files::Provider,
    },
    Calculate(vosu_adapter_upstream::files::Provider),
    Feed(WebSocketHub),
}
impl RouteProvider for RegisteredProvider {
    fn respond(&self, request: SurfaceRequest) -> ResponseFuture<'_, ProviderResponse> {
        Box::pin(async move {
            let response = match self {
                Self::Feed(hub) => return ProviderResponse::Feed(hub.surface_feed(false)),
                Self::State { provider, route } => match provider.cached_json(*route).await {
                    CachedJson::Ready(body) => surface_bytes(200, JSON, body),
                    CachedJson::NotReady => surface_not_ready(),
                    CachedJson::Failed { status, body } => {
                        surface_bytes(status.as_u16(), JSON, body)
                    }
                },
                Self::Calculate(provider) => {
                    let query = url::form_urlencoded::Serializer::new(String::new())
                        .extend_pairs(&request.query)
                        .finish();
                    match vosu_adapter_upstream::calculate::PpQuery::parse(Some(&query)) {
                        Err(error) => surface_bytes(
                            400,
                            JSON,
                            Bytes::from(
                                serde_json::to_vec(&json!({"error":error})).expect("JSON string"),
                            ),
                        ),
                        Ok(query) => match provider.calculate(query).await {
                            vosu_adapter_upstream::calculate::ResultBody::Ready(body) => {
                                surface_bytes(200, JSON, body)
                            }
                            vosu_adapter_upstream::calculate::ResultBody::NotReady => {
                                surface_not_ready()
                            }
                            vosu_adapter_upstream::calculate::ResultBody::Failed {
                                status,
                                body,
                            } => surface_bytes(status, JSON, body),
                        },
                    }
                }
                Self::File { provider } => {
                    let method =
                        Method::from_bytes(request.method.as_bytes()).expect("validated method");
                    let endpoint = super::routes::classify(&method, &request.path)
                        .expect("registered game file");
                    surface_game_response(provider, endpoint, request).await
                }
            };
            ProviderResponse::Http(response)
        })
    }
}
fn surface_bytes(status: u16, content_type: &str, body: Bytes) -> SurfaceResponse {
    SurfaceResponse {
        status,
        headers: vec![("content-type".into(), content_type.into())],
        body: ResponseBody::Bytes(body),
    }
}
fn surface_not_ready() -> SurfaceResponse {
    surface_bytes(
        500,
        JSON,
        Bytes::from_static(b"{\"error\":\"osu is not ready/running\"}"),
    )
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
        builder = builder.header(name, value);
    }
    builder.body(body).expect("core-validated surface response")
}
async fn surface_game_response(
    provider: &vosu_adapter_upstream::files::Provider,
    endpoint: Endpoint,
    request: SurfaceRequest,
) -> SurfaceResponse {
    use vosu_adapter_upstream::files::{Outcome, Route};
    let path = request.path.as_str();
    let (route, encoded) = match endpoint {
        Endpoint::BackgroundImage | Endpoint::BeatmapBackground => (Route::Background, ""),
        Endpoint::BeatmapAudio => (Route::Audio, ""),
        Endpoint::BeatmapFile => (Route::Beatmap, ""),
        Endpoint::SongsFile => (
            Route::Songs,
            path.strip_prefix("/Songs/").unwrap_or_default(),
        ),
        Endpoint::BeatmapPath => (
            Route::BeatmapPath,
            path.strip_prefix("/files/beatmap/").unwrap_or_default(),
        ),
        Endpoint::SkinFile => (
            Route::Skin,
            path.strip_prefix("/files/skin/").unwrap_or_default(),
        ),
        _ => unreachable!("registered game-file route"),
    };
    let query = url::form_urlencoded::Serializer::new(String::new())
        .extend_pairs(&request.query)
        .finish();
    let target = if query.is_empty() {
        path.to_owned()
    } else {
        format!("{path}?{query}")
    };
    let head = request.method == "HEAD";
    let mut response = match provider.resolve(route, encoded, &target).await {
        Outcome::Missing => surface_bytes(
            404,
            "text/plain; charset=utf-8",
            Bytes::from_static(b"Not Found"),
        ),
        Outcome::NotReady => surface_not_ready(),
        Outcome::Redirect(location) => SurfaceResponse {
            status: 301,
            headers: vec![("location".into(), location)],
            body: ResponseBody::Empty,
        },
        Outcome::Directory { entries, truncated } => {
            let entries = entries
                .into_iter()
                .map(|e| (e.name, e.kind == vosu_surface::files::EntryKind::Directory))
                .collect();
            let mut html = files::directory_index(entries, &target);
            if truncated {
                html.push_str("\n<!-- directory listing truncated -->");
            }
            surface_bytes(200, "text/html; charset=utf-8", Bytes::from(html))
        }
        Outcome::File { file, name, lazer } => {
            let mut headers = axum::http::HeaderMap::new();
            for (name, value) in &request.headers {
                headers.append(
                    axum::http::HeaderName::from_bytes(name.as_bytes())
                        .expect("allow-listed header"),
                    value.parse().expect("validated header"),
                );
            }
            let conditional = !matches!(endpoint, Endpoint::SongsFile | Endpoint::BackgroundImage);
            let validators =
                conditional.then(|| files::FileValidators::for_surface(&file, &name, lazer));
            let mut result = if validators
                .as_ref()
                .is_some_and(|v| v.not_modified(&headers))
            {
                SurfaceResponse {
                    status: 304,
                    headers: vec![],
                    body: ResponseBody::Empty,
                }
            } else {
                let range = (!head)
                    .then(|| headers.get(header::RANGE).and_then(|v| v.to_str().ok()))
                    .flatten();
                match files::plan_response(
                    file.metadata.size,
                    files::content_type(Path::new(&name)),
                    files::media_range_eligible(Path::new(&name)),
                    range,
                ) {
                    Err(_) => SurfaceResponse {
                        status: 416,
                        headers: vec![(
                            "content-range".into(),
                            files::unsatisfied_content_range(file.metadata.size),
                        )],
                        body: ResponseBody::Empty,
                    },
                    Ok(plan) => {
                        let mut headers = vec![
                            ("content-type".into(), plan.content_type.to_owned()),
                            ("content-length".into(), plan.content_length.to_string()),
                        ];
                        if let Some(value) = plan.accept_ranges {
                            headers.push(("accept-ranges".into(), value.to_owned()));
                        }
                        if let Some(value) = plan.content_range {
                            headers.push(("content-range".into(), value));
                        }
                        SurfaceResponse {
                            status: plan.status,
                            headers,
                            body: if head {
                                ResponseBody::Empty
                            } else {
                                ResponseBody::File {
                                    file,
                                    offset: plan.offset,
                                    length: plan.content_length,
                                }
                            },
                        }
                    }
                }
            };
            if let Some(validators) = validators {
                let mut headers = axum::http::HeaderMap::new();
                validators.apply(&mut headers);
                result.headers.extend(headers.iter().map(|(k, v)| {
                    (
                        k.to_string(),
                        v.to_str().expect("validator header").to_owned(),
                    )
                }));
            }
            result
        }
    };
    if head {
        if let ResponseBody::Bytes(bytes) = &response.body {
            response
                .headers
                .push(("content-length".into(), bytes.len().to_string()));
        }
        response.body = ResponseBody::Empty;
    }
    response
        .headers
        .push(("content-security-policy".into(), "sandbox".into()));
    response
}
