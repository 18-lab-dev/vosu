use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};
use vosu_core::plugin::{self, PluginHost, PluginInstanceId, ProcessRecord};
use vosu_osu::bridge_b1::{B1Event, B1Inputs, B1Phase, B1SessionKey};
use vosu_plugin_api::{
    ConcurrentPlugin, Init, Message, PluginError, VtableHostServices, vosu_status,
};
use vosu_sys::process::{MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest};

pub const WAIT: Duration = Duration::from_secs(5);
pub static DISPATCHES: AtomicUsize = AtomicUsize::new(0);
pub static FAILURES: Mutex<Vec<String>> = Mutex::new(Vec::new());
pub static DECLINES: Mutex<Vec<vosu_plugin_api::DetachReason>> = Mutex::new(Vec::new());

// Instrument the real entry point, not an imitation: all dispatches, including
// host-internal deliveries without a public status receiver, are checked.
pub struct ObservedPlugin(vosu_osu::plugin::OsuPlugin);
impl ConcurrentPlugin for ObservedPlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        <vosu_osu::plugin::OsuPlugin as ConcurrentPlugin>::create(host, init).map(Self)
    }
    fn on_message(&self, message: Message, correlation_id: u64) -> Result<(), PluginError> {
        if let Message::SessionDetached(detached) = &message {
            DECLINES.lock().unwrap().push(detached.reason);
        }
        let result = self.0.on_message(message, correlation_id);
        DISPATCHES.fetch_add(1, Ordering::Relaxed);
        if let Err(error) = &result {
            FAILURES.lock().unwrap().push(format!("{error:?}"));
        }
        result
    }
}
vosu_plugin_api::export_plugin!(bundled concurrent ObservedPlugin);

#[derive(Default)]
pub struct Events {
    pub attached: BTreeSet<B1SessionKey>,
    pub identified: BTreeMap<
        B1SessionKey,
        (
            vosu_osu_model::enums::ClientType,
            vosu_osu_model::launch::InstanceSettings,
        ),
    >,
    pub phases: BTreeMap<B1SessionKey, Vec<B1Phase>>,
    pub published: BTreeMap<B1SessionKey, usize>,
    pub detached: BTreeSet<B1SessionKey>,
    pub late_publish: bool,
}
pub struct Sink {
    pub events: Mutex<Events>,
    changed: Condvar,
    inputs: tokio::sync::watch::Sender<B1Inputs>,
}
impl Sink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(Events::default()),
            changed: Condvar::new(),
            inputs: tokio::sync::watch::channel(B1Inputs::default()).0,
        })
    }
    pub fn until(&self, timeout: Duration, predicate: impl Fn(&Events) -> bool) {
        let deadline = Instant::now() + timeout;
        let mut events = self.events.lock().unwrap();
        while !predicate(&events) {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "B1 event deadline expired");
            events = self.changed.wait_timeout(events, remaining).unwrap().0;
        }
    }
}
impl vosu_surface::LegacySink<B1Event, B1Inputs> for Sink {
    fn send(&self, event: B1Event) {
        let mut events = self.events.lock().unwrap();
        match event {
            B1Event::Attached {
                key,
                client,
                settings,
                ..
            } => {
                assert!(events.identified.insert(key, (client, settings)).is_none());
                assert!(events.attached.insert(key));
            }
            B1Event::Phase { key, phase } => {
                events.phases.entry(key).or_default().push(phase);
            }
            B1Event::Publish { key, .. } => {
                events.late_publish |= events.detached.contains(&key);
                *events.published.entry(key).or_default() += 1;
            }
            B1Event::Detached { key } => {
                assert!(events.attached.remove(&key));
                assert!(events.detached.insert(key));
            }
        }
        self.changed.notify_all();
    }
    fn inputs(&self) -> tokio::sync::watch::Receiver<B1Inputs> {
        self.inputs.subscribe()
    }
}

pub struct Memory {
    base: usize,
    image: Vec<u8>,
    pub reads: AtomicUsize,
    pub plans: AtomicUsize,
}
impl Memory {
    fn fixture() -> Arc<Self> {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/raw_memory/stable_patterns.json"))
                .unwrap();
        let mut image = Vec::new();
        for item in fixture["patterns"].as_array().unwrap() {
            let pattern = vosu_osu_reader::process::Pattern::parse(
                item["signature"].as_str().unwrap(),
                false,
            )
            .unwrap();
            image.extend(
                pattern
                    .bytes()
                    .iter()
                    .zip(pattern.mask())
                    .map(|(&byte, &mask)| {
                        if mask == 0 {
                            fixture["wildcard_byte"].as_u64().unwrap() as u8
                        } else {
                            byte
                        }
                    }),
            );
            image.extend(std::iter::repeat_n(
                0,
                fixture["gap_bytes"].as_u64().unwrap() as usize,
            ));
        }
        Arc::new(Self {
            base: fixture["base_address"].as_u64().unwrap() as usize,
            image,
            reads: AtomicUsize::new(0),
            plans: AtomicUsize::new(0),
        })
    }
}
impl ProcessMemory for Memory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        dst.fill(0);
        // Scans read this fixture's entire single region; scalar reader
        // requests are smaller and must have traversed the sync-plan route.
        if dst.len() < self.image.len() {
            self.plans.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(offset) = address.checked_sub(self.base)
            && let Some(bytes) = self.image.get(offset..offset.saturating_add(dst.len()))
        {
            dst.copy_from_slice(bytes);
        }
        Ok(())
    }
    fn read_many(&self, requests: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for request in requests {
            self.read_exact(request.address, request.dst)?;
        }
        Ok(())
    }
    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: self.base,
            size: self.image.len(),
        }])
    }
}

