//! The direct streamCompanion writer must reproduce the DTO serialization
//! byte for byte.

use super::*;
use crate::api::wire_fixtures::{
    AWKWARD, NUMBERS, populated_precise, populated_regular, snapshot_with,
};

fn assert_direct_matches(snapshot: &CombinedSnapshot) {
    let direct = String::from_utf8(super::super::wire::encode(snapshot).unwrap()).unwrap();
    let dto = String::from_utf8(encode_json_bytes(&build(snapshot).unwrap()).unwrap()).unwrap();
    assert_eq!(direct, dto);
}

fn runtime(enable_key_overlay: bool) -> ApiRuntimeSettings {
    ApiRuntimeSettings {
        enable_key_overlay,
        enable_mania_key_hit_errors: false,
    }
}

#[test]
fn direct_encoding_matches_dto_for_populated_states_and_numbers() {
    for source in [ClientType::Stable, ClientType::Lazer] {
        for (index, number) in NUMBERS.into_iter().enumerate() {
            let text = if index % 2 == 0 { AWKWARD } else { "plain" };
            for status in [2, 7, 1, 0] {
                let snapshot =
                    snapshot_with(source, None, runtime(index % 2 == 0), |regular, precise| {
                        let mut regular = populated_regular(regular, number, text);
                        regular.global.is_watching_replay = index % 3 == 0;
                        // Feed every time projection the awkward number.
                        regular.beatmap.graph = Arc::new(StrainGraph {
                            primary: Vec::new(),
                            series: vec![GraphSeries {
                                name: text.to_owned(),
                                data: vec![number, -1.0, 2.5],
                            }],
                            x_axis: vec![0.0, number, 1e21, 3.0],
                        });
                        let mut precise = populated_precise(precise, number, text);
                        precise.status = status;
                        precise.unstable_rate = number;
                        (regular, precise)
                    });
                assert_direct_matches(&snapshot);
            }
        }
    }
}

#[test]
fn direct_encoding_matches_dto_for_sparse_variants() {
    let snapshot = snapshot_with(
        ClientType::Stable,
        None,
        ApiRuntimeSettings::default(),
        |regular, precise| {
            let mut regular = populated_regular(regular, 1.5, "x");
            regular.settings.values.clear();
            regular.menu = Default::default();
            regular.beatmap.attributes = None;
            regular.beatmap.parsed = None;
            regular.user.profile = None;
            regular.gameplay.leaderboard_scores.clear();
            regular.gameplay.grade_current = "unknown".into();
            (regular, (*precise).clone())
        },
    );
    assert_direct_matches(&snapshot);

    // Country and ruleset names come from enum spellings; cover the ranges.
    for code in [-1, 0, 1, 37, 216, 250, 255, 1_000] {
        let snapshot = snapshot_with(
            ClientType::Lazer,
            None,
            ApiRuntimeSettings::default(),
            |regular, precise| {
                let mut regular = populated_regular(regular, 3.0, "y");
                if let Some(user) = regular.user.profile.as_mut() {
                    user.country_code = code;
                }
                regular.menu.gamemode = Some(code);
                regular.menu.map_id = None;
                (regular, populated_precise(precise, 3.0, "y"))
            },
        );
        assert_direct_matches(&snapshot);
    }
}
