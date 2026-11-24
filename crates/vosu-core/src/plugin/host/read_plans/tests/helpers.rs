//! Shared fixtures: a test [`read_plan_plugin::ReadPlanPlugin`] instance,
//! event/status helpers, and the gated/slow/dying `ProcessMemory` fakes
//! every topic module here uses. Split out of `tests.rs` so each stays
//! under the maintained-file line limit
//! (`docs/architecture/source-map.md`).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use vosu_plugin_api::{PlanOp, Stage, StageBase, vosu_status};
use vosu_plugin_testkit::read_plan::{
    self as read_plan_plugin, ReadPlanCommand, ReadPlanEvent, ReadPlanPluginConfig,
};
use vosu_sys::process::{MemoryError, MemoryRegion, ProcessMemory, ReadRequest};

use super::super::super::super::registry;
use super::super::*;

pub(super) const RECV_TIMEOUT: Duration = Duration::from_secs(5);

fn read_plan_manifest(plugin_id: &str, process_names: &[&str]) -> String {
    let names = process_names
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=[{names}]\n"
    )
}

/// Registers and creates a [`read_plan_plugin::ReadPlanPlugin`] instance
/// synchronously (`PluginHost::create_instance` runs the plugin's own
/// `create` before returning), so the returned event [`Receiver`] is
/// ready to observe `SESSION_ATTACHED` the moment the caller drives a
/// matching [`ProcessRecord`] through [`PluginHost::process_appeared`].
pub(super) fn create_read_plan_instance(
    host: &Arc<PluginHost>,
    id: PluginInstanceId,
    plugin_id: &str,
    process_names: &[&str],
    config: ReadPlanPluginConfig,
) -> (Receiver<ReadPlanEvent>, SyncSender<ReadPlanCommand>) {
    let (events, commands) = read_plan_plugin::register_with_config(plugin_id, config);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &read_plan_manifest(plugin_id, process_names),
        settings_schema: None,
        vtable: read_plan_plugin::vosu_plugin_vtable,
    }])
    .expect("loading the read-plan testkit plugin");
    let data_dir = tempfile::tempdir().unwrap();
    host.create_instance(id, &loaded[0], data_dir.path().to_path_buf(), None)
        .expect("creating the read-plan testkit instance");
    (events, commands)
}

pub(super) fn expect_attached(events: &Receiver<ReadPlanEvent>) -> u64 {
    match events.recv_timeout(RECV_TIMEOUT).expect("SESSION_ATTACHED") {
        ReadPlanEvent::Attached { handle } => handle,
        other => panic!("expected Attached, got {other:?}"),
    }
}

pub(super) fn expect_status(events: &Receiver<ReadPlanEvent>) -> vosu_status {
    match events
        .recv_timeout(RECV_TIMEOUT)
        .expect("submit/cancel status")
    {
        ReadPlanEvent::SubmitStatus(status) => status,
        other => panic!("expected SubmitStatus, got {other:?}"),
    }
}

/// `SubmitStatus` and `AsyncResult` for one submission can race in
/// either order once the job is admitted: the submitting thread resumes
/// from `send`'s own synchronous return at the same time a worker thread
/// may already be delivering the result through the instance's own
/// dispatch thread (§5 promises a *later* message, never one strictly
/// after `send` itself returns), mirroring `host/scans/tests.rs`'s own
/// `expect_status_and_result` (P2-10). Receives exactly these two
/// events, in whichever order they arrive.
pub(super) fn expect_status_and_async_result(
    events: &Receiver<ReadPlanEvent>,
) -> (vosu_status, vosu_plugin_api::ReadPlanResult) {
    let mut status = None;
    let mut result = None;
    for _ in 0..2 {
        match events
            .recv_timeout(RECV_TIMEOUT)
            .expect("submit status or READ_PLAN_RESULT")
        {
            ReadPlanEvent::SubmitStatus(value) => status = Some(value),
            ReadPlanEvent::AsyncResult(value) => result = Some(value),
            other => panic!("unexpected event {other:?}"),
        }
    }
    (
        status.expect("SubmitStatus"),
        result.expect("READ_PLAN_RESULT"),
    )
}

pub(super) fn one_op_stage(length: u32) -> Stage {
    Stage {
        base: StageBase::Root,
        base_offset: 0,
        ops: vec![PlanOp { offset: 0, length }],
    }
}

