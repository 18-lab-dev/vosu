//! The overlay host's exact `vosu-surface` registrations (A7 route
//! inventory): the counter API on its listener roles and the
//! `/websocket/commands` feed. Core admits every request first (peer,
//! Origin, API keys, the native broker and dashboard CSRF) and hands this
//! provider only the decoded query, the bounded body and, for an admitted
//! dashboard mutation, core's `ManagementProvenance` witness. Credentials,
//! folder opening and connection framing stay in core.
use crate::{
    CounterError, CounterService, CounterSetting, ReloadableCounterService, SettingInput,
    cards::escape_html, commands::CommandProcessor, presentation::OverlayPresenter,
    qualified_counter, service::percent_decode_once,
};
use bytes::Bytes;
use serde_json::{Value, json};
use std::sync::Arc;
use vosu_surface::{
    listener::ListenerRole,
    requester::BundledRequester,
    routes::{
        ProviderResponse, RegistrationError, RegistrationLease, ResponseBody, ResponseFuture,
        RouteDescriptor, RoutePath, RouteProvider, SurfaceFeed, SurfaceRequest, SurfaceResponse,
    },
};

const JSON: &str = "application/json; charset=utf-8";
const HTML: &str = "text/html; charset=utf-8";
const SEARCH: &str = "/api/counters/search";
const DETAILS: &str = "/api/counters/details";
const UPDATES: &str = "/api/counters/updates";
const NAMES: &str = "/api/counters/names";
const DOWNLOAD: &str = "/api/counters/download";
const DELETE: &str = "/api/counters/delete";
const SETTINGS: &str = "/api/counters/settings";
const PROFILES: &str = "/api/counters/profiles";
/// The overlay-listener command feed (key/broker scope `settings:write`).
pub const COMMANDS: &str = "/websocket/commands";

/// Opens one `/websocket/commands` connection. Core injects it: framing,
/// admission and revocation close stay in core's hub, and every frame is
/// processed by this crate's [`CommandProcessor`] through core's bridge.
pub type CommandFeeds = Arc<dyn Fn() -> SurfaceFeed + Send + Sync>;

pub struct OverlayRoutes {
    counters: Arc<ReloadableCounterService>,
    presenter: OverlayPresenter,
    processor: Arc<CommandProcessor>,
    commands: CommandFeeds,
}

impl OverlayRoutes {
    pub fn new(
        counters: Arc<ReloadableCounterService>,
        presenter: OverlayPresenter,
        processor: Arc<CommandProcessor>,
        commands: CommandFeeds,
    ) -> Self {
        Self {
            counters,
            presenter,
            processor,
            commands,
        }
    }

    /// Every route this host owns, by listener role and exact method. Folder,
    /// query and URL captures are `Children` matchers: a nested overlay folder
    /// may arrive with a literal `/`.
    pub fn descriptors() -> Vec<RouteDescriptor> {
        use ListenerRole::{Dashboard, Overlay};
        let route = |listener, method: &str, path: RoutePath, websocket| RouteDescriptor {
            listener,
            method: method.into(),
            path,
            websocket,
        };
        let mut routes = vec![
            route(Dashboard, "GET", RoutePath::Children(SEARCH.into()), false),
            route(Dashboard, "GET", RoutePath::Children(DETAILS.into()), false),
            route(Dashboard, "GET", RoutePath::Exact(UPDATES.into()), false),
            route(Dashboard, "POST", RoutePath::Children(NAMES.into()), false),
            route(
                Dashboard,
                "POST",
                RoutePath::Children(DOWNLOAD.into()),
                false,
            ),
            route(Dashboard, "POST", RoutePath::Children(DELETE.into()), false),
            route(Overlay, "GET", RoutePath::Exact(COMMANDS.into()), true),
        ];
        for listener in [Overlay, Dashboard] {
            for method in ["GET", "POST"] {
                routes.push(route(
                    listener,
                    method,
                    RoutePath::Children(SETTINGS.into()),
                    false,
                ));
                routes.push(route(
                    listener,
                    method,
                    RoutePath::Exact(PROFILES.into()),
                    false,
                ));
            }
        }
        routes
    }

    /// Registers [`Self::descriptors`] for the bundled `vosu.overlayhost`
    /// requester; dropping the lease unregisters every route and feed.
    pub fn register(
        self,
        requester: &dyn BundledRequester,
    ) -> Result<RegistrationLease, RegistrationError> {
        let provider: Arc<dyn RouteProvider> = Arc::new(self);
        requester.register_routes(
            Self::descriptors()
                .into_iter()
                .map(|route| (route, Arc::clone(&provider)))
                .collect(),
        )
    }

