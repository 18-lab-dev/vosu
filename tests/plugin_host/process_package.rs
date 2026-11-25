//! Separate validated packages from execution (`docs/v2/plugin-abi.md`
//! §7.1–§7.2): core validates a `runtime = "process"` package and creates
//! its instance through the launcher without ever mapping the library, so
//! a static constructor can never run in the daemon. Bundled plugins stay
//! linked, in-process code.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::Duration;

use vosu_core::plugin::test_support::{
    LaunchBehavior, RecordingLauncher, write_development_package,
};
use vosu_core::plugin::{self, Execution, PackageOrigin, PluginInstanceId};

/// `$CC`, `cc`, `clang` or `gcc`, the same discovery as
/// `crates/vosu-plugin-api/tests/layout.rs` (Windows needs Clang). A
/// missing compiler fails the test: the constructor probe is required
/// evidence, not optional.
fn find_c_compiler() -> String {
    let mut candidates = Vec::new();
    if let Ok(cc) = std::env::var("CC") {
        candidates.push(cc);
    }
    candidates.extend(["cc", "clang", "gcc"].map(str::to_owned));
    candidates
        .into_iter()
        .find(|candidate| {
            Command::new(candidate)
                .arg("--version")
                .output()
                .is_ok_and(|out| out.status.success())
        })
        .expect("a C compiler ($CC, cc, clang or gcc) is required to build the constructor probe")
}

/// Builds a shared library whose load-time constructor (ELF
/// `.init_array`, or `DllMain` on Windows) writes `sentinel`. It exports
/// no `vosu_plugin_entry_v1`: merely mapping it is what runs code.
fn constructor_probe(dir: &Path, sentinel: &Path) -> (String, Vec<u8>) {
    let escaped = sentinel
        .to_str()
        .expect("UTF-8 temp path")
        .replace('\\', "\\\\")
        .replace('"', "\\\"");
    let source = format!(
        r#"#include <stdio.h>
static void write_sentinel(void) {{
    FILE *file = fopen("{escaped}", "w");
    if (file) {{ fputs("constructor ran", file); fclose(file); }}
}}
#ifdef _WIN32
#include <windows.h>
BOOL WINAPI DllMain(HINSTANCE instance, DWORD reason, LPVOID reserved) {{
    (void)instance; (void)reserved;
    if (reason == DLL_PROCESS_ATTACH) write_sentinel();
    return TRUE;
}}
#else
__attribute__((constructor)) static void run_constructor(void) {{ write_sentinel(); }}
#endif
"#
    );
    let source_path = dir.join("probe.c");
    std::fs::write(&source_path, source).unwrap();
    let name = if cfg!(windows) {
        "probe.dll"
    } else {
        "libprobe.so"
    };
    let output_path = dir.join(name);
    let compiler = find_c_compiler();
    let mut command = Command::new(&compiler);
    command.arg("-shared");
    if !cfg!(windows) {
        command.arg("-fPIC");
    }
    let output = command
        .arg("-o")
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .unwrap_or_else(|error| panic!("running {compiler}: {error}"));
    assert!(
        output.status.success(),
        "{compiler} failed to build the constructor probe:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    (name.to_owned(), std::fs::read(&output_path).unwrap())
}

fn version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap()
}

fn test_host(
    runtime: &tokio::runtime::Runtime,
    log_dir: &Path,
) -> Arc<vosu_core::plugin::PluginHost> {
    let logger = vosu_core::logging::Logger::new(
        log_dir,
        vosu_core::logging::LoggerOptions {
            version: "0.1.0".to_owned(),
            debug_log: false,
            queue_capacity: 64,
        },
        Arc::new(super::RecordingConsole::default()),
    )
    .unwrap();
    let settings = Arc::new(vosu_core::plugin::settings::PluginSettingsService::new(
        Arc::new(EmptySettings),
    ));
    vosu_core::plugin::PluginHost::new(logger, settings, runtime.handle().clone())
}

