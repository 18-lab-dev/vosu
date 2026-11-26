//! M6b H0: the dashboard-only `/api/catalog/*` facade over core's composed
//! catalog services (`docs/v2/catalog-v2.md` "API placement").
//!
//! Authority stays in `routes::dispatch`: the routes are classified only on
//! GET/POST/DELETE, answer 404 on the overlay listener, and every POST and
//! DELETE has passed same-origin dashboard provenance plus `X-Vosu-CSRF`
//! before this module runs. API keys, broker/tool tokens and overlay
//! origins never substitute. This module adds exact paths and methods
//! (near misses are 404, a known path with another method is 405), decodes
//! each `<source>`/`<plan_id>`/`<job_id>`/`<offer_id>` segment exactly once
//! (strict `%XX`, UTF-8, never a raw `/`), refuses unknown body fields, and
//! answers only redacted fixed diagnostics with `Cache-Control: no-store`.
//! No response, error or status carries a credential, masked preview or
//! upstream URL/body.
//!
//! Every source/credential mutation goes through `CatalogRuntime`, so it
//! takes P0's operation gate, invalidates plans/fetches by generation,
//! applies T0 revocation and refreshes G0 offers.
mod art;
mod installed;
mod network;
mod plans;
mod sources;
#[cfg(test)]
mod tests;

#[cfg(feature = "test-harness")]
pub use network::harness_transport;
pub use network::{CatalogNetwork, network};

use super::routes::ProviderFuture;
use crate::catalog::{
    credentials::CredentialError,
    install::InstallError,
    installed::{UninstallError, UninstallService},
    plans::PlanError,
    resolver::BuiltinProviders,
    runtime::{CatalogError, CatalogRuntime},
    sources::{CandidateSnapshot, SnapshotError, SourceStoreError},
};
use axum::{
    body::{Body, Bytes},
    http::{HeaderValue, Method, Request, StatusCode, header},
    response::Response,
};
use parking_lot::Mutex;
use serde::{Serialize, de::DeserializeOwned};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// Everything the facade needs from the root composition.
pub struct CatalogServices {
    pub runtime: Arc<CatalogRuntime>,
    /// U0, sharing the executor's `InstalledStore` (absent while installed
    /// records are unavailable).
    pub uninstall: Option<Arc<UninstallService>>,
    pub builtins: Arc<BuiltinProviders>,
    /// Removed plugins whose loaded code runs until restart.
    pub restart_required: Arc<dyn Fn() -> Vec<String> + Send + Sync>,
    /// Current checked `MAX_PACKAGE_MIB` byte cap.
    pub package_limit: Arc<dyn Fn() -> Option<u64> + Send + Sync>,
    pub network: Arc<dyn CatalogNetwork>,
    /// Cancelled at daemon shutdown.
    pub cancel: CancellationToken,
}

#[derive(Clone)]
pub struct CatalogApi {
    inner: Arc<Inner>,
}

struct Inner {
    services: CatalogServices,
    /// Identity of the candidate snapshot plans and art are pinned to: a new
    /// publication (even at an unchanged source generation) gets a new epoch.
    /// Holding the `Arc` keeps its address from being reused while cached.
    epoch: Mutex<(Option<Arc<CandidateSnapshot>>, u64)>,
    art: Mutex<art::ArtCache>,
}

impl Inner {
    fn runtime(&self) -> &CatalogRuntime {
        &self.services.runtime
    }
    fn epoch(&self, snapshot: &Arc<CandidateSnapshot>) -> u64 {
        let mut epoch = self.epoch.lock();
        if !epoch
            .0
            .as_ref()
            .is_some_and(|cached| Arc::ptr_eq(cached, snapshot))
        {
            epoch.0 = Some(Arc::clone(snapshot));
            epoch.1 += 1;
        }
        epoch.1
    }
}

fn mark_fixture(mut view: serde_json::Value, fixture: bool) -> serde_json::Value {
    if fixture {
        view["fixture"] = true.into();
    }
    view
}

/// One exact route; segment payloads are already decoded once.
#[derive(Debug, PartialEq, Eq)]
enum Route {
    Status,
    Sources,
    Credential(String),
    Trust(String),
    PrivateNetwork(String),
    Refresh,
    Entries,
    Install,
    Confirm(String),
    Job(String),
    Installed,
    UninstallPreview,
    Uninstall,
    Promote,
    Offers,
    AcceptOffer(String),
    Art,
}

const GET: &[Method] = &[Method::GET];
const POST: &[Method] = &[Method::POST];

