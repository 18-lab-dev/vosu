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
//! released on exactly one of those three paths, or (for a slot nobody is
//! waiting on any more) left *orphaned* until one of them arrives, rather
//! than freed early (P3 review item 4: there is no scan cancel opcode, so
//! a permit this plugin releases too early can still be legitimately
//! outstanding on the host's own side).

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
/// every session's [`PlanScanBackend`]. [`PlanScanBackend::scan_batch`]
/// never registers more than this many slots at once for its own single
/// call either (it groups a longer pattern list), so no single caller can
/// ever need more permits than this constant allows to exist in total —
/// the structural fix for the P3 review's deadlock finding (item 1).
const MAX_OUTSTANDING_SCANS: usize = 48;

const DEFAULT_SCAN_TIMEOUT: Duration = Duration::from_secs(30);

struct Slot {
    handle: vosu_session_id,
    result: Option<ScanResult>,
    /// Set once nothing is waiting on this slot any more (a timeout, or a
    /// caller dropping it before `wait`), but the host may still complete
    /// the scan it answers: the slot stays in the map, holding its
    /// permit, until [`ScanWaiters::deliver`] finally sees a late result
    /// for it (released immediately then) or [`ScanWaiters::detach`]/
    /// [`ScanWaiters::shutdown`] releases it (session torn down, or the
    /// plugin itself shutting down).
    orphaned: bool,
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
    /// [`ScanSlot::wait`]'s own timeout, waiting for a `VOSU_OP_SCAN_RESULT`.
    result_timeout: Duration,
    /// [`Self::register_many`]'s own deadline, waiting for enough
    /// outstanding-scan permits — independent of `result_timeout`: a
    /// test shortening one should not have to shorten the other too.
    permit_timeout: Duration,
}

impl ScanWaiters {
    #[must_use]
    pub fn new() -> Arc<Self> {
        Self::with_timeouts(DEFAULT_SCAN_TIMEOUT, DEFAULT_SCAN_TIMEOUT)
    }

    /// Like [`Self::new`], with the same overridden timeout for both
    /// [`ScanSlot::wait`] and [`Self::register_many`]: production code
    /// always uses the 30 s default for both; a test that only cares
    /// about shortening one of the two should use [`Self::with_timeouts`]
    /// instead, so the other keeps a timeout long enough not to fire
    /// while the test's own bounded assertions are still running.
    #[must_use]
    pub fn with_timeout(timeout: Duration) -> Arc<Self> {
        Self::with_timeouts(timeout, timeout)
    }

    #[must_use]
    pub fn with_timeouts(result_timeout: Duration, permit_timeout: Duration) -> Arc<Self> {
        Arc::new(Self {
            inner: Mutex::new(Inner {
                next_id: 1,
                slots: HashMap::new(),
            }),
            delivered: Condvar::new(),
            outstanding: Mutex::new(0),
            permit_freed: Condvar::new(),
            result_timeout,
            permit_timeout,
        })
    }

    /// Registers a single scan the same way [`Self::register_many`]
    /// registers a group: convenience for [`PlanScanBackend::scan_all`],
    /// which only ever needs one.
    pub fn register(self: &Arc<Self>, handle: vosu_session_id) -> Result<ScanSlot, MemoryError> {
        let mut slots = self.register_many(handle, 1)?;
        Ok(slots
            .pop()
            .expect("register_many(_, 1) always returns exactly one slot"))
    }

