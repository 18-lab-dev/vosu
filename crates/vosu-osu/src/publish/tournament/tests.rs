use super::*;
use crate::project::{SessionFacts, o5_tests::sample};
use std::sync::atomic::AtomicBool;
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
use vosu_osu_state::{Delivery, SnapshotPublisher};
use vosu_plugin_api::{HostServices, vosu_status};
#[derive(Default)]
struct Host;
impl HostServices for Host {
    fn send(&self, _: &vosu_plugin_api::Message, _: u64) -> vosu_status {
        vosu_status::VOSU_OK
    }
    fn send_flat(&self, _: vosu_plugin_api::vosu_opcode, _: u32, _: u64, _: &[u8]) -> vosu_status {
        vosu_status::VOSU_OK
    }
}
fn settings(ipc: Option<f64>) -> InstanceSettings {
    InstanceSettings {
        tournament_spectator: ipc.is_some(),
        tournament_ipc_id: ipc,
        devserver_endpoint: None,
    }
}
fn bus(handle: u64) -> Arc<BusPublisher<SessionHost>> {
    Arc::new(BusPublisher::new(
        SessionHost(Arc::new(Host)),
        handle,
        Arc::new(AtomicBool::new(true)),
        SessionFacts {
            client: ClientType::Stable,
            settings: settings(None),
            foreground: None,
        },
    ))
}
fn value(bus: &BusPublisher<SessionHost>) -> serde_json::Value {
    vosu_plugin_api::decode(&bus.aggregate.lock().unwrap().as_ref().unwrap().bytes).unwrap()
}
#[test]
fn aggregate_reaches_every_stable_session_orders_ipc_and_rejects_detached_sources() {
    let roster = Arc::new(Tournament::default());
    let manager = bus(1001);
    let ordinary = bus(1002);
    let right = bus(1003);
    let left = bus(1004);
    for (bus, handle, manager, ipc) in [
        (&manager, 1001, true, None),
        (&ordinary, 1002, false, None),
        (&right, 1003, false, Some(9.0)),
        (&left, 1004, false, Some(2.0)),
    ] {
        bus.tournament.set((roster.clone(), handle)).ok().unwrap();
        roster.attach(
            bus,
            handle,
            manager,
            settings(ipc),
            Controls {
                selected: Arc::new(AtomicBool::new(manager)),
                demand: Default::default(),
            },
        );
    }
    assert_eq!(
        ordinary
            .aggregate
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .availability,
        "pending"
    );
    let mut m = sample();
    Arc::make_mut(&mut m.regular).tourney.scoreboard.best_of = 7;
    roster.observe(1001, &Arc::new(m));
    let mut l = sample();
    Arc::make_mut(&mut l.regular).gameplay.score = 12345.0;
    let mut r = sample();
    Arc::make_mut(&mut r.regular).gameplay.score = 67890.0;
    let left_sample = Arc::new(l);
    let right_sample = Arc::new(r);
    roster.observe(1004, &left_sample);
    assert!(
        value(&ordinary).is_null(),
        "late spectator is pending, not fabricated zero play"
    );
    roster.observe(1003, &right_sample);
    let aggregate = value(&ordinary);
    for bus in [&manager, &left, &right] {
        assert_eq!(value(bus), aggregate);
    }
    assert_eq!(aggregate["bestOf"], 7);
    assert_eq!(aggregate["clients"][0]["ipcId"], 2.0);
    assert_eq!(aggregate["clients"][0]["team"], "left");
    assert_eq!(aggregate["clients"][0]["play"]["score"], 12345.0);
    assert_eq!(aggregate["clients"][1]["team"], "right");
    assert_eq!(aggregate["clients"][1]["play"]["score"], 67890.0);
    let before = ordinary.aggregate.lock().unwrap().clone().unwrap();
    roster.detach(1004);
    roster.observe(1004, &left_sample);
    assert_eq!(value(&ordinary)["clients"].as_array().unwrap().len(), 1);
    ordinary.set_tournament(before);
    assert_eq!(
        value(&ordinary)["clients"].as_array().unwrap().len(),
        1,
        "late broadcast cannot resurrect detached IPC"
    );
    roster.detach(1001);
    assert!(value(&ordinary)["clients"].as_array().unwrap().is_empty());
    let replacement = bus(1005);
    roster.attach(
        &replacement,
        1005,
        true,
        settings(None),
        Controls {
            selected: Arc::new(AtomicBool::new(true)),
            demand: Default::default(),
        },
    );
    assert!(value(&ordinary).is_null());
    let mut m = sample();
    Arc::make_mut(&mut m.regular).tourney.scoreboard.best_of = 13;
    roster.observe(1005, &Arc::new(m));
    assert_eq!(value(&ordinary)["bestOf"], 13);
    roster.observe(1001, &Arc::new(sample()));
    assert_eq!(
        value(&ordinary)["bestOf"],
        13,
        "retired manager generation cannot replace its successor"
    );
}
#[test]
fn source_precision_and_raw_fidelity_survive_aggregate_projection() {
    let mut s = sample();
    let g = &mut Arc::make_mut(&mut s.regular).gameplay;
    g.player_hp = 143.00000000000003;
    g.statistics.great = 19;
    g.statistics.perfect = 23;
    Arc::make_mut(&mut s.precise).unstable_rate = 27.25;
    let precise = Arc::make_mut(&mut s.precise);
    precise.key_overlay = Arc::new(
        [11, 22, 33, 44]
            .into_iter()
            .enumerate()
            .map(|(i, count)| vosu_osu_model::states::KeyOverlayButton {
                name: Arc::from("not-a-logical-name"),
                is_pressed: i % 2 == 0,
                count,
            })
            .collect(),
    );
    let errors = Arc::make_mut(&mut precise.hit_errors);
    errors.all = vec![-7.0, 13.0];
    errors.press = vec![-7.0];
    errors.release = vec![13.0];
    let bytes = crate::project::stable_tourney(Some(&sample()), &[(8.0, &s)]);
    let value: serde_json::Value = vosu_plugin_api::decode(&bytes).unwrap();
    let play = &value["clients"][0]["play"];
    assert_eq!(play["healthRaw"]["normal"], 143.00000000000003);
    assert_eq!(play["rawJudgements"]["great"], 19);
    assert_eq!(play["rawJudgements"]["perfect"], 23);
    assert_eq!(play["preciseUnstableRate"], 27.25);
    assert_eq!(
        play["hitErrors"],
        serde_json::json!({"all":[-7.0,13.0],"press":[-7.0],"release":[13.0]})
    );
    for (name, count) in [("k1", 11), ("k2", 22), ("m1", 33), ("m2", 44)] {
        assert_eq!(value["clients"][0]["keysOverlay"][name]["count"], count);
    }
    let schema: serde_json::Value = serde_json::from_str(include_str!(
        "../../../../../docs/v2/schemas/vosu.osu.schema.json"
    ))
    .unwrap();
    assert!(
        schema["$defs"]["tourneyClientPlay"]["properties"]
            .get("rawJudgements")
            .is_some()
    );
    let producer = bus(1010);
    producer.publish(Arc::new(s), Delivery::All).unwrap();
}

