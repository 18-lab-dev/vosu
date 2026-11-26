//! I0 executor composition: real streaming download over the exact loopback
//! harness, real ZIP/SDK verification, real P0 confirmation and T0 policy.
use super::*;
use crate::catalog::{
    plans::{self, CatalogJob, PlanRegistry, Prerequisites},
    resolver::{BuiltinProviders, Candidate, Request, Snapshot},
    sources::{SourceIdentity, SourceRecord},
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Write as _,
    net::SocketAddr,
    sync::atomic::{AtomicUsize, Ordering},
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use vosu_net::{
    AddressResolver, Deadlines, DownloadLimits, ResolverFuture, StreamingClient, TestNetworkConfig,
};
use vosu_plugin_api::catalog::Catalog;

pub(super) const TARGET: &str = "x86_64-unknown-linux-gnu";
pub(super) const OFFICIAL: &str = "18-lab-dev/vosu-tools";
pub(super) const HOST: &str = "pkg.invalid";
const LIMIT: u64 = 512 * 1024 * 1024;

/// Plain HTTP/1.1 loopback server: exact routes, recorded raw request heads,
/// and an optional held route that only client cancellation can end.
pub(super) struct Server {
    pub socket: SocketAddr,
    routes: Arc<Mutex<HashMap<String, Vec<u8>>>>,
    requests: Arc<Mutex<Vec<String>>>,
    pub received: Arc<tokio::sync::Notify>,
    hold: Arc<Mutex<Option<String>>>,
    task: tokio::task::JoinHandle<()>,
}
impl Server {
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let socket = listener.local_addr().unwrap();
        let routes: Arc<Mutex<HashMap<String, Vec<u8>>>> = Arc::default();
        let requests: Arc<Mutex<Vec<String>>> = Arc::default();
        let received = Arc::new(tokio::sync::Notify::new());
        let hold: Arc<Mutex<Option<String>>> = Arc::default();
        let task = {
            let (routes, requests, received, hold) = (
                routes.clone(),
                requests.clone(),
                received.clone(),
                hold.clone(),
            );
            tokio::spawn(async move {
                let mut connections = tokio::task::JoinSet::new();
                loop {
                    let Ok((mut stream, _)) = listener.accept().await else {
                        return;
                    };
                    let (routes, requests, received, hold) = (
                        routes.clone(),
                        requests.clone(),
                        received.clone(),
                        hold.clone(),
                    );
                    connections.spawn(async move {
                        let mut head = Vec::new();
                        let mut buffer = [0u8; 4096];
                        while !head.windows(4).any(|w| w == b"\r\n\r\n") && head.len() < 65536 {
                            let Ok(count) = stream.read(&mut buffer).await else {
                                return;
                            };
                            if count == 0 {
                                return;
                            }
                            head.extend_from_slice(&buffer[..count]);
                        }
                        let head = String::from_utf8_lossy(&head).into_owned();
                        let path = head.split(' ').nth(1).unwrap_or_default().to_owned();
                        requests.lock().unwrap().push(head);
                        received.notify_one();
                        let held = hold.lock().unwrap().as_deref() == Some(path.as_str());
                        if held {
                            std::future::pending::<()>().await;
                        }
                        let body = routes.lock().unwrap().get(&path).cloned();
                        let response = match &body {
                            Some(body) => format!(
                                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                                body.len()
                            ),
                            None => "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".into(),
                        };
                        let _ = stream.write_all(response.as_bytes()).await;
                        if let Some(body) = body {
                            let _ = stream.write_all(&body).await;
                        }
                        let _ = stream.shutdown().await;
                    });
                }
            })
        };
        Self {
            socket,
            routes,
            requests,
            received,
            hold,
            task,
        }
    }
    pub fn serve(&self, path: &str, body: Vec<u8>) {
        self.routes.lock().unwrap().insert(path.into(), body);
    }
    pub fn hold(&self, path: &str) {
        *self.hold.lock().unwrap() = Some(path.into());
    }
    pub fn requests(&self) -> Vec<String> {
        self.requests.lock().unwrap().clone()
    }
    pub fn url(&self, path: &str) -> String {
        format!("https://{HOST}:{}{path}", self.socket.port())
    }
    pub fn client(&self) -> StreamingClient {
        struct Loopback(std::net::IpAddr);
        impl AddressResolver for Loopback {
            fn resolve<'a>(&'a self, _: &'a str) -> ResolverFuture<'a> {
                let address = self.0;
                Box::pin(async move { Ok(vec![address]) })
            }
        }
        let config = TestNetworkConfig::new(
            Arc::new(Loopback(self.socket.ip())),
            vec![HOST.to_owned()],
            vec![self.socket],
        )
        .unwrap();
        StreamingClient::for_test_network(config, Deadlines::default(), DownloadLimits::default())
    }
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}

