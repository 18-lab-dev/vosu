//! Named D17 `adapter/*` cases over the production providers and hub. Each
//! iteration publishes a distinct source value through the real host bus; work
//! counts (views, encodings, chart parses) are asserted after timing.
use super::adapter::{self as rig, Client, Rig, input, json_at};
use crate::adapter_fixture::Fixture;
use criterion::{BenchmarkGroup, Criterion, Throughput, measurement::WallTime};
use std::{cell::Cell, sync::Arc, time::Duration};
use vosu::server::{
    api_v1::subscriptions::ApiV1SubscriptionHub,
    routes::StateRoute,
    websocket::{Schema, StateProvider},
};
use vosu_osu_model::enums::ClientType;
use vosu_osu_state::{Delivery, StateBundleSnapshot};
use vosu_surface::filter::Filter;

type Group<'a> = BenchmarkGroup<'a, WallTime>;
const SCHEMAS: [Schema; 4] = [
    Schema::V1,
    Schema::StreamCompanion,
    Schema::V2,
    Schema::V2Precise,
];

fn regular_inputs() -> [Arc<StateBundleSnapshot>; 2] {
    input::SCORES.map(|score| input::snapshot(score, input::K1_COUNTS[0]))
}

/// Commits with every adapter provider, schema hub and an API v1 hub present
/// but no client: the adapters must do no work and request no demand.
fn idle_no_clients(group: &mut Group<'_>) {
    let rig = Rig::new();
    let key = rig.session_key(0);
    let providers = SCHEMAS.map(|schema| (schema, rig.provider(schema, rig::CADENCE)));
    let hubs: Vec<_> = providers
        .iter()
        .map(|(schema, (provider, _))| rig.hub(*schema, provider.clone()))
        .collect();
    let api = ApiV1SubscriptionHub::new(rig.fixture.h.host.api_state_source(), Duration::ZERO);
    assert!(
        rig.demand(key).is_empty(),
        "idle consumers request no demand"
    );
    let inputs = regular_inputs();
    let toggle = Cell::new(0);
    let iterations = Cell::new(0_usize);
    group.bench_function("idle_no_clients", |b| {
        b.iter(|| {
            toggle.set(toggle.get() ^ 1);
            rig.publish(inputs[toggle.get()].clone(), Delivery::All);
            iterations.set(iterations.get() + 1);
        })
    });
    for (_, (_, counts)) in &providers {
        assert_eq!(
            counts.work(),
            Default::default(),
            "idle adapters do no work"
        );
    }
    assert!(rig.demand(key).is_empty(), "commits create no demand");
    // Positive control: the commits reached the bus and the counters observe work.
    let (_, (v2, v2_counts)) = &providers[2];
    let body = rig.http(v2, StateRoute::V2);
    assert_eq!(
        json_at(&body, "/play/score").as_f64(),
        Some(input::SCORES[toggle.get()])
    );
    assert_eq!(v2_counts.work().decodes, 1);
    api.shutdown();
    for hub in hubs {
        hub.stop(&rig);
    }
}

/// HTTP `/json/v2` then the WebSocket frame of the same change: one view, one
/// encoding, and the same `Bytes` allocation on both transports.
fn http_then_ws_cache(group: &mut Group<'_>) {
    let rig = Rig::new();
    let (v2, counts) = rig.provider(Schema::V2, rig::CADENCE);
    let hub = rig.hub(Schema::V2, v2.clone());
    let mut client = hub.connect(&rig, None);
    let inputs = regular_inputs();
    let toggle = Cell::new(0);
    let mut step = || {
        toggle.set(toggle.get() ^ 1);
        rig.publish(inputs[toggle.get()].clone(), Delivery::All);
        let body = rig.http(&v2, StateRoute::V2);
        client.ack();
        let frame = rig.runtime().block_on(client.frame());
        assert_eq!(
            frame.as_ptr(),
            body.as_ptr(),
            "HTTP and WS share one encoding"
        );
        frame
    };
    let frame = step();
    assert_eq!(
        json_at(&frame, "/play/score").as_f64(),
        Some(input::SCORES[toggle.get()])
    );
    let before = counts.work();
    let iterations = Cell::new(0_usize);
    group.bench_function("http_then_ws_cache", |b| {
        b.iter(|| {
            std::hint::black_box(step());
            iterations.set(iterations.get() + 1);
        })
    });
    let work = counts.work() - before;
    assert_eq!(work.decodes, iterations.get());
    assert_eq!(work.unfiltered, iterations.get());
    hub.stop(&rig);
}

