//! M5 T1: the relay between core and a real `vosu plugin-host` child —
//! `SYNC` reads routed straight to their submitting thread, local
//! admission before the ring, the non-blocking reader and the semantic
//! delivery queue (`docs/v2/plugin-abi.md` §4, §6.12, §7.2, §7.6), through
//! the real `vosu` executable, core's real launcher and a real
//! `PluginHost`.

#[path = "relay/fixture.rs"]
mod fixture;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use fixture::Command;
use vosu_core::plugin::process::launcher::ProcessHostLauncher;
use vosu_core::plugin::test_support::{self, write_development_package};
use vosu_core::plugin::{
    self, LoadedPlugin, PluginHost, PluginInstanceId, ProcessRecord, WorkerPool,
};
use vosu_plugin_api::memory::{
    MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest,
};
use vosu_plugin_api::{
    ActionInvoke, ActionResult, ActionStatus, Bitness, Caller, CallerKind, CborValue,
    DemandChanged, FocusChanged, Message, StateEntry, StateUpdate, vosu_opcode, vosu_status,
};

const BOUND: Duration = Duration::from_secs(20);
/// The value the injected session memory holds at `0x1000`.
const MARKER: u64 = 0x5E1F_0001_C0FF_EE00;

struct Console;

impl vosu_core::logging::ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _text: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _lines: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _text: &str) -> std::io::Result<()> {
        Ok(())
    }
}

struct EmptySettings;

impl vosu_core::plugin::settings::SettingsStorage for EmptySettings {
    fn load(
        &self,
        _plugin_id: &str,
    ) -> Result<
        serde_json::Map<String, serde_json::Value>,
        vosu_core::plugin::settings::SettingsError,
    > {
        Ok(serde_json::Map::new())
    }
    fn store(
        &self,
        _plugin_id: &str,
        _values: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), vosu_core::plugin::settings::SettingsError> {
        Ok(())
    }
}

/// Session memory whose reads wait until the test releases them.
#[derive(Default)]
struct Memory {
    held: Mutex<bool>,
    released: Condvar,
}

impl Memory {
    fn held() -> Arc<Self> {
        let memory = Arc::new(Self::default());
        *memory.held.lock().unwrap() = true;
        memory
    }

    fn release(&self) {
        *self.held.lock().unwrap() = false;
        self.released.notify_all();
    }
}

impl ProcessMemory for Memory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let held = self.held.lock().unwrap();
        drop(
            self.released
                .wait_timeout_while(held, BOUND, |held| *held)
                .unwrap(),
        );
        let bytes = MARKER.to_le_bytes();
        match address.checked_sub(0x1000) {
            Some(offset) if offset + dst.len() <= bytes.len() => {
                dst.copy_from_slice(&bytes[offset..offset + dst.len()]);
                Ok(())
            }
            _ => Err(MemoryError::Read {
                address,
                len: dst.len(),
                message: "outside the injected memory".to_owned(),
            }),
        }
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: 0x1000,
            size: 8,
        }])
    }
}

/// One test's package, markers, host and synthetic game process.
struct Scenario {
    root: tempfile::TempDir,
    markers: PathBuf,
    loaded: LoadedPlugin,
    host: Arc<PluginHost>,
    runtime: tokio::runtime::Runtime,
    process: String,
}

