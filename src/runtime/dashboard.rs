//! Composition of the bundled dashboard host (M7 D0, A7.1): its page shells
//! and dashboard assets are registered on the dashboard listener through the
//! dashboard instance's own surface requester. Core keeps every management
//! route, the page context and the status/settings JSON.
use super::*;
use vosu_core::plugin::PluginInstanceId;

/// The dashboard's instance id: its position in `BUNDLED`, which
/// `dev_plugins::load_bundled` uses as the instance id.
fn instance() -> Option<PluginInstanceId> {
    crate::plugins::BUNDLED
        .iter()
        .position(|entry| entry.manifest == vosu_dashboard::MANIFEST)
        .map(|index| PluginInstanceId::new(index as u64))
}

/// Registers the dashboard host's exact routes for the life of the lease.
pub(super) fn mount(host: &Arc<PluginHost>) -> io::Result<vosu_surface::routes::RegistrationLease> {
    let instance = instance()
        .ok_or_else(|| io::Error::other("vosu.dashboard is missing from the bundled set"))?;
    let requester = host.surface_requester(instance).map_err(io::Error::other)?;
    let routes = vosu_dashboard::routes::Routes::new(current_version().to_string());
    requester
        .register_routes(routes.descriptors())
        .map_err(|error| io::Error::other(format!("dashboard registration failed: {error:?}")))
}
