use super::allocations_during;
#[path = "../../benches/support/api_v1.rs"]
mod cost;

#[test]
fn idle_hub_adds_no_publication_allocations_or_demand() {
    use vosu_core::api::SessionTarget;
    use vosu_core::server::api_v1::subscriptions::ApiV1SubscriptionHub;
    let id = "test.x1.allocations";
    let fixture = cost::Telemetry::new(id, 992001);
    let source = fixture.h.host.api_state_source();
    let publish = || {
        for n in 0..32 {
            fixture.publish(id, 1200 + n % 2);
        }
    };
    publish();
    let (_, baseline) = allocations_during(publish);
    let hub = ApiV1SubscriptionHub::new(source.clone(), std::time::Duration::ZERO);
    let (_, idle) = allocations_during(publish);
    assert!(
        baseline > 0,
        "the allocator must observe the real commit path"
    );
    assert_eq!(
        idle, baseline,
        "an idle hub must not allocate per publication"
    );
    eprintln!("X1 allocations per 32 publications: baseline={baseline}, idle-hub={idle}");
    let update = source
        .read(&SessionTarget::Active, &cost::Telemetry::plan(id))
        .unwrap();
    assert_eq!(
        update.revision, 0,
        "unobserved publications must not notify API observers"
    );
    assert!(
        fixture
            .h
            .host
            .state()
            .subscriptions
            .lock()
            .unwrap()
            .demand(vosu_core::state::SessionKey(fixture.handle), None)
            .selectors
            .is_empty()
    );
    hub.shutdown();
}

#[test]
fn reference_publication_uses_only_prepared_metadata() {
    use vosu_core::state::{Pointer, SessionKey};
    let references = cost::References::new();
    references.commit(false);
    let pointer = Pointer::parse("/resources/audio/url").unwrap();
    let first = references
        .store
        .current(SessionKey(99131))
        .unwrap()
        .get("vosu.chart", &pointer)
        .unwrap()
        .clone();
    references.commit(false);
    assert_eq!(
        references
            .store
            .current(SessionKey(99131))
            .unwrap()
            .get("vosu.chart", &pointer),
        Some(&first)
    );
    references.commit(true);
    assert_ne!(
        references
            .store
            .current(SessionKey(99131))
            .unwrap()
            .get("vosu.chart", &pointer),
        Some(&first)
    );
    // No filesystem roots or resolver exist: neither publication can conceal IO.
}

#[test]
fn slow_consumer_retains_one_frame_and_delivers_latest() {
    use vosu_core::api::SessionTarget;
    use vosu_core::server::{
        api_v1::subscriptions::ApiV1SubscriptionHub, subscription_wire::Outbound,
    };
    let id = "test.x1.slow";
    let fixture = cost::Telemetry::new(id, 992003);
    let hub =
        ApiV1SubscriptionHub::new(fixture.h.host.api_state_source(), std::time::Duration::ZERO);
    fixture.h.runtime.block_on(cost::harness::bounded(async {
        let (connection, mut receiver) = hub.connect(SessionTarget::Active).unwrap();
        connection.text(
            serde_json::to_string(&[format!("/{id}/score")])
                .unwrap()
                .as_bytes(),
        );
        assert!(matches!(receiver.recv().await.unwrap(), Outbound::Text(_)));
        // Hold the first actual frame in flight while commits advance.
        for n in 0..1024 {
            fixture.publish(id, 1700 + n);
        }
        assert!(
            receiver.try_recv().is_none(),
            "no second frame may queue in flight"
        );
        connection.write_completed();
        let Outbound::Text(bytes) = receiver.recv().await.unwrap() else {
            panic!("latest data frame");
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(value[2], serde_json::json!([[0, 2723]]));
        connection.write_completed();
    }));
    hub.shutdown();
}
