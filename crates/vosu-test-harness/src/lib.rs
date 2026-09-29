//! Development-only, in-process network fixture for vosu integration tests.
//!
//! The fixture deliberately binds one ephemeral loopback socket and exposes it only
//! through [`vosu::net::TestNetworkConfig`]. Production destination policy never
//! receives the resolver or allowlist.

#![forbid(unsafe_code)]

use std::{
    collections::HashMap,
    io::{self, Cursor, Write},
    net::{IpAddr, SocketAddr},
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};

use axum::{
    Router,
    body::Body,
    extract::{OriginalUri, State},
    http::{HeaderMap, HeaderValue, Response, StatusCode, header},
    routing::get,
};
use bytes::Bytes;
use parking_lot::Mutex;
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_util::sync::CancellationToken;
use vosu::net::{
    AddressResolver, NetworkError, ResolverFuture, TestNetworkConfig, TestNetworkEndpoints,
};
use zip::write::SimpleFileOptions;

pub const RESERVED_HOST: &str = "vosu-harness.invalid";
// Fixed synthetic client identity, independent of the daemon package version.
pub const USER_AGENT: &str = "vosu/0.2.0 (https://vosu.app)";

/// Fixed endpoint set injected into an in-process application.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HarnessEndpoints {
    pub catalog: String,
    pub counter_zip: String,
    pub counter_redirect: String,
    pub release_latest: String,
    pub offsets_base: String,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RecordedRequest {
    pub method: String,
    pub path_and_query: String,
    pub user_agent: Option<String>,
}

#[derive(Clone)]
struct MockState {
    counter_zip: Bytes,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    active_requests: Arc<AtomicUsize>,
}

struct ActiveRequest(Arc<AtomicUsize>);
impl ActiveRequest {
    fn enter(counter: Arc<AtomicUsize>) -> Self {
        counter.fetch_add(1, Ordering::AcqRel);
        Self(counter)
    }
}
impl Drop for ActiveRequest {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::AcqRel);
    }
}

/// A deterministic HTTP origin covering catalog, counter, update, and offset traffic.
pub struct MockNetwork {
    address: SocketAddr,
    endpoints: HarnessEndpoints,
    resolver: Arc<FixedResolver>,
    requests: Arc<Mutex<Vec<RecordedRequest>>>,
    active_requests: Arc<AtomicUsize>,
    stop: CancellationToken,
    task: Option<JoinHandle<io::Result<()>>>,
}

impl MockNetwork {
    pub async fn start() -> io::Result<Self> {
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0)).await?;
        let address = listener.local_addr()?;
        let base = format!("http://{RESERVED_HOST}:{}", address.port());
        let endpoints = HarnessEndpoints {
            catalog: format!("{base}/api.json"),
            counter_zip: format!("{base}/downloads/example-counter.zip?token=a%26b"),
            counter_redirect: format!("{base}/redirect-counter"),
            release_latest: format!("{base}/repos/shiruviaa/vosu/releases/latest"),
            offsets_base: format!("{base}/offsets"),
        };
        let requests = Arc::new(Mutex::new(Vec::new()));
        let active_requests = Arc::new(AtomicUsize::new(0));
        let state = MockState {
            counter_zip: Bytes::from(counter_archive()?),
            requests: requests.clone(),
            active_requests: active_requests.clone(),
        };
        let router = Router::new()
            .fallback(get(handle_request))
            .with_state(state);
        let stop = CancellationToken::new();
        let shutdown = stop.clone();
        let task = tokio::spawn(async move {
            axum::serve(listener, router)
                .with_graceful_shutdown(shutdown.cancelled_owned())
                .await
        });
        let resolver = Arc::new(FixedResolver::new([(RESERVED_HOST, vec![address.ip()])]));
        Ok(Self {
            address,
            endpoints,
            resolver,
            requests,
            active_requests,
            stop,
            task: Some(task),
        })
    }

    pub fn address(&self) -> SocketAddr {
        self.address
    }
    pub fn endpoints(&self) -> &HarnessEndpoints {
        &self.endpoints
    }

    pub fn test_network_config(&self) -> Result<TestNetworkConfig, NetworkError> {
        TestNetworkConfig::new(
            self.resolver.clone(),
            vec![RESERVED_HOST.to_owned()],
            vec![self.address],
        )?
        .with_endpoints(TestNetworkEndpoints {
            catalog: self.endpoints.catalog.clone(),
            release: self.endpoints.release_latest.clone(),
            offsets_base: self.endpoints.offsets_base.clone(),
        })
    }

    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.requests.lock().clone()
    }

    pub fn active_requests(&self) -> usize {
        self.active_requests.load(Ordering::Acquire)
    }

    pub async fn shutdown(mut self) -> io::Result<()> {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            task.await.map_err(io::Error::other)??;
        }
        if self.active_requests() != 0 {
            return Err(io::Error::other("mock network retained active requests"));
        }
        Ok(())
    }
}

