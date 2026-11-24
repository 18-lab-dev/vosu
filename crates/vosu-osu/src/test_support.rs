//! Shared test harness for P3 (`memory`, `scan`): a `FakeHost` that
//! re-enters the SDK's own generated `dispatch` synchronously for
//! `VOSU_READ_PLAN_SYNC` submissions — exactly as a real in-process host's
//! D5 delivery does — and, for `VOSU_OP_SCAN_REQUEST`s, answers from a
//! spawned thread after a short simulated delay, the same way a real
//! host's own dispatch queue delivers a `VOSU_OP_SCAN_RESULT` later,
//! never inline inside `HostServices::scan`'s own call stack. A `FakeHost`
//! must be wrapped in [`SharedFakeHost`] to answer scan requests at all
//! (it needs an owned handle to outlive the spawned thread); a
//! read-plan-only test may construct it directly.

use std::cell::{Cell, RefCell};
use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};
use std::thread;

use vosu_osu_reader::process::Pattern;
use vosu_plugin_api::{
    Init, Message, PlanBase, Plugin, PluginError, ReadErrorCode, ReadPlanSubmit, ReadResult,
    ScanResult, StageBase, VtableHostServices, vosu_bytes, vosu_host_services, vosu_message,
    vosu_opcode, vosu_status,
};

type DeliverScanResult = dyn Fn(u64, ScanResult) + Send;

thread_local! {
    /// Set once per `FakeHost::new` call, just before that same thread's
    /// `vt.create` call: `HarnessPlugin::create` takes it out and stores
    /// it as its own field, since `on_message` (and so this callback)
    /// can run on a *different* thread — the spawned thread a
    /// `VOSU_OP_SCAN_RESULT` is answered from (item 7) — where this
    /// thread-local would be empty. `Plugin::create` takes no
    /// test-supplied arguments (the real ABI's own constraint, R16), so
    /// this hop through a thread-local, read back on the same thread one
    /// call later, is the only way to hand it in at all. A plain
    /// callback instead of a `crate::scan::ScanWaiters` directly keeps
    /// this harness independent of that module (it is a generic
    /// `FakeHost`, not P3-specific).
    static PENDING_DELIVERY: RefCell<Option<Box<DeliverScanResult>>> = const { RefCell::new(None) };
}

/// The one plugin every `FakeHost`-driven test uses: its whole job is
/// routing a decoded `Message::ScanResult` into the active test's own
/// delivery callback, exactly the seam `plugin.rs`'s own `on_message`
/// must provide in production (there, `ScanWaiters::deliver`).
struct HarnessPlugin {
    deliver: Box<DeliverScanResult>,
}

impl Plugin for HarnessPlugin {
    fn create(_host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        let deliver = PENDING_DELIVERY
            .with(|cell| cell.borrow_mut().take())
            .expect("FakeHost::new sets PENDING_DELIVERY on this same thread right before create");
        Ok(Self { deliver })
    }

    fn on_message(&mut self, message: Message, correlation_id: u64) -> Result<(), PluginError> {
        match message {
            Message::ScanResult(result) => {
                (self.deliver)(correlation_id, result);
                Ok(())
            }
            other => panic!("the harness plugin never expects {other:?} in on_message"),
        }
    }
}

unsafe extern "C" fn noop_send(_host_ctx: *mut c_void, _message: *const vosu_message) -> u32 {
    vosu_status::VOSU_OK as u32
}

fn noop_host_services() -> vosu_host_services {
    vosu_host_services {
        struct_size: std::mem::size_of::<vosu_host_services>() as u32,
        abi_version: vosu_plugin_api::VOSU_PLUGIN_ABI_MAJOR,
        send: Some(noop_send),
    }
}

fn init_bytes() -> Vec<u8> {
    vosu_plugin_api::encode(&Init {
        plugin_id: "test.vosu_osu.p3".to_owned(),
        settings: vosu_plugin_api::CborValue::Null,
        data_dir: std::path::PathBuf::from("/tmp"),
    })
    .expect("encode Init")
}

