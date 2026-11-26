//! M6b K0 root composition through the real `vosu_core::runtime::CatalogComposition`:
//! S0/C0 open, I0 recovery/executor/store, the root runtime handoff with a
//! real `dlopen` of the testkit cdylib into a real `PluginHost`, G0 offers
//! observing the actual `DiscoveryService`, T0 revocation and shutdown join.
//! Packages come from an in-memory `PackageTransport`; wire-level public-only
//! package requests are covered by `vosu-core` `catalog::install::tests`.
#![cfg(feature = "test-harness")]
#![expect(
    clippy::disallowed_methods,
    reason = "test fixtures write scratch roots and packages"
)]

use semver::Version;
use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::HashMap,
    io::{self, Write as _},
    path::Path,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::sync::Notify;
use tokio_util::sync::CancellationToken;
use vosu_core::runtime::{CatalogComposition, CatalogRoots, catalog_target};
use vosu_core::{
    catalog::{
        install::{DownloadFuture, InstallLayout, JOURNAL_FILE, LoadStatus, PackageTransport},
        plans::{self, CatalogJob, Prerequisites},
        resolver::{BuiltinProviders, Candidate, Request, Snapshot},
        runtime::CatalogRuntime,
        sources::{FetchedCatalog, SourceIdentity, SourceInput, SourceRecord},
        trust::{CurrentPolicy, PolicyError},
    },
    logging::{ConsoleSink, Logger, LoggerOptions},
    plugin::{
        DiscoveryService, PluginHost, Prefilter, ProcessEventBatch, ProcessRecord, ProcessSource,
        settings::{PluginSettingsService, SettingsError, SettingsStorage},
    },
};
use vosu_plugin_api::{Bitness, catalog::Catalog, manifest::PackageKind};

const OFFICIAL: &str = "18-lab-dev/vosu-tools";
const USER: &str = "alice/plugins";
const WAIT: Duration = Duration::from_secs(20);

struct Storage;
impl SettingsStorage for Storage {
    fn load(&self, _: &str) -> Result<Map<String, Value>, SettingsError> {
        Ok(Map::new())
    }
    fn store(&self, _: &str, _: &Map<String, Value>) -> Result<(), SettingsError> {
        Ok(())
    }
}
struct Console;
impl ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _: &str) -> io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> io::Result<()> {
        Ok(())
    }
}

/// In-memory packages. A `held` URL blocks until cancelled, then until
/// `release` permits, so shutdown provably waits for the worker.
#[derive(Default)]
struct Memory {
    packages: Mutex<HashMap<String, Vec<u8>>>,
    held: Mutex<Option<String>>,
    entered: Notify,
    cancelled: Notify,
    release: Notify,
}
impl PackageTransport for Memory {
    fn download<'a>(
        &'a self,
        url: &'a str,
        max_bytes: u64,
        destination: &'a Path,
        cancel: &'a CancellationToken,
    ) -> DownloadFuture<'a> {
        let body = self.packages.lock().unwrap().get(url).cloned();
        let held = self.held.lock().unwrap().as_deref() == Some(url);
        Box::pin(async move {
            if held {
                self.entered.notify_one();
                cancel.cancelled().await;
                self.cancelled.notify_one();
                self.release.notified().await;
                return Err(vosu_net::NetworkError::Cancelled);
            }
            let body = body.ok_or(vosu_net::NetworkError::Status(404))?;
            if body.len() as u64 > max_bytes {
                return Err(vosu_net::NetworkError::TooLarge(max_bytes));
            }
            std::fs::write(destination, body)?;
            Ok(())
        })
    }
}

fn testkit_cdylib() -> (String, Vec<u8>) {
    let name = if cfg!(windows) {
        "vosu_plugin_testkit.dll"
    } else {
        "libvosu_plugin_testkit.so"
    };
    let path = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join(name);
    assert!(
        path.is_file(),
        "run with --features test-harness so the testkit cdylib is built beside {path:?}"
    );
    (name.to_owned(), std::fs::read(path).unwrap())
}

