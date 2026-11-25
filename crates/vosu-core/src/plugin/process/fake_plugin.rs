//! Test support for the relay's unit tests: a plugin `dispatch` written
//! in Rust that records every call (thread, header, payload, alignment)
//! and runs an optional hook inside it, so a test can block, re-enter
//! `send` or observe concurrency exactly where plugin code would; a
//! blocking gate; and a real channel pair with request accounting.

use std::ffi::c_void;
use std::sync::{Arc, Condvar, Mutex, MutexGuard, PoisonError};
use std::thread::ThreadId;
use std::time::{Duration, Instant};

use vosu_plugin_api::{VOSU_PLUGIN_ABI_MAJOR, vosu_message, vosu_plugin_vtable, vosu_status};
use vosu_sys::plugin_process::RingObjects;

use super::channel::{Endpoint, connect};
use super::frame::{Direction, DirectionPolicy};
use super::ring::{REGION_BYTES, initialize};
use crate::plugin::dispatch::PluginCtx;
use crate::plugin::execution::DispatchTarget;

#[cfg(target_os = "linux")]
fn adopt(objects: &RingObjects) -> RingObjects {
    RingObjects::from_inherited(
        objects.mapping_fd().try_clone_to_owned().unwrap(),
        objects.doorbell_fd().try_clone_to_owned().unwrap(),
        REGION_BYTES,
    )
    .unwrap()
}

#[cfg(windows)]
fn adopt(objects: &RingObjects) -> RingObjects {
    RingObjects::from_inherited(
        objects.mapping_handle().try_clone_to_owned().unwrap(),
        objects.doorbell_handle().try_clone_to_owned().unwrap(),
        REGION_BYTES,
    )
    .unwrap()
}

/// (core, child) ends of one channel, both with request accounting, as
/// production connects them.
pub(super) fn channel_pair() -> (Endpoint, Endpoint) {
    let to_plugin = RingObjects::create(REGION_BYTES).unwrap();
    let to_host = RingObjects::create(REGION_BYTES).unwrap();
    initialize(to_plugin.region()).unwrap();
    initialize(to_host.region()).unwrap();
    let child_to_plugin = adopt(&to_plugin);
    let child_to_host = adopt(&to_host);
    let core = connect(
        to_plugin.into(),
        to_host.into(),
        Box::new(DirectionPolicy::with_ledger(Direction::PluginToHost)),
    )
    .unwrap();
    let child = connect(
        child_to_host.into(),
        child_to_plugin.into(),
        Box::new(DirectionPolicy::with_ledger(Direction::HostToPlugin)),
    )
    .unwrap();
    (core, child)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Call {
    pub thread: ThreadId,
    pub opcode: u32,
    pub flags: u32,
    pub correlation: u64,
    pub payload: Vec<u8>,
    pub aligned: bool,
}

type Hook = Box<dyn Fn(&Call) + Send + Sync>;

#[derive(Default)]
pub(super) struct FakePlugin {
    calls: Mutex<Vec<Call>>,
    changed: Condvar,
    hook: Mutex<Option<Arc<Hook>>>,
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

unsafe extern "C" fn dispatch(ctx: *mut c_void, message: *const vosu_message) -> u32 {
    // SAFETY: `ctx` is the `FakePlugin` a test keeps alive (through its
    // `Arc`) for as long as any target built from it is dispatched to.
    let plugin = unsafe { &*ctx.cast::<FakePlugin>() };
    // SAFETY: the caller lends `message` for the duration of this call.
    let message = unsafe { &*message };
    let payload = if message.payload.len == 0 {
        Vec::new()
    } else {
        // SAFETY: the caller lends `payload.len` bytes at `payload.data`.
        unsafe {
            std::slice::from_raw_parts(message.payload.data, message.payload.len as usize).to_vec()
        }
    };
    let call = Call {
        thread: std::thread::current().id(),
        opcode: message.opcode,
        flags: message.flags,
        correlation: message.correlation_id,
        payload,
        aligned: (message.payload.data as usize).is_multiple_of(8),
    };
    let hook = lock(&plugin.hook).clone();
    if let Some(hook) = hook {
        hook(&call);
    }
    lock(&plugin.calls).push(call);
    plugin.changed.notify_all();
    vosu_status::VOSU_OK as u32
}

impl FakePlugin {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Runs `hook` inside every later `dispatch`, before the call is
    /// recorded.
    pub(super) fn set_hook(&self, hook: impl Fn(&Call) + Send + Sync + 'static) {
        *lock(&self.hook) = Some(Arc::new(Box::new(hook)));
    }

    /// A dispatch target for this plugin. The caller keeps `self` alive.
    pub(super) fn target(self: &Arc<Self>) -> DispatchTarget {
        DispatchTarget::Local {
            vtable: vosu_plugin_vtable {
                struct_size: core::mem::size_of::<vosu_plugin_vtable>() as u32,
                abi_version: VOSU_PLUGIN_ABI_MAJOR,
                create: None,
                dispatch: Some(dispatch),
                destroy: None,
            },
            ctx: PluginCtx(Arc::as_ptr(self).cast_mut().cast()),
        }
    }

    pub(super) fn calls(&self) -> Vec<Call> {
        lock(&self.calls).clone()
    }

    /// Waits up to `bound` for at least `count` recorded calls.
    pub(super) fn wait_calls(&self, count: usize, bound: Duration) -> Vec<Call> {
        let deadline = Instant::now() + bound;
        let mut calls = lock(&self.calls);
        while calls.len() < count {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            calls = self
                .changed
                .wait_timeout(calls, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        calls.clone()
    }
}

/// A one-shot gate a hook blocks on until the test opens it.
#[derive(Default)]
pub(super) struct Gate {
    open: Mutex<bool>,
    changed: Condvar,
    waiting: Mutex<usize>,
    arrived: Condvar,
}

impl Gate {
    /// Counts the calling thread as arrived without blocking.
    pub(super) fn arrive(&self) {
        *lock(&self.waiting) += 1;
        self.arrived.notify_all();
    }

    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }

    /// Blocks the calling thread until [`open`](Self::open).
    pub(super) fn wait(&self) {
        *lock(&self.waiting) += 1;
        self.arrived.notify_all();
        let mut open = lock(&self.open);
        while !*open {
            open = self
                .changed
                .wait(open)
                .unwrap_or_else(PoisonError::into_inner);
        }
    }

    /// Waits up to `bound` until `count` threads are blocked in
    /// [`wait`](Self::wait) (or passed it).
    pub(super) fn wait_arrivals(&self, count: usize, bound: Duration) -> bool {
        let deadline = Instant::now() + bound;
        let mut waiting = lock(&self.waiting);
        while *waiting < count {
            let now = Instant::now();
            if now >= deadline {
                return false;
            }
            waiting = self
                .arrived
                .wait_timeout(waiting, deadline - now)
                .unwrap_or_else(PoisonError::into_inner)
                .0;
        }
        true
    }

    pub(super) fn open(&self) {
        *lock(&self.open) = true;
        self.changed.notify_all();
    }
}