/// A fixed byte buffer standing in for one target process's memory,
/// addressed starting at `base`: an address outside `[base, base +
/// bytes.len())` answers as an unmapped read rather than panicking,
/// matching a real `ProcessMemory` backend.
pub struct FakeHost {
    plugin_ctx: Cell<*mut c_void>,
    dispatch: Cell<Option<unsafe extern "C" fn(*mut c_void, *const vosu_message) -> u32>>,
    bytes: Vec<u8>,
    base: usize,
    alive: Arc<AtomicBool>,
    submissions: Cell<usize>,
    /// Forces every op at this absolute address to answer
    /// `VOSU_READ_ERROR_PERMISSION_DENIED` instead of the ordinary
    /// unmapped-address `ReadFailed`.
    deny_read_permission_at: Cell<Option<usize>>,
    /// Forces the next `VOSU_OP_READ_PLAN_SUBMIT` to be rejected outright
    /// with `VOSU_ERR_PERMISSION_DENIED`, simulating a host that refuses
    /// the whole submission rather than one op within it.
    deny_submission_permission: Cell<bool>,
    /// Forces every `VOSU_OP_SCAN_REQUEST` to answer with the host's own
    /// `"permission_denied"` scan error code (§6.4).
    deny_scan_permission: Cell<bool>,
    /// An owned handle to this same host, set by [`SharedFakeHost::new`]:
    /// answering a scan request spawns a thread that must outlive the
    /// call to `HostServices::scan` that triggered it, so it needs a
    /// `'static` handle rather than `&self`. `None` for a host
    /// constructed directly (a read-plan-only test, which never receives
    /// a `ScanRequest`).
    self_handle: RefCell<Option<Weak<FakeHost>>>,
}

// SAFETY: `plugin_ctx` is a raw context from a `HarnessPlugin::create`
// call and `dispatch` that same plugin's own generated dispatch
// function; both are documented callable from any thread for the
// plugin instance's whole lifetime (`docs/v2/plugin-abi.md` §4's
// `concurrent_dispatch` contract, mirrored by `VtableHostServices`'s own
// identical `unsafe impl` in `sdk/plugin.rs`). Every other field is
// `Send + Sync` on its own.
unsafe impl Send for FakeHost {}
// SAFETY: see `Send` above.
unsafe impl Sync for FakeHost {}

/// Lets one `FakeHost` be shared, cheaply cloned, between a session's
/// `PlanProcessMemory` and `PlanScanBackend` exactly as a real session
/// shares one `VtableHostServices` (`Copy`) between both — a local
/// newtype, since the orphan rule forbids implementing the foreign
/// `HostServices` trait directly for the foreign `Arc<FakeHost>`. Also
/// the only way to give a `FakeHost` the `self_handle` answering a scan
/// request needs (above).
#[derive(Clone)]
pub struct SharedFakeHost(pub Arc<FakeHost>);

impl SharedFakeHost {
    #[must_use]
    pub fn new(host: FakeHost) -> Self {
        let shared = Arc::new(host);
        *shared.self_handle.borrow_mut() = Some(Arc::downgrade(&shared));
        Self(shared)
    }
}

impl vosu_plugin_api::HostServices for SharedFakeHost {
    fn send(&self, message: &Message, correlation_id: u64) -> vosu_status {
        self.0.send(message, correlation_id)
    }

    fn send_flat(
        &self,
        opcode: vosu_opcode,
        flags: u32,
        correlation_id: u64,
        payload: &[u8],
    ) -> vosu_status {
        self.0.send_flat(opcode, flags, correlation_id, payload)
    }
}

impl FakeHost {
    /// `deliver` is installed as the harness plugin's thread-local target
    /// for every `Message::ScanResult` this host's `dispatch` re-entry
    /// decodes — pass `move |id, result| waiters.deliver(id, result)` for
    /// a `ScanWaiters` under test. `alive` governs whether a submission
    /// is answered at all or rejected with `VOSU_ERR_TARGET_GONE`
    /// (simulating the target process dying between submission and
    /// execution).
    pub fn new(
        deliver: impl Fn(u64, ScanResult) + Send + 'static,
        bytes: Vec<u8>,
        base: usize,
        alive: Arc<AtomicBool>,
    ) -> Self {
        PENDING_DELIVERY.with(|cell| *cell.borrow_mut() = Some(Box::new(deliver)));
        let vt = vosu_plugin_api::__vtable_for_serialized::<HarnessPlugin>();
        let noop = noop_host_services();
        let init = init_bytes();
        // SAFETY: `noop`/`init` are valid for this call's duration; `vt`
        // is a freshly built vtable for `HarnessPlugin`.
        let plugin_ctx = unsafe {
            (vt.create.expect("vtable always sets create"))(
                &noop as *const vosu_host_services,
                core::ptr::null_mut(),
                vosu_bytes {
                    data: init.as_ptr(),
                    len: init.len() as u32,
                },
            )
        };
        assert!(!plugin_ctx.is_null(), "HarnessPlugin::create never fails");
        Self {
            plugin_ctx: Cell::new(plugin_ctx),
            dispatch: Cell::new(vt.dispatch),
            bytes,
            base,
            alive,
            submissions: Cell::new(0),
            deny_read_permission_at: Cell::new(None),
            deny_submission_permission: Cell::new(false),
            deny_scan_permission: Cell::new(false),
            self_handle: RefCell::new(None),
        }
    }

