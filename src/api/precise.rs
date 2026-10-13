//! Source-compatible `/json/v2/precise` wire builder.

use bytes::Bytes;
use serde_json::ser::Formatter;
use std::cmp::Ordering;
use std::io;
use std::sync::Arc;

use crate::api::json::{JsFormatter, write_js_number};
use crate::api::{ApiError, ApiSchema, CombinedSnapshot, ManagerSnapshot};
use crate::model::api::v2::{KeyOverlayButton, Keypress, Keys, PreciseResponse, PreciseTourney};
use crate::model::states::{
    KeyOverlayButton as StateKeyOverlayButton, KeypressButton as StateKeypressButton,
};
use crate::state::PreciseState;

/// Builds the exact V2 precise document from one immutable publication point.
pub fn build(snapshot: &CombinedSnapshot) -> Result<PreciseResponse, ApiError> {
    let precise = snapshot.precise().ok_or(ApiError::NotReady)?;

    Ok(PreciseResponse {
        keys: build_keys(&precise.key_overlay, &precise.keypresses),
        hit_errors: precise.hit_errors.all.clone(),
        press_hit_errors: precise.hit_errors.press.clone(),
        release_hit_errors: precise.hit_errors.release.clone(),
        tourney: tourney_spectators(snapshot.manager())
            .into_iter()
            .map(|(ipc_id, precise)| PreciseTourney {
                ipc_id,
                keys: build_keys(&precise.key_overlay, &precise.keypresses),
                hit_errors: precise.hit_errors.all.clone(),
                press_hit_errors: precise.hit_errors.press.clone(),
                release_hit_errors: precise.hit_errors.release.clone(),
            })
            .collect(),
    })
}

/// Encodes the unfiltered V2 precise document once for this combined generation.
///
/// The bytes are written directly rather than through a serde DTO because this
/// is the highest-rate cold document; `build` stays the owned contract, and
/// tests require both to serialize identically.
pub fn encode(snapshot: &CombinedSnapshot) -> Result<Bytes, ApiError> {
    snapshot.encode_unfiltered(ApiSchema::V2Precise, |snapshot| {
        let precise = snapshot.precise().ok_or(ApiError::NotReady)?;
        let tourney = tourney_spectators(snapshot.manager());
        let capacity = tourney
            .iter()
            .fold(encoded_capacity(precise), |total, (_, spectator)| {
                total + 32 + encoded_capacity(spectator)
            });
        let mut output = Vec::with_capacity(capacity);
        write_document(&mut output, precise, &tourney)
            .map_err(|error| ApiError::serialization(error.to_string()))?;
        Ok(Bytes::from(output))
    })
}

/// Approximate output size so the cold encode does not repeatedly regrow its
/// buffer; an underestimate only costs ordinary `Vec` growth.
fn encoded_capacity(precise: &PreciseState) -> usize {
    let hit_errors = &precise.hit_errors;
    256 + 96 * precise.keypresses.len()
        + 24 * (hit_errors.all.len() + hit_errors.press.len() + hit_errors.release.len())
}

fn write_document(
    output: &mut Vec<u8>,
    precise: &PreciseState,
    tourney: &[(f64, &PreciseState)],
) -> io::Result<()> {
    output.extend_from_slice(b"{");
    write_fields(output, precise)?;
    output.extend_from_slice(b",\"tourney\":[");
    for (index, (ipc_id, spectator)) in tourney.iter().enumerate() {
        if index > 0 {
            output.push(b',');
        }
        output.extend_from_slice(b"{\"ipcId\":");
        write_js_number(output, *ipc_id)?;
        output.push(b',');
        write_fields(output, spectator)?;
        output.push(b'}');
    }
    output.extend_from_slice(b"]}");
    Ok(())
}

