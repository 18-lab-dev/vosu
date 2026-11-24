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
            state.live_handles.insert(facts.handle);
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
        state.live_handles.remove(&detached.handle);
        state.named_stable_handles.remove(&detached.handle);
        state
            .pending_settle
            .retain(|_, (handle, _)| *handle != detached.handle);
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
fn resolve_lazer_version(host: &impl HostServices, pid: u32, executable_root: &Path) -> String {
    use vosu_osu_reader::memory::lazer_version::{discover_for_process, discover_from_root};
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
mod tests {
    use std::path::PathBuf;
    use std::sync::OnceLock;

    use vosu_osu_model::launch::InstanceSettings;
    use vosu_plugin_api::{Bitness, DetachReason, SessionIdentify, TimerSet};

    use super::*;

    #[derive(Default)]
    struct RecordingHost {
        sent: Mutex<Vec<Message>>,
    }

    impl HostServices for RecordingHost {
        fn send(&self, message: &Message, _correlation_id: u64) -> vosu_status {
            self.sent.lock().unwrap().push(message.clone());
            vosu_status::VOSU_OK
        }
    }

    impl RecordingHost {
        fn sent(&self) -> Vec<Message> {
            self.sent.lock().unwrap().clone()
        }
    }

    /// Every production test constructs its `OsuPlugin` through this
    /// helper rather than `OsuPlugin::new`: the injected resolver is pure
    /// path arithmetic, so no test ever performs a real `/proc`/env-var
    /// read through `classify::resolve_root` (review #7). The lazer
    /// version lookup's own `/proc/<pid>/root` read (review #4) is a
    /// deliberate, separate, bounded syscall this does not (and should
    /// not) suppress; tests that reach it use `pid: 0`, which never names
    /// a real process, so that lookup fails closed deterministically too.
    fn hermetic<H: HostServices + Send + Sync + 'static>(
        host: H,
        platform: Platform,
    ) -> OsuPlugin<H> {
        OsuPlugin::with_root_resolver(host, platform, test_root_resolver)
    }

    fn test_root_resolver(_pid: u32, path: Option<&Path>, _command_line: Option<&str>) -> PathBuf {
        path.and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_default()
    }

    fn attached(handle: u64, pid: u32, name: &str, bitness: Bitness) -> SessionAttached {
        SessionAttached {
            session: "vosu.osu".to_owned(),
            handle,
            generation: 1,
            pid,
            bitness,
            name: Some(name.to_owned()),
            path: None,
            command_line: None,
            argv: Some(Vec::new()),
        }
    }

    /// The single `SessionIdentify` for `handle` among `sent` (panics if
    /// there is not exactly one): identify/decline tests also see a
    /// `TimerSet` (settle) or `LogRecord` (a failed lazer version lookup,
    /// R10) in the same `sent` log, so assertions key on message shape
    /// rather than position.
    fn identify_for(sent: &[Message], handle: u64) -> &SessionIdentify {
        let mut matches = sent.iter().filter_map(|message| match message {
            Message::SessionIdentify(identify) if identify.handle == handle => Some(identify),
            _ => None,
        });
        let identify = matches
            .next()
            .unwrap_or_else(|| panic!("no SessionIdentify for handle {handle} in {sent:?}"));
        assert!(
            matches.next().is_none(),
            "more than one SessionIdentify for handle {handle} in {sent:?}"
        );
        identify
    }

    #[test]
    fn decline_is_sent_before_any_identify() {
        let host = RecordingHost::default();
        let plugin = hermetic(host, Platform::Windows);
        let mut message = attached(1, 100, "osu!.exe", Bitness::X86);
        message.argv = None;
        message.command_line = Some("osu!.exe --tournament".to_owned());
        plugin
            .dispatch(Message::SessionAttached(message), 0)
            .unwrap();

        assert_eq!(
            plugin.host.sent(),
            vec![Message::SessionDecline(SessionDecline { handle: 1 })]
        );
    }

    #[test]
    fn settle_sees_a_named_sibling_attached_in_the_same_pass() {
        let host = RecordingHost::default();
        let plugin = hermetic(host, Platform::Linux);

        // A lone fallback `osu!` attaches first: no sibling yet, so it
        // requests a settle timer instead of identifying immediately.
        plugin
            .dispatch(
                Message::SessionAttached(attached(1, 100, "osu!", Bitness::X86)),
                0,
            )
            .unwrap();
        assert!(plugin.host.sent().iter().all(|message| !matches!(
            message,
            Message::SessionIdentify(_) | Message::SessionDecline(_)
        )));
        let timer = *plugin
            .state
            .lock()
            .unwrap()
            .pending_settle
            .keys()
            .next()
            .unwrap();
        // The settle timer was actually requested (review #7): without
        // this, removing `set_timer` entirely would still pass.
        assert!(plugin.host.sent().contains(&Message::TimerSet(TimerSet {
            timer,
            interval_ms: None
        })));

        // A named stable sibling attaches and identifies within the same
        // pass, before the settle timer fires.
        plugin
            .dispatch(
                Message::SessionAttached(attached(2, 101, "osu!.exe", Bitness::X86)),
                0,
            )
            .unwrap();
        assert_eq!(identify_for(&plugin.host.sent(), 2).variant, "stable");

        // The settle timer fires: the sibling is now visible, so the
        // fallback resolves to stable, not lazer.
        plugin
            .dispatch(Message::TimerFired(TimerFired { timer }), 0)
            .unwrap();
        assert_eq!(identify_for(&plugin.host.sent(), 1).variant, "stable");
        assert!(plugin.state.lock().unwrap().pending_settle.is_empty());
    }

    #[test]
    fn settle_resolves_lazer_without_a_named_sibling() {
        let host = RecordingHost::default();
        let plugin = hermetic(host, Platform::Linux);
        // pid 0 never names a real process: the lazer version lookup's
        // own `/proc/<pid>/root` read (review #4) fails closed
        // deterministically, same as the daemon-view fallback below.
        let mut message = attached(1, 0, "osu!", Bitness::X86);
        message.path = Some("/nonexistent-vosu-osu-test-root/osu!".to_owned());
        plugin
            .dispatch(Message::SessionAttached(message), 0)
            .unwrap();
        let timer = *plugin
            .state
            .lock()
            .unwrap()
            .pending_settle
            .keys()
            .next()
            .unwrap();
        plugin
            .dispatch(Message::TimerFired(TimerFired { timer }), 0)
            .unwrap();
        plugin.join_background_lazer_lookups();
        let sent = plugin.host.sent();
        let identify = identify_for(&sent, 1);
        assert_eq!(identify.variant, "lazer");
        assert_eq!(identify.version, "");
    }

    #[test]
    fn detached_during_settle_sends_nothing() {
        let host = RecordingHost::default();
        let plugin = hermetic(host, Platform::Linux);
        plugin
            .dispatch(
                Message::SessionAttached(attached(1, 100, "osu!", Bitness::X86)),
                0,
            )
            .unwrap();
        let timer = *plugin
            .state
            .lock()
            .unwrap()
            .pending_settle
            .keys()
            .next()
            .unwrap();
        let sent_before_detach = plugin.host.sent();

        plugin
            .dispatch(
                Message::SessionDetached(SessionDetached {
                    handle: 1,
                    reason: DetachReason::Exited,
                }),
                0,
            )
            .unwrap();
        assert!(plugin.state.lock().unwrap().pending_settle.is_empty());

        plugin
            .dispatch(Message::TimerFired(TimerFired { timer }), 0)
            .unwrap();
        // The stale timer, firing after detach, sends nothing further: no
        // identify or decline, and no new message of any kind.
        assert_eq!(plugin.host.sent(), sent_before_detach);
    }

    /// Review #5: a refused `set_timer` must not leave the handle
    /// unidentified forever — it resolves immediately instead.
    #[test]
    fn refused_set_timer_identifies_immediately() {
        #[derive(Default)]
        struct RefusingTimerHost {
            sent: Mutex<Vec<Message>>,
        }
        impl HostServices for RefusingTimerHost {
            fn send(&self, message: &Message, _correlation_id: u64) -> vosu_status {
                if matches!(message, Message::TimerSet(_)) {
                    return vosu_status::VOSU_ERR_INVALID_ARGUMENT;
                }
                self.sent.lock().unwrap().push(message.clone());
                vosu_status::VOSU_OK
            }
        }
        impl RefusingTimerHost {
            fn sent(&self) -> Vec<Message> {
                self.sent.lock().unwrap().clone()
            }
        }

        let host = RefusingTimerHost::default();
        let plugin = hermetic(host, Platform::Linux);
        let mut message = attached(1, 0, "osu!", Bitness::X86);
        message.path = Some("/nonexistent-vosu-osu-test-root/osu!".to_owned());
        plugin
            .dispatch(Message::SessionAttached(message), 0)
            .unwrap();
        plugin.join_background_lazer_lookups();

        assert!(plugin.state.lock().unwrap().pending_settle.is_empty());
        let sent = plugin.host.sent();
        let identify = identify_for(&sent, 1);
        // No sibling was ever attached, so the immediate resolution is
        // exactly what `TIMER_FIRED` would eventually have produced.
        assert_eq!(identify.variant, "lazer");
    }

    /// Review #1: an attach with no `path` fact at all — so
    /// `executable_root` is empty — never reads a relative,
    /// cwd-dependent `osu!.deps.json`; it fails closed to `""` instead.
    #[test]
    fn missing_path_never_reads_a_relative_deps_json() {
        let host = RecordingHost::default();
        let plugin = hermetic(host, Platform::Windows);
        let mut message = attached(1, 0, "osu!.exe", Bitness::X64);
        message.argv = None;
        message.command_line = None; // path and command_line both absent
        plugin
            .dispatch(Message::SessionAttached(message), 0)
            .unwrap();
        plugin.join_background_lazer_lookups();

        let sent = plugin.host.sent();
        let identify = identify_for(&sent, 1);
        assert_eq!(identify.variant, "lazer");
        assert_eq!(identify.version, "");
    }

    /// Review #2: only a Linux "named" identification counts as a sibling
    /// — a settled fallback that itself resolved to stable must not.
    #[test]
    fn a_settled_fallback_does_not_count_as_a_named_sibling() {
        let host = RecordingHost::default();
        let plugin = hermetic(host, Platform::Linux);

        // Two lone `osu!` fallbacks attach; neither sees a sibling yet.
        plugin
            .dispatch(
                Message::SessionAttached(attached(1, 100, "osu!", Bitness::X86)),
                0,
            )
            .unwrap();
        plugin
            .dispatch(
                Message::SessionAttached(attached(2, 0, "osu!", Bitness::X86)),
                0,
            )
            .unwrap();
        let timer_for_handle = |plugin: &OsuPlugin<RecordingHost>, handle: u64| -> u64 {
            plugin
                .state
                .lock()
                .unwrap()
                .pending_settle
                .iter()
                .find(|(_, (settling_handle, _))| *settling_handle == handle)
                .map(|(timer, _)| *timer)
                .unwrap_or_else(|| panic!("no pending settle for handle {handle}"))
        };
        assert_eq!(plugin.state.lock().unwrap().pending_settle.len(), 2);
        let timer_for_1 = timer_for_handle(&plugin, 1);
        let timer_for_2 = timer_for_handle(&plugin, 2);

        // A named sibling attaches and identifies, then detaches again
        // before either timer fires.
        plugin
            .dispatch(
                Message::SessionAttached(attached(3, 101, "osu!.exe", Bitness::X86)),
                0,
            )
            .unwrap();
        assert_eq!(identify_for(&plugin.host.sent(), 3).variant, "stable");
        plugin
            .dispatch(
                Message::SessionDetached(SessionDetached {
                    handle: 3,
                    reason: DetachReason::Exited,
                }),
                0,
            )
            .unwrap();
        assert!(plugin.state.lock().unwrap().named_stable_handles.is_empty());

        // The first fallback's timer fires while the (now gone) named
        // sibling is still visible to it only through the (correctly
        // empty) `named_stable_handles` set: it resolves to lazer.
        plugin
            .dispatch(Message::TimerFired(TimerFired { timer: timer_for_1 }), 0)
            .unwrap();
        plugin.join_background_lazer_lookups();
        assert_eq!(identify_for(&plugin.host.sent(), 1).variant, "lazer");

        // Handle 1's settled-lazer identification must not itself count as
        // a sibling for handle 2's still-pending settle.
        plugin
            .dispatch(Message::TimerFired(TimerFired { timer: timer_for_2 }), 0)
            .unwrap();
        plugin.join_background_lazer_lookups();
        assert_eq!(identify_for(&plugin.host.sent(), 2).variant, "lazer");
    }

    /// Review #3: a detach landing between `identify_session` returning
    /// and this handle being recorded as a named sibling must not leave a
    /// stale entry behind. A fake host whose `send` immediately dispatches
    /// `SESSION_DETACHED` back into the same plugin, from inside the
    /// `SessionIdentify` send itself, reproduces the interleaving the
    /// host's concurrent worker pool could otherwise produce.
    #[test]
    fn detach_racing_identify_never_leaves_a_stale_named_sibling() {
        #[derive(Default)]
        struct ReentrantHost {
            sent: Mutex<Vec<Message>>,
            plugin: OnceLock<std::sync::Weak<OsuPlugin<ReentrantHost>>>,
        }
        impl HostServices for ReentrantHost {
            fn send(&self, message: &Message, _correlation_id: u64) -> vosu_status {
                self.sent.lock().unwrap().push(message.clone());
                if let Message::SessionIdentify(identify) = message
                    && let Some(plugin) = self.plugin.get().and_then(std::sync::Weak::upgrade)
                {
                    plugin
                        .dispatch(
                            Message::SessionDetached(SessionDetached {
                                handle: identify.handle,
                                reason: DetachReason::Exited,
                            }),
                            0,
                        )
                        .unwrap();
                }
                vosu_status::VOSU_OK
            }
        }

        let plugin = Arc::new(hermetic(ReentrantHost::default(), Platform::Linux));
        plugin
            .host
            .plugin
            .set(Arc::downgrade(&plugin))
            .unwrap_or_else(|_| panic!("set once"));

        // A named attach: identifies inline, synchronously re-entering
        // `handle_detached` from inside `send` before `commit_identify`'s
        // own insert runs.
        plugin
            .dispatch(
                Message::SessionAttached(attached(1, 100, "osu!.exe", Bitness::X86)),
                0,
            )
            .unwrap();

        assert!(
            !plugin
                .state
                .lock()
                .unwrap()
                .named_stable_handles
                .contains(&1),
            "a handle detached mid-identify must never be recorded as a live named sibling"
        );
        assert!(!plugin.state.lock().unwrap().live_handles.contains(&1));
    }

    #[test]
    fn windows_variant_identifies_immediately_by_bitness() {
        let host = RecordingHost::default();
        let plugin = hermetic(host, Platform::Windows);
        let mut message = attached(1, 0, "osu!.exe", Bitness::X64);
        message.argv = None;
        message.command_line = None;
        plugin
            .dispatch(Message::SessionAttached(message), 0)
            .unwrap();
        plugin.join_background_lazer_lookups();

        assert_eq!(identify_for(&plugin.host.sent(), 1).variant, "lazer");
    }

    /// `AttachFacts::from_attached` decodes `argv` into the settings a
    /// later session carries forward (P4's session map), even though
    /// `SESSION_IDENTIFY` itself has no settings fields on the wire.
    #[test]
    fn from_attached_decodes_spectator_settings_from_argv() {
        let facts = AttachFacts::from_attached_with(
            &SessionAttached {
                session: "vosu.osu".to_owned(),
                handle: 1,
                generation: 1,
                pid: 1,
                bitness: Bitness::X86,
                name: Some("osu!.exe".to_owned()),
                path: None,
                command_line: None,
                argv: Some(vec![
                    "/opt/osu!/osu!".to_owned(),
                    "--spectateclient=7".to_owned(),
                ]),
            },
            test_root_resolver,
        );
        let InstanceSettings {
            tournament_ipc_id, ..
        } = facts
            .args
            .as_ref()
            .map(vosu_osu_reader::discovery::instance_settings)
            .unwrap();
        assert_eq!(tournament_ipc_id, Some(7.0));
    }
}
