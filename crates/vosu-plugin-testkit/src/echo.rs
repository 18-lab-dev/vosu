//! The serialized echo plugin: echoes every `dispatch`ed message straight
//! back to the host unchanged, carrying the same `correlation_id`
//! (`docs/v2/plugin-abi.md` §9's `Plugin`). Exported both ways
//! `export_plugin!` supports: the bundled form, a plain
//! `vosu_plugin_vtable()`, for the root crate's `BUNDLED` array (D13); and
//! the `cdylib` form, `vosu_plugin_entry_v1`, for `tests/plugin_host.rs`'s
//! `dlopen` of this crate's built shared library.

use vosu_plugin_api::{
    HostServices, Init, Message, Plugin, PluginError, VtableHostServices, vosu_status,
};

pub struct EchoPlugin {
    host: VtableHostServices,
}

impl Plugin for EchoPlugin {
    fn create(host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        Ok(Self { host })
    }

    fn on_message(&mut self, message: Message, correlation_id: u64) -> Result<(), PluginError> {
        match self.host.send(&message, correlation_id) {
            vosu_status::VOSU_OK => Ok(()),
            status => Err(status.into()),
        }
    }
}

vosu_plugin_api::export_plugin!(bundled EchoPlugin);
vosu_plugin_api::export_plugin!(EchoPlugin);
