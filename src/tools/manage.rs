//! Core's exact tool routes (A7.2): the dashboard-listener `/api/tools`
//! management routes and the overlay-listener launch-session exchange.
//! Routing has already matched the exact method and path, and enforced the
//! listener, dashboard provenance and `X-Vosu-CSRF` for every mutation.
use super::*;
use axum::{
    body::Bytes,
    extract::Request,
    http::{Method, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde_json::{Value, json};

/// Pending launch codes; launch fails without minting at this bound.
const MAX_CODES: usize = 1024;
const CODE_LIFETIME: Duration = Duration::from_secs(60);

fn json_response(status: StatusCode, value: Value) -> Response {
    (status, axum::Json(value)).into_response()
}
fn error(status: StatusCode, message: impl AsRef<str>) -> Response {
    json_response(status, json!({"error": message.as_ref()}))
}

impl ToolRegistry {
    /// `GET /api/tools[/<id>[/settings]]` and
    /// `POST /api/tools/<id>/{enable,disable,trust,launch,settings}`.
    pub async fn dashboard(self: &Arc<Self>, request: Request, body: Bytes) -> Response {
        match self.dashboard_result(request, body).await {
            Ok(value) => json_response(StatusCode::OK, value),
            Err(message) => error(StatusCode::BAD_REQUEST, message),
        }
    }

    async fn dashboard_result(&self, request: Request, body: Bytes) -> Result<Value, String> {
        let path = request
            .uri()
            .path()
            .strip_prefix("/api/tools")
            .unwrap_or_default()
            .trim_start_matches('/');
        if path.is_empty() && request.method() == Method::GET {
            let entries = self.entries.lock().values().cloned().collect::<Vec<_>>();
            return Ok(
                json!({"tools": entries.iter().map(|e| self.tool_view(e)).collect::<Vec<_>>()}),
            );
        }
        let (id, action) = path.split_once('/').unwrap_or((path, ""));
        let entry = self.entry(id)?;
        if request.method() == Method::GET {
            return match action {
                "" => Ok(self.tool_view(&entry)),
                "settings" => {
                    let values = self.settings(&entry)?;
                    Ok(match &entry.settings {
                        Some(schema) => schema.dashboard(values),
                        None => json!({"settings": [], "values": {}, "secrets": {}}),
                    })
                }
                _ => Err("unknown tool action".into()),
            };
        }
        if request.method() != Method::POST {
            return Err("method not allowed".into());
        }
        match action {
            "enable" | "disable" | "trust" => {
                if entry.legacy && action != "disable" {
                    return Err(NEEDS_UPDATE.into());
                }
                if action == "trust" && entry.manifest.is_none() {
                    return Err("invalid tools cannot be trusted".into());
                }
                let _gate = self.gate.lock().await;
                {
                    let mut saved = self.saved.lock();
                    let mut next = saved.clone();
                    let state = next.entry(id.into()).or_default();
                    if action == "trust" {
                        state.trusted = true;
                    } else {
                        state.enabled = Some(action == "enable");
                    }
                    atomic_json(&self.config.join("state.json"), &next)
                        .map_err(|e| e.to_string())?;
                    *saved = next;
                }
                if action == "disable" {
                    self.invalidate_tool(id);
                }
                drop(_gate);
                self.publish();
                Ok(json!({"result": "success"}))
            }
            "launch" => self.launch(&entry, &body),
            "settings" => {
                let schema = entry.settings.as_ref().ok_or("tool has no settings")?;
                let inputs: Vec<settings::Input> =
                    serde_json::from_slice(&body).map_err(|e| format!("settings: {e}"))?;
                let _gate = self.gate.lock().await;
                let mut values = self.settings(&entry)?;
                schema.apply(&mut values, inputs);
                atomic_json(&self.tool_settings(id), &values).map_err(|e| e.to_string())?;
                drop(_gate);
                self.publish();
                Ok(json!({"result": "success"}))
            }
            _ => Err("unknown tool action".into()),
        }
    }

    /// Mints one single-use, 60-second launch code (API v1 "Tool launch
    /// sessions" item 1).
    fn launch(&self, entry: &Entry, body: &[u8]) -> Result<Value, String> {
        if entry.legacy {
            return Err(NEEDS_UPDATE.into());
        }
        if self.stopping.load(Ordering::Acquire) || !self.servable(entry) {
            return Err("tool is unavailable".into());
        }
        let code = secret().map_err(|e| e.to_string())?;
        {
            let mut codes = self.codes.lock();
            codes.retain(|_, c| c.expires > Instant::now());
            if codes.len() >= MAX_CODES {
                return Err("too many pending launches".into());
            }
            codes.insert(
                code.clone(),
                Code {
                    tool: entry.id.clone(),
                    expires: Instant::now() + CODE_LIFETIME,
                },
            );
        }
        let theme = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|v| v.get("theme").and_then(Value::as_str).map(str::to_owned))
            .filter(|s| matches!(s.as_str(), "dark" | "light"))
            .unwrap_or_else(|| "dark".into());
        Ok(json!({"url": format!(
            "{}/tools/{}/#vosu-launch={code}&theme={theme}",
            self.overlay_url(),
            entry.id
        )}))
    }

    /// `POST /tools/_host/v1/session` `{"tool","code"}` (overlay listener):
    /// the code is consumed whether or not the exchange succeeds.
    pub fn exchange_session(&self, body: &[u8]) -> Response {
        let Ok(value) = serde_json::from_slice::<Value>(body) else {
            return error(StatusCode::BAD_REQUEST, "invalid JSON");
        };
        let (Some(tool), Some(code)) = (
            value.get("tool").and_then(Value::as_str),
            value.get("code").and_then(Value::as_str),
        ) else {
            return error(StatusCode::BAD_REQUEST, "tool and code required");
        };
        let issued = {
            let mut codes = self.codes.lock();
            let key = codes.keys().find(|key| equal(key, code)).cloned();
            key.and_then(|key| codes.remove(&key))
        };
        if !issued.is_some_and(|code| code.tool == tool && code.expires > Instant::now()) {
            return error(StatusCode::FORBIDDEN, "invalid or expired launch code");
        }
        let Ok(entry) = self.entry(tool) else {
            return error(StatusCode::FORBIDDEN, "tool is unavailable");
        };
        if self.stopping.load(Ordering::Acquire) || !self.servable(&entry) {
            return error(StatusCode::FORBIDDEN, "tool is unavailable");
        }
        let Ok(session) = secret() else {
            return error(StatusCode::INTERNAL_SERVER_ERROR, "random source failed");
        };
        {
            let mut sessions = self.sessions.lock();
            sessions.retain(|_, record| record.is_live());
            if sessions.len() >= action_authority::MAX_LAUNCH_SESSIONS {
                return error(StatusCode::TOO_MANY_REQUESTS, "too many launch sessions");
            }
            sessions.insert(
                session.clone(),
                action_authority::LaunchSession::new(&entry),
            );
        }
        (
            StatusCode::OK,
            [(header::CACHE_CONTROL, "no-store")],
            axum::Json(json!({"session": session})),
        )
            .into_response()
    }
}

impl crate::server::service::ToolRegistryRoutes for ToolRegistry {
    fn session(
        self: Arc<Self>,
        body: Bytes,
    ) -> crate::server::routes::ProviderFuture<'static, Response> {
        Box::pin(async move { self.exchange_session(&body) })
    }
    fn dashboard(
        self: Arc<Self>,
        request: Request,
        body: Bytes,
    ) -> crate::server::routes::ProviderFuture<'static, Response> {
        Box::pin(async move { ToolRegistry::dashboard(&self, request, body).await })
    }
}