    /// Atomically reserves `count` semaphore permits and bounded map
    /// slots for `handle`'s scans, as one group: either all `count` are
    /// granted together, or none are — a caller never holds part of a
    /// group while blocked waiting for the rest. This is what actually
    /// prevents the deadlock a one-permit-at-a-time `register` loop can
    /// hit: several concurrent callers each registering their own group
    /// member-by-member can each claim part of the semaphore and then
    /// block wanting more, with nobody able to finish a group and start
    /// waiting (and so release anything) — "4 interleaved 16-pattern
    /// `scan_batch` calls deadlock permanently" (P3 review item 1).
    /// `count` must not exceed [`MAX_OUTSTANDING_SCANS`] (the semaphore's
    /// own total capacity; [`PlanScanBackend::scan_batch`] enforces this
    /// by grouping). Gives up and returns an error after this instance's
    /// own timeout rather than blocking forever if `count` permits never
    /// free up together. The returned [`ScanSlot`]s each release their
    /// own permit and slot unconditionally, however consumed or dropped
    /// (see [`Slot::orphaned`]).
    pub fn register_many(
        self: &Arc<Self>,
        handle: vosu_session_id,
        count: usize,
    ) -> Result<Vec<ScanSlot>, MemoryError> {
        assert!(
            count <= MAX_OUTSTANDING_SCANS,
            "a single registration group ({count}) must not exceed the {MAX_OUTSTANDING_SCANS}-permit semaphore's own total capacity"
        );
        if count == 0 {
            return Ok(Vec::new());
        }
        let deadline = Instant::now() + self.permit_timeout;
        {
            let mut outstanding = self
                .outstanding
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            loop {
                if *outstanding + count <= MAX_OUTSTANDING_SCANS {
                    *outstanding += count;
                    break;
                }
                let now = Instant::now();
                if now >= deadline {
                    return Err(MemoryError::Backend {
                        operation: "scan",
                        message: format!(
                            "timed out waiting for {count} outstanding-scan permits together"
                        ),
                    });
                }
                let (next_guard, _timeout) = self
                    .permit_freed
                    .wait_timeout(outstanding, deadline - now)
                    .unwrap_or_else(PoisonError::into_inner);
                outstanding = next_guard;
            }
        }
        let ids: Vec<u64> = {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            (0..count)
                .map(|_| {
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
                            orphaned: false,
                        },
                    );
                    id
                })
                .collect()
        };
        Ok(ids
            .into_iter()
            .map(|id| ScanSlot {
                id,
                waiters: Arc::clone(self),
            })
            .collect())
    }

    /// Releases `id`'s permit (not its map slot, which the caller has
    /// already removed, if at all): shared by every path that frees a
    /// permit without a result ever having arrived for the orphaned case
    /// this is folded into by [`Self::abandon`].
    fn free_permit(&self) {
        let mut outstanding = self
            .outstanding
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        *outstanding = outstanding.saturating_sub(1);
        drop(outstanding);
        self.permit_freed.notify_one();
    }

    /// Called by [`ScanSlot`]'s `Drop`, on every path out of a slot's
    /// life (a normal `wait()` return, a timeout, `?` abandoning a
    /// sibling slot in [`PlanScanBackend::scan_batch`], or a panic): if a
    /// result was already delivered (the normal case, `wait()` already
    /// read it) this fully releases the slot and its permit; otherwise
    /// the host may still be running this scan with no way to cancel it,
    /// so this only *orphans* the slot — it stays in the map, still
    /// holding its permit, until a late [`Self::deliver`] or a
    /// [`Self::detach`]/[`Self::shutdown`] finally releases it (P3 review
    /// item 4).
    fn abandon(&self, id: u64) {
        let had_result = {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            match inner.slots.get_mut(&id) {
                Some(slot) if slot.result.is_some() => {
                    inner.slots.remove(&id);
                    true
                }
                Some(slot) => {
                    slot.orphaned = true;
                    false
                }
                None => false,
            }
        };
        if had_result {
            self.free_permit();
        }
    }

    /// Called from the plugin's own `on_message` for every decoded
    /// `Message::ScanResult`: stores `result` under its matching
    /// `correlation_id` and wakes the [`ScanSlot::wait`] call blocked on
    /// it — or, for a slot already orphaned (its own caller gave up
    /// waiting), releases it immediately instead, since nobody is left to
    /// wake. A `correlation_id` this map no longer holds at all (already
    /// released via `detach`/`shutdown`) is silently ignored — the reply
    /// arrived too late to matter to anyone.
    pub fn deliver(&self, correlation_id: u64, result: ScanResult) {
        let release_now = {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            match inner.slots.get(&correlation_id) {
                Some(slot) if slot.orphaned => {
                    inner.slots.remove(&correlation_id);
                    true
                }
                Some(_) => {
                    inner
                        .slots
                        .get_mut(&correlation_id)
                        .expect("just matched above")
                        .result = Some(result);
                    drop(inner);
                    self.delivered.notify_all();
                    false
                }
                None => false,
            }
        };
        if release_now {
            self.free_permit();
        }
    }

    /// Releases every slot pending for `handle` — orphaned or still
    /// waited-on — with a synthetic error, for session teardown
    /// (`VOSU_OP_SESSION_DETACHED`): a scan blocked (or orphaned) on a
    /// session that just died would otherwise wait out its full timeout,
    /// or hold its permit forever, for nothing. Call once per detached
    /// `handle`.
    pub fn detach(&self, handle: vosu_session_id) {
        self.reject_pending(
            |slot| slot.handle == handle,
            "session_detached",
            "session detached while a scan was outstanding",
        );
    }

    /// Releases every slot still pending, for plugin `destroy`
    /// (`docs/v2/plugin-abi.md` abi:82's shutdown grace): nothing should
    /// still be blocked in [`ScanSlot::wait`], or holding an orphaned
    /// permit, once this returns.
    pub fn shutdown(&self) {
        self.reject_pending(|_| true, "shutting_down", "the plugin is shutting down");
    }

    fn reject_pending(&self, matches: impl Fn(&Slot) -> bool, code: &str, message: &str) {
        let mut released = 0usize;
        {
            let mut inner = self.inner.lock().unwrap_or_else(PoisonError::into_inner);
            let mut to_release = Vec::new();
            for (&id, slot) in &mut inner.slots {
                if !matches(slot) {
                    continue;
                }
                if slot.orphaned {
                    to_release.push(id);
                } else if slot.result.is_none() {
                    slot.result = Some(ScanResult {
                        addresses: Vec::new(),
                        error: Some(ErrorInfo {
                            code: code.to_owned(),
                            message: message.to_owned(),
                        }),
                    });
                }
            }
            for id in to_release {
                inner.slots.remove(&id);
                released += 1;
            }
        }
        self.delivered.notify_all();
        for _ in 0..released {
            self.free_permit();
        }
    }
}