/// One change fanned out to 100 plain clients, each acknowledging its write:
/// one view and one encoding, delivered as one shared allocation.
fn plain_100_clients(group: &mut Group<'_>) {
    let rig = Rig::new();
    let (v2, counts) = rig.provider(Schema::V2, rig::CADENCE);
    let hub = rig.hub(Schema::V2, v2);
    let mut clients: Vec<Client> = (0..100).map(|_| hub.connect(&rig, None)).collect();
    let inputs = regular_inputs();
    let toggle = Cell::new(0);
    let mut step = || {
        toggle.set(toggle.get() ^ 1);
        rig.publish(inputs[toggle.get()].clone(), Delivery::All);
        for client in &clients {
            client.ack();
        }
        rig.runtime().block_on(async {
            let first = clients[0].frame().await;
            for client in &mut clients[1..] {
                let frame = client.frame().await;
                assert_eq!(frame.as_ptr(), first.as_ptr(), "one shared allocation");
            }
            first
        })
    };
    let frame = step();
    assert_eq!(
        json_at(&frame, "/play/score").as_f64(),
        Some(input::SCORES[toggle.get()])
    );
    let before = counts.work();
    let iterations = Cell::new(0_usize);
    group.throughput(Throughput::Elements(100));
    group.bench_function("plain_100_clients", |b| {
        b.iter(|| {
            std::hint::black_box(step());
            iterations.set(iterations.get() + 1);
        })
    });
    let work = counts.work() - before;
    assert_eq!(work.decodes, iterations.get());
    assert_eq!(
        work.unfiltered,
        iterations.get(),
        "100 clients, one encoding"
    );
    hub.stop(&rig);
}

/// Legacy filter plans encode per distinct plan per pass; clients sharing a
/// plan share that pass's bytes.
pub fn filter_plans() -> Vec<Vec<Filter>> {
    let nested = |field: &str, keys: &[&str]| Filter::Nested {
        field: field.into(),
        keys: keys
            .iter()
            .map(|key| Filter::Field((*key).into()))
            .collect(),
    };
    vec![
        vec![nested("play", &["score"])],
        vec![
            nested("play", &["score", "combo"]),
            Filter::Field("state".into()),
        ],
        vec![Filter::Field("play".into())],
        vec![Filter::Field("beatmap".into())],
        vec![Filter::Field("state".into())],
        vec![Filter::Field("settings".into())],
        vec![Filter::Field("profile".into())],
        vec![Filter::Field("resultsScreen".into())],
    ]
}

/// Sixteen filtered clients: eight distinct plans, each shared by two clients.
/// Shared plans reuse one encoding within a delivery pass; acknowledgements
/// that land in separate passes encode again, so the count is bounded by
/// [plans, clients] per change and reported rather than assumed.
fn filtered_clients(group: &mut Group<'_>) {
    let rig = Rig::new();
    let (v2, counts) = rig.provider(Schema::V2, rig::CADENCE);
    let hub = rig.hub(Schema::V2, v2);
    let plans = filter_plans();
    let mut clients: Vec<Client> = plans
        .iter()
        .flat_map(|plan| [plan.clone(), plan.clone()])
        .map(|plan| hub.connect(&rig, Some(plan)))
        .collect();
    let inputs = regular_inputs();
    let toggle = Cell::new(0);
    let mut step = || {
        toggle.set(toggle.get() ^ 1);
        rig.publish(inputs[toggle.get()].clone(), Delivery::All);
        for client in &clients {
            client.ack();
        }
        rig.runtime().block_on(async {
            let mut frames = Vec::with_capacity(clients.len());
            for pair in clients.chunks_mut(2) {
                let left = pair[0].frame().await;
                let right = pair[1].frame().await;
                assert_eq!(left, right, "shared plan, identical bytes");
                frames.push(left);
            }
            frames
        })
    };
    // Filter replacement can race the first unfiltered frame; settle first.
    step();
    let frames = step();
    let score = input::SCORES[toggle.get()];
    assert_eq!(json_at(&frames[0], "/play/score").as_f64(), Some(score));
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&frames[0]).unwrap(),
        serde_json::json!({"play": {"score": score as i64}})
    );
    let before = counts.work();
    let iterations = Cell::new(0_usize);
    group.throughput(Throughput::Elements(16));
    group.bench_function("filtered_clients", |b| {
        b.iter(|| {
            std::hint::black_box(step());
            iterations.set(iterations.get() + 1);
        })
    });
    let work = counts.work() - before;
    assert_eq!(work.decodes, iterations.get());
    let changes = iterations.get().max(1);
    assert!(
        work.filtered >= plans.len() * iterations.get()
            && work.filtered <= clients.len() * iterations.get(),
        "filtered encodings per change within [plans, clients]: {work:?}"
    );
    eprintln!(
        "adapter/filtered_clients: {:.2} filtered encodings per change ({} plans, {} clients)",
        work.filtered as f64 / changes as f64,
        plans.len(),
        clients.len()
    );
    assert_eq!(
        work.unfiltered, 0,
        "no plain client, no unfiltered encoding"
    );
    hub.stop(&rig);
}

