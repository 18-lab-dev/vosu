//! D15 real-forge evidence (Linux containers only).
//!
//! The runner starts a pinned Forgejo on an isolated container network and
//! runs this binary in a second container whose network aliases are the exact
//! fixture names and whose OS trust store does (`trusted`) or does not
//! (`untrusted`) contain the internal CA. Requests go through the production
//! `PublicOnly` destination policy, the system resolver (observed by a DNS
//! recorder) and the production TLS builder; the recorder in front of Forgejo
//! records what the forge actually received.

mod trusted;
mod untrusted;

use std::{
    net::{IpAddr, Ipv4Addr, SocketAddr},
    path::Path,
    sync::Arc,
};

use semver::Version;
use tempfile::TempDir;
use tokio_util::sync::CancellationToken;
use url::Url;
use vosu_core::catalog::{
    credentials::CredentialInput,
    forges::{CatalogFetchService, FetchContext},
    sources::{CandidateSnapshot, SourceIdentity, SourceInput},
};
use vosu_net::{
    Deadlines, DestinationPolicy, DownloadLimits, StreamingClient, SystemResolver,
    is_global_unicast,
    streaming::{RequestPolicy, RequestPurpose, SourcePolicy},
};

use crate::{
    fixture::{Fixture, OWNER},
    recorder::{Auth, DnsRecorder, Received, Role, Sentinels, TlsRecorder},
};

pub const JSON: &str = "application/json";
pub const OCTET: &str = "application/octet-stream";
/// reqwest's default; probes add no typed Accept header.
pub const ANY: &str = "*/*";

pub struct Forge<'a> {
    pub fixture: &'a Fixture,
    pub recorder: TlsRecorder,
    pub dns: Arc<DnsRecorder<SystemResolver>>,
    pub client: StreamingClient,
    pub root: TempDir,
    pub service: CatalogFetchService,
}

pub async fn start(fixture: &Fixture) -> Forge<'_> {
    let forge = fixture.forge();
    assert_eq!(
        forge.https_port, 443,
        "Forgejo's ROOT_URL names the default HTTPS port"
    );
    let secrets = &forge.secrets;
    let recorder = TlsRecorder::start(
        fixture,
        Sentinels::new(&secrets.token, &secrets.username, &secrets.password),
        &[
            (
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, forge.https_port)),
                Role::Forge {
                    upstream: forge.upstream.clone(),
                },
            ),
            (
                SocketAddr::from((Ipv4Addr::UNSPECIFIED, forge.cross_port)),
                Role::CrossPort {
                    forge_port: forge.https_port,
                },
            ),
        ],
    )
    .await;
    let dns = Arc::new(DnsRecorder::system());
    let client = StreamingClient::new(
        dns.clone(),
        DestinationPolicy::PublicOnly,
        Deadlines::default(),
        DownloadLimits::default(),
    );
    let root = TempDir::new().expect("config root");
    let service = CatalogFetchService::open(root.path()).expect("open catalog fetch service");
    Forge {
        fixture,
        recorder,
        dns,
        client,
        root,
        service,
    }
}

