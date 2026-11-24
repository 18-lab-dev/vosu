//! `OsuPlugin`: the `ConcurrentPlugin` entry point (D22, abi:408 §10). This
//! unit (P2) wires attach/detach/settle-timer handling through
//! [`crate::classify::decide`] into `SESSION_DECLINE`/`SESSION_IDENTIFY`,
//! and routes `VOSU_OP_SCAN_RESULT` into P3's [`crate::scan::ScanWaiters`].
//! The session map, reader threads, and lazer offset-resolver lifecycle are
//! later units (P4-P5).
//!
//! Generic over `H: HostServices` (default [`VtableHostServices`]), exactly
//! like `PlanProcessMemory` (P3): the exported entry point only ever
//! instantiates `OsuPlugin<VtableHostServices>`, but a test supplies any
//! other `HostServices` implementation (a recording fake) directly, without
//! going through the real ABI vtable. `host` and `state` are `Arc`-wrapped
//! (not bare fields) so a lazer version lookup can run on its own thread
//! (see [`Self::commit_identify`]) while still sharing them with the
//! instance that spawned it.
//!
//! Lock order: a single `Mutex<PluginState>`, never held across a call into
//! `self.host` (`identify_session`/`send`/`set_timer`): every method reads
//! or mutates the state in its own short-lived guard, drops it, and only
//! then calls the host.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use vosu_osu_model::enums::ClientType;
use vosu_plugin_api::{
    CborValue, ConcurrentPlugin, HostServices, Init, LogLevel, Message, PluginError,
    SessionAttached, SessionDecline, SessionDetached, TimerFired, VtableHostServices, vosu_status,
};

use crate::classify::{
    self, AttachDecision, AttachFacts, Identified, PendingIdentify, Platform, RootResolver,
};
use crate::scan::ScanWaiters;

#[derive(Default)]
struct PluginState {
    /// Handles currently attached (inserted at `SESSION_ATTACHED`, removed
    /// at `SESSION_DETACHED`): consulted so a detach racing a concurrent
    /// identify never leaves a stale entry in `named_stable_handles`
    /// (review #3).
    live_handles: HashSet<u64>,
    /// Handles identified through the Linux "named" rule specifically
    /// (`Identified::named`/review #2), consulted so a later fallback
    /// attach's settle sees a named sibling (DF/R3) — never a settled
    /// fallback, a Windows client, or a nameless Linux attach, none of
    /// which are "named" even when they resolved to stable.
    named_stable_handles: HashSet<u64>,
    /// Linux fallback candidates waiting for `TIMER_FIRED`, keyed by this
    /// plugin's own timer id; the stored handle lets `SESSION_DETACHED`
    /// cancel one before its timer fires.
    pending_settle: HashMap<u64, (u64, PendingIdentify)>,
    next_timer: u64,
    /// Lazer version lookups handed off to their own thread (§4; review
    /// #6), joined on `Drop` so none outlives this plugin instance's
    /// borrowed `VtableHostServices`, whose raw pointers the ABI
    /// documents as valid only until `destroy` returns (abi:82).
    lazer_lookups: Vec<JoinHandle<()>>,
    /// Handles whose `SESSION_DETACHED` arrived before their own
    /// `SESSION_ATTACHED` (P3 review #4): `WorkerPool`'s concurrent
    /// dispatch does not preserve per-handle order, so this reordering is
    /// possible even though a host never actually detaches a handle it
    /// has not yet attached. Recorded here so `handle_attached` skips
    /// inserting the handle into `live_handles` — nothing would ever
    /// remove it otherwise — while still classifying and replying to the
    /// attach as usual.
    detached_before_attached: HashSet<u64>,
}

pub struct OsuPlugin<H: HostServices + Send + Sync + 'static = VtableHostServices> {
    host: Arc<H>,
    platform: Platform,
    state: Arc<Mutex<PluginState>>,
    root_resolver: RootResolver,
    scan_waiters: Arc<ScanWaiters>,
}

