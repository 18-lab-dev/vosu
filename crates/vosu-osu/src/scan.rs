//! `PlanScanBackend`: a `ScanBackend` implementation that issues
//! `VOSU_OP_SCAN_REQUEST`s (§6.4) and waits for their `VOSU_OP_SCAN_RESULT`
//! through [`ScanWaiters`], the bounded, plugin-wide correlation-id waiter
//! map a plugin's own `on_message` routes host replies into (R8, D5).
//!
//! A `VOSU_OP_SCAN_REQUEST` has no synchronous delivery like a
//! `VOSU_READ_PLAN_SYNC` plan: its `VOSU_OP_SCAN_RESULT` reply arrives
//! later, on the instance's own dispatch thread, correlated by
//! `correlation_id` (§5). `plugin.rs`'s `on_message` must call
//! [`ScanWaiters::deliver`] for every decoded `Message::ScanResult`;
//! session teardown must call [`ScanWaiters::detach`]; `destroy` must call
//! [`ScanWaiters::shutdown`] — every slot this module hands out is
//! released on exactly one of those three paths, or its own 30 s timeout,
//! never left dangling.

use std::collections::HashMap;
use std::sync::{Arc, Condvar, Mutex, PoisonError};
use std::time::{Duration, Instant};

use vosu_osu_reader::process::{BatchScanResult, MemoryError, Pattern, ScanBackend};
use vosu_plugin_api::{
    ErrorInfo, HostServices, ScanRequest, ScanResult, VtableHostServices, vosu_session_id,
    vosu_status,
};

/// A plugin instance may have at most this many **asynchronous** scans
/// outstanding at once (`docs/v2/plugin-abi.md` §5's table: "64 scans
/// §6.4"): [`ScanWaiters`]'s own correlation-id map is bounded to it as a
/// defensive invariant — in practice [`MAX_OUTSTANDING_SCANS`]'s semaphore
/// always keeps the live count well under this, since every slot the map
/// holds also holds one of that semaphore's permits (checked by
/// `debug_assert!` in [`ScanWaiters::register`]).
const MAX_WAITERS: usize = 64;

/// The cap this backend actually enforces (R8), below the host's own hard
/// limit above: leaves headroom for whatever else this plugin instance
/// has outstanding, since one [`ScanWaiters`] is shared plugin-wide, across
/// every session's [`PlanScanBackend`].
const MAX_OUTSTANDING_SCANS: usize = 48;

const DEFAULT_SCAN_TIMEOUT: Duration = Duration::from_secs(30);

struct Slot {
    handle: vosu_session_id,
    result: Option<ScanResult>,
}

struct Inner {
    next_id: u64,
    slots: HashMap<u64, Slot>,
}

/// Bounded, plugin-wide map from a `VOSU_OP_SCAN_REQUEST`'s
/// `correlation_id` to the reader thread blocked in [`ScanSlot::wait`] for
/// its `VOSU_OP_SCAN_RESULT`, plus the semaphore throttling how many
/// scans this plugin instance keeps outstanding at once (R8). Shared via
/// `Arc` across every session's [`PlanScanBackend`], so the semaphore and
/// the 64-entry bound are truly plugin-wide, not per session.
pub struct ScanWaiters {
    inner: Mutex<Inner>,
    delivered: Condvar,
    outstanding: Mutex<usize>,
    permit_freed: Condvar,
    timeout: Duration,
}

