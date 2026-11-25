//! Package/backend separation (`docs/v2/plugin-abi.md` §1, §2, §7.1): one
//! validation path for every package, and only a local grant ever maps
//! native code in core.

use std::path::{Path, PathBuf};

use vosu_plugin_api::{VOSU_PLUGIN_ABI_MAJOR, vosu_plugin_vtable};

use super::descriptor::{check_descriptor, descriptor_minimum};
use super::{Execution, PackageOrigin};
use crate::plugin::registry::{BundledPlugin, LoadedPlugin, load_bundled, load_development};
use crate::plugin::test_support::write_development_package;
use crate::plugin::{HostError, ValidationSnapshot};

fn version() -> semver::Version {
    semver::Version::parse("2.0.0-alpha.1").unwrap()
}

fn load_dev(dir: &Path, debug_build: bool) -> Result<LoadedPlugin, HostError> {
    load_development(&version(), &ValidationSnapshot::default(), dir, debug_build)
}

/// Bytes no loader accepts: mapping them fails, so a successful process
/// load proves the library was never handed to the loader.
const NOT_A_LIBRARY: &[u8] = b"not a shared object; mapping this must fail";

#[test]
fn process_package_is_validated_without_mapping_its_library() {
    let dir = tempfile::tempdir().unwrap();
    write_development_package(
        dir.path(),
        "test.h0.process",
        "h0-process",
        "process",
        "plugin.bin",
        NOT_A_LIBRARY,
    )
    .unwrap();
    let loaded = load_dev(dir.path(), true).expect("process package validates without loading");
    assert_eq!(loaded.origin(), PackageOrigin::Development);
    let Execution::Process(package) = loaded.execution() else {
        panic!("runtime = \"process\" must never become local code");
    };
    let root = dir.path().canonicalize().unwrap();
    assert_eq!(package.root(), root);
    assert_eq!(package.library(), root.join("lib").join("plugin.bin"));
    // The same validation path as a local package ran: schema, matcher and
    // action assets are all present.
    assert_eq!(loaded.namespace_schemas().len(), 1);
    assert!(loaded.manifest().matcher.is_some());
    assert_eq!(loaded.compiled_action_schemas().len(), 2);

    // Positive control: the identical package under the local grant does
    // hand the same bytes to the loader, which rejects them.
    write_development_package(
        dir.path(),
        "test.h0.process",
        "h0-process",
        "in-process",
        "plugin.bin",
        NOT_A_LIBRARY,
    )
    .unwrap();
    assert!(matches!(
        load_dev(dir.path(), true),
        Err(HostError::Load(_))
    ));
}

#[test]
fn process_package_library_must_exist_inside_the_package() {
    let dir = tempfile::tempdir().unwrap();
    write_development_package(
        dir.path(),
        "test.h0.missing",
        "h0-missing",
        "process",
        "plugin.bin",
        NOT_A_LIBRARY,
    )
    .unwrap();
    std::fs::remove_file(dir.path().join("lib/plugin.bin")).unwrap();
    match load_dev(dir.path(), true) {
        Err(HostError::Io(error)) => assert_eq!(error.kind(), std::io::ErrorKind::NotFound),
        other => panic!(
            "missing process library must refuse the load, got {:?}",
            other.err()
        ),
    }
}

#[cfg(unix)]
#[test]
fn process_package_library_symlink_cannot_escape_the_package() {
    let dir = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    write_development_package(
        dir.path(),
        "test.h0.escape",
        "h0-escape",
        "process",
        "plugin.bin",
        NOT_A_LIBRARY,
    )
    .unwrap();
    std::fs::write(outside.path().join("plugin.bin"), NOT_A_LIBRARY).unwrap();
    std::fs::remove_file(dir.path().join("lib/plugin.bin")).unwrap();
    std::os::unix::fs::symlink(
        outside.path().join("plugin.bin"),
        dir.path().join("lib/plugin.bin"),
    )
    .unwrap();
    match load_dev(dir.path(), true) {
        Err(HostError::Io(error)) => {
            assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied)
        }
        other => panic!(
            "escaping process library must refuse the load, got {:?}",
            other.err()
        ),
    }
}