impl Scenario {
    /// A package of the T1 fixture under `runtime_kind`; a process package
    /// runs under the real `vosu` executable.
    fn new(name: &str, runtime_kind: &str, concurrent: bool) -> Self {
        let root = tempfile::tempdir().unwrap();
        let markers = fixture::dir(root.path(), "markers");
        let build = fixture::dir(root.path(), "build");
        let package = fixture::dir(root.path(), "package");
        let library = fixture::build(&build, &markers);
        let plugin_id = format!("test.t1.{name}");
        let process = format!("t1-{name}-{}", std::process::id());
        write_development_package(
            &package,
            &plugin_id,
            &process,
            runtime_kind,
            fixture::library_name(),
            &library,
        )
        .unwrap();
        if concurrent {
            let manifest = package.join("manifest.toml");
            let text = std::fs::read_to_string(&manifest).unwrap();
            let text = text.replacen(
                &format!("runtime='{runtime_kind}'\n"),
                &format!("runtime='{runtime_kind}'\nconcurrent_dispatch=true\n"),
                1,
            );
            std::fs::write(&manifest, text).unwrap();
        }
        let loaded = plugin::load_development(
            vosu::application_version(),
            &vosu_core::plugin::ValidationSnapshot::default(),
            &package,
            true,
        )
        .expect("the fixture package validates");
        assert_eq!(loaded.manifest().concurrent_dispatch, concurrent);
        let runtime = tokio::runtime::Runtime::new().unwrap();
        let host = {
            let _enter = runtime.enter();
            let logger = vosu_core::logging::Logger::new(
                fixture::dir(root.path(), "logs"),
                vosu_core::logging::LoggerOptions {
                    version: "0.1.0".to_owned(),
                    debug_log: false,
                    queue_capacity: 64,
                },
                Arc::new(Console),
            )
            .unwrap();
            let settings = Arc::new(vosu_core::plugin::settings::PluginSettingsService::new(
                Arc::new(EmptySettings),
            ));
            PluginHost::new(logger, settings, runtime.handle().clone())
        };
        if runtime_kind == "process" {
            let launcher = ProcessHostLauncher::with_executable(
                Path::new(env!("CARGO_BIN_EXE_vosu")),
                vosu::plugin_host::BUILD_IDENTITY,
            )
            .unwrap();
            host.install_process_launcher(Arc::new(launcher)).unwrap();
        }
        let scenario = Self {
            root,
            markers,
            loaded,
            host,
            runtime,
            process,
        };
        let _enter = scenario.runtime.enter();
        let pool = concurrent.then(|| WorkerPool::new(1, 64));
        scenario
            .host
            .create_instance(
                PluginInstanceId::new(1),
                &scenario.loaded,
                fixture::dir(scenario.root.path(), "data"),
                pool,
            )
            .expect("the instance starts");
        scenario
    }

    /// Offers the instance a session on synthetic `pid` whose memory is
    /// `memory`, and waits for the fixture to see it attached.
    fn attach(&self, pid: u32, memory: Arc<Memory>) {
        test_support::set_session_memory(pid, memory, ProcessBitness::X64);
        let _enter = self.runtime.enter();
        self.host.process_appeared(ProcessRecord {
            pid,
            name: self.process.clone(),
            path: format!("/games/{}", self.process),
            command_line: self.process.clone(),
            bitness: Bitness::X64,
            start_time: 1,
            argv: None,
        });
        self.marker("attached");
    }

    /// Dispatches `message` and waits until core wrote it to the child.
    fn dispatch(&self, message: &Message) {
        let _enter = self.runtime.enter();
        let written = self
            .host
            .dispatch_to_instance(PluginInstanceId::new(1), message, 0)
            .expect("core queued the message");
        assert_eq!(
            written.recv_timeout(BOUND).unwrap(),
            vosu_status::VOSU_OK,
            "core wrote the frame"
        );
    }

    fn command(&self, command: Command) {
        self.dispatch(&Message::DemandChanged(DemandChanged {
            handle: command as u64,
            regular: false,
            precise: false,
            selectors: Vec::new(),
        }));
    }

    fn marker(&self, probe: &str) -> (u32, String) {
        fixture::wait_marker(&self.markers, probe, BOUND)
            .unwrap_or_else(|| panic!("no {probe} marker within the bound"))
    }

