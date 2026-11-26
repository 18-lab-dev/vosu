//! The overlay host's core side. A7.1-OR: core seeds `vosu.overlayhost`'s
//! `static_folder_path` setting from the resolved `STATIC_FOLDER_PATH` until
//! M8 migrates the key, delivers every effective change as one
//! `SETTINGS_CHANGED`, and mints the read-only `OverlayRootView` lease the
//! host serves through, retiring the previous generation's lease. A7 route
//! inventory: the host's counter API and command feed registrations.
use std::path::Path;
use std::sync::{Arc, Mutex};

use vosu_core::{
    plugin::{
        OverlayRootRetirement, PluginHost, PluginInstanceId,
        settings::{PluginSettingsService, SettingsChangeDelivery, SettingsSchemaSource},
    },
    runtime::RuntimeSettingsStorage,
    server::{origins::Origins, websocket::WebSocketHub},
};
use vosu_overlay_host::{
    OverlayOrigin, OverlayQueryBinding, OverlayRootView, OverlayRoutes, OverlayStatic,
    ReloadableCounterService, commands::CommandProcessor, presentation::OverlayPresenter,
};
use vosu_plugin_api::{Message, SettingsChanged};
use vosu_surface::routes::RegistrationLease;

/// Owns the overlay root's generations, the overlay host's route
/// registrations and its `overlays.query` binding for one composition.
pub struct OverlayRootComposition {
    host: Arc<PluginHost>,
    instance: PluginInstanceId,
    storage: Arc<RuntimeSettingsStorage>,
    settings: Arc<PluginSettingsService>,
    pub(super) counters: Arc<ReloadableCounterService>,
    pub(super) serving: OverlayStatic,
    generation: Mutex<Option<(u64, Option<OverlayRootRetirement>)>>,
    routes: Mutex<Option<RegistrationLease>>,
    query: Mutex<Option<OverlayQueryBinding>>,
}

impl OverlayRootComposition {
    /// `data_dir` is the one core gave the `instance`'s `create`: it keys
    /// the instance's `overlays.query` binding to `counters` and to the
    /// overlay listener's effective origin in `origins`, read per query.
    pub fn new(
        host: Arc<PluginHost>,
        instance: PluginInstanceId,
        data_dir: &Path,
        storage: Arc<RuntimeSettingsStorage>,
        settings: Arc<PluginSettingsService>,
        counters: Arc<ReloadableCounterService>,
        origins: Arc<Origins>,
    ) -> Self {
        let serving = OverlayStatic::new(Arc::clone(&counters));
        let origin: OverlayOrigin = Arc::new(move || origins.overlay_listener());
        Self {
            host,
            instance,
            storage,
            settings,
            query: Mutex::new(Some(vosu_overlay_host::bind_overlay_query(data_dir, &counters, origin))),
            counters,
            serving,
            generation: Mutex::new(Some((0, None))),
            routes: Mutex::new(None),
        }
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
        let previous = self.routes.lock().unwrap_or_else(|p| p.into_inner()).replace(lease);
        drop(previous);
        Ok(())
    }

    /// Applies one resolved overlay root: seeds and delivers the complete
    /// effective settings object, points the host's storage at it, then
    /// mints the next read-only lease and retires the previous one. A root
    /// that cannot be opened serves nothing until the next change.
    pub fn apply(&self, root: &Path) {
        self.storage.seed_overlay_root(root);
        let id = self.storage.overlay_setting().plugin;
        let schema = self.host.schema_for(id);
        if let Ok(values) = self.settings.effective_settings(id, schema.as_ref())
        {
            if let Err(error) = self.counters.apply_settings(&values) {
                tracing::warn!(%error, "overlay root setting was not applied");
            }
            self.host.settings_changed(
                id,
                Message::SettingsChanged(SettingsChanged { values }),
            );
        }
        let lease = self.host.overlay_root_lease(self.instance, root);
        let mut generation = self.generation.lock().unwrap_or_else(|p| p.into_inner());
        let Some((sequence, current)) = generation.as_mut() else {
            return;
        };
        *sequence += 1;
        match lease {
            Ok((lease, retirement)) => {
                self.serving.set_view(OverlayRootView {
                    generation: *sequence,
                    root: lease,
                });
                *current = Some(retirement);
            }
            Err(error) => {
                tracing::warn!(%error, "overlay root lease was not minted; serving nothing");
                self.serving.clear_view();
                *current = None;
            }
        }
    }

    /// Retires the current lease and unregisters the routes (shutdown).
    pub fn retire(&self) {
        {
            let mut generation = self.generation.lock().unwrap_or_else(|p| p.into_inner());
            self.serving.clear_view();
            generation.take();
        }
        // Unregistration re-enters the plugin host; never under this lock.
        let routes = self.routes.lock().unwrap_or_else(|p| p.into_inner()).take();
        drop(routes);
        let query = self.query.lock().unwrap_or_else(|p| p.into_inner()).take();
        drop(query);
    }
}
