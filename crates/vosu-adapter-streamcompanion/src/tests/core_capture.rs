use super::*;
use std::time::Duration;
use vosu_core::plugin::{self, PluginHost, PluginInstanceId};
use vosu_plugin_api::{
    HostServices, Init, Message, Plugin, PluginError, PublishBatchWriter, PublishValueRef,
    SessionIdentify, VtableHostServices, vosu_opcode, vosu_status,
};
use vosu_surface::state::SessionTarget;

struct Storage;
impl plugin::settings::SettingsStorage for Storage {
    fn load(
        &self,
        _: &str,
    ) -> Result<serde_json::Map<String, Value>, plugin::settings::SettingsError> {
        Ok(Default::default())
    }
    fn store(
        &self,
        _: &str,
        _: &serde_json::Map<String, Value>,
    ) -> Result<(), plugin::settings::SettingsError> {
        Ok(())
    }
}
struct Console;
impl vosu_core::logging::ConsoleSink for Console {
    fn is_tty(&self) -> bool {
        false
    }
    fn write_log(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
    fn clear_progress(&self, _: usize) -> std::io::Result<()> {
        Ok(())
    }
    fn write_progress(&self, _: &str) -> std::io::Result<()> {
        Ok(())
    }
}
struct Source(VtableHostServices);
impl Plugin for Source {
    fn create(host: VtableHostServices, _: Init) -> Result<Self, PluginError> {
        Ok(Self(host))
    }
    fn on_message(&mut self, message: Message, _: u64) -> Result<(), PluginError> {
        if let Message::SessionAttached(attached) = message {
            assert_eq!(
                self.0.send(
                    &Message::SessionIdentify(SessionIdentify {
                        handle: attached.handle,
                        game: "osu".into(),
                        variant: "stable".into(),
                        name: "SC regression".into(),
                        version: "1973".into(),
                        focus_priority: false,
                    }),
                    0
                ),
                vosu_status::VOSU_OK
            );
            let mut capture = fixture();
            let beatmap = Arc::make_mut(&mut capture.values)
                .iter_mut()
                .find(|v| v.pointer == "/beatmap")
                .unwrap();
            let mut value: Value = beatmap.value.as_ref().unwrap().deserialized().unwrap();
            value["time"]["live"] = json!(61234.5);
            value["stats"] = json!({"stars":{"live":7.21},"bpm":{"realtime":187.1234}});
            beatmap.value = Some(CborValue::serialized(&value).unwrap());
            let mut writer = PublishBatchWriter::new();
            writer.begin(attached.handle);
            let encoded: Vec<_> = capture
                .values
                .iter()
                .filter(|v| view::POINTERS.contains(&v.pointer.as_str()))
                .map(|v| {
                    let mut bytes = Vec::new();
                    ciborium::ser::into_writer(v.value.as_ref().unwrap(), &mut bytes).unwrap();
                    (v.pointer.as_str(), bytes)
                })
                .collect();
            for (pointer, bytes) in &encoded {
                writer.push("vosu.osu", pointer, PublishValueRef::Cbor(bytes));
            }
            let mut bytes = Vec::new();
            writer.finish_into(&mut bytes).unwrap();
            let message = Message::decode(vosu_opcode::VOSU_OP_PUBLISH_BATCH, 0, &bytes).unwrap();
            assert_eq!(self.0.send(&message, 0), vosu_status::VOSU_OK);
        }
        Ok(())
    }
}

#[test]
fn production_selectors_capture_live_time_stats_and_preserve_descendant_lanes() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let root = tempfile::tempdir().unwrap();
    let _enter = runtime.enter();
    let logger =
        vosu_core::logging::Logger::new(root.path(), Default::default(), Arc::new(Console))
            .unwrap();
    let host = PluginHost::new_with_foreground(
        logger,
        Arc::new(plugin::settings::PluginSettingsService::new(Arc::new(
            Storage,
        ))),
        runtime.handle().clone(),
        Arc::new(plugin::SyntheticForegroundSource::new(None)),
    );
    let source_manifest = "kind='plugin'\nmanifest_version=1\nid='vosu.osu'\nname='SC fixture source'\nversion='0.1.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n[match]\nprocess_names=['fixture.exe']\n[[provides]]\nid='field:vosu.osu'\nversion='1.3.0'\nschema='state.json'\n[[provides]]\nid='game:osu'\nversion='*'\n";
    // As composition does, the batch's bundled source is a known provider.
    let mut snapshot = host.validation_snapshot();
    snapshot.providers.push(plugin::ProviderRegistration {
        id: "vosu.osu".into(),
        official: true,
        builtin: true,
        replaces: Vec::new(),
        revision: None,
    });
    let loaded = plugin::load_bundled(
        &semver::Version::parse("2.0.0-alpha.1").unwrap(),
        &snapshot,
        &[
            plugin::BundledPlugin {
                manifest: source_manifest,
                settings_schema: None,
                namespace_schemas: &[include_str!(
                    "../../../../docs/v2/schemas/vosu.osu.schema.json"
                )],
                action_schemas: &[],
                vtable: || vosu_plugin_api::__vtable_for_serialized::<Source>(),
            },
            plugin::BundledPlugin {
                manifest: MANIFEST,
                settings_schema: None,
                namespace_schemas: &[],
                action_schemas: &[],
                vtable: vosu_plugin_vtable,
            },
        ],
    )
    .unwrap();
    for (index, loaded) in loaded.iter().enumerate() {
        host.create_instance(
            PluginInstanceId::new(index as u64 + 1),
            loaded,
            root.path().into(),
            None,
        )
        .unwrap();
    }
    host.process_appeared(plugin::ProcessRecord {
        pid: 7913,
        name: "fixture.exe".into(),
        path: "/fixture/fixture.exe".into(),
        command_line: "fixture.exe".into(),
        argv: Some(vec!["fixture.exe".into()]),
        bitness: vosu_plugin_api::Bitness::X86,
        start_time: 1973,
    });
    let requester = host.surface_requester(PluginInstanceId::new(2)).unwrap();
    let view = runtime.block_on(async {
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if let Ok(view) = view::View::capture(requester.as_ref(), SessionTarget::Focused) {
                    break view;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("real core capture readiness")
    });
    assert_eq!(view.play_time, 61234.5);
    assert_eq!(view.live_stars, 7.21);
    assert_eq!(view.current_bpm, 187.1234);
    assert!(view.capture.values.iter().any(|v| v.pointer == "/beatmap"));
    assert!(
        !view
            .capture
            .values
            .iter()
            .any(|v| v.pointer == "/beatmap/time/live" || v.pointer == "/beatmap/stats")
    );
    let wire = parity(&view, None);
    assert_eq!(wire["time"], 61.2345);
    assert_eq!(wire["liveStarRating"], 7.21);
    assert_eq!(wire["currentBpm"], 187.1234);
    let mut overlapping = view::selectors();
    overlapping.push(vosu_plugin_api::Selector {
        ns: "vosu.osu".into(),
        pointer: "/beatmap/time/live".into(),
    });
    assert!(
        requester
            .capture(SessionTarget::Focused, &overlapping)
            .is_err(),
        "positive control: core must reject the former overlapping selector set"
    );
    let registry = host.state().registry.read().unwrap();
    let parent = registry.lanes("vosu.osu", &["beatmap"]);
    for path in [&["beatmap", "time", "live"][..], &["beatmap", "stats"][..]] {
        assert_eq!(
            parent.union(registry.lanes("vosu.osu", path)),
            parent,
            "parent capture must retain every descendant sampling lane"
        );
    }
    assert!(
        parent.regular_always
            || parent.regular_gameplay
            || parent.precise_always
            || parent.precise_gameplay,
        "the lane comparison must not be vacuous"
    );
    drop(registry);
    host.destroy_all(Duration::from_secs(5));
}

#[test]
fn production_selector_set_has_no_duplicates_or_ancestor_overlap() {
    let selectors = view::selectors();
    for (index, left) in selectors.iter().enumerate() {
        for right in &selectors[index + 1..] {
            if left.ns != right.ns {
                continue;
            }
            assert_ne!(left.pointer, right.pointer);
            assert!(
                !left
                    .pointer
                    .strip_prefix(&right.pointer)
                    .is_some_and(|tail| tail.starts_with('/'))
                    && !right
                        .pointer
                        .strip_prefix(&left.pointer)
                        .is_some_and(|tail| tail.starts_with('/')),
                "overlapping production selectors: {left:?}, {right:?}"
            );
        }
    }
}