impl ScanWaiters {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::with_timeout(DEFAULT_SCAN_TIMEOUT)
    }

    /// Like [`Self::new`], with an overridden wait timeout: production
    /// code always uses the 30 s default; tests use a much shorter one so
    /// the timeout path stays a bounded wait rather than a real sleep.
    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                next_id: 1,
                slots: HashMap::new(),
            }),
            delivered: Condvar::new(),
            outstanding: Mutex::new(0),
            permit_freed: Condvar::new(),
            timeout,
        })
    }

    /// Blocks until fewer than [`MAX_OUTSTANDING_SCANS`] scans are already
    /// outstanding plugin-wide, then reserves both a semaphore permit and
    /// a bounded map slot for `handle`'s scan. The returned [`ScanSlot`]
    /// releases both unconditionally, however it is consumed or dropped.
    pub fn register(self: &Arc<Self>, handle: vosu_session_id) -> ScanSlot {
        {
            let mut outstanding = self
                .outstanding
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            while *outstanding >= MAX_OUTSTANDING_SCANS {
                outstanding = self
                    .permit_freed
                    .wait(outstanding)
                    .unwrap_or_else(PoisonError::into_inner);
            }
            *outstanding += 1;
        }
        let id = {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            debug_assert!(
                inner.slots.len() < MAX_WAITERS,
                "the {MAX_OUTSTANDING_SCANS}-permit semaphore must keep this map under its {MAX_WAITERS}-entry bound"
            );
            let id = inner.next_id;
            inner.next_id = inner.next_id.wrapping_add(1).max(1);
            inner.slots.insert(
                id,
                Slot {
                    handle,
                    result: None,
                },
            );
            id
        };
        ScanSlot {
            id,
            waiters: Arc::clone(self),
        }
    }

    /// Releases `id`'s map slot and semaphore permit if still held;
    /// idempotent, so calling it more than once for the same `id` (e.g.
    /// once from [`ScanSlot::wait`]'s body and again from its `Drop`) is
    /// harmless.
    fn release(&self, id: u64) {
        let had_slot = {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            inner.slots.remove(&id).is_some()
        };
        if had_slot {
            let mut outstanding = self
                .outstanding
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            *outstanding = outstanding.saturating_sub(1);
            drop(outstanding);
            self.permit_freed.notify_one();
        }
    }

    /// Called from the plugin's own `on_message` for every decoded
    /// `Message::ScanResult`: stores `result` under its matching
    /// `correlation_id` and wakes the [`ScanSlot::wait`] call blocked on
    /// it. A `correlation_id` this map no longer holds (already timed
    /// out, detached, or shut down) is silently ignored — the reply
    /// arrived too late to matter to anyone.
    pub fn deliver(&self, correlation_id: u64, result: ScanResult) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        if let Some(slot) = inner.slots.get_mut(&correlation_id) {
            slot.result = Some(result);
            drop(inner);
            self.delivered.notify_all();
        }
    }

    /// Releases every slot still pending for `handle` with a synthetic
    /// error, for session teardown (`VOSU_OP_SESSION_DETACHED`): a scan
    /// blocked on a session that just died would otherwise wait out its
    /// full timeout for nothing. Call once per detached `handle`.
    pub fn detach(&self, handle: vosu_session_id) {
        self.reject_pending(
            |slot| slot.handle == handle,
            "session_detached",
            "session detached while a scan was outstanding",
        );
    }

    /// Releases every slot still pending, for plugin `destroy`
    /// (`docs/v2/plugin-abi.md` abi:82's shutdown grace): nothing should
    /// still be blocked in [`ScanSlot::wait`] once this returns.
    pub fn shutdown(&self) {
        self.reject_pending(|_| true, "shutting_down", "the plugin is shutting down");
    }

    fn reject_pending(&self, matches: impl Fn(&Slot) -> bool, code: &str, message: &str) {
        let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
        for slot in inner.slots.values_mut() {
            if slot.result.is_none() && matches(slot) {
                slot.result = Some(ScanResult {
                    addresses: Vec::new(),
                    error: Some(ErrorInfo {
                        code: code.to_owned(),
                        message: message.to_owned(),
                    }),
                });
            }
        }
        drop(inner);
        self.delivered.notify_all();
    }
}

/// One outstanding scan's reserved map slot and semaphore permit.
/// Releases both on every path out — [`Self::wait`]'s return,
/// [`ScanWaiters::detach`], [`ScanWaiters::shutdown`], a plain `drop`, or
/// an unwinding panic — via its `Drop` impl, which is the only place that
/// actually releases: `wait` and `drop` share that one path, so there is
/// no separate release call to forget on either one.
pub struct ScanSlot {
    id: u64,
    waiters: Arc<ScanWaiters>,
}

impl Drop for ScanSlot {
    fn drop(&mut self) {
        self.waiters.release(self.id);
    }
}

impl ScanSlot {
    #[must_use]
    pub const fn id(&self) -> u64 {
        self.id
    }

