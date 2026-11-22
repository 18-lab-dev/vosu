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

use std::path::{Path, PathBuf};
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
/// `cdylib` before this test binary starts. This test binary's own
/// `current_exe()` already sits at
/// `<target-dir>/[<target-triple>/]<profile>/deps/<this-binary>-<hash>`,
/// so its grandparent is exactly the profile directory cargo copies every
/// built `cdylib` into, on every platform and target-dir layout — no env
/// var needed, and it can never point at a stale target dir.
fn testkit_cdylib_path() -> PathBuf {
    let exe = std::env::current_exe().expect("this test binary has a current_exe path");
    let profile_dir = exe
        .parent()
        .and_then(Path::parent)
        .expect("current_exe sits two levels under <target-dir>/<profile>/deps/")
        .to_path_buf();
    let filename = if cfg!(target_os = "windows") {
        "vosu_plugin_testkit.dll"
    } else if cfg!(target_os = "macos") {
        "libvosu_plugin_testkit.dylib"
    } else {
        "libvosu_plugin_testkit.so"
    };
    let path = profile_dir.join(filename);
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
            "kind='plugin'\nmanifest_version=1\nid='acme.testkit.echo'\nname='Testkit Echo'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[targets.{}]\nlibrary='lib/{}'\n",
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
fn release_builds_refuse_development_plugins() {
    let dir = development_plugin_dir();
    match plugin::load_development(dir.path(), false) {
        Err(plugin::HostError::InProcessRefused { .. }) => {}
        Err(other) => panic!("expected InProcessRefused, got {other}"),
        Ok(_) => panic!("expected a release build to refuse --plugin <dir>"),
    }
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
