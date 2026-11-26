use super::*;

/// Process-wide resource owner used by [`App`].
///
#[derive(Clone)]
pub(super) struct RuntimeBeatmap {
    pub(super) counters: Arc<ReloadableCounterService>,
}

impl CurrentBeatmapProvider for RuntimeBeatmap {
    fn files(&self) -> BeatmapFiles {
        BeatmapFiles {
            static_root: Some(self.counters.current_root()),
            ..BeatmapFiles::default()
        }
    }
}

#[derive(Clone)]
pub(super) struct RuntimeCounterFacade {
    pub(super) counters: Arc<ReloadableCounterService>,
}

impl SettingsProvider for RuntimeCounterFacade {
    fn get(&self, overlay: &str) -> Result<serde_json::Value, String> {
        self.counters
            .get_counter_values(overlay)
            .map(serde_json::Value::Object)
            .map_err(|error| error.to_string())
    }

    fn save(&self, overlay: &str, body: serde_json::Value) -> Result<(), String> {
        let payload: Vec<vosu_overlay_host::SettingInput> =
            serde_json::from_value(body).map_err(|error| error.to_string())?;
        self.counters
            .save_user_values(overlay, &payload)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

/// The M6b installed-graph input to overlay manifest validation. Core keeps
/// the plugin host; the overlay host receives only per-manifest results.
pub(super) struct InstalledOverlayValidation(pub(super) std::sync::Weak<PluginHost>);

impl vosu_overlay_host::InstalledPackages for InstalledOverlayValidation {
    fn validate_all(
        &self,
        discovered: &[&vosu_plugin_api::manifest::PackageManifest],
    ) -> Vec<
        Result<vosu_plugin_api::manifest::HostSupport, vosu_plugin_api::manifest::ValidationError>,
    > {
        let mut snapshot = self
            .0
            .upgrade()
            .map_or_else(Default::default, |host| host.validation_snapshot());
        snapshot.packages.extend(
            discovered
                .iter()
                .map(|package| vosu_core::plugin::PackageRegistration::new(package, "overlays")),
        );
        discovered
            .iter()
            .map(|package| {
                snapshot.validate(package, crate::application_version(), "overlays", false)
            })
            .collect()
    }
}

pub(super) fn json_number(value: &serde_json::Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.trim().parse().ok())
}

pub(super) fn format_number(value: f64) -> String {
    if value.fract() == 0.0 {
        format!("{value:.0}")
    } else {
        value.to_string()
    }
}
