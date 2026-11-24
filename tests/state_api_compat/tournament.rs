//! Stable tournament aggregate and state-schema contract negatives.
//!
//! The pinned corpus gives both spectators identical play data, so it cannot
//! catch a swapped or manager-substituted client. These distinct fixtures close
//! the former stable-tournament exemption without broadening any ledger entry.
use super::{Value, json, oracle, ready};
use oracle::adapter_fixture::Fixture;
use std::{fs, sync::Arc};
use vosu::server::routes::CachedJson;
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings, states::TourneyManagerChatItem};
use vosu_osu_state::{Delivery, StateBundleSnapshot, TourneyManagerState, TourneyUserState};

struct Spectator {
    ipc: f64,
    name: &'static str,
    country: &'static str,
    id: i64,
    score: i64,
    combo: i64,
    max_combo: i64,
    keys: [i64; 4],
    errors: [f64; 2],
}
const SPECTATORS: [Spectator; 2] = [
    Spectator {
        ipc: 2.0,
        name: "Second",
        country: "FR",
        id: 77,
        score: 2_222_222,
        combo: 21,
        max_combo: 222,
        keys: [201, 202, 203, 204],
        errors: [-2.25, 2.75],
    },
    Spectator {
        ipc: 1.0,
        name: "First",
        country: "KR",
        id: 55,
        score: 1_111_111,
        combo: 11,
        max_combo: 111,
        keys: [101, 102, 103, 104],
        errors: [1.5, -1.75],
    },
];
const MANAGER_SCORE: i64 = 4_242_424;

fn input() -> oracle::Input {
    serde_json::from_slice(
        &fs::read(oracle::fixture_root().join("stable-tournament/input.json")).unwrap(),
    )
    .unwrap()
}

fn spectator(base: &oracle::Input, s: &Spectator) -> Arc<StateBundleSnapshot> {
    let mut input = base.clone();
    let gameplay = &mut input.services.gameplay;
    gameplay["score"] = json!(s.score);
    gameplay["combo"] = json!(s.combo);
    gameplay["maxCombo"] = json!(s.max_combo);
    gameplay["playerName"] = json!(format!("{}-play", s.name));
    gameplay["hitErrors"] = json!(s.errors);
    gameplay["keyOverlay"] = json!(
        ["K1", "K2", "M1", "M2"]
            .iter()
            .zip(s.keys)
            .map(|(name, count)| json!({"name": name, "isPressed": count % 2 == 0, "count": count}))
            .collect::<Vec<_>>()
    );
    let mut state = oracle::state(&input).as_ref().clone();
    let regular = Arc::make_mut(&mut state.regular);
    regular.tourney.user = Some(TourneyUserState {
        accuracy: 0.0,
        ranked_score: 0.0,
        play_count: 0,
        global_rank: 0,
        pp: 0.0,
        name: s.name.into(),
        country: s.country.into(),
        id: s.id,
    });
    Arc::new(state)
}

fn manager(base: &oracle::Input) -> Arc<StateBundleSnapshot> {
    let mut input = base.clone();
    input.services.gameplay["score"] = json!(MANAGER_SCORE);
    input.services.gameplay["playerName"] = json!("Manager-own-play");
    let mut state = oracle::state(&input).as_ref().clone();
    Arc::make_mut(&mut state.regular).tourney = TourneyManagerState {
        is_default_state: false,
        scoreboard: vosu_osu_model::memory::Tourney {
            ipc_state: 3,
            left_stars: 4,
            right_stars: 3,
            best_of: 7,
            stars_visible: true,
            score_visible: false,
            first_team_name: "Lions".into(),
            second_team_name: "Tigers".into(),
            first_team_score: 31_337,
            second_team_score: 27_182,
        },
        messages: vec![
            TourneyManagerChatItem {
                time: "20:01".into(),
                name: "Referee".into(),
                content: "first".into(),
            },
            TourneyManagerChatItem {
                time: "20:02".into(),
                name: "BanchoBot".into(),
                content: "second".into(),
            },
        ],
        user: None,
    };
    Arc::new(state)
}

