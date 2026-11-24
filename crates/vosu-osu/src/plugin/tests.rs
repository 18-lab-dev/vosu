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
fn hermetic<H: HostServices + Send + Sync + 'static>(host: H, platform: Platform) -> OsuPlugin<H> {
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
/// Both discovery steps are injected (P3 review #1): the process
/// lookup always fails, and the root lookup panics if the empty-root
/// guard ever lets it run, so this would fail (not just coincide with
/// a cwd that happens to have no `osu!.deps.json`) if the guard
/// regressed.
#[test]
fn missing_path_never_reads_a_relative_deps_json() {
    let host = RecordingHost::default();
    let version = resolve_lazer_version_with(
        &host,
        0,
        Path::new(""),
        |_pid| Err(LazerVersionError::MissingProcessRoot),
        |_root| panic!("must never read a relative root when executable_root is empty"),
    );
    assert_eq!(version, "");
    assert!(matches!(host.sent().as_slice(), [Message::LogRecord(_)]));
}

/// The same guard's other side: a non-empty `executable_root` does
/// reach the root-based fallback once the process lookup fails.
#[test]
fn non_empty_executable_root_reaches_the_root_fallback() {
    let host = RecordingHost::default();
    let version = resolve_lazer_version_with(
        &host,
        0,
        Path::new("/some/osu/root"),
        |_pid| Err(LazerVersionError::MissingProcessRoot),
        |_root| Ok("2026.1101.0".to_owned()),
    );
    assert_eq!(version, "2026.1101.0");
    assert!(host.sent().is_empty());
}

/// Review #2 (fixed scenario): only a Linux "named" identification
/// counts as a sibling. A settled fallback that itself resolved to
/// stable must not — the scenario that actually distinguishes this
/// from the old (buggy) code: settle handle 1 to stable *while* the
/// named sibling (handle 3) is still attached, only detach 3
/// afterward, then fire handle 2's timer and observe it still
/// resolves to lazer (the old code would have left handle 1 in the
/// sibling set, resolving handle 2 to stable instead). `pid: 0`
/// throughout keeps every lazer version lookup hermetic.
#[test]
fn a_settled_fallback_does_not_count_as_a_named_sibling() {
    let host = RecordingHost::default();
    let plugin = hermetic(host, Platform::Linux);

    // Two lone `osu!` fallbacks attach; neither sees a sibling yet.
    plugin
        .dispatch(
            Message::SessionAttached(attached(1, 0, "osu!", Bitness::X86)),
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

    // A named sibling attaches and identifies.
    plugin
        .dispatch(
            Message::SessionAttached(attached(3, 101, "osu!.exe", Bitness::X86)),
            0,
        )
        .unwrap();
    assert_eq!(identify_for(&plugin.host.sent(), 3).variant, "stable");

    // Handle 1's timer fires while the named sibling is still
    // attached: it settles to stable (correctly, and not "named").
    plugin
        .dispatch(Message::TimerFired(TimerFired { timer: timer_for_1 }), 0)
        .unwrap();
    assert_eq!(identify_for(&plugin.host.sent(), 1).variant, "stable");

    // The named sibling detaches.
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

    // Handle 2's timer fires: handle 1's settled-stable identification
    // must not itself have counted as a sibling, so this still
    // resolves to lazer.
    plugin
        .dispatch(Message::TimerFired(TimerFired { timer: timer_for_2 }), 0)
        .unwrap();
    plugin.join_background_lazer_lookups();
    assert_eq!(identify_for(&plugin.host.sent(), 2).variant, "lazer");
}

/// P3 review #4: a `SESSION_DETACHED` that arrives before its own
/// `SESSION_ATTACHED` (a `WorkerPool` reordering, not a real host
/// sequence) must not leak `live_handles` forever.
#[test]
fn detached_before_attached_does_not_leak_live_handles() {
    let host = RecordingHost::default();
    let plugin = hermetic(host, Platform::Linux);

    plugin
        .dispatch(
            Message::SessionDetached(SessionDetached {
                handle: 1,
                reason: DetachReason::Lost,
            }),
            0,
        )
        .unwrap();
    assert!(
        plugin
            .state
            .lock()
            .unwrap()
            .detached_before_attached
            .contains(&1)
    );
    assert!(!plugin.state.lock().unwrap().live_handles.contains(&1));

    plugin
        .dispatch(
            Message::SessionAttached(attached(1, 0, "osu!.exe", Bitness::X86)),
            0,
        )
        .unwrap();
    // The record is consumed by the attach, not left behind, and the
    // handle is never inserted into `live_handles` — nothing will
    // ever remove it, since no further `SESSION_DETACHED` is coming.
    assert!(
        !plugin
            .state
            .lock()
            .unwrap()
            .detached_before_attached
            .contains(&1)
    );
    assert!(!plugin.state.lock().unwrap().live_handles.contains(&1));
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