/// Catalog package URLs must be HTTPS (SDK rule) while the exact loopback
/// harness serves plain HTTP; only the scheme is rewritten before the
/// production transport and its credential-free `download_atomic` chain.
pub(super) struct Plaintext(pub PublicPackageTransport);
impl PackageTransport for Plaintext {
    fn download<'a>(
        &'a self,
        url: &'a str,
        max_bytes: u64,
        destination: &'a Path,
        cancel: &'a CancellationToken,
    ) -> DownloadFuture<'a> {
        let url = url.replacen("https://", "http://", 1);
        Box::pin(async move { self.0.download(&url, max_bytes, destination, cancel).await })
    }
}

pub(super) fn zip(entries: &[(&str, &[u8])]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        for (name, body) in entries {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

pub(super) fn sha(bytes: &[u8]) -> String {
    installed::hex(&Sha256::digest(bytes))
}

pub(super) fn schema(namespace: &str, version: &str) -> Vec<u8> {
    json!({"title":namespace,"version":version,"type":"object","properties":{
        "combo":{"x-sampling-lane":{"lane":"regular","activity":"gameplay"}}}})
    .to_string()
    .into_bytes()
}

/// One plugin package and its exact catalog mirror.
pub(super) struct Plugin {
    pub manifest: Value,
    pub entry: Value,
    pub files: Vec<(String, Vec<u8>)>,
}
pub(super) fn plugin(id: &str, version: &str, runtime: &str, requires: Value) -> Plugin {
    let library = format!("lib/lib{}.so", id.replace('.', "_"));
    let manifest = json!({"manifest_version":1,"kind":"plugin","id":id,"name":id,
        "version":version,"host":"^2","abi_version":1,"runtime":runtime,
        "targets":{TARGET:{"library":library}},
        "provides":[{"id":format!("field:{id}"),"version":"1.0.0","schema":format!("schemas/{id}.json")}],
        "requires":requires});
    let entry = json!({"id":id,"name":id,"version":version,"host":"^2","runtime":runtime,
        "provides":[{"id":format!("field:{id}"),"version":"1.0.0","schema_version":"1.0.0",
            "lanes":[{"pointer":"/combo","lane":"regular","activity":"gameplay"}]}],
        "requires":manifest["requires"].clone(),"packages":{}});
    Plugin {
        files: vec![
            (
                "manifest.toml".into(),
                toml::to_string(&manifest).unwrap().into_bytes(),
            ),
            (format!("schemas/{id}.json"), schema(id, "1.0.0")),
            (library, b"\x7fELF fixture library".to_vec()),
        ],
        manifest,
        entry,
    }
}
impl Plugin {
    pub fn archive(&self) -> Vec<u8> {
        let entries: Vec<_> = self
            .files
            .iter()
            .map(|(name, bytes)| (name.as_str(), bytes.as_slice()))
            .collect();
        zip(&entries)
    }
    /// Serve the archive and pin its exact hash/size in the catalog entry.
    pub fn publish(&mut self, server: &Server, path: &str) -> Vec<u8> {
        let bytes = self.archive();
        self.entry["packages"] =
            json!({TARGET:{"url":server.url(path),"sha256":sha(&bytes),"size":bytes.len()}});
        server.serve(path, bytes.clone());
        bytes
    }
}

pub(super) fn catalog(plugins: &[&Plugin], tools: &[Value]) -> Catalog {
    let document = json!({"catalog_version":2,"name":"I0","published":"2026-11-24T00:00:00Z",
        "plugins":plugins.iter().map(|p| p.entry.clone()).collect::<Vec<_>>(),"tools":tools});
    Catalog::parse(&document.to_string()).unwrap()
}

pub(super) fn record(source: &str, trusted: bool) -> SourceRecord {
    SourceRecord {
        identity: SourceIdentity::parse(source).unwrap().identity,
        enabled: true,
        trusted,
        private_network: false,
        order: 0,
    }
}

pub(super) fn prerequisites() -> Prerequisites {
    Prerequisites {
        source_generation: 1,
        trust_generation: 2,
        private_generation: 3,
        credential_generation: 4,
        installed_generation: 5,
        limit_generation: 6,
        snapshot_fingerprint: [7; 32],
        application: "2.1.0".into(),
        target: TARGET.into(),
        max_package_bytes: LIMIT,
    }
}

pub(super) fn plan(
    catalog: &Catalog,
    source: &SourceRecord,
    kind: PackageKind,
    request: &str,
) -> plans::PinnedPlan {
    let entries: Vec<_> = catalog.entries().collect();
    let candidates: Vec<_> = entries
        .iter()
        .enumerate()
        .map(|(index, (kind, entry))| Candidate {
            kind: *kind,
            entry,
            source: &source.identity,
            source_order: 0,
            catalog_order: index,
            trusted: source.trusted,
        })
        .collect();
    let builtins = BuiltinProviders::from_registry(
        &crate::state::schema::SchemaRegistry::with_core().unwrap(),
        &[],
    );
    let application = semver::Version::new(2, 1, 0);
    let plan = plans::build(
        &Snapshot {
            candidates: &candidates,
            installed: &[],
            builtins: &builtins,
            application: &application,
            target: TARGET,
            max_package_bytes: LIMIT,
        },
        &[Request {
            kind,
            id: request,
            source: &source.identity,
        }],
        &CurrentPolicy {
            generation: SourceGeneration(1),
            sources: std::slice::from_ref(source),
        },
        prerequisites(),
    );
    assert!(plan.view().errors.is_empty(), "{:?}", plan.view().errors);
    plan
}

/// Records every handoff and whether the graph was durably committed (no
/// journal, records published, directory in place) at the moment of the call.
#[derive(Default)]
pub(super) struct Recorder {
    pub calls: Mutex<Vec<(String, bool)>>,
    pub web: AtomicUsize,
    pub layout: Mutex<Option<InstallLayout>>,
}
pub(super) fn committed_on_disk(layout: &InstallLayout, id: &str, kind: PackageKind) -> bool {
    !layout.state.join(JOURNAL_FILE).exists()
        && installed::read_records(layout).is_ok_and(|document| document.record(kind, id).is_some())
        && layout
            .destination(kind, id)
            .unwrap()
            .join("manifest.toml")
            .is_file()
}
impl RuntimeHandoff for Recorder {
    fn load_plugin(self: Arc<Self>, request: LoadRequest) -> HandoffFuture<'static, LoadStatus> {
        let layout = self.layout.lock().unwrap().clone().unwrap();
        let committed = committed_on_disk(&layout, &request.record.id, PackageKind::Plugin)
            && request.directory
                == layout
                    .destination(PackageKind::Plugin, &request.record.id)
                    .unwrap();
        self.calls
            .lock()
            .unwrap()
            .push((request.record.id.clone(), committed));
        Box::pin(async { LoadStatus::Loaded })
    }
    fn rescan_web(&self) -> HandoffFuture<'_, ()> {
        self.web.fetch_add(1, Ordering::SeqCst);
        Box::pin(async {})
    }
}

