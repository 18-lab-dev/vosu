//! Source-compatible `/json/v2/precise` wire builder.

use std::cmp::Ordering;
use std::sync::Arc;

use bytes::Bytes;

use crate::api::{ApiError, ApiSchema, CombinedSnapshot, ManagerSnapshot};
use crate::model::api::v2::{
    KeyOverlay, KeyOverlayButton, Keypress, Keypresses, NumberName, PreciseResponse, PreciseTourney,
};
use crate::model::states::{
    KeyOverlayButton as StateKeyOverlayButton, KeypressButton as StateKeypressButton,
};

/// Builds the exact V2 precise document from one immutable publication point.
pub fn build(snapshot: &CombinedSnapshot) -> Result<PreciseResponse, ApiError> {
    let precise = snapshot.precise().ok_or(ApiError::NotReady)?;

    Ok(PreciseResponse {
        keys: build_keys(&precise.key_overlay),
        keypresses: build_keypresses(precise.mode, &precise.keypresses),
        hit_errors: precise.hit_errors.as_ref().clone(),
        tourney: build_tourney(snapshot.manager()),
    })
}

/// Encodes the unfiltered V2 precise document once for this combined generation.
pub fn encode(snapshot: &CombinedSnapshot) -> Result<Bytes, ApiError> {
    snapshot.encode_unfiltered(ApiSchema::V2Precise, |snapshot| {
        super::v2::encode_dto(&build(snapshot)?)
    })
}

fn build_tourney(manager: &ManagerSnapshot) -> Vec<PreciseTourney> {
    if !manager
        .instances()
        .values()
        .any(|instance| instance.is_tournament_manager())
    {
        return Vec::new();
    }

    let mut spectators: Vec<_> = manager
        .instances()
        .values()
        .filter_map(|instance| {
            let settings = instance.settings();
            if !settings.tournament_spectator {
                return None;
            }

            settings
                .tournament_ipc_id
                .map(|ipc_id| (ipc_id, instance.state().precise.as_ref()))
        })
        .collect();

    // `Array.prototype.sort` is stable and a NaN comparator result behaves as
    // equality. `partial_cmp(None) => Equal` preserves that source behavior.
    spectators.sort_by(|(left, _), (right, _)| left.partial_cmp(right).unwrap_or(Ordering::Equal));

    spectators
        .into_iter()
        .map(|(ipc_id, precise)| PreciseTourney {
            ipc_id,
            keys: build_keys(&precise.key_overlay),
            keypresses: build_keypresses(precise.mode, &precise.keypresses),
            hit_errors: precise.hit_errors.as_ref().clone(),
        })
        .collect()
}

fn build_keypresses(mode: i64, buttons: &[StateKeypressButton]) -> Keypresses {
    Keypresses {
        mode: NumberName {
            number: Some(mode),
            name: crate::api::v2::names::ruleset_name(mode).to_owned(),
        },
        keys: buttons
            .iter()
            .map(|button| Keypress {
                name: Arc::clone(&button.name),
                is_pressed: button.is_pressed,
                count: button.count,
                pressed_at: button.pressed_at,
                released_at: button.released_at,
            })
            .collect(),
    }
}

fn build_keys(buttons: &[StateKeyOverlayButton]) -> KeyOverlay {
    KeyOverlay {
        k1: build_key(buttons.first()),
        k2: build_key(buttons.get(1)),
        m1: build_key(buttons.get(2)),
        m2: build_key(buttons.get(3)),
    }
}

fn build_key(button: Option<&StateKeyOverlayButton>) -> KeyOverlayButton {
    button.map_or(
        KeyOverlayButton {
            is_pressed: false,
            count: 0,
        },
        |button| KeyOverlayButton {
            is_pressed: button.is_pressed,
            count: button.count,
        },
    )
}

#[cfg(test)]
mod tests {
    use std::ffi::OsString;
    use std::sync::Arc;

