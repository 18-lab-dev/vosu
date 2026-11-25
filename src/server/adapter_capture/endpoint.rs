//! `GET /__vosu_test/v1/adapter-capture?schema=<v1|sc|v2|v2-precise>`, the
//! test-harness dump endpoint (`tests/fixtures/upstream/GOLDEN_CONTRACT.md`
//! §5). It is compiled only with `test-harness`, which `build.rs` rejects in
//! release builds, and mounted only when the launching runner supplies a
//! per-launch capability in [`CAPABILITY_ENV`].
//!
//! A request from a non-loopback peer, or one without exactly that capability
//! in [`CAPABILITY_HEADER`], is not addressed to the endpoint at all: dispatch
//! continues as if the route did not exist, so it gets the ordinary response
//! for an unknown path. Responses are never CORS-enabled, never cached and
//! never echo an internal error. Each request takes one provider-retained
//! sample: no demand, subscription or history.
use super::{AdapterCaptureSample, schema_from_query};
use crate::server::{routes::ProviderFuture, websocket::Schema};
use axum::{
    body::{Body, Bytes},
    extract::Request,
    http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode, header},
    response::Response,
};
use std::{fmt, io, net::IpAddr, sync::Arc};

pub const PATH: &str = "/__vosu_test/v1/adapter-capture";
pub const CAPABILITY_HEADER: HeaderName = HeaderName::from_static("x-vosu-test-capture");
/// The private per-launch environment entry: 64 lowercase hexadecimal
/// characters (32 random bytes). Read once at composition, never logged.
pub const CAPABILITY_ENV: &str = "VOSU_TEST_ADAPTER_CAPTURE";
/// A larger envelope is refused, never truncated.
pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;

pub const BAD_QUERY_BODY: &[u8] = br#"{"error":"invalid adapter capture query"}"#;
pub const BAD_METHOD_BODY: &[u8] = br#"{"error":"method not allowed"}"#;
pub const TOO_LARGE_BODY: &[u8] = br#"{"error":"adapter capture exceeds 8 MiB"}"#;
pub const FAILED_BODY: &[u8] = br#"{"error":"adapter capture failed"}"#;

/// One provider-retained sample of a schema's provider, i.e.
/// [`crate::server::bus_provider::BusStateProvider::capture_for_harness`].
pub trait CaptureSource: Send + Sync + 'static {
    fn capture(&self, schema: Schema) -> Result<AdapterCaptureSample, String>;
}

/// A per-launch 32-byte capability. Its value is never formatted.
#[derive(Clone)]
pub struct Capability([u8; 32]);
impl fmt::Debug for Capability {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Capability(<redacted>)")
    }
}
impl Capability {
    /// Exactly 64 lowercase hexadecimal characters.
    pub fn parse(text: &[u8]) -> Option<Self> {
        if text.len() != 64 {
            return None;
        }
        let mut bytes = [0; 32];
        for (byte, pair) in bytes.iter_mut().zip(text.chunks_exact(2)) {
            *byte = (nibble(pair[0])? << 4) | nibble(pair[1])?;
        }
        Some(Self(bytes))
    }
    /// `None` when the runner supplied no capability. A malformed value fails
    /// composition rather than silently leaving the endpoint unmounted; the
    /// error never contains the value.
    pub fn from_env() -> io::Result<Option<Self>> {
        let Some(value) = std::env::var_os(CAPABILITY_ENV) else {
            return Ok(None);
        };
        value
            .to_str()
            .and_then(|value| Self::parse(value.as_bytes()))
            .map(Some)
            .ok_or_else(|| {
                io::Error::new(
                    io::ErrorKind::InvalidInput,
                    format!("{CAPABILITY_ENV} must be 64 lowercase hexadecimal characters"),
                )
            })
    }
    /// Exactly one header carrying this capability, compared in constant time.
    fn presented_by(&self, headers: &HeaderMap) -> bool {
        let mut values = headers.get_all(&CAPABILITY_HEADER).iter();
        let (Some(value), None) = (values.next(), values.next()) else {
            return false;
        };
        Self::parse(value.as_bytes()).is_some_and(|presented| {
            presented
                .0
                .iter()
                .zip(self.0)
                .fold(0, |difference, (left, right)| difference | (left ^ right))
                == 0
        })
    }
}
fn nibble(digit: u8) -> Option<u8> {
    match digit {
        b'0'..=b'9' => Some(digit - b'0'),
        b'a'..=b'f' => Some(digit - b'a' + 10),
        _ => None,
    }
}

pub struct AdapterCaptureEndpoint {
    capability: Capability,
    source: Arc<dyn CaptureSource>,
}
impl AdapterCaptureEndpoint {
    pub fn new(capability: Capability, source: Arc<dyn CaptureSource>) -> Self {
        Self { capability, source }
    }
    /// `None` unless `request` targets exactly [`PATH`] from a loopback `peer`
    /// and presents the capability; the caller then routes it as if this
    /// endpoint did not exist. Otherwise: 405 for any method but GET, 400
    /// unless the query is exactly one known `schema`, then one sample.
    pub fn route(
        &self,
        peer: Option<IpAddr>,
        request: &Request,
    ) -> Option<ProviderFuture<'static, Response>> {
        if request.uri().path() != PATH
            || !peer.is_some_and(|peer| peer.to_canonical().is_loopback())
            || !self.capability.presented_by(request.headers())
        {
            return None;
        }
        if request.method() != Method::GET {
            let mut response = fixed(StatusCode::METHOD_NOT_ALLOWED, BAD_METHOD_BODY);
            response
                .headers_mut()
                .insert(header::ALLOW, HeaderValue::from_static("GET"));
            return Some(Box::pin(async move { response }));
        }
        // Raw comparison: no percent-decoding, `+`, repeats or extra pairs.
        let Some(schema) = request
            .uri()
            .query()
            .and_then(|query| query.strip_prefix("schema="))
            .and_then(schema_from_query)
        else {
            return Some(Box::pin(async {
                fixed(StatusCode::BAD_REQUEST, BAD_QUERY_BODY)
            }));
        };
        let source = Arc::clone(&self.source);
        Some(Box::pin(async move {
            // The capture takes provider and host locks; encoding runs after
            // they are released, still off the async workers.
            let envelope =
                tokio::task::spawn_blocking(move || source.capture(schema)?.envelope()).await;
            match envelope {
                Ok(Ok(body)) => respond(body),
                // The internal error may name state or paths: never echo it.
                Ok(Err(_)) | Err(_) => fixed(StatusCode::INTERNAL_SERVER_ERROR, FAILED_BODY),
            }
        }))
    }
}

fn respond(body: Vec<u8>) -> Response {
    if body.len() > MAX_RESPONSE_BYTES {
        return fixed(StatusCode::INSUFFICIENT_STORAGE, TOO_LARGE_BODY);
    }
    json(StatusCode::OK, Bytes::from(body))
}
fn fixed(status: StatusCode, body: &'static [u8]) -> Response {
    json(status, Bytes::from_static(body))
}
fn json(status: StatusCode, body: Bytes) -> Response {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "application/json")
        .header(header::CACHE_CONTROL, "no-store")
        .body(Body::from(body))
        .expect("valid adapter capture response")
}

#[cfg(test)]
mod tests;
