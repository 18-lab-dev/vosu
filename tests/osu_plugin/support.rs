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
pub static PANICS: AtomicUsize = AtomicUsize::new(0);
pub static IN_FLIGHT: AtomicUsize = AtomicUsize::new(0);
pub static PEAK: AtomicUsize = AtomicUsize::new(0);
pub static FOCUS_GAINS: Mutex<BTreeMap<u64, usize>> = Mutex::new(BTreeMap::new());
pub static HANDLES: Mutex<BTreeMap<u32, u64>> = Mutex::new(BTreeMap::new());
pub static ATTACHES: Mutex<BTreeMap<u32, usize>> = Mutex::new(BTreeMap::new());
static TEST_BUSY: Mutex<bool> = Mutex::new(false);
static TEST_AVAILABLE: Condvar = Condvar::new();
static PANIC_HOOK: std::sync::Once = std::sync::Once::new();

// A Send + Sync ownership token, not a thread-affine MutexGuard: Harness is
// shared with producers, but the process-wide B1 slot stays exclusive until
// all its fields (including runtime tasks) have been dropped.
struct TestLease;
impl TestLease {
    fn acquire() -> Self {
        let deadline = Instant::now() + Duration::from_secs(120);
        let mut busy = TEST_BUSY.lock().unwrap();
        while *busy {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(!remaining.is_zero(), "harness lease deadline");
            busy = TEST_AVAILABLE.wait_timeout(busy, remaining).unwrap().0;
        }
        *busy = true;
        PANIC_HOOK.call_once(|| {
            let previous = std::panic::take_hook();
            std::panic::set_hook(Box::new(move |info| {
                PANICS.fetch_add(1, Ordering::Relaxed);
                previous(info);
            }));
        });
        for counter in [&PANICS, &DISPATCHES, &IN_FLIGHT, &PEAK] {
            counter.store(0, Ordering::Relaxed);
        }
        FAILURES.lock().unwrap().clear();
        DECLINES.lock().unwrap().clear();
        ATTACHES.lock().unwrap().clear();
        FOCUS_GAINS.lock().unwrap().clear();
        HANDLES.lock().unwrap().clear();
        Self
    }
}
impl Drop for TestLease {
    fn drop(&mut self) {
        let panics = PANICS.load(Ordering::Relaxed);
        *TEST_BUSY.lock().unwrap() = false;
        TEST_AVAILABLE.notify_one();
        if !std::thread::panicking() {
            assert_eq!(panics, 0, "panic on a dispatch, reader or runtime thread");
        }
    }
}

struct DispatchGuard;
impl Drop for DispatchGuard {
    fn drop(&mut self) {
        DISPATCHES.fetch_add(1, Ordering::Relaxed);
        IN_FLIGHT.fetch_sub(1, Ordering::Relaxed);
        if std::thread::panicking() {
            FAILURES.lock().unwrap().push("dispatch panicked".into());
        }
    }
}

// Instrument the real entry point, not an imitation: all dispatches, including
// host-internal deliveries without a public status receiver, are checked.
pub struct ObservedPlugin(vosu_osu::plugin::OsuPlugin);
impl ConcurrentPlugin for ObservedPlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        <vosu_osu::plugin::OsuPlugin as ConcurrentPlugin>::create(host, init).map(Self)
    }
    fn on_message(&self, message: Message, correlation_id: u64) -> Result<(), PluginError> {
        let active = IN_FLIGHT.fetch_add(1, Ordering::Relaxed) + 1;
        PEAK.fetch_max(active, Ordering::Relaxed);
        let _guard = DispatchGuard;
        if let Message::FocusChanged(focus) = &message
            && focus.focused
        {
            *FOCUS_GAINS.lock().unwrap().entry(focus.handle).or_default() += 1;
        }
        if let Message::SessionAttached(attached) = &message {
            *ATTACHES.lock().unwrap().entry(attached.pid).or_default() += 1;
            HANDLES
                .lock()
                .unwrap()
                .insert(attached.pid, attached.handle);
        }
        if let Message::SessionDetached(detached) = &message {
            DECLINES.lock().unwrap().push(detached.reason);
        }
        let result = self.0.on_message(message, correlation_id);
        if let Err(error) = &result {
            FAILURES.lock().unwrap().push(format!("{error:?}"));
        }
        result
    }
}
vosu_plugin_api::export_plugin!(bundled concurrent ObservedPlugin);

