//! Hardened streaming HTTP and ZIP installation shared by every downloader.

use std::io;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::StreamExt;
use serde::de::DeserializeOwned;
use thiserror::Error;
use tokio::io::AsyncWriteExt;
use tokio_util::sync::CancellationToken;
use url::{Host, Url};

pub type ResolverFuture<'a> = Pin<Box<dyn Future<Output = io::Result<Vec<IpAddr>>> + Send + 'a>>;

pub type SleepFuture<'a> = Pin<Box<dyn Future<Output = ()> + Send + 'a>>;

pub trait TimeSource: Send + Sync {
    fn sleep(&self, duration: Duration) -> SleepFuture<'_>;
}

#[derive(Debug, Default)]
pub struct TokioTime;
impl TimeSource for TokioTime {
    fn sleep(&self, duration: Duration) -> SleepFuture<'_> {
        Box::pin(tokio::time::sleep(duration))
    }
}

pub trait AddressResolver: Send + Sync {
    fn resolve<'a>(&'a self, host: &'a str) -> ResolverFuture<'a>;
}

#[derive(Debug, Default)]
pub struct SystemResolver;
impl AddressResolver for SystemResolver {
    fn resolve<'a>(&'a self, host: &'a str) -> ResolverFuture<'a> {
        Box::pin(async move {
            let addresses = tokio::net::lookup_host((host, 0)).await?;
            Ok(addresses
                .map(|address| normalize_ip(address.ip()))
                .collect())
        })
    }
}

#[cfg(feature = "test-harness")]
#[derive(Clone, Debug)]
pub struct LoopbackHarnessPolicy {
    reserved_hosts: Arc<[String]>,
    allowed_sockets: Arc<[SocketAddr]>,
}

#[derive(Clone, Debug)]
pub enum DestinationPolicy {
    PublicOnly,
    #[cfg(feature = "test-harness")]
    LoopbackHarness(LoopbackHarnessPolicy),
}

#[cfg(feature = "test-harness")]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TestNetworkEndpoints {
    pub catalog: String,
    pub release: String,
    pub offsets_base: String,
}

#[cfg(feature = "test-harness")]
#[derive(Clone)]
pub struct TestNetworkConfig {
    resolver: Arc<dyn AddressResolver>,
    reserved_hosts: Arc<[String]>,
    allowed_sockets: Arc<[SocketAddr]>,
    endpoints: Option<TestNetworkEndpoints>,
}

#[cfg(feature = "test-harness")]
impl TestNetworkConfig {
    pub fn new(
        resolver: Arc<dyn AddressResolver>,
        reserved_hosts: impl Into<Arc<[String]>>,
        allowed_sockets: impl Into<Arc<[SocketAddr]>>,
    ) -> Result<Self, NetworkError> {
        let reserved_hosts: Arc<[String]> = reserved_hosts
            .into()
            .iter()
            .map(|host| host.trim_end_matches('.').to_ascii_lowercase())
            .collect::<Vec<_>>()
            .into();
        let allowed_sockets: Arc<[SocketAddr]> = allowed_sockets
            .into()
            .iter()
            .map(|socket| SocketAddr::new(normalize_ip(socket.ip()), socket.port()))
            .collect::<Vec<_>>()
            .into();
        if reserved_hosts.is_empty()
            || allowed_sockets.is_empty()
            || reserved_hosts
                .iter()
                .any(|host| host.is_empty() || !host.ends_with(".invalid"))
            || allowed_sockets
                .iter()
                .any(|socket| !socket.ip().is_loopback())
        {
            return Err(NetworkError::Destination(
                "invalid test-harness network configuration".into(),
            ));
        }
        Ok(Self {
            resolver,
            reserved_hosts,
            allowed_sockets,
            endpoints: None,
        })
    }

