//! The direct v1 writer must reproduce the DTO serialization byte for byte.

use std::sync::Arc;

use super::*;
use crate::api::wire_fixtures::{
    AWKWARD, NUMBERS, populated_precise, populated_regular, snapshot_with,
};
use crate::api::{ApiRuntimeSettings, ManagerSnapshot};
use crate::instance::InstanceSettings;
use crate::model::states::TourneyManagerChatItem;
use crate::state::{StateBundleSnapshot, TourneyUserState};

fn assert_direct_matches(snapshot: &CombinedSnapshot) {
    let direct = String::from_utf8(wire::encode(snapshot).unwrap()).unwrap();
    let dto = String::from_utf8(crate::api::json::encode(&build(snapshot).unwrap(), 0).unwrap())
        .unwrap();
    assert_eq!(direct, dto);
}

#[test]
fn direct_encoding_matches_dto_for_populated_states_and_numbers() {
    for source in [ClientType::Stable, ClientType::Lazer] {
        for (index, number) in NUMBERS.into_iter().enumerate() {
            let text = if index % 2 == 0 { AWKWARD } else { "plain" };
            for status in [2, 7, 0, 11] {
                let snapshot = snapshot_with(
                    source,
                    None,
                    ApiRuntimeSettings::default(),
                    |regular, precise| {
                        let mut precise = populated_precise(precise, number, text);
                        precise.status = status;
                        (populated_regular(regular, number, text), precise)
                    },
                );
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
            regular.menu = Default::default();
            regular.beatmap.attributes = None;
            regular.beatmap.parsed = None;
            regular.user.profile = None;
            regular.gameplay.leaderboard_scores.clear();
            (regular, (*precise).clone())
        },
    );
    assert_direct_matches(&snapshot);
}

#[test]
fn direct_encoding_matches_dto_for_stable_tournament() {
    let base = snapshot_with(
        ClientType::Stable,
        None,
        ApiRuntimeSettings::default(),
        |regular, precise| (regular.clone(), precise.clone()),
    );
    let base = base.focused().unwrap();
    let entry = |pid, manager, ipc: Option<f64>, name: &str| {
        let mut regular = populated_regular(&base.regular, ipc.unwrap_or(0.5), name);
        regular.tourney.user = Some(TourneyUserState {
            accuracy: 98.5,
            ranked_score: 3.0,
            play_count: 4,
            global_rank: 5,
            pp: 6.5,
            name: name.to_owned(),
            country: "US".to_owned(),
            id: i64::from(pid),
        });
        // Messages from a spectator, the bot, an unknown name and escapes.
        regular.tourney.messages = ["first", "BanchoBot", "nobody", AWKWARD]
            .into_iter()
            .map(|sender| TourneyManagerChatItem {
                time: "12:34".into(),
                name: sender.into(),
                content: format!("{sender} says {AWKWARD}"),
            })
            .collect();
        regular.tourney.scoreboard.first_team_name = name.to_owned();
        regular.tourney.scoreboard.first_team_score = 9_007_199_254_740_993;
        let state = Arc::new(StateBundleSnapshot {
            source: base.source,
            generation: base.generation,
            reset_epoch: base.reset_epoch,
            regular: Arc::new(regular),
            precise: Arc::new(populated_precise(&base.precise, 2.0, name)),
        });
        ManagerInstanceSnapshot::new(
            pid,
            InstanceSettings {
                tournament_spectator: ipc.is_some(),
                tournament_ipc_id: ipc,
                devserver_endpoint: None,
            },
            manager,
            state,
        )
    };
    let snapshot = CombinedSnapshot::from_manager(Arc::new(ManagerSnapshot::new(
        2,
        Some(50),
        false,
        ApiRuntimeSettings::default(),
        [
            entry(50, true, None, AWKWARD),
            entry(90, false, Some(7.5), "later"),
            entry(80, false, Some(f64::NAN), AWKWARD),
            entry(70, false, Some(2.25), "first"),
        ],
    )));
    assert_direct_matches(&snapshot);
}