/// Writes the `keys` through `releaseHitErrors` members shared by the focused
/// document and each tourney entry, in [`PreciseResponse`] field order.
fn write_fields(output: &mut Vec<u8>, precise: &PreciseState) -> io::Result<()> {
    let legacy = &precise.key_overlay;
    output.extend_from_slice(b"\"keys\":{\"k1\":");
    write_key(output, legacy.first())?;
    output.extend_from_slice(b",\"k2\":");
    write_key(output, legacy.get(1))?;
    output.extend_from_slice(b",\"m1\":");
    write_key(output, legacy.get(2))?;
    output.extend_from_slice(b",\"m2\":");
    write_key(output, legacy.get(3))?;
    output.extend_from_slice(b",\"all\":[");
    for (index, button) in precise.keypresses.iter().enumerate() {
        if index > 0 {
            output.push(b',');
        }
        output.extend_from_slice(b"{\"name\":");
        write_str(output, &button.name)?;
        output.extend_from_slice(if button.is_pressed {
            &b",\"isPressed\":true,\"count\":"[..]
        } else {
            &b",\"isPressed\":false,\"count\":"[..]
        });
        JsFormatter.write_i64(output, button.count)?;
        match button.pressed_at {
            Some(value) => {
                output.extend_from_slice(b",\"pressedAt\":");
                write_js_number(output, value)?;
            }
            None => output.extend_from_slice(b",\"pressedAt\":null"),
        }
        match button.released_at {
            Some(value) => {
                output.extend_from_slice(b",\"releasedAt\":");
                write_js_number(output, value)?;
                output.push(b'}');
            }
            None => output.extend_from_slice(b",\"releasedAt\":null}"),
        }
    }
    let hit_errors = &precise.hit_errors;
    output.extend_from_slice(b"]},\"hitErrors\":");
    write_numbers(output, &hit_errors.all)?;
    output.extend_from_slice(b",\"pressHitErrors\":");
    write_numbers(output, &hit_errors.press)?;
    output.extend_from_slice(b",\"releaseHitErrors\":");
    write_numbers(output, &hit_errors.release)
}

fn write_key(output: &mut Vec<u8>, button: Option<&StateKeyOverlayButton>) -> io::Result<()> {
    let key = build_key(button);
    output.extend_from_slice(if key.is_pressed {
        &b"{\"isPressed\":true,\"count\":"[..]
    } else {
        &b"{\"isPressed\":false,\"count\":"[..]
    });
    JsFormatter.write_i64(output, key.count)?;
    output.push(b'}');
    Ok(())
}

/// Writes a JSON string; names normally need no escaping, and the general
/// serde_json escaper (which defines the exact escape spelling) handles the rest.
fn write_str(output: &mut Vec<u8>, value: &str) -> io::Result<()> {
    if value
        .bytes()
        .all(|byte| byte >= 0x20 && byte != b'"' && byte != b'\\')
    {
        output.push(b'"');
        output.extend_from_slice(value.as_bytes());
        output.push(b'"');
        Ok(())
    } else {
        serde_json::to_writer(output, value).map_err(io::Error::other)
    }
}

fn write_numbers(output: &mut Vec<u8>, values: &[f64]) -> io::Result<()> {
    output.push(b'[');
    for (index, value) in values.iter().enumerate() {
        if index > 0 {
            output.push(b',');
        }
        write_js_number(output, *value)?;
    }
    output.push(b']');
    Ok(())
}

fn tourney_spectators(manager: &ManagerSnapshot) -> Vec<(f64, &PreciseState)> {
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
}

