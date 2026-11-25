//! M5 X0 closes J3 on real children: a plugin host lost to a real abort or
//! to an independent OS kill while every kind of per-instance work it owns
//! is live at once (`docs/v2/plugin-abi.md` §7.6 "Loss identity", api-v1
//! jobs and resources):
//!
//! - an identified session whose target memory core is reading;
//! - a pending asynchronous read plan, its read blocked inside core;
//! - a stream body a client is waiting on, its `STREAM_READ` in flight;
//! - a running job (a held `work` invocation);
//! - a relayed call out of it (to a peer's held `work`) and one into it
//!   (from a third plugin's `ACTION_CALL`).
//!
//! The loss must end exactly that instance's waiters: the job and the lost
//! plugin's own call-issuing drive fail `plugin_crashed`, the stream body
//! fails long before the request deadline, the peer's relayed invocation is
//! cancelled, the calling plugin's relayed result is `plugin_crashed`, the
//! owned session leaves the store, and the pending plan is retired: its
//! read, released after the loss, is never delivered to anyone. The
//! instance restarts after its 1-second backoff and its next generation
//! gets its own result for the same plan id, while the peer's and the
//! caller's children, sessions, plans and other jobs stay as they were.

use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use vosu_core::actions::TerminalOutcome;
use vosu_core::plugin::LossCause;
use vosu_core::plugin::process::supervisor::{FailureReason, ProcessPhase};
use vosu_core::state::SessionKey;
use vosu_plugin_api::memory::{
    MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest,
};
use vosu_plugin_api::vosu_opcode;

use crate::support::api::{Api, bounded};
use crate::support::fixture::Receipt;
use crate::support::host::{Identity, wait_until};
use crate::support::services::{BOUND, Services, Spec, failure, kill, op};

/// The lost plugin's plan reads 8 bytes here.
const ADDRESS: u64 = 0x1000;
/// The plan id every plugin and generation submits.
const PLAN: u64 = 0x0A3;
/// The API's resource request deadline: a body that fails long before it
/// was ended by the loss, not by the deadline.
const RESOURCE_DEADLINE: Duration = Duration::from_secs(30);
/// How long the gated read waits for the test before giving up.
const GATE_BOUND: Duration = Duration::from_secs(60);
/// The abort's trigger: a `SETTINGS_CHANGED` only the test sends.
const SETTINGS: u32 = vosu_opcode::VOSU_OP_SETTINGS_CHANGED as u32;

/// Opens its gate when dropped.
struct OpenOnDrop(Arc<Gated>);

impl Drop for OpenOnDrop {
    fn drop(&mut self) {
        self.0.open();
    }
}

