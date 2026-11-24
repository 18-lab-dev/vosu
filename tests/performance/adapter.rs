//! Production adapter cost invariants: the real osu! publisher commits through
//! the host bus, the real adapters decode coherent surface captures, and the
//! production `BusStateProvider`/`WebSocketHub` deliver. Counters only observe.
use super::allocations_during;
use crate::adapter::{Rig, input, json_at};
use bytes::Bytes;
use std::time::Duration;
use vosu::server::{
    api_v1::subscriptions::ApiV1SubscriptionHub,
    routes::StateRoute,
    websocket::{Schema, StateProvider},
};
use vosu_adapter_support::filtered::Filter;
use vosu_core::api::SessionTarget;
use vosu_osu_state::Delivery;
use vosu_plugin_api::{Selector, vosu_status};
use vosu_surface::state::SessionTarget as Target;

fn regular(score: f64) -> std::sync::Arc<vosu_osu_state::StateBundleSnapshot> {
    input::snapshot(score, input::K1_COUNTS[0])
}
fn score(bytes: &Bytes) -> Option<f64> {
    json_at(bytes, "/play/score").as_f64()
}
fn selected(demand: &[(std::sync::Arc<str>, std::sync::Arc<str>)], pointer: &str) -> bool {
    demand
        .iter()
        .any(|(ns, p)| &**ns == "vosu.osu" && &**p == pointer)
}

#[test]
fn production_http_ws_and_plain_clients_share_one_bytes_allocation() {
    let rig = Rig::new();
    let (v2, counts) = rig.provider(Schema::V2, crate::adapter::CADENCE);
    let hub = rig.hub(Schema::V2, v2.clone());
    let mut clients: Vec<_> = (0..100).map(|_| hub.connect(&rig, None)).collect();
    let before = counts.work();
    rig.publish(regular(31_337.0), Delivery::All);
    let http = rig.http(&v2, StateRoute::V2);
    assert_eq!(score(&http), Some(31_337.0));
    for client in &clients {
        client.ack();
    }
    rig.runtime().block_on(async {
        for client in &mut clients {
            let frame = client.frame().await;
            assert_eq!(
                frame.as_ptr(),
                http.as_ptr(),
                "HTTP and every plain client share the encoded Bytes allocation"
            );
        }
    });
    let work = counts.work() - before;
    assert_eq!(
        work.decodes, 1,
        "one coherent view for HTTP and 100 clients"
    );
    assert_eq!(work.unfiltered, 1, "one encoding for HTTP and 100 clients");
    hub.stop(&rig);
}

#[test]
fn production_regular_cache_survives_precise_only_changes() {
    let rig = Rig::new();
    let (v2, regular_counts) = rig.provider(Schema::V2, crate::adapter::CADENCE);
    let (precise, _) = rig.provider(Schema::V2Precise, crate::adapter::CADENCE);
    let retained = rig.http(&v2, StateRoute::V2);
    let before = regular_counts.work();
    for k1 in 100..164 {
        rig.publish(input::snapshot(input::SCORES[0], k1), Delivery::Precise);
    }
    // Positive control: the precise commits reached the bus.
    let keys = rig.http(&precise, StateRoute::V2Precise);
    assert_eq!(json_at(&keys, "/keys/k1/count").as_i64(), Some(163));
    let after = rig.http(&v2, StateRoute::V2);
    assert_eq!(after.as_ptr(), retained.as_ptr(), "regular bytes retained");
    assert_eq!(regular_counts.work(), before, "no regular decode or encode");
    // Positive control: a regular change invalidates.
    rig.publish(regular(input::SCORES[1]), Delivery::All);
    let changed = rig.http(&v2, StateRoute::V2);
    assert_ne!(changed.as_ptr(), retained.as_ptr());
    assert_eq!(score(&changed), Some(input::SCORES[1]));
    assert_eq!((regular_counts.work() - before).decodes, 1);
}

