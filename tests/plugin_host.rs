//! Loads the testkit echo plugin in-process through both paths the host
//! supports (`docs/v2/implementation-plan.md` M1 "Core plugin host" Exit):
//! `--plugin <dir>`'s `dlopen` (`vosu_core::plugin::load_development`)
//! against the real compiled `cdylib` artifact, and the explicit `BUNDLED`
//! array (`vosu_core::plugin::load_bundled`, `vosu::plugins::BUNDLED`)
//! against the same plugin's bundled-form vtable. Both exercise `create`,
//! `dispatch`, and `destroy` end to end (`docs/v2/plugin-abi.md` §1, §2,
//! §4), rather than an in-process-only fixture like `vosu-core`'s own unit
//! tests use.
#![cfg(feature = "test-harness")]
#![expect(
    clippy::disallowed_methods,
    reason = "test fixtures copy and write scratch dev-plugin directories"
)]

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::time::Duration;

use vosu_core::plugin::{self, OwnedMessage, PluginHostHandler, PluginInstance, PluginInstanceId};
use vosu_plugin_api::{CborValue, FocusChanged, Init, Message, encode, vosu_status};

const DESTROY_GRACE: Duration = Duration::from_secs(5);

/// Records every message the plugin sends back to the host (the echo
/// plugin's `on_message` calls `host.send` with the same message it was
/// `dispatch`ed), so a test can assert on it from the calling thread.
struct RecordingHandler {
    tx: SyncSender<(PluginInstanceId, Message, u64)>,
}

impl PluginHostHandler for RecordingHandler {
    fn handle(
        &self,
        instance: PluginInstanceId,
        message: Message,
        correlation_id: u64,
    ) -> vosu_status {
        let _ = self.tx.send((instance, message, correlation_id));
        vosu_status::VOSU_OK
    }
}

fn recording_handler() -> (
    Arc<RecordingHandler>,
    Receiver<(PluginInstanceId, Message, u64)>,
) {
    let (tx, rx) = sync_channel(8);
    (Arc::new(RecordingHandler { tx }), rx)
}

fn init_bytes() -> Vec<u8> {
    encode(&Init {
        plugin_id: "acme.testkit.echo".into(),
        settings: CborValue::Map(Vec::new()),
        data_dir: std::env::temp_dir(),
    })
    .unwrap()
}

fn echo_message() -> Message {
    Message::FocusChanged(FocusChanged {
        handle: 7,
        focused: true,
        foreground: None,
    })
}

/// Dispatches [`echo_message`] into `instance`, waits for both the
/// dispatch status and the echoed message the plugin sends back, and
/// asserts both match.
fn dispatch_and_expect_echo(
    instance: &PluginInstance,
    rx: &Receiver<(PluginInstanceId, Message, u64)>,
) {
    let message = echo_message();
    let correlation_id = 42;
    let reply = instance
        .dispatch(OwnedMessage::from_message(&message, correlation_id).unwrap())
        .unwrap();
    assert_eq!(
        reply.recv_timeout(Duration::from_secs(5)).unwrap(),
        vosu_status::VOSU_OK,
        "dispatch should report the echo plugin's own on_message status"
    );
    let (echoed_instance, echoed_message, echoed_correlation_id) = rx
        .recv_timeout(Duration::from_secs(5))
        .expect("the echo plugin should have sent the message back to the host");
    assert_eq!(echoed_instance, instance.id());
    assert_eq!(echoed_message, message);
    assert_eq!(echoed_correlation_id, correlation_id);
}

// --- `--plugin <dir>`: real `dlopen` of the compiled cdylib --------------

/// The running host's target triple, matching `[targets.<triple>]` keys
/// (`docs/v2/plugin-abi.md` §8). Mirrors `vosu_core::plugin::registry`'s
/// private `current_target_triple`, which this external test has no access
/// to — only the two released targets exist today.
fn current_target_triple() -> &'static str {
    if cfg!(all(target_arch = "x86_64", target_os = "windows")) {
        "x86_64-pc-windows-msvc"
    } else {
        "x86_64-unknown-linux-gnu"
    }
}

