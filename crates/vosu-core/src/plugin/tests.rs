//! Host unit tests (M1 Exit list): trust refusal of `in-process` for an
//! ineligible plugin, dispatch serialization versus `concurrent_dispatch`,
//! and the `destroy` grace timeout.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use vosu_plugin_api::{
    ConcurrentPlugin, FocusChanged, Init, Message, Plugin, PluginError, vosu_status,
};

use super::*;

struct NullHandler;
impl PluginHostHandler for NullHandler {
    fn handle(
        &self,
        _instance: PluginInstanceId,
        _message: Message,
        _correlation_id: u64,
    ) -> vosu_status {
        vosu_status::VOSU_OK
    }
}

fn init_bytes(plugin_id: &str) -> Vec<u8> {
    vosu_plugin_api::encode(&Init {
        plugin_id: plugin_id.into(),
        settings: ciborium::Value::Map(Vec::new()),
        data_dir: std::env::temp_dir(),
    })
    .unwrap()
}

fn focus_changed_message(handle: u64) -> OwnedMessage {
    OwnedMessage::from_message(
        &Message::FocusChanged(FocusChanged {
            handle,
            focused: true,
        }),
        0,
    )
    .unwrap()
}

type Window = (Instant, Instant);
type Log = Arc<Mutex<Vec<Window>>>;

fn registry() -> &'static Mutex<HashMap<String, Log>> {
    static REGISTRY: std::sync::LazyLock<Mutex<HashMap<String, Log>>> =
        std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));
    &REGISTRY
}

fn register_log(plugin_id: &str) -> Log {
    let log: Log = Arc::new(Mutex::new(Vec::new()));
    registry()
        .lock()
        .unwrap()
        .insert(plugin_id.to_string(), Arc::clone(&log));
    log
}

fn log_for(plugin_id: &str) -> Log {
    registry()
        .lock()
        .unwrap()
        .get(plugin_id)
        .cloned()
        .unwrap_or_else(|| panic!("no log registered for {plugin_id:?}"))
}

fn manifest(id: &str, extra: &str) -> String {
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='Test'\nversion='1.0.0'\nhost='>=2.0.0, <3.0.0'\nabi_version=1\nruntime='in-process'\n{extra}"
    )
}

fn expect_in_process_refused(result: Result<Vec<LoadedPlugin>, HostError>) {
    match result {
        Err(HostError::InProcessRefused { .. }) => {}
        Err(other) => panic!("expected InProcessRefused, got {other}"),
        Ok(_) => panic!("expected in-process to be refused"),
    }
}

// --- Trust refusal (§7.1) ------------------------------------------------

#[test]
fn development_plugin_is_refused_outside_debug_builds() {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(
        dir.path().join("manifest.toml"),
        manifest(
            "acme.test",
            "\n[targets.x86_64-unknown-linux-gnu]\nlibrary='lib/libacme.so'\n",
        ),
    )
    .unwrap();

    match registry::load_development(dir.path(), false) {
        Err(HostError::InProcessRefused { .. }) => {}
        Err(other) => panic!("expected InProcessRefused, got {other}"),
        Ok(_) => panic!("expected in-process to be refused outside debug builds"),
    }
}

mod dummy_plugin {
    use super::{Init, Message, Plugin, PluginError};

    pub struct Dummy;
    impl Plugin for Dummy {
        fn create(
            _host: vosu_plugin_api::VtableHostServices,
            _init: Init,
        ) -> Result<Self, PluginError> {
            Ok(Self)
        }
        fn on_message(
            &mut self,
            _message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            Ok(())
        }
    }
    vosu_plugin_api::export_plugin!(bundled Dummy);
}

#[test]
fn bundled_manifest_requesting_process_runtime_is_rejected() {
    let text = "kind='plugin'\nmanifest_version=1\nid='acme.test'\nname='Test'\nversion='1.0.0'\nhost='>=2.0.0, <3.0.0'\nabi_version=1\nruntime='process'\n";
    expect_in_process_refused(registry::load_bundled(&[registry::BundledPlugin {
        manifest: text,
        vtable: dummy_plugin::vosu_plugin_vtable,
    }]));
}

// --- Dispatch serialization vs. concurrent dispatch (§4) -----------------

mod serial_recorder {
    use super::{Init, Log, Message, Plugin, PluginError, log_for};

    pub struct SerialRecorder {
        log: Log,
    }
    impl Plugin for SerialRecorder {
        fn create(
            _host: vosu_plugin_api::VtableHostServices,
            init: Init,
        ) -> Result<Self, PluginError> {
            Ok(Self {
                log: log_for(&init.plugin_id),
            })
        }
        fn on_message(
            &mut self,
            _message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            let start = std::time::Instant::now();
            std::thread::sleep(std::time::Duration::from_millis(20));
            self.log
                .lock()
                .unwrap()
                .push((start, std::time::Instant::now()));
            Ok(())
        }
    }
    vosu_plugin_api::export_plugin!(bundled SerialRecorder);
}

