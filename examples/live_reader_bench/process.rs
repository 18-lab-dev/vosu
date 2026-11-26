//! M5 D17: score the actual osu! cdylib in a real plugin host.
#[path = "fixture.rs"]
mod fixture;
#[path = "../../benches/support/brokered/os.rs"]
#[expect(
    dead_code,
    reason = "the broker bench uses the rest of these OS probes"
)]
mod os;
#[path = "process_protocol.rs"]
mod wire;

use super::{Args, Result, fail};
use std::{
    cell::Cell,
    io::Write,
    path::{Path, PathBuf},
    sync::Arc,
    thread,
    time::{Duration, Instant},
};
use vosu_core::plugin::{
    self, Execution, LoadedPlugin, PluginHost, PluginInstanceId, ProcessRecord,
    approval::{ApprovalGate, ApprovalStore, Digests, Provenance},
    process::{launcher::ProcessHostLauncher, supervisor::ProcessPhase},
};
use vosu_plugin_api::{
    ActionInvoke, Caller, CallerKind, CborValue, Message, Selector, vosu_status,
};
use vosu_surface::state::SessionTarget;

const BOUND: Duration = Duration::from_secs(20);
const DELIVERY_BYTES: u64 = 64 * 1024 * 1024; // plugin-abi.md §7.6
const OSU: u64 = 1;
const CONTENDER: u64 = 2;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) enum Profile {
    #[default]
    NoClient,
    SubscribedPrecise,
    Contention,
    DelayedDispatch,
    FullStream,
    IdleHost,
}

impl Profile {
    const ALL: [Self; 6] = [
        Self::NoClient,
        Self::SubscribedPrecise,
        Self::Contention,
        Self::DelayedDispatch,
        Self::FullStream,
        Self::IdleHost,
    ];

    pub(super) const fn name(self) -> &'static str {
        match self {
            Self::NoClient => "no_client",
            Self::SubscribedPrecise => "subscribed_precise",
            Self::Contention => "contention",
            Self::DelayedDispatch => "delayed_dispatch",
            Self::FullStream => "full_stream",
            Self::IdleHost => "idle_host",
        }
    }

    pub(super) fn parse(name: &str) -> Result<Self> {
        Self::ALL
            .into_iter()
            .find(|p| p.name() == name)
            .ok_or_else(|| fail(format!("unknown process profile {name}")))
    }
}

const LANES: [&str; 2] = ["regular", "precise"];
const PREFIX: &str = "live_reader/process";

pub(super) fn list() {
    for client in ["stable", "lazer"] {
        for lane in LANES {
            println!("{PREFIX}/{client}/{lane}");
        }
    }
    for profile in Profile::ALL {
        println!("{PREFIX}/{}", profile.name());
    }
}

pub(super) fn target() -> Result<()> {
    fixture::target()
}

struct Host {
    host: Arc<PluginHost>,
    approvals: Arc<ApprovalStore>,
    runtime: tokio::runtime::Runtime,
    pool: Arc<plugin::WorkerPool>,
    root: tempfile::TempDir,
    lib: PathBuf,
    next: Cell<u64>,
}

impl Host {
    fn new() -> Result<Self> {
        let executable = std::env::current_exe()?;
        let examples = executable
            .parent()
            .ok_or_else(|| fail("missing examples directory"))?;
        let daemon = examples
            .parent()
            .ok_or_else(|| fail("missing Cargo profile directory"))?
            .join(format!("vosu{}", std::env::consts::EXE_SUFFIX));
        let lib = examples.join(format!(
            "{}live_reader_process_plugin{}",
            std::env::consts::DLL_PREFIX,
            std::env::consts::DLL_SUFFIX
        ));
        for artifact in [&daemon, &lib] {
            if !artifact.is_file() {
                return Err(fail(format!(
                    "missing {}; build --bin vosu --example live_reader_bench --example live_reader_process_plugin together",
                    artifact.display()
                )));
            }
        }
        let root = tempfile::tempdir()?;
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()?;
        let config = runtime.block_on(vosu_core::config::ConfigStore::initialize(
            &root.path().join("vosu.toml"),
        ))?;
        let settings = Arc::new(plugin::settings::PluginSettingsService::new(Arc::new(
            plugin::settings::ConfigSettingsBridgeB2::new(Arc::new(config)),
        )));
        let host = {
            let _enter = runtime.enter();
            let logger = vosu_core::logging::Logger::new(
                root.path().join("logs"),
                vosu_core::logging::LoggerOptions {
                    version: vosu::BUNDLE.version.into(),
                    debug_log: false,
                    queue_capacity: 64,
                },
                Arc::new(super::plan::Silent),
            )?;
            PluginHost::new(logger, settings, runtime.handle().clone())
        };
        let approvals = Arc::new(ApprovalStore::open(root.path()));
        let launcher =
            ProcessHostLauncher::with_executable(&daemon, vosu::BUNDLE.plugin_host_identity)?;
        host.install_process_launcher(Arc::new(ApprovalGate::new(approvals.clone(), launcher)))?;
        let pool = plugin::WorkerPool::new(4, plugin::DEFAULT_QUEUE_CAPACITY);
        Ok(Self {
            host,
            approvals,
            runtime,
            pool,
            root,
            lib,
            next: Cell::new(1),
        })
    }