/// Locates the already-built `vosu-plugin-testkit` `cdylib` artifact
/// without needing `CARGO_BIN_EXE_*` (cdylibs get no such env var, only
/// `[[bin]]` targets do) or a `CARGO_TARGET_DIR`/profile env var read at
/// runtime. `Cargo.toml`'s `test-harness` feature lists
/// `vosu-plugin-testkit` as a (normally optional) dependency, and that
/// crate's `[lib] crate-type = ["rlib", "cdylib"]` means Cargo always
/// builds *both* artifacts for it once it is built at all — so running
/// this test under `--features test-harness` has already built the
/// `cdylib` before this test binary starts. This looks directly in
/// `deps/`, not the profile root one level up: Cargo's `uplift_to` only
/// copies a dependency's cdylib to the profile directory when it is also a
/// root unit built in Build mode, and under `cargo test` every root is a
/// Test unit, so a dependency cdylib like this one is never uplifted —
/// only ever written to `deps/`, under its plain (unhashed) filename, on
/// every platform and target-dir layout. This test binary's own
/// `current_exe()` already sits at
/// `<target-dir>/[<target-triple>/]<profile>/deps/<this-binary>-<hash>`,
/// so its own parent directory is exactly that `deps/` directory — no env
/// var needed, and it can never point at a stale target dir or a copy
/// built without `test-harness`.
fn testkit_cdylib_path() -> PathBuf {
    let exe = std::env::current_exe().expect("this test binary has a current_exe path");
    let deps_dir = exe
        .parent()
        .expect("current_exe sits in <target-dir>/<profile>/deps/")
        .to_path_buf();
    let filename = if cfg!(target_os = "windows") {
        "vosu_plugin_testkit.dll"
    } else if cfg!(target_os = "macos") {
        "libvosu_plugin_testkit.dylib"
    } else {
        "libvosu_plugin_testkit.so"
    };
    let path = deps_dir.join(filename);
    assert!(
        path.is_file(),
        "expected the testkit cdylib to already be built at {path:?}; \
         run this test as `cargo test --features test-harness`, which \
         builds vosu-plugin-testkit (and its cdylib) as a side effect of \
         vosu depending on it under that feature"
    );
    path
}

/// Writes a `--plugin <dir>`-style directory: a copy of the testkit
/// `cdylib` under `lib/`, and a `manifest.toml` whose
/// `[targets.<host-triple>]` points at it — the same shape
/// `vosu_core::plugin::registry::load_development` expects from a real
/// development plugin directory.
fn development_plugin_dir() -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("creating a temp dev-plugin directory");
    let lib_dir = dir.path().join("lib");
    std::fs::create_dir_all(&lib_dir).expect("creating the dev-plugin dir's lib/ subdirectory");
    let source = testkit_cdylib_path();
    let filename = source
        .file_name()
        .expect("the testkit cdylib path has a file name");
    std::fs::copy(&source, lib_dir.join(filename))
        .expect("copying the testkit cdylib into the dev-plugin directory");
    std::fs::write(
        dir.path().join("manifest.toml"),
        format!(
            "kind='plugin'\nmanifest_version=1\nid='acme.testkit.echo'\nname='Testkit Echo'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n\n[targets.{}]\nlibrary='lib/{}'\n",
            current_target_triple(),
            filename.to_str().expect("the cdylib filename is UTF-8"),
        ),
    )
    .expect("writing manifest.toml");
    dir
}

#[test]
fn development_plugin_dlopen_create_dispatch_destroy() {
    let dir = development_plugin_dir();
    let loaded = plugin::load_development(dir.path(), true)
        .expect("loading the dev-plugin directory this test just wrote");
    let (handler, rx) = recording_handler();
    let instance = PluginInstance::create(
        PluginInstanceId::new(1),
        &loaded,
        &init_bytes(),
        handler,
        None,
    )
    .expect("creating the dlopen'd echo plugin instance");

    dispatch_and_expect_echo(&instance, &rx);

    instance
        .destroy(DESTROY_GRACE)
        .expect("destroying the dlopen'd echo plugin instance");
}