/// The value the `n`th read of [`ADDRESS`] returns.
fn value(n: u64) -> u64 {
    0x1A7E_0000_0000_0000 | n
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

/// The lost plugin's target memory: the first read of [`ADDRESS`] waits
/// inside core until [`Gated::open`]; every read returns [`value`] of its
/// own ordinal.
#[derive(Default)]
struct Gated {
    open: Mutex<bool>,
    opened: Condvar,
    reads: AtomicU64,
    /// Reads currently waiting at the gate.
    inside: AtomicUsize,
    /// Reads that returned.
    finished: AtomicUsize,
}

impl Gated {
    fn open(&self) {
        *self.open.lock().unwrap() = true;
        self.opened.notify_all();
    }
}

impl ProcessMemory for Gated {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        if address as u64 != ADDRESS || dst.len() != 8 {
            return Err(MemoryError::Read {
                address,
                len: dst.len(),
                message: "outside the J3 memory".to_owned(),
            });
        }
        let n = self.reads.fetch_add(1, Ordering::SeqCst) + 1;
        if n == 1 {
            self.inside.fetch_add(1, Ordering::SeqCst);
            let open = self.open.lock().unwrap();
            drop(
                self.opened
                    .wait_timeout_while(open, GATE_BOUND, |open| !*open)
                    .unwrap(),
            );
            self.inside.fetch_sub(1, Ordering::SeqCst);
        }
        dst.copy_from_slice(&value(n).to_le_bytes());
        self.finished.fetch_add(1, Ordering::SeqCst);
        Ok(())
    }

    fn read_many(&self, reads: &mut [ReadRequest<'_>]) -> Result<(), MemoryError> {
        for read in reads {
            self.read_exact(read.address, read.dst)?;
        }
        Ok(())
    }

    fn readable_regions(&self) -> Result<Vec<MemoryRegion>, MemoryError> {
        Ok(vec![MemoryRegion {
            address: ADDRESS as usize,
            size: 8,
        }])
    }
}

/// How the lost child dies.
#[derive(Clone, Copy, Debug)]
enum Loss {
    /// The plugin's own dispatch calls `std::process::abort`.
    Abort,
    /// The OS terminates it from outside core.
    Kill,
}

impl Loss {
    /// `(peer, lost, caller)` plugin names and the two targets.
    fn names(self) -> ([&'static str; 3], u32, u32) {
        match self {
            Self::Abort => (
                ["j3a.peer", "j3a.lost", "j3a.caller"],
                0x7E30_00A1,
                0x7E30_00A2,
            ),
            Self::Kill => (
                ["j3k.peer", "j3k.lost", "j3k.caller"],
                0x7E30_00B1,
                0x7E30_00B2,
            ),
        }
    }

    /// The exit status core records for this loss.
    fn status(self) -> i64 {
        match self {
            #[cfg(target_os = "linux")]
            Self::Abort => -6,
            #[cfg(windows)]
            Self::Abort => 0xC000_0409,
            #[cfg(target_os = "linux")]
            Self::Kill => -9,
            #[cfg(windows)]
            Self::Kill => 9,
        }
    }
}

const PEER: usize = 0;
const LOST: usize = 1;
const CALLER: usize = 2;

fn session_live(run: &Services, handle: u64) -> bool {
    run.host()
        .state()
        .store
        .current(SessionKey(handle))
        .is_some()
}

/// Waits for `index`'s `plan_result` of [`PLAN`] from child `pid`.
fn plan_result(run: &Services, index: usize, pid: u32) -> Receipt {
    run.wait(index, "plan_result", |receipt| {
        receipt.pid == pid && receipt.get("plan") == PLAN.to_string()
    })
}

/// Submits [`PLAN`] from `index` against its session `handle`.
fn submit(run: &Services, index: usize, handle: &str, address: u64, length: u32) {
    let handle: u64 = handle.parse().unwrap();
    let drive = run.drive(
        index,
        op(
            "plan",
            json!({"handle": handle, "plan": PLAN, "address": address, "length": length}),
        ),
    );
    assert_eq!(drive.status(), 0, "the plan submission was refused");
}

/// Offers `target` (its memory already hooked) to the lost plugin and
/// returns child `pid`'s attach receipt.
fn attach_lost(run: &Services, pid: u32, target: u32) -> Receipt {
    let plugin = run.plugin(LOST);
    run.host
        .offer_process(target, &plugin.process, vosu_plugin_api::Bitness::X64);
    let attach = run.wait(LOST, "attach", |receipt| {
        receipt.pid == pid && receipt.get("target") == target.to_string()
    });
    for key in ["identify", "declare", "publish"] {
        assert_eq!(attach.get(key), "0", "{key} was refused: {attach:?}");
    }
    attach
}

fn j3(loss: Loss) {
    let ([peer_name, lost_name, caller_name], peer_target, lost_target) = loss.names();
    let mut run = Services::empty();
    let files = run.root.path().join("j3-files");
    std::fs::create_dir_all(&files).unwrap();
    std::fs::write(files.join("alpha.osu"), b"j3 alpha\n").unwrap();
    run.add(&Spec {
        name: peer_name,
        ..Spec::default()
    });
    let peer_id = run.plugin(PEER).plugin_id.clone();
    run.add(&Spec {
        name: lost_name,
        root: Some(files),
        stream: 3 * u64::from(vosu_plugin_api::VOSU_STREAM_CHUNK_MAX_BYTES),
        hold_reads: true,
        fault: matches!(loss, Loss::Abort).then_some(("dispatch_abort", SETTINGS)),
        requires: vec![format!(
            "[[requires]]\nid = 'action:{peer_id}.work'\nversion = '^1.0'"
        )],
        ..Spec::default()
    });
    let lost_id = run.plugin(LOST).plugin_id.clone();
    run.add(&Spec {
        name: caller_name,
        requires: vec![format!(
            "[[requires]]\nid = 'action:{lost_id}.work'\nversion = '^1.0'"
        )],
        ..Spec::default()
    });
    let (peer, lost, caller) = (run.child(PEER), run.child(LOST), run.child(CALLER));
    let identities = [peer, lost, caller].map(Identity::of);
    let api = Api::new(run.host(), RESOURCE_DEADLINE);

    // Live before the loss. The peer: a session, a plan and a held job.
    let peer_attach = run.attach(PEER, peer_target);
    let peer_handle = peer_attach.get("handle").to_owned();
    let peer_job = run.invoke(PEER, "work", json!({"n": 4, "hold": true}));
    run.wait(PEER, "work", |work| work.get("n") == "4");

    // The lost plugin: an identified session ...
    let memory = Arc::new(Gated::default());
    // Opens the gate however the case ends, before the host's teardown
    // waits on the read's worker.
    let _gate = OpenOnDrop(Arc::clone(&memory));
    run.host.hook_memory(
        lost_target,
        Arc::clone(&memory) as Arc<dyn ProcessMemory>,
        ProcessBitness::X64,
    );
    let attach = attach_lost(&run, lost, lost_target);
    let handle: u64 = attach.get("handle").parse().unwrap();
    let session = attach.get("session").to_owned();
    assert!(
        session_live(&run, handle),
        "the session is not in the store"
    );
    // ... a pending plan whose read is blocked inside core ...
    submit(&run, LOST, attach.get("handle"), ADDRESS, 8);
    assert!(
        wait_until(BOUND, || memory.inside.load(Ordering::SeqCst) == 1),
        "the plan's read never reached the target"
    );
    // ... a running job ...
    let job = run.invoke(LOST, "work", json!({"n": 1, "hold": true}));
    run.wait(LOST, "work", |work| work.get("n") == "1");
    // ... a relayed call out, held by the peer ...
    let calling = run.drive(
        LOST,
        op(
            "call",
            json!({"action": format!("{peer_id}.work"), "count": 1, "args": {"n": 2, "hold": true}}),
        ),
    );
    let relayed_out = run.wait(PEER, "work", |work| work.get("n") == "2");
    assert_eq!(relayed_out.get("caller"), lost_id);
    // ... a relayed call in, from the third plugin ...
    let called = run.drive(
        CALLER,
        op(
            "call",
            json!({"action": format!("{lost_id}.work"), "count": 1, "args": {"n": 3, "hold": true}}),
        ),
    );
    let relayed_in = run.wait(LOST, "work", |work| work.get("n") == "3");
    assert_eq!(relayed_in.get("caller"), run.plugin(CALLER).plugin_id);
    let call = run.wait(CALLER, "call", |call| call.get("status") == "0");
    // ... and a stream body whose read the plugin holds unanswered.
    let document = api.json(&format!("/sessions/{session}/state/{lost_id}"));
    let stream = document["resources"]["stream"]["url"]
        .as_str()
        .unwrap_or_else(|| panic!("no stream reference: {document}"))
        .to_owned();
    let client = api.client.clone();
    let url = api.url(&stream);
    // The plugin answers the first read, so the response and its first
    // chunk arrive; the next read stays in flight.
    let body = api.runtime.spawn(async move {
        let mut response = client.get(url).send().await.unwrap();
        let status = response.status().as_u16();
        let mut received = 0;
        let complete = loop {
            match response.chunk().await {
                Ok(Some(chunk)) => received += chunk.len(),
                Ok(None) => break true,
                Err(_) => break false,
            }
        };
        (status, received, complete, Instant::now())
    });
    let held = run.wait(LOST, "held", |held| held.pid == lost);
    assert_eq!(
        run.host().process_status(run.instance(LOST)).unwrap().phase,
        ProcessPhase::Running
    );
    for (name, job) in [
        ("job", &job),
        ("calling drive", &calling),
        ("called drive", &called),
    ] {
        assert!(
            job.try_outcome().is_none(),
            "the {name} ended before the loss"
        );
    }
    println!(
        "RECORD j3 {loss:?} live: session={session} plan={PLAN} stream={} job={} relay_out={} relay_in={}",
        held.get("stream"),
        job.id,
        relayed_out.get("invocation"),
        relayed_in.get("invocation"),
    );

    // The loss.
    let lost_at = Instant::now();
    match loss {
        Loss::Abort => {
            run.apply_settings(LOST, json!({"LABEL": "j3-abort"}));
            let abort = run.wait(LOST, "abort", |abort| abort.pid == lost);
            assert_eq!(abort.get("opcode"), SETTINGS.to_string());
        }
        Loss::Kill => kill(lost),
    }
    assert!(
        wait_until(BOUND, || !identities[1].alive()),
        "the lost child was never reaped"
    );
    let reaped = Instant::now();
    println!(
        "RECORD j3 {loss:?} reaped {:?} after the loss",
        reaped - lost_at
    );

    // Exactly the lost instance's waiters unblock.
    assert_eq!(failure(job.outcome()), "plugin_crashed");
    assert_eq!(failure(calling.outcome()), "plugin_crashed");
    let (status, received, complete, ended) = api.runtime.block_on(bounded(body)).unwrap();
    let after = ended.saturating_duration_since(lost_at);
    println!(
        "RECORD j3 {loss:?} stream body failed {after:?} after the loss (status {status}, {received} bytes)"
    );
    assert_eq!(status, 200, "the response never started");
    assert!(received > 0, "the first chunk never arrived");
    assert!(!complete, "a lost child's stream body ended as if complete");
    assert!(
        after < RESOURCE_DEADLINE / 6,
        "the stream body waited {after:?}, as for the request deadline"
    );
    let cancel = run.wait(PEER, "cancel", |cancel| {
        cancel.get("invocation") == relayed_out.get("invocation")
    });
    assert_eq!(cancel.get("held"), "true", "{cancel:?}");
    let relayed = run.wait(CALLER, "relay_result", |result| {
        result.get("invocation") == call.get("call")
    });
    assert_eq!(
        (relayed.get("status"), relayed.get("code")),
        ("Error", "plugin_crashed"),
        "{relayed:?}"
    );
    assert_eq!(called.status(), 0);
    assert!(
        wait_until(BOUND, || !session_live(&run, handle)),
        "the lost generation's session is still owned"
    );
    assert!(
        peer_job.try_outcome().is_none(),
        "the peer's own job ended with another plugin's loss"
    );
    let status = run.host().process_status(run.instance(LOST)).unwrap();
    assert_eq!(
        status.last_failure,
        Some(FailureReason::Loss(LossCause::ProcessExited {
            status: Some(loss.status())
        })),
        "{status:?}"
    );

    // The pending plan is retired: its read, released now, reaches nobody.
    memory.open();
    assert!(
        wait_until(BOUND, || memory.finished.load(Ordering::SeqCst) == 1),
        "the released read never returned"
    );

    // The restart, after the backoff.
    assert!(
        wait_until(BOUND, || run
            .host()
            .process_status(run.instance(LOST))
            .is_some_and(
                |status| status.phase == ProcessPhase::Running && status.restarts == 1
            )),
        "the lost instance never restarted"
    );
    let delay = reaped.elapsed();
    println!("RECORD j3 {loss:?} restarted {delay:?} after the reap");
    assert!(
        delay >= Duration::from_millis(900),
        "restarted after {delay:?}, before the 1 s backoff"
    );
    let next = run.wait(LOST, "init", |init| init.pid != lost).pid;
    assert!(
        Identity::of(next).alive(),
        "the next generation is not live"
    );
    let reattached = attach_lost(&run, next, lost_target);
    assert_ne!(
        reattached.get("handle").parse::<u64>().unwrap(),
        handle,
        "the lost session handle was reused"
    );
    submit(&run, LOST, reattached.get("handle"), ADDRESS, 8);
    let own = plan_result(&run, LOST, next);
    assert_eq!(
        (own.get("result"), own.get("bytes")),
        ("ok", hex(&value(2).to_le_bytes()).as_str()),
        "the next generation's plan got the retired plan's bytes"
    );
    let results = run.plugin(LOST).fixture.receipts("plan_result");
    assert_eq!(
        results.len(),
        1,
        "a retired plan was delivered: {results:?}"
    );
    assert_eq!(memory.reads.load(Ordering::SeqCst), 2);
    assert!(run.plugin(LOST).fixture.receipts("destroy").is_empty());

    // The peer and the caller are untouched: same children, never lost,
    // the peer's session and plans served, its own job still answerable.
    for (index, identity) in [(PEER, &identities[0]), (CALLER, &identities[2])] {
        assert!(identity.alive(), "{} died", run.plugin(index).plugin_id);
        let status = run.host().process_status(run.instance(index)).unwrap();
        assert!(
            status.phase == ProcessPhase::Running
                && status.restarts == 0
                && status.crash_events == 0
                && status.last_failure.is_none(),
            "{status:?}"
        );
        assert_eq!(run.plugin(index).fixture.receipts("init").len(), 1);
    }
    assert!(session_live(&run, peer_handle.parse().unwrap()));
    submit(&run, PEER, &peer_handle, 0, 4);
    assert_eq!(
        plan_result(&run, PEER, peer).get("bytes"),
        hex(&peer_target.to_le_bytes())
    );
    assert_eq!(run.drive(PEER, op("answer", json!({}))).status(), 0);
    match peer_job.outcome() {
        TerminalOutcome::Succeeded(value) => assert_eq!(value["n"], 4),
        other => panic!("the peer's job: {other:?}"),
    }
    assert!(run.plugin(CALLER).fixture.receipts("detach").is_empty());
}

/// J3 with a real `std::process::abort` inside the lost plugin's dispatch.
#[test]
fn crash_j3_a_real_abort_with_every_waiter_live_ends_only_its_own() {
    j3(Loss::Abort);
}

/// J3 with an OS kill from outside core.
#[test]
fn crash_j3_an_os_kill_with_every_waiter_live_ends_only_its_own() {
    j3(Loss::Kill);
}
