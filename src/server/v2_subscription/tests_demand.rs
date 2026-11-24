use super::*;
#[tokio::test]
async fn demand_tracks_established_selections_only() {
    let provider = FakeProvider::new(1, Some(json!({"a": 1})));
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let mut demand = hub.demand();
    let runner = tokio::spawn({
        let hub = hub.clone();
        async move { bounded!(hub.run()) }
    });

    let (connection, _receiver) = hub.connect();
    tokio::task::yield_now().await;
    assert!(
        demand.borrow().is_empty(),
        "a connection without a selection is not demand"
    );

    connection.subscribe(vec!["/keys".into()]).unwrap();
    bounded!(demand.wait_for(|active| active.gameplay_precise)).unwrap();

    connection.close(1000);
    bounded!(demand.wait_for(|active| active.is_empty())).unwrap();

    let (second, _second_receiver) = hub.connect();
    second.subscribe(vec!["/play/score".into()]).unwrap();
    bounded!(demand.wait_for(|active| active.gameplay_regular)).unwrap();
    hub.shutdown();
    bounded!(runner).unwrap();
    assert!(demand.borrow().is_empty(), "a stopped hub releases demand");
}

#[test]
fn selector_demand_follows_producing_lanes_and_parent_subtrees() {
    use vosu_osu_state::SubscriptionDemand;
    let selected = |paths: &[&str]| {
        compile_pointers(paths.iter().map(|s| (*s).into()).collect())
            .unwrap()
            .demand
    };
    assert!(
        selected(&[
            "/client",
            "/folders/skin",
            "/settings/mode",
            "/beatmap/checksum",
            "/settings/visual",
            "/beatmap/title",
            "/directPath/beatmapFile",
            "/unknown"
        ])
        .is_empty()
    );
    assert_eq!(
        selected(&["/keys", "/play/hitOffsets"]),
        SubscriptionDemand {
            gameplay_precise: true,
            ..Default::default()
        }
    );
    assert_eq!(
        selected(&["/play/score", "/play/combo/current"]),
        SubscriptionDemand {
            gameplay_regular: true,
            ..Default::default()
        }
    );
    assert_eq!(
        selected(&["/beatmap/time/live"]),
        SubscriptionDemand {
            precise: true,
            ..Default::default()
        }
    );
    assert_eq!(
        selected(&["/settings/bassDensity"]),
        SubscriptionDemand {
            regular: true,
            ..Default::default()
        }
    );
    assert_eq!(
        selected(&["/play"]),
        SubscriptionDemand {
            gameplay_regular: true,
            gameplay_precise: true,
            ..Default::default()
        }
    );
    assert_eq!(
        selected(&["/settings"]),
        SubscriptionDemand {
            regular: true,
            gameplay_regular: true,
            ..Default::default()
        }
    );
    let all = selected(&[""]);
    assert!(all.regular && all.precise && all.gameplay_regular && all.gameplay_precise);
}

#[test]
fn slow_selections_do_not_request_work_and_fast_clients_union_and_release_lanes() {
    let provider = FakeProvider::new(1, Some(json!({})));
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let demand = hub.demand();
    let (slow, _slow_rx) = hub.connect();
    slow.subscribe(vec!["/settings/mode".into(), "/beatmap/checksum".into()])
        .unwrap();
    assert!(demand.borrow().is_empty());
    let (precise, _precise_rx) = hub.connect();
    precise.subscribe(vec!["/keys".into()]).unwrap();
    let (regular, _regular_rx) = hub.connect();
    regular.subscribe(vec!["/play/score".into()]).unwrap();
    assert!(demand.borrow().gameplay_precise && demand.borrow().gameplay_regular);
    precise.close(1000);
    assert!(!demand.borrow().gameplay_precise && demand.borrow().gameplay_regular);
    regular.close(1000);
    assert!(demand.borrow().is_empty());
}

#[test]
fn many_clients_retain_each_lane_until_the_last_selection_is_released() {
    let provider = FakeProvider::new(1, Some(json!({})));
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let demand = hub.demand();
    let clients: Vec<_> = (0..1000)
        .map(|_| {
            let (connection, receiver) = hub.connect();
            // The /play parent and /keys both request gameplay-precise work,
            // but that lane counts only once for this client's plan.
            connection
                .subscribe(vec![
                    "/play".into(),
                    "/keys".into(),
                    "/beatmap/time/live".into(),
                    "/settings/bassDensity".into(),
                ])
                .unwrap();
            (connection, receiver)
        })
        .collect();
    for (connection, _) in &clients[..999] {
        connection.cleanup();
        connection.cleanup();
        let active = *demand.borrow();
        assert!(active.regular && active.precise);
        assert!(active.gameplay_regular && active.gameplay_precise);
    }
    drop(clients);
    assert!(demand.borrow().is_empty());
    assert_eq!(lock(&hub.0.demand_counts).lanes, [0; 4]);
}

#[test]
fn rejected_plan_change_and_close_all_release_demand_exactly_once() {
    let provider = FakeProvider::new(1, Some(json!({})));
    let hub = V2SubscriptionHub::new(provider, Duration::ZERO);
    let demand = hub.demand();
    let (first, _first_rx) = hub.connect();
    first.subscribe(vec!["/keys".into()]).unwrap();
    // The wire protocol terminates a second subscribe, rather than replacing
    // an established selection. Its original demand must be released.
    assert!(first.subscribe(vec!["/play/score".into()]).is_err());
    assert!(demand.borrow().is_empty());
    let (second, _second_rx) = hub.connect();
    second.subscribe(vec!["".into()]).unwrap();
    hub.close_all(1001);
    assert!(demand.borrow().is_empty());
    first.cleanup();
    second.cleanup();
    assert_eq!(lock(&hub.0.demand_counts).lanes, [0; 4]);
}