#[test]
fn development_plugin_instance_outlives_a_dropped_loaded_plugin() {
    let dir = development_plugin_dir();
    let loaded = plugin::load_development(dir.path(), true)
        .expect("loading the dev-plugin directory this test just wrote");
    let (handler, rx) = recording_handler();
    let instance = PluginInstance::create(
        PluginInstanceId::new(5),
        &loaded,
        &init_bytes(),
        handler,
        None,
    )
    .expect("creating the dlopen'd echo plugin instance");

    // Drop the `LoadedPlugin` (and the temp directory it was `dlopen`ed
    // from) right away: the P0 finding had `PluginInstance` hold no
    // reference to the library at all, so the next `dispatch`/`destroy`
    // ran unmapped code the moment the loader value went out of scope.
    // `PluginInstance` now clones `LoadedPlugin`'s `Arc<Library>`, so it
    // must keep working regardless.
    drop(loaded);
    drop(dir);

    dispatch_and_expect_echo(&instance, &rx);

    instance
        .destroy(DESTROY_GRACE)
        .expect("destroying the dlopen'd echo plugin instance");
}

#[test]
fn release_builds_refuse_development_plugins() {
    let dir = development_plugin_dir();
    match plugin::load_development(dir.path(), false) {
        Err(plugin::HostError::InProcessRefused { .. }) => {}
        Err(other) => panic!("expected InProcessRefused, got {other}"),
        Ok(_) => panic!("expected a release build to refuse --plugin <dir>"),
    }
}

#[test]
fn development_settings_schema_is_read_from_the_manifest_dir() {
    let dir = tempfile::tempdir().expect("creating a temp dev-plugin directory");
    let lib_dir = dir.path().join("lib");
    std::fs::create_dir_all(&lib_dir).expect("creating the dev-plugin dir's lib/ subdirectory");
    let source = testkit_cdylib_path();
    let filename = source
        .file_name()
        .expect("the testkit cdylib path has a file name");
    std::fs::copy(&source, lib_dir.join(filename))
        .expect("copying the testkit cdylib into the dev-plugin directory");
    std::fs::write(
        dir.path().join("settings.schema.json"),
        r#"[{"uniqueID":"RATE","type":"number","value":42.0}]"#,
    )
    .expect("writing settings.schema.json");
    std::fs::write(
        dir.path().join("manifest.toml"),
        format!(
            "kind='plugin'\nmanifest_version=1\nid='acme.testkit.echo'\nname='Testkit Echo'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\nsettings_schema='settings.schema.json'\n\n[targets.{}]\nlibrary='lib/{}'\n",
            current_target_triple(),
            filename.to_str().expect("the cdylib filename is UTF-8"),
        ),
    )
    .expect("writing manifest.toml");

    let loaded = plugin::load_development(dir.path(), true)
        .expect("loading the dev-plugin directory this test just wrote");
    let defaults = loaded
        .settings_schema()
        .expect("manifest declared settings_schema")
        .defaults();
    assert_eq!(defaults.get("RATE"), Some(&serde_json::Value::from(42.0)));
}

// --- `BUNDLED`: the same plugin's linked-in, non-`dlopen`ed form ---------

#[test]
fn bundled_echo_create_dispatch_destroy() {
    let loaded =
        plugin::load_bundled(vosu::plugins::BUNDLED).expect("loading vosu::plugins::BUNDLED");
    let echo = loaded
        .iter()
        .find(|plugin| plugin.manifest().id == "vosu.testkit.echo")
        .expect("BUNDLED should list the testkit echo plugin under test-harness");
    let (handler, rx) = recording_handler();
    let instance = PluginInstance::create(
        PluginInstanceId::new(2),
        echo,
        &encode(&Init {
            plugin_id: echo.manifest().id.clone(),
            settings: CborValue::Map(Vec::new()),
            data_dir: std::env::temp_dir(),
        })
        .unwrap(),
        handler,
        None,
    )
    .expect("creating the bundled echo plugin instance");

    dispatch_and_expect_echo(&instance, &rx);

    instance
        .destroy(DESTROY_GRACE)
        .expect("destroying the bundled echo plugin instance");
}

// --- `PluginHost`: M1 host services (settings, logging, timers) end to end ---

/// A console sink that records every line `vosu_core::logging::Logger`
/// writes, so this test can assert on plugin-id attribution and the
/// dropped-record summary without touching real stdout.
#[derive(Default)]
struct RecordingConsole {
    lines: std::sync::Mutex<Vec<String>>,
}

