//! TLS and DNS recorders for the D15 fixture.
//!
//! The TLS recorder is the forge's HTTPS front end: it terminates TLS with the
//! fixture leaf for the exact SNI name, records every ClientHello and request
//! it receives, and forwards forge-host requests to the real Forgejo upstream.
//! Credentials are classified against the known sentinels at receipt and only
//! the classification is kept, so recorder output is redacted by construction.

use std::{
    io,
    net::{IpAddr, Ipv4Addr, SocketAddr},
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{HeaderMap, HeaderValue, StatusCode, header},
    response::Response,
};
use base64::Engine as _;
use rustls::{
    ServerConfig,
    pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject},
    server::Acceptor,
};
use tokio::{net::TcpListener, task::JoinHandle};
use tokio_rustls::{LazyConfigAcceptor, server::TlsStream};
use tokio_util::sync::CancellationToken;
use vosu_net::{AddressResolver, ResolverFuture, SystemResolver};

use crate::fixture::{Fixture, Leaf};

/// What the server received in `Authorization`, never the value itself.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Auth {
    None,
    /// Exactly `token <fixture token>` (Gitea/Forgejo scheme).
    Token,
    /// Exactly HTTP Basic for the fixture username and password.
    Basic,
    /// Some other value; it is not retained.
    Other,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Received {
    pub port: u16,
    pub host: String,
    pub path: String,
    pub accept: Option<String>,
    pub auth: Auth,
    pub status: u16,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Handshake {
    pub port: u16,
    pub sni: Option<String>,
    /// `None` on success; otherwise the server-side TLS error text, which
    /// carries the client's alert (e.g. `UnknownCA`, `BadCertificate`).
    pub error: Option<String>,
}

#[derive(Default)]
struct Log {
    received: Vec<Received>,
    handshakes: Vec<Handshake>,
}

/// Expected credential headers, compared at receipt. No Debug.
#[derive(Default)]
pub struct Sentinels {
    token: Option<HeaderValue>,
    basic: Option<HeaderValue>,
}

impl Sentinels {
    pub fn new(token: &str, username: &str, password: &str) -> Self {
        let basic =
            base64::engine::general_purpose::STANDARD.encode(format!("{username}:{password}"));
        Self {
            token: Some(HeaderValue::from_str(&format!("token {token}")).unwrap()),
            basic: Some(HeaderValue::from_str(&format!("Basic {basic}")).unwrap()),
        }
    }

    fn classify(&self, value: Option<&HeaderValue>) -> Auth {
        match value {
            None => Auth::None,
            Some(value) if self.token.as_ref() == Some(value) => Auth::Token,
            Some(value) if self.basic.as_ref() == Some(value) => Auth::Basic,
            Some(_) => Auth::Other,
        }
    }
}

#[derive(Clone)]
pub enum Role {
    /// Forge host: proxy to the real Forgejo. The mirror host answers 200 so a
    /// client that wrongly reached it would be recorded, not fail on TLS.
    Forge { upstream: String },
    /// Same host, other port: `/x1/return/<path>` redirects back to the forge
    /// on its HTTPS port, which closes an A→B→A chain.
    CrossPort { forge_port: u16 },
    /// Loopback OS-store probe: `/x1/ok` answers a fixed body.
    Fixed,
}

struct Route {
    port: u16,
    role: Role,
    forge_host: String,
    mirror_host: String,
    sentinels: Arc<Sentinels>,
    log: Arc<Mutex<Log>>,
    upstream: reqwest::Client,
}

fn load_config(leaf: &Leaf) -> Arc<ServerConfig> {
    let certs = CertificateDer::pem_file_iter(&leaf.cert)
        .and_then(Iterator::collect::<Result<Vec<_>, _>>)
        .expect("fixture leaf certificate");
    let key = PrivateKeyDer::from_pem_file(&leaf.key).expect("fixture leaf key");
    let mut config = ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::aws_lc_rs::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .expect("TLS versions")
    .with_no_client_auth()
    .with_single_cert(certs, key)
    .expect("fixture leaf pair");
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

struct TlsListener {
    tcp: TcpListener,
    port: u16,
    mirror_host: String,
    forge: Arc<ServerConfig>,
    mirror: Arc<ServerConfig>,
    log: Arc<Mutex<Log>>,
}

impl TlsListener {
    async fn handshake(
        &self,
        tcp: tokio::net::TcpStream,
    ) -> Option<TlsStream<tokio::net::TcpStream>> {
        let start = LazyConfigAcceptor::new(Acceptor::default(), tcp)
            .await
            .ok()?;
        let sni = start.client_hello().server_name().map(str::to_owned);
        // Exact SNI selects the mirror leaf; every other name, including the
        // wrong-hostname alias, is offered the forge leaf it does not match.
        let config = if sni.as_deref() == Some(self.mirror_host.as_str()) {
            self.mirror.clone()
        } else {
            self.forge.clone()
        };
        let result = start.into_stream(config).await;
        let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        log.handshakes.push(Handshake {
            port: self.port,
            sni,
            error: result.as_ref().err().map(ToString::to_string),
        });
        result.ok()
    }
}

impl axum::serve::Listener for TlsListener {
    type Io = TlsStream<tokio::net::TcpStream>;
    type Addr = SocketAddr;

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        loop {
            let Ok((tcp, address)) = self.tcp.accept().await else {
                tokio::task::yield_now().await;
                continue;
            };
            let bounded = tokio::time::timeout(Duration::from_secs(5), self.handshake(tcp));
            if let Ok(Some(stream)) = bounded.await {
                return (stream, address);
            }
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        self.tcp.local_addr()
    }
}

async fn handle(State(route): State<Arc<Route>>, request: Request) -> Response {
    let headers = request.headers().clone();
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .map(|value| {
            value
                .rsplit_once(':')
                .map_or(value, |(host, _)| host)
                .to_owned()
        })
        .unwrap_or_default();
    let path = request
        .uri()
        .path_and_query()
        .map(ToString::to_string)
        .unwrap_or_default();
    let response = if request.method() != axum::http::Method::GET {
        status(StatusCode::METHOD_NOT_ALLOWED)
    } else {
        match &route.role {
            Role::Forge { upstream } if host == route.forge_host => {
                proxy(&route.upstream, upstream, &path, &headers).await
            }
            Role::Forge { .. } if host == route.mirror_host => {
                Response::new(Body::from("mirror reached"))
            }
            Role::CrossPort { forge_port } => match path.strip_prefix("/x1/return/") {
                Some(rest) => redirect(&format!(
                    "https://{}{}/{rest}",
                    route.forge_host,
                    if *forge_port == 443 {
                        String::new()
                    } else {
                        format!(":{forge_port}")
                    }
                )),
                None => status(StatusCode::NOT_FOUND),
            },
            Role::Fixed if path == "/x1/ok" => Response::new(Body::from("vosu-x1 os store ok")),
            _ => status(StatusCode::MISDIRECTED_REQUEST),
        }
    };
    route
        .log
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .received
        .push(Received {
            port: route.port,
            host,
            path,
            accept: headers
                .get(header::ACCEPT)
                .and_then(|value| value.to_str().ok())
                .map(str::to_owned),
            auth: route.sentinels.classify(headers.get(header::AUTHORIZATION)),
            status: response.status().as_u16(),
        });
    response
}

fn status(code: StatusCode) -> Response {
    let mut response = Response::new(Body::empty());
    *response.status_mut() = code;
    response
}

fn redirect(location: &str) -> Response {
    let mut response = status(StatusCode::FOUND);
    response
        .headers_mut()
        .insert(header::LOCATION, HeaderValue::from_str(location).unwrap());
    response
}

/// Forwards exactly what the client sent for authorization/negotiation; the
/// upstream's own status, Location and body come back unchanged.
async fn proxy(
    client: &reqwest::Client,
    upstream: &str,
    path: &str,
    headers: &HeaderMap,
) -> Response {
    let mut request = client.get(format!("{upstream}{path}"));
    for name in [header::AUTHORIZATION, header::ACCEPT, header::USER_AGENT] {
        if let Some(value) = headers.get(&name) {
            request = request.header(name, value.clone());
        }
    }
    let Ok(upstream) = request.send().await else {
        return status(StatusCode::BAD_GATEWAY);
    };
    let code = upstream.status();
    let mut copied = HeaderMap::new();
    for name in [header::CONTENT_TYPE, header::LOCATION] {
        if let Some(value) = upstream.headers().get(&name) {
            copied.insert(name, value.clone());
        }
    }
    let Ok(body) = upstream.bytes().await else {
        return status(StatusCode::BAD_GATEWAY);
    };
    let mut response = Response::new(Body::from(body));
    *response.status_mut() = code;
    *response.headers_mut() = copied;
    response
}

pub struct TlsRecorder {
    log: Arc<Mutex<Log>>,
    addresses: Vec<SocketAddr>,
    stop: CancellationToken,
    servers: Vec<JoinHandle<io::Result<()>>>,
}

impl TlsRecorder {
    pub async fn start(
        fixture: &Fixture,
        sentinels: Sentinels,
        listeners: &[(SocketAddr, Role)],
    ) -> Self {
        let log = Arc::new(Mutex::new(Log::default()));
        let sentinels = Arc::new(sentinels);
        let forge = load_config(&fixture.forge_leaf);
        let mirror = load_config(&fixture.mirror_leaf);
        let upstream = reqwest::Client::builder()
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .unwrap();
        let stop = CancellationToken::new();
        let mut addresses = Vec::new();
        let mut servers = Vec::new();
        for (address, role) in listeners {
            let tcp = TcpListener::bind(address).await.unwrap_or_else(|error| {
                panic!("bind fixture listener {address}: {error} (the runner's container provides these ports)")
            });
            let bound = tcp.local_addr().unwrap();
            addresses.push(bound);
            let route = Arc::new(Route {
                port: bound.port(),
                role: role.clone(),
                forge_host: fixture.forge_host.clone(),
                mirror_host: fixture.mirror_host.clone(),
                sentinels: sentinels.clone(),
                log: log.clone(),
                upstream: upstream.clone(),
            });
            let listener = TlsListener {
                tcp,
                port: bound.port(),
                mirror_host: fixture.mirror_host.clone(),
                forge: forge.clone(),
                mirror: mirror.clone(),
                log: log.clone(),
            };
            let app = Router::new().fallback(handle).with_state(route);
            let shutdown = stop.clone();
            servers.push(tokio::spawn(async move {
                axum::serve(listener, app)
                    .with_graceful_shutdown(shutdown.cancelled_owned())
                    .await
            }));
        }
        Self {
            log,
            addresses,
            stop,
            servers,
        }
    }

    pub fn address(&self, index: usize) -> SocketAddr {
        self.addresses[index]
    }

    pub fn received(&self) -> Vec<Received> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .received
            .clone()
    }

    pub fn handshakes(&self) -> Vec<Handshake> {
        self.log
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .handshakes
            .clone()
    }

    /// Discards earlier observations so a test can assert one phase exactly.
    pub fn clear(&self) {
        let mut log = self.log.lock().unwrap_or_else(PoisonError::into_inner);
        log.received.clear();
        log.handshakes.clear();
    }

    pub async fn shutdown(self) {
        self.stop.cancel();
        for server in self.servers {
            tokio::time::timeout(Duration::from_secs(10), server)
                .await
                .expect("recorder drains")
                .expect("recorder task")
                .expect("recorder serve");
        }
    }
}

/// Records every lookup the client's resolver performs and its answers.
pub struct DnsRecorder<R> {
    inner: R,
    lookups: Mutex<Vec<(String, Vec<IpAddr>)>>,
}

impl DnsRecorder<SystemResolver> {
    /// The production system resolver (container DNS), observed.
    pub fn system() -> Self {
        Self::new(SystemResolver)
    }
}

impl<R> DnsRecorder<R> {
    pub fn new(inner: R) -> Self {
        Self {
            inner,
            lookups: Mutex::new(Vec::new()),
        }
    }

    pub fn lookups(&self) -> Vec<(String, Vec<IpAddr>)> {
        self.lookups
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    pub fn hosts(&self) -> Vec<String> {
        self.lookups().into_iter().map(|(host, _)| host).collect()
    }
}

impl<R: AddressResolver> AddressResolver for DnsRecorder<R> {
    fn resolve<'a>(&'a self, host: &'a str) -> ResolverFuture<'a> {
        Box::pin(async move {
            let result = self.inner.resolve(host).await;
            let answers = result.as_ref().map(Clone::clone).unwrap_or_default();
            self.lookups
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push((host.to_owned(), answers));
            result
        })
    }
}

/// Loopback-only name map for the OS-store probe, where no container DNS
/// exists (native Windows). Only the exact fixture names resolve.
pub struct LoopbackNames(pub Vec<String>);

impl AddressResolver for LoopbackNames {
    fn resolve<'a>(&'a self, host: &'a str) -> ResolverFuture<'a> {
        Box::pin(async move {
            if self.0.iter().any(|name| name == host) {
                Ok(vec![IpAddr::V4(Ipv4Addr::LOCALHOST)])
            } else {
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "not a fixture name",
                ))
            }
        })
    }
}

/// Error text including every source, for asserting the TLS failure class
/// (reqwest hides it behind several layers).
fn chain(error: &(dyn std::error::Error + 'static)) -> String {
    let mut text = error.to_string();
    let mut source = error.source();
    while let Some(next) = source {
        text.push_str(": ");
        text.push_str(&next.to_string());
        source = next.source();
    }
    text
}

/// rustls' name-mismatch error (Linux webpki context form or the plain
/// variant the Windows platform verifier maps to).
pub const WRONG_NAME: &[&str] = &["NotValidForName", "not valid for name"];
pub const UNKNOWN_ISSUER: &[&str] = &["UnknownIssuer"];

pub fn assert_certificate_error(
    result: Result<vosu_net::NetworkResponse, vosu_net::NetworkError>,
    expected: &[&str],
) {
    match result {
        Err(error @ vosu_net::NetworkError::Request(_)) => {
            let text = chain(&error);
            assert!(
                expected.iter().any(|needle| text.contains(needle)),
                "expected a certificate rejection ({expected:?}), got: {text}"
            );
        }
        other => panic!("expected a TLS certificate rejection, got {other:?}"),
    }
}
