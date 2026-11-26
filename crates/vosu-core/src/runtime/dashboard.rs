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

/// The environment override: unset leaves `ENABLE_DASHBOARD` in charge.
pub(super) const ENV: &str = "VOSU_DASHBOARD";
/// Why the bundled dashboard host is `inactive` when the dashboard is off.
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

/// Registers the dashboard host's exact routes for the life of the lease,
/// through the dashboard instance's own requester.
pub(super) fn mount(
    bundle: &Bundle,
    host: &Arc<PluginHost>,
) -> io::Result<vosu_surface::routes::RegistrationLease> {
    let instance = bundle.instance(bundle.dashboard).ok_or_else(|| {
        io::Error::other(format!("{} is missing from the bundled set", bundle.dashboard))
    })?;
    let requester = host.surface_requester(instance).map_err(io::Error::other)?;
    requester
        .register_routes(bundle.hosts.dashboard_routes(bundle.version))
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
}