    use super::*;
    use crate::api::{ApiRuntimeSettings, ManagerInstanceSnapshot, ManagerSnapshot};
    use crate::instance::{InstanceSettings, Pid};
    use crate::model::enums::ClientType;
    use crate::state::{
        BassDensityState, BeatmapPPState, GameplayState, GlobalState, LazerMultiSpectatingState,
        MenuState, PreciseState, RankedPlayState, RegularState, ResultScreenState, SettingsState,
        StateBundleSnapshot, TourneyManagerState, UserState,
    };

    fn regular() -> Arc<RegularState> {
        Arc::new(RegularState {
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
        })
    }
    fn button(name: &str, is_pressed: bool, count: i64) -> StateKeyOverlayButton {
        StateKeyOverlayButton {
            name: name.into(),
            is_pressed,
            count,
        }
    }

    fn precise(keys: Vec<StateKeyOverlayButton>, hit_errors: Vec<f64>) -> Arc<PreciseState> {
        precise_for_mode(0, keys, hit_errors)
    }

    fn precise_for_mode(
        mode: i64,
        keys: Vec<StateKeyOverlayButton>,
        hit_errors: Vec<f64>,
    ) -> Arc<PreciseState> {
        let keypresses = keys
            .iter()
            .map(|key| StateKeypressButton {
                name: Arc::clone(&key.name),
                is_pressed: key.is_pressed,
                count: key.count,
                pressed_at: key.is_pressed.then_some(1234.0),
                released_at: None,
            })
            .collect();
        Arc::new(PreciseState {
            status: 2,
            play_time: 1234.0,
            mode,
            key_overlay: Arc::new(keys),
            keypresses: Arc::new(keypresses),
            hit_errors: Arc::new(hit_errors),
            total_hit_errors: 0.0,
            committed_hit_error_index: 0,
            unstable_rate: 0.0,
        })
    }

    fn bundle(generation: u64, precise: Arc<PreciseState>) -> Arc<StateBundleSnapshot> {
        Arc::new(StateBundleSnapshot {
            source: ClientType::Stable,
            generation,
            reset_epoch: 0,
            regular: regular(),
            precise,
        })
    }

    fn entry(
        pid: Pid,
        state: Arc<StateBundleSnapshot>,
        tournament_manager: bool,
        ipc_id: Option<f64>,
    ) -> ManagerInstanceSnapshot {
        ManagerInstanceSnapshot::new(
            pid,
            InstanceSettings {
                tournament_spectator: ipc_id.is_some(),
                tournament_ipc_id: ipc_id,
                devserver_endpoint: None::<OsString>,
            },
            tournament_manager,
            state,
        )
    }