#[tokio::test]
async fn aggregate_demand_unions_and_releases_on_every_lifecycle_exit() {
    use crate::demand::SessionDemand;
    async fn expect(demand: &SessionDemand, regular: bool, precise: bool) {
        let mut receiver = demand.receiver();
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            loop {
                let value = *receiver.borrow_and_update();
                if value.regular == regular && value.precise == precise {
                    break;
                }
                receiver.changed().await.unwrap();
            }
        })
        .await
        .expect("aggregate demand deadline");
    }
    let roster = Tournament::default();
    let make = || SessionDemand::new();
    let manager = make();
    let spectator = make();
    let successor = make();
    let ordinary = make();
    let m = bus(2001);
    let s = bus(2002);
    let n = bus(2003);
    let o = bus(2004);
    for (bus, handle, is_manager, ipc, demand) in [
        (&m, 2001, true, None, &manager),
        (&s, 2002, false, Some(1.0), &spectator),
        (&n, 2003, true, None, &successor),
        (&o, 2004, false, None, &ordinary),
    ] {
        roster.attach(
            bus,
            handle,
            is_manager,
            settings(ipc),
            Controls {
                selected: Arc::new(AtomicBool::new(handle == 2001)),
                demand: demand.tournament.clone(),
            },
        );
    }
    spectator.set_bus(true, false);
    manager.tournament.request(false, true);
    successor.tournament.request(true, false);
    roster.demand_changed();
    expect(&spectator, true, true).await;
    expect(&manager, true, true).await;
    expect(&ordinary, false, false).await;
    manager.tournament.request(false, false);
    roster.demand_changed();
    expect(&spectator, true, false).await;
    expect(&manager, true, false).await;
    assert_eq!(
        [&manager, &spectator, &successor, &ordinary]
            .iter()
            .filter(|d| d.receiver().borrow().precise)
            .count(),
        0,
        "unsubscribe releases every derived precise sampler"
    );
    manager.tournament.request(false, true);
    roster.demand_changed();
    expect(&spectator, true, true).await;
    roster.detach(2001);
    manager.cancel();
    expect(&spectator, true, false).await;
    expect(&successor, true, false).await;
    expect(&manager, false, false).await;
    successor.tournament.request(false, true);
    roster.demand_changed();
    expect(&spectator, true, true).await;
    roster.detach(2002);
    spectator.cancel();
    expect(&spectator, false, false).await;
    roster.detach(2003);
    successor.cancel();
    expect(&ordinary, false, false).await;
    expect(&successor, false, false).await;
    ordinary.cancel();
    assert_eq!(
        [&manager, &spectator, &successor, &ordinary]
            .iter()
            .filter(|d| d.receiver().borrow().regular || d.receiver().borrow().precise)
            .count(),
        0,
        "manager loss, detach and shutdown leave no aggregate demand"
    );
}
