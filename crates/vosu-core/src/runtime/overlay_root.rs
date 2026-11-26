//! The overlay root seed. A7.1-OR: core seeds the overlay host's one
//! core-held setting (`static_folder_path`) from the resolved
//! `STATIC_FOLDER_PATH` until M8 migrates the key. The overlay host's
//! composition (`BundledHosts::overlay`) delivers every effective change and
//! mints the read-only root lease it serves through.
use std::path::{Path, PathBuf};
use std::sync::Arc;

use arc_swap::ArcSwapOption;
use serde_json::{Map, Value};

use super::OverlayRootSetting;
use crate::plugin::settings::{
    BRIDGE_B2_PLUGIN_ID, ConfigSettingsBridgeB2, PluginSettingsStore, SettingsError,
    SettingsStorage,
};

/// Core's settings storage: bridge B2 for `vosu.osu` and the core-held root
/// seed for the overlay host until M8 (not B2, which never gains a second
/// plugin id); every other plugin id is core's own per-plugin store (A7.1).
/// The seed has no other writer: the Settings page still edits
/// `STATIC_FOLDER_PATH`, and so the vosu.toml-backed two are not editable
/// through the plugin settings route.
pub struct RuntimeSettingsStorage {
    bridge: Arc<ConfigSettingsBridgeB2>,
    plugins: Arc<PluginSettingsStore>,
    setting: OverlayRootSetting,
    overlay_root: ArcSwapOption<PathBuf>,
}

impl RuntimeSettingsStorage {
    pub fn new(
        bridge: Arc<ConfigSettingsBridgeB2>,
        plugins: Arc<PluginSettingsStore>,
        setting: OverlayRootSetting,
    ) -> Self {
        Self {
            bridge,
            plugins,
            setting,
            overlay_root: ArcSwapOption::empty(),
        }
    }

    /// The overlay host's id and its seeded setting key.
    pub fn overlay_setting(&self) -> OverlayRootSetting {
        self.setting
    }

    pub fn seed_overlay_root(&self, root: &Path) {
        self.overlay_root.store(Some(Arc::new(root.to_owned())));
    }
}

impl SettingsStorage for RuntimeSettingsStorage {
    fn load(&self, plugin_id: &str) -> Result<Map<String, Value>, SettingsError> {
        if plugin_id == BRIDGE_B2_PLUGIN_ID {
            return self.bridge.load(plugin_id);
        }
        if plugin_id == self.setting.plugin {
            let root = self.overlay_root.load();
            let value = root
                .as_deref()
                .map_or_else(String::new, |root| root.to_string_lossy().into_owned());
            return Ok(Map::from_iter([(
                self.setting.key.to_owned(),
                Value::String(value),
            )]));
        }
        self.plugins.load(plugin_id)
    }

    fn store(&self, plugin_id: &str, values: &Map<String, Value>) -> Result<(), SettingsError> {
        if plugin_id == BRIDGE_B2_PLUGIN_ID {
            return self.bridge.store(plugin_id, values);
        }
        if plugin_id == self.setting.plugin {
            return Err(SettingsError::NotBridged(plugin_id.to_owned()));
        }
        self.plugins.store(plugin_id, values)
    }

    fn editable(&self, plugin_id: &str) -> bool {
        plugin_id != BRIDGE_B2_PLUGIN_ID && plugin_id != self.setting.plugin
    }
}
