// HTTP mechanics for the upstream adapter's retained-object selections.
#[derive(Clone, Default)]
pub struct DeclaredFileProvider(pub vosu_adapter_upstream::files::Provider);
impl RouteDelegate for DeclaredFileProvider {
    fn handle(&self, endpoint: Endpoint, request: Request<Body>, _: Bytes) -> ProviderFuture<'_, Response> {
        Box::pin(async move {
            if matches!(endpoint, Endpoint::BackgroundImage | Endpoint::BeatmapBackground | Endpoint::BeatmapAudio | Endpoint::BeatmapFile | Endpoint::SongsFile | Endpoint::BeatmapPath | Endpoint::SkinFile) {
                declared_game_response(&self.0, endpoint, request).await
            } else { not_found() }
        })
    }
}
async fn declared_game_response(
    provider: &vosu_adapter_upstream::files::Provider,
    endpoint: Endpoint,
    request: Request<Body>,
) -> Response {
    use vosu_adapter_upstream::files::{Outcome, Route};
    let path = request.uri().path();
    let (route, encoded) = match endpoint {
        Endpoint::BackgroundImage | Endpoint::BeatmapBackground => (Route::Background, ""),
        Endpoint::BeatmapAudio => (Route::Audio, ""),
        Endpoint::BeatmapFile => (Route::Beatmap, ""),
        Endpoint::SongsFile => (Route::Songs, path.strip_prefix("/Songs/").unwrap_or_default()),
        Endpoint::BeatmapPath => (Route::BeatmapPath, path.strip_prefix("/files/beatmap/").unwrap_or_default()),
        Endpoint::SkinFile => (Route::Skin, path.strip_prefix("/files/skin/").unwrap_or_default()),
        _ => unreachable!("only game files use declared roots"),
    };
    let head = request.method() == Method::HEAD;
    let mut response = match provider.resolve(route, encoded, &request.uri().to_string()).await {
        Outcome::Missing => not_found(),
        Outcome::NotReady => json_response(StatusCode::INTERNAL_SERVER_ERROR, json!({"error":"osu is not ready/running"})),
        Outcome::Redirect(location) => redirect(location),
        Outcome::Directory { entries, truncated } => {
            let entries = entries.into_iter().map(|entry| (entry.name, entry.kind == vosu_surface::files::EntryKind::Directory)).collect();
            let mut html = files::directory_index(entries, &request.uri().to_string());
            if truncated { html.push_str("\n<!-- directory listing truncated -->"); }
            response(StatusCode::OK, "text/html; charset=utf-8", Bytes::from(html))
        }
        Outcome::File { file, name, lazer } => {
            let conditional = !matches!(endpoint, Endpoint::SongsFile | Endpoint::BackgroundImage);
            return retained_file_response(file, &name, lazer, request.headers(), head, conditional).await;
        }
    };
    if head {
        use axum::body::HttpBody as _;
        if let Some(length) = response.body().size_hint().exact() {
            response.headers_mut().insert(header::CONTENT_LENGTH, length.into());
        }
        *response.body_mut() = Body::empty();
    }
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, axum::http::HeaderValue::from_static("sandbox"));
    response
}

async fn retained_file_response(
    file: vosu_surface::files::OpenedFile,
    name: &str,
    lazer: bool,
    headers: &axum::http::HeaderMap,
    head: bool,
    conditional: bool,
) -> Response {
    let validators = conditional.then(|| files::FileValidators::for_surface(&file, name, lazer));
    let mut response = if validators.as_ref().is_some_and(|validators| validators.not_modified(headers)) {
        Response::builder().status(StatusCode::NOT_MODIFIED).body(Body::empty()).expect("304 response")
    } else {
        let range = (!head).then(|| headers.get(header::RANGE).and_then(|v| v.to_str().ok())).flatten();
        let plan = files::plan_response(file.metadata.size, files::content_type(Path::new(name)), files::media_range_eligible(Path::new(name)), range);
        match plan {
            Err(_) => Response::builder().status(StatusCode::RANGE_NOT_SATISFIABLE)
                .header(header::CONTENT_RANGE, files::unsatisfied_content_range(file.metadata.size))
                .body(Body::empty()).expect("range response"),
            Ok(plan) => {
                let body = if head { Body::empty() } else {
                    let stream = futures_util::stream::try_unfold((file, plan.offset, plan.content_length), |(file, offset, remaining)| async move {
                        if remaining == 0 { return Ok::<_, std::io::Error>(None); }
                        let len = remaining.min(u64::from(vosu_surface::resources::MAX_READ_BYTES)) as u32;
                        let bytes = file.read(offset, len).await.map_err(std::io::Error::other)?;
                        if bytes.is_empty() || bytes.len() > len as usize {
                            return Err(std::io::Error::other("invalid retained file read"));
                        }
                        let count = bytes.len() as u64;
                        Ok(Some((Bytes::from(bytes), (file, offset + count, remaining - count))))
                    });
                    Body::from_stream(stream)
                };
                let mut builder = Response::builder().status(plan.status)
                    .header(header::CONTENT_TYPE, plan.content_type)
                    .header(header::CONTENT_LENGTH, plan.content_length);
                if let Some(value) = plan.accept_ranges { builder = builder.header(header::ACCEPT_RANGES, value); }
                if let Some(value) = plan.content_range { builder = builder.header(header::CONTENT_RANGE, value); }
                builder.body(body).expect("retained file response")
            }
        }
    };
    if let Some(validators) = validators { validators.apply(response.headers_mut()); }
    response.headers_mut().insert(header::CONTENT_SECURITY_POLICY, axum::http::HeaderValue::from_static("sandbox"));
    response
}
