use super::*;

#[test]
fn b2_settings_update_existing_and_future_sessions() {
    let (plugin, _data_dir) = hermetic(RecordingHost::default(), Platform::Windows);
    plugin
        .dispatch(
            Message::SessionAttached(attached(61059, 61059, "osu!.exe", Bitness::X86)),
            0,
        )
        .unwrap();
    let values = CborValue::Map(vec![
        (
            CborValue::Text("CALCULATE_PP".into()),
            CborValue::Bool(false),
        ),
        (CborValue::Text("POLL_RATE".into()), CborValue::Float(321.0)),
    ]);
    plugin
        .dispatch(
            Message::SettingsChanged(vosu_plugin_api::SettingsChanged { values }),
            0,
        )
        .unwrap();
    plugin
        .dispatch(
            Message::SessionAttached(attached(61060, 61060, "osu!.exe", Bitness::X86)),
            0,
        )
        .unwrap();
    for handle in [61059, 61060] {
        let session = plugin.state.lock().unwrap().sessions[&handle].clone();
        let current = *session.osu_settings.borrow();
        assert!(!current.calculate_pp);
        assert_eq!(current.poll_rate, 321.0);
    }
    assert!(
        plugin
            .dispatch(
                Message::SettingsChanged(vosu_plugin_api::SettingsChanged {
                    values: CborValue::Null
                }),
                0
            )
            .is_err()
    );
    assert_eq!(plugin.state.lock().unwrap().settings.poll_rate, 321.0);
}

#[test]
fn missing_sink_never_starts_a_capture_worker() {
    let (plugin, _data_dir) = hermetic(RecordingHost::default(), Platform::Windows);
    for handle in [61067, 61068] {
        plugin
            .dispatch(
                Message::SessionAttached(attached(handle, handle as u32, "osu!.exe", Bitness::X86)),
                0,
            )
            .unwrap();
        assert!(!plugin.state.lock().unwrap().sessions[&handle].capture_is_some());
    }
}

#[test]
fn settings_update_holds_ordering_lock_through_watch_publication() {
    let (plugin, _data_dir) = hermetic(RecordingHost::default(), Platform::Windows);
    plugin
        .dispatch(
            Message::SessionAttached(attached(61069, 61069, "osu!.exe", Bitness::X86)),
            0,
        )
        .unwrap();
    let session = plugin.state.lock().unwrap().sessions[&61069].clone();
    let receiver = session.osu_settings.subscribe();
    let held_settings = receiver.borrow();
    let (publishing_tx, publishing_rx) = std::sync::mpsc::sync_channel(1);
    let (done_tx, done_rx) = std::sync::mpsc::sync_channel(1);
    plugin.state.lock().unwrap().settings_before_send = Some(publishing_tx);
    std::thread::scope(|scope| {
        scope.spawn(|| {
            let values = CborValue::Map(vec![(
                CborValue::Text("POLL_RATE".into()),
                CborValue::Float(432.0),
            )]);
            plugin.update_settings(&values).unwrap();
            done_tx.send(()).unwrap();
        });
        publishing_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        // This borrow blocks the watch writer at the actual publication, not
        // at a timer or a sleep. A second update must still be excluded here.
        let serialized = plugin.state.try_lock().is_err();
        drop(held_settings);
        done_rx.recv_timeout(Duration::from_secs(5)).unwrap();
        assert!(
            serialized,
            "settings ordering lock was released before watch publication"
        );
    });
    assert_eq!(plugin.state.lock().unwrap().settings.poll_rate, 432.0);
    assert_eq!(session.osu_settings.borrow().poll_rate, 432.0);
}
