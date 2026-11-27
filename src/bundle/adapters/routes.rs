//! Register the concrete adapter providers behind core's admitted routes.
use std::{path::Path, sync::Arc};

use bytes::Bytes;
use http::{HeaderMap, HeaderName, Method, header};
use serde_json::json;
use vosu_core::{
    runtime::HostRoutes,
    server::{
        files,
        routes::{Endpoint, StateJsonProvider, StateRoute, classify},
        service::{JSON, surface_bytes, surface_not_ready},
        websocket::WebSocketHub,
    },
};
use vosu_surface::requester::BundledRequester;
use vosu_surface::routes::{
    ProviderResponse, ResponseBody, ResponseFuture, RouteProvider, SurfaceRequest, SurfaceResponse,
};

/// The bundled adapters' overlay route table, grouped by registering owner:
/// `[upstream, gosumemory, streamcompanion]`. `state` and `feeds` are ordered
/// v1, streamCompanion, v2, v2 precise. The production mount and the root
/// compatibility matrix both register exactly this table.
pub(crate) fn adapter_routes(
    state: [Arc<dyn StateJsonProvider>; 4],
    feeds: [WebSocketHub; 4],
    upstream: Arc<dyn BundledRequester>,
) -> [HostRoutes; 3] {
    let files = vosu_adapter_upstream::files::Provider::new(upstream);
    use vosu_surface::{
        listener::ListenerRole,
        routes::{RouteDescriptor, RoutePath},
    };
    let mut groups: [HostRoutes; 3] = Default::default();
    let mut add = |owner: usize, path: RoutePath, method: &str, websocket: bool, provider| {
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
        (
            0,
            StateRoute::V2Precise,
            "/json/v2/precise",
            "/websocket/v2/precise",
        ),
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

enum RegisteredProvider {
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
                Self::State { provider, route } => {
                    let (status, body) = provider.cached_json(*route).await.status_body();
                    surface_bytes(status.as_u16(), JSON, body)
                }
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
                    let endpoint = classify(&method, &request.path).expect("registered game file");
                    surface_game_response(provider, endpoint, request).await
                }
            };
            ProviderResponse::Http(response)
        })
    }
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
            let mut headers = HeaderMap::new();
            for (name, value) in &request.headers {
                headers.append(
                    HeaderName::from_bytes(name.as_bytes()).expect("allow-listed header"),
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
                let range = headers.get(header::RANGE).and_then(|v| v.to_str().ok());
                match files::plan_response(
                    file.metadata.size,
                    matches!(route, Route::Beatmap | Route::Background | Route::Audio)
                        || files::media_range_eligible(Path::new(&name)),
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
                        let content_type = game_file_content_type(route, &file, &name).await;
                        let mut headers = vec![
                            ("content-type".into(), content_type.to_owned()),
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
                let mut headers = HeaderMap::new();
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

async fn game_file_content_type(
    route: vosu_adapter_upstream::files::Route,
    file: &vosu_surface::files::OpenedFile,
    name: &str,
) -> &'static str {
    use vosu_adapter_upstream::files::Route;

    const BINARY: &str = "application/octet-stream";
    match route {
        Route::Beatmap => "text/plain",
        Route::Background | Route::Audio => {
            const PREFIX_BYTES: u64 = 12;
            if file.metadata.size == 0 {
                return BINARY;
            }
            let Ok(prefix) = file
                .read(0, file.metadata.size.min(PREFIX_BYTES) as u32)
                .await
            else {
                return BINARY;
            };
            if route == Route::Background {
                image_media_type(&prefix).unwrap_or(BINARY)
            } else {
                audio_media_type(&prefix).unwrap_or(BINARY)
            }
        }
        _ => match vosu_surface::files::content_type(Path::new(name)) {
            "" => BINARY,
            content_type => content_type,
        },
    }
}

fn image_media_type(prefix: &[u8]) -> Option<&'static str> {
    if prefix.starts_with(b"RIFF") && prefix.get(8..12) == Some(b"WEBP") {
        return Some("image/webp");
    }
    match prefix {
        [0x89, b'P', b'N', b'G', 0x0d, 0x0a, 0x1a, 0x0a, ..] => Some("image/png"),
        [0xff, 0xd8, 0xff, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', b'7' | b'9', b'a', ..] => Some("image/gif"),
        [b'B', b'M', ..] => Some("image/bmp"),
        _ => None,
    }
}

fn audio_media_type(prefix: &[u8]) -> Option<&'static str> {
    if prefix.starts_with(b"RIFF") && prefix.get(8..12) == Some(b"WAVE") {
        return Some("audio/wav");
    }
    match prefix {
        [b'O', b'g', b'g', b'S', 0, ..] => Some("audio/ogg"),
        [b'f', b'L', b'a', b'C', ..] => Some("audio/flac"),
        [b'I', b'D', b'3', 2..=4, ..] => Some("audio/mpeg"),
        [0xff, flags, rate, _, ..]
            if flags & 0xe0 == 0xe0
                && flags & 0x18 != 0x08
                && flags & 0x06 != 0
                && rate & 0xf0 != 0xf0
                && rate & 0x0c != 0x0c =>
        {
            Some("audio/mpeg")
        }
        _ => None,
    }
}
