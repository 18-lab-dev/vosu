//! Wiring between the bundled overlay host and core's listener lifecycle.
use std::{collections::HashMap, io, path::PathBuf, sync::{Arc, Mutex, Weak}};

use serde_json::Value;
use vosu_core::{
    plugin::{PackageRegistration, PluginHost},
    report::ReportCounter,
    runtime::{OverlayCatalog, OverlayInputs},
    server::{
        routes::ProviderFuture,
        service::{CommandReply, CommandSession, OverlayComposition, RelayControl},
        websocket::WebSocketHub,
    },
};
use vosu_overlay_host::{
    ReloadableCounterService, SettingInput,
    catalog::CatalogService,
    commands::{CommandAction, CommandClient, CommandProcessor, SettingsProvider},
    presentation::OverlayPresenter,
};
use vosu_plugin_api::manifest::{HostSupport, PackageManifest, ValidationError};
use vosu_surface::routes::{ResponseFuture, SurfaceRequest, SurfaceResponse};

mod relay;
mod root;

use relay::{RelayRegistration, WebSocketRelaySink, parse_command_filters};
use root::OverlayRootComposition;

pub(super) struct Overlay {
    root: OverlayRootComposition,
    workers: Mutex<Option<Workers>>,
}

struct Workers {
    processor: Arc<CommandProcessor>,
    relays: Arc<WebSocketRelaySink>,
    presenter: Option<OverlayPresenter>,
}

impl Overlay {
    pub(super) fn new(inputs: OverlayInputs) -> Self {
        let counters = Arc::new(ReloadableCounterService::production(
            inputs.static_root,
            inputs.settings_root,
            inputs.cache_root,
        ));
        let version = inputs.version.to_string();
        counters.set_installed_packages(Arc::new(InstalledOverlayValidation {
            host: Arc::downgrade(&inputs.host),
            version: inputs.version,
        }));
        let catalog = match inputs.catalog {
            OverlayCatalog::Production => CatalogService::production(&version),
            OverlayCatalog::Test { client, endpoint } => CatalogService::new(client, endpoint, &version),
        };
        let root = OverlayRootComposition::new(
            inputs.host,
            inputs.instance,
            &inputs.data_dir,
            inputs.storage,
            inputs.settings,
            counters.clone(),
        );
        root.apply(&counters.current_root());
        let relays = Arc::new(WebSocketRelaySink::new());
        let processor = Arc::new(CommandProcessor::new(
            Arc::new(RuntimeCounterFacade(counters.clone())),
            relays.clone(),
        ));
        processor.bind_profiles(counters.settings_store());
        relays.bind_processor(&processor);
        Self {
            root,
            workers: Mutex::new(Some(Workers {
                processor,
                relays,
                presenter: Some(OverlayPresenter::new(counters, catalog, inputs.shutdown)),
            })),
        }
    }
}

impl OverlayComposition for Overlay {
    fn current_root(&self) -> PathBuf {
        self.root.counters.current_root()
    }

    fn reload_root(&self, root: PathBuf) {
        let processor = self.workers.lock().unwrap().as_ref().map(|parts| parts.processor.clone());
        if let Some(processor) = processor {
            self.root.counters.reload_static_root(root);
            self.root.apply(&self.current_root());
            processor.bind_profiles(self.root.counters.settings_store());
        }
    }

    fn mount(&self, commands: WebSocketHub) -> io::Result<()> {
        let (presenter, processor) = {
            let mut workers = self.workers.lock().unwrap();
            let workers = workers.as_mut().ok_or_else(|| io::Error::other("overlay shut down"))?;
            let presenter = workers.presenter.take().ok_or_else(|| io::Error::other("overlay already mounted"))?;
            (presenter, workers.processor.clone())
        };
        self.root.mount_routes(presenter, processor, commands)
    }

