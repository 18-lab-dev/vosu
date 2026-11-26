//! I0 composition through the public core API: real P0 plans/confirmation,
//! the real executor, SDK verification, ZIP preflight and journaled commit.
//! The transport is a recording in-memory `PackageTransport`; wire-level
//! credential absence over the real streaming client is covered by
//! `vosu-core` `catalog::install::tests` (exact loopback harness).
use semver::Version;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::Write as _,
    path::Path,
    sync::{Arc, Mutex},
};
use tokio_util::sync::CancellationToken;
use vosu_core::catalog::{
    install::{
        DownloadFuture, HandoffFuture, InstallExecutor, InstallLayout, InstalledFor,
        InstalledStore, LoadRequest, LoadStatus, PackageTransport, RuntimeHandoff,
    },
    plans::{self, CatalogJob, PlanRegistry, Prerequisites},
    resolver::{BuiltinProviders, Candidate, Request, Snapshot},
    sources::{SourceGeneration, SourceIdentity, SourceRecord},
    trust::CurrentPolicy,
};
use vosu_plugin_api::{catalog::Catalog, manifest::PackageKind};

pub(crate) const TARGET: &str = "x86_64-unknown-linux-gnu";
pub(crate) const OFFICIAL: &str = "18-lab-dev/vosu-tools";

#[derive(Default)]
pub(crate) struct Memory {
    packages: Mutex<HashMap<String, Vec<u8>>>,
    requests: Mutex<Vec<(String, u64)>>,
}
impl PackageTransport for Memory {
    fn download<'a>(
        &'a self,
        url: &'a str,
        max_bytes: u64,
        destination: &'a Path,
        _: &'a CancellationToken,
    ) -> DownloadFuture<'a> {
        self.requests
            .lock()
            .unwrap()
            .push((url.to_owned(), max_bytes));
        let body = self.packages.lock().unwrap().get(url).cloned();
        Box::pin(async move {
            let body = body.ok_or(vosu_net::NetworkError::Status(404))?;
            if body.len() as u64 > max_bytes {
                return Err(vosu_net::NetworkError::TooLarge(max_bytes));
            }
            std::fs::write(destination, body)?;
            Ok(())
        })
    }
}

/// Records each in-process load with whether the graph was already
/// durably committed (records published, journal gone) at that moment.
pub(crate) struct Handoff {
    pub(crate) state: std::path::PathBuf,
    pub(crate) loads: Mutex<Vec<(String, bool)>>,
}
impl RuntimeHandoff for Handoff {
    fn load_in_process<'a>(
        &'a self,
        request: LoadRequest<'a>,
    ) -> HandoffFuture<'a, Result<(), String>> {
        let records =
            std::fs::read_to_string(self.state.join("catalog-installed.json")).unwrap_or_default();
        let committed = !self.state.join("catalog-install-journal.json").exists()
            && records.contains(&format!("\"id\":\"{}\"", request.record.id))
            && request.directory.join("manifest.toml").is_file();
        self.loads
            .lock()
            .unwrap()
            .push((request.record.id.clone(), committed));
        Box::pin(async { Ok(()) })
    }
    fn rescan_web(&self) -> HandoffFuture<'_, ()> {
        Box::pin(async {})
    }
}