impl vosu_core::logging::ConsoleSink for RecordingConsole {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, text: &str) -> std::io::Result<()> {
        self.lines
            .lock()
            .unwrap_or_else(|poison| poison.into_inner())
            .push(text.to_owned());
        Ok(())
    }
    fn clear_progress(&self, _lines: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _text: &str) -> std::io::Result<()> {
        Ok(())
    }
}

/// End to end through a real `PluginHost` and a real plugin instance
/// (`vosu-plugin-testkit`'s `host_services` plugin, not a bare echo): on
/// `create` the plugin itself requests a one-shot timer (§6.11); once
/// `PluginHost`'s `TimerRegistry` fires it and delivers `TIMER_FIRED`
/// back through this same instance's dispatch path, the plugin cancels
/// that timer and sends a burst of `LOG_RECORD`s past the per-instance
/// rate limit (§6.10) — exercising `init.settings` (§6.9), timers, and
/// rate-limited, plugin-id-attributed logging through one `PluginHost`
/// instance, the M1 Exit list's "log rate limiting, timer cancellation".
#[test]
fn plugin_host_runs_the_host_services_testkit_plugin_end_to_end() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime for the test");
    let _enter = runtime.enter();

    let console = Arc::new(RecordingConsole::default());
    let log_dir = tempfile::tempdir().expect("creating a temp log dir");
    let logger = vosu_core::logging::Logger::new(
        log_dir.path(),
        vosu_core::logging::LoggerOptions {
            version: "0.1.0".to_owned(),
            debug_log: true,
            queue_capacity: 4096,
        },
        console.clone(),
    )
    .expect("constructing the logger");

    let config_dir = tempfile::tempdir().expect("creating a temp config dir");
    let config_path = config_dir.path().join("vosu.toml");
    let config = runtime
        .block_on(vosu_core::config::ConfigStore::initialize(&config_path))
        .expect("initializing a throwaway ConfigStore");
    let storage: Arc<dyn vosu_core::plugin::settings::SettingsStorage> = Arc::new(
        vosu_core::plugin::settings::ConfigSettingsBridgeB2::new(Arc::new(config)),
    );
    let settings = Arc::new(vosu_core::plugin::settings::PluginSettingsService::new(
        storage,
    ));

    let host =
        vosu_core::plugin::PluginHost::new(logger.clone(), settings, runtime.handle().clone());

    let manifest = "kind='plugin'\nmanifest_version=1\nid='acme.testkit.host-services'\nname='Host Services Testkit'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n";
    let loaded = plugin::load_bundled(&[plugin::BundledPlugin {
        manifest,
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: vosu_plugin_testkit::host_services::vosu_plugin_vtable,
    }])
    .expect("loading the host-services testkit plugin");

    let data_dir = tempfile::tempdir().expect("creating a temp plugin data dir");
    let instance_id = PluginInstanceId::new(1);
    host.create_instance(instance_id, &loaded[0], data_dir.path().to_path_buf(), None)
        .expect("creating the host-services testkit plugin instance");

    // Waits for the dropped-record summary: it only appears once the
    // plugin's burst (sent only in reaction to `TIMER_FIRED`) has run
    // past the rate limit, so its presence alone proves the timer was
    // set at `create`, fired, and was delivered back to the plugin.
    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        runtime
            .block_on(logger.flush())
            .expect("flushing the logger");
        if console
            .lines
            .lock()
            .unwrap()
            .iter()
            .any(|line| line.contains("record(s) dropped"))
        {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "expected a dropped-record summary within 5s of the timer firing"
        );
        std::thread::sleep(Duration::from_millis(50));
    }

    let lines = console.lines.lock().unwrap();
    assert!(
        lines
            .iter()
            .any(|line| line.contains("plugin acme.testkit.host-services: burst")),
        "expected plugin-id-attributed burst log lines, got: {lines:?}"
    );
    drop(lines);

    host.destroy_all(DESTROY_GRACE);
}

// --- `vosu-core` `test-harness` memory seam (F3) ------------------------