/// Exact path grammar. `None` is 404: unknown path, empty or malformed
/// segment, or a raw `/` inside what must be one encoded segment.
fn route(path: &str) -> Option<(Route, &'static [Method])> {
    let rest = path.strip_prefix("/api/catalog/")?;
    let segments: Vec<&str> = rest.split('/').collect();
    Some(match segments.as_slice() {
        ["status"] => (Route::Status, GET),
        ["sources"] => (Route::Sources, &[Method::GET, Method::POST]),
        ["sources", source, "credential"] => (
            Route::Credential(decode_segment(source)?),
            &[Method::GET, Method::POST, Method::DELETE],
        ),
        ["sources", source, "trust"] => (Route::Trust(decode_segment(source)?), POST),
        ["sources", source, "private-network"] => {
            (Route::PrivateNetwork(decode_segment(source)?), POST)
        }
        ["refresh"] => (Route::Refresh, POST),
        ["entries"] => (Route::Entries, GET),
        ["install"] => (Route::Install, POST),
        ["plans", plan, "confirm"] => (Route::Confirm(decode_segment(plan)?), POST),
        ["jobs", job] => (Route::Job(decode_segment(job)?), GET),
        ["installed"] => (Route::Installed, GET),
        ["uninstall", "preview"] => (Route::UninstallPreview, POST),
        ["uninstall"] => (Route::Uninstall, POST),
        ["promote"] => (Route::Promote, POST),
        ["offers"] => (Route::Offers, GET),
        ["offers", offer, "accept"] => (Route::AcceptOffer(decode_segment(offer)?), POST),
        ["art"] => (Route::Art, GET),
        _ => return None,
    })
}

/// Decode one path segment exactly once. Every `%` must start two hex
/// digits and the result must be UTF-8; nothing is decoded a second time,
/// so `%252F` stays the literal text `%2F`.
pub fn decode_segment(raw: &str) -> Option<String> {
    let bytes = raw.as_bytes();
    if bytes.is_empty() {
        return None;
    }
    let hex = |byte: u8| char::from(byte).to_digit(16).map(|value| value as u8);
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            let high = hex(*bytes.get(index + 1)?)?;
            let low = hex(*bytes.get(index + 2)?)?;
            decoded.push(high << 4 | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

impl CatalogApi {
    pub fn new(services: CatalogServices) -> Self {
        Self {
            inner: Arc::new(Inner {
                services,
                epoch: Mutex::new((None, 0)),
                art: Mutex::new(art::ArtCache::default()),
            }),
        }
    }

    pub fn handle(&self, request: Request<Body>, body: Bytes) -> ProviderFuture<'static, Response> {
        let inner = Arc::clone(&self.inner);
        Box::pin(async move { dispatch(&inner, request, body).await })
    }
}

async fn dispatch(inner: &Arc<Inner>, request: Request<Body>, body: Bytes) -> Response {
    let Some((route, methods)) = route(request.uri().path()) else {
        return error(StatusCode::NOT_FOUND, "Not Found");
    };
    let method = request.method().clone();
    if !methods.contains(&method) {
        let allow = methods
            .iter()
            .map(Method::as_str)
            .collect::<Vec<_>>()
            .join(", ");
        let mut response = error(StatusCode::METHOD_NOT_ALLOWED, "method not allowed");
        if let Ok(value) = HeaderValue::from_str(&allow) {
            response.headers_mut().insert(header::ALLOW, value);
        }
        return response;
    }
    let result = match route {
        Route::Status => installed::status(inner),
        Route::Sources if method == Method::GET => sources::list(inner),
        Route::Sources => sources::replace(inner, &body).await,
        Route::Credential(source) => sources::credential(inner, &method, &source, &body).await,
        Route::Trust(source) => sources::trust(inner, &source, &body).await,
        Route::PrivateNetwork(source) => sources::private_network(inner, &source, &body).await,
        Route::Refresh => sources::refresh(inner).await,
        Route::Entries => sources::entries(inner),
        Route::Install => plans::install(inner, &body).await,
        Route::Confirm(plan) => plans::confirm(inner, &plan, &body),
        Route::Job(job) => plans::job(inner, &job),
        Route::Installed => installed::list(inner),
        Route::UninstallPreview => installed::preview(inner, &body).await,
        Route::Uninstall => installed::uninstall(inner, &body).await,
        Route::Promote => installed::promote(inner, &body).await,
        Route::Offers => Ok(json(
            StatusCode::OK,
            &serde_json::json!({"offers": inner.runtime().offer_views()}),
        )),
        Route::AcceptOffer(offer) => plans::accept_offer(inner, &offer).await,
        Route::Art => return art::serve(inner, request.uri().query()).await,
    };
    result.unwrap_or_else(ApiError::into_response)
}

/// Redacted route failure: a fixed diagnostic, optionally with structured
/// non-secret detail (U0 dependents).
#[derive(Debug)]
pub struct ApiError {
    status: StatusCode,
    message: String,
    detail: Option<serde_json::Value>,
}

impl ApiError {
    fn new(status: StatusCode, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
            detail: None,
        }
    }
    fn disabled() -> Self {
        CatalogError::Disabled.into()
    }
    fn bad_request() -> Self {
        Self::new(StatusCode::BAD_REQUEST, "invalid request body")
    }
    fn into_response(self) -> Response {
        let mut value = serde_json::json!({"error": self.message});
        if let Some(detail) = self.detail {
            value["detail"] = detail;
        }
        json(self.status, &value)
    }
}

