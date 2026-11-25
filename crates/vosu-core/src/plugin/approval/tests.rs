//! The approval store and gate (catalog-v2.md A5.5): exact binding of every
//! recorded value, fail-closed persistence, and a real `PluginHost` whose
//! launches pass through [`ApprovalGate`] on the initial load, a manual
//! restart and an automatic restart.

use std::path::Path;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::{Map, Value};

use super::*;
use crate::identity::PluginInstanceId;
use crate::logging::{ConsoleSink, Logger, LoggerOptions};
use crate::plugin::PluginHost;
use crate::plugin::execution::{Execution, LossCause};
use crate::plugin::process::supervisor::{FailureReason, ProcessPhase};
use crate::plugin::registry::{LoadedPlugin, load_development};
use crate::plugin::settings::{PluginSettingsService, SettingsError, SettingsStorage};
use crate::plugin::test_support::{LaunchBehavior, RecordingLauncher, write_development_package};
use crate::plugin::{HostError, ValidationSnapshot};

const LIBRARY: &[u8] = b"never mapped by core";

fn package(dir: &Path, plugin_id: &str) -> LoadedPlugin {
    write_development_package(
        dir,
        plugin_id,
        "approval-target",
        "process",
        "plugin.bin",
        LIBRARY,
    )
    .unwrap();
    load_development(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &ValidationSnapshot::default(),
        dir,
        false,
    )
    .expect("a prerelease release build loads a process package for approval")
}

fn process(plugin: &LoadedPlugin) -> &ProcessPackage {
    match plugin.execution() {
        Execution::Process(package) => package,
        Execution::Local(_) => panic!("expected a process package"),
    }
}

fn approve_now(store: &ApprovalStore, plugin: &LoadedPlugin) -> ApprovalRecord {
    let package = process(plugin);
    let record = Digests::of(package)
        .unwrap()
        .record(&plugin.manifest().id, Provenance::of(package));
    store.approve(record.clone()).unwrap();
    record
}

fn library_path(dir: &Path) -> std::path::PathBuf {
    dir.join("lib/plugin.bin")
}

#[test]
fn digests_are_64_lowercase_hex_digits() {
    let digest = Sha256Digest([0xab; 32]);
    let text = digest.to_string();
    assert_eq!(text, "ab".repeat(32));
    assert_eq!(Sha256Digest::parse_hex(&text), Some(digest));
    assert_eq!(Sha256Digest::parse_hex(&"AB".repeat(32)), None);
    assert_eq!(Sha256Digest::parse_hex(&"ab".repeat(31)), None);
    assert_eq!(
        Sha256Digest::parse_hex(&format!("{}g", "a".repeat(63))),
        None
    );
}

#[test]
fn a_record_binds_id_runtime_both_digests_and_provenance() {
    let config = tempfile::tempdir().unwrap();
    let package_dir = tempfile::tempdir().unwrap();
    let plugin = package(package_dir.path(), "test.u0.bind");
    let store = ApprovalStore::open(config.path());
    let id = "test.u0.bind";

    let refused = store.check(id, process(&plugin)).unwrap_err();
    assert_eq!(refused.stage, StartupStage::Approval);
    assert_eq!(refused.code, "approval_required");

    let record = approve_now(&store, &plugin);
    assert_eq!(record.runtime, ApprovedRuntime::Process);
    assert_eq!(
        record.provenance,
        Provenance::Development {
            path: package_dir.path().canonicalize().unwrap()
        }
    );
    assert_eq!(
        store.check(id, process(&plugin)).unwrap(),
        record.library_sha256,
        "the approved digest is what the launcher receives"
    );

    // Another id from the same directory is not approved by this record.
    assert_eq!(
        store
            .check("test.u0.other", process(&plugin))
            .unwrap_err()
            .code,
        "approval_required"
    );

    // A record for other bytes never approves these.
    for changed in [
        ApprovalRecord {
            manifest_sha256: Sha256Digest([0; 32]),
            ..record.clone()
        },
        ApprovalRecord {
            library_sha256: Sha256Digest([0; 32]),
            ..record.clone()
        },
    ] {
        store.approve(changed).unwrap();
        assert_eq!(
            store.check(id, process(&plugin)).unwrap_err().code,
            "approval_changed"
        );
    }
    store.approve(record).unwrap();
    store.check(id, process(&plugin)).unwrap();

    // Replacing the library revokes it until approved again.
    std::fs::write(library_path(package_dir.path()), b"other bytes").unwrap();
    assert_eq!(
        store.check(id, process(&plugin)).unwrap_err().code,
        "approval_changed"
    );
}

