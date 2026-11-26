//! Composition of the bundled dashboard host (M7 D0, A7.1): its page shells
//! and dashboard assets are registered on the dashboard listener through the
//! dashboard instance's own surface requester. Core keeps every management
//! route, the page context and the status/settings JSON.
//!
//! Dashboard off (A7.3): `ENABLE_DASHBOARD = false` or `VOSU_DASHBOARD=off`,
//! resolved once in `run_cli` before the second-launch probe or any bind.
//! Off binds no dashboard listener, opens no browser, gives the tray no
//! **Open dashboard**, and leaves `vosu.dashboard` validated but uncreated,
//! listed `inactive` with reason [`DISABLED_REASON`]. Nothing served by the
//! dashboard can change the switch.
use super::*;
use vosu_core::plugin::PluginInstanceId;

/// The bundled dashboard host's plugin id.
pub(super) const ID: &str = vosu_dashboard::PLUGIN_ID;
/// The environment override: unset leaves `ENABLE_DASHBOARD` in charge.
pub(super) const ENV: &str = "VOSU_DASHBOARD";
/// Why `vosu.dashboard` is `inactive` when the dashboard is off.
pub(super) const DISABLED_REASON: &str = "dashboard_disabled";

/// Whether `VOSU_DASHBOARD` forces the dashboard off. Exactly `off` does;
/// any other value, including empty, `on` or `OFF`, is a startup error that
/// names the variable (the value itself is not echoed).
pub(super) fn forced_off(value: Option<&std::ffi::OsStr>) -> io::Result<bool> {
    match value {
        None => Ok(false),
        Some(value) if value == "off" => Ok(true),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "{ENV} must be unset or exactly `off`; set ENABLE_DASHBOARD in vosu.toml to turn the dashboard on or off"
            ),
        )),
    }
}

/// The dashboard's instance id: its position in `BUNDLED`, which
/// `dev_plugins::load_bundled` uses as the instance id.
pub(super) fn instance() -> Option<PluginInstanceId> {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_an_exact_off_forces_the_dashboard_off() {
        assert!(!forced_off(None).unwrap());
        assert!(forced_off(Some("off".as_ref())).unwrap());
        for invalid in ["", "on", "OFF", "Off", " off", "off ", "false", "0"] {
            let error = forced_off(Some(invalid.as_ref())).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{invalid:?}");
            assert!(error.to_string().starts_with("VOSU_DASHBOARD must be"));
        }
    }

    #[test]
    fn the_dashboard_instance_is_the_bundled_dashboard_manifest() {
        let index = crate::plugins::BUNDLED
            .iter()
            .position(|entry| entry.manifest.contains(&format!("id = \"{ID}\"")))
            .expect("vosu.dashboard is bundled");
        assert_eq!(instance(), Some(PluginInstanceId::new(index as u64)));
    }
}