#[test]
fn production_filters_are_isolated_and_demand_is_bounded() {
    let rig = Rig::new();
    let key = rig.session_key(0);
    let (precise, counts) = rig.provider(Schema::V2Precise, crate::adapter::CADENCE);
    let hub = rig.hub(Schema::V2Precise, precise);
    let keys_only = vec![Filter::Field("keys".into())];
    let errors_only = vec![Filter::Field("hitErrors".into())];
    let mut keys = hub.connect(&rig, Some(keys_only.clone()));
    let mut errors = hub.connect(&rig, Some(errors_only));
    let mut plain = hub.connect(&rig, None);
    let demand = rig.demand(key);
    assert!(selected(&demand, "/keys/overlay") && selected(&demand, "/play/hitErrors/all"));
    // Eighty more distinct plans: union demand stays within the schema's own
    // selectors; per-client plans never add selectors or histories.
    let schema_selectors = vosu_adapter_upstream::precise::selectors().len();
    let distinct: Vec<_> = (0..80)
        .map(|n| {
            hub.connect(
                &rig,
                Some(vec![Filter::Nested {
                    field: "keys".into(),
                    keys: vec![Filter::Field(format!("x{n}"))],
                }]),
            )
        })
        .collect();
    assert!(rig.demand(key).len() <= demand.len().max(schema_selectors));
    let before = counts.work();
    rig.publish(input::snapshot(input::SCORES[0], 77), Delivery::Precise);
    for client in [&keys, &errors, &plain] {
        client.ack();
    }
    let (k, e, p) = rig.runtime().block_on(async {
        (
            keys.frame().await,
            errors.frame().await,
            plain.frame().await,
        )
    });
    let k: serde_json::Value = serde_json::from_slice(&k).unwrap();
    assert_eq!(
        k.as_object().unwrap().len(),
        1,
        "keys plan sees only keys: {k}"
    );
    assert_eq!(k["keys"]["k1"]["count"], 77);
    let e: serde_json::Value = serde_json::from_slice(&e).unwrap();
    assert_eq!(
        e.as_object().unwrap().keys().collect::<Vec<_>>(),
        ["hitErrors"]
    );
    assert_eq!(json_at(&p, "/keys/k1/count"), 77);
    let work = counts.work() - before;
    assert_eq!(work.decodes, 1);
    assert_eq!(
        work.unfiltered, 1,
        "the plain client's bytes are not per-plan"
    );
    assert!(
        work.filtered <= 2,
        "only acknowledged filtered clients encode"
    );
    // Replacing one client's plan releases only that client's selectors.
    drop(distinct);
    keys.connection
        .set_filters(vec![Filter::Field("hitErrors".into())]);
    drop(plain);
    let demand = rig.demand(key);
    assert!(
        !selected(&demand, "/keys/overlay"),
        "no remaining plan observes keys"
    );
    assert!(
        selected(&demand, "/play/hitErrors/all"),
        "other plans retained"
    );
    drop((keys, errors));
    assert!(rig.demand(key).is_empty(), "last lease releases demand");
    let _ = keys_only;
    hub.stop(&rig);
}

#[test]
fn production_client_retains_one_in_flight_frame_and_coalesces_1000_updates() {
    let rig = Rig::new();
    let (v2, counts) = rig.provider(Schema::V2, crate::adapter::CADENCE);
    let hub = rig.hub(Schema::V2, v2);
    let mut client = hub.connect(&rig, None);
    let before = counts.work();
    for n in 0..1_000 {
        rig.publish(regular(500_000.0 + f64::from(n)), Delivery::All);
    }
    assert!(
        client.receiver.try_recv().is_none(),
        "an unacknowledged writer never receives a second state frame"
    );
    assert_eq!(
        counts.work() - before,
        Default::default(),
        "no view or encoding is produced for an in-flight client"
    );
    client.ack();
    let frame = rig.runtime().block_on(client.frame());
    assert_eq!(score(&frame), Some(500_999.0), "only the latest state");
    assert!(client.receiver.try_recv().is_none());
    let work = counts.work() - before;
    assert_eq!(work.decodes, 1, "1,000 commits coalesce to one view");
    assert_eq!(work.unfiltered, 1, "1,000 commits coalesce to one encoding");
    hub.stop(&rig);
}

