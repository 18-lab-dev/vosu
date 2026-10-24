//! Exact-method HTTP routing and narrow service-provider boundaries.

use std::{
    collections::HashMap,
    future::Future,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    pin::Pin,
    sync::{Arc, Mutex, Weak},
};

use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{
        ConnectInfo, Extension, Request, State, connect_info::IntoMakeServiceWithConnectInfo,
    },
    http::{HeaderName, HeaderValue, Method, StatusCode, header},
    response::Response,
};
use serde_json::Value;

use super::{
    auth::{AuthorizationDecision, AuthorizationPolicy, MutationAuthorization, RequestHeaders},
    websocket::UpgradeCallbackTracker,
};

pub const MAX_POST_BODY: usize = 1024 * 1024;
#[derive(Clone, Copy)]
pub(crate) struct KeyProtocol;
pub type ProviderFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StateRoute {
    V1,
    StreamCompanion,
    V2,
    V2Precise,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Endpoint {
    State(StateRoute),
    CalculatePp,
    CounterSearch,
    CounterDownload,
    CounterOpen,
    CounterOpenRoot,
    CounterDelete,
    CounterDetails,
    /// Dashboard-only nickname/alias update for one installed overlay.
    CounterNames,
    /// Dashboard-only catalog update annotations for installed overlays.
    CounterUpdates,
    DashboardStatus,
    CounterSettings,
    ApiKeys,
    SettingsSave,
    RunUpdates,
    GenerateReport,
    BackgroundImage,
    BeatmapBackground,
    BeatmapAudio,
    BeatmapFile,
    SongsFile,
    BeatmapPath,
    SkinFile,
    Asset,
    Favicon,
    Root,
    Settings,
    Available,
    Tools,
    ToolOverlay,
    ToolManagement,
    CounterStatic,
    WsV1,
    WsTokens,
    WsV2,
    WsV2Precise,
    WsV2Subscribe,
    WsCommands,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CachedJson {
    Ready(Bytes),
    NotReady,
    Failed { status: StatusCode, body: Bytes },
}

pub trait StateJsonProvider: Send + Sync {
    fn cached_json(&self, route: StateRoute) -> ProviderFuture<'_, CachedJson>;
}

#[derive(Clone, Debug, PartialEq)]
pub enum ModsInput {
    Bits(u32),
    Json(Value),
    Acronyms(String),
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct PpQuery {
    pub path: Option<String>,
    pub lazer: Option<bool>,
    pub mode: Option<u8>,
    pub ar: Option<f64>,
    pub cs: Option<f64>,
    pub hp: Option<f64>,
    pub od: Option<f64>,
    pub clock_rate: Option<f64>,
    pub passed_objects: Option<u32>,
    pub combo: Option<u32>,
    pub n_misses: Option<u32>,
    pub n100: Option<u32>,
    pub n300: Option<u32>,
    pub n50: Option<u32>,
    pub n_geki: Option<u32>,
    pub n_katu: Option<u32>,
    pub mods: Option<ModsInput>,
    pub accuracy: Option<f64>,
    pub slider_end_hits: Option<u32>,
    pub small_tick_hits: Option<u32>,
    pub large_tick_hits: Option<u32>,
    pub hitresult_priority: Option<u8>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CalculateResult {
    Ready(Bytes),
    NotReady,
    Failed { status: StatusCode, body: Bytes },
}

pub trait CalculatePpProvider: Send + Sync {
    fn calculate_pp(&self, query: PpQuery) -> ProviderFuture<'_, CalculateResult>;
}

/// Implemented by page, counter, file, report, and WebSocket slices. This module
/// owns only classification, method enforcement, body limiting, and envelopes.
pub trait RouteDelegate: Send + Sync {
    fn api_keys(&self) -> Option<Arc<super::api_keys::ApiKeys>> {
        None
    }
    fn handle(
        &self,
        endpoint: Endpoint,
        request: Request,
        body: Bytes,
    ) -> ProviderFuture<'_, Response>;
}

pub trait RequestAuthorizer: Send + Sync {
    fn origins(&self) -> Option<(&super::origins::Origins, bool)> {
        None
    }
    fn authorize(&self, peer: IpAddr, headers: RequestHeaders<'_>) -> AuthorizationDecision;
}

impl RequestAuthorizer for AuthorizationPolicy {
    fn authorize(&self, peer: IpAddr, headers: RequestHeaders<'_>) -> AuthorizationDecision {
        AuthorizationPolicy::authorize(self, peer, headers)
    }
}

type FlushCallback = Box<dyn FnOnce() + Send + 'static>;

#[derive(Default)]
struct ResponseFlushState {
    callback: Option<FlushCallback>,
    disconnected: bool,
}

#[derive(Default)]
pub struct ResponseFlushBarrier {
    state: Mutex<ResponseFlushState>,
}

impl ResponseFlushBarrier {
    pub fn arm(&self, callback: FlushCallback) {
        let immediate = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if state.disconnected {
                Some(callback)
            } else {
                state.callback = Some(match state.callback.take() {
                    Some(previous) => Box::new(move || {
                        previous();
                        callback();
                    }),
                    None => callback,
                });
                None
            }
        };
        if let Some(callback) = immediate {
            callback();
        }
    }

    pub fn flushed(&self) {
        let callback = self
            .state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .callback
            .take();
        if let Some(callback) = callback {
            callback();
        }
    }

    pub fn disconnect(&self) {
        let callback = {
            let mut state = self
                .state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.disconnected = true;
            state.callback.take()
        };
        if let Some(callback) = callback {
            callback();
        }
    }
}

#[derive(Default)]
pub struct ResponseFlushRegistry {
    entries: Mutex<HashMap<SocketAddr, Weak<ResponseFlushBarrier>>>,
}

impl ResponseFlushRegistry {
    pub fn register(&self, peer: SocketAddr, barrier: &Arc<ResponseFlushBarrier>) {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .insert(peer, Arc::downgrade(barrier));
    }

    pub fn remove(&self, peer: SocketAddr) {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&peer);
    }

    pub fn barrier(&self, peer: SocketAddr) -> Option<Arc<ResponseFlushBarrier>> {
        self.entries
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .get(&peer)
            .and_then(Weak::upgrade)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct PeerAddress(pub SocketAddr);

/// Transport-observed peer and destination, never forwarded or Host-derived.
#[derive(Clone, Copy, Debug)]
pub struct ConnectionAddresses {
    pub peer: SocketAddr,
    pub local: Option<SocketAddr>,
}

#[derive(Clone)]
pub struct RouteProviders {
    pub authorization: Arc<dyn RequestAuthorizer>,
    pub state: Arc<dyn StateJsonProvider>,
    pub calculation: Arc<dyn CalculatePpProvider>,
    pub delegate: Arc<dyn RouteDelegate>,
}

pub fn router(providers: RouteProviders) -> Router {
    // One fallback deliberately sees every method. Axum's `get` helper is not used,
    // so GET never acquires implicit HEAD and OPTIONS is never synthesized.
    Router::new().fallback(dispatch).with_state(providers)
}

/// The production service adapter. Using this instead of a bare `Router` makes
/// the transport peer address available to the authorization gate.
pub fn service(providers: RouteProviders) -> IntoMakeServiceWithConnectInfo<Router, PeerAddress> {
    router(providers).into_make_service_with_connect_info::<PeerAddress>()
}
pub fn service_with_upgrades(
    providers: RouteProviders,
    tracker: Arc<UpgradeCallbackTracker>,
    flushes: Arc<ResponseFlushRegistry>,
) -> IntoMakeServiceWithConnectInfo<Router, ConnectionAddresses> {
    router(providers)
        .layer(Extension(tracker))
        .layer(Extension(flushes))
        .into_make_service_with_connect_info::<ConnectionAddresses>()
}

pub async fn dispatch(State(providers): State<RouteProviders>, mut request: Request) -> Response {
    let addresses = request
        .extensions()
        .get::<ConnectInfo<ConnectionAddresses>>()
        .copied();
    let local_ip = addresses.and_then(|addresses| addresses.0.local.map(|address| address.ip()));
    if let Some(addresses) = addresses {
        request
            .extensions_mut()
            .insert(ConnectInfo(PeerAddress(addresses.0.peer)));
    }
    if let Some((origins, true)) = providers.authorization.origins() {
        let mut hosts = request.headers().get_all(header::HOST).iter();
        if !hosts
            .next()
            .and_then(|value| value.to_str().ok())
            .is_some_and(|host| {
                format!("http://{host}") == origins.safe_origin(true, request.headers(), local_ip)
            })
            || hosts.next().is_some()
        {
            return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from("Not Found"))
                .unwrap();
        }
        if request.headers().contains_key(header::UPGRADE) {
            return Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from("Not Found"))
                .unwrap();
        }
    }
    let allowed = {
        let peer = request
            .extensions()
            .get::<ConnectInfo<PeerAddress>>()
            .map(|value| value.0.0)
            .or_else(|| {
                request
                    .extensions()
                    .get::<ConnectInfo<SocketAddr>>()
                    .map(|value| value.0)
            })
            .or_else(|| {
                request
                    .extensions()
                    .get::<PeerAddress>()
                    .map(|value| value.0)
            });
        let header_text = |name: HeaderName| {
            request
                .headers()
                .get(name)
                .map(|value| value.to_str().unwrap_or("\0"))
        };
        let headers = RequestHeaders {
            origin: header_text(header::ORIGIN),
            referer: header_text(header::REFERER),
        };
        let peer = peer
            .map(|address| address.ip())
            .unwrap_or(IpAddr::V4(Ipv4Addr::UNSPECIFIED));
        let decision = providers.authorization.authorize(peer, headers);
        let dashboard_source = !decision.header_ok
            && providers
                .authorization
                .origins()
                .is_some_and(|(origins, _)| {
                    headers
                        .origin
                        .or(headers.referer)
                        .and_then(|value| url::Url::parse(value).ok())
                        .is_some_and(|source| {
                            source.origin().ascii_serialization()
                                == origins.safe_origin(true, request.headers(), local_ip)
                        })
                });
        let key_route = (request.method() == Method::POST
            && matches!(
                classify(request.method(), request.uri().path()),
                Some(Endpoint::CounterSettings)
            ))
            || request.uri().path() == "/websocket/commands";
        decision.peer_ok
            && (decision.header_ok
                || dashboard_source
                || (request.method() == Method::GET && request.uri().path() == "/api/dashboard")
                || (key_route
                    && (super::api_keys::presented(request.headers(), true)
                        .is_ok_and(|k| k.is_some())
                        || request
                            .headers()
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.split_once(' '))
                            .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer")))))
    };
    if !allowed {
        return cors(
            Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from("Not Found"))
                .unwrap(),
        );
    }

    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let websocket_upgrade = request
        .headers()
        .get_all(header::CONNECTION)
        .iter()
        .any(|value| {
            value.to_str().is_ok_and(|value| {
                value
                    .split(',')
                    .any(|token| token.trim().eq_ignore_ascii_case("upgrade"))
            })
        })
        && request
            .headers()
            .get_all(header::UPGRADE)
            .iter()
            .any(|value| {
                value
                    .to_str()
                    .is_ok_and(|value| value.trim().eq_ignore_ascii_case("websocket"))
            });
    if websocket_upgrade
        && !matches!(
            classify(&method, &path),
            Some(
                Endpoint::WsV1
                    | Endpoint::WsTokens
                    | Endpoint::WsV2
                    | Endpoint::WsV2Precise
                    | Endpoint::WsV2Subscribe
                    | Endpoint::ToolOverlay
                    | Endpoint::WsCommands
            )
        )
    {
        return cors(not_found());
    }
    let Some(endpoint) = classify(&method, &path) else {
        return cors(not_found_for(&request));
    };
    if let Some((origins, dashboard)) = providers.authorization.origins() {
        if method == Method::GET && path == "/api/dashboard" {
            return cors(json(
                StatusCode::OK,
                Bytes::from(serde_json::json!({"origin": origins.dashboard()}).to_string()),
            ));
        }
        if !dashboard
            && matches!(
                endpoint,
                Endpoint::DashboardStatus
                    | Endpoint::CounterDetails
                    | Endpoint::CounterNames
                    | Endpoint::CounterUpdates
                    | Endpoint::ToolManagement
            )
        {
            return not_found_for(&request);
        }
        if !dashboard
            && matches!(
                endpoint,
                Endpoint::Root | Endpoint::Settings | Endpoint::Available | Endpoint::Tools
            )
        {
            let target = format!(
                "{}{}",
                origins.safe_origin(true, request.headers(), local_ip),
                request.uri().path_and_query().map_or("/", |v| v.as_str())
            );
            return Response::builder()
                .status(StatusCode::FOUND)
                .header(header::LOCATION, target)
                .body(Body::empty())
                .unwrap();
        }
        if dashboard
            && matches!(
                endpoint,
                Endpoint::CounterStatic
                    | Endpoint::ToolOverlay
                    | Endpoint::WsCommands
                    | Endpoint::BackgroundImage
                    | Endpoint::BeatmapBackground
                    | Endpoint::BeatmapAudio
                    | Endpoint::BeatmapFile
                    | Endpoint::SongsFile
                    | Endpoint::BeatmapPath
                    | Endpoint::SkinFile
            )
        {
            return not_found_for(&request);
        }
    }
    let keys = providers.delegate.api_keys();
    let credential = if (method == Method::POST && endpoint != Endpoint::ToolOverlay)
        || (websocket_upgrade && endpoint == Endpoint::WsCommands)
    {
        super::api_keys::presented(request.headers(), websocket_upgrade).map(|key| {
            key.or_else(|| {
                (method == Method::POST)
                    .then(|| {
                        request
                            .headers()
                            .get(header::AUTHORIZATION)
                            .and_then(|value| value.to_str().ok())
                            .and_then(|value| value.split_once(' '))
                            .filter(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"))
                            .map(|(_, token)| (token.to_owned(), false))
                    })
                    .flatten()
            })
        })
    } else {
        Ok(None)
    };
    let mut grant = None;
    let mut broker_grant = None;
    let mut key_protocol = false;
    match credential {
        Ok(Some((key, protocol))) => {
            let scope = if endpoint == Endpoint::WsCommands && websocket_upgrade {
                Some(super::api_keys::Scope::SettingsWrite)
            } else if endpoint == Endpoint::CounterSettings && method == Method::POST {
                if path == "/api/counters/profiles" {
                    Some(super::api_keys::Scope::ProfilesManage)
                } else if !url::form_urlencoded::parse(
                    request.uri().query().unwrap_or_default().as_bytes(),
                )
                .any(|(k, v)| k == "update" && v == "yes")
                {
                    Some(super::api_keys::Scope::SettingsWrite)
                } else {
                    None
                }
            } else {
                None
            };
            let broker = request
                .extensions()
                .get::<Option<Arc<super::broker::BrokerToken>>>()
                .and_then(Option::as_ref);
            let peer = request
                .extensions()
                .get::<ConnectInfo<PeerAddress>>()
                .map(|peer| peer.0.0.ip())
                .or_else(|| {
                    request
                        .extensions()
                        .get::<ConnectInfo<SocketAddr>>()
                        .map(|peer| peer.0.ip())
                })
                .or_else(|| {
                    request
                        .extensions()
                        .get::<PeerAddress>()
                        .map(|peer| peer.0.ip())
                });
            if method == Method::POST
                && scope.is_some()
                && providers
                    .authorization
                    .origins()
                    .is_some_and(|(_, dashboard)| !dashboard)
                && let Some(broker) = broker
                && broker.authorize(request.headers(), peer) == Ok(true)
            {
                broker_grant = Some((Arc::clone(broker), peer));
            } else {
                let authenticated = keys
                    .as_ref()
                    .zip(scope)
                    .and_then(|(store, scope)| store.verify(&key, scope).ok());
                let Some(id) = authenticated else {
                    return cors(json(
                        StatusCode::UNAUTHORIZED,
                        error_json("Invalid API key or scope"),
                    ));
                };
                grant = keys
                    .as_ref()
                    .zip(scope)
                    .map(|(store, scope)| store.grant(id, scope));
                key_protocol = protocol;
            }
        }
        Err(()) => {
            return cors(json(
                StatusCode::UNAUTHORIZED,
                error_json("Invalid API key credential"),
            ));
        }
        Ok(None) => {}
    }
    let tool_put = method == Method::PUT && endpoint == Endpoint::ToolManagement;
    let mutation =
        if method == Method::POST || tool_put || websocket_upgrade || endpoint == Endpoint::ApiKeys
        {
            MutationAuthorization::from_headers(request.headers())
        } else {
            MutationAuthorization::default()
        };
    let trusted_management = providers
        .authorization
        .origins()
        .map_or(mutation.0, |(origins, dashboard)| {
            dashboard && origins.authorized(request.headers(), local_ip)
        });
    if ((method == Method::POST
        && endpoint != Endpoint::ToolOverlay
        && grant.is_none()
        && broker_grant.is_none())
        || tool_put
        || endpoint == Endpoint::ApiKeys)
        && !trusted_management
    {
        return cors(
            Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from("Not Found"))
                .unwrap(),
        );
    }
    request.extensions_mut().insert(mutation);
    if let Some(grant) = grant {
        request.extensions_mut().insert(grant);
    }
    if key_protocol {
        request.extensions_mut().insert(KeyProtocol);
    }
    let page_script = providers
        .authorization
        .origins()
        .and_then(|(origins, dashboard)| {
            (dashboard
                && matches!(
                    endpoint,
                    Endpoint::Root | Endpoint::Settings | Endpoint::Available | Endpoint::Tools
                ))
            .then(|| origins.page_context(request.headers(), local_ip))
        });
    if let Some((origins, true)) = providers.authorization.origins()
        && matches!(
            endpoint,
            Endpoint::Root | Endpoint::Available | Endpoint::CounterSearch
        )
    {
        let origin = origins.safe_origin(false, request.headers(), local_ip);
        if let Ok(host) = HeaderValue::from_str(origin.trim_start_matches("http://")) {
            request.headers_mut().insert(header::HOST, host);
        }
    }
    let response = match endpoint {
        Endpoint::State(route) => match providers.state.cached_json(route).await {
            CachedJson::Ready(body) => json(StatusCode::OK, body),
            CachedJson::NotReady => not_ready(),
            CachedJson::Failed { status, body } => json(status, body),
        },
        Endpoint::CalculatePp => match PpQuery::parse(request.uri().query()) {
            Ok(query) => match providers.calculation.calculate_pp(query).await {
                CalculateResult::Ready(body) => json(StatusCode::OK, body),
                CalculateResult::NotReady => not_ready(),
                CalculateResult::Failed { status, body } => json(status, body),
            },
            Err(message) => json(StatusCode::BAD_REQUEST, error_json(&message)),
        },
        endpoint => {
            let (parts, body) = request.into_parts();
            let body = if method == Method::POST || tool_put || endpoint == Endpoint::ToolOverlay {
                match to_bytes(body, MAX_POST_BODY).await {
                    Ok(body) => body,
                    Err(_) => {
                        return cors(json(
                            StatusCode::PAYLOAD_TOO_LARGE,
                            error_json("payload too large"),
                        ));
                    }
                }
            } else {
                Bytes::new()
            };
            if broker_grant
                .as_ref()
                .is_some_and(|(broker, peer)| broker.authorize(&parts.headers, *peer) != Ok(true))
            {
                return cors(json(
                    StatusCode::UNAUTHORIZED,
                    error_json("Broker credential revoked"),
                ));
            }
            if parts
                .extensions
                .get::<super::api_keys::KeyGrant>()
                .is_some_and(|grant| !grant.allowed())
            {
                return cors(json(
                    StatusCode::UNAUTHORIZED,
                    error_json("API key revoked"),
                ));
            }
            providers
                .delegate
                .handle(endpoint, Request::from_parts(parts, Body::empty()), body)
                .await
        }
    };
    let response = if let Some(script) = page_script
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .is_some_and(|v| v.as_bytes().starts_with(b"text/html"))
    {
        let (mut parts, body) = response.into_parts();
        match to_bytes(body, 16 * 1024 * 1024).await {
            Ok(bytes) => {
                parts.headers.remove(header::CONTENT_LENGTH);
                let csp = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' http: https: data:; frame-src http:; connect-src http:; font-src 'self'; base-uri 'none'; object-src 'none'; form-action 'none'; frame-ancestors 'none'";
                parts.headers.insert(
                    header::CONTENT_SECURITY_POLICY,
                    HeaderValue::from_static(csp),
                );
                parts.headers.insert(
                    header::X_CONTENT_TYPE_OPTIONS,
                    HeaderValue::from_static("nosniff"),
                );
                let html = String::from_utf8_lossy(&bytes).replacen(
                    "<head>",
                    &format!("<head>{script}"),
                    1,
                );
                Response::from_parts(parts, Body::from(html))
            }
            Err(_) => not_found(),
        }
    } else {
        response
    };
    if providers
        .authorization
        .origins()
        .is_some_and(|(_, dashboard)| dashboard)
    {
        // Dashboard HTML contains the CSRF secret. Never expose it to overlay
        // fetches via the overlay protocol's permissive read CORS policy.
        response
    } else {
        cors(response)
    }
}

pub fn classify(method: &Method, path: &str) -> Option<Endpoint> {
    use Endpoint::*;
    let get = method == Method::GET;
    let post = method == Method::POST;
    match path {
        "/json" if get => Some(State(StateRoute::V1)),
        "/json/sc" if get => Some(State(StateRoute::StreamCompanion)),
        "/json/v2" if get => Some(State(StateRoute::V2)),
        "/json/v2/precise" if get => Some(State(StateRoute::V2Precise)),
        "/api/calculate/pp" if get => Some(CalculatePp),
        "/api/dashboard" if get => Some(Root),
        "/api/keys" if get || post => Some(ApiKeys),
        "/api/counters/profiles" if get || post => Some(CounterSettings),
        "/api/settingsSave" if post => Some(SettingsSave),
        "/api/runUpdates" if post => Some(RunUpdates),
        "/api/generateReport" if post => Some(GenerateReport),
        "/backgroundImage" if get => Some(BackgroundImage),
        "/files/beatmap/background" if get => Some(BeatmapBackground),
        "/files/beatmap/audio" if get => Some(BeatmapAudio),
        "/files/beatmap/file" if get => Some(BeatmapFile),
        "/favicon.ico" if get => Some(Favicon),
        "/" if get => Some(Root),
        "/settings" if get => Some(Settings),
        "/available" if get => Some(Available),
        "/tools" if get => Some(Tools),
        "/tools" => None,
        _ if path.starts_with("/tools/") => Some(ToolOverlay),
        _ if method == Method::PUT
            && path
                .strip_prefix("/api/tools/")
                .and_then(|p| p.strip_suffix("/autostart"))
                .is_some_and(crate::tools::manifest::valid_id) =>
        {
            Some(ToolManagement)
        }
        _ if (get || post) && (path == "/api/tools" || path.starts_with("/api/tools/")) => {
            Some(ToolManagement)
        }
        "/ws" if get => Some(WsV1),
        "/tokens" if get => Some(WsTokens),
        "/websocket/v2" if get => Some(WsV2),
        "/websocket/v2/precise" if get => Some(WsV2Precise),
        "/websocket/v2/subscribe" if get => Some(WsV2Subscribe),
        "/websocket/commands" if get => Some(WsCommands),
        "/api/dashboard/status" if get => Some(DashboardStatus),
        _ if get && path.starts_with("/api/counters/details/") => Some(CounterDetails),
        "/api/counters/updates" if get => Some(CounterUpdates),
        _ if get && path.starts_with("/api/counters/search/") => Some(CounterSearch),
        "/api/counters/open-root" if post => Some(CounterOpenRoot),
        _ if post && path.starts_with("/api/counters/download/") => Some(CounterDownload),
        _ if post && path.starts_with("/api/counters/open/") => Some(CounterOpen),
        _ if post && path.starts_with("/api/counters/delete/") => Some(CounterDelete),
        _ if post && path.starts_with("/api/counters/names/") => Some(CounterNames),
        _ if (get || post) && path.starts_with("/api/counters/settings/") => Some(CounterSettings),
        _ if get && path.starts_with("/Songs/") => Some(SongsFile),
        _ if get && path.starts_with("/files/beatmap/") => Some(BeatmapPath),
        _ if get && path.starts_with("/files/skin/") => Some(SkinFile),
        _ if get && path.starts_with("/assets/") => Some(Asset),
        _ if path == "/websocket"
            || path.starts_with("/websocket/")
            || path.starts_with("/ws/")
            || path.starts_with("/tokens/") =>
        {
            None
        }
        _ if path.starts_with("/api/")
            || matches!(
                path,
                "/backgroundImage"
                    | "/favicon.ico"
                    | "/ws"
                    | "/tokens"
                    | "/websocket/v2"
                    | "/websocket/v2/precise"
                    | "/websocket/commands"
                    | "/json"
                    | "/json/sc"
                    | "/json/v2"
                    | "/json/v2/precise"
            ) =>
        {
            None
        }
        _ if get => Some(CounterStatic),
        _ => None,
    }
}

impl PpQuery {
    pub fn parse(raw: Option<&str>) -> Result<Self, String> {
        let mut result = Self::default();
        for (key, value) in url::form_urlencoded::parse(raw.unwrap_or_default().as_bytes()) {
            let value = value.into_owned();
            macro_rules! parse {
                ($field:ident, $ty:ty) => {
                    result.$field =
                        Some(value.parse::<$ty>().map_err(|_| format!("invalid {key}"))?)
                };
            }
            macro_rules! count {
                ($field:ident) => {
                    result.$field = Some(parse_js_u32(&value, &key)?)
                };
            }
            match key.as_ref() {
                "path" => result.path = Some(value),
                "lazer" => parse!(lazer, bool),
                "mode" => parse!(mode, u8),
                "ar" => parse!(ar, f64),
                "cs" => parse!(cs, f64),
                "hp" => parse!(hp, f64),
                "od" => parse!(od, f64),
                "clockRate" => parse!(clock_rate, f64),
                "passedObjects" => count!(passed_objects),
                "combo" => count!(combo),
                "nMisses" => count!(n_misses),
                "n100" => count!(n100),
                "n300" => count!(n300),
                "n50" => count!(n50),
                "nGeki" => count!(n_geki),
                "nKatu" => count!(n_katu),
                "acc" => parse!(accuracy, f64),
                "sliderEndHits" => count!(slider_end_hits),
                "smallTickHits" => count!(small_tick_hits),
                "largeTickHits" => count!(large_tick_hits),
                "hitresultPriority" => {
                    result.hitresult_priority = Some(
                        parse_js_u32(&value, &key)?
                            .try_into()
                            .map_err(|_| format!("invalid {key}"))?,
                    )
                }
                "mods" => result.mods = Some(parse_mods(&value)?),
                _ => {}
            }
        }
        Ok(result)
    }
}

fn parse_js_u32(value: &str, key: &str) -> Result<u32, String> {
    let value = value.trim();
    let number = if value.is_empty() {
        0.0
    } else if let Some(digits) = value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        u64::from_str_radix(digits, 16)
            .map(|value| value as f64)
            .map_err(|_| format!("invalid {key}"))?
    } else if let Some(digits) = value
        .strip_prefix("0b")
        .or_else(|| value.strip_prefix("0B"))
    {
        u64::from_str_radix(digits, 2)
            .map(|value| value as f64)
            .map_err(|_| format!("invalid {key}"))?
    } else if let Some(digits) = value
        .strip_prefix("0o")
        .or_else(|| value.strip_prefix("0O"))
    {
        u64::from_str_radix(digits, 8)
            .map(|value| value as f64)
            .map_err(|_| format!("invalid {key}"))?
    } else {
        value.parse::<f64>().map_err(|_| format!("invalid {key}"))?
    };
    if !number.is_finite() || number < 0.0 || number.fract() != 0.0 || number > u32::MAX as f64 {
        return Err(format!("invalid {key}"));
    }
    Ok(number as u32)
}

fn parse_mods(value: &str) -> Result<ModsInput, String> {
    if let Ok(bits) = value.parse::<u32>() {
        return Ok(ModsInput::Bits(bits));
    }
    if matches!(value.trim_start().chars().next(), Some('{' | '[')) {
        let json: Value = serde_json::from_str(value).map_err(|_| "invalid mods".to_owned())?;
        if json.is_object() || json.is_array() {
            return Ok(ModsInput::Json(json));
        }
        return Err("mods JSON must be an object or array".to_owned());
    }
    Ok(ModsInput::Acronyms(value.to_owned()))
}

fn not_found() -> Response {
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from("Not Found"))
        .unwrap()
}

/// An unknown route: the styled 404 page for a person, the plain body for
/// scripts and API clients (see [`super::pages::wants_page`]).
fn not_found_for(request: &Request<Body>) -> Response {
    if !super::pages::wants_page(request.method(), request.headers()) {
        return not_found();
    }
    Response::builder()
        .status(StatusCode::NOT_FOUND)
        .header(header::CONTENT_TYPE, "text/html; charset=utf-8")
        .body(Body::from(Bytes::from_static(super::pages::PAGE_NOT_FOUND)))
        .unwrap()
}
fn not_ready() -> Response {
    json(
        StatusCode::INTERNAL_SERVER_ERROR,
        Bytes::from_static(br#"{"error":"osu is not ready/running"}"#),
    )
}
fn error_json(message: &str) -> Bytes {
    Bytes::from(serde_json::to_vec(&serde_json::json!({"error": message})).unwrap())
}
fn json(status: StatusCode, body: Bytes) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(body))
        .unwrap()
}
fn cors(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("Origin, X-Requested-With, Content-Type, Accept"),
    );
    headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, GET, PUT, DELETE, OPTIONS"),
    );
    headers.insert(
        HeaderName::from_static("access-control-allow-private-network"),
        HeaderValue::from_static("true"),
    );
    response
}

#[cfg(test)]
mod tests;
