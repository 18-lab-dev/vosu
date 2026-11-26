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

use super::{
    auth::{AuthorizationDecision, AuthorizationPolicy, MutationAuthorization, RequestHeaders},
    websocket::UpgradeCallbackTracker,
};
use crate::removed_routes::RemovedFeature;

mod responses;
use responses::*;

pub const MAX_POST_BODY: usize = 1024 * 1024;
/// Core replaces the first occurrence in a dashboard page shell with the
/// page-context script (A7.1, `docs/v2/plugin-abi.md` §7.5).
pub const PAGE_CONTEXT_MARKER: &str = "<!--vosu-context-->";
#[derive(Clone, Copy)]
pub struct KeyProtocol;
/// Transport admission a bundled-host dispatch needs: the listener the
/// request arrived on and, only for a dashboard request that passed exact
/// dashboard provenance and `X-Vosu-CSRF`, the expected token core's
/// `ManagementAuthority` re-checks when it mints the provider's witness.
#[derive(Clone)]
pub struct SurfaceAdmission {
    pub listener: vosu_surface::listener::ListenerRole,
    pub csrf: Option<Arc<str>>,
}
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
    ApiV1,
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
    /// Dashboard-only `GET /api/settings` (`SettingsView`, A7.1).
    SettingsView,
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
    /// `POST /tools/_host/v1/session`: core's launch-session exchange (A7.2).
    ToolSession,
    /// `GET`/`HEAD /tools/...`: the bundled tool host's exact registrations.
    ToolHost,
    /// Core's exact `/api/tools` management routes (A7.2).
    ToolManagement,
    /// Dashboard-only process-plugin approval and management
    /// (catalog-v2.md A5.5 "Management routes").
    PluginManagement,
    /// Dashboard-only `/api/catalog/*` (M6b H0); exact paths in `catalog_api`.
    Catalog,
    CounterStatic,
    WsV1,
    WsTokens,
    WsV2,
    WsV2Precise,
    Removed(RemovedFeature),
    WsCommands,
}

#[derive(Clone, Debug, PartialEq)]
pub enum CachedJson {
    Ready(Bytes),
    NotReady,
    Failed { status: StatusCode, body: Bytes },
}
/// Production body of every state or calculation read before a client is ready.
pub const NOT_READY_BODY: &[u8] = br#"{"error":"osu is not ready/running"}"#;
impl CachedJson {
    /// The status and body every production state route serves for this result.
    pub fn status_body(self) -> (StatusCode, Bytes) {
        match self {
            Self::Ready(body) => (StatusCode::OK, body),
            Self::NotReady => (
                StatusCode::INTERNAL_SERVER_ERROR,
                Bytes::from_static(NOT_READY_BODY),
            ),
            Self::Failed { status, body } => (status, body),
        }
    }
}

pub trait StateJsonProvider: Send + Sync {
    fn cached_json(&self, route: StateRoute) -> ProviderFuture<'_, CachedJson>;
}

/// Test-harness loopback endpoint routed before classification (adapter capture).
#[cfg(feature = "test-harness")]
pub trait HarnessRoute: Send + Sync {
    fn route(
        &self,
        peer: Option<IpAddr>,
        request: &Request,
    ) -> Option<ProviderFuture<'static, Response>>;
}

