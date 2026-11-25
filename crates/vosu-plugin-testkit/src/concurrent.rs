//! The `concurrent_dispatch` echo variant (`docs/v2/plugin-abi.md` §4; §10's
//! recommendation that the first concurrent-capable plugin ship a stress
//! test exercising overlapping `dispatch` calls, which
//! `crates/vosu-core/src/plugin/tests.rs` does against an equivalent
//! fixture). Bundled-form only: this phase keeps the `cdylib`-loaded
//! integration test to the serialized `echo` plugin.

use vosu_plugin_api::{
    ConcurrentPlugin, HostServices, Init, Message, PluginError, VtableHostServices, vosu_status,
};

pub struct ConcurrentEchoPlugin {
    host: VtableHostServices,
}

impl ConcurrentPlugin for ConcurrentEchoPlugin {
    fn create(host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        Ok(Self { host })
    }

    fn on_message(&self, message: Message, correlation_id: u64) -> Result<(), PluginError> {
        match self.host.send(&message, correlation_id) {
            vosu_status::VOSU_OK => Ok(()),
            status => Err(status.into()),
        }
    }
}

vosu_plugin_internal::export_plugin!(bundled concurrent ConcurrentEchoPlugin);