struct EmptySettings;
impl vosu_core::plugin::settings::SettingsStorage for EmptySettings {
    fn load(
        &self,
        _plugin_id: &str,
    ) -> Result<
        serde_json::Map<String, serde_json::Value>,
        vosu_core::plugin::settings::SettingsError,
    > {
        Ok(serde_json::Map::new())
    }
    fn store(
        &self,
        _plugin_id: &str,
        _values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), vosu_core::plugin::settings::SettingsError> {
        Ok(())
    }
}

#[test]
fn process_package_constructor_never_runs_in_core() {
    let scratch = tempfile::tempdir().unwrap();
    let sentinel: PathBuf = scratch.path().join("constructor-ran");
    let (library_name, library) = constructor_probe(scratch.path(), &sentinel);
    let package = tempfile::tempdir().unwrap();
    write_development_package(
        package.path(),
        "test.h0.constructor",
        "h0-constructor",
        "process",
        &library_name,
        &library,
    )
    .unwrap();

    let loaded = plugin::load_development(
        &version(),
        &vosu_core::plugin::ValidationSnapshot::default(),
        package.path(),
        true,
    )
    .expect("a process package validates without loading its library");
    assert_eq!(loaded.origin(), PackageOrigin::Development);
    let Execution::Process(process) = loaded.execution() else {
        panic!("runtime = \"process\" must never become local code");
    };
    assert!(
        !sentinel.exists(),
        "validation ran the library's constructor in core"
    );

    // Instance creation hands the package to the launcher, never the loader.
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _enter = runtime.enter();
    let logs = tempfile::tempdir().unwrap();
    let host = test_host(&runtime, logs.path());
    let launcher = RecordingLauncher::new(LaunchBehavior::Ready);
    host.install_process_launcher(launcher.clone()).unwrap();
    host.create_instance(
        PluginInstanceId::new(1),
        &loaded,
        package.path().to_owned(),
        None,
    )
    .expect("creating the process instance through the launcher");
    {
        let launches = launcher.launches();
        assert_eq!(launches.len(), 1);
        assert_eq!(launches[0].library, process.library());
    }
    assert!(
        !sentinel.exists(),
        "instance creation ran the library's constructor in core"
    );
    host.destroy_all(Duration::from_secs(5));
    assert_eq!(launcher.launches()[0].endpoint.destroy_calls(), 1);
    assert!(!sentinel.exists());

    // Positive control: the same bytes under the local development grant
    // are mapped, which runs the constructor before entry lookup fails.
    write_development_package(
        package.path(),
        "test.h0.constructor",
        "h0-constructor",
        "in-process",
        &library_name,
        &library,
    )
    .unwrap();
    assert!(matches!(
        plugin::load_development(
            &version(),
            &vosu_core::plugin::ValidationSnapshot::default(),
            package.path(),
            true,
        ),
        Err(plugin::HostError::Load(_))
    ));
    assert!(
        sentinel.exists(),
        "the probe's constructor never ran when mapped"
    );
}

#[test]
fn bundled_plugins_remain_linked_in_process_code() {
    let mut echo = Vec::new();
    for entry in vosu::plugins::BUNDLED {
        let manifest = vosu_plugin_api::manifest::PackageManifest::parse(entry.manifest)
            .and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin)
            .expect("bundled manifests parse");
        assert_eq!(
            manifest.runtime,
            vosu_plugin_api::Runtime::InProcess,
            "{} changed runtime",
            manifest.id
        );
        assert!(
            manifest.targets.is_empty(),
            "{} names a library",
            manifest.id
        );
        if manifest.id == "vosu.testkit.echo" {
            echo.push(*entry);
        }
    }
    assert_eq!(
        echo.len(),
        1,
        "BUNDLED lists the testkit echo plugin under test-harness"
    );
    let loaded = plugin::load_bundled(
        &version(),
        &vosu_core::plugin::ValidationSnapshot::default(),
        &echo,
    )
    .unwrap();
    assert_eq!(loaded[0].origin(), PackageOrigin::Bundled);
    assert!(matches!(loaded[0].execution(), Execution::Local(_)));
}