pub(crate) fn build_keys(
    legacy: &[StateKeyOverlayButton],
    buttons: &[StateKeypressButton],
) -> Keys {
    Keys {
        k1: build_key(legacy.first()),
        k2: build_key(legacy.get(1)),
        m1: build_key(legacy.get(2)),
        m2: build_key(legacy.get(3)),
        all: buttons
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
    use crate::model::states::HitErrorArray;
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
            room: Default::default(),
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
            hit_errors: Arc::new(HitErrorArray {
                all: hit_errors.clone(),
                press: hit_errors,
                release: Vec::new(),
            }),
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
    fn missing_keys_default_and_field_order_is_exact() {
        let focused = bundle(1, precise_for_mode(99, Vec::new(), Vec::new()));
        let snapshot = snapshot(Some(10), vec![entry(10, focused, false, None)]);

        let encoded = encode(&snapshot).expect("precise response");
        assert_eq!(
            std::str::from_utf8(&encoded).expect("UTF-8 JSON"),
            r#"{"keys":{"k1":{"isPressed":false,"count":0},"k2":{"isPressed":false,"count":0},"m1":{"isPressed":false,"count":0},"m2":{"isPressed":false,"count":0},"all":[]},"hitErrors":[],"pressHitErrors":[],"releaseHitErrors":[],"tourney":[]}"#
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
        Arc::make_mut(&mut precise).hit_errors = Arc::new(HitErrorArray {
            all: vec![-7.5, 2.25],
            press: vec![-7.5],
            release: vec![2.25],
        });
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
            response
                .keys
                .all
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
        assert_eq!(response.hit_errors, [-7.5, 2.25]);
        assert_eq!(response.press_hit_errors, [-7.5]);
        assert_eq!(response.release_hit_errors, [2.25]);
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
            r#"{"keys":{"k1":{"isPressed":false,"count":0},"k2":{"isPressed":false,"count":0},"m1":{"isPressed":false,"count":0},"m2":{"isPressed":false,"count":0},"all":[]},"hitErrors":[],"pressHitErrors":[],"releaseHitErrors":[],"tourney":[{"ipcId":2.5,"keys":{"k1":{"isPressed":false,"count":2},"k2":{"isPressed":true,"count":22},"m1":{"isPressed":false,"count":0},"m2":{"isPressed":false,"count":0},"all":[{"name":"K1","isPressed":false,"count":2,"pressedAt":null,"releasedAt":null},{"name":"K2","isPressed":true,"count":22,"pressedAt":1234,"releasedAt":null}]},"hitErrors":[-2.5],"pressHitErrors":[-2.5],"releaseHitErrors":[]},{"ipcId":30.25,"keys":{"k1":{"isPressed":true,"count":30},"k2":{"isPressed":false,"count":0},"m1":{"isPressed":false,"count":0},"m2":{"isPressed":false,"count":0},"all":[{"name":"K1","isPressed":true,"count":30,"pressedAt":1234,"releasedAt":null}]},"hitErrors":[30.5],"pressHitErrors":[30.5],"releaseHitErrors":[]},{"ipcId":null,"keys":{"k1":{"isPressed":true,"count":99},"k2":{"isPressed":false,"count":0},"m1":{"isPressed":false,"count":0},"m2":{"isPressed":false,"count":0},"all":[{"name":"K1","isPressed":true,"count":99,"pressedAt":1234,"releasedAt":null}]},"hitErrors":[99],"pressHitErrors":[99],"releaseHitErrors":[]}]}"#
        );
    }

    #[test]
    fn encode_matches_owned_response_bytes() {
        let numbers = [
            -0.0,
            0.1,
            -7.5,
            1e21,
            9_007_199_254_740_993.0,
            f64::NAN,
            f64::NEG_INFINITY,
            123_456_789.0,
            5e-7,
        ];
        let names = [
            "K1",
            "q\"\\\u{1}\u{7f}é\u{2028}😀",
            "",
            "\u{7f}é\u{2028}😀 /",
            "\u{1f}",
        ];
        let spectator = |count: i64| {
            let mut precise = precise_for_mode(
                1,
                names
                    .iter()
                    .enumerate()
                    .map(|(index, name)| button(name, index % 2 == 0, count + index as i64))
                    .collect(),
                numbers.to_vec(),
            );
            let state = Arc::make_mut(&mut precise);
            state.keypresses = Arc::new(
                state
                    .keypresses
                    .iter()
                    .zip(numbers.iter().cycle())
                    .map(|(key, &time)| StateKeypressButton {
                        pressed_at: Some(time),
                        released_at: (key.count % 2 == 0).then_some(-time),
                        ..key.clone()
                    })
                    .collect(),
            );
            state.hit_errors = Arc::new(HitErrorArray {
                all: numbers.to_vec(),
                press: numbers[..4].to_vec(),
                release: numbers[4..].to_vec(),
            });
            precise
        };
        let snapshot = snapshot(
            Some(50),
            vec![
                entry(50, bundle(1, spectator(i64::MIN)), false, None),
                entry(30, bundle(2, spectator(0)), true, None),
                entry(40, bundle(3, spectator(i64::MAX - 8)), false, Some(-0.0)),
                entry(60, bundle(4, spectator(-3)), false, Some(f64::NAN)),
                entry(70, bundle(5, spectator(7)), false, Some(1e300)),
            ],
        );

        let owned = crate::api::v2::encode_dto(&build(&snapshot).expect("precise response"))
            .expect("owned encoding");
        assert_eq!(encode(&snapshot).expect("precise encoding"), owned);
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
                response.tourney[0].keys.all[0].pressed_at,
                response.tourney[0].keys.all[0].released_at,
                response.tourney[1].keys.all[0].pressed_at,
                response.tourney[1].keys.all[0].released_at,
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
