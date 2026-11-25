//! The test harness's official loading seam (M5 X0,
//! [`super::load_official_test_fixture`]): an official `vosu.*` process
//! package loads only through it, only in a debug build, and never as
//! in-process code.

use std::path::Path;

use crate::plugin::execution::{Execution, PackageOrigin};
use crate::plugin::registry::{LoadedPlugin, load_development, load_official_test_fixture};
use crate::plugin::test_support::write_development_package;
use crate::plugin::{HostError, ValidationSnapshot};

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