    /// Every later op at `address` answers
    /// `VOSU_READ_ERROR_PERMISSION_DENIED` instead of a plain unmapped
    /// `ReadFailed`.
    pub fn deny_read_permission_at(&self, address: usize) {
        self.deny_read_permission_at.set(Some(address));
    }

    /// Every later `VOSU_OP_READ_PLAN_SUBMIT` is rejected outright with
    /// `VOSU_ERR_PERMISSION_DENIED`.
    pub fn deny_submission_with_permission_denied(&self) {
        self.deny_submission_permission.set(true);
    }

    /// Every later `VOSU_OP_SCAN_REQUEST` answers with the host's own
    /// `"permission_denied"` scan error code.
    pub fn deny_scan_with_permission_denied(&self) {
        self.deny_scan_permission.set(true);
    }

    /// How many `VOSU_OP_READ_PLAN_SUBMIT`/`VOSU_OP_SCAN_REQUEST`
    /// messages this host has answered so far — a chunking test's way to
    /// prove a long read actually took more than one round trip.
    pub fn submission_count(&self) -> usize {
        self.submissions.get()
    }

    fn dispatch_message(&self, correlation_id: u64, message: &Message) {
        let payload = message.encode_payload().expect("encode harness reply");
        let raw = vosu_message {
            opcode: message.opcode() as u32,
            flags: 0,
            correlation_id,
            payload: vosu_bytes {
                data: payload.as_ptr(),
                len: payload.len() as u32,
            },
        };
        let dispatch = self.dispatch.get().expect("dispatch set by FakeHost::new");
        // SAFETY: `self.plugin_ctx` is a live context from the matching
        // `create` call above, `dispatch` is that same plugin's own
        // generated dispatch function, and `raw`'s payload (`payload`)
        // outlives this call.
        let raw_status = unsafe { dispatch(self.plugin_ctx.get(), &raw as *const vosu_message) };
        assert_eq!(
            raw_status,
            vosu_status::VOSU_OK as u32,
            "the harness plugin's on_message never rejects a reply"
        );
    }

    fn read_bytes(&self, address: usize, len: usize) -> ReadResult {
        if self.deny_read_permission_at.get() == Some(address) {
            return ReadResult::Error {
                code: ReadErrorCode::PermissionDenied,
                message: format!("permission denied reading address {address:#x}"),
            };
        }
        let Some(end) = address.checked_add(len) else {
            return ReadResult::Error {
                code: ReadErrorCode::AddressOverflow,
                message: format!("{address:#x} + {len:#x} overflowed"),
            };
        };
        if address < self.base || end > self.base + self.bytes.len() {
            return ReadResult::Error {
                code: ReadErrorCode::ReadFailed,
                message: format!("address {address:#x} (len {len}) is unmapped"),
            };
        }
        ReadResult::Ok(self.bytes[address - self.base..end - self.base].to_vec())
    }

    fn execute_plan(&self, submit: &ReadPlanSubmit) -> vosu_plugin_api::ReadPlanResult {
        let PlanBase::Absolute(root) = submit.root else {
            panic!("FakeHost only resolves VOSU_BASE_ABSOLUTE roots");
        };
        let mut results = Vec::new();
        for stage in &submit.stages {
            let StageBase::Root = stage.base else {
                panic!("FakeHost does not resolve dependent stages");
            };
            let base = i128::from(root) + i128::from(stage.base_offset);
            for op in &stage.ops {
                let address = base + i128::from(op.offset);
                let result = match usize::try_from(address) {
                    Ok(address) => self.read_bytes(address, op.length as usize),
                    Err(_) => ReadResult::Error {
                        code: ReadErrorCode::AddressOverflow,
                        message: format!("resolved address {address} does not fit usize"),
                    },
                };
                results.push(result);
            }
        }
        vosu_plugin_api::ReadPlanResult {
            plan: submit.plan,
            results,
        }
    }

