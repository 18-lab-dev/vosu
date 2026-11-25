//! K0 composition: fail-closed startup, recovery before load, durable
//! revocation and the daemon-lifetime pending-restart marker. Real dlopen
//! loads are covered by the root `catalog_runtime` integration test.
use super::*;
use crate::{
    catalog::{
        approvals::{Approval, Artifact},
        install::{
            DownloadFuture, HandoffFuture, InstalledDocument, InstalledFor, InstalledKind,
            InstalledRecord, LoadRequest, LoadStatus, RECORDS_FILE,
        },
        sources::OFFICIAL_SOURCES,
        trust::PolicyError,
    },
    logging::{ConsoleSink, Logger, LoggerOptions},
    plugin::{
        BundledPlugin, DiscoveryObserver, PluginInstanceId, ValidationSnapshot, load_bundled,
        settings::{PluginSettingsService, SettingsError, SettingsStorage},
    },
};
use serde_json::{Map, Value};
use std::{path::Path, sync::Mutex, time::Duration};
use vosu_plugin_api::manifest::Runtime;

const USER: &str = "alice/plugins";

struct NoSettings;
impl SettingsStorage for NoSettings {
    fn load(&self, _: &str) -> Result<Map<String, Value>, SettingsError> {
        Ok(Map::new())
    }
    fn store(&self, _: &str, _: &Map<String, Value>) -> Result<(), SettingsError> {
        Ok(())
    }
}
struct Silent;
impl ConsoleSink for Silent {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
}

struct NoTransport;
impl PackageTransport for NoTransport {
    fn download<'a>(
        &'a self,
        _: &'a str,
        _: u64,
        _: &'a Path,
        _: &'a CancellationToken,
    ) -> DownloadFuture<'a> {
        Box::pin(async { Err(vosu_net::NetworkError::Status(404)) })
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<String>>);
impl RuntimeHandoff for Recorder {
    fn load_in_process<'a>(
        &'a self,
        request: LoadRequest<'a>,
    ) -> HandoffFuture<'a, Result<(), String>> {
        self.0.lock().unwrap().push(request.record.id.clone());
        Box::pin(async { Ok(()) })
    }
    fn rescan_web(&self) -> HandoffFuture<'_, ()> {
        Box::pin(async {})
    }
}

struct Fixture {
    root: tempfile::TempDir,
    host: Arc<PluginHost>,
    handoff: Arc<Recorder>,
}
impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let logger = Logger::new(
            root.path().join("logs"),
            LoggerOptions {
                version: "0.1.0".into(),
                debug_log: false,
                queue_capacity: 64,
            },
            Arc::new(Silent),
        )
        .unwrap();
        let host = PluginHost::new(
            logger,
            Arc::new(PluginSettingsService::new(Arc::new(NoSettings))),
            tokio::runtime::Handle::current(),
        );
        Self {
            root,
            host,
            handoff: Arc::default(),
        }
    }
    fn config(&self) -> std::path::PathBuf {
        self.root.path().join("config")
    }
    fn layout(&self) -> InstallLayout {
        let base = self.root.path();
        InstallLayout::new(
            base.join("state"),
            base.join("plugins"),
            base.join("tools"),
            base.join("overlays"),
            base.join("downloads"),
        )
        .unwrap()
    }
    async fn open(&self) -> CatalogRuntime {
        let host = Arc::clone(&self.host);
        let config = self.config();
        let layout = self.layout();
        let handoff: Arc<dyn RuntimeHandoff> = self.handoff.clone();
        tokio::task::spawn_blocking(move || {
            CatalogRuntime::open(
                host,
                CatalogHost {
                    application: semver::Version::parse("2.0.0-alpha.1").unwrap(),
                    target: "x86_64-unknown-linux-gnu".into(),
                },
                &config,
                Ok(layout),
                Arc::new(NoTransport),
                handoff,
                tokio::runtime::Handle::current(),
            )
        })
        .await
        .unwrap()
    }
    /// A live (running) in-process instance with `id`, as a loaded plugin is.
    fn run(&self, id: &str, instance: u64) {
        let manifest = format!(
            "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='{id}'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n"
        );
        let loaded = load_bundled(
            &semver::Version::parse("2.0.0-alpha.1").unwrap(),
            &ValidationSnapshot::default(),
            &[BundledPlugin {
                manifest: &manifest,
                settings_schema: None,
                namespace_schemas: &[],
                action_schemas: &[],
                vtable: vosu_plugin_testkit::echo::vosu_plugin_vtable,
            }],
        )
        .unwrap();
        self.host
            .create_instance(
                PluginInstanceId::new(instance),
                &loaded[0],
                self.root.path().join("data"),
                None,
            )
            .unwrap();
    }
}

