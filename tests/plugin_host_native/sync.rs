//! M5 X0: `VOSU_READ_PLAN_SYNC` re-entrancy and deadline evidence
//! (`docs/v2/plugin-abi.md` §4, §6.3 "Opt-in synchronous reads", §7.6
//! "Core's `SYNC` budget"; M5 plan §7 "SYNC re-entrancy/deadline").
//!
//! Each case runs the same C fixture ([`fixture`]) in a real
//! `vosu plugin-host` child and in the in-process runtime, and requires the
//! child to answer exactly as in-process does:
//!
//! - the result is dispatched on the submitting thread itself (equal OS
//!   thread ids, not the dispatch thread), aligned, before `send` returns;
//! - a SYNC submission on the dispatch thread is refused, and so is one
//!   nested inside a SYNC result's own dispatch, which is a dispatch;
//! - ordinary dispatch stays blocked while a separate worker's SYNC read
//!   completes;
//! - a two-stage plan whose first read is held past the 2-second budget
//!   returns `VOSU_ERR_TIMED_OUT` (in-process when the held read returns,
//!   since it checks the budget between stages; in a plugin host at the
//!   budget itself, from core's timer), its late result never reaches any
//!   dispatch, and the same thread's next SYNC read gets its own result.

#[path = "sync/fixture.rs"]
pub(super) mod fixture;

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fixture::Command;
use vosu_plugin_api::memory::{
    MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest,
};
use vosu_plugin_api::{DemandChanged, Message, vosu_status};

use super::support::host::{Host, wait_until};

pub(super) const BOUND: Duration = Duration::from_secs(20);
/// The bytes at `0x1000`.
pub(super) const MARKER: u64 = 0x5E1F_0002_C0FF_EE01;
/// The bytes at `0x2000`, once the gate opens.
const LATE: u64 = 0x1A7E_0002_DEAD_0001;
/// The `SYNC` budget both runtimes enforce (`CREATE`'s `sync_budget_ms`,
/// the in-process read-plan deadline).
const BUDGET: Duration = Duration::from_secs(2);

/// How long a read of `0x2000` is held: past the budget, so the in-process
/// runtime's between-stage check and the plugin host's timer both expire.
const HELD: Duration = Duration::from_millis(2_300);

/// Session memory: `0x1000` answers at once; the first read of `0x2000`
/// is held for [`HELD`], and every later one answers at once.
#[derive(Default)]
pub(super) struct Memory {
    opened: Mutex<bool>,
    /// Reads of `0x2000` currently inside `read_exact`.
    inside: AtomicUsize,
    /// Reads of `0x2000` that returned.
    finished: AtomicUsize,
}

impl ProcessMemory for Memory {
    #[expect(
        clippy::disallowed_methods,
        reason = "a native read that blocks inside the target for a fixed time is the case under test"
    )]
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let value = match address {
            0x1000 => MARKER,
            0x2000 => {
                self.inside.fetch_add(1, Ordering::SeqCst);
                let first = !std::mem::replace(&mut *self.opened.lock().unwrap(), true);
                if first {
                    std::thread::sleep(HELD);
                }
                self.inside.fetch_sub(1, Ordering::SeqCst);
                self.finished.fetch_add(1, Ordering::SeqCst);
                LATE
            }
            _ => {
                return Err(MemoryError::Read {
                    address,
                    len: dst.len(),
                    message: "outside the X0 memory".to_owned(),
                });
            }
        };
        let bytes = value.to_le_bytes();
        let Some(source) = bytes.get(..dst.len()) else {
            return Err(MemoryError::Read {
                address,
                len: dst.len(),
                message: "longer than the X0 value".to_owned(),
            });
        };
        dst.copy_from_slice(source);
        Ok(())
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![
            MemoryRegion {
                address: 0x1000,
                size: 8,
            },
            MemoryRegion {
                address: 0x2000,
                size: 8,
            },
        ])
    }
}

/// One instance of the fixture under one runtime, attached to one target.
pub(super) struct Scenario {
    // Field order is drop order: the host and its child go first.
    pub(super) host: Host,
    pub(super) markers: PathBuf,
    pub(super) memory: Arc<Memory>,
    pub(super) runtime: &'static str,
    _root: tempfile::TempDir,
}