/// A [`vosu_sys::process::ProcessMemory`] backed by a plain in-memory
/// byte buffer, standing in for a real OS process: `address` indexes
/// straight into `bytes`, exactly the way
/// `vosu_core::plugin::test_support::set_session_memory` injects it for
/// `PluginHost::session_memory` to pick up in place of opening a real
/// `pid`.
struct InjectedBuffer {
    bytes: Vec<u8>,
}

impl vosu_sys::process::ProcessMemory for InjectedBuffer {
    fn read_exact(
        &self,
        address: usize,
        dst: &mut [u8],
    ) -> Result<(), vosu_sys::process::MemoryError> {
        let end = address.checked_add(dst.len()).ok_or(
            vosu_sys::process::MemoryError::AddressOverflow {
                base: address,
                offset: dst.len(),
            },
        )?;
        let slice =
            self.bytes
                .get(address..end)
                .ok_or_else(|| vosu_sys::process::MemoryError::Read {
                    address,
                    len: dst.len(),
                    message: "out of injected buffer bounds".to_owned(),
                })?;
        dst.copy_from_slice(slice);
        Ok(())
    }

    fn read_many(
        &self,
        reads: &mut [vosu_sys::process::ReadRequest<'_>],
    ) -> Result<(), vosu_sys::process::MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(
        &self,
    ) -> Result<Vec<vosu_sys::process::MemoryRegion>, vosu_sys::process::MemoryError> {
        Ok(vec![vosu_sys::process::MemoryRegion {
            address: 0,
            size: self.bytes.len(),
        }])
    }
}

/// F3's own acceptance test: a real `PluginHost` and a real plugin
/// instance (`vosu-plugin-testkit`'s read-plan plugin) answer a
/// `VOSU_READ_PLAN_SYNC` submission against process memory
/// `vosu_core::plugin::test_support::set_session_memory` injected for a
/// synthetic pid that names no real OS process at all — proving the
/// `test_support` seam, not just `host::read_plans`'s own private hook,
/// reaches a real `PluginHost::session_memory` lookup end to end.
#[test]
fn injected_session_memory_answers_a_sync_plan() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();

    let console = Arc::new(RecordingConsole::default());
    let log_dir = tempfile::tempdir().expect("creating a temp log dir");
    let logger = vosu_core::logging::Logger::new(
        log_dir.path(),
        vosu_core::logging::LoggerOptions {
            version: "0.1.0".to_owned(),
            debug_log: true,
            queue_capacity: 4096,
        },
        console.clone(),
    )
    .expect("constructing the logger");

    let config_dir = tempfile::tempdir().expect("creating a temp config dir");
    let config_path = config_dir.path().join("vosu.toml");
    let config = runtime
        .block_on(vosu_core::config::ConfigStore::initialize(&config_path))
        .expect("initializing a throwaway ConfigStore");
    let storage: Arc<dyn vosu_core::plugin::settings::SettingsStorage> = Arc::new(
        vosu_core::plugin::settings::ConfigSettingsBridgeB2::new(Arc::new(config)),
    );
    let settings = Arc::new(vosu_core::plugin::settings::PluginSettingsService::new(
        storage,
    ));
    let host = vosu_core::plugin::PluginHost::new(logger, settings, runtime.handle().clone());