#[test]
fn production_idle_adapters_add_no_allocations_work_or_demand() {
    let rig = Rig::new();
    let key = rig.session_key(0);
    let publish = || {
        for n in 0..32 {
            rig.publish(regular(input::SCORES[n % 2]), Delivery::All);
        }
    };
    publish();
    let (_, baseline) = allocations_during(publish);
    let schemas = [
        Schema::V1,
        Schema::StreamCompanion,
        Schema::V2,
        Schema::V2Precise,
    ];
    let providers = schemas.map(|schema| (schema, rig.provider(schema, Duration::from_millis(1))));
    let hubs: Vec<_> = providers
        .iter()
        .map(|(schema, (provider, _))| rig.hub(*schema, provider.clone()))
        .collect();
    let api = ApiV1SubscriptionHub::new(rig.fixture.h.host.api_state_source(), Duration::ZERO);
    let (_, idle) = allocations_during(publish);
    assert!(baseline > 0, "the allocator observes the real commit path");
    assert_eq!(
        idle, baseline,
        "idle adapters and hubs allocate nothing per commit"
    );
    for (_, (_, counts)) in &providers {
        assert_eq!(
            counts.work(),
            Default::default(),
            "no capture, view or encoding"
        );
    }
    assert!(
        rig.demand(key).is_empty(),
        "no sampling demand without clients"
    );
    // Positive control: a real client creates demand and work.
    let client = hubs[2].connect(&rig, None);
    assert!(!rig.demand(key).is_empty());
    assert!(providers[2].1.1.work().unfiltered > 0);
    client.ack();
    drop(client);
    assert!(rig.demand(key).is_empty(), "disconnect releases demand");
    api.shutdown();
    for hub in hubs {
        hub.stop(&rig);
    }
}