    fn package(&self, contender_name: Option<&str>) -> Result<LoadedPlugin> {
        let id = if contender_name.is_some() {
            wire::CONTENDER
        } else {
            wire::OSU
        };
        let dir = self.root.path().join(id);
        std::fs::create_dir_all(dir.join("lib"))?;
        let library = self
            .lib
            .file_name()
            .ok_or_else(|| fail("missing library name"))?;
        std::fs::copy(&self.lib, dir.join("lib").join(library))?;
        let version = vosu::BUNDLE.version();
        let snapshot = self.host.validation_snapshot();
        if let Some(name) = contender_name {
            let name = serde_json::to_string(name)?;
            let manifest = format!(
                "kind='plugin'\nmanifest_version=1\nid='{id}'\nname='Broker contender'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='process'\nconcurrent_dispatch=true\n[match]\nprocess_names=[{name}]\nlinux_name_contains=[{name}]\n[[provides]]\nid='game:benchmark'\nversion='*'\n[targets.{target}]\nlibrary='lib/{library}'\n",
                target = if cfg!(windows) {
                    "x86_64-pc-windows-msvc"
                } else {
                    "x86_64-unknown-linux-gnu"
                },
                library = library.display(),
            );
            write_file(&dir.join("manifest.toml"), manifest.as_bytes())?;
            Ok(plugin::load_development(
                &version,
                &snapshot,
                &dir,
                cfg!(debug_assertions),
            )?)
        } else {
            let manifest = include_str!("../../tests/fixtures/plugin_host/osu/manifest.toml")
                .replace("osu_process_plugin", "live_reader_process_plugin");
            write_file(&dir.join("manifest.toml"), manifest.as_bytes())?;
            std::fs::create_dir(dir.join("schemas"))?;
            write_file(
                &dir.join("settings.schema.json"),
                vosu_osu::SETTINGS_SCHEMA.as_bytes(),
            )?;
            write_file(
                &dir.join("schemas/vosu.osu.schema.json"),
                vosu_osu::OSU_NAMESPACE_SCHEMA.as_bytes(),
            )?;
            for (path, schema) in vosu_osu::ACTION_SCHEMAS {
                write_file(&dir.join(path), schema.as_bytes())?;
            }
            Ok(plugin::load_official_osu_benchmark(
                &version, &snapshot, &dir,
            )?)
        }
    }

    fn start(&self, instance: u64, package: &LoadedPlugin, base: Option<usize>) -> Result<()> {
        let Execution::Process(process) = package.execution() else {
            return Err(fail("benchmark package is not a process plugin"));
        };
        let digests = Digests::of(process).map_err(|error| fail(error.to_string()))?;
        self.approvals
            .approve(digests.record(&package.manifest().id, Provenance::of(process)))?;
        let data = self.data(instance);
        std::fs::create_dir(&data)?;
        if let Some(base) = base {
            write_file(&data.join(wire::FIXTURE), base.to_string().as_bytes())?;
        }
        let pool = package
            .manifest()
            .concurrent_dispatch
            .then(|| self.pool.clone());
        self.host
            .create_instance(PluginInstanceId::new(instance), package, data, pool)?;
        self.healthy(instance)
    }

    fn healthy(&self, id: u64) -> Result<()> {
        match self.host.process_status(PluginInstanceId::new(id)) {
            Some(status) if status.phase == ProcessPhase::Running && status.restarts == 0 => Ok(()),
            status => Err(fail(format!(
                "process instance {id:?} did not stay running: {status:?}"
            ))),
        }
    }

