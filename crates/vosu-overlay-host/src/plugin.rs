//! ABI identity for the bundled overlay host. Routes, feeds and the overlay
//! root view are composed through `vosu-surface`; the ABI carries only the
//! `static_folder_path` setting (A7.1-OR), validated on every delivery.
use vosu_plugin_api::{Init, Message, Plugin, PluginError, VtableHostServices, vosu_status};

pub struct OverlayHost;

impl Plugin for OverlayHost {
    fn create(_: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        if init.plugin_id != "vosu.overlayhost" {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
        }
        crate::root::from_cbor(&init.settings)
            .map_err(|_| vosu_status::VOSU_ERR_INVALID_ARGUMENT)?;
        Ok(Self)
    }

    fn on_message(&mut self, message: Message, _: u64) -> Result<(), PluginError> {
        match message {
            Message::SettingsChanged(changed) => crate::root::from_cbor(&changed.values)
                .map(drop)
                .map_err(|_| vosu_status::VOSU_ERR_INVALID_ARGUMENT.into()),
            // Overlay hosting reads no game state and declares no action.
            _ => Err(vosu_status::VOSU_ERR_UNSUPPORTED.into()),
        }
    }
}

vosu_plugin_internal::export_plugin!(bundled OverlayHost);
