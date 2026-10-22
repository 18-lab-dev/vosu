fn skin_metadata(selected: &BeatmapFiles) -> Value {
    let skin = selected.skin.as_deref();
    json!({
        "client": selected.skin_client,
        "name": selected.skin_name,
        "files": if selected.lazer { skin.is_some_and(|s| !s.files.is_empty()) }
            else { selected.skin_root.as_ref().is_some_and(|p| p.is_dir()) },
        "builtin": skin.and_then(|s| s.builtin.as_deref()),
    })
}

#[derive(Debug)]
enum FileRouteError {
    Unsafe,
}

impl From<files::SafePathError> for FileRouteError {
    fn from(_: files::SafePathError) -> Self {
        Self::Unsafe
    }
}

fn file_route_error_response(error: FileRouteError) -> Response {
    match error {
        FileRouteError::Unsafe => not_found(),
    }
}

fn resolve_file_endpoint(
    endpoint: Endpoint,
    selected: &BeatmapFiles,
    path: &str,
    target: &str,
) -> Result<ResourceOutcome, FileRouteError> {
    let outcome = match endpoint {
        Endpoint::BackgroundImage | Endpoint::BeatmapBackground => resolve_exact(
            selected.songs_root.as_deref(),
            selected.background_file.as_deref(),
            selected_missing(selected),
        )?,
        Endpoint::BeatmapAudio => resolve_exact(
            selected.songs_root.as_deref(),
            selected.audio_file.as_deref(),
            selected_missing(selected),
        )?,
        Endpoint::BeatmapFile => resolve_exact(
            selected.songs_root.as_deref(),
            selected.beatmap_file.as_deref(),
            selected_missing(selected),
        )?,
        Endpoint::SongsFile => files::resolve_encoded_resource(
            selected.songs_root.as_deref(),
            path.strip_prefix("/Songs/").unwrap_or_default(),
            target,
            root_missing(selected.songs_root.as_deref(), selected),
        )?,
        Endpoint::BeatmapPath => files::resolve_encoded_resource(
            selected.songs_root.as_deref(),
            path.strip_prefix("/files/beatmap/").unwrap_or_default(),
            target,
            root_missing(selected.songs_root.as_deref(), selected),
        )?,
        Endpoint::SkinFile if selected.lazer => {
            let encoded = path.strip_prefix("/files/skin/").unwrap_or_default();
            let name = files::decode_request_path(encoded)?.replace('\\', "/");
            if name.is_empty() || name.split('/').any(|p| p.is_empty() || p == "." || p == ".." || p.contains(':')) {
                return Ok(ResourceOutcome::Missing(MissingKind::NotFound));
            }
            let Some(relative) = selected.skin.as_ref().and_then(|s| s.files.get(&name.to_lowercase())) else {
                return Ok(ResourceOutcome::Missing(MissingKind::NotFound));
            };
            let mut outcome = files::resolve_resource(selected.songs_root.as_deref(), relative, target, MissingKind::NotFound)?;
            if let ResourceOutcome::File(file) = &mut outcome {
                file.content_type = files::content_type(Path::new(&name));
            }
            outcome
        }
        Endpoint::SkinFile => files::resolve_encoded_resource(
            selected.skin_root.as_deref(),
            path.strip_prefix("/files/skin/").unwrap_or_default(),
            target,
            selected_missing(selected),
        )?,
        Endpoint::CounterStatic => {
            let relative = path.trim_start_matches('/');
            if path.ends_with('/') {
                let index_path = format!("{relative}index.html");
                match files::resolve_encoded_counter_resource(
                    selected.static_root.as_deref(),
                    &index_path,
                    target,
                    MissingKind::NotFound,
                )? {
                    outcome @ (ResourceOutcome::File(_) | ResourceOutcome::Missing(_)) => outcome,
                    ResourceOutcome::DirectoryIndex { .. } | ResourceOutcome::Redirect { .. } => {
                        ResourceOutcome::Missing(MissingKind::NotFound)
                    }
                }
            } else {
                files::resolve_encoded_counter_resource(
                    selected.static_root.as_deref(),
                    relative,
                    target,
                    MissingKind::NotFound,
                )?
            }
        }
        _ => unreachable!(),
    };
    Ok(outcome)
}

