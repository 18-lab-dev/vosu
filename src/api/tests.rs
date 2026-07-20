use std::sync::Arc;

use bytes::Bytes;

use crate::instance::{InstanceSettings, Pid};
use crate::model::enums::ClientType;
use crate::model::states::{HitErrorArray, KeypressButton};

use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread;

use super::*;
use crate::state::{
    BassDensityState, BeatmapPPState, GameplayState, GlobalState, LazerMultiSpectatingState,
    MenuState, PreciseState, RankedPlayState, RegularState, ResultScreenState, SettingsState,
    StateBundleSnapshot, TourneyManagerState, UserState,
};
use std::ffi::OsString;

fn regular(play_time: f64) -> Arc<RegularState> {
    let global = GlobalState {
        play_time,
        ..GlobalState::default()
    };
    Arc::new(RegularState {
        global,
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
    })
}

fn precise(play_time: f64) -> Arc<PreciseState> {
    Arc::new(PreciseState {
        status: 0,
        play_time,
        mode: 0,
        key_overlay: Arc::new(Vec::new()),
        keypresses: Arc::new(Vec::new()),
        hit_errors: Arc::new(HitErrorArray::default()),
        total_hit_errors: 0.0,
        committed_hit_error_index: 0,
        unstable_rate: 0.0,
    })
}

fn bundle(
    generation: u64,
    regular: Arc<RegularState>,
    precise: Arc<PreciseState>,
) -> Arc<StateBundleSnapshot> {
    Arc::new(StateBundleSnapshot {
        source: ClientType::Stable,
        generation,
        reset_epoch: generation / 10,
        regular,
        precise,
    })
}

fn entry(pid: Pid, state: Arc<StateBundleSnapshot>) -> ManagerInstanceSnapshot {
    ManagerInstanceSnapshot::new(
        pid,
        InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: None::<OsString>,
        },
        false,
        state,
    )
}

fn manager(pid: Pid, state: Arc<StateBundleSnapshot>) -> Arc<ManagerSnapshot> {
    Arc::new(ManagerSnapshot::new(
        state.generation,
        Some(pid),
        true,
        ApiRuntimeSettings::default(),
        [entry(pid, state)],
    ))
}

#[test]
fn construction_preserves_manager_and_independent_state_arc_identity() {
    let regular = regular(17.0);
    let precise = precise(17.0);
    let state = bundle(17, Arc::clone(&regular), Arc::clone(&precise));
    let manager = manager(42, Arc::clone(&state));
    let snapshot = CombinedSnapshot::from_manager(Arc::clone(&manager));

    assert!(Arc::ptr_eq(snapshot.manager(), &manager));
    assert!(Arc::ptr_eq(snapshot.focused().expect("focus"), &state));
    assert!(Arc::ptr_eq(snapshot.regular().expect("regular"), &regular));
    assert!(Arc::ptr_eq(snapshot.precise().expect("precise"), &precise));
    assert_eq!(snapshot.generation(), Some(17));
    assert_eq!(snapshot.reset_epoch(), Some(1));
    assert_eq!(snapshot.source(), Some(ClientType::Stable));
}

#[test]
fn focused_client_and_combined_source_share_one_state_identity() {
    let stable = bundle(4, regular(4.0), precise(4.0));
    let lazer = Arc::new(StateBundleSnapshot {
        source: ClientType::Lazer,
        generation: stable.generation,
        reset_epoch: stable.reset_epoch,
        regular: Arc::clone(&stable.regular),
        precise: Arc::clone(&stable.precise),
    });
    let manager = manager(77, lazer);
    let snapshot = CombinedSnapshot::from_manager(manager);

    assert_eq!(snapshot.manager().focused_client(), Some(ClientType::Lazer));
    assert_eq!(snapshot.source(), Some(ClientType::Lazer));
    assert_eq!(
        snapshot
            .manager()
            .focused_instance()
            .expect("focus")
            .client(),
        ClientType::Lazer
    );
}

#[test]
fn manager_entries_are_pid_ordered_with_exact_role_metadata() {
    let first_state = bundle(1, regular(1.0), precise(1.0));
    let second_state = bundle(2, regular(2.0), precise(2.0));
    let spectator = ManagerInstanceSnapshot::new(
        20,
        InstanceSettings {
            tournament_spectator: true,
            tournament_ipc_id: Some(3.0),
            devserver_endpoint: Some(OsString::from("127.0.0.1:24051")),
        },
        false,
        second_state,
    );
    let tournament_manager = ManagerInstanceSnapshot::new(
        5,
        InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: None,
        },
        true,
        first_state,
    );
    let manager = ManagerSnapshot::new(
        9,
        Some(5),
        true,
        ApiRuntimeSettings::default(),
        [spectator, tournament_manager],
    );

    assert_eq!(
        manager.instances().keys().copied().collect::<Vec<_>>(),
        [5, 20]
    );
    assert!(manager.instances()[&5].is_tournament_manager());
    assert!(manager.instances()[&20].settings().tournament_spectator);
    assert_eq!(
        manager.instances()[&20].settings().tournament_ipc_id,
        Some(3.0)
    );
    assert_eq!(
        manager.instances()[&20].settings().devserver_endpoint,
        Some(OsString::from("127.0.0.1:24051"))
    );
}

