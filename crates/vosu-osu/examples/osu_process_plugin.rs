//! The external osu! process-runtime fixture (M5 P0): the same
//! [`vosu_osu::plugin::OsuPlugin`] producer the bundled plugin registers
//! in-process, exported as a `cdylib` for a `runtime = "process"` package
//! (`tests/fixtures/plugin_host/osu/manifest.toml`).
//!
//! Development and test only (`required-features = ["test-harness"]`, which
//! release builds reject). The bundled manifest stays target-free and
//! in-process; only this separately staged library runs in a plugin host.
//!
//! When its data directory holds a parity control (M5 X0), the instance runs
//! M4a's source-oracle publication ingress instead of capturing a client
//! (`osu_process_plugin/parity.rs`); otherwise it is the producer itself.

#[path = "osu_process_plugin/parity.rs"]
mod parity;

use vosu_osu::plugin::OsuPlugin;
use vosu_plugin_api::{ConcurrentPlugin, Init, Message, PluginError, VtableHostServices};

enum ProcessOsu {
    Producer(Box<OsuPlugin>),
    Parity(Box<parity::Parity>),
}

impl ConcurrentPlugin for ProcessOsu {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        match parity::Parity::load(host, &init.data_dir) {
            Some(parity) => Ok(Self::Parity(Box::new(parity))),
            None => OsuPlugin::create(host, init).map(|plugin| Self::Producer(Box::new(plugin))),
        }
    }

    fn on_message(&self, message: Message, correlation_id: u64) -> Result<(), PluginError> {
        match self {
            Self::Producer(plugin) => plugin.on_message(message, correlation_id),
            Self::Parity(parity) => parity.on_message(message, correlation_id),
        }
    }
}

vosu_plugin_api::export_plugin!(concurrent ProcessOsu);