fn root_missing(root: Option<&Path>, files: &BeatmapFiles) -> MissingKind {
    if root.is_none() {
        MissingKind::NotReady
    } else {
        selected_missing(files)
    }
}

fn selected_missing(files: &BeatmapFiles) -> MissingKind {
    if files.ready {
        MissingKind::NotFound
    } else {
        MissingKind::NotReady
    }
}

fn resolve_exact(
    root: Option<&Path>,
    relative: Option<&Path>,
    missing: MissingKind,
) -> Result<ResourceOutcome, files::SafePathError> {
    let (Some(root), Some(relative)) = (root, relative) else {
        return Ok(ResourceOutcome::Missing(missing));
    };
    let Some(relative) = relative.to_str() else {
        return Ok(ResourceOutcome::Missing(MissingKind::NotFound));
    };
    match files::resolve_resource(Some(root), relative, relative, missing)? {
        outcome @ (ResourceOutcome::File(_) | ResourceOutcome::Missing(_)) => Ok(outcome),
        ResourceOutcome::DirectoryIndex { .. } | ResourceOutcome::Redirect { .. } => {
            Ok(ResourceOutcome::Missing(MissingKind::NotFound))
        }
    }
}

async fn resource_response(outcome: ResourceOutcome, range: Option<String>) -> Response {
    resource_response_with_counter_path(outcome, range, None).await
}

async fn counter_resource_response(
    outcome: ResourceOutcome,
    range: Option<String>,
    encoded_html_path: &str,
) -> Response {
    resource_response_with_counter_path(outcome, range, Some(encoded_html_path)).await
}

async fn resource_response_with_counter_path(
    outcome: ResourceOutcome,
    range: Option<String>,
    encoded_html_path: Option<&str>,
) -> Response {
    match outcome {
        ResourceOutcome::Missing(MissingKind::NotReady) => json_response(
            StatusCode::INTERNAL_SERVER_ERROR,
            json!({ "error": "osu is not ready/running" }),
        ),
        ResourceOutcome::Missing(MissingKind::NotFound) => response(
            StatusCode::NOT_FOUND,
            "text/plain; charset=utf-8",
            Bytes::from_static(NOT_FOUND.as_bytes()),
        ),
        ResourceOutcome::Redirect { location } => redirect(location),
        ResourceOutcome::DirectoryIndex { html } => response(
            StatusCode::OK,
            "text/html; charset=utf-8",
            Bytes::from(html),
        ),
        ResourceOutcome::File(target) => {
            let range = range.as_deref();
            let plan = match files::plan_file_response(&target, range) {
                Ok(plan) => plan,
                Err(_) => {
                    return Response::builder()
                        .status(StatusCode::RANGE_NOT_SATISFIABLE)
                        .header(header::CONTENT_RANGE, format!("bytes */{}", target.length))
                        .body(Body::empty())
                        .expect("valid range response");
                }
            };
            let suffix = if plan.status == StatusCode::OK.as_u16()
                && plan.content_type == "text/html; charset=utf-8"
            {
                encoded_html_path.and_then(counter_path_script)
            } else {
                None
            };
            let content_length = if let Some(suffix) = suffix.as_ref() {
                let Ok(suffix_length) = u64::try_from(suffix.len()) else {
                    return internal_error("counter metadata length exceeds response limits");
                };
                let Some(content_length) = plan.content_length.checked_add(suffix_length) else {
                    return internal_error("counter response length overflow");
                };
                content_length
            } else {
                plan.content_length
            };
            let mut file = tokio::fs::File::from_std(target.file);
            if file.seek(SeekFrom::Start(plan.offset)).await.is_err() {
                return internal_error("failed to seek file");
            }
            let suffix_stream = futures_util::stream::iter(
                suffix
                    .into_iter()
                    .map(Ok::<Bytes, std::io::Error>),
            );
            let body = Body::from_stream(
                ReaderStream::new(file.take(plan.content_length)).chain(suffix_stream),
            );
            let mut builder = Response::builder()
                .status(plan.status)
                .header(header::CONTENT_TYPE, plan.content_type)
                .header(header::CONTENT_LENGTH, content_length);
            if let Some(value) = plan.accept_ranges {
                builder = builder.header(header::ACCEPT_RANGES, value);
            }
            if let Some(value) = plan.content_range {
                builder = builder.header(header::CONTENT_RANGE, value);
            }
            builder.body(body).expect("valid file response")
        }
    }
}