impl Scenario {
    /// Instance 1 of the fixture as `test.x0.<name>` under `runtime`
    /// (`process` or `in-process`), attached to target `pid`, whose PID
    /// must be unique in this test binary.
    pub(super) fn new(name: &str, runtime: &'static str, pid: u32) -> Self {
        let root = super::support::scratch();
        let markers = fixture::dir(root.path(), "markers");
        let library = fixture::build(&fixture::dir(root.path(), "build"), &markers);
        let package = root.path().join("package");
        let process = format!("x0-{name}-{}", std::process::id());
        vosu_core::plugin::test_support::write_development_package(
            &package,
            &format!("test.x0.{name}"),
            &process,
            runtime,
            fixture::library_name(),
            &library,
        )
        .unwrap();
        let host = Host::new(&root.path().join("host"));
        let loaded = host.load(&package);
        host.create(1, &loaded)
            .unwrap_or_else(|error| panic!("the {runtime} fixture did not start: {error}"));
        let memory = Arc::new(Memory::default());
        host.hook_memory(pid, memory.clone(), ProcessBitness::X64);
        host.offer_process(pid, &process, vosu_plugin_api::Bitness::X64);
        let scenario = Self {
            host,
            markers,
            memory,
            runtime,
            _root: root,
        };
        scenario.wait("attached");
        scenario
    }

    pub(super) fn command(&self, command: Command) {
        self.host
            .dispatch(
                1,
                &Message::DemandChanged(DemandChanged {
                    handle: command as u64,
                    regular: false,
                    precise: false,
                    selectors: Vec::new(),
                }),
            )
            .expect("core accepted the command");
    }

    /// The marker `probe` and the PID that wrote it.
    pub(super) fn wait(&self, probe: &str) -> (u32, String) {
        fixture::wait_marker(&self.markers, probe, BOUND)
            .unwrap_or_else(|| panic!("{}: no {probe} marker", self.runtime))
    }

    pub(super) fn open(&self, barrier: &str) {
        fixture::open_barrier(&self.markers, barrier);
    }

    /// Checks the PID that wrote a marker ran in the expected runtime.
    pub(super) fn assert_ran_in_runtime(&self, pid: u32) {
        if self.runtime == "process" {
            assert_ne!(pid, std::process::id(), "the plugin ran in core");
        } else {
            assert_eq!(pid, std::process::id());
        }
    }
}

fn status(text: &str, key: &str) -> u32 {
    fixture::field(text, key).parse().unwrap()
}

/// What one runtime's re-entrant SYNC run produced, thread ids reduced to
/// their relations.
#[derive(Debug, PartialEq, Eq)]
struct Reentrant {
    dispatch_refusal: u32,
    status: u32,
    got: String,
    aligned: String,
    value: String,
    plan: String,
    released_before: String,
    nested_status: u32,
    nested_got: String,
    nested_value: String,
    nested_plan: String,
}

fn reentrant(runtime: &'static str, pid: u32) -> Reentrant {
    let scenario = Scenario::new("reentrant", runtime, pid);
    scenario.command(Command::Hold);
    let (child, dispatch) = scenario.wait("dispatch");
    scenario.assert_ran_in_runtime(child);
    scenario.open("go");
    let (_, sync) = scenario.wait("sync");
    let (_, nested) = scenario.wait("nested");
    let dispatch_tid = fixture::field(&dispatch, "tid");
    let submit_tid = fixture::field(&sync, "submit_tid");
    assert_ne!(submit_tid, dispatch_tid, "{runtime}: submitted on dispatch");
    assert_eq!(
        fixture::field(&sync, "result_tid"),
        submit_tid,
        "{runtime}: the result was not dispatched on its submitting thread"
    );
    assert_eq!(
        fixture::field(&nested, "got"),
        "0",
        "{runtime}: a refused nested SYNC produced a result: {nested}"
    );
    assert!(
        fixture::marker(&scenario.markers, "unblocked").is_none(),
        "{runtime}: ordinary dispatch was not held during the reads"
    );
    scenario.open("release");
    scenario.wait("unblocked");
    assert!(
        fixture::marker(&scenario.markers, "stray").is_none(),
        "{runtime}: a SYNC result reached ordinary dispatch"
    );
    Reentrant {
        dispatch_refusal: status(&dispatch, "sync_status"),
        status: status(&sync, "status"),
        got: fixture::field(&sync, "got"),
        aligned: fixture::field(&sync, "aligned"),
        value: fixture::field(&sync, "value"),
        plan: fixture::field(&sync, "plan"),
        released_before: fixture::field(&sync, "released"),
        nested_status: status(&nested, "status"),
        nested_got: fixture::field(&nested, "got"),
        nested_value: fixture::field(&nested, "value"),
        nested_plan: fixture::field(&nested, "plan"),
    }
}

