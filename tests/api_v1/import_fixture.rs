//! Test-only forwarding wrapper: the production OsuPlugin still owns every
//! action, worker, session and shutdown path. Record emissions before the real
//! host sees them, so even a rejected duplicate terminal would be counted.
use std::{
    collections::HashMap,
    path::PathBuf,
    sync::{Arc, LazyLock, Mutex},
};
use vosu_plugin_api::{
    ActionResult, ConcurrentPlugin, HostServices, Init, Message, PluginError, VtableHostServices,
    vosu_opcode, vosu_status,
};

pub type Results = Arc<Mutex<Vec<ActionResult>>>;
static RESULTS: LazyLock<Mutex<HashMap<PathBuf, Results>>> = LazyLock::new(Mutex::default);
pub fn register(root: PathBuf) -> Results {
    let results = Arc::default();
    assert!(
        RESULTS
            .lock()
            .unwrap()
            .insert(root, Arc::clone(&results))
            .is_none()
    );
    results
}
#[derive(Clone)]
struct RecordedHost {
    host: VtableHostServices,
    results: Results,
}
impl HostServices for RecordedHost {
    fn send(&self, message: &Message, correlation: u64) -> vosu_status {
        if let Message::ActionResult(result) = message {
            let mut results = self.results.lock().unwrap();
            assert!(results.len() < 8, "unexpected terminal emission flood");
            results.push(result.clone());
        }
        self.host.send(message, correlation)
    }
    fn send_flat(
        &self,
        opcode: vosu_opcode,
        flags: u32,
        correlation: u64,
        payload: &[u8],
    ) -> vosu_status {
        self.host.send_flat(opcode, flags, correlation, payload)
    }
}
struct RecordedOsu(vosu_osu::plugin::OsuPlugin<RecordedHost>);
impl ConcurrentPlugin for RecordedOsu {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let results = RESULTS
            .lock()
            .unwrap()
            .remove(&init.data_dir)
            .expect("per-instance recording registration");
        #[cfg(target_os = "linux")]
        let platform = vosu_osu::classify::Platform::Linux;
        #[cfg(windows)]
        let platform = vosu_osu::classify::Platform::Windows;
        let plugin = vosu_osu::plugin::OsuPlugin::new(
            RecordedHost { host, results },
            platform,
            init.data_dir,
        );
        // Same settings application as the production ConcurrentPlugin create.
        plugin.dispatch(
            Message::SettingsChanged(vosu_plugin_api::SettingsChanged {
                values: init.settings,
            }),
            0,
        )?;
        Ok(Self(plugin))
    }
    fn on_message(&self, message: Message, correlation: u64) -> Result<(), PluginError> {
        self.0.dispatch(message, correlation)
    }
}
vosu_plugin_api::export_plugin!(bundled concurrent RecordedOsu);