impl<H: HostServices + Send + Sync + 'static> OsuPlugin<H> {
    pub fn new(host: H, platform: Platform) -> Self {
        Self::with_root_resolver(host, platform, classify::resolve_root)
    }

    /// Like [`Self::new`], with an injectable root resolver (P3 review #7):
    /// a test uses this to avoid the real `/proc`/env-var reads
    /// `classify::resolve_root` performs; production
    /// (`ConcurrentPlugin::create`) always uses [`Self::new`].
    pub fn with_root_resolver(host: H, platform: Platform, root_resolver: RootResolver) -> Self {
        Self {
            host: Arc::new(host),
            platform,
            state: Arc::new(Mutex::new(PluginState::default())),
            root_resolver,
            scan_waiters: ScanWaiters::new(),
        }
    }

    /// The scan-result waiter map this instance's [`crate::scan::PlanScanBackend`]s
    /// share (wiring item 8): P4/P5 hand this to each session's backend.
    #[must_use]
    pub fn scan_waiters(&self) -> &Arc<ScanWaiters> {
        &self.scan_waiters
    }

    /// The real `on_message` logic, independent of `ConcurrentPlugin` so
    /// tests can drive it directly against a fake `H`.
    pub fn dispatch(&self, message: Message, correlation_id: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(attached) => self.handle_attached(&attached),
            Message::SessionDetached(detached) => {
                self.handle_detached(&detached);
                Ok(())
            }
            Message::TimerFired(fired) => self.handle_timer_fired(&fired),
            Message::ScanResult(result) => {
                self.scan_waiters.deliver(correlation_id, result);
                Ok(())
            }
            // Every other opcode this concurrent dispatch call could see has
            // no plugin-side action yet (later units: P3 read-plan replies;
            // P4/P5 session-lifecycle wiring beyond attach/detach/settle).
            _ => Ok(()),
        }
    }

    fn handle_attached(&self, message: &SessionAttached) -> Result<(), PluginError> {
        let facts = AttachFacts::from_attached_with(message, self.root_resolver);
        let named_sibling_attached = {
            let mut state = self.state.lock().expect("plugin state mutex poisoned");
            // A `SESSION_DETACHED` that raced this very attach and arrived
            // first (P3 review #4): this handle already detached, so it
            // must never be inserted into `live_handles` — no later
            // `SESSION_DETACHED` is still coming to remove it. Still
            // classified and replied to below, exactly as usual.
            if !state.detached_before_attached.remove(&facts.handle) {
                state.live_handles.insert(facts.handle);
            }
            !state.named_stable_handles.is_empty()
        };
        match classify::decide(&facts, self.platform, named_sibling_attached) {
            AttachDecision::Decline => self.send_decline(facts.handle),
            AttachDecision::Identify(identified) => self.commit_identify(facts.handle, identified),
            AttachDecision::Settle(pending) => self.schedule_settle(facts.handle, pending),
        }
    }

    fn handle_detached(&self, detached: &SessionDetached) {
        let mut state = self.state.lock().expect("plugin state mutex poisoned");
        let was_live = state.live_handles.remove(&detached.handle);
        state.named_stable_handles.remove(&detached.handle);
        state
            .pending_settle
            .retain(|_, (handle, _)| *handle != detached.handle);
        if !was_live {
            state.detached_before_attached.insert(detached.handle);
        }
        drop(state);
        self.scan_waiters.detach(detached.handle);
    }

    fn handle_timer_fired(&self, fired: &TimerFired) -> Result<(), PluginError> {
        let pending = {
            let mut state = self.state.lock().expect("plugin state mutex poisoned");
            state.pending_settle.remove(&fired.timer)
        };
        let Some((handle, pending)) = pending else {
            // Already detached (or a stale/foreign timer id): nothing to do.
            return Ok(());
        };
        self.resolve_settled(handle, pending)
    }

    /// The final client for a settled Linux fallback, against the sibling
    /// state known right now: used both when `TIMER_FIRED` actually fires
    /// and when `set_timer` itself was refused (review #5) and this handle
    /// must still be identified immediately instead of waiting forever.
    fn resolve_settled(&self, handle: u64, pending: PendingIdentify) -> Result<(), PluginError> {
        let named_sibling_attached = {
            let state = self.state.lock().expect("plugin state mutex poisoned");
            !state.named_stable_handles.is_empty()
        };
        let client = if named_sibling_attached {
            ClientType::Stable
        } else {
            ClientType::Lazer
        };
        self.commit_identify(
            handle,
            Identified {
                client,
                settings: pending.settings,
                root: pending.root,
                executable_root: pending.executable_root,
                pid: pending.pid,
                // A settled fallback is never "named" (review #2), whether
                // it resolved to stable or lazer.
                named: false,
            },
        )
    }

    fn send_decline(&self, handle: u64) -> Result<(), PluginError> {
        status_to_result(
            self.host
                .send(&Message::SessionDecline(SessionDecline { handle }), 0),
        )
    }

    fn schedule_settle(&self, handle: u64, pending: PendingIdentify) -> Result<(), PluginError> {
        let timer = {
            let mut state = self.state.lock().expect("plugin state mutex poisoned");
            let timer = state.next_timer;
            state.next_timer += 1;
            state.pending_settle.insert(timer, (handle, pending));
            timer
        };
        if self.host.set_timer(timer, None) == vosu_status::VOSU_OK {
            return Ok(());
        }
        // `set_timer` was refused (e.g. the 64-timer cap): remove the entry
        // just inserted and resolve immediately with the sibling state
        // known right now, rather than leaving this handle unidentified
        // forever (§6.2; review #5).
        let removed = {
            let mut state = self.state.lock().expect("plugin state mutex poisoned");
            state.pending_settle.remove(&timer)
        };
        match removed {
            Some((handle, pending)) => self.resolve_settled(handle, pending),
            // Raced with a detach between the insert above and here:
            // nothing left to resolve.
            None => Ok(()),
        }
    }

    /// Sends `SESSION_IDENTIFY` and, once it succeeds, records this handle
    /// as a named sibling if (and only if) `identified.named` and the
    /// handle is still live (review #2, #3): a stable identification that
    /// was not itself "named" must never count as a sibling, and a handle
    /// that detached while `identify_session` was in flight on another
    /// dispatch thread must never be added at all.
    ///
    /// Stable resolves and sends inline — no lazer version to look up.
    /// Lazer hands off to its own thread (see `resolve_lazer_version`):
    /// §4 says a `TIMER_FIRED`/`SESSION_ATTACHED` dispatch must hand off
    /// blocking work rather than do it inline on the host's worker pool,
    /// and the up-to-4 MiB `osu!.deps.json` read/parse, while bounded,
    /// still is blocking I/O (review #6). The spawned thread calls
    /// `finish_identify` itself, preserving per-handle identify ordering
    /// (nothing else ever identifies this handle again).
    fn commit_identify(&self, handle: u64, identified: Identified) -> Result<(), PluginError> {
        match identified.client {
            ClientType::Stable => finish_identify(
                self.host.as_ref(),
                &self.state,
                handle,
                "stable",
                "osu!",
                String::new(),
                identified.named,
            ),
            ClientType::Lazer => {
                let host = Arc::clone(&self.host);
                let state = Arc::clone(&self.state);
                let executable_root = identified.executable_root.clone();
                let pid = identified.pid;
                let named = identified.named;
                let spawned = thread::Builder::new()
                    .name("vosu-osu-lazer-version".to_owned())
                    .spawn(move || {
                        let version = resolve_lazer_version(host.as_ref(), pid, &executable_root);
                        let _ = finish_identify(
                            host.as_ref(),
                            &state,
                            handle,
                            "lazer",
                            "osu!(lazer)",
                            version,
                            named,
                        );
                    });
                match spawned {
                    Ok(join_handle) => {
                        let mut state = self.state.lock().expect("plugin state mutex poisoned");
                        state.lazer_lookups.retain(|lookup| !lookup.is_finished());
                        state.lazer_lookups.push(join_handle);
                        Ok(())
                    }
                    Err(_) => {
                        // Spawning the OS thread itself failed (resource
                        // exhaustion): fall back to resolving inline rather
                        // than dropping the identify entirely.
                        let version = resolve_lazer_version(
                            self.host.as_ref(),
                            identified.pid,
                            &identified.executable_root,
                        );
                        finish_identify(
                            self.host.as_ref(),
                            &self.state,
                            handle,
                            "lazer",
                            "osu!(lazer)",
                            version,
                            identified.named,
                        )
                    }
                }
            }
        }
    }

    /// Test-only: blocks until every lazer-version lookup spawned so far
    /// has sent its identify and returned, so a test can assert on a fake
    /// host's recorded messages deterministically instead of racing a
    /// background thread.
    #[cfg(test)]
    fn join_background_lazer_lookups(&self) {
        let lookups = std::mem::take(
            &mut self
                .state
                .lock()
                .expect("plugin state mutex poisoned")
                .lazer_lookups,
        );
        for lookup in lookups {
            let _ = lookup.join();
        }
    }
}