#[test]
fn explicit_no_focus_is_observable() {
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::not_ready(
        0,
        ApiRuntimeSettings::default(),
    )));
    assert!(!snapshot.manager().is_ready());
    assert_eq!(snapshot.manager().focused_pid(), None);
    assert_eq!(snapshot.manager().focused_client(), None);
    assert!(snapshot.focused().is_none());
    assert!(snapshot.regular().is_none());
    assert!(snapshot.precise().is_none());
}

#[test]
fn concurrent_success_and_failure_each_encode_once() {
    for schema in ApiSchema::ALL {
        for expected in [
            Ok(Bytes::from_static(b"{}")),
            Err(ApiError::Serialization("fixture".into())),
        ] {
            let snapshot = Arc::new(CombinedSnapshot::from_manager(Arc::new(
                ManagerSnapshot::not_ready(0, ApiRuntimeSettings::default()),
            )));
            let calls = Arc::new(AtomicUsize::new(0));
            let mut workers = Vec::new();
            for _ in 0..24 {
                let snapshot = Arc::clone(&snapshot);
                let calls = Arc::clone(&calls);
                let expected = expected.clone();
                workers.push(thread::spawn(move || {
                    snapshot.encode_unfiltered(schema, |_| {
                        calls.fetch_add(1, Ordering::SeqCst);
                        thread::yield_now();
                        expected
                    })
                }));
            }
            for worker in workers {
                assert_eq!(worker.join().expect("cache worker"), expected);
            }
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }
}

#[test]
fn panicking_encoder_is_retried_without_poisoning_cache() {
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::not_ready(
        0,
        ApiRuntimeSettings::default(),
    )));
    let calls = AtomicUsize::new(0);
    let panic = catch_unwind(AssertUnwindSafe(|| {
        snapshot.encode_unfiltered(ApiSchema::V2Precise, |_| {
            calls.fetch_add(1, Ordering::SeqCst);
            panic!("fixture panic")
        })
    }));
    assert!(panic.is_err());
    let result = snapshot.encode_unfiltered(ApiSchema::V2Precise, |_| {
        calls.fetch_add(1, Ordering::SeqCst);
        Ok(Bytes::from_static(b"retry"))
    });
    assert_eq!(result, Ok(Bytes::from_static(b"retry")));
    assert_eq!(calls.load(Ordering::SeqCst), 2);
}

