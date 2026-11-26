//! The bundled composition a daemon executable injects (M7 C0): its version
//! and plugin-host identity, the explicit `BUNDLED` array with its required
//! identities, and [`BundledHosts`], the one object through which core's
//! runtime reaches the bundled hosts' in-process pieces. Core names no
//! bundled crate; `src/main.rs` passes `&vosu::BUNDLE` to [`super::run_cli`].
use std::{
    ffi::OsString,
    io,
    path::PathBuf,
    sync::Arc,
    time::Duration,
};

use bytes::Bytes;
use tokio_util::sync::CancellationToken;
use vosu_surface::{
    filter::Filter,
    listener::ListenerRole,
    projection::Projection,
    requester::BundledRequester,
    routes::{RouteDescriptor, RouteProvider},
    tool_view::ToolHostViews,
};

use crate::{
    app::StartupFlags,
    plugin::{BundledPlugin, PluginHost, PluginInstanceId, settings::PluginSettingsService},
    server::{
        routes::{ProviderFuture, StateJsonProvider},
        service::OverlayComposition,
        websocket::WebSocketHub,
    },
};

/// One executable's bundled set. `required` lists the production plugin ids
/// in `plugins` order (each required entry's instance id is its position);
/// optional fixtures follow them.
pub struct Bundle {
    /// The executable's package version (`CARGO_PKG_VERSION`): the updater,
    /// dashboard, catalog, package validation and log prefix all report it.
    pub version: &'static str,
    /// `vosu plugin-host`'s build identity (`docs/v2/plugin-abi.md` §7.6).
    pub plugin_host_identity: &'static str,
    pub plugins: &'static [BundledPlugin<'static>],
    pub required: &'static [&'static str],
    /// The bundled dashboard host's id (A7.3 switch).
    pub dashboard: &'static str,
    /// The bundled web tool host's id.
    pub tool_host: &'static str,
    /// The overlay host's core-seeded root setting (A7.1-OR).
    pub overlay_root: OverlayRootSetting,
    pub hosts: &'static dyn BundledHosts,
}

/// `plugin`'s one core-held setting `key`, seeded from `STATIC_FOLDER_PATH`.
#[derive(Clone, Copy, Debug)]
pub struct OverlayRootSetting {
    pub plugin: &'static str,
    pub key: &'static str,
}

impl Bundle {
    /// [`Bundle::version`] as the semver policy every package consumer gets.
    pub fn version(&self) -> semver::Version {
        semver::Version::parse(self.version).expect("bundle version is valid semver")
    }

    /// A required bundled plugin's instance id: its `plugins` position.
    pub fn instance(&self, id: &str) -> Option<PluginInstanceId> {
        self.required
            .iter()
            .position(|required| *required == id)
            .map(|index| PluginInstanceId::new(index as u64))
    }
}

/// The three bundled projection adapters' surface requesters.
#[derive(Clone)]
pub struct AdapterRequesters {
    pub upstream: Arc<dyn BundledRequester>,
    pub gosumemory: Arc<dyn BundledRequester>,
    pub stream_companion: Arc<dyn BundledRequester>,
}

/// One projection per legacy schema, plus the adapters' owned work.
pub struct Projections {
    pub v1: Arc<dyn Projection<Filter, Bytes>>,
    pub stream_companion: Arc<dyn Projection<Filter, Bytes>>,
    pub v2: Arc<dyn Projection<Filter, Bytes>>,
    pub v2_precise: Arc<dyn Projection<Filter, Bytes>>,
    pub services: Box<dyn AdapterServices>,
}

/// What the adapters own beyond their projections.
pub trait AdapterServices: Send + Sync {
    /// The client version the upstream adapter reports for `session`.
    fn client_version(&self, session: &str) -> Option<String>;
    /// Joins the adapters' derived work (the streamCompanion chart worker).
    fn shutdown(&self) -> ProviderFuture<'_, io::Result<()>>;
}

/// One bundled host's route registrations.
pub type HostRoutes = Vec<(RouteDescriptor, Arc<dyn RouteProvider>)>;

/// Where the overlay host's community catalog comes from.
pub enum OverlayCatalog {
    Production,
    /// A test-harness network: the hardened client and catalog endpoint.
    Test {
        client: vosu_net::streaming::StreamingClient,
        endpoint: String,
    },
}

/// What [`BundledHosts::overlay`] composes the overlay host from.
pub struct OverlayInputs {
    pub host: Arc<PluginHost>,
    pub instance: PluginInstanceId,
    /// The `data_dir` core gave the instance's `create`.
    pub data_dir: PathBuf,
    pub storage: Arc<super::RuntimeSettingsStorage>,
    pub settings: Arc<PluginSettingsService>,
    pub static_root: PathBuf,
    pub settings_root: PathBuf,
    pub cache_root: PathBuf,
    pub version: semver::Version,
    pub catalog: OverlayCatalog,
    pub shutdown: CancellationToken,
    pub origins: Arc<crate::server::origins::Origins>,
}

/// Everything core's runtime would otherwise name a bundled crate for.
/// Implementations hold no state: each call composes or reads fresh objects.
pub trait BundledHosts: Sync {
    /// vosu's own startup flags (`--update`, `--onedrive`) from `argv`.
    fn startup_flags(&self, args: &[OsString]) -> StartupFlags;
    /// The capture cadence of a configured poll rate in milliseconds.
    fn poll_duration(&self, milliseconds: f64) -> Duration;
    fn projections(&self, requesters: &AdapterRequesters) -> Projections;
    /// The adapters' overlay route table, grouped by registering owner:
    /// `[upstream, gosumemory, streamCompanion]`. `state` and `feeds` are
    /// ordered v1, streamCompanion, v2, v2 precise.
    fn adapter_routes(
        &self,
        state: [Arc<dyn StateJsonProvider>; 4],
        feeds: [WebSocketHub; 4],
        upstream: Arc<dyn BundledRequester>,
    ) -> [HostRoutes; 3];
    /// The dashboard host's page shells and assets for `version`.
    fn dashboard_routes(&self, version: &str) -> HostRoutes;
    /// Mounts the web tool host and its design-system set on `roles`; the
    /// returned owner unregisters everything when dropped.
    fn mount_tool_host(
        &self,
        requester: Arc<dyn BundledRequester>,
        views: ToolHostViews,
        roles: &[ListenerRole],
    ) -> Result<Box<dyn Send + Sync>, String>;
    fn overlay(&self, inputs: OverlayInputs) -> Arc<dyn OverlayComposition>;
}