#[test]
fn production_demand_is_raii_per_connection_and_independent_of_api_v1() {
    let rig = Rig::new();
    let key = rig.session_key(0);
    let api = ApiV1SubscriptionHub::new(rig.fixture.h.host.api_state_source(), Duration::ZERO);
    let (api_connection, mut api_receiver) = {
        let _enter = rig.runtime().enter();
        api.connect(SessionTarget::Active).unwrap()
    };
    api_connection.text(br#"["/vosu.osu/play/score"]"#);
    rig.runtime()
        .block_on(crate::adapter::bounded(api_receiver.recv()))
        .unwrap();
    assert!(selected(&rig.demand(key), "/play/score"), "API v1 demand");
    assert!(!selected(&rig.demand(key), "/keys/overlay"));
    let (precise, _) = rig.provider(Schema::V2Precise, crate::adapter::CADENCE);
    let hub = rig.hub(Schema::V2Precise, precise);
    let first = hub.connect(&rig, None);
    let second = hub.connect(&rig, None);
    assert!(
        selected(&rig.demand(key), "/keys/overlay"),
        "adapter demand"
    );
    drop(first);
    assert!(
        selected(&rig.demand(key), "/keys/overlay"),
        "one disconnect keeps the other client's demand"
    );
    drop(second);
    let demand = rig.demand(key);
    assert!(
        !selected(&demand, "/keys/overlay"),
        "last lease drops demand"
    );
    assert!(
        selected(&demand, "/play/score"),
        "API v1 demand is independent"
    );
    drop((api_connection, api_receiver));
    api.shutdown();
    hub.stop(&rig);
}

#[test]
fn production_unchanged_chart_is_never_reopened_or_reparsed() {
    let rig = Rig::new();
    let charts = crate::adapter::Charts::new(&rig);
    let (sc, counts) = rig.provider(Schema::StreamCompanion, crate::adapter::CADENCE);
    let mut events = sc.subscribe(Schema::StreamCompanion);
    let hub = rig.hub(Schema::StreamCompanion, sc.clone());
    let mut client = hub.connect(&rig, None);
    rig.publish(charts.snapshot(input::SCORES[0], 0), Delivery::All);
    charts.await_bytes(&rig, &sc, &mut events, 0);
    let chart = rig.fixture.chart.counts();
    assert_eq!((chart.opens, chart.reads, chart.parses), (1, 1, 1));
    let before = counts.work();
    for n in 0..64 {
        let snapshot = charts.snapshot(input::SCORES[n % 2], 0);
        rig.publish(snapshot, Delivery::All);
        let mut precise = (*charts.snapshot(input::SCORES[n % 2], 0)).clone();
        precise.precise = input::snapshot(0.0, 200 + n as i64).precise.clone();
        rig.publish(std::sync::Arc::new(precise), Delivery::Precise);
        client.ack();
        let frame = rig.runtime().block_on(client.frame());
        assert!(memchr::memmem::find(&frame, charts.marker(0)).is_some());
    }
    assert!((counts.work() - before).decodes >= 64, "views were rebuilt");
    assert_eq!(rig.fixture.chart.counts(), chart, "no open, read or parse");
    // Positive control: a changed chart is read and parsed once.
    rig.publish(charts.snapshot(input::SCORES[0], 1), Delivery::All);
    charts.await_bytes(&rig, &sc, &mut events, 1);
    let changed = rig.fixture.chart.counts();
    assert_eq!(changed.parses, chart.parses + 1);
    drop(client);
    hub.stop(&rig);
}

#[test]
fn fixture_routes_streams_only_to_their_owner_and_close_releases() {
    let rig = Rig::new();
    let charts = crate::adapter::Charts::new(&rig);
    rig.publish(charts.snapshot(input::SCORES[0], 0), Delivery::All);
    let requester = rig
        .fixture
        .h
        .host
        .surface_requester(vosu_core::plugin::PluginInstanceId::new(4))
        .unwrap();
    let selector = Selector {
        ns: "vosu.chart".into(),
        pointer: "/resources/file".into(),
    };
    let mut subscription = requester
        .subscribe(Target::Focused, vec![selector.clone()])
        .unwrap();
    let reference = rig.runtime().block_on(crate::adapter::bounded(async {
        loop {
            let capture = requester
                .capture(Target::Focused, std::slice::from_ref(&selector))
                .unwrap();
            if let Some(value) = capture.values[0].value.as_ref() {
                let url: serde_json::Value = value.deserialized().unwrap();
                break (
                    capture.session.unwrap(),
                    url["url"].as_str().unwrap().to_owned(),
                );
            }
            subscription.updates.changed().await.unwrap();
        }
    }));
    let token = reference.1.rsplit('/').next().unwrap().to_owned();
    assert_eq!(
        rig.fixture.stream_owner(u64::MAX).err(),
        Some(vosu_status::VOSU_ERR_TARGET_GONE),
        "an unowned stream gets the production TARGET_GONE error"
    );
    let open = || {
        rig.runtime()
            .block_on(requester.open_published_resource(&reference.0, &token))
            .unwrap()
    };
    let resource = open();
    let owned: Vec<u64> = (1..=64)
        .filter(|stream| rig.fixture.stream_owner(*stream).is_ok())
        .collect();
    assert_eq!(owned.len(), 1, "exactly the open stream is owned");
    let prefix = rig.runtime().block_on(resource.file.read(0, 19)).unwrap();
    assert_eq!(prefix, b"osu file format v14", "reads route to the owner");
    drop(resource);
    // The close precedes the next resolve in the instance's ordered outbox.
    let reopened = open();
    assert_eq!(
        rig.fixture.stream_owner(owned[0]).err(),
        Some(vosu_status::VOSU_ERR_TARGET_GONE),
        "close released the stream"
    );
    drop(reopened);
}