/// A deliberately slow [`ProcessMemory`] (set up through
/// [`set_session_memory_hook_for_tests`]) trips the read-plan deadline
/// (shortened for a test via [`set_read_plan_deadline_for_tests`]).
pub(super) struct SlowMemory {
    pub(super) sleep: Duration,
}

impl ProcessMemory for SlowMemory {
    fn read_exact(&self, _address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        std::thread::sleep(self.sleep);
        dst.fill(0);
        Ok(())
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

/// A [`ProcessMemory`] whose every read blocks until [`release_gate`] is
/// called, so a plan admitted under it never finishes (and so never frees
/// its [`OutstandingPlans`] slot by itself) until the test releases it —
/// the only way to hold async plans genuinely outstanding at once
/// deterministically, rather than racing real near-instant reads.
///
/// `started`, when set, counts how many `read_exact` calls have actually
/// begun blocking on `gate` so far: `VOSU_OK`/`SubmitStatus` alone only
/// proves a plan was admitted and queued, never that a worker thread has
/// actually dequeued and started running it
/// (`pool::a_cancel_racing_admission_still_removes_the_queued_job_and_leaves_no_leak`
/// flaked once, natively, on exactly that gap: it waited for
/// `BACKGROUND_WORKER_COUNT` submissions to each report `VOSU_OK` before
/// assuming every worker was now genuinely busy, but a slow-to-schedule
/// worker thread can still be sitting on an unstarted job when the next,
/// supposedly-queued-behind-them plan is submitted — a still-idle worker
/// can then dequeue that plan immediately instead). [`wait_for_started`]
/// blocks on this signal instead.
pub(super) struct GatedMemory {
    pub(super) gate: Arc<(Mutex<bool>, Condvar)>,
    pub(super) started: Option<Arc<(Mutex<usize>, Condvar)>>,
}

impl ProcessMemory for GatedMemory {
    fn read_exact(&self, _address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        if let Some(started) = &self.started {
            let (lock, condvar) = &**started;
            *lock.lock().unwrap_or_else(|poison| poison.into_inner()) += 1;
            condvar.notify_all();
        }
        let (lock, condvar) = &*self.gate;
        let mut released = lock.lock().unwrap_or_else(|poison| poison.into_inner());
        while !*released {
            released = condvar
                .wait(released)
                .unwrap_or_else(|poison| poison.into_inner());
        }
        dst.fill(0);
        Ok(())
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }
}

/// Blocks until [`GatedMemory::started`]'s own counter reaches `count`
/// (bounded: panics after [`RECV_TIMEOUT`] rather than hanging a stuck
/// test forever).
pub(super) fn wait_for_started(started: &Arc<(Mutex<usize>, Condvar)>, count: usize) {
    let (lock, condvar) = &**started;
    let guard = lock.lock().unwrap_or_else(|poison| poison.into_inner());
    let (guard, timeout) = condvar
        .wait_timeout_while(guard, RECV_TIMEOUT, |started| *started < count)
        .unwrap_or_else(|poison| poison.into_inner());
    assert!(
        !timeout.timed_out(),
        "only {} of {count} worker(s) ever started",
        *guard
    );
}

pub(super) fn release_gate(gate: &Arc<(Mutex<bool>, Condvar)>) {
    let (lock, condvar) = &**gate;
    *lock.lock().unwrap_or_else(|poison| poison.into_inner()) = true;
    condvar.notify_all();
}

/// A [`ProcessMemory`] that starts alive and can be [`Self::kill`]ed by a
/// test (F2): lets a test cache it while alive (so `session_memory`'s own
/// `open` succeeds) and only later report dead, distinguishing
/// `execute`'s own `is_alive()` check from a failed `open`
/// (`open_session_memory`, checked before `execute` ever runs —
/// `dead_process_answers_target_gone`'s own doc comment).
pub(super) struct DyingMemory {
    alive: AtomicBool,
}

impl DyingMemory {
    pub(super) fn new() -> Self {
        Self {
            alive: AtomicBool::new(true),
        }
    }

    pub(super) fn kill(&self) {
        self.alive.store(false, Ordering::SeqCst);
    }
}

impl ProcessMemory for DyingMemory {
    fn read_exact(&self, _address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        dst.fill(0);
        Ok(())
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(Vec::new())
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::SeqCst)
    }
}