fn counter_path_script(encoded_html_path: &str) -> Option<Bytes> {
    let (encoded_html_path, profile) = encoded_html_path.split_once("?vosuProfile=")
        .map_or((encoded_html_path, None), |(path, profile)| (path, Some(profile)));
    let decoded_html_path = percent_decode_once(encoded_html_path).ok()?;
    let logical_path = decoded_html_path.trim_start_matches('/');
    let counter_path = logical_path
        .rsplit_once('/')
        .map_or("", |(parent, _)| parent);
    let mut script = String::with_capacity(counter_path.len() + 48);
    script.push_str("<script>window.COUNTER_PATH=");
    if let Some(profile) = profile {
        push_javascript_string(&mut script, &crate::counters::qualified_counter(counter_path, profile));
        script.push_str(";window.VOSU_PROFILE=");
        push_javascript_string(&mut script, profile);
    } else {
        push_javascript_string(&mut script, counter_path);
    }
    script.push_str(";</script>");
    Some(Bytes::from(script))
}

fn push_javascript_string(output: &mut String, value: &str) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";

    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{0008}' => output.push_str("\\b"),
            '\u{000C}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            '<' => output.push_str("\\u003C"),
            '>' => output.push_str("\\u003E"),
            '&' => output.push_str("\\u0026"),
            '\u{2028}' => output.push_str("\\u2028"),
            '\u{2029}' => output.push_str("\\u2029"),
            '\u{0000}'..='\u{001F}' => {
                let byte = character as u8;
                output.push_str("\\u00");
                output.push(HEX[usize::from(byte >> 4)] as char);
                output.push(HEX[usize::from(byte & 0x0F)] as char);
            }
            _ => output.push(character),
        }
    }
    output.push('"');
}

fn embedded_response(page: EmbeddedResponse) -> Response {
    response(
        StatusCode::from_u16(page.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
        page.content_type,
        page.body,
    )
}

fn update_response(
    result: Result<UpdateRouteResult, String>,
    barrier: Option<Arc<ResponseFlushBarrier>>,
) -> Response {
    match result {
        Ok(UpdateRouteResult::Updated(trigger)) => deferred_update_response(trigger, barrier),
        Ok(UpdateRouteResult::Latest) => {
            json_response(StatusCode::OK, json!({ "status": "latest" }))
        }
        Ok(UpdateRouteResult::NoFiles) => {
            json_response(StatusCode::OK, json!({ "status": "noFiles" }))
        }
        Err(error) => json_response(StatusCode::OK, json!({ "error": error })),
    }
}

struct DeferredUpdateBody {
    payload: Option<Bytes>,
    trigger: Option<DeferredUpdateTrigger>,
    barrier: Option<Arc<ResponseFlushBarrier>>,
}

impl DeferredUpdateBody {
    fn new(
        payload: Bytes,
        trigger: DeferredUpdateTrigger,
        barrier: Option<Arc<ResponseFlushBarrier>>,
    ) -> Self {
        Self {
            payload: Some(payload),
            trigger: Some(trigger),
            barrier,
        }
    }

    fn arm_after_eof(&mut self) {
        if let Some(trigger) = self.trigger.take() {
            if let Some(barrier) = self.barrier.take() {
                barrier.arm(Box::new(move || trigger.fire()));
            } else {
                trigger.fire();
            }
        }
    }
}

impl futures_util::Stream for DeferredUpdateBody {
    type Item = Result<Bytes, Infallible>;

    fn poll_next(mut self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if let Some(payload) = self.payload.take() {
            return Poll::Ready(Some(Ok(payload)));
        }
        self.arm_after_eof();
        Poll::Ready(None)
    }
}

impl Drop for DeferredUpdateBody {
    fn drop(&mut self) {
        // A pre-EOF drop is the explicit disconnect policy. Fire directly:
        // Hyper can drop TrackedIo before the body field, so waiting on an I/O
        // callback here could orphan a prepared update.
        if let Some(trigger) = self.trigger.take() {
            trigger.fire();
        }
    }
}

fn deferred_update_response(
    trigger: DeferredUpdateTrigger,
    barrier: Option<Arc<ResponseFlushBarrier>>,
) -> Response {
    let payload = json_bytes(json!({ "status": "updated" }));
    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, JSON)
        .header(header::CONTENT_LENGTH, payload.len())
        .body(Body::from_stream(DeferredUpdateBody::new(
            payload, trigger, barrier,
        )))
        .expect("valid deferred update response")
}