    fn subscribe(&self, session: String) -> Result<vosu_surface::state::SurfaceSubscription> {
        let upstream = plugin::load_bundled(
            &vosu::BUNDLE.version(),
            &self.host.validation_snapshot(),
            &[plugin::BundledPlugin {
                manifest: vosu_adapter_upstream::MANIFEST,
                settings_schema: None,
                namespace_schemas: &[],
                action_schemas: &[],
                vtable: vosu_adapter_upstream::vosu_plugin_vtable,
            }],
        )?;
        self.host.create_instance(
            PluginInstanceId::new(3),
            &upstream[0],
            self.root.path().join("upstream"),
            None,
        )?;
        Ok(self.host.surface_host().subscribe(
            "vosu.adapter.upstream",
            SessionTarget::Id(session),
            vec![Selector {
                ns: wire::OSU.into(),
                pointer: "/beatmap/time/live".into(),
            }],
        )?)
    }

    fn data(&self, id: u64) -> PathBuf {
        self.root.path().join(format!("data-{id:?}"))
    }

    fn command(&self, id: u64, action: &str, session: Option<u64>, args: CborValue) -> Result<u64> {
        let invocation = self.next.get();
        self.next.set(invocation + 1);
        let receipt = self.host.dispatch_to_instance(
            PluginInstanceId::new(id),
            &Message::ActionInvoke(ActionInvoke {
                invocation,
                action: action.into(),
                session,
                caller: Caller {
                    kind: CallerKind::Tool,
                    id: "live-reader-bench".into(),
                },
                args,
            }),
            0,
        )?;
        let status = receipt.recv_timeout(BOUND)?;
        if status != vosu_status::VOSU_OK {
            return Err(fail(format!(
                "{action}: process transport refused frame: {status:?}"
            )));
        }
        Ok(invocation)
    }

    fn snapshot(&self, id: u64) -> Result<wire::Snapshot> {
        self.observe(id, wire::SNAPSHOT)
    }

    fn observe(&self, id: u64, action: &str) -> Result<wire::Snapshot> {
        let n = self.command(id, action, None, CborValue::Null)?;
        let path = self.data(id).join(wire::receipt(n));
        let bytes = until("child snapshot", || match std::fs::read(&path) {
            Ok(bytes) => Ok(Some(bytes)),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                self.healthy(id)?;
                Ok(None)
            }
            Err(error) => Err(error.into()),
        })?;
        std::fs::remove_file(path)?;
        let snapshot: wire::Snapshot = serde_json::from_slice(&bytes)?;
        if snapshot.pid == std::process::id() {
            return Err(fail("callback unexpectedly ran in core"));
        }
        Ok(snapshot)
    }
}

impl Drop for Host {
    fn drop(&mut self) {
        let _enter = self.runtime.enter();
        self.host.destroy_all(Duration::from_secs(5));
    }
}

fn write_file(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    file.write_all(bytes)?;
    Ok(())
}

fn until<T>(what: &str, mut ready: impl FnMut() -> Result<Option<T>>) -> Result<T> {
    let deadline = Instant::now() + BOUND;
    loop {
        if let Some(value) = ready()? {
            return Ok(value);
        }
        if Instant::now() >= deadline {
            return Err(fail(format!("timed out awaiting {what}")));
        }
        thread::park_timeout(Duration::from_millis(5));
    }
}

#[cfg(target_os = "linux")]
fn record(pid: u32) -> Result<ProcessRecord> {
    let metadata = vosu_sys::process::process_metadata_matching(pid, |_| true)?
        .ok_or_else(|| fail(format!("PID {pid} not found")))?;
    Ok(plugin::linux_common::map_metadata(metadata))
}

#[cfg(windows)]
fn record(pid: u32) -> Result<ProcessRecord> {
    use vosu_core::plugin::ProcessSource;
    plugin::WindowsProcessSource::default()
        .inspect(pid)?
        .ok_or_else(|| fail(format!("PID {pid} not found")))
}

fn affinity(snapshot: &wire::Snapshot) -> Result<Vec<usize>> {
    let child = os::pid_affinity(snapshot.pid);
    let core = os::own_affinity();
    if child.is_empty() || child != core {
        return Err(fail(format!(
            "child {} affinity {child:?} differs from core {core:?}",
            snapshot.pid
        )));
    }
    Ok(child)
}

