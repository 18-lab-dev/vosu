//! ABI identity for the bundled projection; surface consumers are owned by the daemon.
use vosu_plugin_api::{Init, Message, Plugin, PluginError, VtableHostServices, vosu_status};
pub struct Adapter;
impl Plugin for Adapter {
    fn create(_: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        if init.plugin_id != "vosu.adapter.upstream" {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
        }
        Ok(Self)
    }
    fn on_message(&mut self, _: Message, _: u64) -> Result<(), PluginError> {
        // Projection state/demand uses the permanent bundled surface, not ABI game messages.
        Err(vosu_status::VOSU_ERR_UNSUPPORTED.into())
    }
}
vosu_plugin_api::export_plugin!(bundled Adapter);
