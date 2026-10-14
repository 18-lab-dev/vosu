//! The direct V2 writer must reproduce the DTO serialization byte for byte.

use super::*;
use crate::api::wire_fixtures::{
    AWKWARD, NUMBERS, populated_precise, populated_regular, snapshot_with,
};

fn direct_and_dto(snapshot: &CombinedSnapshot) -> (String, String) {
    let direct = super::super::wire::encode(snapshot).unwrap();
    let dto = encode_dto(&build_internal::<&[f64]>(snapshot, false).unwrap()).unwrap();
    (
        String::from_utf8(direct).unwrap(),
        String::from_utf8(dto.to_vec()).unwrap(),
    )
}

fn assert_direct_matches(snapshot: &CombinedSnapshot) {
    let (direct, dto) = direct_and_dto(snapshot);
    assert_eq!(direct, dto);
    // The filtered/subscription contract serializes `build`, graph data included.
    assert_eq!(direct, json(&build(snapshot).unwrap()));
}

#[test]
fn direct_encoding_matches_dto_for_default_states() {
    for source in [ClientType::Stable, ClientType::Lazer] {
        assert_direct_matches(&default_snapshot(source));
    }
}

#[test]
fn direct_encoding_matches_dto_for_populated_states_and_numbers() {
    for source in [ClientType::Stable, ClientType::Lazer] {
        for (index, number) in NUMBERS.into_iter().enumerate() {
            let text = if index % 2 == 0 { AWKWARD } else { "plain" };
            let endpoint = (index % 3 == 0).then_some(text);
            let runtime = ApiRuntimeSettings::default();
            let snapshot = snapshot_with(source, endpoint, runtime, |regular, precise| {
                (
                    populated_regular(regular, number, text),
                    populated_precise(precise, number, text),
                )
            });
            assert_direct_matches(&snapshot);
        }
    }
}

#[test]
fn direct_encoding_matches_dto_for_sparse_variants() {
    let runtime = ApiRuntimeSettings::default();
    let snapshot = snapshot_with(ClientType::Stable, None, runtime, |regular, precise| {
        let mut regular = populated_regular(regular, 1.5, "x");
        regular.settings.values.clear();
        regular.menu = Default::default();
        regular.room.info = None;
        regular.beatmap.attributes = None;
        regular.beatmap.mode = None;
        regular.beatmap.parsed = None;
        if let Some(user) = regular.user.profile.as_mut() {
            user.matchmaking = None;
            user.background_colour = 0x00ab_cdef;
        }
        regular.gameplay.leaderboard_scores.clear();
        regular.gameplay.mods.array.clear();
        (regular, (*precise).clone())
    });
    assert_direct_matches(&snapshot);

    for status in [0, 2, 7, 11] {
        let snapshot = snapshot_with(ClientType::Lazer, None, runtime, |regular, precise| {
            let mut regular = populated_regular(regular, 2.0, "y");
            regular.user.profile = None;
            let mut precise = populated_precise(precise, 2.0, "y");
            precise.status = status;
            (regular, precise)
        });
        assert_direct_matches(&snapshot);
    }
}

#[test]
fn direct_encoding_matches_dto_for_large_settings_maps() {
    // Past the linear-scan limit, lookups take the hashed path; the looked-up
    // keys sit after many unrelated entries in both the root and a group.
    let runtime = ApiRuntimeSettings::default();
    let snapshot = snapshot_with(ClientType::Stable, None, runtime, |regular, precise| {
        let mut regular = populated_regular(regular, 4.5, "z");
        let values = &mut regular.settings.values;
        let mut tablet = serde_json::Map::new();
        for index in 0..40 {
            values.insert(format!("unused{index}"), Value::from(index));
            tablet.insert(format!("unused{index}"), Value::from(index));
        }
        tablet.insert("enabled".into(), Value::Bool(true));
        tablet.insert("rotation".into(), Value::from(12.5));
        values.shift_remove("tablet");
        values.insert("tablet".into(), Value::Object(tablet));
        values.shift_remove("sortType");
        values.insert("sortType".into(), Value::from(5));
        (regular, (*precise).clone())
    });
    assert_direct_matches(&snapshot);
}

#[test]
fn direct_encoding_matches_dto_for_stable_tournament() {
    let base = StateBundle::new(ClientType::Stable, 1, Arc::new(NeverCalculate)).snapshot();
    let mut manager_regular = populated_regular(&base.regular, 0.5, AWKWARD);
    manager_regular.tourney.user = Some(TourneyUserState {
        accuracy: 98.5,
        ranked_score: 3.0,
        play_count: 4,
        global_rank: 5,
        pp: 6.5,
        name: AWKWARD.to_owned(),
        country: "US".to_owned(),
        id: 7,
    });
    let manager = manager_entry(50, with_regular(&base, manager_regular), true, None);
    let spectator = |pid, ipc| {
        let regular = populated_regular(&base.regular, ipc, "spectator");
        manager_entry(pid, with_regular(&base, regular), false, Some(ipc))
    };
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        2,
        Some(50),
        false,
        ApiRuntimeSettings::default(),
        [manager, spectator(90, 7.5), spectator(80, f64::NAN)],
    )));
    assert_direct_matches(&snapshot);
}
