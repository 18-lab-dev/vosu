//! `OsuPlugin`: the `ConcurrentPlugin` entry point (D22, abi:408 §10). This
//! unit (P2) wires attach/detach/settle-timer handling through
//! [`crate::classify::decide`] into `SESSION_DECLINE`/`SESSION_IDENTIFY`;
//! the session map, reader threads, and lazer offset-resolver lifecycle are
//! later units (P3-P5).
//!
//! Generic over `H: HostServices` (default [`VtableHostServices`]), exactly
//! like `PlanProcessMemory` (P3): the exported entry point only ever
//! instantiates `OsuPlugin<VtableHostServices>`, but a test supplies any
//! other `HostServices` implementation (a recording fake) directly, without
//! going through the real ABI vtable.
//!
//! Lock order: a single `Mutex<PluginState>`, never held across a call into
//! `self.host` (`identify_session`/`send`/`set_timer`): every method reads
//! or mutates the state in its own short-lived guard, drops it, and only
//! then calls the host.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::sync::Mutex;

use vosu_osu_model::enums::ClientType;
use vosu_plugin_api::{
    CborValue, ConcurrentPlugin, HostServices, Init, LogLevel, Message, PluginError,
    SessionAttached, SessionDecline, SessionDetached, TimerFired, VtableHostServices, vosu_status,
};

use crate::classify::{self, AttachDecision, AttachFacts, Identified, PendingIdentify, Platform};

#[derive(Default)]
struct PluginState {
    /// Handles currently identified as stable: consulted so a later Linux
    /// fallback attach's settle sees a named sibling (DF/R3).
    named_stable_handles: HashSet<u64>,
    /// Linux fallback candidates waiting for `TIMER_FIRED`, keyed by this
    /// plugin's own timer id; the stored handle lets `SESSION_DETACHED`
    /// cancel one before its timer fires.
    pending_settle: HashMap<u64, (u64, PendingIdentify)>,
    next_timer: u64,
}

pub struct OsuPlugin<H: HostServices + Send + Sync + 'static = VtableHostServices> {
    host: H,
    platform: Platform,
    state: Mutex<PluginState>,
}

impl<H: HostServices + Send + Sync + 'static> OsuPlugin<H> {
    pub fn new(host: H, platform: Platform) -> Self {
        Self {
            host,
            platform,
            state: Mutex::new(PluginState::default()),
        }
    }

    /// The real `on_message` logic, independent of `ConcurrentPlugin` so
    /// tests can drive it directly against a fake `H`.
    pub fn dispatch(&self, message: Message, _correlation_id: u64) -> Result<(), PluginError> {
        match message {
            Message::SessionAttached(attached) => self.handle_attached(&attached),
            Message::SessionDetached(detached) => {
                self.handle_detached(&detached);
                Ok(())
            }
            Message::TimerFired(fired) => self.handle_timer_fired(&fired),
            // Every other opcode this concurrent dispatch call could see has
            // no plugin-side action yet (later units: P3 read-plan/scan
            // replies — `ScanWaiters::deliver` routes `ScanResult` by
            // `correlation_id` — and P4/P5 session-lifecycle wiring beyond
            // attach/detach/settle).
            _ => Ok(()),
        }
    }

    fn handle_attached(&self, message: &SessionAttached) -> Result<(), PluginError> {
        let facts = AttachFacts::from_attached(message);
        let named_sibling_attached = {
            let state = self.state.lock().expect("plugin state mutex poisoned");
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
        state.named_stable_handles.remove(&detached.handle);
        state
            .pending_settle
            .retain(|_, (handle, _)| *handle != detached.handle);
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
        status_to_result(self.host.set_timer(timer, None))
    }

    fn commit_identify(&self, handle: u64, identified: Identified) -> Result<(), PluginError> {
        let (variant, name) = match identified.client {
            ClientType::Stable => ("stable", "osu!"),
            ClientType::Lazer => ("lazer", "osu!(lazer)"),
        };
        let version = match identified.client {
            ClientType::Stable => String::new(),
            ClientType::Lazer => self.lazer_version(&identified.executable_root),
        };
        status_to_result(
            self.host
                .identify_session(handle, "osu", variant, name, &version, false),
        )?;
        if identified.client == ClientType::Stable {
            self.state
                .lock()
                .expect("plugin state mutex poisoned")
                .named_stable_handles
                .insert(handle);
        }
        Ok(())
    }

    /// DF: lazer's version comes from `osu!.deps.json` under
    /// `executable_root`; a read failure (missing/corrupt file, no
    /// `executable_root` at all on a degraded-facts attach) identifies with
    /// `""` rather than failing the attach (R10: the spec does not yet
    /// define an unknown-version re-identify).
    fn lazer_version(&self, executable_root: &Path) -> String {
        match vosu_osu_reader::memory::lazer_version::discover_from_root(executable_root) {
            Ok(version) => version,
            Err(error) => {
                let _ = self.host.log(
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
    use vosu_osu_model::launch::InstanceSettings;
    use vosu_plugin_api::{Bitness, DetachReason, SessionIdentify};

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
        let plugin = OsuPlugin::new(host, Platform::Windows);
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
        let plugin = OsuPlugin::new(host, Platform::Linux);

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
        assert_eq!(plugin.state.lock().unwrap().pending_settle.len(), 1);

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
        assert_eq!(identify_for(&plugin.host.sent(), 1).variant, "stable");
        assert!(plugin.state.lock().unwrap().pending_settle.is_empty());
    }

    #[test]
    fn settle_resolves_lazer_without_a_named_sibling() {
        let host = RecordingHost::default();
        let plugin = OsuPlugin::new(host, Platform::Linux);
        let mut message = attached(1, 100, "osu!", Bitness::X86);
        // A deterministic, guaranteed-absent `executable_root` so the
        // lazer version lookup reliably fails closed to "" (R10), rather
        // than depending on the test process's own working directory.
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
        let sent = plugin.host.sent();
        let identify = identify_for(&sent, 1);
        assert_eq!(identify.variant, "lazer");
        assert_eq!(identify.version, "");
    }

    #[test]
    fn detached_during_settle_sends_nothing() {
        let host = RecordingHost::default();
        let plugin = OsuPlugin::new(host, Platform::Linux);
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

    #[test]
    fn windows_variant_identifies_immediately_by_bitness() {
        let host = RecordingHost::default();
        let plugin = OsuPlugin::new(host, Platform::Windows);
        let mut message = attached(1, 100, "osu!.exe", Bitness::X64);
        message.argv = None;
        message.command_line = None;
        plugin
            .dispatch(Message::SessionAttached(message), 0)
            .unwrap();

        assert_eq!(identify_for(&plugin.host.sent(), 1).variant, "lazer");
    }

    /// `AttachFacts::from_attached` decodes `argv` into the settings a
    /// later session carries forward (P4's session map), even though
    /// `SESSION_IDENTIFY` itself has no settings fields on the wire.
    #[test]
    fn from_attached_decodes_spectator_settings_from_argv() {
        let facts = AttachFacts::from_attached(&SessionAttached {
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
        });
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