    fn execute_scan(&self, request: &vosu_plugin_api::ScanRequest) -> vosu_plugin_api::ScanResult {
        if self.deny_scan_permission.get() {
            return vosu_plugin_api::ScanResult {
                addresses: Vec::new(),
                error: Some(vosu_plugin_api::ErrorInfo {
                    code: "permission_denied".to_owned(),
                    message: "permission denied scanning this process".to_owned(),
                }),
            };
        }
        let pattern = match Pattern::parse(&request.pattern, request.non_zero_mask) {
            Ok(pattern) => pattern,
            Err(error) => {
                return vosu_plugin_api::ScanResult {
                    addresses: Vec::new(),
                    error: Some(vosu_plugin_api::ErrorInfo {
                        code: "invalid_pattern".to_owned(),
                        message: error.to_string(),
                    }),
                };
            }
        };
        let mut addresses: Vec<u64> = scan_bytes(&self.bytes, &pattern)
            .into_iter()
            .map(|offset| (self.base + offset) as u64)
            .collect();
        if !request.all_matches {
            addresses.truncate(1);
        }
        vosu_plugin_api::ScanResult {
            addresses,
            error: None,
        }
    }
}

impl vosu_plugin_api::HostServices for FakeHost {
    fn send(&self, message: &Message, correlation_id: u64) -> vosu_status {
        self.submissions.set(self.submissions.get() + 1);
        if !self.alive.load(Ordering::Relaxed) {
            return vosu_status::VOSU_ERR_TARGET_GONE;
        }
        match message {
            Message::ScanRequest(request) => {
                let request = request.clone();
                let weak = self.self_handle.borrow().clone().expect(
                    "a scan-answering FakeHost must be constructed through SharedFakeHost::new",
                );
                // Answer from a spawned thread, exactly as a real host's
                // own dispatch queue delivers a VOSU_OP_SCAN_RESULT later
                // rather than inline inside this `send` call (review item
                // 7): this is what actually exercises `ScanWaiters`'
                // cross-thread condvar wakeup, a late-after-timeout drop,
                // and genuine concurrency between several outstanding
                // requests.
                thread::spawn(move || {
                    if let Some(host) = weak.upgrade() {
                        let result = host.execute_scan(&request);
                        host.dispatch_message(correlation_id, &Message::ScanResult(result));
                    }
                });
                vosu_status::VOSU_OK
            }
            other => panic!("FakeHost.send only expects ScanRequest, got {other:?}"),
        }
    }

    fn send_flat(
        &self,
        opcode: vosu_opcode,
        flags: u32,
        _correlation_id: u64,
        payload: &[u8],
    ) -> vosu_status {
        self.submissions.set(self.submissions.get() + 1);
        if !self.alive.load(Ordering::Relaxed) {
            return vosu_status::VOSU_ERR_TARGET_GONE;
        }
        if self.deny_submission_permission.get() {
            return vosu_status::VOSU_ERR_PERMISSION_DENIED;
        }
        assert_eq!(opcode, vosu_opcode::VOSU_OP_READ_PLAN_SUBMIT);
        let message = Message::decode(opcode, flags, payload).expect("well-formed submit");
        let Message::ReadPlanSubmit(submit) = message else {
            panic!("decoding VOSU_OP_READ_PLAN_SUBMIT always yields ReadPlanSubmit");
        };
        assert!(
            submit.sync,
            "FakeHost only exercises VOSU_READ_PLAN_SYNC submissions"
        );
        let result = self.execute_plan(&submit);
        self.dispatch_message(0, &Message::ReadPlanResult(result));
        vosu_status::VOSU_OK
    }
}

/// Brute-force signature scan over one contiguous buffer, in ascending
/// offset order — the fixture's whole buffer is one "region", so this
/// already matches `docs/v2/plugin-abi.md` §6.4's ordering rule (ascending
/// region, then ascending offset).
fn scan_bytes(haystack: &[u8], pattern: &Pattern) -> Vec<usize> {
    let bytes = pattern.bytes();
    let mask = pattern.mask();
    if bytes.is_empty() || haystack.len() < bytes.len() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    'starts: for start in 0..=haystack.len() - bytes.len() {
        for (index, (&expected, &masked)) in bytes.iter().zip(mask).enumerate() {
            let actual = haystack[start + index];
            if masked != 0 {
                if actual != expected {
                    continue 'starts;
                }
            } else if pattern.non_zero_mask() && actual == 0 {
                continue 'starts;
            }
        }
        matches.push(start);
    }
    matches
}
