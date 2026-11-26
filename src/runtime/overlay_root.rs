//! The overlay host's core side. A7.1-OR: core seeds `vosu.overlayhost`'s
//! `static_folder_path` setting from the resolved `STATIC_FOLDER_PATH` until
//! M8 migrates the key, delivers every effective change as one
//! `SETTINGS_CHANGED`, and mints the read-only `OverlayRootView` lease the
//! host serves through, retiring the previous generation's lease. A7 route
//! inventory: the host's counter API and command feed registrations.
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwapOption;
use serde_json::{Map, Value};
use vosu_core::plugin::settings::{
    ConfigSettingsBridgeB2, PluginSettingsService, SettingsChangeDelivery, SettingsError,
    SettingsSchemaSource, SettingsStorage,
};
use vosu_core::plugin::{OverlayRootRetirement, PluginHost, PluginInstanceId};
use vosu_overlay_host::{
    OverlayRootView, OverlayRoutes, OverlayStatic, ReloadableCounterService,
    commands::CommandProcessor, presentation::OverlayPresenter,
};
use vosu_plugin_api::{Message, SettingsChanged};
use vosu_surface::routes::RegistrationLease;

use crate::server::websocket::WebSocketHub;

pub(crate) const OVERLAY_HOST_ID: &str = "vosu.overlayhost";

/// Core's settings storage until M8: bridge B2 for `vosu.osu`, and the
/// core-held `static_folder_path` seed for `vosu.overlayhost` (not B2,
/// which never gains a second plugin id). The seed has no other writer:
/// the Settings page still edits `STATIC_FOLDER_PATH`.
pub struct RuntimeSettingsStorage {
    bridge: Arc<ConfigSettingsBridgeB2>,
    overlay_root: ArcSwapOption<PathBuf>,
}

impl RuntimeSettingsStorage {
    pub fn new(bridge: Arc<ConfigSettingsBridgeB2>) -> Self {
        Self {
            bridge,
            overlay_root: ArcSwapOption::empty(),
        }
    }

    pub fn seed_overlay_root(&self, root: &Path) {
        self.overlay_root.store(Some(Arc::new(root.to_owned())));
    }
}

impl SettingsStorage for RuntimeSettingsStorage {
    fn load(&self, plugin_id: &str) -> Result<Map<String, Value>, SettingsError> {
        if plugin_id != OVERLAY_HOST_ID {
            return self.bridge.load(plugin_id);
        }
        let root = self.overlay_root.load();
        let value = root
            .as_deref()
            .map_or_else(String::new, |root| root.to_string_lossy().into_owned());
        Ok(Map::from_iter([(
            vosu_overlay_host::root::STATIC_FOLDER_PATH.to_owned(),
            Value::String(value),
        )]))
    }

    fn store(&self, plugin_id: &str, values: &Map<String, Value>) -> Result<(), SettingsError> {
        if plugin_id == OVERLAY_HOST_ID {
            return Err(SettingsError::NotBridged(plugin_id.to_owned()));
        }
        self.bridge.store(plugin_id, values)
    }
}

/// Owns the overlay root's generations and the overlay host's route
/// registrations for one composition.
pub struct OverlayRootComposition {
    host: Arc<PluginHost>,
    instance: PluginInstanceId,
    storage: Arc<RuntimeSettingsStorage>,
    settings: Arc<PluginSettingsService>,
    counters: Arc<ReloadableCounterService>,
    serving: Arc<OverlayStatic>,
    generation: Mutex<(u64, Option<OverlayRootRetirement>)>,
    routes: Mutex<Option<RegistrationLease>>,
}

impl OverlayRootComposition {
    pub fn new(
        host: Arc<PluginHost>,
        instance: PluginInstanceId,
        storage: Arc<RuntimeSettingsStorage>,
        settings: Arc<PluginSettingsService>,
        counters: Arc<ReloadableCounterService>,
    ) -> Self {
        let serving = Arc::new(OverlayStatic::new(Arc::clone(&counters)));
        Self {
            host,
            instance,
            storage,
            settings,
            counters,
            serving,
            generation: Mutex::new((0, None)),
            routes: Mutex::new(None),
        }
    }

    pub fn serving(&self) -> Arc<OverlayStatic> {
        Arc::clone(&self.serving)
    }

    /// Registers `vosu.overlayhost`'s counter API and `/websocket/commands`
    /// feed (A7 route inventory) for its bundled requester. The commands
    /// hub frames each connection; `processor` handles its frames.
    pub fn mount_routes(
        &self,
        presenter: OverlayPresenter,
        processor: Arc<CommandProcessor>,
        commands: WebSocketHub,
    ) -> std::io::Result<()> {
        let routes = OverlayRoutes::new(
            Arc::clone(&self.counters),
            presenter,
            processor,
            Arc::new(move || commands.surface_feed(true)),
        );
        let lease = self
            .host
            .surface_requester(self.instance)
            .map_err(std::io::Error::other)
            .and_then(|requester| {
                routes.register(requester.as_ref()).map_err(|error| {
                    std::io::Error::other(format!("overlay host registration failed: {error:?}"))
                })
            })?;
        *self.routes.lock().unwrap_or_else(|p| p.into_inner()) = Some(lease);
        Ok(())
    }

    /// Applies one resolved overlay root: seeds and delivers the complete
    /// effective settings object, points the host's storage at it, then
    /// mints the next read-only lease and retires the previous one. A root
    /// that cannot be opened serves nothing until the next change.
    pub fn apply(&self, root: &Path) {
        self.storage.seed_overlay_root(root);
        let schema = self.host.schema_for(OVERLAY_HOST_ID);
        if let Ok(values) = self
            .settings
            .effective_settings(OVERLAY_HOST_ID, schema.as_ref())
        {
            if let Err(error) = self.counters.apply_settings(&values) {
                tracing::warn!(%error, "overlay root setting was not applied");
            }
            self.host.settings_changed(
                OVERLAY_HOST_ID,
                Message::SettingsChanged(SettingsChanged { values }),
            );
        }
        let mut generation = self.generation.lock().unwrap_or_else(|p| p.into_inner());
        generation.0 += 1;
        match self.host.overlay_root_lease(self.instance, root) {
            Ok((lease, retirement)) => {
                self.serving.set_view(OverlayRootView {
                    generation: generation.0,
                    root: lease,
                });
                generation.1 = Some(retirement);
            }
            Err(error) => {
                tracing::warn!(%error, "overlay root lease was not minted; serving nothing");
                self.serving.clear_view();
                generation.1 = None;
            }
        }
    }

    /// Retires the current lease and unregisters the routes (shutdown).
    pub fn retire(&self) {
        self.serving.clear_view();
        self.generation.lock().unwrap_or_else(|p| p.into_inner()).1 = None;
        // Unregistration re-enters the plugin host; never under this lock.
        let routes = self.routes.lock().unwrap_or_else(|p| p.into_inner()).take();
        drop(routes);
    }
}
