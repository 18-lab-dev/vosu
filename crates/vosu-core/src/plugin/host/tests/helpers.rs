//! Shared fixtures: a test `PluginHost`/logger/settings wiring, session
//! testkit manifests, and `ProcessRecord` builders every other submodule
//! here uses. Split out of `tests.rs` so each topic module stays under
//! the maintained-file line limit (`docs/architecture/source-map.md`).

use std::time::Duration;

use serde_json::{Map, Value};
use tokio::runtime::Handle;
use vosu_plugin_api::Bitness;
use vosu_plugin_testkit::session::{self, SessionEvent, SessionPluginConfig};

use crate::logging::{ConsoleSink, LoggerOptions};
use crate::plugin::host::*;
use crate::plugin::settings::{SettingsError, SettingsStorage};
use crate::plugin::{focus, registry};

pub(super) struct DummyStorage;
impl SettingsStorage for DummyStorage {
    fn load(&self, _plugin_id: &str) -> Result<Map<String, Value>, SettingsError> {
        Ok(Map::new())
    }
    fn store(&self, _plugin_id: &str, _values: &Map<String, Value>) -> Result<(), SettingsError> {
        Ok(())
    }
}

pub(super) struct SilentConsole;
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

/// Returns the backing `TempDir` too, so the caller keeps it alive
/// for exactly its own test's scope instead of leaking it into
/// `/tmp` forever. Uses [`focus::SyntheticForegroundSource`] with no
/// foreground owner (equivalent in every test outcome to
/// `OsForegroundSource` on every non-Windows build this gate runs on,
/// and deterministic instead of depending on real OS focus) and the
/// real 5-second identification timeout; tests that need a different
/// foreground or a short timeout use [`test_host_with`] instead.
pub(in crate::plugin::host) fn test_host(runtime: Handle) -> (tempfile::TempDir, Arc<PluginHost>) {
    test_host_with(
        runtime,
        Arc::new(focus::SyntheticForegroundSource::new(None)),
        crate::plugin::session::IDENTIFICATION_TIMEOUT,
    )
}

/// Same as [`test_host`], injecting `foreground` and
/// `identification_timeout` through [`PluginHost::new_for_tests`].
pub(super) fn test_host_with(
    runtime: Handle,
    foreground: Arc<dyn ForegroundSource>,
    identification_timeout: Duration,
) -> (tempfile::TempDir, Arc<PluginHost>) {
    let log_dir = tempfile::tempdir().unwrap();
    let logger = Logger::new(
        log_dir.path(),
        LoggerOptions {
            version: "0.1.0".to_owned(),
            debug_log: false,
            queue_capacity: 64,
        },
        Arc::new(SilentConsole),
    )
    .unwrap();
    let settings = Arc::new(PluginSettingsService::new(Arc::new(DummyStorage)));
    (
        log_dir,
        PluginHost::new_for_tests(
            logger,
            settings,
            runtime,
            foreground,
            identification_timeout,
        ),
    )
}

/// Bounds every `recv` this phase's session/focus tests make on a
/// [`session::SessionEvent`] channel: generous enough that a slow CI
/// machine never flakes, but still a hard failure (not a hang) when
/// the host wires something up wrong.
pub(super) const RECV_TIMEOUT: Duration = Duration::from_secs(5);

/// A minimal game-plugin manifest with a `[match]` table matching
/// every name in `process_names` exactly (§6.1).
pub(super) fn session_manifest(plugin_id: &str, process_names: &[&str]) -> String {
    let names = process_names
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=[{names}]\n"
    )
}

/// Registers a [`session::SessionPlugin`] instance under `plugin_id`,
/// matching every name in `process_names`, with `config`. Creates it
/// synchronously (`PluginHost::create_instance` runs the plugin's own
/// `create` before returning), so the returned event [`Receiver`] is
/// ready to observe `SESSION_ATTACHED` the moment the caller drives a
/// matching [`ProcessRecord`] through [`PluginHost::process_appeared`].
pub(super) fn create_session_instance(
    host: &Arc<PluginHost>,
    id: PluginInstanceId,
    plugin_id: &str,
    process_names: &[&str],
    config: SessionPluginConfig,
) -> std::sync::mpsc::Receiver<SessionEvent> {
    create_session_instance_from_manifest(
        host,
        id,
        plugin_id,
        config,
        &session_manifest(plugin_id, process_names),
    )
}

/// Same as [`create_session_instance`], with a caller-supplied
/// `[match]` table (`manifest_text`): lets a test match on
/// `process_path_patterns`/`command_line_patterns` instead of exact
/// `process_names`, e.g. to shadow another plugin's `process_names`
/// entry without tripping `MatcherEngine::register`'s own D18
/// static-overlap refusal (which only compares `process_names`/
/// `comm_hints` across plugins, not path/command-line patterns).
pub(super) fn create_session_instance_from_manifest(
    host: &Arc<PluginHost>,
    id: PluginInstanceId,
    plugin_id: &str,
    config: SessionPluginConfig,
    manifest_text: &str,
) -> std::sync::mpsc::Receiver<SessionEvent> {
    let events = session::register_with_config(plugin_id, config);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: manifest_text,
        vtable: session::vosu_plugin_vtable,
    }])
    .expect("loading the session testkit plugin");
    let data_dir = tempfile::tempdir().unwrap();
    host.create_instance(id, &loaded[0], data_dir.path().to_path_buf(), None)
        .expect("creating the session testkit instance");
    events
}

/// A manifest whose `[match]` table matches by `process_path_patterns`
/// instead of `process_names` — see
/// [`create_session_instance_from_manifest`]'s doc comment.
pub(super) fn session_manifest_by_path_suffix(plugin_id: &str, name: &str) -> String {
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_path_patterns=['.*/{name}']\n"
    )
}

pub(in crate::plugin::host) fn test_process(pid: u32, name: &str) -> ProcessRecord {
    ProcessRecord {
        pid,
        name: name.to_owned(),
        path: format!("/games/{name}"),
        command_line: name.to_owned(),
        bitness: Bitness::X64,
        start_time: 1,
        argv: None,
    }
}