/// Implemented by page, counter, file, report, and WebSocket slices. This module
/// owns only classification, method enforcement, body limiting, and envelopes.
pub trait RouteDelegate: Send + Sync {
    fn registered_http<'a>(
        &'a self,
        _request: &'a Request,
    ) -> Option<ProviderFuture<'a, Response>> {
        None
    }
    fn api_keys(&self) -> Option<Arc<super::api_keys::ApiKeys>> {
        None
    }
    /// The test-harness adapter capture endpoint, mounted only when this launch
    /// received its capability.
    #[cfg(feature = "test-harness")]
    fn adapter_capture(&self) -> Option<&dyn HarnessRoute> {
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

/// The listener's providers. State and calculation routes have no fallback:
/// they are bundled adapters' surface registrations, reached through
/// [`RouteDelegate::registered_http`].
#[derive(Clone)]
pub struct RouteProviders {
    pub authorization: Arc<dyn RequestAuthorizer>,
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
    let api_v1 = request.uri().path() == "/api/v1" || request.uri().path().starts_with("/api/v1/");
    // API routes have independent methods, credentials and JSON envelopes.
    // They still pass through the same transport peer and Origin gate below.
    if api_v1
        && providers
            .authorization
            .origins()
            .is_some_and(|(_, dashboard)| dashboard)
    {
        return super::api_v1::error::error(
            StatusCode::NOT_FOUND,
            "invalid_request",
            "Unknown API route",
        );
    }
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
        let peer = request_peer(&request);
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
                    origins.dashboard_enabled()
                        && headers
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
        if api_v1 {
            return cors(super::api_v1::error::error(
                StatusCode::FORBIDDEN,
                "forbidden",
                "Peer or Origin forbidden",
            ));
        }
        return cors(
            Response::builder()
                .status(StatusCode::FORBIDDEN)
                .body(Body::from("Not Found"))
                .unwrap(),
        );
    }
    if api_v1 {
        return api_v1_cors(
            providers
                .delegate
                .handle(Endpoint::ApiV1, request, Bytes::new())
                .await,
        );
    }

    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    if let Some(endpoint @ Endpoint::Removed(_)) = classify(&method, &path) {
        // A removed route answers exactly like an unknown one (A4.5, A7.5).
        // Only the overlay listener's tombstones are recorded; on the
        // dashboard listener the path is simply unknown.
        let response = cors(not_found_for(&request));
        if !providers
            .authorization
            .origins()
            .is_some_and(|(_, dashboard)| dashboard)
        {
            let _ = providers
                .delegate
                .handle(endpoint, request, Bytes::new())
                .await;
        }
        return response;
    }
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
                    | Endpoint::WsCommands
            )
        )
    {
        return cors(not_found());
    }
    // Only a loopback caller holding this launch's capability reaches the
    // harness endpoint; anyone else falls through to ordinary routing. The
    // dashboard listener never serves it, and it is never CORS-enabled.
    #[cfg(feature = "test-harness")]
    if !providers
        .authorization
        .origins()
        .is_some_and(|(_, dashboard)| dashboard)
        && let Some(response) = providers.delegate.adapter_capture().and_then(|endpoint| {
            endpoint.route(request_peer(&request).map(|peer| peer.ip()), &request)
        })
    {
        return response.await;
    }
    let Some(endpoint) = classify(&method, &path) else {
        return cors(not_found_for(&request));
    };
    if let Some((origins, dashboard)) = providers.authorization.origins() {
        // A7.3: with the dashboard off, nothing serves its origin, so there is
        // nothing to discover and nowhere to redirect.
        if method == Method::GET && path == "/api/dashboard" {
            if !origins.dashboard_enabled() {
                return cors(not_found_for(&request));
            }
            return cors(json(
                StatusCode::OK,
                Bytes::from(serde_json::json!({"origin": origins.dashboard()}).to_string()),
            ));
        }
        if !dashboard
            && matches!(
                endpoint,
                Endpoint::DashboardStatus
                    | Endpoint::CounterSearch
                    | Endpoint::SettingsView
                    | Endpoint::CounterDetails
                    | Endpoint::CounterNames
                    | Endpoint::CounterUpdates
                    | Endpoint::ToolManagement
                    | Endpoint::PluginManagement
                    | Endpoint::Catalog
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
            if !origins.dashboard_enabled() {
                return not_found_for(&request);
            }
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
                    | Endpoint::ToolSession
                    | Endpoint::ToolHost
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
    } else if endpoint == Endpoint::PluginManagement {
        // Plugin management exists only on the dashboard listener.
        return cors(not_found_for(&request));
    }
    let keys = providers.delegate.api_keys();
    let credential = if (method == Method::POST && endpoint != Endpoint::ToolSession)
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
            let peer = request_peer(&request).map(|peer| peer.ip());
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
    // Every DELETE (catalog and plugin management) is a mutation like POST.
    let delete = method == Method::DELETE;
    let mutation =
        if method == Method::POST || delete || websocket_upgrade || endpoint == Endpoint::ApiKeys {
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
        && endpoint != Endpoint::ToolSession
        && grant.is_none()
        && broker_grant.is_none())
        || delete
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
    if let Some((origins, dashboard)) = providers.authorization.origins() {
        request.extensions_mut().insert(SurfaceAdmission {
            listener: if dashboard {
                vosu_surface::listener::ListenerRole::Dashboard
            } else {
                vosu_surface::listener::ListenerRole::Overlay
            },
            csrf: (dashboard && trusted_management).then(|| Arc::from(origins.csrf.as_str())),
        });
    }
    // Core-owned page context for the dashboard host's page shells (A7.1).
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
    // An unregistered state or calculation path has no owner at all.
    if matches!(endpoint, Endpoint::State(_) | Endpoint::CalculatePp) {
        return cors(match providers.delegate.registered_http(&request) {
            Some(response) => response.await,
            None => not_found_for(&request),
        });
    }
    let response = {
        let (parts, body) = request.into_parts();
        let body = if method == Method::POST || delete {
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
                // Only the first marker, and only when the shell carries one:
                // the provider never sees the CSRF secret (A7.1).
                let html =
                    String::from_utf8_lossy(&bytes).replacen(PAGE_CONTEXT_MARKER, &script, 1);
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

/// The transport peer, never forwarded or Host-derived.
fn request_peer(request: &Request) -> Option<SocketAddr> {
    let extensions = request.extensions();
    extensions
        .get::<ConnectInfo<PeerAddress>>()
        .map(|value| value.0.0)
        .or_else(|| {
            extensions
                .get::<ConnectInfo<SocketAddr>>()
                .map(|value| value.0)
        })
        .or_else(|| extensions.get::<PeerAddress>().map(|value| value.0))
}

pub fn classify(method: &Method, path: &str) -> Option<Endpoint> {
    use Endpoint::*;
    if path == "/api/v1" || path.starts_with("/api/v1/") {
        return Some(ApiV1);
    }
    let get = method == Method::GET;
    let file_read = get || method == Method::HEAD;
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
        "/api/settings" if get => Some(SettingsView),
        "/api/runUpdates" if post => Some(RunUpdates),
        "/api/generateReport" if post => Some(GenerateReport),
        "/backgroundImage" if get => Some(BackgroundImage),
        "/files/beatmap/background" if file_read => Some(BeatmapBackground),
        "/files/beatmap/audio" if file_read => Some(BeatmapAudio),
        "/files/beatmap/file" if file_read => Some(BeatmapFile),
        "/favicon.ico" if get => Some(Favicon),
        "/" if get => Some(Root),
        "/settings" if get => Some(Settings),
        "/available" if get => Some(Available),
        "/tools" if get => Some(Tools),
        "/tools" => None,
        "/tools/_host/v1/session" if post => Some(ToolSession),
        // Every other `/tools/` method, upgrade and path is 404 (A7.2).
        _ if path.starts_with("/tools/") => file_read.then_some(ToolHost),
        _ if tool_management(method, path) => Some(ToolManagement),
        "/api/plugins" if get => Some(PluginManagement),
        _ if plugin_management_action(method, path) => Some(PluginManagement),
        _ if (get || post || method == Method::DELETE) && path.starts_with("/api/catalog/") => {
            Some(Catalog)
        }
        "/ws" if get => Some(WsV1),
        "/tokens" if get => Some(WsTokens),
        "/websocket/v2" if get => Some(WsV2),
        "/websocket/v2/precise" if get => Some(WsV2Precise),
        "/websocket/v2/subscribe" => Some(Removed(RemovedFeature::SelectedV2Feed)),
        "/files/skin/" => Some(Removed(RemovedFeature::LegacySkinIndex)),
        "/api/ingame" => Some(Removed(RemovedFeature::IngamePage)),
        "/files/skin" => None,
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
        _ if file_read && path.starts_with("/files/beatmap/") => Some(BeatmapPath),
        _ if file_read && path.starts_with("/files/skin/") => Some(SkinFile),
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

/// `POST /api/plugins/<id>/{approval,restart,stop}`,
/// `DELETE /api/plugins/<id>/approval` and `GET`/`POST
/// /api/plugins/<id>/settings` (A7.1), with one non-empty `<id>` segment.
fn plugin_management_action(method: &Method, path: &str) -> bool {
    let Some((id, action)) = path
        .strip_prefix("/api/plugins/")
        .and_then(|rest| rest.split_once('/'))
    else {
        return false;
    };
    !id.is_empty()
        && match action {
            "approval" => method == Method::POST || method == Method::DELETE,
            "restart" | "stop" => method == Method::POST,
            "settings" => method == Method::GET || method == Method::POST,
            _ => false,
        }
}

/// `GET /api/tools`, `GET /api/tools/<id>[/settings]` and
/// `POST /api/tools/<id>/{enable,disable,trust,launch,settings}` (A7.2);
/// the deleted service, log, data and autostart routes are 404.
fn tool_management(method: &Method, path: &str) -> bool {
    if path == "/api/tools" {
        return method == Method::GET;
    }
    let Some(rest) = path.strip_prefix("/api/tools/") else {
        return false;
    };
    let (id, action) = match rest.split_once('/') {
        Some((id, action)) => (id, Some(action)),
        None => (rest, None),
    };
    super::ids::valid_id(id)
        && match action {
            None => method == Method::GET,
            Some("settings") => method == Method::GET || method == Method::POST,
            Some("enable" | "disable" | "trust" | "launch") => method == Method::POST,
            Some(_) => false,
        }
}

#[cfg(test)]
mod plugin_tests;
#[cfg(test)]
mod tests;