/// One outstanding scan's reserved map slot and semaphore permit.
/// Releases both — immediately, or by orphaning until a late result or
/// `detach`/`shutdown` (above) — on every path out: [`Self::wait`]'s
/// return, a plain `drop`, or an unwinding panic, all through its `Drop`
/// impl, the only place that actually releases.
pub struct ScanSlot {
    id: u64,
    waiters: Arc<ScanWaiters>,
}

impl Drop for ScanSlot {
    fn drop(&mut self) {
        self.waiters.abandon(self.id);
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
    /// `MemoryError`. On a timeout this slot is orphaned rather than
    /// released (see [`Slot::orphaned`]); every other outcome removes it
    /// outright. `"permission_denied"` becomes
    /// [`MemoryError::PermissionDenied`] rather than the generic
    /// [`MemoryError::Backend`] every other scan error code becomes.
    pub fn wait(self) -> Result<Vec<u64>, MemoryError> {
        let deadline = Instant::now() + self.waiters.result_timeout;
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
            }) if error.code == "permission_denied" => Err(MemoryError::PermissionDenied {
                address: 0,
                len: 0,
                message: error.message,
            }),
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
    alive: Arc<std::sync::atomic::AtomicBool>,
}

impl<H: HostServices + Send + Sync + 'static> PlanScanBackend<H> {
    #[must_use]
    pub fn new(host: H, handle: vosu_session_id, waiters: Arc<ScanWaiters>) -> Self {
        Self::with_shared(
            host,
            handle,
            waiters,
            Arc::new(std::sync::atomic::AtomicBool::new(true)),
        )
    }

    /// Like [`Self::new`], sharing a liveness flag with
    /// [`crate::memory::PlanProcessMemory::with_shared`] for the same
    /// session: a `VOSU_ERR_TARGET_GONE` rejection seen on either path
    /// marks the session dead for both.
    #[must_use]
    pub fn with_shared(
        host: H,
        handle: vosu_session_id,
        waiters: Arc<ScanWaiters>,
        alive: Arc<std::sync::atomic::AtomicBool>,
    ) -> Self {
        Self {
            host,
            handle,
            waiters,
            alive,
        }
    }

    #[must_use]
    pub fn is_alive(&self) -> bool {
        self.alive.load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Submits one `VOSU_OP_SCAN_REQUEST` over an already-registered
    /// `slot`. A submission the host rejects outright is turned into an
    /// immediate synthetic [`ScanWaiters::deliver`] so `slot`'s `wait`
    /// never blocks for a reply that can now never arrive;
    /// `VOSU_ERR_TARGET_GONE` also flips [`Self::alive`] false, same as a
    /// read plan's rejection does.
    fn send_for(&self, slot: &ScanSlot, pattern: &Pattern, all_matches: bool) {
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
            if status == vosu_status::VOSU_ERR_TARGET_GONE {
                self.alive
                    .store(false, std::sync::atomic::Ordering::Relaxed);
            }
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
    }

    /// Registers one group of at most [`MAX_OUTSTANDING_SCANS`] patterns'
    /// permits atomically ([`ScanWaiters::register_many`]), sends every
    /// request in the group before waiting on any of them (so they are
    /// genuinely concurrent), then waits on each — appending
    /// `(pattern_index, address)` for patterns that matched to `out`.
    /// Every slot in the group is released before the next group's
    /// registration ever runs.
    fn scan_group(
        &self,
        patterns: &[Pattern],
        base_index: usize,
        out: &mut Vec<(usize, u64)>,
    ) -> Result<(), MemoryError> {
        let slots = self.waiters.register_many(self.handle, patterns.len())?;
        for (slot, pattern) in slots.iter().zip(patterns) {
            self.send_for(slot, pattern, false);
        }
        for (offset, slot) in slots.into_iter().enumerate() {
            let addresses = slot.wait()?;
            if let Some(&address) = addresses.first() {
                out.push((base_index + offset, address));
            }
        }
        Ok(())
    }
}

