//! The M5 production-bench plugin: a `runtime = "process"` `cdylib` that
//! `benches/support/production` stages and core starts in a real
//! `vosu plugin-host` child, for P1's `production_sync`/`production_async`
//! variants (`benches/brokered_reads_bench.rs`, module [`p1`]) and X1's
//! `plan_memory/*_process` cases (`benches/plan_memory_bench.rs`, module
//! [`x1`]).
//!
//! Core drives it with the commands of
//! `benches/support/production/protocol.rs`. The dispatch thread only
//! records sessions, hands asynchronous plan results to the waiting thread
//! and forwards doorbells; a worker thread of this plugin's own runs every
//! command, since a `SYNC` plan may never be submitted from the dispatch
//! thread. A failed check is an `error` reply; a panic aborts the plugin
//! host (release builds abort on panic), which core reports as the
//! instance's loss. Build it before the benches, in the same profile:
//! `cargo build --locked --release --example brokered_reads_plugin`.
//!
//! The allocator below counts this library's own allocations only; the
//! plugin host's relay is the `vosu` executable's code.

#![expect(
    clippy::disallowed_methods,
    reason = "bench fixture plugin: publishes its replies as files; never shipped"
)]

#[path = "../benches/support/brokered/abi.rs"]
mod abi;
#[path = "../benches/support/plan_process_cases.rs"]
mod cases;
#[path = "brokered_reads_plugin/census.rs"]
mod census;
#[path = "../benches/support/brokered/os.rs"]
#[expect(
    dead_code,
    reason = "shared with the bench runner; the plugin measures only itself"
)]
mod os;
#[path = "brokered_reads_plugin/p1.rs"]
mod p1;
#[path = "../benches/support/brokered/plans.rs"]
#[expect(
    dead_code,
    reason = "shared with the bench runner; the plugin converts the plans to ABI stages"
)]
mod plans;
#[path = "../benches/support/production/protocol.rs"]
#[expect(dead_code, reason = "shared with core, which also reads numbers")]
mod protocol;
#[path = "../benches/support/region_walk_scans.rs"]
mod region_walk_scans;
#[path = "../benches/support/stable_fixture.rs"]
#[expect(
    dead_code,
    reason = "shared with plan_memory_bench; the plugin needs only its patterns and times"
)]
mod stable_fixture;
#[path = "brokered_reads_plugin/x1.rs"]
mod x1;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, SyncSender, sync_channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use vosu_plugin_api::memory::ProcessBitness;
use vosu_plugin_api::{
    Bitness, Init, Message, Plugin, PluginError, VtableHostServices, vosu_status,
};

use census::Rings;
use protocol::Fields;

#[global_allocator]
static ALLOCATOR: os::CountingAllocator = os::CountingAllocator;

/// What the dispatch thread shares with the worker.
#[derive(Default)]
struct Shared {
    /// Attached sessions by pid: `(handle, bitness)`.
    sessions: Mutex<HashMap<u32, (u64, ProcessBitness)>>,
    results: p1::Results,
}

struct BenchPlugin {
    shared: Arc<Shared>,
    dir: PathBuf,
    doorbells: Option<SyncSender<u64>>,
    worker: Option<JoinHandle<()>>,
}

impl Plugin for BenchPlugin {
    fn create(host: VtableHostServices, init: Init) -> Result<Self, PluginError> {
        let shared = Arc::new(Shared::default());
        let (doorbells, commands) = sync_channel(64);
        let worker = {
            let shared = Arc::clone(&shared);
            let dir = init.data_dir.clone();
            std::thread::Builder::new()
                .name("bench-plugin-worker".to_owned())
                .spawn(move || Worker::new(host, shared, dir).run(&commands))
                .map_err(|_| PluginError(vosu_status::VOSU_ERR_BACKEND))?
        };
        Ok(Self {
            shared,
            dir: init.data_dir,
            doorbells: Some(doorbells),
            worker: Some(worker),
        })
    }