impl<H: HostServices + Send + Sync + 'static> Drop for OsuPlugin<H> {
    fn drop(&mut self) {
        // Every slot `ScanWaiters` handed out is released here if a
        // session detach did not already release it (wiring item 8).
        self.scan_waiters.shutdown();
        // Every lazer-version lookup thread is joined so none outlives
        // this instance's borrowed `VtableHostServices` (review #6): its
        // raw pointers are valid only until `destroy` returns (abi:82),
        // and this `Drop` is exactly what `destroy_concurrent` runs.
        let lookups = std::mem::take(
            &mut self
                .state
                .lock()
                .expect("plugin state mutex poisoned")
                .lazer_lookups,
        );
        for lookup in lookups {
            let _ = lookup.join();
        }
    }
}

/// Sends `SESSION_IDENTIFY` and records a named sibling, shared by the
/// inline (stable) and spawned-thread (lazer) paths of
/// [`OsuPlugin::commit_identify`] so both apply review #2/#3's rules
/// identically. Free (not a method) so the spawned thread can call it with
/// only the `Arc`s it captured, without borrowing `OsuPlugin` itself.
fn finish_identify<H: HostServices>(
    host: &H,
    state: &Mutex<PluginState>,
    handle: u64,
    variant: &str,
    name: &str,
    version: String,
    named: bool,
) -> Result<(), PluginError> {
    status_to_result(host.identify_session(handle, "osu", variant, name, &version, false))?;
    if named {
        let mut state = state.lock().expect("plugin state mutex poisoned");
        if state.live_handles.contains(&handle) {
            state.named_stable_handles.insert(handle);
        }
    }
    Ok(())
}

