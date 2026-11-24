//! End-to-end scan routing tests, driven through a real
//! [`vosu_plugin_testkit::scan::ScanPlugin`] instance (never `scan.rs`/
//! this module's own functions called directly), against the test
//! process's own memory exactly as `read_plans/tests.rs` does for read
//! plans.

use std::sync::mpsc::{Receiver, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use vosu_plugin_api::ScanResult;
use vosu_plugin_testkit::scan::{self as scan_plugin, ScanCommand, ScanEvent};
use vosu_sys::process::{MemoryError, MemoryRegion, ProcessBitness, ProcessMemory, ReadRequest};

use super::super::super::registry;
use super::super::read_plans::set_session_memory_hook_for_tests;
use super::super::tests::{test_host, test_process};
use super::*;

const RECV_TIMEOUT: Duration = Duration::from_secs(5);

fn scan_manifest(plugin_id: &str, process_names: &[&str]) -> String {
    let names = process_names
        .iter()
        .map(|name| format!("'{name}'"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "kind='plugin'\nmanifest_version=1\nid='{plugin_id}'\nname='{plugin_id}'\nversion='0.1.0'\nhost='>=1.0.0, <2.0.0'\nabi_version=1\nruntime='in-process'\n\n[match]\nprocess_names=[{names}]\n"
    )
}

/// Registers and creates a [`scan_plugin::ScanPlugin`] instance
/// synchronously (`PluginHost::create_instance` runs the plugin's own
/// `create` before returning), so the returned event [`Receiver`] is
/// ready to observe `SESSION_ATTACHED` the moment the caller drives a
/// matching [`ProcessRecord`] through [`PluginHost::process_appeared`].
fn create_scan_instance(
    host: &Arc<PluginHost>,
    id: PluginInstanceId,
    plugin_id: &str,
    process_names: &[&str],
) -> (Receiver<ScanEvent>, SyncSender<ScanCommand>) {
    let (events, commands) = scan_plugin::register(plugin_id);
    let loaded = registry::load_bundled(&[registry::BundledPlugin {
        manifest: &scan_manifest(plugin_id, process_names),
        settings_schema: None,
        namespace_schemas: &[],
        action_schemas: &[],
        vtable: scan_plugin::vosu_plugin_vtable,
    }])
    .expect("loading the scan testkit plugin");
    let data_dir = tempfile::tempdir().unwrap();
    host.create_instance(id, &loaded[0], data_dir.path().to_path_buf(), None)
        .expect("creating the scan testkit instance");
    (events, commands)
}

fn expect_attached(events: &Receiver<ScanEvent>) -> u64 {
    match events.recv_timeout(RECV_TIMEOUT).expect("SESSION_ATTACHED") {
        ScanEvent::Attached { handle } => handle,
        other => panic!("expected Attached, got {other:?}"),
    }
}

fn expect_status(events: &Receiver<ScanEvent>) -> vosu_status {
    match events.recv_timeout(RECV_TIMEOUT).expect("submit status") {
        ScanEvent::SubmitStatus(status) => status,
        other => panic!("expected SubmitStatus, got {other:?}"),
    }
}

fn expect_result(events: &Receiver<ScanEvent>) -> (u64, ScanResult) {
    match events.recv_timeout(RECV_TIMEOUT).expect("SCAN_RESULT") {
        ScanEvent::Result {
            correlation_id,
            result,
        } => (correlation_id, result),
        other => panic!("expected Result, got {other:?}"),
    }
}

/// `SubmitStatus` and `Result` for one submission can race in either
/// order once the job is admitted: the submitting thread resumes from
/// `send`'s own synchronous return at the same time a worker thread may
/// already be delivering the result through the instance's own dispatch
/// thread (§5 promises a *later* message, never one strictly after
/// `send` itself returns) — a fast scan against a small fixed buffer
/// (`FixedMemory`) can win that race either way, unlike a `GatedMemory`
/// scan, which cannot complete before the test releases it. Receives
/// exactly these two events, in whichever order they arrive.
fn expect_status_and_result(events: &Receiver<ScanEvent>) -> (vosu_status, u64, ScanResult) {
    let mut status = None;
    let mut result = None;
    for _ in 0..2 {
        match events
            .recv_timeout(RECV_TIMEOUT)
            .expect("submit status or SCAN_RESULT")
        {
            ScanEvent::SubmitStatus(value) => status = Some(value),
            ScanEvent::Result {
                correlation_id,
                result: value,
            } => result = Some((correlation_id, value)),
            other => panic!("unexpected event {other:?}"),
        }
    }
    let status = status.expect("SubmitStatus");
    let (correlation_id, result) = result.expect("SCAN_RESULT");
    (status, correlation_id, result)
}

fn submit(
    commands: &SyncSender<ScanCommand>,
    correlation_id: u64,
    session: u64,
    module: Option<&str>,
    pattern: &str,
) {
    commands
        .send(ScanCommand {
            correlation_id,
            session,
            module: module.map(ToOwned::to_owned),
            pattern: pattern.to_owned(),
            non_zero_mask: false,
            all_matches: true,
        })
        .expect("sending the scan command");
}

/// A [`ProcessMemory`] backed by one fixed in-memory buffer at a chosen
/// base address — deterministic and fast, unlike scanning a real
/// process's whole (potentially multi-gigabyte) address space, which is
/// both slow and liable to an incidental false match somewhere in the
/// runtime/libc/loaded-library noise a real process carries.
struct FixedMemory {
    base: usize,
    bytes: Vec<u8>,
}

impl ProcessMemory for FixedMemory {
    fn read_exact(&self, address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let unreadable = || MemoryError::Read {
            address,
            len: dst.len(),
            message: "outside FixedMemory's fixed buffer".to_owned(),
        };
        let offset = address.checked_sub(self.base).ok_or_else(unreadable)?;
        let end = offset
            .checked_add(dst.len())
            .filter(|&end| end <= self.bytes.len())
            .ok_or_else(unreadable)?;
        dst.copy_from_slice(&self.bytes[offset..end]);
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
            address: self.base,
            size: self.bytes.len(),
        }])
    }
}

