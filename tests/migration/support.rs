//! Shared helpers for the M8-L2 X0 adversarial migration integration suite.

use std::collections::BTreeMap;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::{Body, Bytes, to_bytes};
use axum::http::{Method, Request, StatusCode};
use serde_json::Value;
use vosu_core::catalog::sources::{SourceLifecycle, SourceStoreError};
use vosu_core::config::ConfigStore;
use vosu_core::migration::{
    MigrationOutcome, MigrationSchemas, OSU_PLUGIN_ID, OVERLAY_HOST_PLUGIN_ID, run_local_migration,
};
use vosu_core::plugin::settings::PluginSettingsStore;
use vosu_core::tools::ToolRegistry;
use vosu_plugin_api::manifest::settings::SettingsSchema;
use vosu_proptest::bounded;
use vosu_sys::platform::{BuildChannel, PathResolver, Paths};

/// Constructs the pre-rename legacy environment filename dynamically so tracked
/// test files stay free of old-brand literals while still testing real on-disk
/// pre-rename migration inputs.
pub(crate) fn pre_rename_env_filename() -> String {
    format!("{}{}.env", "to", "su")
}

/// Extracts the real bundled `settings.schema.json` documents for `vosu.osu`
/// and `vosu.overlayhost` directly from `vosu::BUNDLE` / `vosu::BUNDLED`,
/// matching production `initialize_startup_storage`.
pub(crate) fn bundled_migration_schemas() -> MigrationSchemas {
    let parse_schema = |plugin_id: &str| -> SettingsSchema {
        let index = vosu::BUNDLE
            .plugin_ids
            .iter()
            .position(|id| *id == plugin_id)
            .unwrap_or_else(|| panic!("bundled plugin {plugin_id} missing from vosu::BUNDLE"));
        let text = vosu::BUNDLED
            .get(index)
            .and_then(|entry| entry.settings_schema)
            .unwrap_or_else(|| panic!("bundled plugin {plugin_id} declares no settings schema"));
        SettingsSchema::parse(text)
            .unwrap_or_else(|error| panic!("bundled plugin {plugin_id} schema invalid: {error}"))
    };
    MigrationSchemas {
        osu: parse_schema(OSU_PLUGIN_ID),
        overlay_host: parse_schema(OVERLAY_HOST_PLUGIN_ID),
    }
}

/// No-op `SourceLifecycle` for opening the real `SourceStore` in tests.
pub(crate) struct NoopSourceLifecycle;

impl SourceLifecycle for NoopSourceLifecycle {
    fn reconcile_sources(&self, _identities: &[String]) -> Result<(), SourceStoreError> {
        Ok(())
    }
}

/// Executes the exact production `BuildChannel::Release` startup storage
/// sequence: `PluginSettingsStore::open` -> `run_local_migration` ->
/// `ConfigStore::initialize`.
pub(crate) async fn run_release_startup(
    config_root: &Path,
    data_root: &Path,
) -> io::Result<(MigrationOutcome, ConfigStore, Arc<PluginSettingsStore>)> {
    let settings_store = Arc::new(PluginSettingsStore::open(config_root));
    let schemas = bundled_migration_schemas();
    let cfg = config_root.to_path_buf();
    let data = data_root.to_path_buf();
    let store = Arc::clone(&settings_store);
    let outcome = tokio::task::spawn_blocking(move || {
        run_local_migration(&cfg, &data, BuildChannel::Release, &schemas, &store)
    })
    .await
    .map_err(|error| io::Error::other(format!("migration task failed: {error}")))?
    .map_err(|error| io::Error::other(error.to_string()))?;

    let config_store = bounded!(ConfigStore::initialize(config_root.join("vosu.toml")))?;
    Ok((outcome, config_store, settings_store))
}

/// Executes the production `BuildChannel::Prerelease` startup storage sequence:
/// confirms `run_local_migration` is a strict no-op, then runs
/// `ConfigStore::initialize_prerelease` against the normal root read-only.
pub(crate) async fn run_prerelease_startup(
    alpha_paths: &Paths,
    normal_config_root: &Path,
) -> io::Result<(MigrationOutcome, ConfigStore, Arc<PluginSettingsStore>)> {
    let settings_store = Arc::new(PluginSettingsStore::open(alpha_paths.config()));
    let schemas = bundled_migration_schemas();
    let cfg = alpha_paths.config().to_path_buf();
    let data = alpha_paths.data().to_path_buf();
    let store = Arc::clone(&settings_store);
    let outcome = tokio::task::spawn_blocking(move || {
        run_local_migration(&cfg, &data, BuildChannel::Prerelease, &schemas, &store)
    })
    .await
    .map_err(|error| io::Error::other(format!("prerelease migration task failed: {error}")))?
    .map_err(|error| io::Error::other(error.to_string()))?;

    let config_store = bounded!(ConfigStore::initialize_prerelease(
        alpha_paths.config().join("vosu.toml"),
        normal_config_root.join("vosu.toml"),
    ))?;
    Ok((outcome, config_store, settings_store))
}