    /// Blocks this reader thread until [`ScanWaiters::deliver`] (normal
    /// completion), [`ScanWaiters::detach`]/[`ScanWaiters::shutdown`] (a
    /// synthetic error), or this scan's own timeout — whichever comes
    /// first — then returns the scanned addresses or the failure as a
    /// `MemoryError`. The slot's permit and map entry are released when
    /// this value drops, regardless of which of those three outcomes it
    /// was or whether the caller used `?` to propagate the error.
    pub fn wait(self) -> Result<Vec<u64>, MemoryError> {
        let deadline = Instant::now() + self.waiters.timeout;
        let delivered = {
            let mut inner = self
                .waiters
                .inner
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            loop {
                match inner.slots.get(&self.id) {
                    Some(Slot {
                        result: Some(result),
                        ..
                    }) => break Some(result.clone()),
                    Some(Slot { result: None, .. }) => {}
                    None => break None,
                }
                let now = Instant::now();
                if now >= deadline {
                    break None;
                }
                let (next_guard, _timeout) = self
                    .waiters
                    .delivered
                    .wait_timeout(inner, deadline - now)
                    .unwrap_or_else(PoisonError::into_inner);
                // Mesa-style condvar: re-check the slot at the top of the
                // loop regardless of whether this wait actually timed out
                // or was woken (possibly spuriously); the deadline check
                // above is what actually bounds the loop.
                inner = next_guard;
            }
        };
        match delivered {
            Some(ScanResult {
                addresses,
                error: None,
            }) => Ok(addresses),
            Some(ScanResult {
                error: Some(error), ..
            }) => Err(MemoryError::Backend {
                operation: "scan",
                message: format!("{}: {}", error.code, error.message),
            }),
            None => Err(MemoryError::Backend {
                operation: "scan",
                message: "timed out waiting for VOSU_OP_SCAN_RESULT".to_owned(),
            }),
        }
    }
}

/// A `ScanBackend` that scans this session's process through host-side
/// `VOSU_OP_SCAN_REQUEST`s instead of walking readable regions directly,
/// generic over the `HostServices` implementation so a unit test or C6's
/// bench can supply its own fake host.
pub struct PlanScanBackend<H: HostServices + Send + Sync + 'static = VtableHostServices> {
    host: H,
    handle: vosu_session_id,
    waiters: Arc<ScanWaiters>,
}

impl<H: HostServices + Send + Sync + 'static> PlanScanBackend<H> {
    #[must_use]
    pub fn new(host: H, handle: vosu_session_id, waiters: Arc<ScanWaiters>) -> Self {
        Self {
            host,
            handle,
            waiters,
        }
    }

    /// Registers a slot, submits one `VOSU_OP_SCAN_REQUEST`, and returns
    /// the slot unwaited (not yet blocking) so [`ScanBackend::scan_batch`]
    /// can issue every pattern's request before waiting on any of them. A
    /// submission the host rejects outright is turned into an immediate
    /// synthetic [`ScanWaiters::deliver`] so the returned slot's `wait`
    /// never blocks for a reply that can now never arrive.
    fn request(&self, pattern: &Pattern, all_matches: bool) -> ScanSlot {
        let slot = self.waiters.register(self.handle);
        let request = ScanRequest {
            session: self.handle,
            module: None,
            pattern: pattern.to_string(),
            non_zero_mask: pattern.non_zero_mask(),
            all_matches,
        };
        let correlation_id = slot.id();
        let status = self.host.scan(correlation_id, request);
        if status != vosu_status::VOSU_OK {
            self.waiters.deliver(
                correlation_id,
                ScanResult {
                    addresses: Vec::new(),
                    error: Some(ErrorInfo {
                        code: "rejected".to_owned(),
                        message: format!("host rejected the scan request: {status:?}"),
                    }),
                },
            );
        }
        slot
    }
}

impl<H: HostServices + Send + Sync + 'static> ScanBackend for PlanScanBackend<H> {
    fn scan_all(&self, pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
        let addresses = self.request(pattern, true).wait()?;
        Ok(addresses
            .into_iter()
            .map(|address| address as usize)
            .collect())
    }

    /// Issues every pattern's `all_matches: false` request before waiting
    /// on any of them ("N concurrent requests", P3): sending never blocks
    /// on a reply, so every request is already outstanding once the first
    /// `wait` call starts blocking.
    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        let slots: Vec<ScanSlot> = patterns
            .iter()
            .map(|pattern| self.request(pattern, false))
            .collect();
        let mut results = Vec::with_capacity(patterns.len());
        for (index, slot) in slots.into_iter().enumerate() {
            let addresses = slot.wait()?;
            if let Some(&address) = addresses.first() {
                results.push(BatchScanResult {
                    index,
                    address: address as usize,
                });
            }
        }
        Ok(results)
    }
}

#[cfg(test)]
mod tests;