/// Attaches spectator ipc 2, the manager, then spectator ipc 1 (out of IPC
/// order) and selects `selected` (0 = manager, 1/2 = spectator ipc id).
fn aggregate(selected: u8) -> [Value; 3] {
    let base = input();
    let mut fixture = Fixture::new(ClientType::Stable);
    let mut publishers = Vec::new();
    let settings = |ipc: Option<f64>| InstanceSettings {
        tournament_spectator: ipc.is_some(),
        tournament_ipc_id: ipc,
        devserver_endpoint: None,
    };
    for order in 0..3 {
        let (state, ipc, manager_session) = match order {
            0 => (spectator(&base, &SPECTATORS[0]), Some(2.0), false),
            1 => (manager(&base), None, true),
            _ => (spectator(&base, &SPECTATORS[1]), Some(1.0), false),
        };
        let selection = ipc.map_or(0, |ipc: f64| ipc as u8);
        let publisher = fixture.attach(settings(ipc), manager_session, selection == selected);
        publisher.publish(state, Delivery::All).unwrap();
        publishers.push(publisher);
    }
    let documents = ["v1", "v2", "v2-precise"].map(|schema| {
        let bytes = ready("stable-aggregate", schema, fixture.outcome(schema));
        let text = String::from_utf8(bytes).unwrap();
        // The state-schema bestOf rename never reaches the adapter wire.
        assert!(!text.contains("bestOf\""), "{schema}: {text}");
        serde_json::from_str(&text).unwrap()
    });
    ready("stable-aggregate", "sc", fixture.outcome("sc"));
    drop(publishers);
    documents
}

#[test]
fn stable_aggregate_maps_distinct_manager_and_spectators_on_every_session() {
    let mut aggregates = Vec::new();
    for selected in [0, 1, 2] {
        let [v1, v2, precise] = aggregate(selected);
        let tourney = &v2["tourney"];
        assert_eq!(tourney["bestOF"], 7, "{selected}");
        assert_eq!(tourney["ipcState"], 3);
        assert_eq!(tourney["team"], json!({"left": "Lions", "right": "Tigers"}));
        assert_eq!(tourney["points"], json!({"left": 4, "right": 3}));
        assert_eq!(
            tourney["totalScore"],
            json!({"left": 31_337, "right": 27_182})
        );
        let chat: Vec<_> = tourney["chat"]
            .as_array()
            .unwrap()
            .iter()
            .map(|line| line["message"].clone())
            .collect();
        assert_eq!(chat, [json!("first"), json!("second")]);
        let clients = tourney["clients"].as_array().unwrap();
        assert_eq!(clients.len(), 2);
        let managed = clients.iter().map(|c| c.to_string()).collect::<String>();
        assert!(!managed.contains(&MANAGER_SCORE.to_string()), "{managed}");
        assert!(!managed.contains("Manager-own-play"));
        let precise_clients = precise["tourney"].as_array().unwrap();
        let v1_clients = v1["tourney"]["ipcClients"].as_array().unwrap();
        assert_eq!(v1["tourney"]["manager"]["bestOF"], 7);
        for (index, s) in [&SPECTATORS[1], &SPECTATORS[0]].into_iter().enumerate() {
            let client = &clients[index];
            assert_eq!(client["ipcId"], json!(s.ipc as i64), "sorted by IPC id");
            assert_eq!(client["team"], if index == 0 { "left" } else { "right" });
            assert_eq!(client["user"]["name"], s.name);
            assert_eq!(client["user"]["id"], s.id);
            assert_eq!(client["play"]["score"], s.score);
            assert_eq!(client["play"]["playerName"], format!("{}-play", s.name));
            assert_eq!(
                client["play"]["combo"],
                json!({"current": s.combo, "max": s.max_combo})
            );
            assert_eq!(client["play"]["hitErrorArray"], json!(s.errors));
            let precise = &precise_clients[index];
            assert_eq!(precise["ipcId"], json!(s.ipc as i64));
            for (slot, count) in ["k1", "k2", "m1", "m2"].into_iter().zip(s.keys) {
                assert_eq!(precise["keys"][slot]["count"], count, "{} {slot}", s.name);
            }
            assert_eq!(precise["hitErrors"], json!(s.errors));
            let v1 = &v1_clients[index];
            assert_eq!(v1["spectating"]["name"], s.name);
            assert_eq!(v1["spectating"]["userID"], s.id);
            assert_eq!(v1["gameplay"]["score"], s.score);
            assert_eq!(v1["gameplay"]["hits"]["hitErrorArray"], json!(s.errors));
        }
        aggregates.push([
            v2["tourney"].clone(),
            precise["tourney"].clone(),
            v1["tourney"].clone(),
        ]);
    }
    assert_eq!(
        aggregates[0], aggregates[1],
        "manager vs spectator selection"
    );
    assert_eq!(aggregates[0], aggregates[2], "manager vs other spectator");
}