    let plugin_id = "acme.testkit.readplan.injected";
    let (events, commands) = vosu_plugin_testkit::read_plan::register(plugin_id);
    let manifest = format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=['proc-injected']\n"
    );
    let loaded = plugin::load_bundled(&[plugin::BundledPlugin {
        manifest: &manifest,
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: vosu_plugin_testkit::read_plan::vosu_plugin_vtable,
    }])
    .expect("loading the read-plan testkit plugin");

    let data_dir = tempfile::tempdir().expect("creating a temp plugin data dir");
    host.create_instance(
        PluginInstanceId::new(1),
        &loaded[0],
        data_dir.path().to_path_buf(),
        None,
    )
    .expect("creating the read-plan testkit instance");

    // A synthetic pid naming no real OS process: `test_support` is the
    // only thing that makes this session's memory resolvable.
    let pid: u32 = 0x7FFF_FFFE;
    let marker: u64 = 0xABCD_EF01_2345_6789;
    let memory: Arc<dyn vosu_sys::process::ProcessMemory> = Arc::new(InjectedBuffer {
        bytes: marker.to_le_bytes().to_vec(),
    });
    vosu_core::plugin::test_support::set_session_memory(
        pid,
        memory,
        vosu_sys::process::ProcessBitness::X64,
    );

    host.process_appeared(vosu_core::plugin::ProcessRecord {
        pid,
        name: "proc-injected".to_owned(),
        path: "/games/proc-injected".to_owned(),
        command_line: "proc-injected".to_owned(),
        bitness: vosu_plugin_api::Bitness::X64,
        start_time: 1,
        argv: None,
    });

    let handle = match events
        .recv_timeout(Duration::from_secs(5))
        .expect("SESSION_ATTACHED")
    {
        vosu_plugin_testkit::read_plan::ReadPlanEvent::Attached { handle } => handle,
        other => panic!("expected Attached, got {other:?}"),
    };

    commands
        .send(
            vosu_plugin_testkit::read_plan::ReadPlanCommand::SubmitSync {
                plan: 1,
                handle,
                root: vosu_plugin_api::PlanBase::Absolute(0),
                stages: vec![vosu_plugin_api::Stage {
                    base: vosu_plugin_api::StageBase::Root,
                    base_offset: 0,
                    ops: vec![vosu_plugin_api::PlanOp {
                        offset: 0,
                        length: 8,
                    }],
                }],
            },
        )
        .expect("sending the sync submit command");

    match events
        .recv_timeout(Duration::from_secs(5))
        .expect("sync result")
    {
        vosu_plugin_testkit::read_plan::ReadPlanEvent::SyncResult(Ok(result)) => {
            assert_eq!(result.plan, 1);
            match &result.results[0] {
                vosu_plugin_api::ReadResult::Ok(bytes) => {
                    assert_eq!(bytes.as_slice(), &marker.to_le_bytes());
                }
                other => panic!("expected Ok, got {other:?}"),
            }
        }
        other => panic!("expected an Ok SyncResult, got {other:?}"),
    }

    vosu_core::plugin::test_support::clear_session_memory(pid);
    host.destroy_all(DESTROY_GRACE);
}

/// F4 review item 5 / ReviewF4fix's own P2: the previous version of this
/// test hooked a synthetic pid (`0x7FFF_FFFD`) that names no real OS
/// process at all, so it could never discriminate the fix from its own
/// absence: `build_module_regions_real` would fail to snapshot that pid
/// either way (no such process exists to snapshot), falling back to the
/// exact same `UnresolvedModuleRegions`/`module_not_found` answer the
/// `test-harness` gate itself produces. Hooking `std::process::id()` (a
/// pid that genuinely exists — this test process) with a module name
/// that genuinely resolves in it (`libc.so.6`/`ntdll.dll`, below) makes
/// the two branches disagree: with the `test-harness` gate active,
/// `is_hooked_for_tests(pid)` still substitutes `UnresolvedModuleRegions`
/// for this hooked pid, so the scan answers `module_not_found` despite
/// the module genuinely being loaded; without it (reverted to plain
/// `cfg(test)`, which this `test-harness`-only integration test crate
/// never satisfies), `build_module_regions_real` snapshots this real
/// process's real module table, resolves the module for real, and the
/// scan instead runs against the tiny injected buffer at that real
/// address — never `module_not_found`. Verified by reverting
/// `host/scans.rs`'s `build_module_regions` gate to plain `cfg(test)`:
/// this test then fails (the result is no longer `module_not_found`),
/// confirming it actually depends on the `test-harness` branch.
#[cfg(target_os = "linux")]
const REAL_MODULE_NAME: &str = "libc.so.6";
#[cfg(windows)]
const REAL_MODULE_NAME: &str = "ntdll.dll";