fn zip(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut cursor = std::io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        for (name, body) in entries {
            writer
                .start_file(name.as_str(), zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

pub(crate) struct Package {
    files: Vec<(String, Vec<u8>)>,
    pub(crate) entry: Value,
}
fn host() -> String {
    format!("={}", vosu::BUNDLE.version)
}
pub(crate) fn package(id: &str, version: &str, runtime: &str, requires: Value) -> Package {
    let library = format!("lib/{id}.so");
    let manifest = json!({"manifest_version":1,"kind":"plugin","id":id,"name":id,"version":version,
        "host":host(),"abi_version":1,"runtime":runtime,"targets":{TARGET:{"library":library}},
        "provides":[{"id":format!("field:{id}"),"version":"1.0.0","schema":"schema.json"}],
        "requires":requires});
    let schema = json!({"title":id,"version":"1.0.0","type":"object","properties":{
        "combo":{"x-sampling-lane":{"lane":"regular","activity":"gameplay"}}}});
    Package {
        files: vec![
            (
                "manifest.toml".into(),
                toml::to_string(&manifest).unwrap().into_bytes(),
            ),
            ("schema.json".into(), schema.to_string().into_bytes()),
            (library, b"fixture".to_vec()),
        ],
        entry: json!({"id":id,"name":id,"version":version,"host":host(),"runtime":runtime,
            "provides":[{"id":format!("field:{id}"),"version":"1.0.0","schema_version":"1.0.0",
                "lanes":[{"pointer":"/combo","lane":"regular","activity":"gameplay"}]}],
            "requires":manifest["requires"].clone(),"packages":{}}),
    }
}
impl Package {
    pub(crate) fn publish(&mut self, memory: &Memory, archive: Option<Vec<u8>>) {
        let bytes = zip(&self.files);
        let url = format!(
            "https://packages.example.org/{}.zip",
            self.entry["id"].as_str().unwrap()
        );
        self.entry["packages"] = json!({TARGET:{"url":url,
            "sha256":Sha256::digest(&bytes).iter().map(|b| format!("{b:02x}")).collect::<String>(),
            "size":bytes.len()}});
        memory
            .packages
            .lock()
            .unwrap()
            .insert(url, archive.unwrap_or(bytes));
    }
}

pub(crate) fn prerequisites() -> Prerequisites {
    Prerequisites {
        source_generation: 1,
        trust_generation: 1,
        private_generation: 1,
        credential_generation: 1,
        installed_generation: 0,
        limit_generation: 1,
        snapshot_fingerprint: [9; 32],
        application: vosu::BUNDLE.version.to_owned(),
        target: TARGET.into(),
        max_package_bytes: 512 * 1024 * 1024,
    }
}
pub(crate) fn source(identity: &str, trusted: bool) -> SourceRecord {
    SourceRecord {
        identity: SourceIdentity::parse(identity).unwrap().identity,
        enabled: true,
        trusted,
        private_network: false,
        order: 0,
    }
}
fn plan(packages: &[&Package], record: &SourceRecord, request: &str) -> plans::PinnedPlan {
    let catalog = Catalog::parse(
        &json!({"catalog_version":2,"name":"Install","published":"informational",
            "plugins":packages.iter().map(|p| p.entry.clone()).collect::<Vec<_>>()})
        .to_string(),
    )
    .unwrap();
    let candidates: Vec<_> = catalog
        .plugins
        .iter()
        .enumerate()
        .map(|(index, entry)| Candidate {
            kind: PackageKind::Plugin,
            entry,
            source: &record.identity,
            source_order: 0,
            catalog_order: index,
            trusted: record.trusted,
        })
        .collect();
    let builtins = BuiltinProviders::from_registry(
        &vosu_core::state::schema::SchemaRegistry::with_core().unwrap(),
        &[],
    );
    let application: &Version = &vosu::BUNDLE.version();
    let plan = plans::build(
        &Snapshot {
            candidates: &candidates,
            installed: &[],
            builtins: &builtins,
            application,
            target: TARGET,
            max_package_bytes: 512 * 1024 * 1024,
        },
        &[Request {
            kind: PackageKind::Plugin,
            id: request,
            source: &record.identity,
        }],
        &CurrentPolicy {
            generation: SourceGeneration(1),
            sources: std::slice::from_ref(record),
        },
        prerequisites(),
    );
    assert!(plan.view().errors.is_empty(), "{:?}", plan.view().errors);
    plan
}

struct Fixture {
    _root: tempfile::TempDir,
    layout: InstallLayout,
    memory: Arc<Memory>,
    handoff: Arc<Handoff>,
    executor: InstallExecutor,
}
impl Fixture {
    fn new(policy: SourceRecord) -> Self {
        let root = tempfile::tempdir().unwrap();
        let base = root.path();
        let layout = InstallLayout::new(
            base.join("state"),
            base.join("plugins"),
            base.join("tools"),
            base.join("overlays"),
            base.join("downloads"),
        )
        .unwrap();
        let memory = Arc::new(Memory::default());
        let handoff = Arc::new(Handoff {
            state: layout.state.clone(),
            loads: Mutex::default(),
        });
        let current = move || Some((SourceGeneration(1), vec![policy.clone()]));
        let executor = InstallExecutor::new(
            Arc::new(InstalledStore::open(layout.clone()).unwrap()),
            memory.clone(),
            handoff.clone(),
            Arc::new(current),
            tokio::runtime::Handle::current(),
        );
        Self {
            _root: root,
            layout,
            memory,
            handoff,
            executor,
        }
    }
    async fn confirm(&self, plan: plans::PinnedPlan, approve: &[&str]) -> CatalogJob {
        let registry = PlanRegistry::new();
        let view = registry.insert(plan).unwrap();
        let approve: Vec<String> = approve.iter().map(|a| (*a).to_owned()).collect();
        let response = registry
            .confirm(&view.plan_id, &approve, prerequisites, &self.executor)
            .unwrap();
        self.executor.wait_idle().await;
        registry.job(&response.job_id).unwrap()
    }
    fn published(&self) -> Vec<String> {
        let mut names: Vec<_> = std::fs::read_dir(&self.layout.plugins)
            .map(|entries| {
                entries
                    .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }
}

#[tokio::test]
async fn install_commits_dependency_first_and_loads_only_after_commit() {
    let official = source(OFFICIAL, true);
    let fixture = Fixture::new(official.clone());
    let mut dependency = package("acme.base", "1.0.0", "in-process", json!([]));
    let mut root = package(
        "acme.app",
        "3.0.0",
        "in-process",
        json!([{"id":"field:acme.base","version":"^1"}]),
    );
    dependency.publish(&fixture.memory, None);
    root.publish(&fixture.memory, None);
    let job = fixture
        .confirm(plan(&[&root, &dependency], &official, "acme.app"), &[])
        .await;
    assert_eq!((job.status.as_str(), job.error), ("completed", None));
    // Independent expectation: dependency-first, exact pinned cap per request.
    let requests = fixture.memory.requests.lock().unwrap().clone();
    let urls: Vec<_> = requests.iter().map(|(url, _)| url.as_str()).collect();
    assert_eq!(
        urls,
        [
            "https://packages.example.org/acme.base.zip",
            "https://packages.example.org/acme.app.zip"
        ]
    );
    assert_eq!(fixture.published(), ["acme.app", "acme.base"]);
    assert_eq!(
        *fixture.handoff.loads.lock().unwrap(),
        [
            ("acme.base".to_owned(), true),
            ("acme.app".to_owned(), true)
        ]
    );
    let store = InstalledStore::open(fixture.layout.clone()).unwrap();
    let document = store.snapshot();
    let origins: Vec<_> = document
        .records
        .iter()
        .map(|r| (r.id.as_str(), r.installed_for))
        .collect();
    assert_eq!(
        origins,
        [
            ("acme.base", InstalledFor::Dependency),
            ("acme.app", InstalledFor::Requested)
        ]
    );
}

#[tokio::test]
async fn install_root_failure_after_dependency_prepared_publishes_nothing() {
    let official = source(OFFICIAL, true);
    let fixture = Fixture::new(official.clone());
    let mut dependency = package("acme.base", "1.0.0", "in-process", json!([]));
    let mut root = package(
        "acme.app",
        "3.0.0",
        "in-process",
        json!([{"id":"field:acme.base","version":"^1"}]),
    );
    dependency.publish(&fixture.memory, None);
    let mut wrong = zip(&root.files);
    *wrong.last_mut().unwrap() ^= 1;
    root.publish(&fixture.memory, Some(wrong));
    let job = fixture
        .confirm(plan(&[&root, &dependency], &official, "acme.app"), &[])
        .await;
    assert_eq!(job.error.as_deref(), Some("package SHA-256 mismatch"));
    assert_eq!(fixture.memory.requests.lock().unwrap().len(), 2);
    assert!(fixture.published().is_empty());
    assert!(fixture.handoff.loads.lock().unwrap().is_empty());
    assert!(!fixture.layout.state.join("catalog-installed.json").exists());
}

#[tokio::test]
async fn install_refuses_mirror_mismatch_and_windows_collisions() {
    let official = source(OFFICIAL, true);
    for (label, mutate, expected) in [
        (
            "version",
            Box::new(|p: &mut Package| {
                let mut manifest: toml::Value =
                    toml::from_str(std::str::from_utf8(&p.files[0].1).unwrap()).unwrap();
                manifest["version"] = toml::Value::String("1.0.1".into());
                p.files[0].1 = toml::to_string(&manifest).unwrap().into_bytes();
            }) as Box<dyn Fn(&mut Package)>,
            "package differs from its catalog entry: catalog.mirror at /version",
        ),
        (
            "lane",
            Box::new(|p: &mut Package| {
                p.entry["provides"][0]["lanes"][0]["activity"] = json!("always");
            }),
            "package differs from its catalog entry: catalog.mirror at /provides/0/lanes",
        ),
        (
            "case collision",
            Box::new(|p: &mut Package| p.files.push(("Schema.json".into(), b"{}".to_vec()))),
            "unsafe package archive: portable name collision",
        ),
        (
            "device name",
            Box::new(|p: &mut Package| p.files.push(("nul.txt".into(), b"x".to_vec()))),
            "unsafe package archive: nonportable entry name",
        ),
    ] {
        let fixture = Fixture::new(official.clone());
        let mut base = package("acme.base", "1.0.0", "in-process", json!([]));
        mutate(&mut base);
        base.publish(&fixture.memory, None);
        let job = fixture
            .confirm(plan(&[&base], &official, "acme.base"), &[])
            .await;
        assert_eq!(job.error.as_deref(), Some(expected), "{label}");
        assert!(fixture.published().is_empty(), "{label}");
    }
}

#[tokio::test]
async fn install_process_plugin_commits_with_approval_but_requires_m5() {
    let user = source("alice/plugins", false);
    let fixture = Fixture::new(user.clone());
    let mut process = package("acme.proc", "1.0.0", "process", json!([]));
    process.publish(&fixture.memory, None);
    let job = fixture
        .confirm(plan(&[&process], &user, "acme.proc"), &["acme.proc"])
        .await;
    assert_eq!(job.status, "completed", "{:?}", job.error);
    assert!(fixture.handoff.loads.lock().unwrap().is_empty());
    let reports = fixture.executor.load_reports();
    assert_eq!(reports[0].status, LoadStatus::RequiresM5ProcessRuntime);
    let document = InstalledStore::open(fixture.layout.clone())
        .unwrap()
        .snapshot();
    let record = &document.records[0];
    assert!(
        record
            .approval
            .as_ref()
            .is_some_and(|approval| approval.matches(record.artifact.as_ref().unwrap()))
    );
}