pub(super) struct Fixture {
    pub root: tempfile::TempDir,
    pub layout: InstallLayout,
    pub server: Server,
    pub recorder: Arc<Recorder>,
    pub executor: InstallExecutor,
    pub policy: Arc<Mutex<Vec<SourceRecord>>>,
}
impl Fixture {
    pub async fn new(sources: Vec<SourceRecord>) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path().to_path_buf();
        let layout = InstallLayout::new(
            base.join("state"),
            base.join("plugins"),
            base.join("tools"),
            base.join("overlays"),
            base.join("downloads"),
        )
        .unwrap();
        let server = Server::start().await;
        let recorder = Arc::new(Recorder::default());
        *recorder.layout.lock().unwrap() = Some(layout.clone());
        let policy = Arc::new(Mutex::new(sources));
        let current = policy.clone();
        let executor = Self::executor(&layout, &server, &recorder, move || {
            Some((SourceGeneration(1), current.lock().unwrap().clone()))
        });
        Self {
            root,
            layout,
            server,
            recorder,
            executor,
            policy,
        }
    }
    pub fn executor(
        layout: &InstallLayout,
        server: &Server,
        recorder: &Arc<Recorder>,
        policy: impl PolicySource + 'static,
    ) -> InstallExecutor {
        InstallExecutor::new(
            Arc::new(InstalledStore::open(layout.clone()).unwrap()),
            Arc::new(Plaintext(PublicPackageTransport::new(server.client()))),
            recorder.clone(),
            Arc::new(policy),
            tokio::runtime::Handle::current(),
        )
    }
    pub async fn install(&self, plan: plans::PinnedPlan, approve: &[&str]) -> CatalogJob {
        let registry = PlanRegistry::new();
        let view = registry.insert(plan).unwrap();
        let approve: Vec<String> = approve.iter().map(|id| (*id).to_owned()).collect();
        let response = registry
            .confirm(&view.plan_id, &approve, prerequisites, &self.executor)
            .unwrap();
        self.executor.wait_idle().await;
        let job = registry.job(&response.job_id).unwrap();
        assert_eq!(job.phase, "finished");
        job
    }
    pub fn records(&self) -> InstalledDocument {
        installed::read_records(&self.layout).unwrap()
    }
    /// Nothing published and no unpublished tree left behind.
    pub fn assert_untouched(&self) {
        assert!(!self.layout.state.join(RECORDS_FILE).exists());
        assert!(!self.layout.state.join(JOURNAL_FILE).exists());
        for kind in [PackageKind::Plugin, PackageKind::Tool, PackageKind::Overlay] {
            let root = self.layout.root(kind);
            assert_eq!(
                std::fs::read_dir(root).map_or(0, Iterator::count),
                0,
                "{}",
                root.display()
            );
        }
        assert!(self.recorder.calls.lock().unwrap().is_empty());
    }
}