pub struct Harness {
    pub host: Arc<PluginHost>,
    pub settings: Arc<plugin::settings::PluginSettingsService>,
    pub schema: plugin::settings::SettingsSchema,
    pub id: String,
    pub sink: Arc<Sink>,
    pub memory: Arc<Memory>,
    pub memories: Vec<Arc<Memory>>,
    pub pids: Vec<u32>,
    _installed: vosu_surface::Installed,
    _dirs: Vec<tempfile::TempDir>,
    _runtime: tokio::runtime::Runtime,
}
struct Console;
// The production B2 bridge intentionally serves only vosu.osu. Preserve its
// real persistence semantics while each integration instance has a unique id.
struct Storage(plugin::settings::ConfigSettingsBridgeB2);
impl plugin::settings::SettingsStorage for Storage {
    fn load(
        &self,
        _: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, plugin::settings::SettingsError> {
        plugin::settings::SettingsStorage::load(&self.0, "vosu.osu")
    }
    fn store(
        &self,
        _: &str,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), plugin::settings::SettingsError> {
        plugin::settings::SettingsStorage::store(&self.0, "vosu.osu", values)
    }
}
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
impl Harness {
    pub fn new(id: &str, first_pid: u32, count: u32) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let dirs: Vec<_> = (0..3).map(|_| tempfile::tempdir().unwrap()).collect();
        let enter = runtime.enter();
        let logger = vosu_core::logging::Logger::new(
            dirs[0].path(),
            vosu_core::logging::LoggerOptions {
                version: "test".into(),
                debug_log: true,
                queue_capacity: 4096,
            },
            Arc::new(Console),
        )
        .unwrap();
        let config = runtime
            .block_on(vosu_core::config::ConfigStore::initialize(
                &dirs[1].path().join("vosu.toml"),
            ))
            .unwrap();
        let storage = Arc::new(Storage(plugin::settings::ConfigSettingsBridgeB2::new(
            Arc::new(config),
        )));
        let settings = Arc::new(plugin::settings::PluginSettingsService::new(storage));
        let host = PluginHost::new(logger, settings.clone(), runtime.handle().clone());
        let manifest = vosu_osu::MANIFEST.replace("id = \"vosu.osu\"", &format!("id = \"{id}\""));
        let loaded = plugin::load_bundled(&[plugin::BundledPlugin {
            manifest: &manifest,
            settings_schema: Some(vosu_osu::SETTINGS_SCHEMA),
            vtable: vosu_plugin_vtable,
        }])
        .unwrap();
        let sink = Sink::new();
        let installed =
            vosu_surface::install(sink.clone() as Arc<vosu_osu::bridge_b1::B1Sink>).unwrap();
        host.create_instance(
            PluginInstanceId::new(1),
            &loaded[0],
            dirs[2].path().to_path_buf(),
            Some(plugin::WorkerPool::new(4, 65536)),
        )
        .unwrap();
        let pids: Vec<_> = (first_pid..first_pid + count).collect();
        let memories: Vec<_> = pids
            .iter()
            .map(|&pid| {
                let memory = Memory::fixture();
                plugin::test_support::set_session_memory(pid, memory.clone(), ProcessBitness::X86);
                memory
            })
            .collect();
        let memory = memories[0].clone();
        drop(enter);
        Self {
            host,
            settings,
            schema: plugin::settings::SettingsSchema::parse(vosu_osu::SETTINGS_SCHEMA).unwrap(),
            id: id.into(),
            sink,
            memory,
            memories,
            pids,
            _installed: installed,
            _dirs: dirs,
            _runtime: runtime,
        }
    }
    pub fn record(pid: u32, args: &[&str]) -> ProcessRecord {
        ProcessRecord {
            pid,
            name: "osu!.exe".into(),
            path: "/games/osu!.exe".into(),
            command_line: std::iter::once("osu!.exe")
                .chain(args.iter().copied())
                .collect::<Vec<_>>()
                .join(" "),
            bitness: vosu_plugin_api::Bitness::X86,
            start_time: 1,
            argv: Some(
                std::iter::once("osu!.exe")
                    .chain(args.iter().copied())
                    .map(str::to_owned)
                    .collect(),
            ),
        }
    }
    pub fn apply(&self, rate: f64) -> Message {
        self.settings
            .apply(
                &self.id,
                &self.schema,
                &serde_json::Map::from_iter([("POLL_RATE".into(), serde_json::Value::from(rate))]),
            )
            .unwrap()
    }
    pub fn dispatch(&self, message: &Message) {
        assert_eq!(
            self.host
                .dispatch_to_instance(PluginInstanceId::new(1), message, 0)
                .unwrap()
                .recv_timeout(WAIT)
                .unwrap(),
            vosu_status::VOSU_OK
        );
    }
}
impl Drop for Harness {
    fn drop(&mut self) {
        self.host.destroy_all(WAIT);
        for &pid in &self.pids {
            plugin::test_support::clear_session_memory(pid);
        }
    }
}
pub fn eventually(predicate: impl Fn() -> bool) {
    let deadline = Instant::now() + WAIT;
    while !predicate() {
        assert!(Instant::now() < deadline, "observation deadline expired");
        std::thread::yield_now();
    }
}

pub fn assert_dispatches_ok() {
    assert!(DISPATCHES.load(Ordering::Relaxed) > 0);
    assert!(
        FAILURES.lock().unwrap().is_empty(),
        "dispatch failures: {:?}",
        *FAILURES.lock().unwrap()
    );
}
