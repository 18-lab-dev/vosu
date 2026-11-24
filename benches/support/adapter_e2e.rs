//! D17 `end_to_end/adapter*`: the production adapter path from a source
//! snapshot through the real osu! projection, ABI batch, host bus commit,
//! coherent surface capture and real adapter, to encoded output bytes.
//! Every iteration publishes a distinct value and must observe a new encoding;
//! exact decode/encode counts are asserted after timing.
use super::adapter::{self as rig, Rig, input, json_at};
use bytes::Bytes;
use criterion::Criterion;
use std::{cell::Cell, time::Duration};
use vosu::server::{
    api_v1::subscriptions::ApiV1SubscriptionHub, routes::StateRoute, subscription_wire,
    websocket::Schema,
};
use vosu_core::api::SessionTarget;
use vosu_osu_state::Delivery;

async fn api_frame(receiver: &mut subscription_wire::Receiver) -> Bytes {
    match rig::bounded(receiver.recv()).await {
        Some(subscription_wire::Outbound::Text(bytes)) => bytes,
        frame => panic!("expected an API v1 state frame, got {frame:?}"),
    }
}

/// Regular change → `/json/v2` HTTP bytes **and** an encoded, delivered and
/// acknowledged API v1 subscription frame.
fn adapter(c: &mut Criterion) {
    let rig = Rig::new();
    let (v2, counts) = rig.provider(Schema::V2, rig::CADENCE);
    let api = ApiV1SubscriptionHub::new(rig.fixture.h.host.api_state_source(), Duration::ZERO);
    let (connection, mut receiver) = {
        let _enter = rig.runtime().enter();
        api.connect(SessionTarget::Active).unwrap()
    };
    connection.text(br#"["/vosu.play/score"]"#);
    rig.runtime().block_on(api_frame(&mut receiver));
    connection.write_completed();
    let inputs = input::SCORES.map(|score| input::snapshot(score, input::K1_COUNTS[0]));
    let toggle = Cell::new(0);
    let mut step = || {
        toggle.set(toggle.get() ^ 1);
        rig.publish(inputs[toggle.get()].clone(), Delivery::All);
        let body = rig.http(&v2, StateRoute::V2);
        let frame = rig.runtime().block_on(api_frame(&mut receiver));
        connection.write_completed();
        (body, frame)
    };
    // Declared outputs carry the distinctive nonzero input before timing.
    for _ in 0..2 {
        let (body, frame) = step();
        let score = input::SCORES[toggle.get()];
        assert_eq!(json_at(&body, "/play/score").as_f64(), Some(score));
        assert_eq!(json_at(&frame, "/2/0/1").as_f64(), Some(score));
    }
    let before = counts.work();
    let iterations = Cell::new(0_usize);
    let mut previous = step();
    c.benchmark_group("end_to_end")
        .bench_function("adapter", |b| {
            b.iter(|| {
                let next = step();
                // A cache hit or a repeated frame would time less work.
                assert_ne!(
                    next.0.as_ptr(),
                    previous.0.as_ptr(),
                    "new /json/v2 encoding"
                );
                assert_ne!(next.1, previous.1, "new API v1 frame");
                iterations.set(iterations.get() + 1);
                previous = std::hint::black_box(next);
            })
        });
    let work = counts.work() - before;
    let iterations = iterations.get() + 1;
    assert_eq!(work.decodes, iterations, "one coherent view per change");
    assert_eq!(
        work.unfiltered, iterations,
        "one /json/v2 encoding per change"
    );
    api.shutdown();
}

/// Precise-only change → encoded `/json/v2/precise` WebSocket frame through the
/// production hub's acknowledgement path; the regular view is never rebuilt.
fn adapter_precise(c: &mut Criterion) {
    let rig = Rig::new();
    let (regular, regular_counts) = rig.provider(Schema::V2, rig::CADENCE);
    let (precise, counts) = rig.provider(Schema::V2Precise, rig::CADENCE);
    let retained = rig.http(&regular, StateRoute::V2);
    let regular_before = regular_counts.work();
    let hub = rig.hub(Schema::V2Precise, precise);
    let mut client = hub.connect(&rig, None);
    let inputs = input::K1_COUNTS.map(|k1| input::snapshot(input::SCORES[0], k1));
    let toggle = Cell::new(0);
    let mut step = || {
        toggle.set(toggle.get() ^ 1);
        rig.publish(inputs[toggle.get()].clone(), Delivery::Precise);
        client.ack();
        rig.runtime().block_on(client.frame())
    };
    for _ in 0..2 {
        let frame = step();
        assert_eq!(
            json_at(&frame, "/keys/k1/count").as_i64(),
            Some(input::K1_COUNTS[toggle.get()])
        );
    }
    let before = counts.work();
    let iterations = Cell::new(0_usize);
    let mut previous = step();
    c.benchmark_group("end_to_end")
        .bench_function("adapter_precise", |b| {
            b.iter(|| {
                let next = step();
                assert_ne!(next, previous, "new precise frame");
                iterations.set(iterations.get() + 1);
                previous = std::hint::black_box(next);
            })
        });
    let work = counts.work() - before;
    let iterations = iterations.get() + 1;
    assert_eq!(work.decodes, iterations, "one precise view per change");
    assert_eq!(
        work.unfiltered, iterations,
        "one precise encoding per change"
    );
    let after = rig.http(&regular, StateRoute::V2);
    assert_eq!(
        after.as_ptr(),
        retained.as_ptr(),
        "precise-only changes retain the regular view and bytes"
    );
    assert_eq!(regular_counts.work(), regular_before, "no regular rebuild");
    hub.stop(&rig);
}

pub fn bench(c: &mut Criterion) {
    adapter(c);
    adapter_precise(c);
}