    /// The fixture's dispatch log once it holds at least `count` entries.
    fn log(&self, pid: u32, count: usize) -> Vec<Message> {
        let deadline = std::time::Instant::now() + BOUND;
        loop {
            let log = fixture::log(&self.markers, pid);
            if log.len() >= count || std::time::Instant::now() >= deadline {
                return log
                    .into_iter()
                    .map(|(opcode, payload)| {
                        let opcode = vosu_opcode::try_from(opcode).unwrap();
                        Message::decode(opcode, 0, &payload).unwrap()
                    })
                    .collect();
            }
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    fn destroy(&self) {
        let _enter = self.runtime.enter();
        self.host.destroy_all(Duration::from_secs(5));
    }
}

fn status(text: &str, key: &str) -> u32 {
    fixture::field(text, key).parse().unwrap()
}

/// T1 counterexample: ordinary dispatch blocked while a different thread's
/// SYNC result is ready. The result still reaches its submitting thread
/// (and is dispatched there, aligned) while dispatch stays blocked; the
/// dispatch thread's own SYNC attempt is refused as in-process.
#[test]
fn relay_sync_read_completes_on_its_submitter_while_dispatch_is_blocked() {
    let scenario = Scenario::new("sync", "process", false);
    let pid = 0x7FFF_F101;
    let memory = Arc::new(Memory::default());
    scenario.attach(pid, memory);
    scenario.command(Command::Block);

    let (child, sync) = scenario.marker("sync");
    assert_ne!(child, std::process::id(), "the plugin ran in its host");
    assert_eq!(
        status(&sync, "status"),
        vosu_status::VOSU_OK as u32,
        "{sync}"
    );
    assert_eq!(fixture::field(&sync, "same_thread"), "1", "{sync}");
    assert_eq!(fixture::field(&sync, "value"), format!("{MARKER:016x}"));
    assert_eq!(
        fixture::field(&sync, "released"),
        "0",
        "the SYNC result arrived while dispatch was still blocked"
    );
    let (_, on_dispatch) = scenario.marker("dispatch_sync");
    assert_eq!(
        status(&on_dispatch, "status"),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32
    );
    fixture::open_barrier(&scenario.markers, "probe");
    scenario.marker("probed");
    fixture::open_barrier(&scenario.markers, "release");
    scenario.marker("unblocked");
    scenario.destroy();
    test_support::clear_session_memory(pid);
}

/// T1 counterexample: a 65th outstanding asynchronous plan is refused
/// with `VOSU_ERR_INVALID_ARGUMENT`; results free their slots.
#[test]
fn relay_the_sixty_fifth_async_plan_is_refused_and_results_free_slots() {
    let scenario = Scenario::new("plans", "process", false);
    let pid = 0x7FFF_F102;
    let memory = Memory::held();
    scenario.attach(pid, Arc::clone(&memory));
    scenario.command(Command::Plans);
    let (child, plans) = scenario.marker("plans");
    assert_eq!(status(&plans, "ok"), 64, "{plans}");
    assert_eq!(
        status(&plans, "extra"),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32,
        "{plans}"
    );
    memory.release();
    let results = scenario.log(child, 64 + 1);
    let delivered = results
        .iter()
        .filter(|message| matches!(message, Message::ReadPlanResult(_)))
        .count();
    assert_eq!(delivered, 64, "every admitted plan answered once");
    scenario.command(Command::More);
    let (_, more) = scenario.marker("more");
    assert_eq!(status(&more, "status"), vosu_status::VOSU_OK as u32);
    scenario.destroy();
    test_support::clear_session_memory(pid);
}

fn entry(pointer: &str, value: Option<u64>) -> StateEntry {
    StateEntry {
        ns: "acme.other".into(),
        pointer: pointer.into(),
        value: value.map(|value| CborValue::Integer(value.into())),
        error: None,
    }
}

fn update(values: Vec<StateEntry>, more: bool) -> Message {
    Message::StateUpdate(StateUpdate {
        session: 9,
        values,
        more,
    })
}

fn focus(focused: bool) -> Message {
    Message::FocusChanged(FocusChanged {
        handle: 7,
        focused,
        foreground: None,
    })
}

fn invoke(invocation: u64) -> Message {
    Message::ActionInvoke(ActionInvoke {
        invocation,
        action: "acme.fixture.run".into(),
        session: None,
        caller: Caller {
            kind: CallerKind::Tool,
            id: "tool".into(),
        },
        args: CborValue::Bytes(vec![invocation as u8; 1024]),
    })
}

fn result(invocation: u64) -> Message {
    Message::ActionResult(ActionResult {
        invocation,
        status: ActionStatus::Ok,
        value: None,
        code: None,
        message: None,
    })
}

/// T1 counterexamples, while the plugin's dispatch is blocked: a split
/// `STATE_UPDATE` is never merged before its last part; complete updates
/// merge per key without losing an earlier removal; latest-only focus
/// coalesces; a terminal result is neither lost nor queued behind a
/// backlog of ordinary work, none of which is dropped.
#[test]
fn relay_delivery_merges_coalesces_and_keeps_terminals_under_a_blocked_dispatch() {
    let scenario = Scenario::new("delivery", "process", false);
    scenario.command(Command::Block);
    scenario.marker("dispatch_sync");
    scenario.dispatch(&update(
        vec![entry("/a", Some(0)), entry("/gone", None)],
        false,
    ));
    scenario.dispatch(&update(vec![entry("/a", Some(1))], true));
    for focused in [false, true, false] {
        scenario.dispatch(&focus(focused));
    }
    for invocation in 0..150 {
        scenario.dispatch(&invoke(invocation));
    }
    scenario.dispatch(&result(77));
    scenario.dispatch(&update(vec![entry("/b", Some(2))], false));
    scenario.dispatch(&update(
        vec![entry("/gone", None), entry("/c", Some(3))],
        false,
    ));
    // An acknowledged round trip from the blocked plugin: its STATUS is
    // behind every frame above, so all of them reached the queue.
    fixture::open_barrier(&scenario.markers, "probe");
    scenario.marker("probed");
    fixture::open_barrier(&scenario.markers, "release");
    let (child, _) = scenario.marker("unblocked");

    let log = scenario.log(child, 1 + 1 + 1 + 150);
    assert_eq!(log.len(), 153, "nothing dropped, nothing duplicated");
    assert_eq!(
        log[0],
        result(77),
        "the terminal result overtook the backlog"
    );
    assert_eq!(
        log[1],
        update(
            vec![
                entry("/a", Some(1)),
                entry("/gone", None),
                entry("/b", Some(2)),
                entry("/c", Some(3)),
            ],
            false
        ),
        "one merged update: newest per key, the removal kept"
    );
    assert_eq!(log[2], focus(false), "latest focus only");
    for (index, message) in log[3..].iter().enumerate() {
        assert_eq!(*message, invoke(index as u64));
    }
    scenario.destroy();
}

/// T1 counterexample: requests core refuses return core's own status, the
/// same as in-process, and produce no later result.
#[test]
fn relay_refused_sends_match_in_process_statuses_and_produce_no_result() {
    let mut observed = Vec::new();
    for runtime_kind in ["in-process", "process"] {
        let name = format!("refused{}", runtime_kind.replace('-', ""));
        let scenario = Scenario::new(&name, runtime_kind, false);
        scenario.command(Command::Refused);
        let (pid, refused) = scenario.marker("refused");
        assert_eq!(pid == std::process::id(), runtime_kind == "in-process");
        let scan = status(&refused, "scan");
        let plan = status(&refused, "plan");
        assert_ne!(scan, vosu_status::VOSU_OK as u32, "{refused}");
        assert_ne!(plan, vosu_status::VOSU_OK as u32, "{refused}");
        // A later frame, after anything the refused requests could cause.
        scenario.dispatch(&focus(true));
        let log = scenario.log(pid, 1);
        assert_eq!(log, vec![focus(true)], "no result for a refused request");
        observed.push((scan, plan));
        scenario.destroy();
    }
    assert_eq!(observed[0], observed[1], "in-process vs process statuses");
}

/// `concurrent_dispatch` is honored in the plugin host: two deliveries run
/// at once on its worker pool.
#[test]
fn relay_concurrent_dispatch_runs_two_deliveries_at_once() {
    let scenario = Scenario::new("concurrent", "process", true);
    scenario.command(Command::OverlapA);
    scenario.command(Command::OverlapB);
    let (child, a) = scenario.marker("overlap-a");
    let (_, b) = scenario.marker("overlap-b");
    assert_ne!(child, std::process::id());
    assert_eq!((a.trim(), b.trim()), ("1", "1"), "both ran concurrently");
    scenario.destroy();
}

/// M5 I0: relayed action traffic into a caller child whose dispatch is
/// blocked. A callee child sends 500 progress messages for each of eight
/// calls and then answers each one. Once the blocked caller resumes,
/// every call's result arrives exactly once, ahead of the progress
/// backlog, and progress is coalesced to the latest message per call.
#[test]
fn relay_a_progress_flood_never_drops_or_delays_terminals_to_a_blocked_caller() {
    use super::support::services::{Services, Spec, op};
    let mut run = Services::empty();
    run.add(&Spec {
        name: "relay.callee",
        ..Spec::default()
    });
    run.add(&Spec {
        name: "relay.caller",
        requires: vec![
            "[[requires]]\nid = 'action:test.i0.relay.callee.work'\nversion = '^1.0'".to_owned(),
        ],
        ..Spec::default()
    });
    let (callee, caller) = (0, 1);
    let drive = run.drive(
        caller,
        op(
            "call_block",
            serde_json::json!({
                "action": "test.i0.relay.callee.work",
                "count": 8,
                "args": {"n": 0, "progress": 500},
            }),
        ),
    );
    let child = run.wait(caller, "blocked", |_| true).pid;
    let worked = &run.plugin(callee).fixture;
    assert!(
        super::support::host::wait_until(BOUND, || worked.receipts("worked").len() == 8),
        "the callee never answered all eight calls"
    );
    for answer in worked.receipts("worked") {
        // Core accepted every progress message and the result before the
        // callee wrote this receipt.
        assert_eq!((answer.get("refused"), answer.get("result")), ("0", "0"));
    }
    // A frame dispatched now is written behind every relayed frame core
    // queued for the caller before it; the probe's STATUS is behind that.
    let written = {
        let _enter = run.host.enter();
        run.host()
            .dispatch_to_instance(run.instance(caller), &focus(true), 0)
            .expect("core queued the fence")
    };
    assert_eq!(written.recv_timeout(BOUND).unwrap(), vosu_status::VOSU_OK);
    run.barrier(caller, "probe", child);
    run.wait(caller, "probed", |_| true);
    run.barrier(caller, "release", child);
    assert!(matches!(
        drive.outcome(),
        vosu_core::actions::TerminalOutcome::Succeeded(_)
    ));
    // The drive finished on the eighth result; the coalesced progress the
    // queue holds behind the terminals is dispatched after it. Every one
    // of the 4000 frames was queued while dispatch was blocked, so exactly
    // the latest one per call remains.
    let fixture = &run.plugin(caller).fixture;
    assert!(
        super::support::host::wait_until(BOUND, || fixture.receipts("relay_progress").len() >= 8),
        "the coalesced progress never arrived: {:?}",
        fixture.receipts("relay_progress")
    );
    let journal = run.plugin(caller).fixture.journal(child);
    let relayed: Vec<_> = journal
        .iter()
        .skip_while(|receipt| receipt.probe != "unblocked")
        .filter(|receipt| receipt.probe.starts_with("relay_"))
        .collect();
    let results: Vec<_> = relayed
        .iter()
        .filter(|receipt| receipt.probe == "relay_result")
        .collect();
    let mut calls: Vec<&str> = results
        .iter()
        .map(|result| {
            assert_eq!(result.get("status"), "Ok", "{result:?}");
            result.get("invocation")
        })
        .collect();
    calls.sort_unstable();
    calls.dedup();
    assert_eq!((results.len(), calls.len()), (8, 8), "one result per call");
    assert!(
        relayed[..8]
            .iter()
            .all(|receipt| receipt.probe == "relay_result"),
        "the terminals overtook the progress backlog: {relayed:?}"
    );
    let progress: Vec<_> = relayed[8..].iter().collect();
    let mut coalesced: Vec<&str> = progress
        .iter()
        .map(|receipt| {
            assert_eq!(
                receipt.get("fraction"),
                "1",
                "the latest progress: {receipt:?}"
            );
            receipt.get("invocation")
        })
        .collect();
    coalesced.sort_unstable();
    coalesced.dedup();
    assert_eq!(
        (progress.len(), coalesced),
        (8, calls),
        "exactly the latest progress of each call, of 4000 sent: {progress:?}"
    );
    println!(
        "RECORD 4000 progress messages reached the blocked caller as {}",
        progress.len()
    );
}