#[test]
fn serial_dispatch_runs_messages_strictly_in_order() {
    let log = register_log("acme.serial");
    let text = manifest("acme.serial", "");
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &text,
        vtable: serial_recorder::vosu_plugin_vtable,
    }])
    .unwrap();
    let instance = PluginInstance::create(
        PluginInstanceId::new(1),
        &loaded[0],
        &init_bytes("acme.serial"),
        Arc::new(NullHandler),
        None,
    )
    .unwrap();

    let mut replies = Vec::new();
    for i in 0..4 {
        replies.push(instance.dispatch(focus_changed_message(i)).unwrap());
    }
    for reply in replies {
        reply.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    instance.destroy(Duration::from_secs(5)).unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 4);
    for window in entries.windows(2) {
        assert!(
            window[0].1 <= window[1].0,
            "message {:?} overlapped with the next message {:?}: serial dispatch must not run concurrently",
            window[0],
            window[1]
        );
    }
}

mod concurrent_recorder {
    use super::{ConcurrentPlugin, Init, Log, Message, PluginError, log_for};

    pub struct ConcurrentRecorder {
        log: Log,
    }
    impl ConcurrentPlugin for ConcurrentRecorder {
        fn create(
            _host: vosu_plugin_api::VtableHostServices,
            init: Init,
        ) -> Result<Self, PluginError> {
            Ok(Self {
                log: log_for(&init.plugin_id),
            })
        }
        fn on_message(&self, _message: Message, _correlation_id: u64) -> Result<(), PluginError> {
            let start = std::time::Instant::now();
            std::thread::sleep(std::time::Duration::from_millis(50));
            self.log
                .lock()
                .unwrap()
                .push((start, std::time::Instant::now()));
            Ok(())
        }
    }
    vosu_plugin_api::export_plugin!(bundled concurrent ConcurrentRecorder);
}

#[test]
fn concurrent_dispatch_runs_messages_in_overlapping_windows() {
    let log = register_log("acme.concurrent");
    let text = manifest("acme.concurrent", "concurrent_dispatch=true\n");
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &text,
        vtable: concurrent_recorder::vosu_plugin_vtable,
    }])
    .unwrap();
    let pool = WorkerPool::new(4, 64);
    let instance = PluginInstance::create(
        PluginInstanceId::new(2),
        &loaded[0],
        &init_bytes("acme.concurrent"),
        Arc::new(NullHandler),
        Some(Arc::clone(&pool)),
    )
    .unwrap();

    let started = Instant::now();
    let mut replies = Vec::new();
    for _ in 0..3 {
        replies.push(instance.dispatch(focus_changed_message(0)).unwrap());
    }
    for reply in replies {
        reply.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    let total = started.elapsed();
    instance.destroy(Duration::from_secs(5)).unwrap();

    let entries = log.lock().unwrap();
    assert_eq!(entries.len(), 3);
    // Three 50ms jobs run one-at-a-time would take >= 150ms; overlapping
    // them on a 4-worker pool should finish well under that.
    assert!(
        total < Duration::from_millis(140),
        "expected concurrent dispatch to overlap, took {total:?}"
    );
}

// --- Destroy grace timeout (§4) ------------------------------------------

mod slow_destroy {
    use super::{Init, Message, Plugin, PluginError};

    pub struct SlowDestroy;
    impl Plugin for SlowDestroy {
        fn create(
            _host: vosu_plugin_api::VtableHostServices,
            _init: Init,
        ) -> Result<Self, PluginError> {
            Ok(Self)
        }
        fn on_message(
            &mut self,
            _message: Message,
            _correlation_id: u64,
        ) -> Result<(), PluginError> {
            Ok(())
        }
    }
    impl Drop for SlowDestroy {
        fn drop(&mut self) {
            std::thread::sleep(std::time::Duration::from_millis(200));
        }
    }
    vosu_plugin_api::export_plugin!(bundled SlowDestroy);
}

#[test]
fn destroy_times_out_when_the_plugin_does_not_return_in_time() {
    let text = manifest("acme.slow", "");
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &text,
        vtable: slow_destroy::vosu_plugin_vtable,
    }])
    .unwrap();
    let instance = PluginInstance::create(
        PluginInstanceId::new(3),
        &loaded[0],
        &init_bytes("acme.slow"),
        Arc::new(NullHandler),
        None,
    )
    .unwrap();

    match instance.destroy(Duration::from_millis(10)) {
        Err(HostError::DestroyTimedOut) => {}
        Err(other) => panic!("expected DestroyTimedOut, got {other}"),
        Ok(()) => panic!("expected destroy to time out"),
    }
}