fn artifact(id: &str, runtime: Runtime, hash: u8) -> Artifact {
    Artifact {
        id: id.into(),
        source: SourceIdentity::parse(USER).unwrap().identity,
        version: "1.0.0".into(),
        sha256: [hash; 32],
        runtime,
    }
}
fn record(artifact: Artifact, approved: bool) -> InstalledRecord {
    InstalledRecord {
        kind: InstalledKind::Plugin,
        id: artifact.id.clone(),
        version: artifact.version.clone(),
        source: artifact.source.clone(),
        target: "x86_64-unknown-linux-gnu".into(),
        sha256: artifact.sha256.iter().map(|b| format!("{b:02x}")).collect(),
        approval: approved.then(|| Approval::grant(&artifact)),
        artifact: Some(artifact),
        installed_for: InstalledFor::Requested,
    }
}
fn write_records(layout: &InstallLayout, records: Vec<InstalledRecord>) {
    std::fs::create_dir_all(&layout.state).unwrap();
    std::fs::write(
        layout.state.join(RECORDS_FILE),
        serde_json::to_vec(&InstalledDocument {
            format: 1,
            generation: 1,
            records,
        })
        .unwrap(),
    )
    .unwrap();
}
fn sources_with_user() -> Vec<SourceInput> {
    OFFICIAL_SOURCES
        .iter()
        .chain([&USER])
        .map(|source| SourceInput {
            source: (*source).into(),
            enabled: true,
        })
        .collect()
}
fn user() -> SourceIdentity {
    SourceIdentity::parse(USER).unwrap().identity
}
fn status(reports: &[LoadReport], id: &str) -> LoadStatus {
    reports
        .iter()
        .find(|report| report.id == id)
        .unwrap()
        .status
        .clone()
}