impl<H: HostServices + Send + Sync + 'static> ScanBackend for PlanScanBackend<H> {
    fn scan_all(&self, pattern: &Pattern) -> Result<Vec<usize>, MemoryError> {
        let slot = self.waiters.register(self.handle)?;
        self.send_for(&slot, pattern, true);
        let addresses = slot.wait()?;
        Ok(addresses
            .into_iter()
            .map(|address| address as usize)
            .collect())
    }

    /// Splits `patterns` into groups of at most [`MAX_OUTSTANDING_SCANS`]
    /// (item 1), resolving one group fully before starting the next, then
    /// returns every matched pattern's `(index, address)` sorted by
    /// ascending address — `ScanBackend`'s own contract ("the region
    /// walk's own order", `vosu-osu-reader/src/process.rs`). This is an
    /// exact match for a single-region fixture (every test and
    /// `StablePatterns` today); across several regions it can still
    /// diverge from the walk's own tie-break in one case the walk's own
    /// region-at-a-time traversal cannot be reconstructed from addresses
    /// alone: two different patterns whose *first* match both land in the
    /// same region, where the walk orders by request index, not by the
    /// matched offset within that region.
    fn scan_batch(&self, patterns: &[Pattern]) -> Result<Vec<BatchScanResult>, MemoryError> {
        let mut matches = Vec::with_capacity(patterns.len());
        for (group_index, group) in patterns.chunks(MAX_OUTSTANDING_SCANS).enumerate() {
            self.scan_group(group, group_index * MAX_OUTSTANDING_SCANS, &mut matches)?;
        }
        matches.sort_by_key(|&(_, address)| address);
        Ok(matches
            .into_iter()
            .map(|(index, address)| BatchScanResult {
                index,
                address: address as usize,
            })
            .collect())
    }
}

#[cfg(test)]
mod tests;