/// A worker's SYNC read completes on its own thread while ordinary
/// dispatch is held; a SYNC read on the dispatch thread, or nested in a
/// SYNC result's dispatch, is refused with no result: in a plugin host
/// exactly as in-process.
#[test]
fn sync_results_reach_their_submitter_while_dispatch_is_held_and_nesting_is_refused() {
    let expected = reentrant("in-process", 0x7FF0_5101);
    let actual = reentrant("process", 0x7FF0_5102);
    assert_eq!(
        expected,
        Reentrant {
            dispatch_refusal: vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32,
            status: vosu_status::VOSU_OK as u32,
            got: "1".into(),
            aligned: "1".into(),
            value: format!("{MARKER:016x}"),
            plan: "11".into(),
            released_before: "0".into(),
            nested_status: vosu_status::VOSU_ERR_INVALID_ARGUMENT as u32,
            nested_got: "0".into(),
            nested_value: format!("{:016x}", 0),
            nested_plan: "0".into(),
        }
    );
    assert_eq!(actual, expected);
}

/// What one runtime's deadline run produced.
#[derive(Debug, PartialEq, Eq)]
struct Deadline {
    status: u32,
    got: String,
    retry_status: u32,
    retry_got: String,
    retry_value: String,
    retry_plan: String,
    retry_same_thread: bool,
}

fn deadline(runtime: &'static str, pid: u32) -> Deadline {
    let scenario = Scenario::new("deadline", runtime, pid);
    scenario.command(Command::Late);
    let (child, late) = scenario.wait("late");
    scenario.assert_ran_in_runtime(child);
    let elapsed = Duration::from_millis(fixture::field(&late, "elapsed_ms").parse().unwrap());
    let held = scenario.memory.inside.load(Ordering::SeqCst);
    println!("RECORD sync {runtime} deadline after {elapsed:?}, read still held: {held}");
    assert!(
        elapsed >= BUDGET - Duration::from_millis(100) && elapsed < BUDGET * 2,
        "{runtime}: answered after {elapsed:?}, not at the 2 s budget"
    );
    if runtime == "process" {
        // Core's timer answered while the native read was still blocked.
        assert!(
            elapsed < HELD,
            "the plugin host's answer waited for the held read ({elapsed:?})"
        );
    }
    // Wait until core's held read returned, so a late result exists if
    // anything were to deliver it.
    assert!(
        wait_until(BOUND, || scenario.memory.finished.load(Ordering::SeqCst)
            >= 1
            && scenario.memory.inside.load(Ordering::SeqCst) == 0),
        "{runtime}: the held read never returned"
    );
    // The next SYNC read on the same thread gets its own result, never the
    // late one; the late one never reached any dispatch.
    scenario.open("retry");
    let (_, retry) = scenario.wait("retry");
    assert!(
        fixture::marker(&scenario.markers, "stray").is_none(),
        "{runtime}: the late result was dispatched"
    );
    if runtime == "process" {
        let status = scenario
            .host
            .host
            .process_status(vosu_core::plugin::PluginInstanceId::new(1))
            .unwrap();
        assert_eq!(
            (status.crash_events, status.restarts, status.last_failure),
            (0, 0, None),
            "a timed-out SYNC read cost the child its channel"
        );
    }
    Deadline {
        status: status(&late, "status"),
        got: fixture::field(&late, "got"),
        retry_status: status(&retry, "status"),
        retry_got: fixture::field(&retry, "got"),
        retry_value: fixture::field(&retry, "value"),
        retry_plan: fixture::field(&retry, "plan"),
        retry_same_thread: fixture::field(&retry, "submit_tid")
            == fixture::field(&retry, "result_tid"),
    }
}

/// A SYNC read held past the 2-second budget answers `VOSU_ERR_TIMED_OUT`
/// at the budget; its late result never arrives and never satisfies the
/// thread's next SYNC read, which gets its own bytes: in a plugin host
/// exactly as in-process.
#[test]
fn sync_a_read_past_the_budget_times_out_and_its_late_result_never_arrives() {
    let expected = deadline("in-process", 0x7FF0_5201);
    let actual = deadline("process", 0x7FF0_5202);
    assert_eq!(
        expected,
        Deadline {
            status: vosu_status::VOSU_ERR_TIMED_OUT as u32,
            got: "0".into(),
            retry_status: vosu_status::VOSU_OK as u32,
            retry_got: "1".into(),
            retry_value: format!("{MARKER:016x}"),
            retry_plan: "22".into(),
            retry_same_thread: true,
        }
    );
    assert_eq!(actual, expected);
}
