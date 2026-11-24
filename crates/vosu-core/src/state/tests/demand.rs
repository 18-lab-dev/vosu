use super::super::*;
use std::sync::Arc;

fn select(ns: &str, p: &str) -> Vec<(Arc<str>, Pointer)> {
    vec![(Arc::from(ns), Pointer::parse(p).unwrap())]
}

#[test]
fn demand_is_union_of_selectors_with_status_gating() {
    let schemas = SchemaRegistry::with_core().unwrap();
    let mut subscriptions = SubscriptionRegistry::default();
    let key = SessionKey(5101);
    subscriptions
        .replace(
            &schemas,
            SubscriberId::Plugin(5101),
            key,
            select("vosu.play", "/progress"),
            InternalGrant::default(),
        )
        .unwrap();
    subscriptions
        .replace(
            &schemas,
            SubscriberId::Surface(5102),
            key,
            select("vosu.play", "/accuracy"),
            InternalGrant::default(),
        )
        .unwrap();
    for status in ["play", "pause"] {
        let demand = subscriptions.demand(key, Some(status));
        assert!(demand.regular);
        assert!(demand.precise);
        assert_eq!(demand.selectors.len(), 2);
    }
    for status in [Some("results"), None] {
        let demand = subscriptions.demand(key, status);
        assert!(!demand.regular);
        // Progress's precise/always lane remains active outside gameplay.
        assert!(demand.precise);
    }
    let demand = subscriptions.demand(SessionKey(9999), Some("play"));
    assert_eq!(demand, DemandSnapshot::default());
}

#[test]
fn both_lane_sets_both_flags() {
    let mut schemas = SchemaRegistry::with_core().unwrap();
    schemas.register("test.both", "test.both", r#"{"title":"test.both","version":"1.0.0","type":"object","properties":{"x":{"type":"number","x-sampling-lane":{"lane":"both","activity":"gameplay"}}}}"#).unwrap();
    let mut subscriptions = SubscriptionRegistry::default();
    subscriptions
        .replace(
            &schemas,
            SubscriberId::Plugin(5201),
            SessionKey(5201),
            select("test.both", "/x"),
            InternalGrant::default(),
        )
        .unwrap();
    let demand = subscriptions.demand(SessionKey(5201), Some("pause"));
    assert!(demand.regular && demand.precise);
    let demand = subscriptions.demand(SessionKey(5201), Some("results"));
    assert!(!demand.regular && !demand.precise);
}

#[test]
fn complete_demand_is_sorted_and_deduplicated() {
    let schemas = SchemaRegistry::with_core().unwrap();
    let mut subscriptions = SubscriptionRegistry::default();
    for id in [5301, 5302] {
        subscriptions
            .replace(
                &schemas,
                SubscriberId::Plugin(id),
                SessionKey(5301),
                select("vosu.chart", "/title"),
                InternalGrant::default(),
            )
            .unwrap();
    }
    assert_eq!(
        subscriptions.demand(SessionKey(5301), None).selectors,
        vec![(Arc::from("vosu.chart"), Arc::from("/title"))]
    );
}