#[test]
fn the_store_persists_one_record_per_id_and_provenance() {
    let config = tempfile::tempdir().unwrap();
    let package_dir = tempfile::tempdir().unwrap();
    let plugin = package(package_dir.path(), "test.u0.persist");
    let store = ApprovalStore::open(config.path());
    let record = approve_now(&store, &plugin);
    approve_now(&store, &plugin);
    let installed = ApprovalRecord {
        provenance: Provenance::Installed {
            source: "https://example.invalid/catalog-v2.json".into(),
            version: "1.0.0".into(),
            package_sha256: Sha256Digest([7; 32]),
        },
        ..record.clone()
    };
    store.approve(installed.clone()).unwrap();

    let reopened = ApprovalStore::open(config.path());
    assert_eq!(
        reopened.record(&record.id, &record.provenance).unwrap(),
        Some(record.clone())
    );
    assert_eq!(
        reopened.record(&record.id, &installed.provenance).unwrap(),
        Some(installed.clone())
    );
    let text = std::fs::read_to_string(config.path().join(APPROVALS_FILE)).unwrap();
    let file: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(file["version"], 1);
    assert_eq!(file["approvals"].as_array().unwrap().len(), 2);
    assert_eq!(file["approvals"][0]["runtime"], "process");
    assert_eq!(file["approvals"][0]["provenance"]["kind"], "development");

    assert!(reopened.revoke(&record.id, &record.provenance).unwrap());
    assert!(!reopened.revoke(&record.id, &record.provenance).unwrap());
    let again = ApprovalStore::open(config.path());
    assert_eq!(again.record(&record.id, &record.provenance).unwrap(), None);
    assert_eq!(
        again.record(&record.id, &installed.provenance).unwrap(),
        Some(installed)
    );
}

#[test]
fn an_unreadable_store_fails_closed_and_is_never_overwritten() {
    let config = tempfile::tempdir().unwrap();
    let package_dir = tempfile::tempdir().unwrap();
    let plugin = package(package_dir.path(), "test.u0.corrupt");
    let path = config.path().join(APPROVALS_FILE);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    for corrupt in [
        "not json".to_owned(),
        r#"{"version":2,"approvals":[]}"#.to_owned(),
        r#"{"version":1,"approvals":[],"extra":true}"#.to_owned(),
    ] {
        std::fs::write(&path, &corrupt).unwrap();
        let store = ApprovalStore::open(config.path());
        assert_eq!(
            store
                .check("test.u0.corrupt", process(&plugin))
                .unwrap_err()
                .code,
            "approvals_unreadable"
        );
        let record = Digests::of(process(&plugin))
            .unwrap()
            .record("test.u0.corrupt", Provenance::of(process(&plugin)));
        assert!(store.approve(record).is_err());
        assert_eq!(std::fs::read_to_string(&path).unwrap(), corrupt);
    }
}

struct DummyStorage;
impl SettingsStorage for DummyStorage {
    fn load(&self, _plugin_id: &str) -> Result<Map<String, Value>, SettingsError> {
        Ok(Map::new())
    }
    fn store(&self, _plugin_id: &str, _values: &Map<String, Value>) -> Result<(), SettingsError> {
        Ok(())
    }
}

struct SilentConsole;
impl ConsoleSink for SilentConsole {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _text: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _lines: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _text: &str) -> std::io::Result<()> {
        Ok(())
    }
}

/// Records the digest each approved launch received, then launches through
/// a recording launcher that spawns nothing.
struct Approved {
    inner: Arc<RecordingLauncher>,
    digests: Mutex<Vec<[u8; 32]>>,
}

impl ApprovedLaunch for Arc<Approved> {
    fn launch_approved(
        &self,
        launch: ProcessLaunch<'_>,
        library_sha256: [u8; 32],
    ) -> Result<Arc<dyn ProcessEndpoint>, HostError> {
        self.digests
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(library_sha256);
        self.inner.launch(launch)
    }
}