#[tokio::test(flavor = "multi_thread")]
async fn corrupt_stores_disable_features_without_rewriting_them() {
    let fixture = Fixture::new();
    let layout = fixture.layout();
    std::fs::create_dir_all(fixture.config()).unwrap();
    std::fs::create_dir_all(&layout.state).unwrap();
    let sources = fixture.config().join("catalog-source-settings.json");
    let records = layout.state.join(RECORDS_FILE);
    std::fs::write(&sources, b"{corrupt").unwrap();
    std::fs::write(&records, b"[corrupt").unwrap();
    let generation = fixture.host.discovery_generation();
    let runtime = fixture.open().await;
    assert_eq!(
        runtime.status(),
        &CatalogStatus {
            sources: Some(SourceStoreError::Invalid),
            installed: Some(InstallError::RecordsInvalid),
        }
    );
    assert!(runtime.fetch().is_none() && runtime.executor().is_none());
    assert!(runtime.load_installed().await.is_empty());
    assert_eq!(
        runtime.set_trust(user(), false).await,
        Err(CatalogError::Disabled)
    );
    // No observer was registered for a disabled source feature.
    assert_eq!(fixture.host.discovery_generation(), generation);
    assert_eq!(std::fs::read(&sources).unwrap(), b"{corrupt");
    assert_eq!(std::fs::read(&records).unwrap(), b"[corrupt");
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn startup_rolls_back_a_half_committed_install_before_any_load() {
    let fixture = Fixture::new();
    let layout = fixture.layout();
    // Journal written, new tree renamed into place, records never published.
    let destination = layout.plugins.join("acme.half");
    std::fs::create_dir_all(&destination).unwrap();
    std::fs::write(destination.join("manifest.toml"), b"half").unwrap();
    std::fs::create_dir_all(&layout.state).unwrap();
    let journal = layout.state.join(crate::catalog::install::JOURNAL_FILE);
    std::fs::write(
        &journal,
        serde_json::to_vec(&serde_json::json!({"format":1,"previous_generation":0,
            "next_generation":1,"operations":[{"staging":layout.plugins.join(".stage-half"),
            "destination":destination,"backup":null}]}))
        .unwrap(),
    )
    .unwrap();
    let runtime = fixture.open().await;
    assert_eq!(runtime.status(), &CatalogStatus::default());
    assert!(!journal.exists() && !destination.exists());
    assert!(runtime.load_installed().await.is_empty());
    assert!(fixture.handoff.0.lock().unwrap().is_empty());
    assert_eq!(runtime.installed_generation(), 0);
    runtime.shutdown().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn untrust_clears_approvals_durably_and_pins_running_code_until_restart() {
    let fixture = Fixture::new();
    let layout = fixture.layout();
    write_records(
        &layout,
        vec![
            record(artifact("acme.live", Runtime::InProcess, 1), false),
            record(artifact("acme.idle", Runtime::InProcess, 2), false),
            record(artifact("acme.proc", Runtime::Process, 3), true),
        ],
    );
    fixture.run("acme.live", 7001);
    let runtime = fixture.open().await;
    // The fresh source store does not know the user source yet: User tier.
    let reports = runtime.load_installed().await;
    assert_eq!(
        status(&reports, "acme.idle"),
        LoadStatus::Refused(PolicyError::InProcessDenied)
    );
    assert_eq!(
        status(&reports, "acme.proc"),
        LoadStatus::RequiresM5ProcessRuntime
    );
    let observed = runtime.offers().generation();
    runtime.replace_sources(sources_with_user()).await.unwrap();
    assert!(
        runtime.offers().generation() > observed,
        "source change refreshes G0"
    );
    let granted = runtime.set_trust(user(), true).await.unwrap();
    assert_eq!(granted.pending_restart, Vec::<String>::new());

    let observed = runtime.offers().generation();
    let revoked = runtime.set_trust(user(), false).await.unwrap();
    assert_eq!(revoked.pending_restart, ["acme.live"]);
    assert_eq!(revoked.approvals_cleared, ["acme.proc"]);
    assert!(revoked.stop_process.is_empty());
    assert!(runtime.offers().generation() > observed);
    // Durable before the source change: a restart reads no approval.
    assert_eq!(runtime.installed_generation(), 2);
    let on_disk: InstalledDocument =
        serde_json::from_slice(&std::fs::read(layout.state.join(RECORDS_FILE)).unwrap()).unwrap();
    assert_eq!(on_disk.generation, 2);
    assert!(
        on_disk
            .records
            .iter()
            .all(|record| record.approval.is_none())
    );

    // Re-trust: the idle plugin becomes loadable, the running one stays
    // "revocation pending restart" for the daemon lifetime.
    runtime.set_trust(user(), true).await.unwrap();
    assert_eq!(runtime.pending_restart(), ["acme.live"]);
    let reports = runtime.executor().unwrap().load_committed().await;
    assert_eq!(
        status(&reports, "acme.live"),
        LoadStatus::Refused(PolicyError::RevocationPendingRestart)
    );
    assert_eq!(status(&reports, "acme.idle"), LoadStatus::Loaded);
    assert_eq!(*fixture.handoff.0.lock().unwrap(), ["acme.idle"]);

    // Untrust again: the next load is blocked by current policy.
    runtime.set_trust(user(), false).await.unwrap();
    let reports = runtime.executor().unwrap().load_committed().await;
    assert_eq!(
        status(&reports, "acme.idle"),
        LoadStatus::Refused(PolicyError::InProcessDenied)
    );
    runtime.shutdown().await;
    fixture.host.destroy_all(Duration::from_secs(5));
}

#[tokio::test(flavor = "multi_thread")]
async fn removing_a_trusted_source_revokes_like_untrust() {
    let fixture = Fixture::new();
    let layout = fixture.layout();
    write_records(
        &layout,
        vec![record(artifact("acme.proc", Runtime::Process, 3), true)],
    );
    let runtime = fixture.open().await;
    runtime.replace_sources(sources_with_user()).await.unwrap();
    runtime.set_trust(user(), true).await.unwrap();
    let official: Vec<_> = sources_with_user().into_iter().take(3).collect();
    let removed = runtime.replace_sources(official).await.unwrap();
    assert_eq!(removed.approvals_cleared, ["acme.proc"]);
    let (_, records) = runtime.fetch().unwrap().sources().snapshot().unwrap();
    assert!(records.iter().all(|record| record.identity != user()));
    // Control: a change that removes no trust clears nothing.
    let kept = runtime
        .replace_sources(sources_with_user().into_iter().take(3).collect())
        .await
        .unwrap();
    assert!(kept.approvals_cleared.is_empty());
    runtime.shutdown().await;
}