#[test]
fn component_or_manager_changes_invalidate_dependent_caches() {
    let regular_one = regular(1.0);
    let precise_one = precise(1.0);
    let state_one = bundle(1, Arc::clone(&regular_one), Arc::clone(&precise_one));
    let manager_one = manager(11, state_one);
    let snapshots = SnapshotManager::new(Arc::clone(&manager_one));
    let first = snapshots.load();
    for schema in ApiSchema::ALL {
        assert_eq!(
            first.encode_unfiltered(schema, |_| Ok(Bytes::from_static(b"first"))),
            Ok(Bytes::from_static(b"first"))
        );
    }

    // A polling pass may rematerialize semantically identical manager data.
    let equal_manager = Arc::new(ManagerSnapshot::new(
        manager_one.revision(),
        Some(11),
        true,
        ApiRuntimeSettings::default(),
        manager_one.instances().values().cloned(),
    ));
    let equal_update = snapshots.publish(equal_manager);
    assert!(
        ApiSchema::ALL
            .iter()
            .all(|schema| equal_update.cached(*schema).is_some())
    );

    let settings_change = Arc::new(ManagerSnapshot::new(
        manager_one.revision() + 1,
        Some(11),
        true,
        ApiRuntimeSettings {
            enable_key_overlay: false,
        },
        manager_one.instances().values().cloned(),
    ));
    let settings_update = snapshots.publish(settings_change);
    assert!(!settings_update.runtime_settings().enable_key_overlay);
    assert!(
        ApiSchema::ALL
            .iter()
            .all(|schema| settings_update.cached(*schema).is_none())
    );
    for schema in ApiSchema::ALL {
        assert_eq!(
            settings_update.encode_unfiltered(schema, |_| Ok(Bytes::from_static(b"settings"))),
            Ok(Bytes::from_static(b"settings"))
        );
    }

    // A semantic focus change advances the revision and invalidates caches.
    let manager_focus_change = Arc::new(ManagerSnapshot::new(
        manager_one.revision() + 2,
        Some(11),
        false,
        ApiRuntimeSettings::default(),
        manager_one.instances().values().cloned(),
    ));
    let focus_update = snapshots.publish(manager_focus_change);
    assert!(Arc::ptr_eq(
        focus_update.regular().expect("regular"),
        &regular_one
    ));
    assert!(Arc::ptr_eq(
        focus_update.precise().expect("precise"),
        &precise_one
    ));
    assert!(
        ApiSchema::ALL
            .iter()
            .all(|schema| focus_update.cached(*schema).is_none())
    );

    let precise_two = precise(2.0);
    let precise_state = bundle(2, Arc::clone(&regular_one), Arc::clone(&precise_two));
    let precise_update = snapshots.publish(manager(11, precise_state));
    assert!(Arc::ptr_eq(
        precise_update.regular().expect("regular"),
        &regular_one
    ));
    assert!(
        ApiSchema::ALL
            .iter()
            .all(|schema| precise_update.cached(*schema).is_none())
    );

    let regular_two = regular(3.0);
    let regular_state = bundle(3, Arc::clone(&regular_two), Arc::clone(&precise_two));
    let regular_update = snapshots.publish(manager(11, regular_state));
    assert!(Arc::ptr_eq(
        regular_update.precise().expect("precise"),
        &precise_two
    ));
    assert!(
        ApiSchema::ALL
            .iter()
            .all(|schema| regular_update.cached(*schema).is_none())
    );

    let no_focus = snapshots.publish(Arc::new(ManagerSnapshot::not_ready(
        4,
        ApiRuntimeSettings::default(),
    )));
    assert_eq!(no_focus.manager().revision(), 4);
    assert!(!no_focus.manager().is_ready());
    assert!(no_focus.focused().is_none());
    assert!(no_focus.regular().is_none());
    assert!(no_focus.precise().is_none());
    assert!(
        ApiSchema::ALL
            .iter()
            .all(|schema| no_focus.cached(*schema).is_none())
    );
}

#[test]
fn readers_never_observe_mixed_generation_or_manager_components() {
    let initial = bundle(0, regular(0.0), precise(0.0));
    let snapshots = Arc::new(SnapshotManager::new(manager(7, initial)));
    let publisher = {
        let snapshots = Arc::clone(&snapshots);
        thread::spawn(move || {
            for generation in 1..=2_000_i64 {
                let state = bundle(
                    generation as u64,
                    regular(generation as f64),
                    precise(generation as f64),
                );
                snapshots.publish(manager(7, state));
            }
        })
    };

    for _ in 0..8_000 {
        let snapshot = snapshots.load();
        let generation = snapshot.generation().expect("generation");
        assert_eq!(
            snapshot.regular().expect("regular").global.play_time,
            generation as f64
        );
        assert_eq!(
            snapshot.precise().expect("precise").play_time,
            generation as f64
        );
        assert_eq!(
            snapshot
                .manager()
                .focused_instance()
                .expect("entry")
                .state()
                .generation,
            generation
        );
    }
    publisher.join().expect("publisher");
}

#[test]
fn publication_retains_only_latest_snapshot() {
    let initial = bundle(1, regular(1.0), precise(1.0));
    let snapshots = SnapshotManager::new(manager(7, initial));
    let old = snapshots.load();
    let old_weak = Arc::downgrade(&old);
    drop(old);

    for generation in 2..=1_000_i64 {
        let state = bundle(
            generation as u64,
            regular(generation as f64),
            precise(generation as f64),
        );
        snapshots.publish(manager(7, state));
    }

    assert!(old_weak.upgrade().is_none());
    assert_eq!(snapshots.load().generation(), Some(1_000));
}

fn encode_schema(schema: ApiSchema, snapshot: &CombinedSnapshot) -> Result<Bytes, ApiError> {
    match schema {
        ApiSchema::V1 => v1::encode(snapshot),
        ApiSchema::V2 => v2::encode(snapshot),
        ApiSchema::StreamCompanion => sc::encode(snapshot),
        ApiSchema::V2Precise => precise::encode(snapshot),
    }
}

fn schema_index(schema: ApiSchema) -> usize {
    match schema {
        ApiSchema::V1 => 0,
        ApiSchema::V2 => 1,
        ApiSchema::StreamCompanion => 2,
        ApiSchema::V2Precise => 3,
    }
}