/// A slow transport holds its frame across sixteen commits, then acknowledges:
/// it receives exactly the latest state, built from one view and one encoding.
fn slow_consumer(group: &mut Group<'_>) {
    let rig = Rig::new();
    let (v2, counts) = rig.provider(Schema::V2, rig::CADENCE);
    let hub = rig.hub(Schema::V2, v2);
    let mut client = hub.connect(&rig, None);
    let inputs: Vec<_> = (0..32)
        .map(|n| input::snapshot(100_000.0 + f64::from(n), input::K1_COUNTS[0]))
        .collect();
    let block = Cell::new(0);
    let markers = [
        format!("\"score\":{}", 100_015),
        format!("\"score\":{}", 100_031),
    ];
    let mut step = || {
        block.set(block.get() ^ 1);
        for snapshot in &inputs[block.get() * 16..block.get() * 16 + 16] {
            rig.publish(snapshot.clone(), Delivery::All);
        }
        assert!(client.receiver.try_recv().is_none(), "one frame in flight");
        client.ack();
        let frame = rig.runtime().block_on(client.frame());
        assert!(
            memchr::memmem::find(&frame, markers[block.get()].as_bytes()).is_some(),
            "latest commit only"
        );
        frame
    };
    step();
    let before = counts.work();
    let iterations = Cell::new(0_usize);
    group.bench_function("slow_consumer", |b| {
        b.iter(|| {
            std::hint::black_box(step());
            iterations.set(iterations.get() + 1);
        })
    });
    let work = counts.work() - before;
    assert_eq!(work.decodes, iterations.get(), "sixteen commits, one view");
    assert_eq!(
        work.unfiltered,
        iterations.get(),
        "sixteen commits, one encoding"
    );
    hub.stop(&rig);
}

/// Alternate the foreground between two attached stable sessions with distinct
/// scores; each switch yields a fresh coherent view on HTTP and WebSocket.
fn focus_switch(group: &mut Group<'_>) {
    // `adapter_fixture::Fixture` assigns attached client pids from 750001.
    const FIRST_PID: u32 = 750_001;
    let rig = Rig::with(Fixture::new(ClientType::Stable), 2);
    for (publisher, score) in rig.publishers.iter().zip(input::SCORES) {
        publisher
            .publish(input::snapshot(score, input::K1_COUNTS[0]), Delivery::All)
            .unwrap();
    }
    let (v2, counts) = rig.provider(Schema::V2, rig::CADENCE);
    let hub = rig.hub(Schema::V2, v2.clone());
    let mut client = hub.connect(&rig, None);
    let toggle = Cell::new(1);
    let mut step = || {
        toggle.set(toggle.get() ^ 1);
        rig.fixture
            .h
            .foreground
            .set(Some(FIRST_PID + toggle.get() as u32));
        rig.fixture.h.host.foreground_changed();
        let body = rig.http(&v2, StateRoute::V2);
        client.ack();
        let frame = rig.runtime().block_on(client.frame());
        assert_eq!(frame.as_ptr(), body.as_ptr());
        frame
    };
    for _ in 0..2 {
        let frame = step();
        assert_eq!(
            json_at(&frame, "/play/score").as_f64(),
            Some(input::SCORES[toggle.get()]),
            "the selected session's coherent image"
        );
    }
    let before = counts.work();
    let iterations = Cell::new(0_usize);
    group.bench_function("focus_switch", |b| {
        b.iter(|| {
            std::hint::black_box(step());
            iterations.set(iterations.get() + 1);
        })
    });
    let work = counts.work() - before;
    assert_eq!(work.decodes, iterations.get(), "one view per switch");
    assert_eq!(work.unfiltered, iterations.get());
    hub.stop(&rig);
}

