#[path = "support/api_e2e.rs"]
mod api_e2e;
#[path = "support/api_v1.rs"]
mod support;
use criterion::{BatchSize, Criterion, criterion_group, criterion_main};
use std::{
    hint::black_box,
    sync::{Arc, Mutex, RwLock, atomic::AtomicBool},
};
use vosu_core::state::{
    ClientFacts, InternalGrant, Pointer, PublisherGrant, SchemaRegistry, SessionKey, StateStore,
    SubscriberId, SubscriptionRegistry,
};
use vosu_osu::{project::SessionFacts, publish::BusPublisher};
use vosu_osu_model::{enums::ClientType, launch::InstanceSettings};
use vosu_osu_state::{Delivery, SnapshotPublisher, StateBundleSnapshot};
use vosu_plugin_api::{
    HostServices, Message, PluginManifest, StateUpdate, decode_publish_batch_ref, vosu_opcode,
    vosu_status,
};

const KEY: SessionKey = SessionKey(8202);
fn settings() -> InstanceSettings {
    InstanceSettings {
        tournament_spectator: false,
        tournament_ipc_id: None,
        devserver_endpoint: None,
    }
}
fn facts() -> SessionFacts {
    SessionFacts {
        client: ClientType::Stable,
        settings: settings(),
        foreground: None,
    }
}
fn snapshot(n: i64) -> Arc<StateBundleSnapshot> {
    let initial = vosu_osu_state::StateBundle::new(
        ClientType::Stable,
        1,
        Arc::new(vosu_osu_state::state_backend::LiveStateCalculationBackend::new()),
    )
    .snapshot();
    let mut regular = (*initial.regular).clone();
    regular.global.status = 2;
    regular.gameplay.score = n as f64;
    let mut precise = (*initial.precise).clone();
    precise.status = 2;
    precise.play_time = n as f64;
    Arc::new(StateBundleSnapshot {
        source: ClientType::Stable,
        generation: 1,
        reset_epoch: 0,
        regular: Arc::new(regular),
        precise: Arc::new(precise),
    })
}
struct Bus {
    schemas: Arc<RwLock<SchemaRegistry>>,
    store: StateStore,
    grant: PublisherGrant,
    subscriptions: SubscriptionRegistry,
}
impl Bus {
    fn new() -> Self {
        let manifest =
            PluginManifest::parse(include_str!("../crates/vosu-osu/manifest.toml")).unwrap();
        let grant = PublisherGrant::from_manifest(&manifest).unwrap();
        let mut schemas = SchemaRegistry::with_core().unwrap();
        schemas
            .register(
                "vosu.osu",
                "vosu.osu",
                include_str!("../docs/v2/schemas/vosu.osu.schema.json"),
            )
            .unwrap();
        let schemas = Arc::new(RwLock::new(schemas));
        let store = StateStore::new(schemas.clone());
        store.open_session(
            KEY,
            &grant,
            ClientFacts {
                game: "osu".into(),
                variant: "stable".into(),
                name: "bench".into(),
                version: "1".into(),
                pid: 8202,
                session: "bench".into(),
                plugin_id: "vosu.osu".into(),
                plugin_version: "2.0.0-alpha.0".into(),
            },
            true,
        );
        let mut subscriptions = SubscriptionRegistry::default();
        let selectors = [
            ("vosu.play", "/score"),
            ("vosu.play", "/combo"),
            ("vosu.play", "/accuracy"),
            ("vosu.play", "/health"),
            ("vosu.osu", "/session/state"),
            ("vosu.osu", "/beatmap/time/live"),
            ("vosu.osu", "/play/healthBar"),
            ("vosu.osu", "/play/hitErrors"),
            ("vosu.osu", "/profile"),
            ("vosu.osu", "/settings"),
        ];
        subscriptions
            .replace(
                &schemas.read().unwrap(),
                SubscriberId::Plugin(1),
                KEY,
                selectors
                    .into_iter()
                    .map(|(ns, p)| (Arc::from(ns), Pointer::parse(p).unwrap()))
                    .collect(),
                InternalGrant::none(),
            )
            .unwrap();
        Self {
            schemas,
            store,
            grant,
            subscriptions,
        }
    }
    fn commit(&self, bytes: &[u8], encode: bool) {
        let batch = decode_publish_batch_ref(bytes).unwrap();
        let outcome = self
            .store
            .commit(&self.grant, KEY, batch.entries())
            .unwrap();
        if encode {
            let schemas = self.schemas.read().unwrap();
            black_box(outcome.current.api_json(&schemas));
            for (_, values) in self.subscriptions.changes(&schemas, &outcome) {
                black_box(
                    Message::StateUpdate(StateUpdate {
                        session: KEY.0,
                        more: false,
                        values,
                    })
                    .encode_payload()
                    .unwrap(),
                );
            }
        }
    }
}
#[derive(Clone)]
struct Sink(Arc<Mutex<Vec<u8>>>);
impl HostServices for Sink {
    fn send(&self, _: &Message, _: u64) -> vosu_status {
        vosu_status::VOSU_OK
    }
    fn send_flat(&self, _: vosu_opcode, _: u32, _: u64, bytes: &[u8]) -> vosu_status {
        let mut out = self.0.lock().unwrap();
        out.clear();
        out.extend_from_slice(bytes);
        vosu_status::VOSU_OK
    }
}
fn publisher() -> (BusPublisher<Sink>, Arc<Mutex<Vec<u8>>>) {
    let bytes = Arc::new(Mutex::new(Vec::new()));
    (
        BusPublisher::new(
            Sink(bytes.clone()),
            KEY.0,
            Arc::new(AtomicBool::new(true)),
            facts(),
        ),
        bytes,
    )
}
fn bench(c: &mut Criterion) {
    let a = snapshot(1);
    let b = snapshot(2);
    let mut commits = c.benchmark_group("bus_commit");
    for (name, regular) in [("regular_batch", true), ("precise_batch", false)] {
        let (publisher, bytes) = publisher();
        publisher.publish(a.clone(), Delivery::All).unwrap();
        bytes.lock().unwrap().clear();
        let mut next = (*a).clone();
        if regular {
            next.regular = b.regular.clone();
        } else {
            next.precise = b.precise.clone();
        }
        publisher.publish(Arc::new(next), Delivery::All).unwrap();
        let encoded = bytes.lock().unwrap().clone();
        assert!(
            !encoded.is_empty(),
            "incremental source must produce a batch"
        );
        let bus = Bus::new();
        commits.bench_function(name, |bencher| {
            bencher.iter(|| bus.commit(black_box(&encoded), false))
        });
    }
    commits.finish();
    let mut projection = c.benchmark_group("osu_projection");
    projection.bench_function("cold", |bencher| {
        bencher.iter_batched(
            publisher,
            |(publisher, _)| publisher.publish(a.clone(), Delivery::All).unwrap(),
            BatchSize::SmallInput,
        )
    });
    for (name, regular) in [
        ("incremental_regular", true),
        ("incremental_precise", false),
    ] {
        let (publisher, _) = publisher();
        publisher.publish(a.clone(), Delivery::All).unwrap();
        let mut next = (*a).clone();
        if regular {
            next.regular = b.regular.clone();
        } else {
            next.precise = b.precise.clone();
        }
        let next = Arc::new(next);
        let mut toggle = false;
        projection.bench_function(name, |bencher| {
            bencher.iter(|| {
                toggle = !toggle;
                publisher
                    .publish(if toggle { next.clone() } else { a.clone() }, Delivery::All)
                    .unwrap();
            })
        });
    }
    let (publisher, _) = publisher();
    publisher.publish(a.clone(), Delivery::All).unwrap();
    projection.bench_function("unchanged", |bencher| {
        bencher.iter(|| publisher.publish(a.clone(), Delivery::All).unwrap())
    });
    projection.finish();
    let mut end = c.benchmark_group("end_to_end");
    let (publisher, bytes) = self::publisher();
    let bus = Bus::new();
    let mut toggle = false;
    end.bench_function("bus", |bencher| {
        bencher.iter(|| {
            toggle = !toggle;
            publisher
                .publish(if toggle { a.clone() } else { b.clone() }, Delivery::All)
                .unwrap();
            bus.commit(&bytes.lock().unwrap(), true);
        })
    });
    let mut precise_next = (*a).clone();
    precise_next.precise = b.precise.clone();
    let precise_next = Arc::new(precise_next);
    end.bench_function("bus_precise", |bencher| {
        bencher.iter(|| {
            toggle = !toggle;
            publisher
                .publish(
                    if toggle {
                        a.clone()
                    } else {
                        precise_next.clone()
                    },
                    Delivery::Precise,
                )
                .unwrap();
            bus.commit(&bytes.lock().unwrap(), true);
        })
    });
    end.finish();
    api_e2e::bench(c, "end_to_end", "api_frame");
}
criterion_group!(benches, bench);
criterion_main!(benches);