pub(super) fn dependency_graph(server: &Server) -> (Plugin, Plugin) {
    let mut dependency = plugin("acme.dep", "1.0.0", "in-process", json!([]));
    let mut root = plugin(
        "acme.root",
        "2.0.0",
        "in-process",
        json!([{"id":"field:acme.dep","version":"^1"}]),
    );
    dependency.publish(server, "/dep.zip");
    root.publish(server, "/root.zip");
    (dependency, root)
}

#[tokio::test]
async fn graph_commits_dependency_first_then_loads_only_committed_packages() {
    let fixture = Fixture::new(vec![record(OFFICIAL, true)]).await;
    let (dependency, root) = dependency_graph(&fixture.server);
    let catalog = catalog(&[&root, &dependency], &[]);
    let source = record(OFFICIAL, true);
    let job = fixture
        .install(
            plan(&catalog, &source, PackageKind::Plugin, "acme.root"),
            &[],
        )
        .await;
    assert_eq!(job.status, "completed", "{:?}", job.error);
    let records = fixture.records();
    assert_eq!(records.generation, 1);
    let ids: Vec<_> = records.records.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, ["acme.dep", "acme.root"]);
    assert_eq!(records.records[0].installed_for, InstalledFor::Dependency);
    assert_eq!(records.records[1].installed_for, InstalledFor::Requested);
    assert_eq!(records.records[1].source.as_str(), OFFICIAL);
    assert_eq!(
        records.records[1].sha256,
        package_hash(&root),
        "record pins the exact verified archive"
    );
    assert!(records.records.iter().all(|r| r.approval.is_none()));
    assert_eq!(
        *fixture.recorder.calls.lock().unwrap(),
        [
            ("acme.dep".to_owned(), true),
            ("acme.root".to_owned(), true)
        ]
    );
    assert_eq!(
        fixture.executor.load_reports(),
        [
            LoadReport {
                kind: InstalledKind::Plugin,
                id: "acme.dep".into(),
                status: LoadStatus::Loaded
            },
            LoadReport {
                kind: InstalledKind::Plugin,
                id: "acme.root".into(),
                status: LoadStatus::Loaded
            },
        ]
    );
    // Only the published package trees remain; downloads were private.
    assert_eq!(
        std::fs::read_dir(&fixture.layout.plugins).unwrap().count(),
        2
    );
    assert_eq!(
        std::fs::read_dir(&fixture.layout.downloads).map_or(0, Iterator::count),
        0
    );
    assert_eq!(fixture.recorder.web.load(Ordering::SeqCst), 0);
}

fn package_hash(plugin: &Plugin) -> String {
    plugin.entry["packages"][TARGET]["sha256"]
        .as_str()
        .unwrap()
        .to_owned()
}

