//! Loads the real osu! producer and records its production capture threads.
#[path = "../brokered_reads_plugin/census.rs"]
#[expect(
    dead_code,
    reason = "the P1 bench also uses the one-shot census entry point"
)]
mod census;
mod process_cpu;
mod process_protocol;
#[path = "../../benches/support/stable_fixture.rs"]
#[expect(
    dead_code,
    reason = "the shared fixture's capture assertions live in the plan benches"
)]
mod stable_fixture;

use std::{
    io::Write,
    path::PathBuf,
    sync::{
        Arc, Condvar, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    thread::{self, JoinHandle},
    time::Duration,
};

use process_protocol as wire;
use vosu_osu::{bench_capture, memory::PlanProcessMemory, plugin::OsuPlugin};
use vosu_plugin_api::{
    Bitness, CborValue, ConcurrentPlugin, HostServices, Init, Message, PluginError,
    VtableHostServices,
    memory::{ProcessBitness, ProcessMemory},
    vosu_status,
};

struct Benchmark {
    osu: Option<OsuPlugin>,
    host: VtableHostServices,
    dir: PathBuf,
    hold: Mutex<bool>,
    released: Condvar,
    precise_demand: AtomicBool,
    flood_frames: AtomicU64,
    flood_bytes: AtomicU64,
    contender_reads: Arc<AtomicU64>,
    stop: Arc<AtomicBool>,
    worker: Mutex<Option<JoinHandle<()>>>,
    transport: Mutex<Option<(census::Rings, census::Cursors, census::Census)>>,
}

fn failed(error: impl std::fmt::Display) -> PluginError {
    eprintln!("live reader cdylib: {error}");
    PluginError(vosu_status::VOSU_ERR_BACKEND)
}

impl Benchmark {
    fn publish(&self, name: &str, bytes: &[u8]) -> Result<(), PluginError> {
        let mut tmp = tempfile::NamedTempFile::new_in(&self.dir).map_err(failed)?;
        tmp.write_all(bytes).map_err(failed)?;
        tmp.persist(self.dir.join(name)).map_err(failed)?;
        Ok(())
    }

    fn snapshot(&self, invocation: u64, begin: bool) -> Result<(), PluginError> {
        let mut transport = self.transport.lock().unwrap();
        if begin {
            let rings = census::Rings::map().map_err(failed)?;
            let start = rings.cursors();
            *transport = Some((rings, start, census::Census::default()));
        }
        let traffic = match transport.as_mut() {
            Some((rings, cursor, total)) => {
                let next = rings.cursors();
                total.add(rings.between(*cursor, next).map_err(failed)?);
                *cursor = next;
                *total
            }
            None => census::Census::default(),
        };
        drop(transport);
        let snapshot = wire::Snapshot {
            pid: std::process::id(),
            cpu_ns: process_cpu::process_cpu_ns(),
            lanes: bench_capture::snapshot(),
            precise_demand: self.precise_demand.load(Ordering::Relaxed),
            held: *self.hold.lock().unwrap(),
            flood_frames: self.flood_frames.load(Ordering::Acquire),
            flood_bytes: self.flood_bytes.load(Ordering::Relaxed),
            contender_reads: self.contender_reads.load(Ordering::Relaxed),
            wire_frames: traffic.frames(),
            control_frames: traffic.private,
            wire_read_submissions: traffic.submits,
            wire_read_results: traffic.results,
        };
        self.publish(
            &wire::receipt(invocation),
            &serde_json::to_vec(&snapshot).map_err(failed)?,
        )
    }

    fn contend(&self, session: vosu_plugin_api::SessionAttached) -> Result<(), PluginError> {
        let status =
            self.host
                .identify_session(session.handle, "benchmark", "reader", "1", "", false);
        if status != vosu_status::VOSU_OK {
            return Err(status.into());
        }
        let base = fixture_base(&self.dir)?
            .ok_or_else(|| failed("contender needs a native target address"))?;
        let bitness = match session.bitness {
            Bitness::X86 => ProcessBitness::X86,
            Bitness::X64 => ProcessBitness::X64,
        };
        let memory = PlanProcessMemory::new(self.host, session.handle, session.pid, bitness);
        let reads = self.contender_reads.clone();
        let stop = self.stop.clone();
        let worker = thread::Builder::new()
            .name("vosu-bench-contender".into())
            .spawn(move || {
                let mut buf = [0; 64];
                while !stop.load(Ordering::Relaxed) {
                    if let Err(error) = memory.read_exact(base, &mut buf) {
                        if !stop.load(Ordering::Relaxed) {
                            eprintln!("contender stopped: {error}");
                        }
                        break;
                    }
                    reads.fetch_add(1, Ordering::Relaxed);
                }
            })
            .map_err(failed)?;
        *self.worker.lock().unwrap() = Some(worker);
        Ok(())
    }
}

fn fixture_base(dir: &std::path::Path) -> Result<Option<usize>, PluginError> {
    match std::fs::read_to_string(dir.join(wire::FIXTURE)) {
        Ok(text) => Ok(Some(text.trim().parse().map_err(failed)?)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(failed(error)),
    }
}

impl ConcurrentPlugin for Benchmark {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let dir = init.data_dir.clone();
        let osu = match init.plugin_id.as_str() {
            wire::OSU => {
                bench_capture::install(
                    process_cpu::thread_cpu,
                    fixture_base(&dir)?.map(stable_fixture::patterns_at),
                );
                Some(OsuPlugin::create(host, init)?)
            }
            wire::CONTENDER => None,
            id => return Err(failed(format_args!("unrecognized benchmark identity {id}"))),
        };
        Ok(Self {
            osu,
            host,
            dir,
            hold: Mutex::default(),
            released: Condvar::new(),
            precise_demand: AtomicBool::default(),
            flood_frames: AtomicU64::default(),
            flood_bytes: AtomicU64::default(),
            contender_reads: Arc::default(),
            stop: Arc::default(),
            worker: Mutex::default(),
            transport: Mutex::default(),
        })
    }

    fn on_message(&self, message: Message, correlation: u64) -> Result<(), PluginError> {
        if let Message::ActionInvoke(action) = &message {
            match action.action.as_str() {
                wire::BEGIN | wire::SNAPSHOT => {
                    return self.snapshot(action.invocation, action.action == wire::BEGIN);
                }
                wire::HOLD => {
                    let mut hold = self.hold.lock().unwrap();
                    *hold = true;
                    self.publish(wire::HELD, b"held")?;
                    let (mut hold, _) = self
                        .released
                        .wait_timeout_while(hold, Duration::from_secs(45), |state| *state)
                        .unwrap();
                    *hold = false;
                    return Ok(());
                }
                wire::RELEASE => {
                    *self.hold.lock().unwrap() = false;
                    self.released.notify_one();
                    return Ok(());
                }
                wire::FLOOD => {
                    let CborValue::Bytes(bytes) = &action.args else {
                        return Err(failed("expected byte payload"));
                    };
                    if bytes.len() != wire::FLOOD_BYTES
                        || self.flood_frames.load(Ordering::Relaxed) >= wire::FLOOD_FRAMES as u64
                    {
                        return Err(failed("unexpected flood frame length or count"));
                    }
                    self.flood_bytes
                        .fetch_add(bytes.len() as u64, Ordering::Relaxed);
                    self.flood_frames.fetch_add(1, Ordering::Release);
                    return Ok(());
                }
                _ => {}
            }
        }
        if let Message::DemandChanged(demand) = &message {
            self.precise_demand.store(demand.precise, Ordering::Relaxed);
        }
        if let Some(osu) = &self.osu {
            osu.on_message(message, correlation)
        } else {
            match message {
                Message::SessionAttached(session) => self.contend(session),
                Message::SessionDetached(_) => {
                    self.stop.store(true, Ordering::Relaxed);
                    Ok(())
                }
                _ => Ok(()),
            }
        }
    }
}

impl Drop for Benchmark {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.get_mut().unwrap().take() {
            let _ = worker.join();
        }
    }
}

vosu_plugin_internal::export_plugin!(concurrent Benchmark);