    async fn http(&self, request: SurfaceRequest) -> SurfaceResponse {
        let path = request.path.as_str();
        let post = request.method == "POST";
        if path == UPDATES {
            return match self.presenter.updates().await {
                Ok(updates) => json_response(200, updates),
                Err(error) => json_response(502, json!({ "error": error })),
            };
        }
        if path == PROFILES {
            return self.profiles(&request);
        }
        if let Some(raw) = capture(path, SEARCH) {
            return self.search(raw, &request).await;
        }
        if let Some(raw) = capture(path, DETAILS) {
            return self.details(raw).await;
        }
        if let Some(raw) = capture(path, SETTINGS) {
            return self.settings(raw, &request).await;
        }
        // Every remaining route is a dashboard mutation: core admitted it
        // with dashboard provenance and CSRF, and its witness must be here.
        if !post || request.management.is_none() {
            return forbidden();
        }
        if let Some(raw) = capture(path, DOWNLOAD) {
            return self.download(raw, &request).await;
        }
        if let Some(raw) = capture(path, NAMES) {
            return self.names(raw, &request.body);
        }
        if let Some(raw) = capture(path, DELETE) {
            return self.delete(raw);
        }
        not_found()
    }

    async fn search(&self, raw: &str, request: &SurfaceRequest) -> SurfaceResponse {
        let query = percent_decode_once(raw).unwrap_or_default();
        let Some(external) = search_source(&request.query) else {
            return bytes_response(
                400,
                HTML,
                Bytes::from_static(b"Error: unknown search source"),
            );
        };
        search_response(self.presenter.search(&query, external).await)
    }

    async fn details(&self, raw: &str) -> SurfaceResponse {
        let Ok(name) = percent_decode_once(raw) else {
            return json_response(400, json!({ "error": "invalid folder name" }));
        };
        let counters = Arc::clone(&self.counters);
        match tokio::task::spawn_blocking(move || counters.details(&name)).await {
            Ok(Ok(details)) => json_response(200, details),
            Ok(Err(error)) => json_response(400, json!({ "error": error.to_string() })),
            Err(_) => json_response(500, json!({ "error": "overlay size worker failed" })),
        }
    }

    async fn download(&self, raw: &str, request: &SurfaceRequest) -> SurfaceResponse {
        let parsed = CounterService::parse_download_request(
            raw,
            query_value(&request.query, "name"),
            query_value(&request.query, "update"),
        );
        let result = match parsed {
            Ok(download) => self.counters.download(&download).await,
            Err(error) => Err(error),
        };
        match result {
            Ok(()) => json_response(200, json!({ "status": "Finished" })),
            Err(CounterError::InstallBusy) => {
                json_response(429, json!({ "error": "counter install capacity is full" }))
            }
            Err(error) => json_response(200, json!({ "error": error.to_string() })),
        }
    }

    /// `{"nickname","alias"}`; null or blank clears a value.
    fn names(&self, raw: &str, body: &[u8]) -> SurfaceResponse {
        #[derive(serde::Deserialize)]
        struct Names {
            nickname: Option<String>,
            alias: Option<String>,
        }
        let Some(folder) = percent_decode_once(raw)
            .ok()
            .filter(|name| !name.is_empty())
        else {
            return json_response(400, json!({ "error": "no folder name" }));
        };
        let Ok(names) = serde_json::from_slice::<Names>(body) else {
            return json_response(400, json!({ "error": "invalid JSON" }));
        };
        match self
            .counters
            .set_name(&folder, names.nickname.as_deref(), names.alias.as_deref())
        {
            Ok(saved) => json_response(200, json!(saved)),
            Err(error) => json_response(400, json!({ "error": error })),
        }
    }

    fn delete(&self, raw: &str) -> SurfaceResponse {
        match percent_decode_once(raw)
            .ok()
            .filter(|name| !name.is_empty())
        {
            None => json_response(200, json!({ "error": "no folder name" })),
            Some(name) => match self.counters.delete(&name) {
                Ok(()) => json_response(200, json!({ "status": "deleted" })),
                Err(error) => json_response(200, json!({ "error": error.to_string() })),
            },
        }
    }

    async fn settings(&self, raw: &str, request: &SurfaceRequest) -> SurfaceResponse {
        let overlay = percent_decode_once(raw).unwrap_or_default();
        if overlay.is_empty() {
            return json_response(200, json!({ "error": "no folder name" }));
        }
        let profile = query_value(&request.query, "profile");
        if request.method == "GET" {
            let name = profile.map_or_else(
                || overlay.clone(),
                |profile| qualified_counter(&overlay, profile),
            );
            return match self.counters.get_settings(&name) {
                Ok((settings, values)) => {
                    json_response(200, json!({ "settings": settings, "values": values }))
                }
                Err(error) => error_json(&error),
            };
        }
        if query_value(&request.query, "update") == Some("yes") {
            // Developer settings are a dashboard mutation (`update=yes`).
            if request.management.is_none() {
                return forbidden();
            }
            let name = profile.map_or_else(
                || overlay.clone(),
                |profile| qualified_counter(&overlay, profile),
            );
            return match serde_json::from_slice::<Vec<CounterSetting>>(&request.body) {
                Ok(settings) => {
                    match self
                        .counters
                        .save_developer_settings(&name, settings, Some("yes"))
                    {
                        Ok(_) => json_response(200, json!({ "result": "success" })),
                        Err(error) => error_json(&error),
                    }
                }
                Err(error) => json_response(200, json!({ "error": error.to_string() })),
            };
        }
        let resolved = match self.counters.settings_store().resolve_profile(profile) {
            Ok(profile) => profile,
            Err(error) => return json_response(200, json!({ "error": error.to_string() })),
        };
        let name = qualified_counter(&overlay, &resolved);
        match serde_json::from_slice::<Vec<SettingInput>>(&request.body) {
            Ok(values) => match self
                .counters
                .save_user_settings_debounced(name, values)
                .await
            {
                Ok(saved) => {
                    if saved {
                        self.refresh_profile(&overlay, &resolved);
                    }
                    json_response(200, json!({ "result": "success" }))
                }
                Err(error) => error_json(&error),
            },
            Err(error) => json_response(200, json!({ "error": error.to_string() })),
        }
    }

