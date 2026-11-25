//! The test harness's official loading seam (M5 X0,
//! [`super::load_official_test_fixture`]): an official `vosu.*` process
//! package loads only through it, only in a debug build, never as
//! in-process code, and never past the approval gate without a record.

use std::path::Path;
use std::sync::Arc;

use crate::identity::PluginInstanceId;
use crate::plugin::approval::{ApprovalGate, ApprovalStore, ApprovedLaunch, Digests, Provenance};
use crate::plugin::execution::{
    Execution, PackageOrigin, ProcessEndpoint, ProcessLaunch, ProcessLauncher,
};
use crate::plugin::process::control::StartupStage;
use crate::plugin::registry::{LoadedPlugin, load_development, load_official_test_fixture};
use crate::plugin::test_support::{LaunchBehavior, RecordingLauncher, write_development_package};
use crate::plugin::{HostError, PluginHost, ValidationSnapshot};

fn version() -> semver::Version {
    semver::Version::parse("2.0.0-alpha.1").unwrap()
}

fn official(dir: &Path, debug_build: bool) -> Result<LoadedPlugin, HostError> {
    load_official_test_fixture(&version(), &ValidationSnapshot::default(), dir, debug_build)
}

/// Bytes no loader accepts: a package that validates with them was never
/// handed to the loader.
const NOT_A_LIBRARY: &[u8] = b"not a shared object; mapping this must fail";

fn package(dir: &Path, runtime: &str) {
    write_development_package(
        dir,
        "vosu.x0official",
        "x0-official",
        runtime,
        "plugin.bin",
        NOT_A_LIBRARY,
    )
    .unwrap();
}

#[test]
fn official_fixture_a_release_build_refuses_before_reading_the_package() {
    // No package exists at all: the refusal comes before any read.
    let missing = tempfile::tempdir().unwrap().path().join("absent");
    assert!(matches!(
        official(&missing, false),
        Err(HostError::InProcessRefused { .. })
    ));
    let dir = tempfile::tempdir().unwrap();
    package(dir.path(), "process");
    assert!(matches!(
        official(dir.path(), false),
        Err(HostError::InProcessRefused { .. })
    ));
}

#[test]
fn official_fixture_loads_an_official_identity_the_development_loader_refuses() {
    let dir = tempfile::tempdir().unwrap();
    package(dir.path(), "process");
    let refused = load_development(&version(), &ValidationSnapshot::default(), dir.path(), true)
        .err()
        .expect("the development loader refuses an official identity");
    assert!(
        refused.to_string().contains("manifest.namespace"),
        "{refused}"
    );

    let loaded = official(dir.path(), true).expect("the official seam accepts it");
    assert_eq!(loaded.manifest().id, "vosu.x0official");
    assert_eq!(loaded.origin(), PackageOrigin::OfficialTestFixture);
    assert!(loaded.origin().is_official());
    assert_ne!(loaded.origin(), PackageOrigin::Bundled);
    let Execution::Process(package) = loaded.execution() else {
        panic!("the official fixture must stay a process package");
    };
    let root = dir.path().canonicalize().unwrap();
    assert_eq!(package.library(), root.join("lib").join("plugin.bin"));
}

#[test]
fn official_fixture_refuses_in_process_before_mapping_anything() {
    let dir = tempfile::tempdir().unwrap();
    package(dir.path(), "in-process");
    // Mapping `NOT_A_LIBRARY` would fail with a load error instead.
    assert!(matches!(
        official(dir.path(), true),
        Err(HostError::InProcessRefused { .. })
    ));
}

/// Launches through a recording launcher that spawns nothing.
struct Recording(Arc<RecordingLauncher>);

impl ApprovedLaunch for Recording {
    fn launch_approved(
        &self,
        launch: ProcessLaunch<'_>,
        _library_sha256: [u8; 32],
    ) -> Result<Arc<dyn ProcessEndpoint>, HostError> {
        self.0.launch(launch)
    }
}

struct Silent;

impl crate::logging::ConsoleSink for Silent {
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

struct NoSettings;

impl crate::plugin::settings::SettingsStorage for NoSettings {
    fn load(
        &self,
        _plugin_id: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, crate::plugin::settings::SettingsError>
    {
        Ok(serde_json::Map::new())
    }
    fn store(
        &self,
        _plugin_id: &str,
        _values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), crate::plugin::settings::SettingsError> {
        Ok(())
    }
}

/// The seam grants officialness, never approval: under the production
/// approval gate an official fixture package without a record is refused
/// before the launcher exactly like a development package, and launches
/// only once its exact digests are approved.
#[test]
fn official_fixture_never_skips_the_approval_gate() {
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let logs = tempfile::tempdir().unwrap();
    let logger = crate::logging::Logger::new(
        logs.path(),
        crate::logging::LoggerOptions {
            version: "0.1.0".to_owned(),
            debug_log: false,
            queue_capacity: 64,
        },
        Arc::new(Silent),
    )
    .unwrap();
    let host = PluginHost::new(
        logger,
        Arc::new(crate::plugin::settings::PluginSettingsService::new(
            Arc::new(NoSettings),
        )),
        runtime.handle().clone(),
    );
    let config = tempfile::tempdir().unwrap();
    let store = Arc::new(ApprovalStore::open(config.path()));
    let recording = RecordingLauncher::new(LaunchBehavior::Ready);
    host.install_process_launcher(Arc::new(ApprovalGate::new(
        Arc::clone(&store),
        Recording(Arc::clone(&recording)),
    )))
    .unwrap();
    let dir = tempfile::tempdir().unwrap();
    package(dir.path(), "process");
    let loaded = official(dir.path(), true).unwrap();

    let id = PluginInstanceId::new(9);
    match host.create_instance(id, &loaded, std::env::temp_dir(), None) {
        Err(HostError::ProcessStartupFailed(failure)) => {
            assert_eq!(failure.stage, StartupStage::Approval);
            assert_eq!(failure.code, "approval_required");
        }
        other => panic!("expected an approval refusal, got {other:?}"),
    }
    assert!(
        recording.launches().is_empty(),
        "an unapproved official fixture reached the launcher"
    );

    let Execution::Process(package) = loaded.execution() else {
        unreachable!("checked by the loader");
    };
    store
        .approve(
            Digests::of(package)
                .unwrap()
                .record(&loaded.manifest().id, Provenance::of(package)),
        )
        .unwrap();
    let id = PluginInstanceId::new(10);
    host.create_instance(id, &loaded, std::env::temp_dir(), None)
        .expect("the approved official fixture launches");
    assert_eq!(recording.launches().len(), 1);
    host.destroy_all(std::time::Duration::from_secs(5));
}