    pub fn with_endpoints(mut self, endpoints: TestNetworkEndpoints) -> Result<Self, NetworkError> {
        for endpoint in [
            &endpoints.catalog,
            &endpoints.release,
            &endpoints.offsets_base,
        ] {
            let url = Url::parse(endpoint)?;
            let host = url
                .host_str()
                .unwrap_or_default()
                .trim_end_matches('.')
                .to_ascii_lowercase();
            let port = url
                .port_or_known_default()
                .ok_or_else(|| NetworkError::Destination("test endpoint has no port".into()))?;
            if !self.reserved_hosts.iter().any(|reserved| reserved == &host)
                || !self
                    .allowed_sockets
                    .iter()
                    .any(|socket| socket.port() == port)
                || !matches!(url.scheme(), "http" | "https")
            {
                return Err(NetworkError::Destination(
                    "test endpoint is outside the reserved exact-listener policy".into(),
                ));
            }
        }
        self.endpoints = Some(endpoints);
        Ok(self)
    }

    pub fn endpoints(&self) -> Option<&TestNetworkEndpoints> {
        self.endpoints.as_ref()
    }

    pub(crate) fn into_parts(self) -> (Arc<dyn AddressResolver>, DestinationPolicy) {
        let policy = DestinationPolicy::LoopbackHarness(LoopbackHarnessPolicy {
            reserved_hosts: self.reserved_hosts,
            allowed_sockets: self.allowed_sockets,
        });
        (self.resolver, policy)
    }
}

