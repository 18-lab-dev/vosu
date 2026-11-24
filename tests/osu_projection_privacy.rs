#![cfg(feature = "test-harness")]
//! The real osu! projection's private path payload must never reach Api audiences.
use std::sync::Arc;
use vosu_core::state::{
    Audience, InternalGrant, Pointer, SessionKey, SessionState, StateObject, StateValue,
    project_document, project_namespace, project_select, schema::SchemaRegistry,
};
use vosu_osu::project::{Caches, OsuPointer, ProjectionSink, Projector, SessionFacts};
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings, states::HitErrorArray};
use vosu_osu_state::*;
use vosu_plugin_api::PublishValueRef;
#[derive(Default)]
struct Paths(Option<StateValue>, usize);
const SENTINELS: &[&str] = &[
    "/private/osu",
    "/private/songs",
    "/private/skin",
    "private-map",
    "private.osu",
    "private.jpg",
    "private.mp3",
];
fn assert_public_value(at: OsuPointer, bytes: &[u8]) {
    if at.ns() == "vosu.osu"
        && (at.pointer() == "/internal" || at.pointer().starts_with("/internal/"))
    {
        return;
    }
    for sentinel in SENTINELS {
        assert!(
            !bytes
                .windows(sentinel.len())
                .any(|w| w == sentinel.as_bytes()),
            "{}{} leaks {sentinel}",
            at.ns(),
            at.pointer()
        );
    }
}
impl ProjectionSink for Paths {
    fn put(&mut self, at: OsuPointer, value: PublishValueRef<'_>) {
        self.1 += 1;
        match value {
            PublishValueRef::String(s) => assert_public_value(at, s.as_bytes()),
            PublishValueRef::Cbor(bytes) => {
                let mut json = Vec::new();
                StateValue::from_cbor(bytes).unwrap().write_json(&mut json);
                assert_public_value(at, &json);
            }
            PublishValueRef::Bytes(bytes)
            | PublishValueRef::ResourceRef(bytes)
            | PublishValueRef::TreeRef(bytes) => assert_public_value(at, bytes),
            _ => {}
        }
    }
    fn put_cbor(&mut self, at: OsuPointer, write: &mut dyn FnMut(&mut Vec<u8>)) {
        let mut bytes = vec![];
        write(&mut bytes);
        let value = StateValue::from_cbor(&bytes).unwrap();
        let mut json = Vec::new();
        value.write_json(&mut json);
        assert_public_value(at, &json);
        if at == OsuPointer::OsuInternalPaths {
            self.0 = Some(value);
        } else {
            self.1 += 1;
        }
    }
}
fn object(fields: Vec<(&str, StateValue)>) -> StateValue {
    StateValue::Object(Arc::new(StateObject::new(
        fields.into_iter().map(|(k, v)| (Arc::from(k), v)).collect(),
    )))
}
fn snapshot() -> StateBundleSnapshot {
    let mut menu = MenuState::default();
    menu.folder = Some("private-map".into());
    menu.filename = Some("private.osu".into());
    menu.background_filename = Some("private.jpg".into());
    menu.audio_filename = Some("private.mp3".into());
    StateBundleSnapshot {
        source: ClientType::Stable,
        generation: 1,
        reset_epoch: 0,
        regular: Arc::new(RegularState {
            global: GlobalState {
                game_folder: "/private/osu".into(),
                songs_folder: "/private/songs".into(),
                skin_folder: "/private/skin".into(),
                ..Default::default()
            },
            settings: SettingsState::default(),
            beatmap: BeatmapPPState::default(),
            menu,
            bass_density: BassDensityState::default(),
            gameplay: GameplayState::default(),
            result_screen: ResultScreenState::default(),
            tourney: TourneyManagerState::default(),
            user: UserState::default(),
            lazer_multi_spectating: LazerMultiSpectatingState::default(),
            ranked_play: RankedPlayState::default(),
            room: Default::default(),
        }),
        precise: Arc::new(PreciseState {
            catcher: None,
            status: 0,
            play_time: 0.0,
            mode: 0,
            key_overlay: Arc::new(vec![]),
            keypresses: Arc::new(vec![]),
            hit_errors: Arc::new(HitErrorArray::default()),
            total_hit_errors: 0.0,
            committed_hit_error_index: 0,
            unstable_rate: 0.0,
        }),
    }
}
fn grant(cap: &str) -> InternalGrant {
    InternalGrant::from_manifest(&vosu_plugin_api::manifest::PackageManifest::parse(&format!("kind='plugin'\nmanifest_version=1\nid='acme.privacy'\nname='Privacy'\nversion='1.0.0'\nhost='>=2.0.0-alpha.1, <3.0.0'\nabi_version=1\nruntime='in-process'\n[[requires]]\nid='field:{cap}'\nversion='*'\n")).and_then(vosu_plugin_api::manifest::PackageManifest::into_plugin).unwrap())
}
#[test]
fn actual_osu_paths_are_visible_only_to_explicit_internal_grant() {
    let mut registry = SchemaRegistry::with_core().unwrap();
    registry
        .register_versioned(
            "vosu.osu",
            "vosu.osu",
            &semver::Version::new(1, 2, 0),
            vosu_plugin_api::manifest::schema::SchemaProvenance::TrustedOfficial,
            vosu_osu::OSU_NAMESPACE_SCHEMA,
        )
        .unwrap();
    for client in [ClientType::Stable, ClientType::Lazer] {
        let facts = SessionFacts {
            client,
            settings: InstanceSettings {
                tournament_spectator: false,
                tournament_ipc_id: None,
                devserver_endpoint: None,
            },
            foreground: None,
        };
        let mut paths = Paths::default();
        Projector::default().project(&snapshot(), &facts, &mut Caches::default(), &mut paths);
        assert!(
            paths.1 > 0,
            "privacy check must inspect public projected pointers"
        );
        let ns = object(vec![(
            "internal",
            object(vec![("paths", paths.0.unwrap())]),
        )]);
        let state = SessionState::new(
            SessionKey(905),
            1,
            vec![(Arc::from("vosu.osu"), ns.clone())],
        );
        for pointer in [
            "/internal",
            "/internal/paths",
            "/internal/paths/stable/game",
            "/internal/paths/lazer/data",
            "/internal/paths/current/beatmapFile",
            "/internal/paths/skinFolder",
        ] {
            assert!(
                project_select(
                    &registry,
                    &state,
                    "vosu.osu",
                    &Pointer::parse(pointer).unwrap(),
                    Audience::Api
                )
                .is_none(),
                "{pointer}"
            )
        }
        let root = Pointer::root();
        let projected =
            project_select(&registry, &state, "vosu.osu", &root, Audience::Api).unwrap();
        assert!(projected.get(&["internal"]).is_none());
        assert!(
            project_namespace(&registry, "vosu.osu", &ns, Audience::Api)
                .get(&["internal"])
                .is_none()
        );
        let doc = project_document(&registry, &state, Audience::Api);
        assert!(doc[0].1.get(&["internal"]).is_none());
        let bytes = state.api_json(&registry);
        assert!(!bytes.windows(7).any(|w| w == b"private"));
        assert!(!bytes.windows(8).any(|w| w == b"internal"));
        let whole = grant("vosu.osu");
        let exact = grant("vosu.osu.internal.paths");
        let pointer = Pointer::parse("/internal/paths").unwrap();
        assert!(
            project_select(
                &registry,
                &state,
                "vosu.osu",
                &pointer,
                Audience::Plugin(&whole)
            )
            .is_none()
        );
        let visible = project_select(
            &registry,
            &state,
            "vosu.osu",
            &pointer,
            Audience::Plugin(&exact),
        )
        .unwrap();
        assert_eq!(visible.as_ref(), ns.get(&["internal", "paths"]).unwrap());
        let current = visible.get(&["current", "beatmapFile"]).unwrap();
        assert_eq!(current, &StateValue::String(Arc::from("private.osu")));
    }
}