impl Drop for MockNetwork {
    fn drop(&mut self) {
        self.stop.cancel();
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

#[derive(Debug)]
pub struct FixedResolver {
    answers: HashMap<String, Vec<IpAddr>>,
}

impl FixedResolver {
    pub fn new<const N: usize>(answers: [(&str, Vec<IpAddr>); N]) -> Self {
        Self {
            answers: answers
                .into_iter()
                .map(|(host, ips)| (host.trim_end_matches('.').to_ascii_lowercase(), ips))
                .collect(),
        }
    }
}

impl AddressResolver for FixedResolver {
    fn resolve<'a>(&'a self, host: &'a str) -> ResolverFuture<'a> {
        Box::pin(async move {
            self.answers
                .get(&host.trim_end_matches('.').to_ascii_lowercase())
                .cloned()
                .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "unmapped harness hostname"))
        })
    }
}

async fn handle_request(
    State(state): State<MockState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response<Body> {
    let _active = ActiveRequest::enter(state.active_requests.clone());
    state.requests.lock().push(RecordedRequest {
        method: "GET".to_owned(),
        path_and_query: uri
            .path_and_query()
            .map_or_else(|| uri.path().to_owned(), ToString::to_string),
        user_agent: headers
            .get(header::USER_AGENT)
            .and_then(|value| value.to_str().ok())
            .map(str::to_owned),
    });

    match uri.path() {
        "/api.json" => json(
            StatusCode::OK,
            r#"[{"name":"Harness Counter","author":"CI","version":"1.0","folderName":"harness-counter","resolution":[640,480],"authorlinks":[],"settings":[]}]"#,
        ),
        "/downloads/example-counter.zip" => {
            response(StatusCode::OK, "application/zip", state.counter_zip.clone())
        }
        "/redirect-counter" => Response::builder()
            .status(StatusCode::FOUND)
            .header(
                header::LOCATION,
                "/downloads/example-counter.zip?token=a%26b",
            )
            .body(Body::empty())
            .expect("valid redirect response"),
        "/repos/shiruviaa/vosu/releases/latest" => json(
            StatusCode::OK,
            r#"{"tag_name":"v0.1.0","draft":false,"prerelease":false,"assets":[]}"#,
        ),
        "/offsets/2026.714.0.json" => json(
            StatusCode::OK,
            r#"{"OsuVersion":"2026.714.0","RulesetStore":1}"#,
        ),
        "/status/503" => response(
            StatusCode::SERVICE_UNAVAILABLE,
            "text/plain",
            Bytes::from_static(b"must not be buffered"),
        ),
        "/slow" => {
            tokio::time::sleep(std::time::Duration::from_secs(2)).await;
            response(StatusCode::OK, "text/plain", Bytes::from_static(b"late"))
        }
        _ => response(StatusCode::NOT_FOUND, "text/plain", Bytes::new()),
    }
}

fn json(status: StatusCode, body: &'static str) -> Response<Body> {
    response(
        status,
        "application/json",
        Bytes::from_static(body.as_bytes()),
    )
}

fn response(status: StatusCode, content_type: &'static str, body: Bytes) -> Response<Body> {
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(header::CONTENT_TYPE, HeaderValue::from_static(content_type));
    response
}

fn counter_archive() -> io::Result<Vec<u8>> {
    let cursor = Cursor::new(Vec::new());
    let mut archive = zip::ZipWriter::new(cursor);
    let options = SimpleFileOptions::default();
    archive.start_file("index.html", options)?;
    archive.write_all(b"<!doctype html><title>Harness Counter</title>")?;
    archive.start_file("metadata.txt", options)?;
    archive.write_all(b"Name: Harness Counter\nAuthor: CI\nVersion: 1.0\n640x480\n")?;
    archive.start_file("assets/value.txt", options)?;
    archive.write_all(b"downloaded through exact-listener policy")?;
    Ok(archive.finish()?.into_inner())
}