    fn snapshot(
        focused_pid: Option<Pid>,
        instances: Vec<ManagerInstanceSnapshot>,
    ) -> CombinedSnapshot {
        CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
            1,
            focused_pid,
            true,
            ApiRuntimeSettings::default(),
            instances,
        )))
    }

    #[test]
    fn no_focused_client_is_not_ready() {
        let snapshot = snapshot(None, Vec::new());
        assert_eq!(build(&snapshot), Err(ApiError::NotReady));
        assert_eq!(encode(&snapshot), Err(ApiError::NotReady));
    }

    #[test]
    fn missing_keys_default_and_field_order_is_exact() {
        let focused = bundle(1, precise_for_mode(99, Vec::new(), Vec::new()));
        let snapshot = snapshot(Some(10), vec![entry(10, focused, false, None)]);

        let encoded = encode(&snapshot).expect("precise response");
        assert_eq!(
            std::str::from_utf8(&encoded).expect("UTF-8 JSON"),
            r#"{"keys":{"k1":{"isPressed":false,"count":0},"k2":{"isPressed":false,"count":0},"m1":{"isPressed":false,"count":0},"m2":{"isPressed":false,"count":0}},"keypresses":{"mode":{"number":99,"name":""},"keys":[]},"hitErrors":[],"tourney":[]}"#
        );
    }

    #[test]
    fn focused_updates_are_copied_exactly() {
        let mut precise = precise_for_mode(
            3,
            vec![
                button("K1", true, 11),
                button("K2", false, 12),
                button("M1", true, 13),
                button("M2", false, 14),
                button("K5", true, 15),
                button("K6", false, 16),
            ],
            vec![-7.5, 2.25],
        );
        let legacy = Arc::new(precise.key_overlay[..3].to_vec());
        Arc::make_mut(&mut precise).key_overlay = legacy;
        let focused = bundle(1, precise);
        let snapshot = snapshot(Some(10), vec![entry(10, focused, false, None)]);
        let response = build(&snapshot).expect("precise response");

        assert_eq!(
            response.keys.k1,
            KeyOverlayButton {
                is_pressed: true,
                count: 11
            }
        );
        assert_eq!(
            response.keys.k2,
            KeyOverlayButton {
                is_pressed: false,
                count: 12
            }
        );
        assert_eq!(
            response.keys.m1,
            KeyOverlayButton {
                is_pressed: true,
                count: 13
            }
        );
        assert_eq!(
            response.keys.m2,
            KeyOverlayButton {
                is_pressed: false,
                count: 0
            }
        );
        assert_eq!(
            response.keypresses.mode,
            NumberName {
                number: Some(3),
                name: "mania".to_owned(),
            }
        );
        assert_eq!(
            response
                .keypresses
                .keys
                .iter()
                .map(|key| {
                    (
                        key.name.as_ref(),
                        key.is_pressed,
                        key.count,
                        key.pressed_at,
                        key.released_at,
                    )
                })
                .collect::<Vec<_>>(),
            [
                ("K1", true, 11, Some(1234.0), None),
                ("K2", false, 12, None, None),
                ("M1", true, 13, Some(1234.0), None),
                ("M2", false, 14, None, None),
                ("K5", true, 15, Some(1234.0), None),
                ("K6", false, 16, None, None),
            ]
        );
    }

    #[test]
    fn spectators_are_omitted_without_tournament_manager() {
        let focused = bundle(1, precise(Vec::new(), Vec::new()));
        let spectator = bundle(2, precise(vec![button("K1", true, 9)], vec![3.0]));
        let snapshot = snapshot(
            Some(10),
            vec![
                entry(10, focused, false, None),
                entry(20, spectator, false, Some(7.0)),
            ],
        );

        assert!(
            build(&snapshot)
                .expect("precise response")
                .tourney
                .is_empty()
        );
    }

    #[test]
    fn tournament_spectators_sort_by_ipc_id_not_pid_or_insertion_order() {
        let focused = bundle(1, precise(Vec::new(), Vec::new()));
        let manager = bundle(2, precise(Vec::new(), Vec::new()));
        let ipc_30 = bundle(
            3,
            precise_for_mode(2, vec![button("K1", true, 30)], vec![30.5]),
        );
        let ipc_2 = bundle(
            4,
            precise_for_mode(
                1,
                vec![button("K1", false, 2), button("K2", true, 22)],
                vec![-2.5],
            ),
        );
        let ipc_infinity = bundle(
            5,
            precise_for_mode(3, vec![button("K1", true, 99)], vec![99.0]),
        );
        let snapshot = snapshot(
            Some(50),
            vec![
                entry(50, focused, false, None),
                entry(40, ipc_30, false, Some(30.25)),
                entry(30, manager, true, None),
                entry(60, ipc_2, false, Some(2.5)),
                entry(70, ipc_infinity, false, Some(f64::INFINITY)),
            ],
        );

        let encoded = encode(&snapshot).expect("precise response");
        assert_eq!(
            std::str::from_utf8(&encoded).expect("UTF-8 JSON"),
            r#"{"keys":{"k1":{"isPressed":false,"count":0},"k2":{"isPressed":false,"count":0},"m1":{"isPressed":false,"count":0},"m2":{"isPressed":false,"count":0}},"keypresses":{"mode":{"number":0,"name":"osu"},"keys":[]},"hitErrors":[],"tourney":[{"ipcId":2.5,"keys":{"k1":{"isPressed":false,"count":2},"k2":{"isPressed":true,"count":22},"m1":{"isPressed":false,"count":0},"m2":{"isPressed":false,"count":0}},"keypresses":{"mode":{"number":1,"name":"taiko"},"keys":[{"name":"K1","isPressed":false,"count":2,"pressedAt":null,"releasedAt":null},{"name":"K2","isPressed":true,"count":22,"pressedAt":1234,"releasedAt":null}]},"hitErrors":[-2.5]},{"ipcId":30.25,"keys":{"k1":{"isPressed":true,"count":30},"k2":{"isPressed":false,"count":0},"m1":{"isPressed":false,"count":0},"m2":{"isPressed":false,"count":0}},"keypresses":{"mode":{"number":2,"name":"fruits"},"keys":[{"name":"K1","isPressed":true,"count":30,"pressedAt":1234,"releasedAt":null}]},"hitErrors":[30.5]},{"ipcId":null,"keys":{"k1":{"isPressed":true,"count":99},"k2":{"isPressed":false,"count":0},"m1":{"isPressed":false,"count":0},"m2":{"isPressed":false,"count":0}},"keypresses":{"mode":{"number":3,"name":"mania"},"keys":[{"name":"K1","isPressed":true,"count":99,"pressedAt":1234,"releasedAt":null}]},"hitErrors":[99]}]}"#
        );
    }

    #[test]
    fn tournament_keypress_timestamps_are_isolated_per_spectator() {
        let focused = bundle(1, precise(Vec::new(), Vec::new()));
        let manager = bundle(2, precise(Vec::new(), Vec::new()));
        let mut first = precise(vec![button("K1", true, 1)], Vec::new());
        Arc::make_mut(&mut first).keypresses = Arc::new(vec![StateKeypressButton {
            name: "K1".into(),
            is_pressed: true,
            count: 1,
            pressed_at: Some(100.0),
            released_at: Some(80.0),
        }]);
        let mut second = precise(vec![button("K1", false, 2)], Vec::new());
        Arc::make_mut(&mut second).keypresses = Arc::new(vec![StateKeypressButton {
            name: "K1".into(),
            is_pressed: false,
            count: 2,
            pressed_at: Some(120.0),
            released_at: Some(140.0),
        }]);
        let snapshot = snapshot(
            Some(10),
            vec![
                entry(10, focused, false, None),
                entry(20, manager, true, None),
                entry(30, bundle(3, first), false, Some(1.0)),
                entry(40, bundle(4, second), false, Some(2.0)),
            ],
        );

        let response = build(&snapshot).expect("precise response");
        assert_eq!(
            (
                response.tourney[0].keypresses.keys[0].pressed_at,
                response.tourney[0].keypresses.keys[0].released_at,
                response.tourney[1].keypresses.keys[0].pressed_at,
                response.tourney[1].keypresses.keys[0].released_at,
            ),
            (Some(100.0), Some(80.0), Some(120.0), Some(140.0))
        );
    }

    #[test]
    fn ipc_ids_use_ecmascript_number_to_string_boundaries() {
        let focused = bundle(1, precise(Vec::new(), Vec::new()));
        let manager = bundle(2, precise(Vec::new(), Vec::new()));
        let large_integer = bundle(3, precise(Vec::new(), Vec::new()));
        let fixed_threshold = bundle(4, precise(Vec::new(), Vec::new()));
        let exponent_threshold = bundle(5, precise(Vec::new(), Vec::new()));
        let snapshot = snapshot(
            Some(10),
            vec![
                entry(10, focused, false, None),
                entry(20, manager, true, None),
                entry(30, large_integer, false, Some(1_000_000_000_000_000_128.0)),
                entry(40, fixed_threshold, false, Some(1e20)),
                entry(50, exponent_threshold, false, Some(1e21)),
            ],
        );

        let encoded = encode(&snapshot).expect("precise response");
        let json = std::str::from_utf8(&encoded).expect("UTF-8 JSON");
        let large = json
            .find(r#""ipcId":1000000000000000100"#)
            .expect("large integer uses ECMAScript rounding");
        let fixed = json
            .find(r#""ipcId":100000000000000000000"#)
            .expect("1e20 remains fixed notation");
        let exponent = json
            .find(r#""ipcId":1e+21"#)
            .expect("1e21 uses exponent notation");
        assert!(large < fixed && fixed < exponent);
    }
}