fn zip(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut cursor = io::Cursor::new(Vec::new());
    {
        let mut writer = zip::ZipWriter::new(&mut cursor);
        for (name, body) in files {
            writer
                .start_file(name.as_str(), zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
    }
    cursor.into_inner()
}

/// One real in-process package around the testkit echo cdylib. `schema`
/// adds the plugin's own namespace schema (`x-visibility` on `/internal`).
fn package(memory: &Memory, id: &str, schema: Option<&str>) -> Value {
    let (library, bytes) = testkit_cdylib();
    let host = format!("={}", vosu::BUNDLE.version);
    let target = catalog_target();
    let mut manifest = json!({"manifest_version":1,"kind":"plugin","id":id,"name":id,
        "version":"1.0.0","host":host,"abi_version":1,"runtime":"in-process",
        "targets":{target:{"library":format!("lib/{library}")}}});
    let mut entry = json!({"id":id,"name":id,"version":"1.0.0","host":host,
        "runtime":"in-process"});
    let mut files = vec![(format!("lib/{library}"), bytes)];
    if let Some(schema) = schema {
        manifest["provides"] =
            json!([{"id":format!("field:{id}"),"version":"1.1.0","schema":"schema.json"}]);
        entry["provides"] = json!([{"id":format!("field:{id}"),"version":"1.1.0",
            "schema_version":"1.1.0","lanes":[]}]);
        files.push(("schema.json".into(), schema.as_bytes().to_vec()));
    }
    files.push((
        "manifest.toml".into(),
        toml::to_string(&manifest).unwrap().into_bytes(),
    ));
    let archive = zip(&files);
    let url = format!("https://packages.example.org/{id}.zip");
    entry["packages"] = json!({target:{"url":url,"size":archive.len(),
        "sha256":Sha256::digest(&archive).iter().map(|b| format!("{b:02x}")).collect::<String>()}});
    memory.packages.lock().unwrap().insert(url, archive);
    entry
}

fn plugin_host() -> (tempfile::TempDir, Arc<PluginHost>) {
    let logs = tempfile::tempdir().unwrap();
    let logger = Logger::new(
        logs.path(),
        LoggerOptions {
            version: "0.1.0".into(),
            debug_log: false,
            queue_capacity: 64,
        },
        Arc::new(Console),
    )
    .unwrap();
    let host = PluginHost::new(
        logger,
        Arc::new(PluginSettingsService::new(Arc::new(Storage))),
        tokio::runtime::Handle::current(),
    );
    (logs, host)
}

fn roots(base: &Path) -> CatalogRoots {
    CatalogRoots {
        config: base.join("config"),
        layout: InstallLayout::new(
            base.join("catalog"),
            base.join("catalog/plugins"),
            base.join("tools"),
            base.join("static"),
            base.join("downloads"),
        ),
        plugin_data: base.join("plugin-data"),
    }
}

fn identity(source: &str) -> SourceIdentity {
    SourceIdentity::parse(source).unwrap().identity
}

/// The exact prerequisites a composition would pin, from its live state.
fn prerequisites(runtime: &CatalogRuntime) -> Prerequisites {
    let (generation, _) = runtime.fetch().unwrap().sources().snapshot().unwrap();
    Prerequisites {
        source_generation: generation.0,
        trust_generation: generation.0,
        private_generation: generation.0,
        credential_generation: generation.0,
        installed_generation: runtime.installed_generation(),
        limit_generation: 1,
        snapshot_fingerprint: [7; 32],
        application: vosu::BUNDLE.version.to_owned(),
        target: catalog_target().into(),
        max_package_bytes: 512 * 1024 * 1024,
    }
}

fn plan(
    runtime: &CatalogRuntime,
    entries: &[Value],
    source: &str,
    request: &str,
) -> plans::PinnedPlan {
    let catalog = Catalog::parse(
        &json!({"catalog_version":2,"name":"K0","published":"informational","plugins":entries})
            .to_string(),
    )
    .unwrap();
    let (generation, records) = runtime.fetch().unwrap().sources().snapshot().unwrap();
    let record: &SourceRecord = records
        .iter()
        .find(|record| record.identity.as_str() == source)
        .unwrap();
    let candidates: Vec<_> = catalog
        .plugins
        .iter()
        .enumerate()
        .map(|(index, entry)| Candidate {
            kind: PackageKind::Plugin,
            entry,
            source: &record.identity,
            source_order: record.order,
            catalog_order: index,
            trusted: record.trusted,
        })
        .collect();
    let builtins = BuiltinProviders::from_registry(
        &vosu_core::state::schema::SchemaRegistry::with_core().unwrap(),
        &[],
    );
    let application: &Version = &vosu::BUNDLE.version();
    plans::build(
        &Snapshot {
            candidates: &candidates,
            installed: &[],
            builtins: &builtins,
            application,
            target: catalog_target(),
            max_package_bytes: 512 * 1024 * 1024,
        },
        &[Request {
            kind: PackageKind::Plugin,
            id: request,
            source: &record.identity,
        }],
        &CurrentPolicy {
            generation,
            sources: &records,
        },
        prerequisites(runtime),
    )
}

async fn install(runtime: &CatalogRuntime, plan: plans::PinnedPlan) -> CatalogJob {
    assert!(plan.view().errors.is_empty(), "{:?}", plan.view().errors);
    let view = runtime.plans().insert(plan).unwrap();
    let executor = runtime.executor().unwrap();
    let pinned = prerequisites(runtime);
    let response = runtime
        .plans()
        .confirm(&view.plan_id, &[], || pinned, executor.as_ref())
        .unwrap();
    executor.wait_idle().await;
    runtime.plans().job(&response.job_id).unwrap()
}

fn live(host: &PluginHost, id: &str) -> bool {
    host.validation_snapshot()
        .providers
        .iter()
        .any(|provider| provider.id == id)
}

fn sources(user: bool) -> Vec<SourceInput> {
    vosu_core::catalog::sources::OFFICIAL_SOURCES
        .iter()
        .copied()
        .chain(user.then_some(USER))
        .map(|source| SourceInput {
            source: source.into(),
            enabled: true,
        })
        .collect()
}

const INTERNAL: &str = r#"{"title":"vosu.osu","version":"1.1.0","type":"object","properties":{"internal":{"type":"object","x-visibility":"plugins"}}}"#;

#[tokio::test(flavor = "multi_thread")]
async fn k0_official_x_visibility_plugin_installs_registers_and_user_source_is_refused() {
    let base = tempfile::tempdir().unwrap();
    let (_logs, host) = plugin_host();
    let memory = Arc::new(Memory::default());
    let catalog = CatalogComposition::start(
        Arc::clone(&host),
        roots(base.path()),
        memory.clone(),
        &vosu::BUNDLE,
    )
    .await
    .unwrap();
    let runtime = catalog.services();
    let entry = package(&memory, "vosu.osu", Some(INTERNAL));
    let job = install(
        runtime,
        plan(runtime, std::slice::from_ref(&entry), OFFICIAL, "vosu.osu"),
    )
    .await;
    assert_eq!((job.status.as_str(), job.error), ("completed", None));
    let reports = runtime.executor().unwrap().load_reports();
    assert_eq!(reports[0].status, LoadStatus::Loaded);
    assert!(live(&host, "vosu.osu"));
    // Registered with official provenance: its `/internal` subtree is hidden.
    {
        let registry = host.state().registry.read().unwrap();
        let schema = registry
            .get("vosu.osu")
            .expect("x-visibility schema registered");
        assert_eq!(schema.version, Version::new(1, 1, 0));
        assert_eq!(registry.hidden_subtrees("vosu.osu").len(), 1);
    }

    // The same plugin from an untrusted user source never reaches install
    // or load: no in-process downgrade, no executable plan.
    runtime.replace_sources(sources(true)).await.unwrap();
    let refused = plan(runtime, &[entry], USER, "vosu.osu");
    assert!(refused.view().adds.is_empty());
    assert!(!refused.view().errors.is_empty());
    catalog.shutdown().await;
    tokio::task::spawn_blocking(move || host.destroy_all(Duration::from_secs(5)))
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn k0_untrust_blocks_the_next_load_until_restart_then_retrust_loads() {
    let base = tempfile::tempdir().unwrap();
    let memory = Arc::new(Memory::default());
    let (_logs, host) = plugin_host();
    let catalog = CatalogComposition::start(
        Arc::clone(&host),
        roots(base.path()),
        memory.clone(),
        &vosu::BUNDLE,
    )
    .await
    .unwrap();
    let runtime = catalog.services();
    runtime.replace_sources(sources(true)).await.unwrap();
    runtime.set_trust(identity(USER), true).await.unwrap();
    let entry = package(&memory, "acme.echo", None);
    let job = install(runtime, plan(runtime, &[entry], USER, "acme.echo")).await;
    assert_eq!(job.status, "completed", "{:?}", job.error);
    assert!(live(&host, "acme.echo"));

    let revoked = runtime.set_trust(identity(USER), false).await.unwrap();
    assert_eq!(revoked.pending_restart, ["acme.echo"]);
    runtime.set_trust(identity(USER), true).await.unwrap();
    let reports = runtime.executor().unwrap().load_committed().await;
    assert_eq!(
        reports[0].status,
        LoadStatus::Refused(PolicyError::RevocationPendingRestart)
    );
    runtime.set_trust(identity(USER), false).await.unwrap();
    catalog.shutdown().await;
    tokio::task::spawn_blocking(move || host.destroy_all(Duration::from_secs(5)))
        .await
        .unwrap();

    // Restart: the persisted untrust blocks the startup load...
    let (_logs, host) = plugin_host();
    let catalog = CatalogComposition::start(
        Arc::clone(&host),
        roots(base.path()),
        memory.clone(),
        &vosu::BUNDLE,
    )
    .await
    .unwrap();
    let runtime = catalog.services();
    assert_eq!(
        runtime.executor().unwrap().load_reports()[0].status,
        LoadStatus::Refused(PolicyError::InProcessDenied)
    );
    assert!(!live(&host, "acme.echo") && runtime.pending_restart().is_empty());
    // ...and a fresh daemon may load it again once the user re-trusts.
    runtime.set_trust(identity(USER), true).await.unwrap();
    let reports = runtime.executor().unwrap().load_committed().await;
    assert_eq!(reports[0].status, LoadStatus::Loaded);
    assert!(live(&host, "acme.echo"));
    catalog.shutdown().await;
    tokio::task::spawn_blocking(move || host.destroy_all(Duration::from_secs(5)))
        .await
        .unwrap();
}

/// Scripted process source for the real discovery service.
#[derive(Default)]
struct Processes {
    records: Mutex<Vec<ProcessRecord>>,
    reconcile: AtomicBool,
    notify: Notify,
}
impl ProcessSource for Processes {
    fn enumerate(&self) -> io::Result<Vec<ProcessRecord>> {
        Ok(self.records.lock().unwrap().clone())
    }
    fn drain_events(&self) -> ProcessEventBatch {
        ProcessEventBatch {
            needs_reconcile: self.reconcile.swap(false, Ordering::SeqCst),
            ..ProcessEventBatch::default()
        }
    }
    fn set_prefilter(&self, _: &Prefilter) -> io::Result<()> {
        Ok(())
    }
    fn notifier(&self) -> Option<&Notify> {
        Some(&self.notify)
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn k0_recovers_before_discovery_offers_observe_discovery_and_shutdown_joins() {
    let base = tempfile::tempdir().unwrap();
    let layout = roots(base.path()).layout.unwrap();
    // Interrupted commit: journal durable, new tree renamed into place,
    // installed records never published.
    let half = layout.plugins.join("acme.half");
    std::fs::create_dir_all(&half).unwrap();
    std::fs::write(half.join("manifest.toml"), b"partial").unwrap();
    std::fs::create_dir_all(&layout.state).unwrap();
    let journal = layout.state.join(JOURNAL_FILE);
    std::fs::write(
        &journal,
        serde_json::to_vec(
            &json!({"format":1,"previous_generation":0,"next_generation":1,
            "operations":[{"staging":layout.plugins.join(".stage-half"),"destination":half,
            "backup":null}]}),
        )
        .unwrap(),
    )
    .unwrap();
    let (_logs, host) = plugin_host();
    let memory = Arc::new(Memory::default());
    let catalog = CatalogComposition::start(
        Arc::clone(&host),
        roots(base.path()),
        memory.clone(),
        &vosu::BUNDLE,
    )
    .await
    .unwrap();
    // Recovery completed inside start, before discovery exists.
    assert!(!journal.exists() && !half.exists());
    let runtime = catalog.services();
    assert_eq!(runtime.installed_generation(), 0);

    // G0 is the host's observer: publish one game entry, then let the real
    // discovery service report a matching process.
    runtime
        .replace_sources(vec![SourceInput {
            source: "acme/games".into(),
            enabled: true,
        }])
        .await
        .unwrap();
    let store = runtime.fetch().unwrap().sources();
    let ticket = store.begin_fetch().unwrap();
    let game = json!({"catalog_version":2,"name":"Games","published":"informational",
        "plugins":[{"id":"acme.game","name":"Acme game","version":"3.1.0",
            "host":format!("={}", vosu::BUNDLE.version),
            "runtime":"process","provides":[{"id":"field:acme.game","version":"1.0.0",
                "schema_version":"1.0.0","lanes":[]},{"id":"game:mygame","version":"*"}],
            "matcher":{"process_names":["mygame"]},
            "packages":{catalog_target():{"url":"https://packages.test/acme.game.zip",
                "sha256":"ef".repeat(32),"size":2048}}}]});
    store
        .complete_fetch(
            ticket,
            vec![FetchedCatalog {
                source: identity("acme/games"),
                result: Ok(game.to_string().into_bytes()),
            }],
            &[],
            &[],
            &vosu::BUNDLE.version(),
            catalog_target(),
        )
        .unwrap();
    runtime.refresh_offers();
    let processes = Arc::new(Processes::default());
    *processes.records.lock().unwrap() = vec![ProcessRecord {
        pid: 4242,
        name: "mygame".into(),
        path: "/opt/mygame/mygame".into(),
        command_line: "mygame".into(),
        bitness: Bitness::X64,
        start_time: 9,
        argv: None,
    }];
    let service =
        DiscoveryService::start(&host, processes.clone(), tokio::runtime::Handle::current());
    let deadline = Instant::now() + WAIT;
    while runtime.offer_views().is_empty() {
        assert!(Instant::now() < deadline, "discovery never reached G0");
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let offers = runtime.offer_views();
    assert_eq!(
        (offers[0].id.as_str(), offers[0].process.as_str()),
        ("acme.game", "mygame")
    );
    tokio::task::spawn_blocking(move || service.stop())
        .await
        .unwrap();

    // Shutdown joins the executor: the worker is inside a download that
    // ignores nothing but cancellation and then waits for `release`.
    runtime.replace_sources(sources(false)).await.unwrap();
    let entry = package(&memory, "acme.held", None);
    *memory.held.lock().unwrap() = Some("https://packages.example.org/acme.held.zip".into());
    let plan = plan(runtime, &[entry], OFFICIAL, "acme.held");
    let view = runtime.plans().insert(plan).unwrap();
    let pinned = prerequisites(runtime);
    let executor = Arc::clone(runtime.executor().unwrap());
    let job = runtime
        .plans()
        .confirm(&view.plan_id, &[], || pinned, executor.as_ref())
        .unwrap()
        .job_id;
    memory.entered.notified().await;
    let catalog = Arc::new(catalog);
    let stopping = tokio::spawn({
        let catalog = Arc::clone(&catalog);
        async move { catalog.shutdown().await }
    });
    memory.cancelled.notified().await;
    assert!(!stopping.is_finished(), "shutdown returned before the join");
    assert_eq!(runtime_job(&catalog, &job).status, "running");
    memory.release.notify_one();
    stopping.await.unwrap();
    let finished = runtime_job(&catalog, &job);
    assert_eq!(
        (finished.status.as_str(), finished.phase.as_str()),
        ("failed", "finished")
    );
    assert!(!layout.plugins.join("acme.held").exists());
    tokio::task::spawn_blocking(move || host.destroy_all(Duration::from_secs(5)))
        .await
        .unwrap();
}

fn runtime_job(catalog: &CatalogComposition, job: &str) -> CatalogJob {
    catalog.services().plans().job(job).unwrap()
}