/// M1 Exit (phase (d1) step 4): a `VOSU_OP_SCAN_REQUEST` against a fixed
/// buffer finds a pattern planted in it, through `VOSU_OP_SCAN_RESULT`
/// carrying the request's own `correlation_id`.
#[test]
fn found_pattern_returns_matching_address() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_scan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.scan.found",
        &["proc-scan-found"],
    );

    let pid = 500_200_010;
    let base = 0x1_0000_0000usize;
    let mut bytes = vec![0u8; 64];
    let marker = [0xDE, 0xAD, 0xBE, 0xEF, 0xCA, 0xFE, 0xBA, 0xBE];
    bytes[20..28].copy_from_slice(&marker);
    set_session_memory_hook_for_tests(
        pid,
        Arc::new(FixedMemory { base, bytes }),
        ProcessBitness::X64,
    );

    host.process_appeared(test_process(pid, "proc-scan-found"));
    let handle = expect_attached(&events);

    submit(&commands, 42, handle, None, "DE AD BE EF CA FE BA BE");
    let (status, correlation_id, result) = expect_status_and_result(&events);
    assert_eq!(status, vosu_status::VOSU_OK);
    assert_eq!(correlation_id, 42);
    assert!(result.error.is_none());
    assert_eq!(result.addresses, vec![(base + 20) as u64]);
}

/// A pattern that cannot occur answers `addresses: []`, `error: null` —
/// found-nothing is not the A1 error form (§6.4).
#[test]
fn not_found_pattern_returns_empty_addresses_no_error() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_scan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.scan.notfound",
        &["proc-scan-notfound"],
    );

    let pid = 500_200_011;
    let base = 0x2_0000_0000usize;
    set_session_memory_hook_for_tests(
        pid,
        Arc::new(FixedMemory {
            base,
            bytes: vec![0u8; 64],
        }),
        ProcessBitness::X64,
    );

    host.process_appeared(test_process(pid, "proc-scan-notfound"));
    let handle = expect_attached(&events);

    submit(&commands, 7, handle, None, "DE AD BE EF CA FE BA BE");
    let (status, correlation_id, result) = expect_status_and_result(&events);
    assert_eq!(status, vosu_status::VOSU_OK);
    assert_eq!(correlation_id, 7);
    assert!(result.addresses.is_empty());
    assert!(result.error.is_none());
}

