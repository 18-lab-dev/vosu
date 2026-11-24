use super::support;
use criterion::Criterion;
use std::{
    sync::{Arc, atomic::AtomicBool},
    time::Duration,
};
use vosu::server::api_v1::subscriptions::ApiV1SubscriptionHub;
use vosu_core::api::SessionTarget;
use vosu_osu::{project::SessionFacts, publish::BusPublisher};
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
use vosu_osu_state::{Delivery, SnapshotPublisher};
use vosu_plugin_testkit::publisher;

pub fn bench(c: &mut Criterion, group: &str, case: &str) {
    let h = support::harness::Harness::new();
    let manifest = include_str!("../../crates/vosu-osu/manifest.toml");
    publisher::register_manifest("vosu.osu", manifest);
    let events = publisher::register("vosu.osu");
    let loaded = vosu_core::plugin::load_bundled(&[vosu_core::plugin::BundledPlugin {
        manifest,
        settings_schema: Some(include_str!("../../crates/vosu-osu/settings.schema.json")),
        namespace_schemas: &[include_str!("../../docs/v2/schemas/vosu.osu.schema.json")],
        action_schemas: vosu_osu::ACTION_SCHEMAS,
        vtable: publisher::vosu_plugin_vtable,
    }])
    .unwrap();
    let pool = vosu_core::plugin::WorkerPool::new(1, 64);
    h.host
        .create_instance(
            vosu_core::plugin::PluginInstanceId::new(1),
            &loaded[0],
            h.root.path().into(),
            Some(pool),
        )
        .unwrap();
    h.host
        .process_appeared(support::harness::record(993001, "osu!.exe"));
    let deadline = std::time::Instant::now() + support::harness::WAIT;
    let handle = loop {
        if let publisher::PublisherEvent::Attached { handle } = events
            .recv_timeout(deadline.saturating_duration_since(std::time::Instant::now()))
            .unwrap()
        {
            break handle;
        }
    };
    let initial = vosu_osu_state::StateBundle::new(
        ClientType::Stable,
        1,
        Arc::new(vosu_osu_state::state_backend::LiveStateCalculationBackend::new()),
    )
    .snapshot();
    let state = |score| {
        let mut next = (*initial).clone();
        let mut regular = (*next.regular).clone();
        regular.global.status = 2;
        regular.gameplay.score = score;
        next.regular = Arc::new(regular);
        let precise = Arc::make_mut(&mut next.precise);
        precise.status = 2;
        precise.play_time = 1234.0;
        Arc::new(next)
    };
    let a = state(12345.0);
    let b = state(23456.0);
    let publisher = BusPublisher::new(
        publisher::host_services("vosu.osu"),
        handle,
        Arc::new(AtomicBool::new(true)),
        SessionFacts {
            client: ClientType::Stable,
            settings: InstanceSettings {
                tournament_spectator: false,
                tournament_ipc_id: None,
                devserver_endpoint: None,
            },
            foreground: None,
        },
    );
    publisher.publish(a.clone(), Delivery::All).unwrap();
    if group == "api_v1" {
        let source = h.host.api_state_source();
        let plan = vosu_core::api::ApiPlan::new(&["/vosu.play/score"]).unwrap();
        c.benchmark_group("selection_equivalent")
            .bench_function("score_read", |bencher| {
                bencher.iter(|| {
                    let update = source.read(&SessionTarget::Active, &plan).unwrap();
                    let mut body = b"{\"value\":".to_vec();
                    update.values[0].as_ref().unwrap().write_json(&mut body);
                    body.push(b'}');
                    assert_eq!(body, b"{\"value\":12345}");
                    std::hint::black_box(body);
                })
            });
    }
    let hub = ApiV1SubscriptionHub::new(h.host.api_state_source(), Duration::ZERO);
    let (connection, mut receiver) = {
        let _enter = h.runtime.enter();
        hub.connect(SessionTarget::Active).unwrap()
    };
    connection.text(br#"["/vosu.play/score","/vosu.osu/play/score"]"#);
    h.runtime
        .block_on(support::harness::bounded(receiver.recv()))
        .unwrap();
    connection.write_completed();
    let mut toggle = false;
    c.benchmark_group(group).bench_function(case, |bencher| {
        bencher.iter(|| {
            h.runtime.block_on(support::harness::bounded(async {
                toggle = !toggle;
                publisher
                    .publish(if toggle { b.clone() } else { a.clone() }, Delivery::All)
                    .unwrap();
                let frame = receiver.recv().await.unwrap();
                assert!(matches!(
                    frame,
                    vosu::server::subscription_wire::Outbound::Text(_)
                ));
                std::hint::black_box(frame);
                connection.write_completed();
            }))
        })
    });
    hub.shutdown();
}
