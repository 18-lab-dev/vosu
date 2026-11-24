use super::*;

#[derive(Clone)]
struct RefusingHost(vosu_status);
impl HostServices for RefusingHost {
    fn send(&self, message: &Message, _: u64) -> vosu_status {
        match message {
            Message::SessionIdentify(_) | Message::SessionDecline(_) => self.0,
            _ => vosu_status::VOSU_OK,
        }
    }
}

#[test]
fn stale_identify_is_benign_and_never_registers_a_reader() {
    let (plugin, _data_dir) = hermetic(
        RefusingHost(vosu_status::VOSU_ERR_TARGET_GONE),
        Platform::Linux,
    );
    plugin
        .dispatch(
            Message::SessionAttached(attached(8701, 0, "osu!.exe", Bitness::X86)),
            0,
        )
        .unwrap();
    let state = plugin.state.lock().unwrap();
    assert!(state.sessions.is_empty());
    assert!(state.named_stable_handles.is_empty());
}

#[test]
fn stale_decline_is_benign_but_other_lifecycle_errors_propagate() {
    for status in [
        vosu_status::VOSU_ERR_TARGET_GONE,
        vosu_status::VOSU_ERR_INVALID_ARGUMENT,
        vosu_status::VOSU_ERR_PERMISSION_DENIED,
    ] {
        let (plugin, _data_dir) = hermetic(RefusingHost(status), Platform::Windows);
        let mut decline = attached(8702, 0, "osu!.exe", Bitness::X86);
        decline.argv = None;
        decline.command_line = Some("osu!.exe --tournament".into());
        let result = plugin.dispatch(Message::SessionAttached(decline), 0);
        if status == vosu_status::VOSU_ERR_TARGET_GONE {
            assert!(result.is_ok());
        } else {
            assert_eq!(result.unwrap_err().0, status);
            let result = plugin.dispatch(
                Message::SessionAttached(attached(8703, 0, "osu!.exe", Bitness::X86)),
                0,
            );
            assert_eq!(result.unwrap_err().0, status);
        }
        assert!(plugin.state.lock().unwrap().sessions.is_empty());
    }
}
