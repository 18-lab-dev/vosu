use super::*;
use vosu_plugin_api::{DemandChanged, FieldSelector, FocusChanged};

#[test]
fn selected_error_subtrees_enable_collection_without_an_effective_precise_lane() {
    let (plugin, _data) = hermetic(RecordingHost::default(), Platform::Windows);
    plugin
        .dispatch(
            Message::SessionAttached(attached(61105, 61105, "osu!.exe", Bitness::X86)),
            0,
        )
        .unwrap();
    let session = plugin.state.lock().unwrap().sessions[&61105].clone();
    let publication = session.publication.get().unwrap();
    for (ns, pointer, collect) in [
        ("vosu.osu", "/play/hitOffsets", true),
        ("vosu.osu", "/play/keyHitErrors", true),
        ("vosu.osu", "/play/keyHitErrors/0", true),
        ("vosu.osu", "/play", true),
        ("vosu.osu", "", true),
        ("vosu.osu", "/play/hitOffsetsExtra", false),
        ("vosu.osu", "/play/score", false),
        ("other", "/play/hitOffsets", false),
    ] {
        plugin
            .dispatch(
                Message::DemandChanged(DemandChanged {
                    handle: 61105,
                    regular: false,
                    precise: false,
                    selectors: vec![FieldSelector {
                        ns: ns.into(),
                        pointer: pointer.into(),
                    }],
                }),
                0,
            )
            .unwrap();
        let receiver = publication.demand.bus_receiver();
        assert_eq!(receiver.borrow().collect_errors, collect, "{ns}{pointer}");
        assert!(!receiver.borrow().lanes.precise);
    }
}

#[derive(Clone, Default)]
struct ReentrantHost {
    plugin: Arc<OnceLock<std::sync::Weak<OsuPlugin<ReentrantHost, OfflineGenerator>>>>,
}
impl HostServices for ReentrantHost {
    fn send(&self, message: &Message, _: u64) -> vosu_status {
        if let Message::SessionIdentify(identify) = message {
            let plugin = self.plugin.get().unwrap().upgrade().unwrap();
            plugin
                .dispatch(
                    Message::DemandChanged(DemandChanged {
                        handle: identify.handle,
                        regular: true,
                        precise: true,
                        selectors: vec![],
                    }),
                    0,
                )
                .unwrap();
            plugin
                .dispatch(
                    Message::FocusChanged(FocusChanged {
                        handle: identify.handle,
                        focused: true,
                        foreground: Some(true),
                    }),
                    0,
                )
                .unwrap();
        }
        vosu_status::VOSU_OK
    }
}
#[test]
fn identify_reentrant_controls_are_retained_until_session_registration() {
    let host = ReentrantHost::default();
    let (plugin, _data) = hermetic(host.clone(), Platform::Windows);
    let plugin = Arc::new(plugin);
    host.plugin.set(Arc::downgrade(&plugin)).unwrap();
    plugin
        .dispatch(
            Message::SessionAttached(attached(61106, 61106, "osu!.exe", Bitness::X86)),
            0,
        )
        .unwrap();
    let session = plugin.state.lock().unwrap().sessions[&61106].clone();
    let publication = session.publication.get().unwrap();
    let receiver = publication.demand.bus_receiver();
    assert!(receiver.borrow().lanes.regular);
    assert!(receiver.borrow().lanes.precise);
    assert!(receiver.borrow().collect_errors);
    assert!(
        !publication.bus.update_foreground(Some(true)),
        "registration must retain the pre-return foreground"
    );
    assert!(plugin.state.lock().unwrap().pending_controls.is_empty());
}
