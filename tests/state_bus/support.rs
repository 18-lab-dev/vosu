use std::sync::{
    Arc, Mutex,
    mpsc::{Receiver, SyncSender, sync_channel},
};
use std::time::{Duration, Instant};
use vosu_core::plugin::{self, PluginHost, PluginInstanceId};
use vosu_plugin_api::{
    HostServices, PublishBatchWriter, PublishValueRef, Selector, StateEntry, vosu_opcode,
    vosu_status,
};
use vosu_plugin_testkit::{
    publisher::{self, PublisherEvent},
    subscriber::{self, SubscribeCommand, SubscriberEvent},
};

pub const WAIT: Duration = Duration::from_secs(30);
pub const NS: &str = "vosu.osu";
pub const SENTINEL: &str = "/private/K8-sentinel-never-public";
struct Storage;
pub fn osu_lease() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    let deadline = Instant::now() + WAIT;
    loop {
        if let Ok(guard) = LOCK.try_lock() {
            return guard;
        }
        assert!(Instant::now() < deadline, "bounded vosu.osu fixture lease");
        std::thread::yield_now();
    }
}
impl plugin::settings::SettingsStorage for Storage {
    fn load(
        &self,
        _: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, plugin::settings::SettingsError> {
        Ok(Default::default())
    }
    fn store(
        &self,
        _: &str,
        _: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), plugin::settings::SettingsError> {
        Ok(())
    }
}
struct Console;
impl vosu_core::logging::ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
}
pub struct Harness {
    pub host: Arc<PluginHost>,
    pub runtime: tokio::runtime::Runtime,
    dirs: tempfile::TempDir,
    next: u64,
}
impl Harness {
    pub fn new() -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let dirs = tempfile::tempdir().unwrap();
        let logger = {
            let _enter = runtime.enter();
            vosu_core::logging::Logger::new(
                dirs.path(),
                vosu_core::logging::LoggerOptions {
                    version: "test".into(),
                    debug_log: true,
                    queue_capacity: 64,
                },
                Arc::new(Console),
            )
            .unwrap()
        };
        let host = PluginHost::new_with_foreground(
            logger,
            Arc::new(plugin::settings::PluginSettingsService::new(Arc::new(
                Storage,
            ))),
            runtime.handle().clone(),
            Arc::new(plugin::SyntheticForegroundSource::new(None)),
        );
        Self {
            host,
            runtime,
            dirs,
            next: 1,
        }
    }
    pub fn load(
        &mut self,
        manifest: &str,
        vtable: fn() -> vosu_plugin_api::vosu_plugin_vtable,
        schema: Option<(&str, &str)>,
    ) {
        let schemas: Vec<_> = schema.into_iter().map(|(_, text)| text).collect();
        let loaded = plugin::load_bundled(
            &semver::Version::parse(env!("CARGO_PKG_VERSION")).unwrap(),
            &self.host.validation_snapshot(),
            &[plugin::BundledPlugin {
                manifest,
                settings_schema: None,
                namespace_schemas: &schemas,
                action_schemas: &[],
                vtable,
            }],
        )
        .unwrap();
        self.host
            .create_instance(
                PluginInstanceId::new(self.next),
                &loaded[0],
                self.dirs.path().into(),
                Some(plugin::WorkerPool::new(2, 64)),
            )
            .unwrap();
        self.next += 1;
    }
    pub fn publisher(&mut self, id: &str, pid: u32) -> (u64, Receiver<PublisherEvent>) {
        let manifest = format!(
            "{}\nconcurrent_dispatch=true\n[[provides]]\nid='field:{id}'\nversion='1.2.0'\nschema='osu.json'\n[match]\nprocess_names=['{id}']\n[[provides]]\nid='game:fixture'\nversion='*'\n",
            manifest(id)
        );
        publisher::register_manifest(id, &manifest);
        let events = publisher::register(id);
        let schema = if id == NS {
            include_str!("../../docs/v2/schemas/vosu.osu.schema.json").to_owned()
        } else {
            format!(r#"{{"title":"{id}","version":"1.2.0","type":"object","properties":{{}}}}"#)
        };
        self.load(
            &manifest,
            publisher::vosu_plugin_vtable,
            Some((id, &schema)),
        );
        self.host.process_appeared(record(pid, id));
        let handle = until(&events, |event| {
            if let PublisherEvent::Attached { handle } = event {
                Some(handle)
            } else {
                None
            }
        });
        (handle, events)
    }
    pub fn subscriber(&mut self, id: &str, grant: &str) -> Observer {
        let (events, commands) = subscriber::register(id);
        self.load(
            &format!("{}{grant}", manifest(id)),
            subscriber::vosu_plugin_vtable,
            None,
        );
        Observer { events, commands }
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.host.destroy_all(WAIT);
    }
}
pub fn manifest(id: &str) -> String {
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='{id}'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n"
    )
}
pub fn record(pid: u32, id: &str) -> plugin::ProcessRecord {
    plugin::ProcessRecord {
        pid,
        name: id.into(),
        path: SENTINEL.into(),
        command_line: SENTINEL.into(),
        argv: Some(vec![SENTINEL.into()]),
        bitness: vosu_plugin_api::Bitness::X64,
        start_time: 1,
    }
}
pub fn select(pointer: &str) -> Selector {
    Selector {
        ns: NS.into(),
        pointer: pointer.into(),
    }
}
pub fn select_ns(ns: &str, pointer: &str) -> Selector {
    Selector {
        ns: ns.into(),
        pointer: pointer.into(),
    }
}
pub fn until<T, R>(rx: &Receiver<T>, mut f: impl FnMut(T) -> Option<R>) -> R {
    let deadline = Instant::now() + WAIT;
    loop {
        let event = rx
            .recv_timeout(deadline.saturating_duration_since(Instant::now()))
            .expect("bounded event deadline");
        if let Some(result) = f(event) {
            return result;
        }
    }
}
pub struct Observer {
    pub events: Receiver<SubscriberEvent>,
    commands: SyncSender<SubscribeCommand>,
}
impl Observer {
    pub fn subscribe(&self, session: Option<u64>, selectors: Vec<Selector>) {
        self.commands
            .try_send(SubscribeCommand { session, selectors })
            .unwrap();
    }
    pub fn learn(&self) -> vosu_plugin_api::SessionAttached {
        self.subscribe(
            None,
            vec![Selector {
                ns: "vosu.sessions".into(),
                pointer: String::new(),
            }],
        );
        until(&self.events, |event| {
            if let SubscriberEvent::Attached(attached) = event {
                Some(attached)
            } else {
                None
            }
        })
    }
    pub fn update(&self) -> Vec<StateEntry> {
        until(&self.events, |event| match event {
            SubscriberEvent::Update { values, .. } => Some(values),
            SubscriberEvent::SubscribeStatus(status) => {
                assert_eq!(status, vosu_status::VOSU_OK);
                None
            }
            _ => None,
        })
    }
}
pub fn publish(id: &str, handle: u64, fields: &[(&str, PublishValueRef<'_>)]) -> vosu_status {
    let mut writer = PublishBatchWriter::new();
    writer.begin(handle);
    for (pointer, value) in fields {
        writer.push(id, pointer, *value);
    }
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    publisher::host_services(id).send_flat(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, 0, &bytes)
}
pub fn clean(value: impl std::fmt::Debug) {
    assert!(!format!("{value:?}").contains(SENTINEL));
}

#[derive(Clone, Default)]
pub struct Capture(pub Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Capture {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Self;
    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

pub mod delayed {
    use super::*;
    use vosu_plugin_api::{ConcurrentPlugin, Init, Message, PluginError, VtableHostServices};
    pub static EVENTS: Mutex<Option<SyncSender<(u64, VtableHostServices)>>> = Mutex::new(None);
    pub struct Delayed(VtableHostServices);
    impl ConcurrentPlugin for Delayed {
        fn create(host: VtableHostServices, _: Init) -> Result<Self, PluginError> {
            Ok(Self(host))
        }
        fn on_message(&self, message: Message, _: u64) -> Result<(), PluginError> {
            if let Message::SessionAttached(attached) = message {
                EVENTS
                    .lock()
                    .unwrap()
                    .as_ref()
                    .unwrap()
                    .try_send((attached.handle, self.0))
                    .unwrap();
            }
            Ok(())
        }
    }
    vosu_plugin_api::export_plugin!(bundled concurrent Delayed);
    pub fn register() -> Receiver<(u64, VtableHostServices)> {
        let (tx, rx) = sync_channel(1);
        *EVENTS.lock().unwrap() = Some(tx);
        rx
    }
}