#[cfg(any(target_os = "linux", windows))]
#[test]
fn injected_session_memory_resolves_no_modules_for_a_scan() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();

    let console = Arc::new(RecordingConsole::default());
    let log_dir = tempfile::tempdir().expect("creating a temp log dir");
    let logger = vosu_core::logging::Logger::new(
        log_dir.path(),
        vosu_core::logging::LoggerOptions {
            version: "0.1.0".to_owned(),
            debug_log: true,
            queue_capacity: 4096,
        },
        console.clone(),
    )
    .expect("constructing the logger");

    let config_dir = tempfile::tempdir().expect("creating a temp config dir");
    let config_path = config_dir.path().join("vosu.toml");
    let config = runtime
        .block_on(vosu_core::config::ConfigStore::initialize(&config_path))
        .expect("initializing a throwaway ConfigStore");
    let storage: Arc<dyn vosu_core::plugin::settings::SettingsStorage> = Arc::new(
        vosu_core::plugin::settings::ConfigSettingsBridgeB2::new(Arc::new(config)),
    );
    let settings = Arc::new(vosu_core::plugin::settings::PluginSettingsService::new(
        storage,
    ));
    let host = vosu_core::plugin::PluginHost::new(logger, settings, runtime.handle().clone());

    let plugin_id = "acme.testkit.scan.injected";
    let (events, commands) = vosu_plugin_testkit::scan::register(plugin_id);
    let manifest = format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=['proc-scan-injected']\n"
    );
    let loaded = plugin::load_bundled(&[plugin::BundledPlugin {
        manifest: &manifest,
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: vosu_plugin_testkit::scan::vosu_plugin_vtable,
    }])
    .expect("loading the scan testkit plugin");

    let data_dir = tempfile::tempdir().expect("creating a temp plugin data dir");
    host.create_instance(
        PluginInstanceId::new(1),
        &loaded[0],
        data_dir.path().to_path_buf(),
        None,
    )
    .expect("creating the scan testkit instance");

    // A pid that genuinely exists (this test process itself), so a real
    // module-table snapshot, were the `test-harness` gate not substituting
    // `UnresolvedModuleRegions` for it, would genuinely succeed and find
    // `REAL_MODULE_NAME` — unlike the previous version's synthetic pid,
    // which could never tell the two branches apart (module doc comment).
    let pid: u32 = std::process::id();
    let memory: Arc<dyn vosu_sys::process::ProcessMemory> = Arc::new(InjectedBuffer {
        bytes: vec![0xDE, 0xAD, 0xBE, 0xEF],
    });
    vosu_core::plugin::test_support::set_session_memory(
        pid,
        memory,
        vosu_sys::process::ProcessBitness::X64,
    );

    host.process_appeared(vosu_core::plugin::ProcessRecord {
        pid,
        name: "proc-scan-injected".to_owned(),
        path: "/games/proc-scan-injected".to_owned(),
        command_line: "proc-scan-injected".to_owned(),
        bitness: vosu_plugin_api::Bitness::X64,
        start_time: 1,
        argv: None,
    });

    let handle = match events
        .recv_timeout(Duration::from_secs(5))
        .expect("SESSION_ATTACHED")
    {
        vosu_plugin_testkit::scan::ScanEvent::Attached { handle } => handle,
        other => panic!("expected Attached, got {other:?}"),
    };

    commands
        .send(vosu_plugin_testkit::scan::ScanCommand {
            correlation_id: 11,
            session: handle,
            module: Some(REAL_MODULE_NAME.to_owned()),
            pattern: "DE AD BE EF".to_owned(),
            non_zero_mask: false,
            all_matches: true,
        })
        .expect("sending the scan command");

    let mut status = None;
    let mut result = None;
    for _ in 0..2 {
        match events
            .recv_timeout(Duration::from_secs(5))
            .expect("submit status or SCAN_RESULT")
        {
            vosu_plugin_testkit::scan::ScanEvent::SubmitStatus(value) => status = Some(value),
            vosu_plugin_testkit::scan::ScanEvent::Result {
                correlation_id,
                result: value,
            } => result = Some((correlation_id, value)),
            other => panic!("unexpected event {other:?}"),
        }
    }
    assert_eq!(status.expect("SubmitStatus"), vosu_status::VOSU_OK);
    let (correlation_id, result) = result.expect("SCAN_RESULT");
    assert_eq!(correlation_id, 11);
    assert!(result.addresses.is_empty());
    assert_eq!(
        result.error.expect("module_not_found error").code,
        "module_not_found",
        "the test-harness gate must substitute UnresolvedModuleRegions for this hooked, \
         really-existing pid — not resolve REAL_MODULE_NAME for real"
    );

    vosu_core::plugin::test_support::clear_session_memory(pid);
    host.destroy_all(DESTROY_GRACE);
}