/// A module-scoped scan against a `set_session_memory_hook_for_tests` pid
/// resolves no real module (F3 P2's own fix): `build_module_regions`
/// substitutes `UnresolvedModuleRegions` for a hooked pid exactly as
/// `read_plans.rs` does, so the request answers `module_not_found` rather
/// than this test host trying (and failing) to read a real `/proc`/loader
/// table for a synthetic pid that was never actually exec'd.
#[test]
fn module_scoped_scan_on_hooked_pid_resolves_no_modules() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_scan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.scan.module",
        &["proc-scan-module"],
    );

    let pid = 500_200_012;
    let base = 0x3_0000_0000usize;
    set_session_memory_hook_for_tests(
        pid,
        Arc::new(FixedMemory {
            base,
            bytes: vec![0u8; 64],
        }),
        ProcessBitness::X64,
    );

    host.process_appeared(test_process(pid, "proc-scan-module"));
    let handle = expect_attached(&events);

    submit(&commands, 9, handle, Some("some.dll"), "DE AD BE EF");
    let (status, correlation_id, result) = expect_status_and_result(&events);
    assert_eq!(status, vosu_status::VOSU_OK);
    assert_eq!(correlation_id, 9);
    assert!(result.addresses.is_empty());
    assert_eq!(
        result.error.expect("module_not_found error").code,
        "module_not_found"
    );
}

/// §5: a malformed `pattern` is refused synchronously
/// (`VOSU_ERR_INVALID_ARGUMENT`), before any scan ever runs — no
/// `SCAN_RESULT` follows.
#[test]
fn malformed_pattern_is_refused_synchronously() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_scan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.scan.malformed",
        &["proc-scan-malformed"],
    );

    let pid = std::process::id();
    host.process_appeared(test_process(pid, "proc-scan-malformed"));
    let handle = expect_attached(&events);

    // Not valid `Pattern::parse` grammar: odd number of hex digits.
    submit(&commands, 1, handle, None, "DE A");
    assert_eq!(
        expect_status(&events),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );
}

/// §5: a scan touching a session this plugin did not match answers
/// `VOSU_ERR_PERMISSION_DENIED`.
#[test]
fn permission_denied_for_another_plugins_session() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events_a, commands_a) = create_scan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.scan.a",
        &["proc-scan-a"],
    );
    let (events_b, _commands_b) = create_scan_instance(
        &host,
        PluginInstanceId::new(2),
        "testkit.scan.b",
        &["proc-scan-b"],
    );

    host.process_appeared(test_process(911, "proc-scan-a"));
    let _handle_a = expect_attached(&events_a);
    host.process_appeared(test_process(912, "proc-scan-b"));
    let handle_b = expect_attached(&events_b);

    submit(&commands_a, 1, handle_b, None, "DE AD BE EF");
    assert_eq!(
        expect_status(&events_a),
        vosu_status::VOSU_ERR_PERMISSION_DENIED
    );
}

/// §5: a session whose process is already gone answers
/// `VOSU_ERR_TARGET_GONE`.
#[test]
fn dead_process_answers_target_gone() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_scan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.scan.dead",
        &["proc-scan-dead"],
    );

    // Not registered through `set_session_memory_hook_for_tests`, and far
    // past any real pid this machine will ever assign.
    let pid = 999_999_998;
    host.process_appeared(test_process(pid, "proc-scan-dead"));
    let handle = expect_attached(&events);

    submit(&commands, 1, handle, None, "DE AD BE EF");
    assert_eq!(expect_status(&events), vosu_status::VOSU_ERR_TARGET_GONE);
}

/// A [`ProcessMemory`] whose every read blocks until [`release_gate`] is
/// called, so a scan admitted under it never finishes on its own until
/// the test releases it — the only way to hold a scan genuinely in flight
/// deterministically, rather than racing a real near-instant scan.
struct GatedMemory {
    gate: Arc<(Mutex<bool>, Condvar)>,
}

impl ProcessMemory for GatedMemory {
    fn read_exact(&self, _address: usize, dst: &mut [u8]) -> Result<(), MemoryError> {
        let (lock, condvar) = &*self.gate;
        let released = lock.lock().unwrap_or_else(|poison| poison.into_inner());
        let (released, _) = condvar
            .wait_timeout_while(released, Duration::from_secs(10), |released| !*released)
            .unwrap_or_else(|poison| poison.into_inner());
        assert!(*released, "scan memory gate was not released within 10s");
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
        Ok(vec![MemoryRegion {
            address: 0x1000,
            size: 0x10,
        }])
    }
}

fn release_gate(gate: &Arc<(Mutex<bool>, Condvar)>) {
    let (lock, condvar) = &**gate;
    *lock.lock().unwrap_or_else(|poison| poison.into_inner()) = true;
    condvar.notify_all();
}