    fn serve_static(&self, request: SurfaceRequest) -> ResponseFuture<'_, SurfaceResponse> {
        Box::pin(async move { self.root.serving.serve(&request).await })
    }

    fn has_settings(&self, overlay: &str) -> bool {
        self.root.counters.settings_store().counter_values(overlay).is_ok()
    }

    fn installed_names(&self) -> HashMap<String, String> {
        let Ok(installed) = self.root.counters.installed() else { return HashMap::new() };
        let nicknames = self.root.counters.names().snapshot();
        installed.into_iter().map(|counter| {
            let name = nicknames.get(&counter.folder_name)
                .and_then(|name| name.nickname.clone())
                .unwrap_or(counter.name);
            (counter.folder_name, name)
        }).collect()
    }

    fn report_counters(&self) -> Result<Vec<ReportCounter>, String> {
        Ok(self.root.counters.discover().map_err(|error| error.to_string())?
            .into_iter().map(|counter| ReportCounter {
                author: counter.author,
                name: counter.name,
                version: counter.version,
                folder_name: counter.folder_name,
            }).collect())
    }

    fn values_path(&self, id: &str) -> PathBuf {
        self.root.counters.settings_store().values_path(id)
    }

    fn valid_folder(&self, name: &str) -> bool {
        vosu_overlay_host::validate_component(name).is_ok()
    }

    fn query_overlay(&self, query: &str) -> Option<String> {
        let client = CommandClient::from_query(0, Some(query)).ok()?;
        let (overlay, _) = vosu_overlay_host::split_counter(&client.overlay);
        Some(overlay.to_owned())
    }

    fn command_session(
        &self,
        namespace: u64,
        client_id: u64,
        overlay: Option<&str>,
        control: Arc<dyn RelayControl>,
    ) -> Option<Box<dyn CommandSession>> {
        let client = CommandClient::from_query(client_id, overlay).ok()?;
        let (processor, relays) = {
            let workers = self.workers.lock().unwrap();
            let workers = workers.as_ref()?;
            (workers.processor.clone(), workers.relays.clone())
        };
        let registration = relays.register(namespace, client.clone(), control);
        Some(Box::new(Session { client, processor, _registration: registration }))
    }

    fn shutdown(&self) -> ProviderFuture<'_, ()> {
        Box::pin(async {
            let workers = self.workers.lock().unwrap().take();
            if let Some(workers) = workers {
                workers.processor.shutdown().await;
                workers.relays.shutdown().await;
                self.root.retire();
                self.root.counters.shutdown().await;
            }
        })
    }
}

struct Session {
    client: CommandClient,
    processor: Arc<CommandProcessor>,
    _registration: RelayRegistration,
}

impl CommandSession for Session {
    fn process(&self, text: &str) -> CommandReply {
        command_reply(self.processor.process(&self.client, text))
    }
}

fn command_reply(action: CommandAction) -> CommandReply {
    match action {
        CommandAction::Reply(bytes) => CommandReply::Reply(bytes),
        CommandAction::ApplyFilters(source) => parse_command_filters(&source)
            .map_or(CommandReply::NoAck, CommandReply::ApplyFilters),
        CommandAction::NoAck => CommandReply::NoAck,
    }
}

struct RuntimeCounterFacade(Arc<ReloadableCounterService>);

impl SettingsProvider for RuntimeCounterFacade {
    fn get(&self, overlay: &str) -> Result<Value, String> {
        self.0.get_counter_values(overlay).map(Value::Object).map_err(|error| error.to_string())
    }

    fn save(&self, overlay: &str, body: Value) -> Result<(), String> {
        let payload: Vec<SettingInput> = serde_json::from_value(body).map_err(|error| error.to_string())?;
        self.0.save_user_values(overlay, &payload).map(|_| ()).map_err(|error| error.to_string())
    }
}

/// Per-manifest results from the installed package graph, without giving the
/// overlay host ownership of that graph or keeping the plugin host alive.
struct InstalledOverlayValidation {
    host: Weak<PluginHost>,
    version: semver::Version,
}

impl vosu_overlay_host::InstalledPackages for InstalledOverlayValidation {
    fn validate_all(&self, discovered: &[&PackageManifest]) -> Vec<Result<HostSupport, ValidationError>> {
        let mut snapshot = self.host.upgrade().map_or_else(Default::default, |host| host.validation_snapshot());
        snapshot.packages.extend(discovered.iter().map(|package| PackageRegistration::new(package, "overlays")));
        discovered.iter().map(|package| snapshot.validate(package, &self.version, "overlays", false)).collect()
    }
}