fn wait_phase(host: &PluginHost, id: PluginInstanceId, accept: impl Fn(&ProcessPhase) -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let phase = host.process_status(id).unwrap().phase;
        if accept(&phase) {
            return;
        }
        assert!(Instant::now() < deadline, "stuck in {phase:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn failure_code(host: &PluginHost, id: PluginInstanceId) -> String {
    match host.process_status(id).unwrap().last_failure {
        Some(FailureReason::Startup(failure)) => {
            assert_eq!(failure.stage, StartupStage::Approval);
            failure.code
        }
        other => panic!("expected an approval failure, got {other:?}"),
    }
}

#[test]
fn every_load_and_restart_passes_the_gate_with_the_approved_digest() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let logs = tempfile::tempdir().unwrap();
    let logger = Logger::new(
        logs.path(),
        LoggerOptions {
            version: "0.1.0".to_owned(),
            debug_log: false,
            queue_capacity: 64,
        },
        Arc::new(SilentConsole),
    )
    .unwrap();
    let host = PluginHost::new(
        logger,
        Arc::new(PluginSettingsService::new(Arc::new(DummyStorage))),
        runtime.handle().clone(),
    );
    let config = tempfile::tempdir().unwrap();
    let store = Arc::new(ApprovalStore::open(config.path()));
    let approved = Arc::new(Approved {
        inner: RecordingLauncher::new(LaunchBehavior::Ready),
        digests: Mutex::new(Vec::new()),
    });
    host.install_process_launcher(Arc::new(ApprovalGate::new(
        Arc::clone(&store),
        Arc::clone(&approved),
    )))
    .unwrap();
    let package_dir = tempfile::tempdir().unwrap();
    let plugin = package(package_dir.path(), "test.u0.gate");
    let id = PluginInstanceId::new(7);
    let launches = || approved.inner.launches().len();

    // Unapproved: refused before the launcher, not retried, not a crash.
    match host.create_instance(id, &plugin, std::env::temp_dir(), None) {
        Err(HostError::ProcessStartupFailed(failure)) => {
            assert_eq!(failure.stage, StartupStage::Approval);
        }
        other => panic!("expected an approval refusal, got {other:?}"),
    }
    assert_eq!(launches(), 0, "an unapproved package reached the launcher");
    let status = host.process_status(id).unwrap();
    assert_eq!(status.phase, ProcessPhase::Failed);
    assert_eq!(status.crash_events, 0);
    assert_eq!(failure_code(&host, id), "approval_required");

    // Approved: a manual restart launches with exactly the approved digest.
    let record = approve_now(&store, &plugin);
    host.restart_process_instance(id).unwrap();
    assert_eq!(
        host.process_status(id).unwrap().phase,
        ProcessPhase::Running
    );
    assert_eq!(launches(), 1);
    assert_eq!(
        *approved.digests.lock().unwrap(),
        vec![record.library_sha256.0]
    );

    // Changed library, then a crash: the automatic restart is refused by the
    // gate, which is not a crash event and leaves it failed.
    std::fs::write(library_path(package_dir.path()), b"replaced after approval").unwrap();
    approved.inner.launches()[0]
        .loss
        .report(LossCause::ProcessExited { status: Some(0) });
    wait_phase(&host, id, |phase| *phase == ProcessPhase::Failed);
    assert_eq!(failure_code(&host, id), "approval_changed");
    assert_eq!(launches(), 1, "changed bytes reached the launcher");
    assert_eq!(host.process_status(id).unwrap().crash_events, 1);

    // A manual restart is checked too.
    assert!(host.restart_process_instance(id).is_err());
    assert_eq!(launches(), 1);

    // Approving the new bytes starts them, with the new digest.
    let record = approve_now(&store, &plugin);
    host.restart_process_instance(id).unwrap();
    assert_eq!(launches(), 2);
    assert_eq!(
        approved.digests.lock().unwrap().last(),
        Some(&record.library_sha256.0)
    );

    // Revocation: teardown, and the next start is refused.
    assert!(store.revoke(&record.id, &record.provenance).unwrap());
    host.teardown_instance(id).unwrap();
    assert_eq!(
        host.process_status(id).unwrap().phase,
        ProcessPhase::Stopped
    );
    assert!(host.restart_process_instance(id).is_err());
    assert_eq!(failure_code(&host, id), "approval_required");
    assert_eq!(launches(), 2);
    drop(_enter);
}