#[test]
fn state_schema_contract_entries_do_not_authorize_adapter_wire_differences() {
    // tourney-null-without-score: the bus publishes null, but the upstream v2
    // adapters still fail exactly like 1.x; the contract entry is no wire skip.
    let input: oracle::Input = serde_json::from_slice(
        &fs::read(oracle::fixture_root().join("lazer-lobby-tournament/input.json")).unwrap(),
    )
    .unwrap();
    let mut snapshot = oracle::state(&input).as_ref().clone();
    Arc::make_mut(&mut snapshot.regular)
        .lazer_multi_spectating
        .data
        .as_mut()
        .unwrap()
        .spectating_clients[0]
        .score = None;
    let mut fixture = Fixture::new(ClientType::Lazer);
    let publisher = fixture.attach(
        InstanceSettings {
            tournament_spectator: false,
            tournament_ipc_id: None,
            devserver_endpoint: None,
        },
        false,
        true,
    );
    publisher
        .publish(Arc::new(snapshot), Delivery::All)
        .unwrap();
    // Both adapters that emit the lazer client list fail like 1.x.
    for schema in ["v1", "v2"] {
        match fixture.outcome(schema) {
            CachedJson::Failed { status, body } => {
                assert_eq!(status, 500, "{schema}");
                let body: Value = serde_json::from_slice(&body).unwrap();
                assert!(body["error"].is_string(), "{schema}: {body}");
            }
            other => panic!("{schema}: missing score must fail, got {other:?}"),
        }
    }
    // Precise does NOT fail: it stays Ready with `tourney: []`, over HTTP and
    // on the precise WebSocket feed alike. Evidence for the 1.x behavior:
    // - v1.2.5 (and pre-M0 baseline e1df38f) `src/api/precise.rs`
    //   `tourney_spectators` returns an empty list unless a stable
    //   tournament-manager instance exists, and reads only stable IPC
    //   spectator instances; lazer multi-spectating scores are never consulted,
    //   so a missing lazer score has no precise failure path.
    // - The pinned source oracle `lazer-lobby-tournament/v2-precise.json`
    //   (upstream 2359305) is `..."tourney":[]` for an input with two lazer
    //   spectating clients, i.e. precise never lists lazer clients.
    let pinned =
        fs::read(oracle::fixture_root().join("lazer-lobby-tournament/v2-precise.json")).unwrap();
    let http = ready(&input.id, "v2-precise", fixture.outcome("v2-precise"));
    assert_eq!(
        String::from_utf8_lossy(&http),
        String::from_utf8_lossy(&pinned)
    );
    let frame = {
        use vosu::server::websocket::{Schema, StateProvider};
        fixture.providers[3]
            .capture(Schema::V2Precise)
            .expect("precise feed snapshot")
            .encode_unfiltered()
            .expect("the precise feed frame must not fail for a missing lazer score")
    };
    assert_eq!(frame, http, "the precise feed sends the same document");
    let precise: Value = serde_json::from_slice(&http).unwrap();
    assert_eq!(precise["tourney"], json!([]));
    drop(publisher);
    drop(fixture);

    // result.statistics.sliderBreaks: the schema's constant does not add the
    // member to 1.x v2 resultsScreen.hits, while v1 keeps its own literal.
    let input: oracle::Input = serde_json::from_slice(
        &fs::read(oracle::fixture_root().join("stable-result-catch-ht/input.json")).unwrap(),
    )
    .unwrap();
    let replay = oracle::replay(&input);
    assert_eq!(
        super::bus::fact(&replay, "vosu.osu", "/result/statistics/sliderBreaks"),
        Some(json!(0))
    );
    let v2: Value =
        serde_json::from_slice(&ready(&input.id, "v2", replay.fixture.outcome("v2"))).unwrap();
    assert!(v2["resultsScreen"]["hits"].get("sliderBreaks").is_none());
    assert!(v2["resultsScreen"]["hits"].get("sliderEndHits").is_some());
}
