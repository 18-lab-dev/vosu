use std::sync::{Arc, RwLock};
use vosu_core::state::{
    Audience, ClientFacts, InternalGrant, Pointer, PublisherGrant, SchemaRegistry, SessionKey,
    StateStore, project_select,
};
use vosu_osu::project::{Caches, DiffSink, LastValues, Projector, SessionFacts};
use vosu_osu_model::{
    enums::ClientType,
    launch::InstanceSettings,
    states::{HitErrorArray, KeyOverlayButton, KeypressButton},
};
use vosu_osu_state::*;
use vosu_plugin_api::{
    PluginManifest, PublishBatchWriter, Selector, StateEntry, decode_publish_batch_ref,
};
use vosu_surface::state::{CapturedSelection, IdentifiedRoster, IdentifiedSession};

pub(super) fn source() -> StateBundleSnapshot {
    let mut regular = RegularState {
        global: GlobalState::default(),
        settings: SettingsState::default(),
        beatmap: BeatmapPPState::default(),
        menu: MenuState::default(),
        bass_density: BassDensityState::default(),
        gameplay: GameplayState::default(),
        result_screen: ResultScreenState::default(),
        tourney: TourneyManagerState::default(),
        user: UserState::default(),
        lazer_multi_spectating: LazerMultiSpectatingState::default(),
        ranked_play: RankedPlayState::default(),
        room: Default::default(),
    };
    regular.gameplay.score = 123456.789;
    regular.gameplay.accuracy = 98.123456789;
    regular.gameplay.combo = 23;
    regular.gameplay.max_combo = 47;
    regular.gameplay.player_hp = 123.456789;
    regular.gameplay.player_hp_smooth = 87.654321;
    regular.gameplay.statistics.miss = 1;
    regular.gameplay.statistics.meh = 2;
    regular.gameplay.statistics.ok = 3;
    regular.gameplay.statistics.good = 4;
    regular.gameplay.statistics.great = 5;
    regular.gameplay.statistics.perfect = 6;
    regular.gameplay.statistics.small_tick_hit = 7;
    regular.gameplay.statistics.large_tick_hit = 8;
    regular.gameplay.statistics.slider_tail_hit = 9;
    regular.gameplay.slider_breaks = 10;
    regular.global.game_folder = "/private-game-positive-control".into();
    regular.global.songs_folder = "/private-songs-positive-control".into();
    regular.global.skin_folder = "/private-skin-positive-control".into();
    regular.menu.folder = Some("folder/../distinct-chart".into());
    regular.menu.filename = Some("a \" unicode🎵.osu".into());
    regular.menu.background_filename = Some("background.jpg".into());
    regular.menu.audio_filename = Some("song.mp3".into());
    let precise = PreciseState {
        status: 2,
        play_time: 1234.5,
        mode: 0,
        key_overlay: Arc::new(
            [11, 22, 33, 44]
                .into_iter()
                .enumerate()
                .map(|(index, count)| KeyOverlayButton {
                    name: format!("physical-{index}").into(),
                    is_pressed: index % 2 == 0,
                    count,
                })
                .collect(),
        ),
        keypresses: Arc::new(vec![KeypressButton {
            name: "different-logical".into(),
            is_pressed: true,
            count: 999,
            pressed_at: Some(345.0),
            released_at: Some(678.0),
        }]),
        hit_errors: Arc::new(HitErrorArray {
            all: vec![-7.5, 2.25],
            press: vec![31.0],
            release: vec![42.0],
            key: None,
            offsets: Vec::new(),
        }),
        catcher: None,
        total_hit_errors: 0.0,
        committed_hit_error_index: 0,
        unstable_rate: 17.0,
    };
    StateBundleSnapshot {
        source: ClientType::Stable,
        generation: 71,
        reset_epoch: 0,
        regular: Arc::new(regular),
        precise: Arc::new(precise),
    }
}

pub(super) fn capture(snapshot: &StateBundleSnapshot, selectors: &[Selector]) -> CapturedSelection {
    let mut registry = SchemaRegistry::with_core().unwrap();
    registry
        .register("vosu.osu", "vosu.osu", vosu_osu::OSU_NAMESPACE_SCHEMA)
        .unwrap();
    let registry = Arc::new(RwLock::new(registry));
    let store = StateStore::new(registry.clone());
    let manifest = PluginManifest::parse(vosu_osu::MANIFEST).unwrap();
    let grant = PublisherGrant::from_manifest(&manifest).unwrap();
    let variant = if snapshot.source == ClientType::Stable {
        "stable"
    } else {
        "lazer"
    };
    store.open_session(
        SessionKey(81),
        &grant,
        ClientFacts {
            game: "osu".into(),
            variant: variant.into(),
            name: "osu!".into(),
            version: "test".into(),
            pid: 1234,
            session: "fixture-session".into(),
            plugin_id: manifest.id,
            plugin_version: manifest.version,
        },
        true,
    );
    let mut writer = PublishBatchWriter::new();
    writer.begin(81);
    Projector::default().project(
        snapshot,
        &SessionFacts {
            client: snapshot.source,
            settings: InstanceSettings {
                tournament_spectator: false,
                tournament_ipc_id: None,
                devserver_endpoint: None,
            },
            foreground: Some(false),
        },
        &mut Caches::default(),
        &mut DiffSink::new(&mut LastValues::default(), &mut writer),
    );
    let mut bytes = Vec::new();
    writer.finish_into(&mut bytes).unwrap();
    store
        .commit(
            &grant,
            SessionKey(81),
            decode_publish_batch_ref(&bytes).unwrap().entries(),
        )
        .unwrap();
    let state = store.current(SessionKey(81)).unwrap();
    let registry = registry.read().unwrap();
    let internal = InternalGrant::from_subtrees(vec![Pointer::parse("/internal/paths").unwrap()]);
    let values: Vec<_> = selectors
        .iter()
        .map(|selector| StateEntry {
            ns: selector.ns.clone(),
            pointer: selector.pointer.clone(),
            value: project_select(
                &registry,
                &state,
                &selector.ns,
                &Pointer::parse(&selector.pointer).unwrap(),
                Audience::Plugin(&internal),
            )
            .map(|value| value.to_cbor_value()),
            error: None,
        })
        .collect();
    let session = IdentifiedSession {
        session: "fixture-session".into(),
        generation: 9,
        owner: "vosu.osu".into(),
        owner_instance: 5,
        game: "osu".into(),
        variant: variant.into(),
        foreground: Some(false),
    };
    let other = IdentifiedSession {
        session: "foreground-other".into(),
        generation: 10,
        foreground: Some(true),
        ..session.clone()
    };
    CapturedSelection {
        session: Some(session.clone()),
        focus_revision: 6,
        roster: IdentifiedRoster {
            revision: 12,
            sessions: Arc::from([session, other]),
        },
        revision: 7,
        namespace_revisions: Arc::from([("vosu.osu".into(), 7)]),
        values: values.into(),
    }
}

pub(super) fn replace(capture: &mut CapturedSelection, pointer: &str, value: serde_json::Value) {
    let value = ciborium::Value::serialized(&value).unwrap();
    let entries = Arc::make_mut(&mut capture.values);
    let entry = entries
        .iter_mut()
        .find(|entry| entry.ns == "vosu.osu" && entry.pointer == pointer)
        .unwrap();
    entry.value = Some(value);
}