#[tokio::test]
async fn dependency_prepared_but_root_failure_publishes_no_partial_graph() {
    let fixture = Fixture::new(vec![record(OFFICIAL, true)]).await;
    let (dependency, root) = dependency_graph(&fixture.server);
    // Same length, different bytes: only the root's hash check fails.
    let mut corrupt = root.archive();
    let last = corrupt.len() - 30;
    corrupt[last] ^= 0x55;
    fixture.server.serve("/root.zip", corrupt);
    let catalog = catalog(&[&root, &dependency], &[]);
    let job = fixture
        .install(
            plan(
                &catalog,
                &record(OFFICIAL, true),
                PackageKind::Plugin,
                "acme.root",
            ),
            &[],
        )
        .await;
    assert_eq!(job.status, "failed");
    assert_eq!(job.error.as_deref(), Some("package SHA-256 mismatch"));
    // The dependency really was fetched and prepared first.
    let requests = fixture.server.requests();
    assert!(requests[0].starts_with("GET /dep.zip "), "{requests:?}");
    assert!(requests[1].starts_with("GET /root.zip "), "{requests:?}");
    fixture.assert_untouched();
}

#[tokio::test]
async fn size_and_hash_mismatches_are_refused_before_publication() {
    for (label, mutate, expected) in [
        (
            "extra byte",
            Box::new(|bytes: &mut Vec<u8>| bytes.push(0)) as Box<dyn Fn(&mut Vec<u8>)>,
            "package size mismatch",
        ),
        (
            "short body",
            Box::new(|bytes: &mut Vec<u8>| {
                bytes.pop();
            }),
            "package size mismatch",
        ),
        (
            "same size",
            Box::new(|bytes: &mut Vec<u8>| bytes[10] ^= 1),
            "package SHA-256 mismatch",
        ),
    ] {
        let fixture = Fixture::new(vec![record(OFFICIAL, true)]).await;
        let mut dependency = plugin("acme.dep", "1.0.0", "in-process", json!([]));
        let mut bytes = dependency.publish(&fixture.server, "/dep.zip");
        mutate(&mut bytes);
        fixture.server.serve("/dep.zip", bytes);
        let catalog = catalog(&[&dependency], &[]);
        let job = fixture
            .install(
                plan(
                    &catalog,
                    &record(OFFICIAL, true),
                    PackageKind::Plugin,
                    "acme.dep",
                ),
                &[],
            )
            .await;
        assert_eq!(job.error.as_deref(), Some(expected), "{label}");
        fixture.assert_untouched();
    }
}