    fn on_message(&mut self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(attached) => {
                let bitness = match attached.bitness {
                    Bitness::X86 => ProcessBitness::X86,
                    Bitness::X64 => ProcessBitness::X64,
                };
                self.shared
                    .sessions
                    .lock()
                    .unwrap()
                    .insert(attached.pid, (attached.handle, bitness));
                publish(
                    &protocol::attached_path(&self.dir, attached.pid),
                    &Fields::new().with("handle", attached.handle),
                );
            }
            Message::SessionDetached(detached) => {
                self.shared
                    .sessions
                    .lock()
                    .unwrap()
                    .retain(|_, (handle, _)| *handle != detached.handle);
            }
            Message::ReadPlanResult(result) => self.shared.results.deliver(result),
            Message::DemandChanged(demand)
                if demand.handle & protocol::DOORBELL_MASK == protocol::DOORBELL =>
            {
                if let Some(doorbells) = &self.doorbells {
                    doorbells
                        .send(demand.handle & !protocol::DOORBELL_MASK)
                        .map_err(|_| PluginError(vosu_status::VOSU_ERR_BACKEND))?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

impl Drop for BenchPlugin {
    fn drop(&mut self) {
        drop(self.doorbells.take());
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

/// Publishes `fields` at `path` atomically: written beside it, then
/// renamed, so core never reads a partial reply.
fn publish(path: &Path, fields: &Fields) {
    let partial = path.with_extension("partial");
    std::fs::write(&partial, fields.render()).expect("write a reply");
    std::fs::rename(&partial, path).expect("publish a reply");
}

/// The plugin's own thread: runs every command in doorbell order.
struct Worker {
    host: VtableHostServices,
    shared: Arc<Shared>,
    dir: PathBuf,
    /// This plugin host's rings, mapped once.
    rings: Result<Rings, String>,
    p1: p1::P1,
    x1: x1::X1,
}

impl Worker {
    fn new(host: VtableHostServices, shared: Arc<Shared>, dir: PathBuf) -> Self {
        Self {
            host,
            shared,
            dir,
            rings: Rings::map(),
            p1: p1::P1::default(),
            x1: x1::X1::default(),
        }
    }

    fn run(mut self, commands: &Receiver<u64>) {
        while let Ok(n) = commands.recv() {
            let reply = std::fs::read_to_string(protocol::command_path(&self.dir, n))
                .map_err(|error| format!("reading command {n}: {error}"))
                .and_then(|text| self.execute(&Fields::parse(&text)))
                .unwrap_or_else(|error| Fields::new().with("error", error));
            publish(&protocol::reply_path(&self.dir, n), &reply);
        }
    }

    fn session(shared: &Shared, command: &Fields) -> Result<(u32, u64, ProcessBitness), String> {
        let pid = command.number("pid") as u32;
        let (handle, bitness) = *shared
            .sessions
            .lock()
            .unwrap()
            .get(&pid)
            .ok_or_else(|| format!("no session attached for pid {pid}"))?;
        Ok((pid, handle, bitness))
    }

    fn execute(&mut self, command: &Fields) -> Result<Fields, String> {
        let session = |shared: &Shared| Self::session(shared, command);
        let Self {
            host,
            shared,
            rings,
            p1,
            x1,
            ..
        } = self;
        let rings = || {
            rings
                .as_ref()
                .map_err(|error| format!("ring census: {error}"))
        };
        match command.text("command") {
            "info" => {
                let usage = os::Usage::end();
                Ok(Fields::new()
                    .with("pid", std::process::id())
                    .with("affinity", os::cpu_list(&os::own_affinity()))
                    .with("cpu_ns", usage.cpu_ns)
                    .with("cpu_cycles", usage.cpu_cycles))
            }
            "p1" => {
                let (_, handle, _) = session(shared)?;
                p1.samples(host, &shared.results, rings()?, command, handle)
            }
            "p1-batch" => p1.batch(host, &shared.results, rings()?, command),
            "p1-report" => p1.report(command),
            "x1-setup" => {
                let (pid, handle, bitness) = session(shared)?;
                x1.setup(*host, command, handle, pid, bitness)
            }
            "x1-census" => x1.census(rings()?, command),
            "x1-batch" => x1.batch(command),
            other => Err(format!("unknown command {other}")),
        }
    }
}

vosu_plugin_api::export_plugin!(BenchPlugin);
