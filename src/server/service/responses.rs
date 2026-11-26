
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
        Ok(UpdateRouteResult::MajorAvailable {
            version,
            release_url,
        }) => json_response(
            StatusCode::OK,
            json!({ "status": "majorAvailable", "version": version, "releaseUrl": release_url }),
        ),
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

/// A dashboard card preview (`?vosu-preview=1`): the preview page itself,
/// or a WebSocket its marker script opened.
fn is_preview_request(request: &Request<Body>) -> bool {
    request.uri().query().is_some_and(|query| {
        query
            .split('&')
            .any(|pair| pair == crate::server::pages::OVERLAY_PREVIEW_QUERY)
    })
}

/// Search failures are dominated by catalog fetches, so they surface as 502
/// with the same escaped `Error:` text the dynamic pages render.
fn counter_search_response(result: Result<Bytes, String>) -> Response {
    match result {
        Ok(body) => response(StatusCode::OK, "text/html; charset=utf-8", body),
        Err(error) => response(
            StatusCode::BAD_GATEWAY,
            "text/html; charset=utf-8",
            Bytes::from(format!("Error: {}", vosu_core::report::escape_html(&error))),
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