/// DF: lazer's version comes from `osu!.deps.json`. On Linux this first
/// tries the target's own filesystem view
/// (`vosu_osu_reader::memory::lazer_version::discover_for_process`, which
/// reads through `/proc/<pid>/root/...`) so a sandboxed/Flatpak lazer is
/// still found exactly as 1.x did (review #4); it falls back to the
/// daemon-view `executable_root` fact only when that pid-based lookup
/// failed and `executable_root` is non-empty — an empty `executable_root`
/// never falls back to a relative, cwd-dependent read (review #1). A read
/// failure (missing/corrupt file, unknown root) identifies with `""`
/// rather than failing the attach (R10).
type LazerVersionError = vosu_osu_reader::memory::lazer_version::LazerVersionError;
/// Injectable process-based lazer version lookup (P3 review #1's test
/// seam): production always passes `discover_for_process`.
type DiscoverForProcess = fn(u32) -> Result<String, LazerVersionError>;
/// Injectable root-based lazer version lookup, the same seam's other
/// half: production always passes `discover_from_root`.
type DiscoverFromRoot = fn(&Path) -> Result<String, LazerVersionError>;

fn resolve_lazer_version(host: &impl HostServices, pid: u32, executable_root: &Path) -> String {
    use vosu_osu_reader::memory::lazer_version::{discover_for_process, discover_from_root};
    resolve_lazer_version_with(
        host,
        pid,
        executable_root,
        discover_for_process,
        discover_from_root,
    )
}

/// Like [`resolve_lazer_version`], with both discovery steps injectable
/// (P3 review #1): a test proves the empty-`executable_root` guard itself
/// — not just that the (also empty, cwd-dependent) real result happens to
/// match — by injecting a process lookup that always fails and a root
/// lookup that panics if it is ever called with an empty path.
fn resolve_lazer_version_with(
    host: &impl HostServices,
    pid: u32,
    executable_root: &Path,
    discover_for_process: DiscoverForProcess,
    discover_from_root: DiscoverFromRoot,
) -> String {
    let result = discover_for_process(pid).or_else(|process_error| {
        if executable_root.as_os_str().is_empty() {
            Err(process_error)
        } else {
            discover_from_root(executable_root)
        }
    });
    match result {
        Ok(version) => version,
        Err(error) => {
            let _ = host.log(
                LogLevel::Warn,
                "osu!lazer version discovery failed; identifying without a version",
                CborValue::Map(vec![(
                    CborValue::Text("error".to_owned()),
                    CborValue::Text(error.to_string()),
                )]),
            );
            String::new()
        }
    }
}

impl ConcurrentPlugin for OsuPlugin<VtableHostServices> {
    fn create(host: VtableHostServices, _init: Init) -> Result<Self, PluginError> {
        Ok(Self::new(host, current_platform()))
    }

    fn on_message(&self, message: Message, correlation_id: u64) -> Result<(), PluginError> {
        self.dispatch(message, correlation_id)
    }
}

fn status_to_result(status: vosu_status) -> Result<(), PluginError> {
    match status {
        vosu_status::VOSU_OK => Ok(()),
        status => Err(status.into()),
    }
}

#[cfg(windows)]
fn current_platform() -> Platform {
    Platform::Windows
}

#[cfg(target_os = "linux")]
fn current_platform() -> Platform {
    Platform::Linux
}

#[cfg(test)]
mod tests;
