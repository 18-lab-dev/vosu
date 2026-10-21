use super::*;
use crate::server::files::{self, MissingKind, ResourceOutcome};
use axum::{
    body::{Body, Bytes},
    extract::{FromRequestParts, Request, WebSocketUpgrade},
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use futures_util::{SinkExt as _, StreamExt as _};

fn json_response(status: StatusCode, value: Value) -> Response {
    (status, axum::Json(value)).into_response()
}
fn error(status: StatusCode, message: impl AsRef<str>) -> Response {
    json_response(status, json!({"error": message.as_ref()}))
}
fn text_header(headers: &HeaderMap, key: &str) -> Option<String> {
    headers
        .get(key)
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned)
}

/// Remove both fixed hop-by-hop headers and headers named by Connection.
fn stripped(headers: &HeaderMap) -> HeaderMap {
    let named = headers
        .get_all(header::CONNECTION)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(','))
        .map(|s| s.trim().to_ascii_lowercase())
        .collect::<HashSet<_>>();
    let mut clean = HeaderMap::new();
    for (name, value) in headers {
        if name.as_str().starts_with("x-vosu-")
            || named.contains(name.as_str())
            || matches!(
                name.as_str(),
                "connection"
                    | "keep-alive"
                    | "proxy-authenticate"
                    | "proxy-authorization"
                    | "te"
                    | "trailer"
                    | "transfer-encoding"
                    | "upgrade"
                    | "cookie"
                    | "set-cookie"
                    | "host"
                    | "content-length"
            )
        {
            continue;
        }
        clean.append(name.clone(), value.clone());
    }
    clean
}
impl ToolHost {
    fn token_owner(&self, token: &str, host_api: bool) -> Option<Arc<Entry>> {
        if let Some(owner) = self.cli_owner(token) {
            return Some(owner);
        }
        self.entries
            .lock()
            .values()
            .find(|e| {
                let state = e.state.lock();
                (state.status == "running" || (host_api && state.status == "starting"))
                    && state.token.as_ref().is_some_and(|s| equal(s, token))
            })
            .cloned()
    }
    pub(super) fn caller(
        &self,
        target: &Entry,
        headers: &HeaderMap,
        write: bool,
    ) -> Option<String> {
        if let Some(session) = text_header(headers, "x-vosu-tool-session")
            && self
                .sessions
                .lock()
                .iter()
                .any(|(secret, id)| id == &target.id && equal(secret, &session))
        {
            return Some("ui".into());
        }
        if let Some(token) = text_header(headers, "x-vosu-tool-token") {
            if let Some(owner) = self.cli_owner(&token) {
                return (owner.id == target.id
                    || owner
                        .manifest
                        .as_ref()
                        .is_some_and(|m| m.requires.iter().any(|d| d.id == target.id)))
                .then(|| format!("cli:{}", owner.id));
            }
            if let Some(owner) = self.token_owner(&token, false) {
                return owner
                    .manifest
                    .as_ref()
                    .is_some_and(|m| m.requires.iter().any(|d| d.id == target.id))
                    .then(|| format!("tool:{}", owner.id));
            }
            return None;
        }
        (!write).then(|| "overlay".into())
    }
    pub async fn dashboard(self: &Arc<Self>, request: Request, body: Bytes) -> Response {
        match self.dashboard_result(request, body).await {
            Ok(value) => json_response(StatusCode::OK, value),
            Err(message) => error(StatusCode::BAD_REQUEST, message),
        }
    }
    async fn dashboard_result(
        self: &Arc<Self>,
        request: Request,
        body: Bytes,
    ) -> Result<Value, String> {
        let path = request
            .uri()
            .path()
            .strip_prefix("/api/tools")
            .unwrap_or_default()
            .trim_start_matches('/');
        if path.is_empty() && request.method() == Method::GET {
            let entries = self.entries.lock().values().cloned().collect::<Vec<_>>();
            return Ok(json!({"tools":entries.iter().map(|e| self.view(e)).collect::<Vec<_>>() }));
        }
        let (id, action) = path.split_once('/').unwrap_or((path, ""));
        let entry = self.entry(id)?;
        if request.method() == Method::GET {
            return match action {
                "" => Ok(self.view(&entry)),
                "logs" => {
                    let after = url::form_urlencoded::parse(
                        request.uri().query().unwrap_or_default().as_bytes(),
                    )
                    .find(|(k, _)| k == "after")
                    .and_then(|(_, v)| v.parse::<u64>().ok())
                    .unwrap_or(0);
                    Ok(
                        json!({"lines":entry.logs.lock().iter().filter(|line| line.seq > after).collect::<Vec<_>>()}),
                    )
                }
                "settings" => Ok(json!({"settings":entry.schema, "values":self.settings(&entry)?})),
                _ => Err("unknown tool action".into()),
            };
        }
        if request.method() != Method::POST {
            return Err("method not allowed".into());
        }
        match action {
            "enable" | "disable" | "trust" => {
                let _gate = self.gate.lock().await;
                if action == "trust" && entry.manifest.is_none() {
                    return Err("invalid tools cannot be trusted".into());
                }
                {
                    let mut saved = self.saved.lock();
                    let state = saved.entry(id.into()).or_default();
                    if action == "trust" {
                        state.trusted = true;
                    } else {
                        state.enabled = Some(action == "enable");
                    }
                    atomic_json(&self.config.join("state.json"), &*saved)
                        .map_err(|e| e.to_string())?;
                }
                if action == "disable" {
                    self.cli_sessions
                        .lock()
                        .retain(|_, session| session.tool != id);
                    self.stop(&entry).await;
                }
            }
            "start" => self.start(id, true).await?,
            "stop" => self.stop_serialized(&entry, true).await,
            "restart" => {
                self.stop_serialized(&entry, false).await;
                self.start(id, true).await?;
            }
            "launch" => {
                if !matches!(
                    self.status(&entry).0.as_str(),
                    "stopped" | "starting" | "running"
                ) {
                    return Err("tool is unavailable".into());
                }
                if entry.manifest.as_ref().is_none_or(|m| m.ui.is_none()) {
                    return Err("tool has no UI".into());
                }
                let code = secret().map_err(|e| e.to_string())?;
                {
                    let mut codes = self.codes.lock();
                    codes.retain(|_, c| c.expires > Instant::now());
                    if codes.len() >= 1024 {
                        return Err("too many pending launches".into());
                    }
                    codes.insert(
                        code.clone(),
                        Code {
                            tool: id.into(),
                            expires: Instant::now() + Duration::from_secs(60),
                        },
                    );
                }
                let theme = serde_json::from_slice::<Value>(&body)
                    .ok()
                    .and_then(|v| v.get("theme").and_then(Value::as_str).map(str::to_owned))
                    .filter(|s| matches!(s.as_str(), "dark" | "light"))
                    .unwrap_or_else(|| "dark".into());
                return Ok(
                    json!({"url":format!("{}/tools/{id}/#vosu-launch={code}&theme={theme}",self.overlay_url())}),
                );
            }
            "open-data" => {
                fs::create_dir_all(&entry.data_dir).map_err(|e| e.to_string())?;
                open::that_detached(&entry.data_dir).map_err(|e| e.to_string())?;
            }
            "clear-cache" => {
                let _gate = self.gate.lock().await;
                if entry.state.lock().task.is_some() {
                    return Err("stop the tool before clearing its cache".into());
                }
                if entry.cache_dir.exists() {
                    fs::remove_dir_all(&entry.cache_dir).map_err(|e| e.to_string())?;
                }
                fs::create_dir_all(&entry.cache_dir).map_err(|e| e.to_string())?;
            }
            "settings" => {
                let inputs: Vec<SettingInput> =
                    serde_json::from_slice(&body).map_err(|e| format!("settings: {e}"))?;
                let mut values = self.settings(&entry)?;
                for item in inputs {
                    if let Some(setting) =
                        entry.schema.iter().find(|s| s.unique_id == item.unique_id)
                    {
                        let value = match setting.kind {
                            crate::counters::SettingType::Number => {
                                crate::counters::coerce_number(item.value)
                            }
                            crate::counters::SettingType::Checkbox => {
                                crate::counters::coerce_checkbox(item.value)
                            }
                            _ => item.value,
                        };
                        values.insert(setting.unique_id.clone(), value);
                    }
                }
                atomic_json(
                    &self.config.join("settings").join(format!("{id}.json")),
                    &values,
                )
                .map_err(|e| e.to_string())?;
                return Ok(json!({"result":"success"}));
            }
            _ => return Err("unknown tool action".into()),
        }
        Ok(json!({"result":"success"}))
    }
    pub async fn overlay(self: &Arc<Self>, request: Request, body: Bytes) -> Response {
        let path = request
            .uri()
            .path()
            .strip_prefix("/tools/")
            .unwrap_or_default()
            .to_owned();
        if let Some(host) = path.strip_prefix("_host/v1/") {
            if host == "cli" || host.starts_with("cli/") {
                return self.cli_api(host, request, &body).await;
            }
            return self.host_api(host, &request, &body);
        }
        let (id, tail) = path.split_once('/').unwrap_or((&path, ""));
        let Ok(entry) = self.entry(id) else {
            return error(StatusCode::NOT_FOUND, "unknown tool");
        };
        if let Some(api) = tail.strip_prefix("api/") {
            return self.proxy(entry, format!("/{api}"), request, body).await;
        }
        if !matches!(request.method().as_str(), "GET" | "HEAD") {
            return error(StatusCode::NOT_FOUND, "not found");
        }
        if !matches!(
            self.status(&entry).0.as_str(),
            "running" | "stopped" | "starting"
        ) {
            return error(StatusCode::SERVICE_UNAVAILABLE, "tool is unavailable");
        }
        let Some(manifest) = &entry.manifest else {
            return error(StatusCode::NOT_FOUND, "not found");
        };
        let (root, relative) = if tail == "_icon" {
            let Some(icon) = &manifest.icon else {
                return error(StatusCode::NOT_FOUND, "not found");
            };
            (entry.root.clone(), icon.as_str())
        } else {
            let Some(ui) = &manifest.ui else {
                return error(StatusCode::NOT_FOUND, "tool has no UI");
            };
            let confined = std::fs::canonicalize(&entry.root)
                .ok()
                .zip(std::fs::canonicalize(entry.root.join(&ui.root)).ok())
                .is_some_and(|(tool, ui)| ui.starts_with(tool));
            if !confined {
                return error(StatusCode::NOT_FOUND, "unsafe UI root");
            }
            (
                entry.root.join(&ui.root),
                if tail.is_empty() {
                    ui.entry.as_str()
                } else {
                    tail
                },
            )
        };
        if !request.uri().path().ends_with('/') && tail.is_empty() {
            return (
                StatusCode::FOUND,
                [(header::LOCATION, format!("{}/", request.uri().path()))],
            )
                .into_response();
        }
        match files::resolve_encoded_resource(
            Some(&root),
            relative,
            request.uri().path(),
            MissingKind::NotFound,
        ) {
            Ok(ResourceOutcome::File(file)) => {
                let mut response = Response::builder()
                    .header(header::CONTENT_TYPE, file.content_type)
                    .header(header::CONTENT_LENGTH, file.length);
                response = response.header(header::X_CONTENT_TYPE_OPTIONS, "nosniff");
                let body = if request.method() == Method::HEAD {
                    Body::empty()
                } else {
                    Body::from_stream(tokio_util::io::ReaderStream::new(
                        tokio::fs::File::from_std(file.file),
                    ))
                };
                response.body(body).unwrap()
            }
            Ok(ResourceOutcome::Redirect { location }) => {
                (StatusCode::FOUND, [(header::LOCATION, location)]).into_response()
            }
            _ => error(StatusCode::NOT_FOUND, "not found"),
        }
    }
    fn host_api(&self, action: &str, request: &Request, body: &[u8]) -> Response {
        if request.method() == Method::GET && action == "bridge.js" {
            let entries = self
                .entries
                .lock()
                .values()
                .filter_map(|e| {
                    e.manifest
                        .as_ref()
                        .map(|m| json!({"id":m.id,"name":m.name,"version":m.version}))
                })
                .collect::<Vec<_>>();
            let source = format!(
                "const metadata = {};\nconst dashboardUrl = {};\n{}",
                json!(entries),
                json!(self.dashboard_url()),
                include_str!("../../assets/server/tool-bridge.js")
            );
            return (
                [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                source,
            )
                .into_response();
        }
        if request.method() == Method::GET && action == "theme.css" {
            return crate::assets::ServerAssets::get("tokens.css").map_or_else(
                || error(StatusCode::NOT_FOUND, "theme unavailable"),
                |asset| {
                    (
                        [(header::CONTENT_TYPE, "text/css; charset=utf-8")],
                        asset.data.into_owned(),
                    )
                        .into_response()
                },
            );
        }
        if request.method() == Method::POST && action == "session" {
            let Ok(value) = serde_json::from_slice::<Value>(body) else {
                return error(StatusCode::BAD_REQUEST, "invalid JSON");
            };
            let (Some(tool), Some(code)) = (
                value.get("tool").and_then(Value::as_str),
                value.get("code").and_then(Value::as_str),
            ) else {
                return error(StatusCode::BAD_REQUEST, "tool and code required");
            };
            let mut codes = self.codes.lock();
            let key = codes.keys().find(|key| equal(key, code)).cloned();
            let code = key.and_then(|key| codes.remove(&key));
            if !code.is_some_and(|code| code.tool == tool && code.expires > Instant::now()) {
                return error(StatusCode::FORBIDDEN, "invalid or expired launch code");
            }
            let Ok(session) = secret() else {
                return error(StatusCode::INTERNAL_SERVER_ERROR, "random source failed");
            };
            self.sessions.lock().insert(session.clone(), tool.into());
            return json_response(StatusCode::OK, json!({"session":session}));
        }
        if request.method() != Method::GET {
            return error(StatusCode::NOT_FOUND, "not found");
        }
        let owner = text_header(request.headers(), "x-vosu-tool-token")
            .and_then(|token| self.token_owner(&token, true));
        let Some(entry) = owner else {
            return error(StatusCode::UNAUTHORIZED, "invalid tool token");
        };
        match action {
            "self" => json_response(
                StatusCode::OK,
                json!({"id":entry.id,"version":entry.manifest.as_ref().map(|m| &m.version),"data_dir":entry.data_dir,"cache_dir":entry.cache_dir}),
            ),
            "settings" => match self.settings(&entry) {
                Ok(values) => json_response(StatusCode::OK, json!(values)),
                Err(e) => error(StatusCode::INTERNAL_SERVER_ERROR, e),
            },
            "context"
                if entry
                    .manifest
                    .as_ref()
                    .is_some_and(|m| m.permissions.host.iter().any(|p| p == "context")) =>
            {
                json_response(StatusCode::OK, (self.context)())
            }
            "context" => error(StatusCode::FORBIDDEN, "context permission required"),
            _ => error(StatusCode::NOT_FOUND, "not found"),
        }
    }
    async fn proxy(
        self: &Arc<Self>,
        entry: Arc<Entry>,
        path: String,
        request: Request,
        body: Bytes,
    ) -> Response {
        let path = match files::decode_request_path(&path) {
            Ok(path)
                if !path.contains('\\')
                    && !path.split('/').any(|part| matches!(part, "." | "..")) =>
            {
                path
            }
            _ => return error(StatusCode::NOT_FOUND, "invalid tool API path"),
        };
        let Some(write) = entry
            .manifest
            .as_ref()
            .and_then(|m| m.access(request.method().as_str(), &path))
        else {
            return error(StatusCode::NOT_FOUND, "unlisted tool route");
        };
        let Some(caller) = self.caller(&entry, request.headers(), write) else {
            return error(
                StatusCode::FORBIDDEN,
                "tool UI session or dependent tool token required",
            );
        };
        let websocket = request
            .headers()
            .get(header::UPGRADE)
            .is_some_and(|v| v.as_bytes().eq_ignore_ascii_case(b"websocket"));
        if websocket && write {
            return error(StatusCode::FORBIDDEN, "WebSockets require a read route");
        }
        let Ok(_pending) = self.pending.try_acquire() else {
            return error(
                StatusCode::SERVICE_UNAVAILABLE,
                "too many pending tool requests",
            );
        };
        if let Err(message) = self.start(&entry.id, false).await {
            return error(StatusCode::SERVICE_UNAVAILABLE, message);
        }
        let activity = Activity::new(entry.clone());
        let (port, token) = {
            let mut state = entry.state.lock();
            state.last_used = Instant::now();
            let (Some(port), Some(token)) = (state.port, state.token.clone()) else {
                return error(
                    StatusCode::SERVICE_UNAVAILABLE,
                    "service stopped before forwarding",
                );
            };
            (port, token)
        };
        let suffix = request
            .uri()
            .query()
            .map(|q| format!("?{q}"))
            .unwrap_or_default();
        let encoded_path = path
            .split('/')
            .map(files::encode_path_segment)
            .collect::<Vec<_>>()
            .join("/");
        let url = format!("http://127.0.0.1:{port}{encoded_path}{suffix}");
        let mut headers = stripped(request.headers());
        headers.insert(
            HeaderName::from_static("x-vosu-tool-token"),
            HeaderValue::from_str(&token).unwrap(),
        );
        headers.insert(
            HeaderName::from_static("x-vosu-caller"),
            HeaderValue::from_str(&caller).unwrap(),
        );
        if websocket {
            use tokio_tungstenite::tungstenite::client::IntoClientRequest as _;
            let Some(registration) = request
                .extensions()
                .get::<Arc<crate::server::websocket::UpgradeCallbackTracker>>()
                .and_then(|tracker| tracker.register())
            else {
                return error(StatusCode::SERVICE_UNAVAILABLE, "listener is draining");
            };
            let mut upstream = match url.replacen("http:", "ws:", 1).into_client_request() {
                Ok(v) => v,
                Err(e) => return error(StatusCode::BAD_GATEWAY, e.to_string()),
            };
            upstream.headers_mut().extend(headers);
            let config = tokio_tungstenite::tungstenite::protocol::WebSocketConfig::default()
                .max_frame_size(Some(crate::server::websocket::MAX_FRAME_BYTES))
                .max_message_size(Some(crate::server::websocket::MAX_MESSAGE_BYTES));
            let (upstream, handshake) =
                match tokio_tungstenite::connect_async_with_config(upstream, Some(config), false)
                    .await
                {
                    Ok(v) => v,
                    Err(e) => return error(StatusCode::BAD_GATEWAY, e.to_string()),
                };
            let (mut parts, _) = request.into_parts();
            let upgrade = match WebSocketUpgrade::from_request_parts(&mut parts, &()).await {
                Ok(v) => v,
                Err(e) => return e.into_response(),
            };
            let upgrade = if let Some(protocol) = handshake
                .headers()
                .get(header::SEC_WEBSOCKET_PROTOCOL)
                .and_then(|v| v.to_str().ok())
            {
                upgrade.protocols([protocol.to_owned()])
            } else {
                upgrade
            };
            let upgrade = upgrade
                .max_frame_size(crate::server::websocket::MAX_FRAME_BYTES)
                .max_message_size(crate::server::websocket::MAX_MESSAGE_BYTES);
            let stop = self.cancel.clone();
            return upgrade.on_upgrade(move |downstream| async move {
                let _activity = activity;
                registration
                    .run(bridge_websocket(downstream, upstream, stop))
                    .await;
            });
        }
        let upstream = match self
            .http
            .request(request.method().clone(), url)
            .headers(headers)
            .body(body)
            .send()
            .await
        {
            Ok(response) => response,
            Err(e) => return error(StatusCode::BAD_GATEWAY, e.to_string()),
        };
        let status = upstream.status();
        let headers = stripped(upstream.headers());
        let stream = upstream.bytes_stream().map(move |chunk| {
            let _activity = &activity;
            chunk
        });
        let mut response = Response::new(Body::from_stream(stream));
        *response.status_mut() = status;
        *response.headers_mut() = headers;
        response
    }
}
async fn bridge_websocket(
    downstream: axum::extract::ws::WebSocket,
    upstream: tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
    stop: CancellationToken,
) {
    use axum::extract::ws::Message as Down;
    use tokio_tungstenite::tungstenite::Message as Up;
    let (mut ds, mut dr) = downstream.split();
    let (mut us, mut ur) = upstream.split();
    tokio::select! {
        () = stop.cancelled() => {},
        () = async { while let Some(Ok(message)) = dr.next().await { let message = match message { Down::Text(v)=>Up::Text(v.as_str().into()),Down::Binary(v)=>Up::Binary(v),Down::Ping(v)=>Up::Ping(v),Down::Pong(v)=>Up::Pong(v),Down::Close(_)=>Up::Close(None) }; if us.send(message).await.is_err() { break; } } } => {},
        () = async { while let Some(Ok(message)) = ur.next().await { let message = match message { Up::Text(v)=>Down::Text(v.as_str().into()),Up::Binary(v)=>Down::Binary(v),Up::Ping(v)=>Down::Ping(v),Up::Pong(v)=>Down::Pong(v),Up::Close(_)=>Down::Close(None),Up::Frame(_)=>continue }; if ds.send(message).await.is_err() { break; } } } => {},
    }
    let _ = tokio::time::timeout(Duration::from_secs(1), async {
        let _ = ds.close().await;
        let _ = us.close().await;
    })
    .await;
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn strips_proxy_credentials_and_connection_named_headers() {
        let mut headers = HeaderMap::new();
        for (name, value) in [
            ("connection", "x-private, Upgrade"),
            ("x-private", "secret"),
            ("cookie", "secret"),
            ("x-vosu-tool-token", "spoof"),
            ("x-vosu-caller", "spoof"),
            ("content-type", "application/json"),
        ] {
            headers.insert(
                HeaderName::from_bytes(name.as_bytes()).unwrap(),
                HeaderValue::from_static(value),
            );
        }
        let clean = stripped(&headers);
        assert_eq!(clean.len(), 1);
        assert!(clean.contains_key("content-type"));
    }
}