fn status(code: u16) -> StatusCode {
    StatusCode::from_u16(code).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

impl From<CatalogError> for ApiError {
    fn from(error: CatalogError) -> Self {
        let code = match &error {
            CatalogError::Disabled => StatusCode::SERVICE_UNAVAILABLE,
            CatalogError::Plan(error) => status(error.status_code()),
            CatalogError::Source(error) => source_status(*error),
            CatalogError::Snapshot(error) => snapshot_status(*error),
            CatalogError::Install(error) => install_status(error),
            CatalogError::Credential(error) => credential_status(*error),
            CatalogError::Worker => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self::new(code, error.to_string())
    }
}
impl From<SnapshotError> for ApiError {
    fn from(error: SnapshotError) -> Self {
        Self::new(snapshot_status(error), error.to_string())
    }
}
impl From<SourceStoreError> for ApiError {
    fn from(error: SourceStoreError) -> Self {
        Self::new(source_status(error), error.to_string())
    }
}
impl From<PlanError> for ApiError {
    fn from(error: PlanError) -> Self {
        Self::new(status(error.status_code()), error.to_string())
    }
}
impl From<UninstallError> for ApiError {
    fn from(error: UninstallError) -> Self {
        let mut api = Self::new(status(error.status_code()), error.to_string());
        if let UninstallError::Required { dependents, .. } = &error {
            api.detail = Some(serde_json::json!({ "dependents": dependents }));
        }
        api
    }
}

fn source_status(error: SourceStoreError) -> StatusCode {
    match error {
        SourceStoreError::Invalid => StatusCode::BAD_REQUEST,
        SourceStoreError::NotFound => StatusCode::NOT_FOUND,
        SourceStoreError::OfficialPolicy => StatusCode::CONFLICT,
        SourceStoreError::Unavailable | SourceStoreError::RecoveryRequired => {
            StatusCode::SERVICE_UNAVAILABLE
        }
    }
}
fn snapshot_status(error: SnapshotError) -> StatusCode {
    match error {
        SnapshotError::Stale => StatusCode::CONFLICT,
        SnapshotError::InvalidResults => StatusCode::INTERNAL_SERVER_ERROR,
        SnapshotError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
    }
}
fn install_status(error: &InstallError) -> StatusCode {
    match error {
        InstallError::RecordsInvalid | InstallError::RecoveryRequired => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    }
}
fn credential_status(error: CredentialError) -> StatusCode {
    match error {
        CredentialError::InvalidInput => StatusCode::BAD_REQUEST,
        CredentialError::UnknownSource => StatusCode::NOT_FOUND,
        CredentialError::Unavailable => StatusCode::SERVICE_UNAVAILABLE,
        CredentialError::Persistence | CredentialError::GenerationExhausted => {
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}

/// Every catalog management response is `no-store`, secret mutations
/// included: nothing here belongs in a browser or proxy cache.
fn json(status: StatusCode, value: &impl Serialize) -> Response {
    let body = serde_json::to_vec(value).unwrap_or_else(|_| b"{}".to_vec());
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .expect("valid catalog response")
}

fn error(status: StatusCode, message: &str) -> Response {
    ApiError::new(status, message).into_response()
}

fn no_content() -> Response {
    Response::builder()
        .status(StatusCode::NO_CONTENT)
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::empty())
        .expect("valid catalog response")
}

/// Strict JSON body: unknown fields and malformed input are one fixed 400
/// that never echoes the submitted bytes.
fn parse<T: DeserializeOwned>(body: &[u8]) -> Result<T, ApiError> {
    serde_json::from_slice(body).map_err(|_| ApiError::bad_request())
}

type ApiResult = Result<Response, ApiError>;