#[test]
fn release_builds_still_refuse_process_development_packages() {
    let dir = tempfile::tempdir().unwrap();
    write_development_package(
        dir.path(),
        "test.h0.release",
        "h0-release",
        "process",
        "plugin.bin",
        NOT_A_LIBRARY,
    )
    .unwrap();
    assert!(matches!(
        load_dev(dir.path(), false),
        Err(HostError::InProcessRefused { .. })
    ));
}

#[test]
fn bundled_packages_stay_local_with_bundled_provenance() {
    let manifest = "kind='plugin'\nmanifest_version=1\nid='test.h0.bundled'\nname='Bundled'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n";
    let loaded = load_bundled(
        &version(),
        &ValidationSnapshot::default(),
        &[BundledPlugin {
            manifest,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: vosu_plugin_testkit::echo::vosu_plugin_vtable,
        }],
    )
    .unwrap();
    assert_eq!(loaded[0].origin(), PackageOrigin::Bundled);
    let Execution::Local(code) = loaded[0].execution() else {
        panic!("a bundled plugin must stay in-process");
    };
    assert!(
        code.library.is_none(),
        "bundled code is linked, never dlopened"
    );
}

#[test]
fn descriptor_checks_reject_every_invalid_header() {
    let valid = vosu_plugin_testkit::echo::vosu_plugin_vtable();
    check_descriptor(&valid).unwrap();
    let wrong_major = vosu_plugin_vtable {
        abi_version: VOSU_PLUGIN_ABI_MAJOR + 1,
        ..valid
    };
    assert!(matches!(
        check_descriptor(&wrong_major),
        Err(HostError::AbiMismatch { found, .. }) if found == VOSU_PLUGIN_ABI_MAJOR + 1
    ));
    let too_small = vosu_plugin_vtable {
        struct_size: descriptor_minimum() as u32 - 1,
        ..valid
    };
    assert!(matches!(
        check_descriptor(&too_small),
        Err(HostError::DescriptorTooSmall { .. })
    ));
    for missing in [
        vosu_plugin_vtable {
            create: None,
            ..valid
        },
        vosu_plugin_vtable {
            dispatch: None,
            ..valid
        },
        vosu_plugin_vtable {
            destroy: None,
            ..valid
        },
    ] {
        assert!(matches!(
            check_descriptor(&missing),
            Err(HostError::MissingRequiredFunction)
        ));
    }
}

/// The testkit cdylib Cargo builds beside this test binary, copied under a
/// name no other test maps, so module enumeration attributes any mapping
/// to this test alone.
fn unique_testkit_copy(dir: &Path) -> (String, Vec<u8>) {
    let library = if cfg!(windows) {
        "vosu_plugin_testkit.dll"
    } else {
        "libvosu_plugin_testkit.so"
    };
    let source: PathBuf = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join(library);
    assert!(
        source.is_file(),
        "testkit cdylib must be built beside this test binary"
    );
    let nonce = dir
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap()
        .replace(|c: char| !c.is_ascii_alphanumeric(), "");
    let name = if cfg!(windows) {
        format!("h0_probe_{nonce}.dll")
    } else {
        format!("libh0_probe_{nonce}.so")
    };
    (name, std::fs::read(source).unwrap())
}

fn mapped_in_this_process(file_name: &str) -> bool {
    vosu_sys::process::enumerate_modules(std::process::id())
        .unwrap()
        .iter()
        .any(|module| module.name.ends_with(file_name))
}

#[test]
fn process_package_of_a_real_plugin_library_is_never_mapped_in_core() {
    let dir = tempfile::tempdir().unwrap();
    let (name, bytes) = unique_testkit_copy(dir.path());
    write_development_package(
        dir.path(),
        "test.h0.real",
        "h0-real",
        "process",
        &name,
        &bytes,
    )
    .unwrap();
    let loaded = load_dev(dir.path(), true).unwrap();
    assert!(matches!(loaded.execution(), Execution::Process(_)));
    assert!(
        !mapped_in_this_process(&name),
        "validating a process package mapped its library into core"
    );
    drop(loaded);

    // Positive control: module enumeration does see the same file once the
    // local grant loads it.
    write_development_package(
        dir.path(),
        "test.h0.real",
        "h0-real",
        "in-process",
        &name,
        &bytes,
    )
    .unwrap();
    let local = load_dev(dir.path(), true).unwrap();
    assert!(matches!(local.execution(), Execution::Local(_)));
    assert!(mapped_in_this_process(&name));
}