/// §6.4/A1: a session's detach frees every scan still outstanding for it,
/// delivering the session-detached error form exactly once — the worker
/// still running against [`GatedMemory`] never also delivers a second
/// `SCAN_RESULT` once it is finally released.
#[test]
fn detach_mid_scan_delivers_session_detached_error_exactly_once() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_scan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.scan.detach",
        &["proc-scan-detach"],
    );

    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let pid = 500_200_001;
    set_session_memory_hook_for_tests(
        pid,
        Arc::new(GatedMemory {
            gate: Arc::clone(&gate),
        }),
        ProcessBitness::X64,
    );

    host.process_appeared(test_process(pid, "proc-scan-detach"));
    let handle = expect_attached(&events);

    submit(&commands, 9, handle, None, "DE AD BE EF");
    assert_eq!(expect_status(&events), vosu_status::VOSU_OK);

    // The scan is now outstanding, blocked in `GatedMemory::read`.
    // Detach the session before releasing it.
    host.process_exited(pid);

    let (correlation_id, result) = expect_result(&events);
    assert_eq!(correlation_id, 9);
    assert!(result.addresses.is_empty());
    let error = result.error.expect("session-detached error form");
    assert_eq!(error.code, "session_detached");

    // Let the gated worker finish (it may still be running, racing the
    // cancellation): no second `SCAN_RESULT` follows — only one slot was
    // ever outstanding, and it has already been freed above.
    release_gate(&gate);
    assert!(
        events.recv_timeout(Duration::from_millis(200)).is_err(),
        "no second SCAN_RESULT should follow a session-detached delivery"
    );
}

/// §5/§6.4: the 64-outstanding-scan cap holds; a 65th scan is refused
/// outright; a session's detach frees every slot it still held.
#[test]
fn outstanding_scans_capped_then_freed_by_detach() {
    let runtime = tokio::runtime::Runtime::new().expect("building a Tokio runtime");
    let _enter = runtime.enter();
    let (_log_dir, host) = test_host(runtime.handle().clone());
    let (events, commands) = create_scan_instance(
        &host,
        PluginInstanceId::new(1),
        "testkit.scan.cap",
        &["proc-scan-cap"],
    );

    let gate = Arc::new((Mutex::new(false), std::sync::Condvar::new()));
    let pid1 = 500_200_002;
    let pid2 = 500_200_003;
    set_session_memory_hook_for_tests(
        pid1,
        Arc::new(GatedMemory {
            gate: Arc::clone(&gate),
        }),
        ProcessBitness::X64,
    );
    set_session_memory_hook_for_tests(
        pid2,
        Arc::new(GatedMemory {
            gate: Arc::clone(&gate),
        }),
        ProcessBitness::X64,
    );

    host.process_appeared(test_process(pid1, "proc-scan-cap"));
    let handle1 = expect_attached(&events);

    for correlation_id in 1..=64u64 {
        submit(&commands, correlation_id, handle1, None, "DE AD BE EF");
        assert_eq!(
            expect_status(&events),
            vosu_status::VOSU_OK,
            "scan {correlation_id}"
        );
    }

    // The cap holds: a 65th scan is refused outright.
    submit(&commands, 65, handle1, None, "DE AD BE EF");
    assert_eq!(
        expect_status(&events),
        vosu_status::VOSU_ERR_INVALID_ARGUMENT
    );

    // Detaching the session frees every slot it still held, for the whole
    // plugin instance (not just that one session) — proven by every one
    // of the 64 gated scans now delivering its session-detached error.
    host.process_exited(pid1);
    for _ in 1..=64u64 {
        let (_correlation_id, result) = expect_result(&events);
        let error = result.error.expect("session-detached error form");
        assert_eq!(error.code, "session_detached");
    }

    // A fresh session can scan again, proving the cap was actually freed.
    host.process_appeared(test_process(pid2, "proc-scan-cap"));
    let handle2 = expect_attached(&events);
    submit(&commands, 66, handle2, None, "DE AD BE EF");
    assert_eq!(expect_status(&events), vosu_status::VOSU_OK);

    // Let every gated worker (cancelled or not) finish, so dropping `host`
    // below does not wait on one still blocked.
    release_gate(&gate);
    let (correlation_id, _result) = expect_result(&events);
    assert_eq!(correlation_id, 66);
}