    fn profiles(&self, request: &SurfaceRequest) -> SurfaceResponse {
        let store = self.counters.settings_store();
        let result = (|| -> Result<crate::Profiles, String> {
            if request.method == "GET" {
                return store.profiles().map_err(|e| e.to_string());
            }
            let value: Value = serde_json::from_slice(&request.body).map_err(|e| e.to_string())?;
            let field = |key| value.get(key).and_then(Value::as_str);
            let action = field("action").ok_or("missing action")?;
            let name = field("name").ok_or("missing name")?;
            let before = store.profiles().map_err(|e| e.to_string())?;
            let registry = if action == "copyOverlay" {
                store
                    .copy_overlay(
                        name,
                        field("source").ok_or("missing source")?,
                        field("target").ok_or("missing target")?,
                    )
                    .map_err(|e| e.to_string())?;
                self.refresh_profile(name, field("target").unwrap_or_default());
                store.profiles().map_err(|e| e.to_string())?
            } else {
                store
                    .manage_profile(
                        action,
                        name,
                        field(if action == "create" {
                            "copyFrom"
                        } else {
                            "target"
                        }),
                    )
                    .map_err(|e| e.to_string())?
            };
            if before.active != registry.active {
                self.processor.refresh_active_settings();
            }
            Ok(registry)
        })();
        match result {
            Ok(registry) => json_response(200, json!(registry)),
            Err(error) => json_response(200, json!({ "error": error })),
        }
    }

    fn refresh_profile(&self, overlay: &str, profile: &str) {
        self.processor
            .refresh_settings(qualified_counter(overlay, profile));
        if self
            .counters
            .settings_store()
            .resolve_profile(None)
            .is_ok_and(|active| active == profile)
        {
            self.processor.refresh_settings(overlay);
        }
    }
}

impl RouteProvider for OverlayRoutes {
    fn respond(&self, request: SurfaceRequest) -> ResponseFuture<'_, ProviderResponse> {
        Box::pin(async move {
            if request.path == COMMANDS {
                ProviderResponse::Feed((self.commands)())
            } else {
                ProviderResponse::Http(self.http(request).await)
            }
        })
    }
}

/// The still-encoded remainder after `base/`, which may be empty.
fn capture<'a>(path: &'a str, base: &str) -> Option<&'a str> {
    path.strip_prefix(base)?.strip_prefix('/')
}

fn query_value<'a>(pairs: &'a [(String, String)], key: &str) -> Option<&'a str> {
    pairs
        .iter()
        .find_map(|(candidate, value)| (candidate == key).then_some(value.as_str()))
}

/// Search mode is explicit: `source=available` searches the catalog and an
/// absent source (or `installed`) searches local overlays. Any other value is
/// rejected rather than silently falling back to one of the lists.
fn search_source(query: &[(String, String)]) -> Option<bool> {
    match query_value(query, "source") {
        None | Some("installed") => Some(false),
        Some("available") => Some(true),
        Some(_) => None,
    }
}

fn search_response(result: Result<Bytes, String>) -> SurfaceResponse {
    match result {
        Ok(body) => bytes_response(200, HTML, body),
        Err(error) => bytes_response(
            502,
            HTML,
            Bytes::from(format!("Error: {}", escape_html(&error))),
        ),
    }
}

fn error_json(error: &CounterError) -> SurfaceResponse {
    json_response(200, json!({ "error": error.to_string() }))
}

fn bytes_response(status: u16, content_type: &str, body: Bytes) -> SurfaceResponse {
    SurfaceResponse {
        status,
        headers: vec![("content-type".into(), content_type.into())],
        body: ResponseBody::Bytes(body),
    }
}

fn json_response(status: u16, value: Value) -> SurfaceResponse {
    bytes_response(
        status,
        JSON,
        Bytes::from(serde_json::to_vec(&value).expect("JSON value serialization cannot fail")),
    )
}

fn forbidden() -> SurfaceResponse {
    bytes_response(
        403,
        "text/plain; charset=utf-8",
        Bytes::from_static(b"Not Found"),
    )
}

fn not_found() -> SurfaceResponse {
    bytes_response(
        404,
        "text/plain; charset=utf-8",
        Bytes::from_static(b"Not Found"),
    )
}

#[cfg(test)]
mod tests;
