//! ABI identity for the bundled dashboard. Its pages and assets are served
//! through `vosu-surface` route registration, composed by the daemon; the
//! plugin itself reads no state and receives no management authority.
use vosu_plugin_api::{Init, Message, Plugin, PluginError, VtableHostServices, vosu_status};

pub struct Dashboard;

impl Plugin for Dashboard {
    fn create(_: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        if init.plugin_id != crate::PLUGIN_ID {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
        }
        Ok(Self)
    }

    fn on_message(&mut self, _: Message, _: u64) -> Result<(), PluginError> {
        Err(vosu_status::VOSU_ERR_UNSUPPORTED.into())
    }
}

vosu_plugin_internal::export_plugin!(bundled Dashboard);