#[test]
fn synthetic_live_state_worker_smoke_encodes_every_transition() {
    use crate::model::enums::GameState;
    use crate::model::states::KeyOverlayButton;

    let snapshots = Arc::new(SnapshotManager::new(Arc::new(ManagerSnapshot::not_ready(
        0,
        ApiRuntimeSettings::default(),
    ))));
    let worker = {
        let snapshots = Arc::clone(&snapshots);
        thread::spawn(move || {
            let transitions = [
                (ClientType::Stable, GameState::Menu, false, false),
                (ClientType::Stable, GameState::Play, false, false),
                (ClientType::Stable, GameState::Play, false, true),
                (ClientType::Stable, GameState::Play, true, false),
                (ClientType::Stable, GameState::ResultScreen, false, false),
                (ClientType::Stable, GameState::Lobby, false, false),
                (ClientType::Lazer, GameState::Menu, false, false),
                (ClientType::Lazer, GameState::Play, false, false),
                (ClientType::Lazer, GameState::Play, false, true),
                (ClientType::Lazer, GameState::Play, true, false),
                (ClientType::Lazer, GameState::ResultScreen, false, false),
                (ClientType::Lazer, GameState::Lobby, false, false),
            ];
            let mut prior_regular = None;
            let mut histories: [Vec<Bytes>; 4] = std::array::from_fn(|_| Vec::new());
            let mut changed = [0_usize; 4];

            for (offset, (source, status, paused, precise_only)) in
                transitions.into_iter().enumerate()
            {
                let generation = offset as u64 + 1;
                let play_time = generation as f64 * 100.0;
                let regular = if precise_only {
                    Arc::clone(prior_regular.as_ref().expect("prior regular state"))
                } else {
                    let mut state = (*regular(play_time)).clone();
                    state.global.status = i64::from(status.value());
                    state.global.paused = paused;
                    state.global.game_time = play_time as i64;
                    state.global.play_time = play_time;
                    state.menu.gamemode = Some(0);
                    state.gameplay.mode = 0;
                    state.result_screen.raw.mode = 0;
                    let state = Arc::new(state);
                    prior_regular = Some(Arc::clone(&state));
                    state
                };
                let precise = Arc::new(PreciseState {
                    status: i64::from(status.value()),
                    play_time,
                    mode: 0,
                    key_overlay: Arc::new(vec![KeyOverlayButton {
                        name: "K1".into(),
                        is_pressed: generation.is_multiple_of(2),
                        count: generation as i64,
                    }]),
                    keypresses: Arc::new(vec![KeypressButton {
                        name: "K1".into(),
                        is_pressed: generation.is_multiple_of(2),
                        count: generation as i64,
                        pressed_at: generation.is_multiple_of(2).then_some(play_time),
                        released_at: None,
                    }]),
                    hit_errors: Arc::new(HitErrorArray {
                        all: vec![generation as f64],
                        press: vec![generation as f64],
                        release: Vec::new(),
                    }),
                    total_hit_errors: generation as f64,
                    committed_hit_error_index: generation as i64,
                    unstable_rate: generation as f64,
                });
                let state = Arc::new(StateBundleSnapshot {
                    source,
                    generation,
                    reset_epoch: u64::from(matches!(
                        status,
                        GameState::Menu | GameState::ResultScreen | GameState::Lobby
                    )),
                    regular,
                    precise,
                });
                let pid = if source == ClientType::Stable { 7 } else { 8 };
                let snapshot = snapshots.publish(manager(pid, state));
                assert_eq!(snapshot.source(), Some(source));
                assert_eq!(snapshot.generation(), Some(generation));

                for schema in ApiSchema::ALL {
                    let first = encode_schema(schema, &snapshot).unwrap_or_else(|error| {
                        panic!("{schema:?} generation {generation}: {error}")
                    });
                    let second = encode_schema(schema, &snapshot).unwrap_or_else(|error| {
                        panic!("{schema:?} cached generation {generation}: {error}")
                    });
                    assert_eq!(first.as_ptr(), second.as_ptr());
                    serde_json::from_slice::<serde_json::Value>(&first)
                        .unwrap_or_else(|error| panic!("{schema:?} invalid JSON: {error}"));
                    let index = schema_index(schema);
                    if histories[index]
                        .last()
                        .is_some_and(|previous| previous != &first)
                    {
                        changed[index] += 1;
                    }
                    histories[index].push(first);
                }
            }

            for (schema, changes) in ApiSchema::ALL.into_iter().zip(changed) {
                assert!(
                    changes >= 9,
                    "{schema:?} changed only {changes} times across live transitions"
                );
            }
        })
    };
    worker
        .join()
        .expect("synthetic state worker must not panic");

    let exited = snapshots.publish(Arc::new(ManagerSnapshot::not_ready(
        13,
        ApiRuntimeSettings::default(),
    )));
    assert!(!exited.manager().is_ready());
    for schema in ApiSchema::ALL {
        assert_eq!(encode_schema(schema, &exited), Err(ApiError::NotReady));
    }
    assert_eq!(ApiError::NotReady.to_string(), "osu is not ready/running");
}