#[tokio::test]
async fn package_and_art_servers_never_receive_catalog_credentials() {
    use crate::catalog::{
        credentials::{CredentialInput, CredentialStore},
        forges::{CatalogFetchService, FetchContext},
        sources::SourceInput,
    };
    const USER: &str = "i0-user-sentinel-6b1d";
    const PASS: &str = "i0-pass-sentinel-93fe";
    let fixture = Fixture::new(Vec::new()).await;
    let config = fixture.root.path().join("config");
    let identity = format!(
        "https://{HOST}:{}/catalog-v2.json",
        fixture.server.socket.port()
    );
    let service = CatalogFetchService::open(&config).unwrap();
    service
        .sources()
        .replace(&[
            SourceInput {
                source: identity.clone(),
                enabled: true,
            },
            SourceInput {
                source: "control/no-credential".into(),
                enabled: true,
            },
        ])
        .unwrap();
    service
        .set_credential(
            &identity,
            CredentialInput::Basic {
                username: USER.into(),
                password: PASS.into(),
            },
        )
        .await
        .unwrap();
    // Package and listed artwork on the SAME origin as the credentialed catalog.
    let tool_manifest = json!({"manifest_version":1,"kind":"tool","id":"skill-tracker",
        "name":"Skill","version":"0.1.0","host":"^2","ui":{"root":"web"}});
    let package = zip(&[
        (
            "manifest.toml",
            toml::to_string(&tool_manifest).unwrap().as_bytes(),
        ),
        ("web/index.html", b"<p>tool</p>"),
    ]);
    fixture.server.serve("/tool.zip", package.clone());
    fixture
        .server
        .serve("/icon.png", b"\x89PNG\r\n\x1a\nart".to_vec());
    let tool_entry = json!({"id":"skill-tracker","name":"Skill","version":"0.1.0","host":"^2",
        "icon_url":fixture.server.url("/icon.png"),
        "packages":{"any":{"url":fixture.server.url("/tool.zip"),"sha256":sha(&package),"size":package.len()}}});
    let document = json!({"catalog_version":2,"name":"Private","published":"2026-11-24T00:00:00Z",
        "tools":[tool_entry]})
    .to_string()
    .into_bytes();
    let application = semver::Version::new(2, 1, 0);
    let mut authenticated = Vec::new();
    let snapshot = service
        .refresh_with(
            &fixture.server.client(),
            FetchContext {
                registrations: &[],
                revisions: &[],
                application: &application,
                target: TARGET,
            },
            &CancellationToken::new(),
            |source, auth| {
                authenticated.push((source.parsed.identity.as_str().to_owned(), auth.is_some()));
                std::future::ready(Ok(document.clone()))
            },
        )
        .await
        .unwrap();
    // Positive control: the exact catalog request was handed a scoped
    // credential, and the stored lease for that exact source is the sentinel
    // pair; the credential-free control source got none.
    assert_eq!(
        authenticated,
        [
            (identity.clone(), true),
            ("control/no-credential".into(), false)
        ]
    );
    let store = CredentialStore::open(&config);
    store
        .reconcile_sources(&[identity.clone(), "control/no-credential".into()])
        .unwrap();
    match store.lease(&identity).unwrap().unwrap().0.as_ref() {
        CredentialInput::Basic { username, password } => {
            assert_eq!((username.as_str(), password.as_str()), (USER, PASS));
        }
        CredentialInput::Token { .. } => panic!("sentinel kind"),
    }
    // Install through the real snapshot → plan → confirm → executor path.
    let candidate = snapshot.candidates().next().unwrap();
    let source = candidate.source.clone();
    let candidates = [Candidate {
        kind: candidate.kind,
        entry: candidate.entry,
        source: &source.identity,
        source_order: 0,
        catalog_order: 0,
        trusted: false,
    }];
    let builtins = BuiltinProviders::default();
    let plan = plans::build(
        &Snapshot {
            candidates: &candidates,
            installed: &[],
            builtins: &builtins,
            application: &application,
            target: TARGET,
            max_package_bytes: LIMIT,
        },
        &[Request {
            kind: PackageKind::Tool,
            id: "skill-tracker",
            source: &source.identity,
        }],
        &CurrentPolicy {
            generation: SourceGeneration(1),
            sources: std::slice::from_ref(&source),
        },
        prerequisites(),
    );
    let job = fixture.install(plan, &[]).await;
    assert_eq!(job.status, "completed", "{:?}", job.error);
    assert_eq!(fixture.recorder.web.load(Ordering::SeqCst), 1);
    // Listed artwork uses the art proxy's public `request_bytes` method.
    let art = fixture
        .server
        .client()
        .request_bytes(
            &fixture
                .server
                .url("/icon.png")
                .replacen("https://", "http://", 1),
            "vosu-catalog-art",
            &CancellationToken::new(),
        )
        .await
        .unwrap();
    assert_eq!(art.status, 200);
    let requests = fixture.server.requests();
    assert_eq!(requests.len(), 2, "{requests:?}");
    assert!(requests[0].starts_with("GET /tool.zip "));
    assert!(requests[1].starts_with("GET /icon.png "));
    // Recorder control: real header blocks are captured.
    assert!(
        requests[0]
            .to_ascii_lowercase()
            .contains("user-agent: vosu-catalog")
    );
    let basic = base64::Engine::encode(
        &base64::engine::general_purpose::STANDARD,
        format!("{USER}:{PASS}"),
    );
    for request in &requests {
        let lower = request.to_ascii_lowercase();
        assert!(!lower.contains("authorization"), "{request}");
        for sentinel in [USER, PASS, basic.as_str()] {
            assert!(!request.contains(sentinel), "{request}");
        }
    }
    // Nothing persisted or reported carries the sentinels either.
    let persisted = std::fs::read_to_string(fixture.layout.state.join(RECORDS_FILE)).unwrap();
    for sentinel in [USER, PASS, basic.as_str()] {
        assert!(!persisted.contains(sentinel));
        assert!(!format!("{:?}", fixture.executor.load_reports()).contains(sentinel));
    }
}

