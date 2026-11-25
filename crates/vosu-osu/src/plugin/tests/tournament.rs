use super::*;
#[test]
fn tournament_manager_is_stable_priority_even_when_x64_and_not_foreground() {
    for platform in [Platform::Linux, Platform::Windows] {
        let (plugin, _data) = hermetic(RecordingHost::default(), platform);
        let mut message = attached(91, 0, "osu!", Bitness::X64);
        message.argv = Some(vec!["osu!.exe".into(), "--tournament".into()]);
        plugin
            .dispatch(Message::SessionAttached(message), 0)
            .unwrap();
        let sent = plugin.host.sent();
        let identified = identify_for(&sent, 91);
        assert_eq!(identified.variant, "stable");
        assert!(identified.focus_priority);
        let session = plugin.state.lock().unwrap().sessions[&91].clone();
        assert!(session.is_tournament_manager());
        assert_eq!(session.client, ClientType::Stable);
        plugin
            .dispatch(
                Message::SessionDetached(SessionDetached {
                    handle: 91,
                    reason: DetachReason::Exited,
                }),
                0,
            )
            .unwrap();
        assert!(session.lifecycle_token().is_cancelled());
        plugin.join_background_session_detaches();
        assert!(plugin.state.lock().unwrap().sessions.is_empty());
    }
}
