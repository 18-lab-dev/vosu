//! Rust helpers for the vosu tool protocol v1 (`docs/tools/protocol.md`).
//!
//! A service built with this crate reads its launch environment, binds loopback
//! port 0, announces readiness on stdout, rejects requests that lack the
//! per-launch token, and exits when vosu closes its stdin.

use std::{io::Write as _, net::Ipv4Addr, path::PathBuf, sync::Arc, time::Duration};

use axum::{
    Router,
    extract::{Request, State},
    http::StatusCode,
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::get,
};
use serde::{Deserialize, de::DeserializeOwned};
use subtle::ConstantTimeEq as _;
use tokio::io::AsyncReadExt as _;

pub const PROTOCOL: &str = "1";
pub const TOKEN_HEADER: &str = "x-vosu-tool-token";
pub const SESSION_HEADER: &str = "x-vosu-tool-session";
pub const CALLER_HEADER: &str = "x-vosu-caller";

/// Time between stdin EOF and a forced exit, leaving vosu's 5-second kill
/// deadline as a backstop. Graceful shutdown alone can wait forever on
/// long-lived WebSocket connections.
const EXIT_GRACE: Duration = Duration::from_secs(3);

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("missing environment variable {0}; this program must be started by vosu")]
    MissingEnv(&'static str),
    #[error("unsupported VOSU_TOOL_PROTOCOL {0:?}; expected {PROTOCOL}")]
    Protocol(String),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Http(#[from] reqwest::Error),
    #[error("{url} returned HTTP {status}")]
    Status { url: String, status: u16 },
}

/// The launch environment vosu provides to a tool service.
#[derive(Clone, Debug)]
pub struct ToolEnv {
    pub id: String,
    pub token: Arc<str>,
    /// Overlay listener base URL without a trailing slash.
    pub vosu_url: String,
    pub data_dir: PathBuf,
    pub cache_dir: PathBuf,
}

fn var(name: &'static str) -> Result<String, Error> {
    std::env::var(name).map_err(|_| Error::MissingEnv(name))
}

impl ToolEnv {
    pub fn from_env() -> Result<Self, Error> {
        let protocol = var("VOSU_TOOL_PROTOCOL")?;
        if protocol != PROTOCOL {
            return Err(Error::Protocol(protocol));
        }
        Ok(Self {
            id: var("VOSU_TOOL_ID")?,
            token: var("VOSU_TOOL_TOKEN")?.into(),
            vosu_url: var("VOSU_URL")?.trim_end_matches('/').to_owned(),
            data_dir: var("VOSU_TOOL_DATA_DIR")?.into(),
            cache_dir: var("VOSU_TOOL_CACHE_DIR")?.into(),
        })
    }

    pub fn client(&self) -> Client {
        Client {
            http: reqwest::Client::new(),
            base: self.vosu_url.clone(),
            token: self.token.clone(),
        }
    }
}

/// Serves `router` until vosu closes stdin. Adds `GET /healthz` and the token
/// check for every route, so `router` must not define `/healthz` itself.
pub async fn serve(env: &ToolEnv, router: Router) -> Result<(), Error> {
    let router = router
        .route("/healthz", get(|| async { StatusCode::NO_CONTENT }))
        .layer(middleware::from_fn_with_state(
            env.token.clone(),
            require_token,
        ));
    let listener = tokio::net::TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await?;
    let port = listener.local_addr()?.port();
    {
        let mut stdout = std::io::stdout().lock();
        writeln!(stdout, "vosu-tool-ready {port}")?;
        stdout.flush()?;
    }
    let server = axum::serve(listener, router).with_graceful_shutdown(stdin_closed());
    tokio::select! {
        result = server => result?,
        () = async { stdin_closed().await; tokio::time::sleep(EXIT_GRACE).await } => {}
    }
    Ok(())
}

async fn stdin_closed() {
    let mut stdin = tokio::io::stdin();
    let mut buffer = [0_u8; 64];
    while matches!(stdin.read(&mut buffer).await, Ok(n) if n > 0) {}
}

async fn require_token(State(token): State<Arc<str>>, request: Request, next: Next) -> Response {
    let presented = request
        .headers()
        .get(TOKEN_HEADER)
        .map(|value| value.as_bytes())
        .unwrap_or_default();
    if bool::from(presented.ct_eq(token.as_bytes())) {
        next.run(request).await
    } else {
        StatusCode::UNAUTHORIZED.into_response()
    }
}

/// Who sent a forwarded request, from `X-Vosu-Caller`.
pub fn caller(request: &axum::http::HeaderMap) -> Option<&str> {
    request.get(CALLER_HEADER)?.to_str().ok()
}

/// HTTP client for vosu public APIs, the host API, and dependency tools.
/// Every request carries this tool's token.
#[derive(Clone, Debug)]
pub struct Client {
    http: reqwest::Client,
    base: String,
    token: Arc<str>,
}

impl Client {
    /// Absolute URL for a vosu path such as `/json/v2`.
    pub fn url(&self, path: &str) -> String {
        format!("{}/{}", self.base, path.trim_start_matches('/'))
    }

    /// WebSocket URL for a vosu path such as `/websocket/v2/subscribe`.
    pub fn ws_url(&self, path: &str) -> String {
        let url = self.url(path);
        match url.strip_prefix("https://") {
            Some(rest) => format!("wss://{rest}"),
            None => format!("ws://{}", url.trim_start_matches("http://")),
        }
    }

    pub fn request(&self, method: reqwest::Method, path: &str) -> reqwest::RequestBuilder {
        self.http
            .request(method, self.url(path))
            .header(TOKEN_HEADER, &*self.token)
    }

    pub async fn get_json<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        send_json(self.request(reqwest::Method::GET, path)).await
    }

    /// `GET /tools/_host/v1/<path>`.
    pub async fn host<T: DeserializeOwned>(&self, path: &str) -> Result<T, Error> {
        self.get_json(&format!("/tools/_host/v1/{}", path.trim_start_matches('/')))
            .await
    }

    /// Request to a dependency's API: `/tools/<tool>/api/<path>`.
    pub fn tool(&self, method: reqwest::Method, tool: &str, path: &str) -> reqwest::RequestBuilder {
        self.request(
            method,
            &format!("/tools/{tool}/api/{}", path.trim_start_matches('/')),
        )
    }

    pub async fn tool_json<T: DeserializeOwned>(&self, tool: &str, path: &str) -> Result<T, Error> {
        send_json(self.tool(reqwest::Method::GET, tool, path)).await
    }

    /// Effective settings with defaults applied.
    pub async fn settings<T: DeserializeOwned>(&self) -> Result<T, Error> {
        self.host("settings").await
    }

    /// Selected osu! instance and beatmap; requires the `context` permission.
    pub async fn context(&self) -> Result<Context, Error> {
        self.host("context").await
    }
}

/// Sends `request` and decodes a 2xx JSON body.
pub async fn send_json<T: DeserializeOwned>(request: reqwest::RequestBuilder) -> Result<T, Error> {
    let response = request.send().await?;
    let status = response.status();
    if !status.is_success() {
        return Err(Error::Status {
            url: response.url().to_string(),
            status: status.as_u16(),
        });
    }
    Ok(response.json().await?)
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct Context {
    /// `"stable"` or `"lazer"`; `None` when no osu! instance is attached.
    pub client: Option<String>,
    pub songs_dir: Option<PathBuf>,
    pub beatmap: Option<BeatmapContext>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq)]
pub struct BeatmapContext {
    /// MD5 of the `.osu` file.
    pub checksum: String,
    /// `"osu"`, `"taiko"`, `"fruits"`, or `"mania"`.
    pub mode: String,
    pub folder: PathBuf,
    pub file: PathBuf,
    pub audio: Option<PathBuf>,
    pub background: Option<PathBuf>,
}
