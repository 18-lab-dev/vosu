//! ABI identity for the bundled tool host; its routes are mounted by composition.
use vosu_plugin_api::{Init, Message, Plugin, PluginError, VtableHostServices, vosu_status};
pub struct ToolHostPlugin;
impl Plugin for ToolHostPlugin {
    fn create(_: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        if init.plugin_id != "vosu.toolhost" {
            return Err(vosu_status::VOSU_ERR_INVALID_ARGUMENT.into());
        }
        Ok(Self)
    }
    fn on_message(&mut self, _: Message, _: u64) -> Result<(), PluginError> {
        // Tool hosting uses only its exact surface registrations and the
        // core-pushed `ToolHostView`; it receives no ABI messages.
        Err(vosu_status::VOSU_ERR_UNSUPPORTED.into())
    }
}
vosu_plugin_internal::export_plugin!(bundled ToolHostPlugin);