/// streamCompanion with real chart resources: an unchanged chart is never
/// reparsed; a changed chart's cold resource IO, parse and derivation is
/// measured as its own case, through to the bytes carrying its derived facts.
fn streamcompanion(group: &mut Group<'_>) {
    let rig = Rig::new();
    let charts = rig::Charts::new(&rig);
    let (sc, counts) = rig.provider(Schema::StreamCompanion, rig::CADENCE);
    let mut events = sc.subscribe(Schema::StreamCompanion);
    let hub = rig.hub(Schema::StreamCompanion, sc.clone());
    // The connected client's lease is the dependent consumer of chart work.
    let mut client = hub.connect(&rig, None);
    rig.publish(charts.snapshot(input::SCORES[0], 0), Delivery::All);
    charts.await_bytes(&rig, &sc, &mut events, 0);
    let inputs = input::SCORES.map(|score| charts.snapshot(score, 0));
    let toggle = Cell::new(0);
    let mut step = || {
        toggle.set(toggle.get() ^ 1);
        rig.publish(inputs[toggle.get()].clone(), Delivery::All);
        client.ack();
        let frame = rig.runtime().block_on(client.frame());
        assert!(memchr::memmem::find(&frame, charts.marker(0)).is_some());
        frame
    };
    step();
    let parses = rig.fixture.chart.counts();
    let before = counts.work();
    let iterations = Cell::new(0_usize);
    group.bench_function("sc_unchanged_chart", |b| {
        b.iter(|| {
            std::hint::black_box(step());
            iterations.set(iterations.get() + 1);
        })
    });
    let work = counts.work() - before;
    assert_eq!(work.decodes, iterations.get());
    assert_eq!(work.unfiltered, iterations.get());
    assert_eq!(
        rig.fixture.chart.counts(),
        parses,
        "no open/read/parse for an unchanged chart"
    );

    // A changed chart: the osu! cold-resource worker publishes the new
    // reference, the SC worker opens/reads/parses it, and each provider
    // notification (bus change, derived completion) is the barrier for the
    // next production read: no cadence wait is timed.
    let inputs = [0, 1].map(|chart| charts.snapshot(input::SCORES[0], chart));
    let toggle = Cell::new(0);
    let mut step = || {
        toggle.set(toggle.get() ^ 1);
        events.borrow_and_update();
        rig.publish(inputs[toggle.get()].clone(), Delivery::All);
        let body = charts.await_bytes(&rig, &sc, &mut events, toggle.get());
        client.ack();
        let frame = rig.runtime().block_on(client.frame());
        assert_eq!(frame.as_ptr(), body.as_ptr(), "WS shares the HTTP bytes");
        frame
    };
    step();
    step();
    let before = rig.fixture.chart.counts();
    let iterations = Cell::new(0_usize);
    group.bench_function("sc_changed_chart", |b| {
        b.iter(|| {
            std::hint::black_box(step());
            iterations.set(iterations.get() + 1);
        })
    });
    let after = rig.fixture.chart.counts();
    eprintln!(
        "adapter/sc_changed_chart: {} changes, {} opens, {} reads, {} parses",
        iterations.get(),
        after.opens - before.opens,
        after.reads - before.reads,
        after.parses - before.parses
    );
    let changes = iterations.get() as u64;
    assert_eq!(
        after.opens - before.opens,
        changes,
        "one resource open per change"
    );
    assert_eq!(
        after.reads - before.reads,
        changes,
        "one cold read per change"
    );
    assert_eq!(
        after.parses - before.parses,
        changes,
        "one parse per change"
    );
    drop(client);
    hub.stop(&rig);
}

pub fn bench(c: &mut Criterion) {
    let mut group = c.benchmark_group("adapter");
    idle_no_clients(&mut group);
    http_then_ws_cache(&mut group);
    slow_consumer(&mut group);
    focus_switch(&mut group);
    streamcompanion(&mut group);
    // Fan-out cases report per-client throughput; Criterion cannot unset it.
    plain_100_clients(&mut group);
    filtered_clients(&mut group);
    group.finish();
}