fn query_pairs(request: &Request<Body>) -> Vec<(String, String)> {
    url::form_urlencoded::parse(request.uri().query().unwrap_or_default().as_bytes())
        .into_owned()
        .collect()
}

fn decoded_query_value(query: &str, key: &str) -> Option<String> {
    url::form_urlencoded::parse(query.as_bytes())
        .find_map(|(candidate, value)| (candidate == key).then(|| value.into_owned()))
}

/// Search mode is explicit: `source=available` searches the catalog and an
/// absent source (or `installed`) searches local overlays. Any other value is
/// rejected rather than silently falling back to one of the lists.
fn counter_search_source(query: Option<&str>) -> Option<bool> {
    match query
        .and_then(|query| decoded_query_value(query, "source"))
        .as_deref()
    {
        None | Some("installed") => Some(false),
        Some("available") => Some(true),
        Some(_) => None,
    }
}

/// Search failures are dominated by catalog fetches, so they surface as 502
/// with the same escaped `Error:` text the dynamic pages render.
fn counter_search_response(result: Result<Bytes, String>) -> Response {
    match result {
        Ok(body) => response(StatusCode::OK, "text/html; charset=utf-8", body),
        Err(error) => response(
            StatusCode::BAD_GATEWAY,
            "text/html; charset=utf-8",
            Bytes::from(format!("Error: {}", crate::report::escape_html(&error))),
        ),
    }
}

fn namespaced_client_id(namespace: u64, local_id: u64) -> Option<u64> {
    local_id.checked_mul(8)?.checked_add(namespace)
}
fn query_value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find_map(|(candidate, value)| (candidate == key).then_some(value.as_str()))
}
fn decoded_capture(request: &Request<Body>, prefix: &str) -> Option<String> {
    percent_decode_once(request.uri().path().strip_prefix(prefix)?).ok()
}
fn percent_decode_once(raw: &str) -> Result<String, ()> {
    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return Err(());
            }
            let high = hex(bytes[index + 1]).ok_or(())?;
            let low = hex(bytes[index + 2]).ok_or(())?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).map_err(|_| ())
}
fn hex(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}
fn redirect(location: String) -> Response {
    Response::builder()
        .status(StatusCode::MOVED_PERMANENTLY)
        .header(header::LOCATION, location)
        .body(Body::empty())
        .expect("valid redirect")
}
fn response(status: StatusCode, content_type: &'static str, body: Bytes) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, content_type)
        .body(Body::from(body))
        .expect("valid response")
}
fn json_response(status: StatusCode, value: Value) -> Response {
    response(status, JSON, json_bytes(value))
}
fn json_bytes(value: Value) -> Bytes {
    Bytes::from(serde_json::to_vec(&value).expect("JSON value serialization cannot fail"))
}
fn not_found() -> Response {
    response(
        StatusCode::NOT_FOUND,
        "text/plain; charset=utf-8",
        Bytes::from_static(NOT_FOUND.as_bytes()),
    )
}
fn internal_error(message: impl ToString) -> Response {
    json_response(
        StatusCode::INTERNAL_SERVER_ERROR,
        json!({ "error": message.to_string() }),
    )
}
fn calculation_error(status: StatusCode, message: impl ToString) -> CalculateResult {
    CalculateResult::Failed {
        status,
        body: json_bytes(json!({ "error": message.to_string() })),
    }
}