/// Copies the realistic synthetic 1.x fixture tree from
/// `tests/fixtures/migration/realistic_1x/` into `(config_root, data_root)`,
/// staging `pre_rename.env.fixture` under the real pre-rename legacy filename.
pub(crate) fn seed_realistic_1x_fixture(config_root: &Path, data_root: &Path) {
    let fixture_root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
        .join("migration")
        .join("realistic_1x");
    copy_dir_recursive(&fixture_root.join("config"), config_root);
    copy_dir_recursive(&fixture_root.join("data"), data_root);
}

fn copy_dir_recursive(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).unwrap();
    for entry in fs::read_dir(src).unwrap() {
        let entry = entry.unwrap();
        let src_path = entry.path();
        let name = entry.file_name();
        let dst_name = if name == "pre_rename.env.fixture" {
            OsString::from(pre_rename_env_filename())
        } else {
            name
        };
        let dst_path = dst.join(dst_name);
        if entry.file_type().unwrap().is_dir() {
            copy_dir_recursive(&src_path, &dst_path);
        } else {
            fs::copy(&src_path, &dst_path).unwrap();
        }
    }
}

/// Captures an ordered byte snapshot of every regular file under `root`,
/// optionally excluding a top-level child directory name (e.g. `"prerelease"`).
pub(crate) fn snapshot_tree(root: &Path, exclude_dir: Option<&str>) -> BTreeMap<PathBuf, Vec<u8>> {
    fn visit(
        base: &Path,
        current: &Path,
        exclude_dir: Option<&str>,
        out: &mut BTreeMap<PathBuf, Vec<u8>>,
    ) {
        let Ok(entries) = fs::read_dir(current) else {
            return;
        };
        for entry in entries {
            let entry = entry.unwrap();
            if exclude_dir.is_some_and(|name| entry.file_name() == name) {
                continue;
            }
            let path = entry.path();
            if entry.file_type().unwrap().is_dir() {
                visit(base, &path, exclude_dir, out);
            } else {
                let rel = path.strip_prefix(base).unwrap().to_path_buf();
                out.insert(rel, fs::read(&path).unwrap());
            }
        }
    }
    let mut out = BTreeMap::new();
    visit(root, root, exclude_dir, &mut out);
    out
}

pub(crate) fn read_toml_table(path: &Path) -> toml::Table {
    let text = String::from_utf8(vosu_sys::platform::read_published(path).unwrap()).unwrap();
    toml::from_str(&text).unwrap()
}

pub(crate) fn read_json_value(path: &Path) -> Value {
    let bytes = vosu_sys::platform::read_published(path).unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

/// Sends a dashboard API request to a real `ToolRegistry` and decodes its
/// `(StatusCode, Value)` response.
pub(crate) async fn tool_dashboard_json(
    registry: &Arc<ToolRegistry>,
    method: Method,
    uri: &str,
    body: Bytes,
) -> (StatusCode, Value) {
    let request = Request::builder()
        .method(method)
        .uri(uri)
        .body(Body::empty())
        .unwrap();
    let response = bounded!(registry.dashboard(request, body));
    let status = response.status();
    let bytes = bounded!(to_bytes(response.into_body(), 1024 * 1024)).unwrap();
    let json = serde_json::from_slice(&bytes).unwrap();
    (status, json)
}

/// Cross-platform `PathResolver` backed by isolated temporary directories.
pub(crate) struct TestPathResolver {
    pub(crate) executable: PathBuf,
    pub(crate) launch: PathBuf,
    pub(crate) root: PathBuf,
}

impl PathResolver for TestPathResolver {
    fn current_exe(&self) -> io::Result<PathBuf> {
        Ok(self.executable.clone())
    }

    fn launch_dir(&self) -> io::Result<PathBuf> {
        Ok(self.launch.clone())
    }

    fn env_var(&self, name: &str) -> Option<OsString> {
        let sub = match name {
            "HOME" => "home",
            "XDG_CONFIG_HOME" => "xdg-config",
            "XDG_DATA_HOME" => "xdg-data",
            "XDG_CACHE_HOME" => "xdg-cache",
            "XDG_RUNTIME_DIR" => "xdg-runtime",
            "LOCALAPPDATA" => "local-appdata",
            _ => return None,
        };
        Some(self.root.join(sub).into_os_string())
    }
}