impl Forge<'_> {
    pub fn host(&self) -> &str {
        &self.fixture.forge_host
    }

    pub fn repository(&self, repo: &str) -> String {
        format!("https://{}/{OWNER}/{repo}", self.host())
    }

    pub fn download_path(repo: &str, tag: &str, asset: &str) -> String {
        format!("/{OWNER}/{repo}/releases/download/{tag}/{asset}")
    }

    pub fn token(&self) -> CredentialInput {
        CredentialInput::Token {
            token: self.fixture.forge().secrets.token.clone(),
        }
    }

    pub fn basic(&self) -> CredentialInput {
        let secrets = &self.fixture.forge().secrets;
        CredentialInput::Basic {
            username: secrets.username.clone(),
            password: secrets.password.clone(),
        }
    }

    /// Replaces the source list with exactly `source`; removal purges any
    /// earlier credential and flag, so each phase starts from defaults.
    pub async fn configure(&self, source: &str, credential: Option<CredentialInput>) {
        self.service
            .sources()
            .replace(&[SourceInput {
                source: source.to_owned(),
                enabled: true,
            }])
            .expect("replace sources");
        if let Some(credential) = credential {
            self.service
                .set_credential(source, credential)
                .await
                .expect("store source credential");
        }
    }

    pub fn allow_private(&self, source: &str) {
        self.service
            .sources()
            .set_private_network(&SourceIdentity::parse(source).unwrap().identity, true)
            .expect("set exact source private-network flag");
    }

    pub async fn refresh(&self) -> Arc<CandidateSnapshot> {
        let version = Version::new(2, 0, 0);
        self.service
            .refresh(
                &self.client,
                FetchContext {
                    registrations: &[],
                    revisions: &[],
                    application: &version,
                    target: "x86_64-unknown-linux-gnu",
                },
                &CancellationToken::new(),
            )
            .await
            .expect("refresh publishes a snapshot")
    }

    pub fn source_policy(&self, private: bool) -> SourcePolicy {
        SourcePolicy::repository(
            &Url::parse(&format!("https://{}/", self.host())).unwrap(),
            private,
        )
        .unwrap()
    }

    pub fn probe_policy(&self, host: &str, private: bool) -> RequestPolicy {
        RequestPolicy::source(
            RequestPurpose::Probe,
            SourcePolicy::repository(&Url::parse(&format!("https://{host}/")).unwrap(), private)
                .unwrap(),
        )
        .unwrap()
    }

    pub fn received(&self, path: &str, accept: Option<&str>, auth: Auth, status: u16) -> Received {
        self.received_on(self.fixture.forge().https_port, path, accept, auth, status)
    }

    pub fn received_on(
        &self,
        port: u16,
        path: &str,
        accept: Option<&str>,
        auth: Auth,
        status: u16,
    ) -> Received {
        Received {
            port,
            host: self.host().to_owned(),
            path: path.to_owned(),
            accept: accept.map(str::to_owned),
            auth,
            status,
        }
    }

    /// Unknown-host detection as Forgejo really answers it: GitLab's probe
    /// 404s, Gitea's succeeds; neither carries a credential.
    pub fn probes(&self) -> [Received; 2] {
        [
            self.received("/api/v4/version", Some(ANY), Auth::None, 404),
            self.received("/api/v1/version", Some(ANY), Auth::None, 200),
        ]
    }

    pub fn metadata(&self, repo: &str, auth: Auth, status: u16) -> Received {
        self.received(
            &format!("/api/v1/repos/{OWNER}/{repo}/releases/latest"),
            Some(JSON),
            auth,
            status,
        )
    }

    /// Every lookup of `host` so far answered only non-global addresses, so a
    /// refusal is the destination policy's decision rather than DNS failure.
    pub fn assert_private_answers(&self, host: &str, since: usize) {
        let lookups: Vec<_> = self.dns.lookups().into_iter().skip(since).collect();
        let answers: Vec<IpAddr> = lookups
            .iter()
            .filter(|(name, _)| name == host)
            .flat_map(|(_, answers)| answers.iter().copied())
            .collect();
        assert!(
            !answers.is_empty() && answers.iter().all(|address| !is_global_unicast(*address)),
            "{host} must resolve to private fixture addresses: {lookups:?}"
        );
    }

    pub async fn shutdown(self) {
        self.recorder.shutdown().await;
    }
}

/// The approved credential store is the only file that may contain a secret,
/// and it must; every other byte under the config root is secret-free.
pub fn assert_secrets_only_in_credential_store(root: &Path, secrets: &[&str]) {
    let mut files = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(&directory).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    let store = root.join("catalog-credentials.toml");
    assert!(files.contains(&store), "credential store was not written");
    for file in files {
        let bytes = std::fs::read(&file).unwrap();
        for (index, secret) in secrets.iter().enumerate() {
            let present = bytes
                .windows(secret.len())
                .any(|window| window == secret.as_bytes());
            assert_eq!(
                present,
                file == store,
                "secret #{index} presence in {} (only the credential store may hold it)",
                file.display()
            );
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        assert_eq!(
            std::fs::metadata(&store).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}

/// Diagnostic surfaces (Debug/Display of snapshots, records, failures and
/// credential presence) never carry a secret.
pub fn assert_no_secret_in(surfaces: &[String], secrets: &[&str]) {
    for (index, secret) in secrets.iter().enumerate() {
        for surface in surfaces {
            assert!(
                !surface.contains(secret),
                "secret #{index} leaked into a diagnostic surface"
            );
        }
    }
}