pub(super) fn run(args: &Args) -> Result<()> {
    let profile = args.process_profile;
    // The owned helper never touches another process or uses a hooked memory backend.
    let fixture = args
        .process_fixture
        .then(fixture::Fixture::start)
        .transpose()?;
    let contender = (profile == Profile::Contention)
        .then(fixture::Fixture::start)
        .transpose()?;
    let rig = Host::new()?;
    let _enter = rig.runtime.enter();
    rig.start(
        OSU,
        &rig.package(None)?,
        fixture.as_ref().map(|target| target.base),
    )?;
    let before = rig.snapshot(OSU)?;
    let cpus = affinity(&before)?;
    if profile == Profile::IdleHost {
        let start = Instant::now();
        thread::park_timeout(if args.cycles == 1 {
            Duration::from_millis(100)
        } else {
            Duration::from_secs(5)
        });
        let elapsed = start.elapsed();
        let after = rig.snapshot(OSU)?;
        rig.healthy(OSU)?;
        if before.pid != after.pid
            || after.lanes.iter().any(|lane| lane.ticks != 0)
            || !rig.host.diagnostic_sessions().is_empty()
        {
            return Err(fail("idle host did not remain the same unattached child"));
        }
        println!(
            "{}",
            serde_json::json!({"profile":format!("{PREFIX}/{}", profile.name()), "plugin_pid":after.pid,"affinity":cpus,"wall_ns":elapsed.as_nanos(),"plugin_process_cpu_ns":after.cpu_ns-before.cpu_ns,"capture_ticks":0,"receipts":2})
        );
        return Ok(());
    }
    let pid = fixture.as_ref().map_or(args.pid, |target| target.pid());
    let mut target = record(pid)?;
    if fixture.is_some() {
        // Only the target identity facts and stable signature anchors are synthetic;
        // the graph lives in a separate real native process.
        target.name = "osu!.exe".into();
        target.bitness = vosu_plugin_api::Bitness::X86;
    }
    rig.host.process_appeared(target);
    let client = if args.client == vosu_osu_model::enums::ClientType::Stable {
        "stable"
    } else {
        "lazer"
    };
    let session = until("osu! session publication", || {
        rig.healthy(OSU)?;
        Ok(rig
            .host
            .diagnostic_sessions()
            .into_iter()
            .find(|s| s.pid == pid && s.state == "reading"))
    })?;
    if session.client != client {
        return Err(fail(format!(
            "PID {pid} classified as {}, requested {client}",
            session.client
        )));
    }
    let _subscription = (profile != Profile::NoClient)
        .then(|| rig.subscribe(session.session))
        .transpose()?;
    let contender_begin = if let Some(target) = &contender {
        let record = record(target.pid())?;
        rig.start(
            CONTENDER,
            &rig.package(Some(&record.name))?,
            Some(target.base),
        )?;
        rig.host.process_appeared(record);
        let initial = until("contender broker reads", || {
            let receipt = rig.snapshot(CONTENDER)?;
            Ok((receipt.contender_reads > 0).then_some(receipt))
        })?;
        affinity(&initial)?;
        Some(initial)
    } else {
        None
    };
    let warm = until("ready production captures and requested demand", || {
        let receipt = rig.snapshot(OSU)?;
        let ready = receipt
            .lanes
            .iter()
            .all(|lane| lane.ticks >= args.warmup as u64 && lane.ready > 0);
        Ok(
            (ready && (receipt.precise_demand == (profile != Profile::NoClient)))
                .then_some(receipt),
        )
    })?;
    let held = matches!(profile, Profile::DelayedDispatch | Profile::FullStream);
    if held {
        rig.command(OSU, wire::HOLD, Some(session.handle), CborValue::Null)?;
        until("scoped dispatch hold", || {
            Ok(rig.data(OSU).join(wire::HELD).is_file().then_some(()))
        })?;
        if profile == Profile::FullStream {
            let args = CborValue::Bytes(vec![0xa5; wire::FLOOD_BYTES]);
            for _ in 0..wire::FLOOD_FRAMES {
                rig.command(OSU, wire::FLOOD, Some(session.handle), args.clone())?;
            }
        }
    }
    let start = rig.observe(OSU, wire::BEGIN)?;
    let start_wall = Instant::now();
    let end = until("measured capture ticks", || {
        thread::park_timeout(Duration::from_millis(50));
        let receipt = rig.snapshot(OSU)?;
        Ok((0..2)
            .all(|i| receipt.lanes[i].ticks >= start.lanes[i].ticks + args.cycles as u64)
            .then_some(receipt))
    })?;
    let elapsed = start_wall.elapsed();
    let mut queued_bytes_lower_bound = 0;
    if held {
        rig.command(OSU, wire::RELEASE, None, CborValue::Null)?;
        if !start.held || !end.held || end.flood_frames != 0 {
            return Err(fail(
                "scoped dispatch did not hold throughout the capture window",
            ));
        }
        if profile == Profile::FullStream {
            let drained = until("full delivery stream drain", || {
                let receipt = rig.snapshot(OSU)?;
                Ok((receipt.flood_frames == wire::FLOOD_FRAMES as u64).then_some(receipt))
            })?;
            if drained.flood_bytes != (wire::FLOOD_FRAMES * wire::FLOOD_BYTES) as u64 {
                return Err(fail("full delivery stream lost bytes"));
            }
            // No callback finished while held. At most a ring and one frame
            // being decoded can be outside the child's delivery queue.
            queued_bytes_lower_bound = drained.flood_bytes
                - plugin::process::ring::CAPACITY as u64
                - plugin::process::frame::MAX_FRAME_BYTES as u64;
            if queued_bytes_lower_bound * 10 < DELIVERY_BYTES * 9 {
                return Err(fail("delivery stream did not reach at least 90% capacity"));
            }
        }
    }
    let contender_reads = if let Some(before) = contender_begin {
        let after = rig.snapshot(CONTENDER)?;
        rig.healthy(CONTENDER)?;
        if before.pid == end.pid
            || after.pid != before.pid
            || after.contender_reads <= before.contender_reads
        {
            return Err(fail(
                "distinct contender did not complete real brokered reads",
            ));
        }
        after.contender_reads - before.contender_reads
    } else {
        0
    };
    rig.healthy(OSU)?;
    if start.pid != warm.pid || end.pid != start.pid || start.precise_demand != end.precise_demand {
        return Err(fail("the measured child or sampling demand changed"));
    }
    if end.wire_read_submissions <= start.wire_read_submissions
        || end.wire_read_results <= start.wire_read_results
        || end.control_frames <= start.control_frames
    {
        return Err(fail(
            "the production rings carried no observed broker traffic",
        ));
    }
    let mut lanes = Vec::with_capacity(2);
    for (i, name) in LANES.into_iter().enumerate() {
        let lane = end.lanes[i].since(start.lanes[i]);
        if lane.ready == 0
            || lane.counts.submissions == 0
            || lane.counts.reads == 0
            || lane.wall_ns == 0
            || (lane.cpu_ns == 0 && lane.cpu_cycles == 0)
        {
            return Err(fail(format!(
                "{client}/{name} had no ready brokered captures: {lane:?}"
            )));
        }
        lanes.push(serde_json::json!({"case":format!("{PREFIX}/{client}/{name}"),"capture_thread_cpu_ns_per_tick":lane.cpu_ns as f64 / lane.ticks as f64,"capture_thread_cpu_cycles_per_tick":cfg!(windows).then(|| lane.cpu_cycles as f64 / lane.ticks as f64),"wall_ns_per_tick":lane.wall_ns as f64 / lane.ticks as f64,"total":lane}));
    }
    println!(
        "{}",
        serde_json::json!({"profile":format!("{PREFIX}/{}", profile.name()),"fixture":args.process_fixture,"client":client,"client_version":session.version,"pid":pid,"plugin_pid":end.pid,"affinity":cpus,"wall_ns":elapsed.as_nanos(),"plugin_process_cpu_ns":end.cpu_ns-start.cpu_ns,"started_with_precise_demand":start.precise_demand,"scoped_dispatch_held":held,"queued_payload_bytes_lower_bound":queued_bytes_lower_bound,"contender_reads":contender_reads,"wire_frames":end.wire_frames-start.wire_frames,"control_frames":end.control_frames-start.control_frames,"wire_read_submissions":end.wire_read_submissions-start.wire_read_submissions,"wire_read_results":end.wire_read_results-start.wire_read_results,"lanes":lanes})
    );
    Ok(())
}