#[tokio::test]
async fn process_plugin_commits_with_exact_approval_before_handoff() {
    let user = "alice/plugins";
    let fixture = Fixture::new(vec![record(user, false)]).await;
    let mut process = plugin("acme.proc", "1.0.0", "process", json!([]));
    process.publish(&fixture.server, "/proc.zip");
    let catalog = catalog(&[&process], &[]);
    let source = record(user, false);
    let plan = plan(&catalog, &source, PackageKind::Plugin, "acme.proc");
    assert!(plan.view().adds[0].needs_approval);
    let job = fixture.install(plan, &["acme.proc"]).await;
    assert_eq!(job.status, "completed", "{:?}", job.error);
    let record = fixture.records().records.remove(0);
    let artifact = record.artifact.clone().unwrap();
    assert_eq!(artifact.runtime, Runtime::Process);
    assert!(record.matches_approval(record.process_digests.unwrap()));
    assert_eq!(
        *fixture.recorder.calls.lock().unwrap(),
        [("acme.proc".into(), true)]
    );
    assert_eq!(
        fixture.executor.load_reports()[0].status,
        LoadStatus::Loaded
    );
}

#[tokio::test]
async fn load_uses_current_policy_not_the_plan_time_trust_label() {
    let source = "alice/trusted";
    let fixture = Fixture::new(vec![record(source, true)]).await;
    let mut in_process = plugin("acme.native", "1.0.0", "in-process", json!([]));
    in_process.publish(&fixture.server, "/native.zip");
    let catalog = catalog(&[&in_process], &[]);
    let plan = plan(
        &catalog,
        &record(source, true),
        PackageKind::Plugin,
        "acme.native",
    );
    // Trust revoked after confirmation: the committed package must not load.
    *fixture.policy.lock().unwrap() = vec![record(source, false)];
    let job = fixture.install(plan, &[]).await;
    assert_eq!(job.status, "completed", "{:?}", job.error);
    assert!(fixture.recorder.calls.lock().unwrap().is_empty());
    assert_eq!(
        fixture.executor.load_reports()[0].status,
        LoadStatus::Refused(PolicyError::InProcessDenied)
    );
}

#[tokio::test]
async fn cancellation_during_download_publishes_nothing_and_finishes_the_job() {
    let fixture = Fixture::new(vec![record(OFFICIAL, true)]).await;
    let (dependency, root) = dependency_graph(&fixture.server);
    fixture.server.hold("/root.zip");
    let catalog = catalog(&[&root, &dependency], &[]);
    let registry = PlanRegistry::new();
    let view = registry
        .insert(plan(
            &catalog,
            &record(OFFICIAL, true),
            PackageKind::Plugin,
            "acme.root",
        ))
        .unwrap();
    let response = registry
        .confirm(&view.plan_id, &[], prerequisites, &fixture.executor)
        .unwrap();
    // Barrier: wait until the held root request reached the server.
    while fixture.server.requests().len() < 2 {
        fixture.server.received.notified().await;
    }
    assert_eq!(registry.job(&response.job_id).unwrap().status, "running");
    // The held response is never released: only cancellation can end it.
    fixture.executor.shutdown().await;
    let job = registry.job(&response.job_id).unwrap();
    assert_eq!(
        (job.status.as_str(), job.error.as_deref()),
        ("failed", Some("catalog install cancelled"))
    );
    fixture.assert_untouched();
    // The gate was released and a shut-down executor refuses new work.
    let again = registry
        .insert(plan(
            &catalog,
            &record(OFFICIAL, true),
            PackageKind::Plugin,
            "acme.root",
        ))
        .unwrap();
    assert_eq!(
        registry.confirm(&again.plan_id, &[], prerequisites, &fixture.executor),
        Err(plans::PlanError::Executor)
    );
}

#[test]
fn installed_for_promotes_requests_and_never_demotes() {
    use crate::catalog::plans::Reason;
    assert_eq!(
        InstalledFor::merge(None, Reason::Dependency),
        InstalledFor::Dependency
    );
    assert_eq!(
        InstalledFor::merge(None, Reason::Requested),
        InstalledFor::Requested
    );
    assert_eq!(
        InstalledFor::merge(Some(InstalledFor::Dependency), Reason::Requested),
        InstalledFor::Requested
    );
    assert_eq!(
        InstalledFor::merge(Some(InstalledFor::Requested), Reason::Dependency),
        InstalledFor::Requested
    );
}