#[derive(Clone, Copy, Debug)]
pub struct Deadlines {
    pub connect: Duration,
    pub idle: Duration,
    pub total: Duration,
}
impl Default for Deadlines {
    fn default() -> Self {
        Self {
            connect: Duration::from_secs(10),
            idle: Duration::from_secs(30),
            total: Duration::from_secs(300),
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct DownloadLimits {
    pub max_bytes: u64,
    pub max_redirects: usize,
}
impl Default for DownloadLimits {
    fn default() -> Self {
        Self {
            max_bytes: 1024 * 1024 * 1024,
            max_redirects: 5,
        }
    }
}

#[derive(Clone, Copy, Debug)]
pub struct ZipLimits {
    pub max_entries: usize,
    pub max_expanded_bytes: u64,
}
impl Default for ZipLimits {
    fn default() -> Self {
        Self {
            max_entries: 100_000,
            max_expanded_bytes: 2 * 1024 * 1024 * 1024,
        }
    }
}

#[derive(Debug)]
pub struct NetworkResponse {
    pub status: u16,
    pub body: Bytes,
    pub final_url: Url,
}

#[derive(Debug, Error)]
pub enum NetworkError {
    #[error("invalid URL: {0}")]
    Url(#[from] url::ParseError),
    #[error("only HTTPS URLs are allowed")]
    Scheme,
    #[error("destination is not permitted: {0}")]
    Destination(String),
    #[error("DNS resolution failed: {0}")]
    Resolve(#[source] io::Error),
    #[error("request failed: {0}")]
    Request(#[from] reqwest::Error),
    #[error("request timed out")]
    Timeout,
    #[error("request was cancelled")]
    Cancelled,
    #[error("redirect has no valid Location header")]
    RedirectLocation,
    #[error("too many redirects")]
    RedirectLimit,
    #[error("HTTPS redirect downgrade is forbidden")]
    Downgrade,
    #[error("response status {0}")]
    Status(u16),
    #[error("response exceeds {0} bytes")]
    TooLarge(u64),
    #[error("I/O failed: {0}")]
    Io(#[from] io::Error),
    #[error("JSON decoding failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("ZIP failed: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("unsafe ZIP entry: {0}")]
    UnsafeZip(String),
}

#[derive(Clone)]
pub struct StreamingClient {
    resolver: Arc<dyn AddressResolver>,
    policy: DestinationPolicy,
    deadlines: Deadlines,
    clock: Arc<dyn TimeSource>,
    limits: DownloadLimits,
}

impl StreamingClient {
    pub fn production() -> Self {
        Self::new(
            Arc::new(SystemResolver),
            DestinationPolicy::PublicOnly,
            Deadlines::default(),
            DownloadLimits::default(),
        )
    }
    pub fn new(
        resolver: Arc<dyn AddressResolver>,
        policy: DestinationPolicy,
        deadlines: Deadlines,
        limits: DownloadLimits,
    ) -> Self {
        Self {
            resolver,
            policy,
            deadlines,
            limits,
            clock: Arc::new(TokioTime),
        }
    }

    #[cfg(feature = "test-harness")]
    pub fn for_test_network(
        config: TestNetworkConfig,
        deadlines: Deadlines,
        limits: DownloadLimits,
    ) -> Self {
        let (resolver, policy) = config.into_parts();
        Self::new(resolver, policy, deadlines, limits)
    }

    pub fn with_time_source(mut self, clock: Arc<dyn TimeSource>) -> Self {
        self.clock = clock;
        self
    }

    pub fn with_deadlines_and_limits(
        mut self,
        deadlines: Deadlines,
        limits: DownloadLimits,
    ) -> Self {
        self.deadlines = deadlines;
        self.limits = limits;
        self
    }

    pub async fn request_bytes(
        &self,
        url: &str,
        user_agent: &str,
        cancel: &CancellationToken,
    ) -> Result<NetworkResponse, NetworkError> {
        self.with_total(self.request_inner(url, user_agent, cancel, None))
            .await
    }

    pub async fn request_json<T: DeserializeOwned>(
        &self,
        url: &str,
        user_agent: &str,
        cancel: &CancellationToken,
    ) -> Result<T, NetworkError> {
        let response = self.request_bytes(url, user_agent, cancel).await?;
        if !(200..300).contains(&response.status) {
            return Err(NetworkError::Status(response.status));
        }
        Ok(serde_json::from_slice(&response.body)?)
    }

    pub async fn download_atomic(
        &self,
        url: &str,
        user_agent: &str,
        destination: &Path,
        cancel: &CancellationToken,
    ) -> Result<(), NetworkError> {
        let parent = destination.parent().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "destination has no parent")
        })?;
        tokio::fs::create_dir_all(parent).await?;
        let temp = tempfile::Builder::new()
            .prefix(".download-")
            .suffix(".part")
            .tempfile_in(parent)?;
        let (file, path) = temp.keep().map_err(|error| error.error)?;
        let path_guard = PartialFile(path.clone());
        let file = tokio::fs::File::from_std(file);
        let response = self
            .with_total(self.request_inner(url, user_agent, cancel, Some(file)))
            .await?;
        if !(200..300).contains(&response.status) {
            return Err(NetworkError::Status(response.status));
        }
        if cancel.is_cancelled() {
            return Err(NetworkError::Cancelled);
        }
        let synced = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&path)?;
        synced.sync_all()?;
        drop(synced);
        crate::platform::atomic_replace(&path, destination)?;
        drop(path_guard);
        Ok(())
    }

    async fn with_total<T>(
        &self,
        future: impl Future<Output = Result<T, NetworkError>>,
    ) -> Result<T, NetworkError> {
        tokio::select! {
            result = future => result,
            () = self.clock.sleep(self.deadlines.total) => Err(NetworkError::Timeout),
        }
    }

    async fn request_inner(
        &self,
        url: &str,
        user_agent: &str,
        cancel: &CancellationToken,
        mut output: Option<tokio::fs::File>,
    ) -> Result<NetworkResponse, NetworkError> {
        let mut current = Url::parse(url)?;
        let original_https = current.scheme() == "https";
        let mut redirects = 0usize;
        loop {
            if cancel.is_cancelled() {
                return Err(NetworkError::Cancelled);
            }
            let resolved = tokio::select! {
                () = cancel.cancelled() => return Err(NetworkError::Cancelled),
                result = self.resolve_and_validate(&current) => result?,
            };
            let (host, port, addresses) = resolved;
            if original_https && current.scheme() != "https" {
                return Err(NetworkError::Downgrade);
            }
            let sockets: Vec<_> = addresses
                .iter()
                .map(|ip| SocketAddr::new(*ip, port))
                .collect();
            let client = reqwest::Client::builder()
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .connect_timeout(self.deadlines.connect)
                .resolve_to_addrs(&host, &sockets)
                .build()?;
            let response = tokio::select! {
                () = cancel.cancelled() => return Err(NetworkError::Cancelled),
                result = wait_for(
                    self.clock.as_ref(),
                    self.deadlines.idle,
                    client.get(current.clone()).header(reqwest::header::USER_AGENT, user_agent).send(),
                ) => {
                    match result? {
                        Ok(response) => response,
                        Err(error) if error.is_timeout() => return Err(NetworkError::Timeout),
                        Err(error) => return Err(NetworkError::Request(error)),
                    }
                },
            };
            if response.status().is_redirection() {
                current = redirect_target(
                    &current,
                    response.headers().get(reqwest::header::LOCATION),
                    redirects,
                    self.limits.max_redirects,
                )?;
                redirects += 1;
                continue;
            }
            let status = response.status().as_u16();
            // Security and resource invariant: never poll a status-error body.
            if !(200..300).contains(&status) {
                return Ok(NetworkResponse {
                    status,
                    body: Bytes::new(),
                    final_url: current,
                });
            }
            if response
                .content_length()
                .is_some_and(|length| length > self.limits.max_bytes)
            {
                return Err(NetworkError::TooLarge(self.limits.max_bytes));
            }
            let mut stream = response.bytes_stream();
            let mut body = Vec::new();
            let mut count = 0u64;
            while let Some(chunk) = tokio::select! {
                () = cancel.cancelled() => return Err(NetworkError::Cancelled),
                result = wait_for(self.clock.as_ref(), self.deadlines.idle, stream.next()) => result?,
            } {
                let chunk = chunk?;
                count = count
                    .checked_add(chunk.len() as u64)
                    .ok_or(NetworkError::TooLarge(self.limits.max_bytes))?;
                if count > self.limits.max_bytes {
                    return Err(NetworkError::TooLarge(self.limits.max_bytes));
                }
                if let Some(file) = output.as_mut() {
                    tokio::select! {
                        () = cancel.cancelled() => return Err(NetworkError::Cancelled),
                        result = file.write_all(&chunk) => result?,
                    }
                } else {
                    body.extend_from_slice(&chunk);
                }
            }
            if let Some(file) = output.as_mut() {
                tokio::select! {
                    () = cancel.cancelled() => return Err(NetworkError::Cancelled),
                    result = file.flush() => result?,
                }
            }
            return Ok(NetworkResponse {
                status,
                body: Bytes::from(body),
                final_url: current,
            });
        }
    }

    async fn resolve_and_validate(
        &self,
        url: &Url,
    ) -> Result<(String, u16, Vec<IpAddr>), NetworkError> {
        let host = url
            .host()
            .ok_or_else(|| NetworkError::Destination("URL has no host".into()))?;
        let host_text = match host {
            Host::Domain(value) => value.to_owned(),
            Host::Ipv4(value) => value.to_string(),
            Host::Ipv6(value) => value.to_string(),
        };
        let port = url
            .port_or_known_default()
            .ok_or_else(|| NetworkError::Destination("URL has no port".into()))?;
        let addresses = match host {
            Host::Ipv4(value) => vec![IpAddr::V4(value)],
            Host::Ipv6(value) => vec![normalize_ip(IpAddr::V6(value))],
            Host::Domain(_) => self
                .resolver
                .resolve(&host_text)
                .await
                .map_err(NetworkError::Resolve)?,
        };
        if addresses.is_empty() {
            return Err(NetworkError::Destination("empty DNS answer".into()));
        }
        let addresses: Vec<_> = addresses.into_iter().map(normalize_ip).collect();
        match &self.policy {
            DestinationPolicy::PublicOnly => {
                if url.scheme() != "https" {
                    return Err(NetworkError::Scheme);
                }
                if let Some(address) = addresses
                    .iter()
                    .find(|address| !is_global_unicast(**address))
                {
                    return Err(NetworkError::Destination(address.to_string()));
                }
            }
            #[cfg(feature = "test-harness")]
            DestinationPolicy::LoopbackHarness(policy) => {
                if !policy
                    .reserved_hosts
                    .iter()
                    .any(|allowed| allowed.eq_ignore_ascii_case(&host_text))
                {
                    return Err(NetworkError::Destination(host_text));
                }
                if url.scheme() != "https" && url.scheme() != "http" {
                    return Err(NetworkError::Scheme);
                }
                if addresses
                    .iter()
                    .any(|ip| !policy.allowed_sockets.contains(&SocketAddr::new(*ip, port)))
                {
                    return Err(NetworkError::Destination(
                        "socket is outside harness allowlist".into(),
                    ));
                }
            }
        }
        Ok((host_text, port, addresses))
    }
}

async fn wait_for<T>(
    clock: &dyn TimeSource,
    duration: Duration,
    future: impl Future<Output = T>,
) -> Result<T, NetworkError> {
    tokio::select! {
        result = future => Ok(result),
        () = clock.sleep(duration) => Err(NetworkError::Timeout),
    }
}

fn redirect_target(
    current: &Url,
    location: Option<&reqwest::header::HeaderValue>,
    redirects: usize,
    maximum: usize,
) -> Result<Url, NetworkError> {
    if redirects >= maximum {
        return Err(NetworkError::RedirectLimit);
    }
    let location = location
        .and_then(|value| value.to_str().ok())
        .ok_or(NetworkError::RedirectLocation)?;
    let next = current.join(location)?;
    if current.scheme() == "https" && next.scheme() != "https" {
        return Err(NetworkError::Downgrade);
    }
    Ok(next)
}

struct PartialFile(PathBuf);
impl Drop for PartialFile {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct PartialDirectory(PathBuf);
impl Drop for PartialDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn normalize_ip(address: IpAddr) -> IpAddr {
    match address {
        IpAddr::V6(value) => value
            .to_ipv4_mapped()
            .map(IpAddr::V4)
            .unwrap_or(IpAddr::V6(value)),
        other => other,
    }
}

pub fn is_global_unicast(address: IpAddr) -> bool {
    match normalize_ip(address) {
        IpAddr::V4(value) => {
            let [a, b, c, _] = value.octets();
            !(a == 0
                || a == 10
                || a == 127
                || a >= 224
                || (a == 100 && (64..=127).contains(&b))
                || (a == 169 && b == 254)
                || (a == 172 && (16..=31).contains(&b))
                || (a == 192 && b == 168)
                || (a == 192 && b == 0)
                || (a == 192 && b == 88 && c == 99)
                || (a == 198 && (b == 18 || b == 19))
                || (a == 198 && b == 51 && c == 100)
                || (a == 203 && b == 0 && c == 113))
        }
        IpAddr::V6(value) => {
            let s = value.segments();
            if value.is_unspecified() || value.is_loopback() || value.is_multicast() {
                return false;
            }
            if (s[0] & 0xfe00) == 0xfc00 || (s[0] & 0xffc0) == 0xfe80 {
                return false;
            }
            // Only the RFC 6052 well-known 64:ff9b::/96 prefix embeds IPv4.
            // The rest of 64:ff9b::/32, including local-use 64:ff9b:1::/48,
            // is special-purpose and is never accepted.
            if s[0] == 0x64 && s[1] == 0xff9b {
                if s[2..6] != [0, 0, 0, 0] {
                    return false;
                }
                let v4 =
                    Ipv4Addr::new((s[6] >> 8) as u8, s[6] as u8, (s[7] >> 8) as u8, s[7] as u8);
                return is_global_unicast(IpAddr::V4(v4));
            }
            if s[0] == 0x2001 && s[1] == 0x0db8 {
                return false;
            }
            if (s[0] & 0xe000) != 0x2000 || s[0] == 0x3fff || s[0] == 0x2002 {
                return false;
            }
            // IANA special-purpose 2001::/23; only documented global exceptions.
            if s[0] == 0x2001 && s[1] <= 0x1ff {
                return s[1] == 3 || (s[1] == 4 && s[2] == 0x112);
            }
            true
        }
    }
}

mod archive;

pub use archive::extract_zip_atomic;

#[cfg(test)]
mod tests;