#[derive(Default)]
pub struct EventOrder {
    pub attached: Option<usize>,
    pub starting: Option<usize>,
    pub resolving: Option<usize>,
    pub reading: Option<usize>,
    pub first_publish: Option<usize>,
}
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
    pub latest: BTreeMap<B1SessionKey, Arc<vosu_osu_state::StateBundleSnapshot>>,
    pub detached: BTreeSet<B1SessionKey>,
    pub late_publish: bool,
    sequence: usize,
    pub order: BTreeMap<B1SessionKey, EventOrder>,
}
pub struct Sink {
    pub events: Mutex<Events>,
    changed: Condvar,
    pub inputs: tokio::sync::watch::Sender<B1Inputs>,
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
        let sequence = events.sequence;
        events.sequence += 1;
        match event {
            B1Event::Attached {
                key,
                client,
                settings,
                ..
            } => {
                events.order.entry(key).or_default().attached = Some(sequence);
                assert!(events.identified.insert(key, (client, settings)).is_none());
                assert!(events.attached.insert(key));
            }
            B1Event::Phase { key, phase } => {
                let order = events.order.entry(key).or_default();
                match phase {
                    B1Phase::ResolvingOffsets => &mut order.resolving,
                    B1Phase::StartingReader => &mut order.starting,
                    B1Phase::Reading => &mut order.reading,
                }
                .get_or_insert(sequence);
                events.phases.entry(key).or_default().push(phase);
            }
            B1Event::Publish { key, live, .. } => {
                events.latest.insert(key, live);
                events
                    .order
                    .entry(key)
                    .or_default()
                    .first_publish
                    .get_or_insert(sequence);
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
    status: AtomicUsize,
    status_address: usize,
}
impl Memory {
    fn fixture() -> Arc<Self> {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/raw_memory/stable_patterns.json"))
                .unwrap();
        let mut image = Vec::new();
        let mut status_address = 0;
        for (index, item) in fixture["patterns"].as_array().unwrap().iter().enumerate() {
            if index == 9 {
                // status_ptr is four bytes before its signature. Redirect its
                // pointer to a separate mutable scalar, leaving scan bytes real.
                let offset = image.len() - 4;
                status_address = 0x20_0000;
                image[offset..].copy_from_slice(&(status_address as u32).to_le_bytes());
            }
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
            status: AtomicUsize::new(0),
            status_address,
        })
    }
    pub fn set_status(&self, value: usize) {
        self.status.store(value, Ordering::Release);
    }
}
impl ProcessMemory for Memory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        self.reads.fetch_add(1, Ordering::Relaxed);
        dst.fill(0);
        if address == self.status_address && dst.len() == 4 {
            dst.copy_from_slice(&(self.status.load(Ordering::Acquire) as u32).to_le_bytes());
            return Ok(());
        }
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
    pub foreground: Arc<plugin::SyntheticForegroundSource>,
    config: Arc<vosu_core::config::ConfigStore>,
    _installed: Option<vosu_surface::Installed>,
    _dirs: Vec<tempfile::TempDir>,
    _runtime: tokio::runtime::Runtime,
    _lease: TestLease,
}
struct Console;
// The production B2 bridge intentionally serves only vosu.osu. Preserve its
// real persistence semantics while each integration instance has a unique id.
struct Storage {
    bridge: plugin::settings::ConfigSettingsBridgeB2,
    expected_id: String,
}
impl plugin::settings::SettingsStorage for Storage {
    fn load(
        &self,
        id: &str,
    ) -> Result<serde_json::Map<String, serde_json::Value>, plugin::settings::SettingsError> {
        assert_eq!(id, self.expected_id);
        plugin::settings::SettingsStorage::load(&self.bridge, "vosu.osu")
    }
    fn store(
        &self,
        id: &str,
        values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), plugin::settings::SettingsError> {
        assert_eq!(id, self.expected_id);
        plugin::settings::SettingsStorage::store(&self.bridge, "vosu.osu", values)
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
        let lease = TestLease::acquire();
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
        let config = Arc::new(
            runtime
                .block_on(vosu_core::config::ConfigStore::initialize(
                    &dirs[1].path().join("vosu.toml"),
                ))
                .unwrap(),
        );
        let storage = Arc::new(Storage {
            bridge: plugin::settings::ConfigSettingsBridgeB2::new(Arc::clone(&config)),
            expected_id: id.into(),
        });
        let settings = Arc::new(plugin::settings::PluginSettingsService::new(storage));
        let foreground = Arc::new(plugin::SyntheticForegroundSource::new(None));
        let host = PluginHost::new_with_foreground(
            logger,
            settings.clone(),
            runtime.handle().clone(),
            foreground.clone(),
        );
        let mut manifest: toml::Table = toml::from_str(vosu_osu::MANIFEST).unwrap();
        let namespace_schemas = if id == "vosu.osu" {
            &[vosu_osu::OSU_NAMESPACE_SCHEMA][..]
        } else {
            // Synthetic plugin identities cannot provide the reserved real
            // namespace. These B1 lifecycle fixtures retain common coverage.
            // Real-id schema registration is also covered by tests/state_bus.rs
            // (K8) and real_manifest_registers_its_namespace_schema.
            manifest.remove("provides");
            manifest.remove("actions");
            &[][..]
        };
        manifest.insert("id".into(), toml::Value::String(id.into()));
        let manifest = toml::to_string(&manifest).unwrap();
        let loaded = plugin::load_bundled(&[plugin::BundledPlugin {
            manifest: &manifest,
            settings_schema: Some(vosu_osu::SETTINGS_SCHEMA),
            namespace_schemas,
            action_schemas: vosu_osu::ACTION_SCHEMAS,
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
            foreground,
            config,
            _installed: Some(installed),
            _dirs: dirs,
            _runtime: runtime,
            _lease: lease,
        }
    }
    pub fn remove_b1(&mut self) {
        self._installed.take();
    }
    pub fn subscriber(
        &self,
        id: &str,
    ) -> (
        std::sync::mpsc::Receiver<vosu_plugin_testkit::subscriber::SubscriberEvent>,
        std::sync::mpsc::SyncSender<vosu_plugin_testkit::subscriber::SubscribeCommand>,
    ) {
        let (events, commands) = vosu_plugin_testkit::subscriber::register(id);
        let manifest = format!(
            "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='{id}'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n"
        );
        let loaded = plugin::load_bundled(&[plugin::BundledPlugin {
            manifest: &manifest,
            settings_schema: None,
            namespace_schemas: &[],
            action_schemas: &[],
            vtable: vosu_plugin_testkit::subscriber::vosu_plugin_vtable,
        }])
        .unwrap();
        self.host
            .create_instance(
                PluginInstanceId::new(2),
                &loaded[0],
                self._dirs[2].path().to_path_buf(),
                Some(plugin::WorkerPool::new(2, 64)),
            )
            .unwrap();
        (events, commands)
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
        // B2 writes can still be saving/reloading after the session detaches.
        // Join the config worker before dropping its directory or Tokio's timer
        // driver: its spawn_blocking task is not cancelled by runtime shutdown.
        self._runtime
            .block_on(async { tokio::time::timeout(WAIT, self.config.shutdown()).await })
            .expect("config shutdown deadline expired")
            .expect("config shutdown failed");
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
    assert_eq!(PANICS.load(Ordering::Relaxed), 0, "background panic");
    assert!(
        FAILURES.lock().unwrap().is_empty(),
        "dispatch failures: {:?}",
        *FAILURES.lock().unwrap()
    );
}
